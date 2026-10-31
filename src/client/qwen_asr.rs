use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::audio::vad::SpeechGate;
use crate::client::ws::{self, AudioStreamState, ReconnectingSession, SessionEnd, WsStream};
use crate::client::TranslationEvent;

/// 近场识别：本项目输入始终为说话人自己的麦克风（耳机/桌面近讲），
/// 服务端默认远场 vad_model（far_field_meeting_16k）会降低近讲场景的 VAD 灵敏度。
const ASR_VAD_MODEL: &str = "near_meeting_16k";

#[derive(Debug, Deserialize)]
struct AsrServerResponse {
    header: AsrHeader,
    payload: AsrPayload,
}

#[derive(Debug, Deserialize)]
struct AsrHeader {
    event: String,
    error_code: Option<String>,
    error_message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AsrPayload {
    output: Option<AsrOutput>,
}

#[derive(Debug, Deserialize)]
struct AsrOutput {
    /// `task-finished` 等事件的 `output` 为空对象，句段字段必须是可缺省的
    sentence: Option<AsrSentence>,
}

#[derive(Debug, Deserialize)]
struct AsrSentence {
    #[serde(default)]
    text: String,
    #[serde(default)]
    sentence_end: bool,
    #[serde(default)]
    heartbeat: bool,
}

/// ASR 会话的共享状态（跨重连保留）
pub(crate) struct AsrRuntime {
    api_key: String,
    model: String,
    ws_url: String,
    io: AudioStreamState,
    /// 本地 VAD 发送门控；None 表示关闭（全量上传）
    gate: Option<SpeechGate>,
}

impl ws::SessionState for AsrRuntime {
    fn before_reconnect(&mut self) {
        // 丢弃断连期间积压的音频，并通知下游 chunker 丢弃半句上下文
        self.io.drain_and_reset();
        // 音频时间线已断，门控的起音/尾部状态也要重置（保留噪声底估计）
        if let Some(gate) = &mut self.gate {
            gate.reset_transient();
        }
    }
}

pub struct QwenAsrClient;

impl QwenAsrClient {
    /// 运行 Qwen Audio 3.1 实时双向流式语音识别（无翻译，直通 ASR -> TTS）。
    ///
    /// 断线重连、退避与握手超时统一由 [`ws::run_with_reconnect`] 处理。
    ///
    /// `gate` 为本地 VAD 发送门控（None 表示关闭、全量上传）。
    ///
    /// `ws_url` 为百炼业务空间专属域名的 run-task 端点，
    /// 形如 `wss://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference`。
    pub async fn run_duplex(
        api_key: String,
        model: String,
        ws_url: String,
        gate: Option<SpeechGate>,
        rx_audio: Receiver<Vec<u8>>,
        tx_events: Sender<TranslationEvent>,
        is_running: Arc<AtomicBool>,
    ) -> Result<()> {
        let mut state = AsrRuntime {
            api_key,
            model,
            ws_url,
            io: AudioStreamState {
                rx_audio,
                tx_events,
            },
            gate,
        };
        ws::run_with_reconnect::<Self>("Qwen ASR", &is_running, &mut state).await
    }

    /// 建立连接、发送 run-task 并等待 task-started
    async fn connect_and_start(
        api_key: &str,
        model: &str,
        ws_url: &str,
    ) -> Result<(WsStream, String)> {
        let task_id = Uuid::new_v4().simple().to_string();
        let mut stream = ws::connect_authenticated(api_key, ws_url).await?;

        let run_task_payload = json!({
            "header": {
                "action": "run-task",
                "task_id": task_id,
                "streaming": "duplex"
            },
            "payload": {
                "task_group": "audio",
                "task": "asr",
                "function": "recognition",
                "model": model,
                "parameters": {
                    "format": "pcm",
                    "sample_rate": 16000,
                    "vad_model": ASR_VAD_MODEL,
                    // 心跳：长时间只有静音音频时保持会话不被服务端超时断开
                    "heartbeat": true
                },
                "input": {}
            }
        });

        stream
            .send(Message::Text(run_task_payload.to_string()))
            .await
            .context("发送 Qwen ASR run-task 失败")?;

        let value = ws::wait_for_json(&mut stream, ws::CONNECT_TIMEOUT, |v| {
            matches!(
                v["header"]["event"].as_str(),
                Some("task-started") | Some("task-failed")
            )
        })
        .await
        .context("Qwen ASR 握手失败")?;

        if value["header"]["event"] == "task-failed" {
            let err_code = value["header"]["error_code"].as_str().unwrap_or_default();
            let err_msg = value["header"]["error_message"]
                .as_str()
                .unwrap_or_default();
            bail!("Qwen ASR 握手失败 [{}]: {}", err_code, err_msg);
        }

        println!("✨ Qwen-Audio-3.1 极速语音识别通道已就绪（无翻译纯识别模式）...");
        Ok((stream, task_id))
    }

