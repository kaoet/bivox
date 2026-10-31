//! 百炼 Qwen-Audio-TTS 全双工双向流式合成客户端。
//!
//! 走 DashScope 统一的 run-task 协议（`/api-ws/v1/inference`）：
//! 建连 → run-task → task-started → continue-task(文本) → finish-task →
//! task-finished，音频通过 binary 帧流式返回。事件与参数结构同 ASR 链路一致，
//! 因此建连/握手/断线重连骨架完全复用 [`ws`]。

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::audio::playback::AudioPlaybackHandle;
use crate::client::ws::{self, ReconnectingSession, SessionEnd, WsRead, WsWrite};

#[derive(Debug, Clone)]
pub struct QwenTtsConfig {
    pub api_key: String,
    pub tts_model: String,
    pub voice: String,
    /// 业务空间专属域名的 run-task 端点，
    /// 形如 `wss://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference`
    pub ws_url: String,
}

#[derive(Clone)]
pub struct TtsChunkRequest {
    pub text: String,
    pub is_sentence_end: bool,
}

#[derive(Debug, Deserialize)]
struct TtsHeader {
    event: Option<String>,
    error_message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TtsServerResponse {
    header: TtsHeader,
}

/// TTS 会话的共享状态（跨重连保留）
pub(crate) struct TtsRuntime {
    config: QwenTtsConfig,
    rx_chunks: Receiver<TtsChunkRequest>,
    playback: AudioPlaybackHandle,
    /// 上一个连接在 run-task/握手阶段失败时保留的首个切片；
    /// 重连后优先重试它，避免整句漏合成
    pending_chunk: Option<TtsChunkRequest>,
    /// 当前 pending 切片连续启动失败的次数（超过阈值则丢弃，避免卡死后续字幕）
    pending_attempts: u32,
}

/// pending 切片最多重试的次数（每次失败都会经历一轮退避重连）
const MAX_PENDING_ATTEMPTS: u32 = 3;

impl TtsRuntime {
    /// 任务启动失败（连接/握手/task-failed）：保留当前句等重连重试；
    /// 连续失败超过阈值则丢弃该句，避免一句坏数据永久卡住整条 TTS 流水线。
    fn note_pending_failure(&mut self) {
        self.pending_attempts += 1;
        if self.pending_attempts >= MAX_PENDING_ATTEMPTS {
            if let Some(chunk) = self.pending_chunk.take() {
                eprintln!(
                    "⚠️ TTS 同一句连续 {} 次启动失败，丢弃该句: {:?}",
                    self.pending_attempts, chunk.text
                );
            }
            self.pending_attempts = 0;
        } else if let Some(chunk) = self.pending_chunk.as_ref() {
            eprintln!(
                "⚠️ TTS 该句启动失败（第 {}/{} 次），重连后重试: {:?}",
                self.pending_attempts, MAX_PENDING_ATTEMPTS, chunk.text
            );
        }
    }
}

impl ws::SessionState for TtsRuntime {
    fn before_reconnect(&mut self) {
        // 断线时截断残留的未播放音频，避免重连后回放旧缓冲
        self.playback.flush();
    }
}

/// 等待下一个句子切片的两种结果
enum ChunkWait {
    Chunk(TtsChunkRequest),
    End(SessionEnd),
}

pub struct QwenTtsClient;

impl QwenTtsClient {
    /// 运行百炼 Qwen-Audio-TTS 全双工双向异步流式语音合成服务：
    /// - 单条 WebSocket 连接跨句子长效复用（省掉每句的 TLS/TCP 握手）
    /// - 空闲期间用 WS Ping 保活（inference 连接约 60s 无帧会被服务端关闭）
    /// - 断线由 [`ws::run_with_reconnect`] 统一退避重连，未成功启动的句子会被保留重试
    pub async fn run_duplex(
        config: QwenTtsConfig,
        rx_chunks: Receiver<TtsChunkRequest>,
        playback: AudioPlaybackHandle,
        is_running: Arc<AtomicBool>,
    ) -> Result<()> {
        let mut state = TtsRuntime {
            config,
            rx_chunks,
            playback,
            pending_chunk: None,
            pending_attempts: 0,
        };
        ws::run_with_reconnect::<Self>("TTS", &is_running, &mut state).await
    }

