use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::client::qwen_realtime::TranslationEvent;

const DASHSCOPE_WS_URL: &str = "wss://dashscope.aliyuncs.com/api-ws/v1/inference";

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
    sentence: AsrSentence,
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

pub struct QwenAsrClient;

impl QwenAsrClient {
    /// 运行 Qwen Audio 3.1 实时双向流式语音识别（无翻译，直通 ASR -> TTS）
    pub async fn run_duplex(
        api_key: String,
        model: String,
        mut rx_audio: Receiver<Vec<u8>>,
        tx_events: Sender<TranslationEvent>,
        is_running: Arc<AtomicBool>,
    ) -> Result<()> {
        let task_id = Uuid::new_v4().simple().to_string();

        let mut request = DASHSCOPE_WS_URL
            .into_client_request()
            .context("创建 Qwen ASR WebSocket 请求失败")?;

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
            .context("连接百炼 ASR WebSocket 端点失败")?;

        let (mut write, mut read) = ws_stream.split();

        // 1. 发送 run-task 任务握手
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
                    "sample_rate": 16000
                },
                "input": {}
            }
        });

        write
            .send(Message::Text(run_task_payload.to_string()))
            .await
            .context("发送 Qwen ASR run-task 失败")?;

        // 2. 等待握手确认 (task-started)
        let mut task_started = false;
        while let Some(msg_res) = read.next().await {
            match msg_res {
                Ok(Message::Text(text)) => {
                    let resp: AsrServerResponse =
                        serde_json::from_str(&text).context("解析 Qwen ASR 握手响应失败")?;
                    match resp.header.event.as_str() {
                        "task-started" => {
                            task_started = true;
                            break;
                        }
                        "task-failed" => {
                            let err_code = resp.header.error_code.unwrap_or_default();
                            let err_msg = resp.header.error_message.unwrap_or_default();
                            bail!("Qwen ASR 握手失败 [{}]: {}", err_code, err_msg);
                        }
                        _ => {}
                    }
                }
                Ok(Message::Close(_)) => bail!("Qwen ASR WebSocket 在握手完成前被关闭"),
                Err(e) => bail!("Qwen ASR 连接异常: {:?}", e),
                _ => {}
            }
        }

        if !task_started {
            bail!("Qwen ASR 未能成功启动 (未收到 task-started)");
        }

        println!("✨ Qwen-Audio-3.1 极速语音识别通道已就绪（无翻译纯识别模式）...");

        let running_clone = is_running.clone();
        let task_id_clone = task_id.clone();

        // 3. 协程 A：持续上传麦克风 16kHz PCM16 二进制流
        let send_handle = tokio::spawn(async move {
            while running_clone.load(Ordering::Relaxed) {
                match rx_audio.recv().await {
                    Some(pcm_chunk) => {
                        if write.send(Message::Binary(pcm_chunk)).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }

            // 退出时发送 finish-task
            let finish_payload = json!({
                "header": {
                    "action": "finish-task",
                    "task_id": task_id_clone,
                    "streaming": "duplex"
                },
                "payload": { "input": {} }
            });
            let _ = write.send(Message::Text(finish_payload.to_string())).await;
        });

        // 4. 协程 B：接收服务端识别流
        while let Some(msg_res) = read.next().await {
            if !is_running.load(Ordering::Relaxed) {
                break;
            }

            match msg_res {
                Ok(Message::Text(text)) => {
                    let Ok(resp) = serde_json::from_str::<AsrServerResponse>(&text) else {
                        continue;
                    };

                    match resp.header.event.as_str() {
                        "result-generated" => {
                            if let Some(output) = resp.payload.output {
                                let sentence = output.sentence;
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
                        }
                        "task-finished" => break,
                        "task-failed" => {
                            let err_code = resp.header.error_code.unwrap_or_default();
                            let err_msg = resp.header.error_message.unwrap_or_default();
                            eprintln!("Qwen ASR 任务失败 [{}]: {}", err_code, err_msg);
                            break;
                        }
                        _ => {}
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    eprintln!("Qwen ASR WebSocket 接收异常: {:?}", e);
                    break;
                }
                _ => {}
            }
        }

        let _ = send_handle.await;
        Ok(())
    }
}