    /// 单次会话的收发泵：并发上传音频 + 接收识别结果，直到会话结束
    async fn pump_session(
        ws_stream: WsStream,
        task_id: &str,
        state: &mut AsrRuntime,
        is_running: &AtomicBool,
    ) -> SessionEnd {
        let (mut write, mut read) = ws_stream.split();
        let AudioStreamState {
            rx_audio,
            tx_events,
        } = &mut state.io;
        let gate = &mut state.gate;
        let mut audio_closed = false;

        loop {
            if !is_running.load(Ordering::Relaxed) {
                Self::send_finish_task(&mut write, task_id).await;
                return SessionEnd::Shutdown;
            }

            tokio::select! {
                // 持续上传麦克风 16kHz PCM16 二进制流（经过本地 VAD 门控）
                maybe_chunk = rx_audio.recv(), if !audio_closed => {
                    match maybe_chunk {
                        Some(pcm_chunk) => {
                            let chunks: Vec<Vec<u8>> = match gate.as_mut() {
                                Some(gate) => gate.process_chunk(pcm_chunk),
                                None => vec![pcm_chunk],
                            };
                            for chunk in chunks {
                                if let Err(e) = write.send(Message::Binary(chunk)).await {
                                    return SessionEnd::Disconnected(format!("发送音频失败: {:?}", e));
                                }
                            }
                        }
                        None => {
                            // 音频采集通道已关闭（程序退出），通知服务端结束
                            audio_closed = true;
                            Self::send_finish_task(&mut write, task_id).await;
                        }
                    }
                }
                // 接收服务端识别流
                maybe_msg = read.next() => {
                    match maybe_msg {
                        Some(Ok(Message::Text(text))) => {
                            let Ok(resp) = serde_json::from_str::<AsrServerResponse>(&text) else {
                                continue;
                            };

                            match resp.header.event.as_str() {
                                "result-generated" => {
                                    let Some(sentence) = resp
                                        .payload
                                        .output
                                        .and_then(|output| output.sentence)
                                    else {
                                        continue;
                                    };

                                    if sentence.heartbeat || sentence.text.is_empty() {
                                        continue;
                                    }

                                    // 纯转写模式下，翻译字段直接回填识别文本，直接送入下游分块与 TTS
                                    let _ = tx_events
                                        .send(TranslationEvent {
                                            transcription: sentence.text.clone(),
                                            translation: sentence.text,
                                            is_sentence_end: sentence.sentence_end,
                                        })
                                        .await;
                                }
                                "task-finished" => {
                                    if audio_closed || !is_running.load(Ordering::Relaxed) {
                                        return SessionEnd::Shutdown;
                                    }
                                    return SessionEnd::Disconnected(
                                        "服务端提前返回 task-finished".to_string(),
                                    );
                                }
                                "task-failed" => {
                                    let err_code = resp.header.error_code.unwrap_or_default();
                                    let err_msg = resp.header.error_message.unwrap_or_default();
                                    return SessionEnd::Disconnected(format!(
                                        "task-failed [{}]: {}",
                                        err_code, err_msg
                                    ));
                                }
                                _ => {}
                            }
                        }
                        Some(Ok(Message::Close(_))) => {
                            return SessionEnd::Disconnected("WebSocket 被服务端关闭".to_string());
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            return SessionEnd::Disconnected(format!("WebSocket 接收异常: {:?}", e));
                        }
                        None => {
                            return SessionEnd::Disconnected("WebSocket 流结束".to_string());
                        }
                    }
                }
            }
        }
    }