    /// 一条连接的生命周期：连接后循环处理多个句子的 TTS 任务
    async fn run_connection(state: &mut TtsRuntime, is_running: &AtomicBool) -> Result<SessionEnd> {
        let ws_stream =
            ws::connect_authenticated(&state.config.api_key, &state.config.ws_url).await?;
        println!("✨ 百炼 TTS WebSocket 连接已建立...");

        // 连接只 split 一次并长期持有：空闲等待时也要 poll 读半边，
        // 才能消费 Ping/Pong、及时发现服务端 Close，并用写半边发送保活 Ping。
        let (mut write, mut read) = ws_stream.split();
        let mut keepalive = ws::KeepAlive::new();

        loop {
            if !is_running.load(Ordering::Relaxed) {
                return Ok(SessionEnd::Shutdown);
            }

            // 等待下一个句子切片；期间用 WS Ping 保活，避免 ~60s 空闲被服务端断开。
            // 若上一次启动失败，pending_chunk 仍在，直接重试同一句而不是取新句。
            if state.pending_chunk.is_none() {
                match Self::wait_next_chunk(
                    &mut write,
                    &mut read,
                    &mut state.rx_chunks,
                    &mut keepalive,
                    is_running,
                )
                .await
                {
                    ChunkWait::Chunk(chunk) => {
                        state.pending_chunk = Some(chunk);
                        state.pending_attempts = 0;
                    }
                    ChunkWait::End(end) => return Ok(end),
                }
            }

            let first_chunk = state
                .pending_chunk
                .clone()
                .expect("pending_chunk 已确保存在");

            // ===== 每个句子一个 task：run-task -> continue-task* -> finish-task =====
            let task_id = Uuid::new_v4().simple().to_string();

            let run_task_payload = json!({
                "header": {
                    "action": "run-task",
                    "task_id": task_id,
                    "streaming": "duplex"
                },
                "payload": {
                    "task_group": "audio",
                    "task": "tts",
                    "function": "SpeechSynthesizer",
                    "model": &state.config.tts_model,
                    "parameters": {
                        "text_type": "PlainText",
                        "format": "pcm",
                        "sample_rate": 22050,
                        "voice": &state.config.voice,
                        "volume": 50,
                        "rate": 1.0,
                        "pitch": 1.0
                    },
                    "input": {}
                }
            });
            if let Err(e) = write
                .send(Message::Text(run_task_payload.to_string()))
                .await
            {
                // 保留 pending_chunk，重连后用同一句重试
                state.note_pending_failure();
                return Ok(SessionEnd::Disconnected(format!(
                    "发送 TTS run-task 失败: {:?}",
                    e
                )));
            }

            let started = match ws::wait_for_json(&mut read, ws::CONNECT_TIMEOUT, |v| {
                matches!(
                    v["header"]["event"].as_str(),
                    Some("task-started") | Some("task-failed")
                )
            })
            .await
            {
                Ok(value) => value,
                Err(e) => {
                    state.note_pending_failure();
                    return Ok(SessionEnd::Disconnected(format!(
                        "TTS 等待 task-started 失败: {e:#}"
                    )));
                }
            };

            if started["header"]["event"] == "task-failed" {
                let msg = started["header"]["error_message"]
                    .as_str()
                    .unwrap_or("未知错误");
                state.note_pending_failure();
                return Ok(SessionEnd::Disconnected(format!(
                    "TTS task-failed: {}",
                    msg
                )));
            }

            // 任务已成功启动：pending 文本进入正常发送流程
            state.pending_chunk = None;
            let mut pending_chunk = Some(first_chunk);
            let mut finish_sent = false;

            // 全双工：一边按序发送文本切片，一边接收音频流，直到本句 task-finished
            loop {
                if let Some(chunk) = pending_chunk.take() {
                    if let Err(e) = Self::send_continue_chunk(&mut write, &task_id, &chunk).await {
                        return Ok(SessionEnd::Disconnected(format!(
                            "发送 continue-task 失败: {e:#}"
                        )));
                    }
                    if chunk.is_sentence_end {
                        if let Err(e) = Self::send_finish_task(&mut write, &task_id).await {
                            return Ok(SessionEnd::Disconnected(format!(
                                "发送 finish-task 失败: {e:#}"
                            )));
                        }
                        finish_sent = true;
                    }
                }

                tokio::select! {
                    maybe_chunk = state.rx_chunks.recv(), if !finish_sent => {
                        match maybe_chunk {
                            Some(chunk) => pending_chunk = Some(chunk),
                            None => return Ok(SessionEnd::Shutdown),
                        }
                    }
                    maybe_msg = read.next() => {
                        match maybe_msg {
                            Some(Ok(Message::Binary(pcm_bytes))) => {
                                state.playback.push_pcm16(&pcm_bytes);
                            }
                            Some(Ok(Message::Text(t))) => {
                                let Ok(resp) = serde_json::from_str::<TtsServerResponse>(&t) else {
                                    continue;
                                };
                                if let Some(err) = resp.header.error_message {
                                    eprintln!("TTS 服务端报错: {}", err);
                                    return Ok(SessionEnd::Disconnected(format!(
                                        "TTS 服务端报错: {}",
                                        err
                                    )));
                                }
                                if resp.header.event.as_deref() == Some("task-finished") {
                                    break;
                                }
                            }
                            Some(Ok(Message::Close(_))) => {
                                return Ok(SessionEnd::Disconnected(
                                    "WebSocket 被服务端关闭".to_string(),
                                ));
                            }
                            Some(Ok(_)) => {}
                            Some(Err(e)) => {
                                return Ok(SessionEnd::Disconnected(format!(
                                    "WebSocket 接收异常: {:?}",
                                    e
                                )));
                            }
                            None => {
                                return Ok(SessionEnd::Disconnected(
                                    "WebSocket 流结束".to_string(),
                                ));
                            }
                        }
                    }
                    _ = keepalive.tick() => ws::send_ping(&mut write).await,
                }
            }

            // 本句合成完成，冲刷播放缓冲，连接继续复用
            state.playback.flush();
        }
    }

