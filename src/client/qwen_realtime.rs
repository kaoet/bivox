use anyhow::{Context, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Clone)]
pub struct TranslationEvent {
    #[allow(dead_code)]
    pub transcription: String,
    pub translation: String,
    pub is_sentence_end: bool,
}

const QWEN_REALTIME_WS_URL: &str =
    "wss://dashscope.aliyuncs.com/api-ws/v1/realtime?model=qwen3.8-livetranslate-flash-realtime";

pub struct QwenRealtimeClient;

impl QwenRealtimeClient {
    pub async fn run_duplex(
        api_key: String,
        _source_lang: String,
        target_lang: String,
        mut rx_audio: Receiver<Vec<u8>>,
        tx_events: Sender<TranslationEvent>,
        is_running: Arc<AtomicBool>,
    ) -> Result<()> {
        let mut request = QWEN_REALTIME_WS_URL
            .into_client_request()
            .context("创建 Qwen LiveTranslate WebSocket 请求失败")?;

        let auth_val = format!("Bearer {}", api_key);
        request.headers_mut().insert(
            "Authorization",
            auth_val.parse().context("无效的 Authorization Header")?,
        );
        request.headers_mut().insert(
            "X-DashScope-DataInspection",
            HeaderValue::from_static("enable"),
        );

        let (ws_stream, _) = connect_async(request)
            .await
            .context("连接百炼 Qwen LiveTranslate Realtime 端点失败")?;

        let (mut write, mut read) = ws_stream.split();

        // 1. 等待 session.created，然后发送 session.update 配置目标翻译语言
        let mut session_ready = false;
        while let Some(msg_res) = read.next().await {
            match msg_res {
                Ok(Message::Text(t)) => {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&t) {
                        let event_type = val["type"].as_str().unwrap_or_default();
                        if event_type == "session.created" {
                            let update_event = json!({
                                "event_id": "event_session_init",
                                "type": "session.update",
                                "session": {
                                    "modalities": ["text"],
                                    "voice": "Tina",
                                    "translation": {
                                        "language": target_lang
                                    }
                                }
                            });
                            write
                                .send(Message::Text(update_event.to_string()))
                                .await
                                .context("发送 Qwen LiveTranslate session.update 失败")?;
                        } else if event_type == "session.updated" {
                            session_ready = true;
                            break;
                        } else if event_type == "error" {
                            let err_msg = val["error"]["message"].as_str().unwrap_or("未知错误");
                            return Err(anyhow::anyhow!("Qwen LiveTranslate 初始化错误: {}", err_msg));
                        }
                    }
                }
                Ok(Message::Close(_)) => {
                    return Err(anyhow::anyhow!("WebSocket 在会话建立前已断开"));
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("Qwen WebSocket 异常: {:?}", e));
                }
                _ => {}
            }
        }

        if !session_ready {
            return Err(anyhow::anyhow!("Qwen LiveTranslate 会话未建立成功"));
        }

        println!("✨ Qwen3.8-LiveTranslate 同传通道已就绪，正在全双工流式互译...");

        let running_clone = is_running.clone();

        // 2. 协程 A：持续上传麦克风录音 PCM16 数据 (Base64)
        let send_handle = tokio::spawn(async move {
            while running_clone.load(Ordering::Relaxed) {
                match rx_audio.recv().await {
                    Some(pcm_chunk) => {
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&pcm_chunk);
                        let append_event = json!({
                            "type": "input_audio_buffer.append",
                            "audio": b64
                        });
                        if write.send(Message::Text(append_event.to_string())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        });

        // 3. 协程 B：解析实时事件流
        let mut current_source_text = String::new();
        let mut current_trans_text = String::new();

        while let Some(msg_res) = read.next().await {
            if !is_running.load(Ordering::Relaxed) {
                break;
            }

            match msg_res {
                Ok(Message::Text(text)) => {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
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
                                    let _ = tx_events.send(TranslationEvent {
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
                                let _ = tx_events.send(TranslationEvent {
                                    transcription: current_source_text.clone(),
                                    translation: current_trans_text.clone(),
                                    is_sentence_end: true,
                                }).await;

                                // 换句重置
                                current_source_text.clear();
                                current_trans_text.clear();
                            }
                            // 备用：response.text.delta / response.text.done
                            "response.text.delta" => {
                                if let Some(delta) = val["delta"].as_str() {
                                    current_trans_text.push_str(delta);
                                    let _ = tx_events.send(TranslationEvent {
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
                                let _ = tx_events.send(TranslationEvent {
                                    transcription: current_source_text.clone(),
                                    translation: current_trans_text.clone(),
                                    is_sentence_end: true,
                                }).await;

                                current_source_text.clear();
                                current_trans_text.clear();
                            }
                            "error" => {
                                let err_msg = val["error"]["message"].as_str().unwrap_or(&text);
                                eprintln!("Qwen LiveTranslate 服务端报错: {}", err_msg);
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    eprintln!("Qwen WebSocket 异常: {:?}", e);
                    break;
                }
                _ => {}
            }
        }

        let _ = send_handle.await;
        Ok(())
    }
}