    async fn send_finish_task<S>(write: &mut S, task_id: &str)
    where
        S: SinkExt<Message> + Unpin,
    {
        let finish_payload = json!({
            "header": {
                "action": "finish-task",
                "task_id": task_id,
                "streaming": "duplex"
            },
            "payload": { "input": {} }
        });
        let _ = write.send(Message::Text(finish_payload.to_string())).await;
    }
}

impl ReconnectingSession for QwenAsrClient {
    type State = AsrRuntime;

    fn run_once<'a>(
        state: &'a mut AsrRuntime,
        is_running: &'a AtomicBool,
    ) -> Pin<Box<dyn Future<Output = Result<SessionEnd>> + Send + 'a>> {
        Box::pin(async move {
            let (ws_stream, task_id) =
                Self::connect_and_start(&state.api_key, &state.model, &state.ws_url).await?;
            Ok(Self::pump_session(ws_stream, &task_id, state, is_running).await)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_task_finished_with_empty_output() {
        // 真实服务端 task-finished 的 payload.output 是空对象，不能因此解析失败
        let text = r#"{"header":{"task_id":"x","event":"task-finished","attributes":{}},"payload":{"output":{}}}"#;
        let resp: AsrServerResponse = serde_json::from_str(text).unwrap();
        assert_eq!(resp.header.event, "task-finished");
        assert!(resp
            .payload
            .output
            .as_ref()
            .and_then(|output| output.sentence.as_ref())
            .is_none());
    }

    #[test]
    fn parses_result_generated_sentence() {
        let text = r#"{"header":{"task_id":"x","event":"result-generated","attributes":{}},"payload":{"output":{"sentence":{"sentence_id":1,"begin_time":0,"end_time":780,"text":"你好","sentence_end":true,"heartbeat":false,"words":[]},"text":"你好"}}}"#;
        let resp: AsrServerResponse = serde_json::from_str(text).unwrap();
        let sentence = resp
            .payload
            .output
            .as_ref()
            .and_then(|output| output.sentence.as_ref())
            .unwrap();
        assert_eq!(sentence.text, "你好");
        assert!(sentence.sentence_end);
        assert!(!sentence.heartbeat);
    }

    #[test]
    fn parses_heartbeat_result_without_text() {
        let text = r#"{"header":{"task_id":"x","event":"result-generated","attributes":{}},"payload":{"output":{"sentence":{"sentence_id":0,"text":"","sentence_end":false,"heartbeat":true}}}}"#;
        let resp: AsrServerResponse = serde_json::from_str(text).unwrap();
        let sentence = resp
            .payload
            .output
            .as_ref()
            .and_then(|output| output.sentence.as_ref())
            .unwrap();
        assert!(sentence.heartbeat);
    }

    /// 握手失败（task-failed）后识别协程必须继续重连，而不是直接退出。
    #[tokio::test]
    async fn reconnects_after_handshake_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::net::TcpListener;
        use tokio_tungstenite::accept_async;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_server = attempts.clone();

        let server = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    break;
                };
                let attempts = attempts_server.clone();
                tokio::spawn(async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    let Ok(mut ws) = accept_async(tcp).await else {
                        return;
                    };
                    // 读掉 run-task，回一个 task-failed 模拟握手失败
                    let _ = ws.next().await;
                    let fail = json!({
                        "header": {
                            "task_id": "mock",
                            "event": "task-failed",
                            "error_code": "CLIENT_ERROR",
                            "error_message": "mock handshake failure"
                        },
                        "payload": {}
                    });
                    let _ = ws.send(Message::Text(fail.to_string())).await;
                    let _ = ws.close(None).await;
                });
            }
        });

        let (_tx_audio, rx_audio) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        let (tx_events, _rx_events) = tokio::sync::mpsc::channel(8);
        let is_running = Arc::new(AtomicBool::new(true));

        let ws_url = format!("ws://{}/api-ws/v1/inference", addr);
        let client = tokio::spawn(QwenAsrClient::run_duplex(
            "sk-test".to_string(),
            "qwen-audio-3.1-asr-flash-streaming".to_string(),
            ws_url,
            None,
            rx_audio,
            tx_events,
            is_running.clone(),
        ));

        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        assert!(!client.is_finished(), "识别协程不应在握手失败后退出");
        assert!(
            attempts.load(Ordering::SeqCst) >= 2,
            "应至少重连一次，实际建连 {} 次",
            attempts.load(Ordering::SeqCst)
        );

        is_running.store(false, Ordering::SeqCst);
        client.abort();
        server.abort();
    }

    /// 本地 VAD 门控接入 ASR 后：静音只发保活、起音补 pre-roll、语音段原样发送
    #[tokio::test]
    async fn gates_silence_and_sends_pre_roll_on_speech() {
        use crate::audio::vad::{GateConfig, SpeechGate, VadConfig};
        use std::sync::Mutex;
        use std::time::Duration;
        use tokio::net::TcpListener;
        use tokio_tungstenite::accept_async;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // (长度, 是否全零)
        let received = Arc::new(Mutex::new(Vec::<(usize, bool)>::new()));
        let received_server = received.clone();

        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp).await.unwrap();
            // 读 run-task，回 task-started
            let _ = ws.next().await;
            let started = json!({ "header": { "event": "task-started" }, "payload": {} });
            ws.send(Message::Text(started.to_string())).await.unwrap();

            while let Some(Ok(msg)) = ws.next().await {
                if let Message::Binary(bytes) = msg {
                    received_server
                        .lock()
                        .unwrap()
                        .push((bytes.len(), bytes.iter().all(|byte| *byte == 0)));
                }
            }
        });

        let (tx_audio, rx_audio) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
        let (tx_events, _rx_events) = tokio::sync::mpsc::channel(8);
        let is_running = Arc::new(AtomicBool::new(true));
        let gate = SpeechGate::new(GateConfig {
            vad: VadConfig {
                hangover_ms: 100,
                ..Default::default()
            },
            pre_roll_ms: 100,
            tail_ms: 200,
            keepalive_ms: 500,
            debug: false,
        });

        let ws_url = format!("ws://{}/api-ws/v1/inference", addr);
        let client = tokio::spawn(QwenAsrClient::run_duplex(
            "sk-test".to_string(),
            "qwen-audio-3.1-asr-flash-streaming".to_string(),
            ws_url,
            Some(gate),
            rx_audio,
            tx_events,
            is_running.clone(),
        ));

        // 20 个 100ms 静音 chunk：500ms 保活间隔 → 只应发 4 个零静音
        tokio::time::sleep(Duration::from_millis(100)).await;
        for _ in 0..20 {
            tx_audio.send(vec![0u8; 3200]).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(300)).await;

        // 一个 100ms 语音 chunk：起音 → 补发 pre-roll（上一个静音）+ 语音
        let mut tone = Vec::with_capacity(3200);
        for i in 0..1600 {
            let phase = 2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16000.0;
            let sample = (phase.sin() * 3000.0) as i16;
            tone.extend_from_slice(&sample.to_le_bytes());
        }
        tx_audio.send(tone).await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;

        let received = received.lock().unwrap().clone();
        assert_eq!(
            received.len(),
            6,
            "20 个静音 chunk 应只有 4 个保活，加语音起音的 pre-roll+语音共 6 个，实际 {:?}",
            received
        );
        assert!(
            received[0..4].iter().all(|(_, zero)| *zero),
            "保活应为零静音"
        );
        assert_eq!(received[4].0, 3200, "pre-roll 应与采集 chunk 等长");
        assert!(!received[5].1, "起音后应发送真实语音而不是静音");

        is_running.store(false, Ordering::SeqCst);
        client.abort();
        server.abort();
    }
}