    /// 空闲等待下一个句子切片：同时消费连接事件并定期发送保活 Ping
    async fn wait_next_chunk(
        write: &mut WsWrite,
        read: &mut WsRead,
        rx_chunks: &mut Receiver<TtsChunkRequest>,
        keepalive: &mut ws::KeepAlive,
        is_running: &AtomicBool,
    ) -> ChunkWait {
        loop {
            if !is_running.load(Ordering::Relaxed) {
                return ChunkWait::End(SessionEnd::Shutdown);
            }

            tokio::select! {
                maybe_chunk = rx_chunks.recv() => {
                    return match maybe_chunk {
                        Some(chunk) => ChunkWait::Chunk(chunk),
                        None => ChunkWait::End(SessionEnd::Shutdown),
                    };
                }
                maybe_msg = read.next() => {
                    match maybe_msg {
                        Some(Ok(Message::Close(_))) => {
                            return ChunkWait::End(SessionEnd::Disconnected(
                                "WebSocket 被服务端关闭".to_string(),
                            ));
                        }
                        // Pong / 其他心跳帧：读掉即可
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            return ChunkWait::End(SessionEnd::Disconnected(format!(
                                "WebSocket 接收异常: {:?}",
                                e
                            )));
                        }
                        None => {
                            return ChunkWait::End(SessionEnd::Disconnected(
                                "WebSocket 流结束".to_string(),
                            ));
                        }
                    }
                }
                _ = keepalive.tick() => ws::send_ping(write).await,
            }
        }
    }

