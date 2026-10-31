use anyhow::{Context, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_tungstenite::tungstenite::Message;

use crate::client::ws::{self, AudioStreamState, ReconnectingSession, SessionEnd, WsStream};
use crate::client::TranslationEvent;

/// 实时同传模型名（同时用于拼接业务空间 Realtime 端点 URL）
pub const LIVETRANSLATE_REALTIME_MODEL: &str = "qwen3.8-livetranslate-flash-realtime";

/// LiveTranslate 会话的共享状态（跨重连保留）
pub(crate) struct RealtimeRuntime {
    api_key: String,
    ws_url: String,
    target_lang: String,
    io: AudioStreamState,
}

impl ws::SessionState for RealtimeRuntime {
    fn before_reconnect(&mut self) {
        self.io.drain_and_reset();
    }
}

pub struct QwenRealtimeClient;

impl QwenRealtimeClient {
    /// 运行 Qwen LiveTranslate 实时双向同传。
    ///
    /// `ws_url` 形如 `wss://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/api-ws/v1/realtime?model=...`；
    /// 断线重连、退避与握手超时统一由 [`ws::run_with_reconnect`] 处理。
    pub async fn run_duplex(
        api_key: String,
        ws_url: String,
        target_lang: String,
        rx_audio: Receiver<Vec<u8>>,
        tx_events: Sender<TranslationEvent>,
        is_running: Arc<AtomicBool>,
    ) -> Result<()> {
        let mut state = RealtimeRuntime {
            api_key,
            ws_url,
            target_lang,
            io: AudioStreamState {
                rx_audio,
                tx_events,
            },
        };
        ws::run_with_reconnect::<Self>("Qwen LiveTranslate", &is_running, &mut state).await
    }

    /// 建立连接并完成 session.created → session.update → session.updated 握手
    async fn connect_and_configure(state: &RealtimeRuntime) -> Result<WsStream> {
        let mut stream = ws::connect_authenticated(&state.api_key, &state.ws_url).await?;

        let created = ws::wait_for_json(&mut stream, ws::CONNECT_TIMEOUT, |v| {
            matches!(v["type"].as_str(), Some("session.created") | Some("error"))
        })
        .await
        .context("Qwen LiveTranslate 等待 session.created 失败")?;
        ensure_no_error(&created, "session.created")?;

        let update_event = json!({
            "event_id": "event_session_init",
            "type": "session.update",
            "session": {
                "modalities": ["text"],
                "voice": "Tina",
                "translation": {
                    "language": state.target_lang
                }
            }
        });
        stream
            .send(Message::Text(update_event.to_string()))
            .await
            .context("发送 Qwen LiveTranslate session.update 失败")?;

        let updated = ws::wait_for_json(&mut stream, ws::CONNECT_TIMEOUT, |v| {
            matches!(v["type"].as_str(), Some("session.updated") | Some("error"))
        })
        .await
        .context("Qwen LiveTranslate 等待 session.updated 失败")?;
        ensure_no_error(&updated, "session.updated")?;

        println!("✨ Qwen3.8-LiveTranslate 同传通道已就绪，正在全双工流式互译...");
        Ok(stream)
    }

    /// 单次会话的收发泵：上传 Base64 音频 + 接收实时事件
    async fn pump_session(
        ws_stream: WsStream,
        io: &mut AudioStreamState,
        is_running: &AtomicBool,
    ) -> SessionEnd {
        let (mut write, mut read) = ws_stream.split();

        let mut current_source_text = String::new();
        let mut current_trans_text = String::new();

        loop {
            if !is_running.load(Ordering::Relaxed) {
                return SessionEnd::Shutdown;
            }

            tokio::select! {
                // 持续上传麦克风音频（Realtime 协议要求 Base64 文本事件）
                maybe_chunk = io.rx_audio.recv() => {
                    match maybe_chunk {
                        Some(pcm_chunk) => {
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&pcm_chunk);
                            let append_event = json!({
                                "type": "input_audio_buffer.append",
                                "audio": b64
                            });
                            if let Err(e) = write.send(Message::Text(append_event.to_string())).await {
                                return SessionEnd::Disconnected(format!("发送音频失败: {:?}", e));
                            }
                        }
                        None => return SessionEnd::Shutdown,
                    }
                }
                // 解析实时事件流
                maybe_msg = read.next() => {
                    match maybe_msg {
                        Some(Ok(Message::Text(text))) => {
                            let Ok(val) = serde_json::from_str::<Value>(&text) else {
                                continue;
                            };
                            let event_type = val["type"].as_str().unwrap_or_default();

                            match event_type {
                                // 原文语音转写完成
                                "conversation.item.input_audio_transcription.completed" => {
                                    if let Some(t) = val["transcript"].as_str() {
                                        current_source_text = t.to_string();
                                    }
                                }
                                // 译文流式文本增量
                                "response.audio_transcript.delta" => {
                                    if let Some(delta) = val["delta"].as_str() {
                                        current_trans_text.push_str(delta);
                                        let _ = io.tx_events.send(TranslationEvent {
                                            transcription: current_source_text.clone(),
                                            translation: current_trans_text.clone(),
                                            is_sentence_end: false,
                                        }).await;
                                    }
                                }
                                // 译文整句或单轮完成
                                "response.audio_transcript.done" => {
                                    if let Some(full) = val["transcript"].as_str() {
                                        current_trans_text = full.to_string();
                                    }
                                    let _ = io.tx_events.send(TranslationEvent {
                                        transcription: current_source_text.clone(),
                                        translation: current_trans_text.clone(),
                                        is_sentence_end: true,
                                    }).await;

                                    current_source_text.clear();
                                    current_trans_text.clear();
                                }
                                // 备用：仅文本输出的事件
                                "response.text.delta" => {
                                    if let Some(delta) = val["delta"].as_str() {
                                        current_trans_text.push_str(delta);
                                        let _ = io.tx_events.send(TranslationEvent {
                                            transcription: current_source_text.clone(),
                                            translation: current_trans_text.clone(),
                                            is_sentence_end: false,
                                        }).await;
                                    }
                                }
                                "response.text.done" => {
                                    if let Some(full) = val["text"].as_str() {
                                        current_trans_text = full.to_string();
                                    }
                                    let _ = io.tx_events.send(TranslationEvent {
                                        transcription: current_source_text.clone(),
                                        translation: current_trans_text.clone(),
                                        is_sentence_end: true,
                                    }).await;

                                    current_source_text.clear();
                                    current_trans_text.clear();
                                }
                                "error" => {
                                    let err_msg = val["error"]["message"]
                                        .as_str()
                                        .unwrap_or(&text);
                                    eprintln!("Qwen LiveTranslate 服务端报错: {}", err_msg);
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
}

impl ReconnectingSession for QwenRealtimeClient {
    type State = RealtimeRuntime;

    fn run_once<'a>(
        state: &'a mut RealtimeRuntime,
        is_running: &'a AtomicBool,
    ) -> Pin<Box<dyn Future<Output = Result<SessionEnd>> + Send + 'a>> {
        Box::pin(async move {
            let ws_stream = Self::connect_and_configure(state).await?;
            Ok(Self::pump_session(ws_stream, &mut state.io, is_running).await)
        })
    }
}

fn ensure_no_error(value: &Value, stage: &str) -> Result<()> {
    if value["type"] == "error" {
        let err_msg = value["error"]["message"].as_str().unwrap_or("未知错误");
        anyhow::bail!("Qwen LiveTranslate {} 阶段出错: {}", stage, err_msg);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    /// 转发翻译事件，并在服务端主动断开后自动重连
    #[tokio::test]
    async fn forwards_events_and_reconnects() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let connections_server = connections.clone();

        let server = tokio::spawn(async move {
            for index in 0..2 {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                connections_server.fetch_add(1, Ordering::SeqCst);
                let Ok(mut ws) = accept_async(tcp).await else {
                    return;
                };

                ws.send(Message::Text(
                    json!({ "type": "session.created", "session": {} }).to_string(),
                ))
                .await
                .unwrap();

                // 等待 session.update，回复 session.updated
                loop {
                    let Some(Ok(Message::Text(t))) = ws.next().await else {
                        return;
                    };
                    let Ok(v) = serde_json::from_str::<Value>(&t) else {
                        continue;
                    };
                    if v["type"] == "session.update" {
                        ws.send(Message::Text(
                            json!({ "type": "session.updated", "session": {} }).to_string(),
                        ))
                        .await
                        .unwrap();
                        break;
                    }
                }

                if index == 0 {
                    // 推一条完整的翻译事件后主动断开，触发客户端重连
                    for event in [
                        json!({
                            "type": "conversation.item.input_audio_transcription.completed",
                            "transcript": "你好"
                        }),
                        json!({
                            "type": "response.audio_transcript.delta",
                            "delta": "Hello"
                        }),
                        json!({
                            "type": "response.audio_transcript.done",
                            "transcript": "Hello"
                        }),
                    ] {
                        ws.send(Message::Text(event.to_string())).await.unwrap();
                    }
                    let _ = ws.close(None).await;
                    continue;
                }

                // 第二条连接保持存活
                let _ = ws.next().await;
            }
        });

        let (tx_audio, rx_audio) = tokio::sync::mpsc::channel(8);
        let (tx_events, mut rx_events) = tokio::sync::mpsc::channel(16);
        let is_running = Arc::new(AtomicBool::new(true));

        let client = tokio::spawn(QwenRealtimeClient::run_duplex(
            "sk-test".to_string(),
            format!("ws://{addr}/api-ws/v1/realtime?model=test"),
            "en".to_string(),
            rx_audio,
            tx_events,
            is_running.clone(),
        ));

        // 推一个音频包，驱动 append 发送分支
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = tx_audio.send(vec![0u8; 3200]).await;

        // 1) 应转发完整翻译事件
        let mut got_final = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline - tokio::time::Instant::now();
            match tokio::time::timeout(remaining, rx_events.recv()).await {
                Ok(Some(event)) if event.translation == "Hello" && event.is_sentence_end => {
                    got_final = true;
                    break;
                }
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => break,
            }
        }
        assert!(got_final, "应转发完整的翻译事件");

        // 2) 服务端断开后应自动重连
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            if connections.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            connections.load(Ordering::SeqCst) >= 2,
            "服务端断开后应重连，实际建连 {} 次",
            connections.load(Ordering::SeqCst)
        );
        assert!(!client.is_finished(), "重连后协程应继续运行");

        is_running.store(false, Ordering::SeqCst);
        client.abort();
        server.abort();
    }
}
