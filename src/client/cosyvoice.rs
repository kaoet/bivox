use anyhow::{Context, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::audio::playback::AudioPlaybackHandle;

const DASHSCOPE_WS_URL: &str = "wss://dashscope.aliyuncs.com/api-ws/v1/inference";
const COSYVOICE_MODEL: &str = "cosyvoice-v3.5-flash";

#[derive(Debug, Clone)]
pub struct CosyVoiceConfig {
    pub api_key: String,
    pub tts_model: String,
    pub voice: String,
    pub ref_audio: Option<PathBuf>,
    pub ref_text: Option<String>,
}

#[derive(Clone)]
pub struct TtsChunkRequest {
    pub text: String,
    pub is_sentence_end: bool,
}

#[derive(Debug, Deserialize)]
struct CosyHeader {
    event: Option<String>,
    error_code: Option<String>,
    error_message: Option<String>,
    #[allow(dead_code)]
    task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CosySentence {
    #[allow(dead_code)]
    index: Option<usize>,
    #[allow(dead_code)]
    words: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct CosyOutput {
    #[allow(dead_code)]
    sentence: Option<CosySentence>,
    #[allow(dead_code)]
    #[serde(rename = "type")]
    sentence_type: Option<String>,
    #[allow(dead_code)]
    original_text: Option<String>,
    #[allow(dead_code)]
    normalized_text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CosyPayload {
    #[allow(dead_code)]
    output: Option<CosyOutput>,
    #[allow(dead_code)]
    usage: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct CosyServerResponse {
    header: CosyHeader,
    #[allow(dead_code)]
    payload: Option<CosyPayload>,
}

pub struct CosyVoiceClient;

fn has_sentence_punctuation(s: &str) -> bool {
    let trimmed = s.trim_end();
    if let Some(last_char) = trimmed.chars().last() {
        matches!(
            last_char,
            '.' | '?'
                | '!'
                | ';'
                | ':'
                | '。'
                | '？'
                | '！'
                | '；'
                | '…'
                | '—'
                | '～'
                | ','
                | '，'
                | '、'
        )
    } else {
        false
    }
}

fn contains_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        ('\u{4E00}'..='\u{9FFF}').contains(&c)
            || ('\u{3400}'..='\u{4DBF}').contains(&c)
            || ('\u{F900}'..='\u{FAFF}').contains(&c)
    })
}

impl CosyVoiceClient {
    async fn connect_ws(
        api_key: &str,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    > {
        let mut request = DASHSCOPE_WS_URL
            .into_client_request()
            .context("创建 CosyVoice WebSocket 请求失败")?;

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
            .context("连接百炼 CosyVoice WebSocket 端点失败")?;

        Ok(ws_stream)
    }

    /// 运行百炼 CosyVoice 全双工双向异步流式语音合成服务：
    /// - 句子内切片并行持续发送（continue-task 与音频接收全双工并发）
    /// - 遇句末（is_sentence_end）发送 finish-task 确保百炼及时刷出末尾语音缓冲
    /// - 单连接跨句子长效复用，消除多重 TLS/TCP 握手延迟
    pub async fn run_duplex(
        config: CosyVoiceConfig,
        mut rx_chunks: Receiver<TtsChunkRequest>,
        playback: AudioPlaybackHandle,
        is_running: Arc<AtomicBool>,
    ) -> Result<()> {
        let extra_params = if let (Some(audio_path), Some(prompt_text)) =
            (&config.ref_audio, &config.ref_text)
        {
            match std::fs::read(audio_path) {
                Ok(pcm_or_wav) => {
                    let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&pcm_or_wav);
                    Some(json!({
                        "reference_audio": audio_b64,
                        "reference_text": prompt_text,
                    }))
                }
                Err(e) => {
                    eprintln!("读取参考音频文件失败 ({:?}): {:?}", audio_path, e);
                    None
                }
            }
        } else {
            None
        };

        let mut ws_stream_opt = None;

        while is_running.load(Ordering::Relaxed) {
            // 1. 等待当前句子的首个短语块
            let first_chunk = match rx_chunks.recv().await {
                Some(chunk) => chunk,
                None => break, // 通道已关闭
            };

            let mut sentence_done = false;

            for _attempt in 0..2 {
                if ws_stream_opt.is_none() {
                    match Self::connect_ws(&config.api_key).await {
                        Ok(ws) => {
                            ws_stream_opt = Some(ws);
                            println!("✨ 百炼 CosyVoice WebSocket 连接已建立...");
                        }
                        Err(e) => {
                            eprintln!("连接百炼 CosyVoice 端点失败: {:?}", e);
                            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                            continue;
                        }
                    }
                }

                let ws_stream = ws_stream_opt.take().unwrap();
                let (mut write, mut read) = ws_stream.split();
                let task_id = Uuid::new_v4().simple().to_string();

                let mut parameters = json!({
                    "text_type": "PlainText",
                    "format": "pcm",
                    "sample_rate": 22050,
                    "voice": config.voice,
                    "volume": 50,
                    "rate": 1.0,
                    "pitch": 1.0
                });

                if let Some(extra) = &extra_params {
                    if let (Some(dst), Some(src)) = (parameters.as_object_mut(), extra.as_object())
                    {
                        for (k, v) in src {
                            dst.insert(k.clone(), v.clone());
                        }
                    }
                }

                let model_name = if config.tts_model.is_empty() {
                    COSYVOICE_MODEL
                } else {
                    &config.tts_model
                };

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
                        "model": model_name,
                        "parameters": parameters,
                        "input": {}
                    }
                });

                if let Err(e) = write
                    .send(Message::Text(run_task_payload.to_string()))
                    .await
                {
                    eprintln!("发送 CosyVoice run-task 失败 (连接可能已断开): {:?}", e);
                    continue;
                }

                // 等待 task-started 信号
                let mut task_started = false;
                while let Some(msg_res) = read.next().await {
                    match msg_res {
                        Ok(Message::Text(t)) => {
                            if let Ok(resp) = serde_json::from_str::<CosyServerResponse>(&t) {
                                if let Some(err) = resp.header.error_message {
                                    let err_code = resp.header.error_code.unwrap_or_default();
                                    eprintln!(
                                        "CosyVoice 握手错误 [{}] (模型: {}): {}",
                                        err_code, model_name, err
                                    );
                                    break;
                                }
                                if resp.header.event.as_deref() == Some("task-started") {
                                    task_started = true;
                                    break;
                                }
                            }
                        }
                        _ => break,
                    }
                }

                if !task_started {
                    eprintln!("未收到 CosyVoice task-started 信号，尝试重建连接重试");
                    continue;
                }

                let playback_sink = playback.clone();
                let is_running_read = is_running.clone();

                // 启动当前句子的并行接收协程
                let read_handle = tokio::spawn(async move {
                    while let Some(msg_res) = read.next().await {
                        if !is_running_read.load(Ordering::Relaxed) {
                            break;
                        }

                        match msg_res {
                            Ok(Message::Binary(pcm_bytes)) => {
                                playback_sink.push_pcm16(&pcm_bytes);
                            }
                            Ok(Message::Text(t)) => {
                                if let Ok(resp) = serde_json::from_str::<CosyServerResponse>(&t) {
                                    if let Some(err) = resp.header.error_message {
                                        eprintln!("CosyVoice 服务端报错: {}", err);
                                        continue;
                                    }

                                    if resp.header.event.as_deref() == Some("task-finished") {
                                        break;
                                    }
                                }
                            }
                            _ => break,
                        }
                    }

                    read
                });

                // 发送首个切片
                let first_is_end = first_chunk.is_sentence_end;
                if let Err(e) = Self::send_continue_chunk(&mut write, &task_id, &first_chunk).await
                {
                    eprintln!("发送 CosyVoice continue-task 失败: {:?}", e);
                    let _ = read_handle.await;
                    continue;
                }

                if first_is_end {
                    // 首个短语切片已是整句末尾，立即发送 finish-task 促使服务端刷出全部尾部音频
                    let _ = Self::send_finish_task(&mut write, &task_id).await;
                } else {
                    // 首个短语尚未到达句末，继续从通道接收当前句子的后续切片并发发送
                    while let Some(chunk) = rx_chunks.recv().await {
                        let is_end = chunk.is_sentence_end;
                        if let Err(e) =
                            Self::send_continue_chunk(&mut write, &task_id, &chunk).await
                        {
                            eprintln!("发送 CosyVoice continue-task 失败: {:?}", e);
                            break;
                        }
                        if is_end {
                            let _ = Self::send_finish_task(&mut write, &task_id).await;
                            break;
                        }
                    }
                }

                // 等待服务端将当前句子的全部音频下发完毕 (task-finished)
                match read_handle.await {
                    Ok(read) => {
                        playback.flush();
                        // 重新合并读写半部，以供下一句话复用该已建立的底层 WebSocket 连接
                        if let Ok(reunited) = write.reunite(read) {
                            ws_stream_opt = Some(reunited);
                        }
                        sentence_done = true;
                        break;
                    }
                    Err(e) => {
                        eprintln!("CosyVoice 接收协程异常: {:?}", e);
                        playback.flush();
                        break;
                    }
                }
            }

            if !sentence_done {
                eprintln!("当前句子合成重试失败，跳过并继续处理后续句子");
            }
        }

        Ok(())
    }

    async fn send_continue_chunk<S>(
        write_sink: &mut S,
        task_id: &str,
        chunk: &TtsChunkRequest,
    ) -> Result<()>
    where
        S: SinkExt<Message> + Unpin,
        S::Error: std::error::Error + Send + Sync + 'static,
    {
        let send_text = if chunk.is_sentence_end && !has_sentence_punctuation(&chunk.text) {
            if contains_cjk(&chunk.text) {
                format!("{}。", chunk.text)
            } else {
                format!("{}.", chunk.text)
            }
        } else {
            chunk.text.clone()
        };

        let continue_payload = json!({
            "header": {
                "action": "continue-task",
                "task_id": task_id,
                "streaming": "duplex"
            },
            "payload": {
                "input": {
                    "text": send_text
                }
            }
        });
        println!("continue-task: {send_text}");

        write_sink
            .send(Message::Text(continue_payload.to_string()))
            .await
            .context("发送 CosyVoice continue-task 失败")?;

        Ok(())
    }

    async fn send_finish_task<S>(write_sink: &mut S, task_id: &str) -> Result<()>
    where
        S: SinkExt<Message> + Unpin,
        S::Error: std::error::Error + Send + Sync + 'static,
    {
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
            .context("发送 CosyVoice finish-task 失败")?;

        Ok(())
    }
}