    async fn send_continue_chunk(
        write_sink: &mut WsWrite,
        task_id: &str,
        chunk: &TtsChunkRequest,
    ) -> Result<()> {
        let continue_payload = json!({
            "header": {
                "action": "continue-task",
                "task_id": task_id,
                "streaming": "duplex"
            },
            "payload": {
                "input": {
                    "text": chunk.text
                }
            }
        });

        write_sink
            .send(Message::Text(continue_payload.to_string()))
            .await
            .context("发送 TTS continue-task 失败")?;

        Ok(())
    }

    async fn send_finish_task(write_sink: &mut WsWrite, task_id: &str) -> Result<()> {
        let finish_payload = json!({
            "header": {
                "action": "finish-task",
                "task_id": task_id,
                "streaming": "duplex"
            },
            "payload": {
                "input": {}
            }
        });

        write_sink
            .send(Message::Text(finish_payload.to_string()))
            .await
            .context("发送 TTS finish-task 失败")?;

        Ok(())
    }
}

impl ReconnectingSession for QwenTtsClient {
    type State = TtsRuntime;

    fn run_once<'a>(
        state: &'a mut TtsRuntime,
        is_running: &'a AtomicBool,
    ) -> Pin<Box<dyn Future<Output = Result<SessionEnd>> + Send + 'a>> {
        Box::pin(async move { Self::run_connection(state, is_running).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use serde_json::Value;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    /// 两条句子应在同一条 WebSocket 连接上完成（验证连接复用 + select 收发泵）
    #[tokio::test]
    async fn reuses_one_connection_for_multiple_sentences() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let completed = Arc::new(AtomicUsize::new(0));
        let completed_server = completed.clone();

        let server = tokio::spawn(async move {
            // 只接受一次连接：若客户端断开重连，第二句就无法完成
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp).await.unwrap();

            for _ in 0..2 {
                loop {
                    let Some(Ok(Message::Text(t))) = ws.next().await else {
                        return;
                    };
                    let Ok(v) = serde_json::from_str::<Value>(&t) else {
                        continue;
                    };
                    match v["header"]["action"].as_str() {
                        Some("run-task") => {
                            let started =
                                json!({ "header": { "event": "task-started" }, "payload": {} });
                            ws.send(Message::Text(started.to_string())).await.unwrap();
                        }
                        Some("continue-task") => {}
                        Some("finish-task") => {
                            // 回一段 PCM16 + task-finished
                            ws.send(Message::Binary(vec![1, 0, 2, 0])).await.unwrap();
                            let finished =
                                json!({ "header": { "event": "task-finished" }, "payload": {} });
                            ws.send(Message::Text(finished.to_string())).await.unwrap();
                            completed_server.fetch_add(1, Ordering::SeqCst);
                            break;
                        }
                        _ => {}
                    }
                }
            }

            // 保持连接存活
            let _ = ws.next().await;
        });

        let (tx_chunks, rx_chunks) = tokio::sync::mpsc::channel(8);
        let playback = AudioPlaybackHandle::for_test();
        let is_running = Arc::new(AtomicBool::new(true));
        let config = QwenTtsConfig {
            api_key: "sk-test".to_string(),
            tts_model: "qwen-tts-test".to_string(),
            voice: "test-voice".to_string(),
            ws_url: format!("ws://{addr}/api-ws/v1/inference"),
        };

        let client = tokio::spawn(QwenTtsClient::run_duplex(
            config,
            rx_chunks,
            playback.clone(),
            is_running.clone(),
        ));

        tx_chunks
            .send(TtsChunkRequest {
                text: "第一句。".to_string(),
                is_sentence_end: true,
            })
            .await
            .unwrap();
        tx_chunks
            .send(TtsChunkRequest {
                text: "第二句。".to_string(),
                is_sentence_end: true,
            })
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_secs(2)).await;

        assert_eq!(
            completed.load(Ordering::SeqCst),
            2,
            "两个句子都应在同一条连接上合成完成"
        );
        assert!(
            playback.queued_samples_len() >= 4,
            "应收到两段音频数据，实际 {} 个样本",
            playback.queued_samples_len()
        );
        assert!(!client.is_finished(), "连接应保持复用，协程不应退出");

        is_running.store(false, Ordering::SeqCst);
        client.abort();
        server.abort();
    }

    /// 连接在 run-task 阶段失败时，当前句子必须保留到重连后重试，不能丢字
    #[tokio::test]
    async fn retries_pending_chunk_after_handshake_failure() {
        use std::sync::Mutex;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let received_texts = Arc::new(Mutex::new(Vec::<String>::new()));
        let received_texts_server = received_texts.clone();

        let server = tokio::spawn(async move {
            // 第一条连接：run-task 后回 task-failed 并断开，模拟"连接已死/服务端拒绝"
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp).await.unwrap();
            let _ = ws.next().await; // 读掉 run-task
            let fail = json!({
                "header": {
                    "task_id": "mock",
                    "event": "task-failed",
                    "error_code": "CLIENT_ERROR",
                    "error_message": "mock failure"
                },
                "payload": {}
            });
            let _ = ws.send(Message::Text(fail.to_string())).await;
            let _ = ws.close(None).await;

            // 第二条连接：正常完成后记录收到的文本切片
            let (tcp2, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp2).await.unwrap();
            while let Some(Ok(Message::Text(t))) = ws.next().await {
                let Ok(v) = serde_json::from_str::<Value>(&t) else {
                    continue;
                };
                match v["header"]["action"].as_str() {
                    Some("run-task") => {
                        let started =
                            json!({ "header": { "event": "task-started" }, "payload": {} });
                        ws.send(Message::Text(started.to_string())).await.unwrap();
                    }
                    Some("continue-task") => {
                        if let Some(text) = v["payload"]["input"]["text"].as_str() {
                            received_texts_server.lock().unwrap().push(text.to_string());
                        }
                    }
                    Some("finish-task") => {
                        ws.send(Message::Binary(vec![1, 0, 2, 0])).await.unwrap();
                        let finished =
                            json!({ "header": { "event": "task-finished" }, "payload": {} });
                        ws.send(Message::Text(finished.to_string())).await.unwrap();
                    }
                    _ => {}
                }
            }
        });

        let (tx_chunks, rx_chunks) = tokio::sync::mpsc::channel(8);
        let playback = AudioPlaybackHandle::for_test();
        let is_running = Arc::new(AtomicBool::new(true));
        let config = QwenTtsConfig {
            api_key: "sk-test".to_string(),
            tts_model: "qwen-tts-test".to_string(),
            voice: "test-voice".to_string(),
            ws_url: format!("ws://{addr}/api-ws/v1/inference"),
        };

        let client = tokio::spawn(QwenTtsClient::run_duplex(
            config,
            rx_chunks,
            playback.clone(),
            is_running.clone(),
        ));

        tx_chunks
            .send(TtsChunkRequest {
                text: "第一句。".to_string(),
                is_sentence_end: true,
            })
            .await
            .unwrap();

        // 首连接 500ms 退避后重连，再完成合成
        tokio::time::sleep(Duration::from_secs(3)).await;

        assert_eq!(
            received_texts.lock().unwrap().as_slice(),
            ["第一句。".to_string()],
            "重连后应重试同一句，而不是丢失"
        );
        assert!(
            playback.queued_samples_len() >= 2,
            "重连后应完成合成并收到音频"
        );
        assert!(!client.is_finished());

        is_running.store(false, Ordering::SeqCst);
        client.abort();
        server.abort();
    }
}
