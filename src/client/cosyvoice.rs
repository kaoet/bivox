use anyhow::{Context, Result};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::mpsc::Receiver;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

use crate::audio::playback::AudioPlaybackHandle;

const DASHSCOPE_WS_URL: &str = "wss://dashscope.aliyuncs.com/api-ws/v1/inference";

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsWrite = SplitSink<WsStream, Message>;
type WsRead = SplitStream<WsStream>;

#[derive(Debug, Clone)]
pub struct CosyVoiceConfig {
    pub api_key: String,
    pub tts_model: String,
    pub voice: String,
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

impl CosyVoiceClient {
    async fn connect_ws(api_key: &str) -> Result<WsStream> {
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
        let mut ws_stream_opt = None;

        while is_running.load(Ordering::Relaxed) {
            let Some(first_chunk) = rx_chunks.recv().await else {
                break;
            };

            let Some((mut write, read, task_id)) =
                Self::start_task_with_retry(&mut ws_stream_opt, &config).await
            else {
                eprintln!("当前句子合成重试失败，跳过并继续处理后续句子");
                continue;
            };

            // 启动当前句子的并行接收协程
            let read_handle = tokio::spawn(Self::receive_task_audio(
                read,
                playback.clone(),
                is_running.clone(),
            ));

            // 持续发送当前句子的首个及后续切片，遇到句末则发送 finish-task
            let mut current_chunk = Some(first_chunk);
            while let Some(chunk) = current_chunk {
                if let Err(e) = Self::send_continue_chunk(&mut write, &task_id, &chunk).await {
                    eprintln!("发送 CosyVoice continue-task 失败: {:?}", e);
                    break;
                }
                if chunk.is_sentence_end {
                    let _ = Self::send_finish_task(&mut write, &task_id).await;
                    break;
                }
                current_chunk = rx_chunks.recv().await;
            }

            // 等待服务端将当前句子的全部音频下发完毕 (task-finished)
            match read_handle.await {
                Ok(read) => {
                    playback.flush();
                    // 重新合并读写半部，以供下一句话复用该已建立的底层 WebSocket 连接
                    if let Ok(reunited) = write.reunite(read) {
                        ws_stream_opt = Some(reunited);
                    }
                }
                Err(e) => {
                    eprintln!("CosyVoice 接收协程异常: {:?}", e);
                    playback.flush();
                }
            }
        }

        Ok(())
    }

    async fn start_task_with_retry(
        ws_stream_opt: &mut Option<WsStream>,
        config: &CosyVoiceConfig,
    ) -> Option<(WsWrite, WsRead, String)> {
        for _attempt in 0..2 {
            if ws_stream_opt.is_none() {
                match Self::connect_ws(&config.api_key).await {
                    Ok(ws) => {
                        *ws_stream_opt = Some(ws);
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
                    "model": &config.tts_model,
                    "parameters": {
                        "text_type": "PlainText",
                        "format": "pcm",
                        "sample_rate": 22050,
                        "voice": &config.voice,
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
                eprintln!("发送 CosyVoice run-task 失败 (连接可能已断开): {:?}", e);
                continue;
            }

            if Self::wait_for_task_started(&mut read, &config.tts_model).await {
                return Some((write, read, task_id));
            }

            eprintln!("未收到 CosyVoice task-started 信号，尝试重建连接重试");
        }

        None
    }

    async fn wait_for_task_started(read: &mut WsRead, model_name: &str) -> bool {
        while let Some(Ok(Message::Text(t))) = read.next().await {
            let Ok(resp) = serde_json::from_str::<CosyServerResponse>(&t) else {
                continue;
            };

            if let Some(err) = resp.header.error_message {
                let err_code = resp.header.error_code.unwrap_or_default();
                eprintln!(
                    "CosyVoice 握手错误 [{}] (模型: {}): {}",
                    err_code, model_name, err
                );
                return false;
            }

            if resp.header.event.as_deref() == Some("task-started") {
                return true;
            }
        }

        false
    }

    async fn receive_task_audio(
        mut read: WsRead,
        playback: AudioPlaybackHandle,
        is_running: Arc<AtomicBool>,
    ) -> WsRead {
        while let Some(Ok(msg)) = read.next().await {
            if !is_running.load(Ordering::Relaxed) {
                break;
            }

            match msg {
                Message::Binary(pcm_bytes) => {
                    playback.push_pcm16(&pcm_bytes);
                }
                Message::Text(t) => {
                    let Ok(resp) = serde_json::from_str::<CosyServerResponse>(&t) else {
                        continue;
                    };
                    if let Some(err) = resp.header.error_message {
                        eprintln!("CosyVoice 服务端报错: {}", err);
                        continue;
                    }
                    if resp.header.event.as_deref() == Some("task-finished") {
                        break;
                    }
                }
                _ => break,
            }
        }

        read
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
        let continue_payload = json!({
            "header": {
                "action": "continue-task",
                "task_id": task_id,
                "streaming": "duplex"
            },
            "payload": {
                "input": {
                    "text": &chunk.text
                }
            }
        });

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
