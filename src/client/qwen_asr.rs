use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::client::AsrEvent;
use crate::client::ws::{self, AudioStreamState, ReconnectingSession, SessionEnd, WsStream};

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
    /// 句子 id（同一句重复下发最终结果时相同）
    #[serde(default)]
    sentence_id: Option<u64>,
}

/// ASR 会话的共享状态（跨重连保留）
pub(crate) struct AsrRuntime {
    api_key: String,
    model: String,
    ws_url: String,
    language_hints: Vec<String>,
    /// 预编译热词列表 ID（D31）；`None` = 本次识别不使用热词。
    vocabulary_id: Option<String>,
    io: AudioStreamState,
    /// 本地 VAD 线程的重置标志；None 表示未启用门控（全量上传）。
    vad_reset: Option<Arc<AtomicBool>>,
}

impl ws::SessionState for AsrRuntime {
    fn before_reconnect(&mut self) {
        // 丢弃断连期间积压的音频，并通知下游 chunker 丢弃半句上下文
        self.io.drain_and_reset();
        // 音频时间线已断：请 VAD 线程在下一块音频前重置瞬态（模型 state/起音/尾部）。
        if let Some(flag) = &self.vad_reset {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

pub struct QwenAsrClient;

impl QwenAsrClient {
    /// 运行 Qwen Audio 3.1 实时双向流式语音识别（直通 ASR -> TTS）。
    ///
    /// 断线重连、退避与握手超时统一由 [`ws::run_with_reconnect`] 处理。
    ///
    /// `vad_reset` 为本地 VAD 线程的重置标志（断线重连时置位）；None 表示未启用门控。
    ///
    /// `ws_url` 为百炼业务空间专属域名的 run-task 端点，
    /// 形如 `wss://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference`。
    ///
    /// `vocabulary_id` 为百炼预编译热词列表 ID（D31）；`None` 表示不带热词。
    #[allow(clippy::too_many_arguments)]
    pub async fn run_duplex(
        api_key: String,
        model: String,
        ws_url: String,
        language_hints: Vec<String>,
        vocabulary_id: Option<String>,
        vad_reset: Option<Arc<AtomicBool>>,
        rx_audio: Receiver<Vec<u8>>,
        tx_events: Sender<AsrEvent>,
        is_running: Arc<AtomicBool>,
    ) -> Result<()> {
        let mut state = AsrRuntime {
            api_key,
            model,
            ws_url,
            language_hints,
            vocabulary_id,
            io: AudioStreamState {
                rx_audio,
                tx_events,
            },
            vad_reset,
        };
        ws::run_with_reconnect::<Self>("Qwen ASR", &is_running, &mut state).await
    }

    /// 建立连接、发送 run-task 并等待 task-started
    async fn connect_and_start(
        api_key: &str,
        model: &str,
        ws_url: &str,
        language_hints: &[String],
        vocabulary_id: Option<&str>,
    ) -> Result<(WsStream, String)> {
        let task_id = Uuid::new_v4().simple().to_string();
        let mut stream = ws::connect_authenticated(api_key, ws_url).await?;

        let parameters = run_task_parameters(language_hints, vocabulary_id);

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
                "parameters": parameters,
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

        log::info!("✨ Qwen-Audio-3.1 极速语音识别通道已就绪...");
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
        let mut audio_closed = false;

        loop {
            if !is_running.load(Ordering::Relaxed) {
                Self::send_finish_task(&mut write, task_id).await;
                return SessionEnd::Shutdown;
            }

            tokio::select! {
                // 持续上传（已由独立 VAD 线程门控的）16kHz PCM16 二进制流
                maybe_chunk = rx_audio.recv(), if !audio_closed => {
                    match maybe_chunk {
                        Some(pcm_chunk) => {
                            if let Err(e) = write.send(Message::Binary(pcm_chunk)).await {
                                return SessionEnd::Disconnected(format!("发送音频失败: {:?}", e));
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

                                    // 识别文本直接送入下游分块与 TTS
                                    let _ = tx_events
                                        .send(AsrEvent {
                                            text: sentence.text,
                                            is_sentence_end: sentence.sentence_end,
                                            sentence_id: sentence.sentence_id,
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
            let (ws_stream, task_id) = Self::connect_and_start(
                &state.api_key,
                &state.model,
                &state.ws_url,
                &state.language_hints,
                state.vocabulary_id.as_deref(),
            )
            .await?;
            Ok(Self::pump_session(ws_stream, &task_id, state, is_running).await)
        })
    }
}

/// 组装 run-task 的 `payload.parameters`。
///
/// 语言提示与预编译热词都只在非空时出现：没有配置热词就不能带 `vocabulary_id`
/// 空串（D31），语言列表为空时也不发空数组。
fn run_task_parameters(
    language_hints: &[String],
    vocabulary_id: Option<&str>,
) -> serde_json::Value {
    let mut parameters = json!({
        "format": "pcm",
        "sample_rate": 16000,
        "vad_model": ASR_VAD_MODEL,
        // 心跳：长时间只有静音音频时保持会话不被服务端超时断开
        "heartbeat": true
    });
    if !language_hints.is_empty() {
        parameters["language_hints"] = json!(language_hints);
    }
    if let Some(vocabulary_id) = vocabulary_id.filter(|id| !id.is_empty()) {
        // D31：预编译热词列表 ID；必须放在 payload.parameters 里。
        parameters["vocabulary_id"] = json!(vocabulary_id);
    }
    parameters
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
        assert!(
            resp.payload
                .output
                .as_ref()
                .and_then(|output| output.sentence.as_ref())
                .is_none()
        );
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
        // 句子 id 必须解析出来：字幕用它区分「同一句重复下发」和「两次相同的发音」
        assert_eq!(sentence.sentence_id, Some(1));
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
            vec!["zh".to_string()],
            None,
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

    /// 断线重连应请求 VAD 线程重置瞬态（否则模型 state/起音拖尾会跨连接串音）。
    #[test]
    fn reconnect_requests_vad_reset() {
        use crate::client::ws::SessionState;
        let (_tx_audio, rx_audio) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        let (tx_events, _rx_events) = tokio::sync::mpsc::channel(1);
        let reset = Arc::new(AtomicBool::new(false));
        let mut runtime = AsrRuntime {
            api_key: "sk-test".to_string(),
            model: "qwen-audio-3.1-asr-flash-streaming".to_string(),
            ws_url: "ws://127.0.0.1".to_string(),
            language_hints: vec!["zh".to_string()],
            vocabulary_id: None,
            io: AudioStreamState {
                rx_audio,
                tx_events,
            },
            vad_reset: Some(reset.clone()),
        };
        runtime.before_reconnect();
        assert!(reset.load(Ordering::SeqCst), "重连应置位 VAD 重置标志");
    }

    /// run-task 的 parameters 只带配置了的语言提示与热词（D31）。
    #[test]
    fn run_task_parameters_only_include_configured_fields() {
        let hints = vec!["zh".to_string(), "ja".to_string()];

        let plain = run_task_parameters(&hints, None);
        assert_eq!(plain["language_hints"], json!(["zh", "ja"]));
        assert!(
            plain.get("vocabulary_id").is_none(),
            "未配置热词时不应出现 vocabulary_id"
        );
        assert_eq!(plain["sample_rate"], 16000);

        let with_vocabulary = run_task_parameters(&hints, Some("vocab-1"));
        assert_eq!(with_vocabulary["vocabulary_id"], "vocab-1");

        // 空串等于没配置；空语言列表不写空数组
        let empty = run_task_parameters(&[], Some(""));
        assert!(empty.get("language_hints").is_none());
        assert!(empty.get("vocabulary_id").is_none());
    }

    /// 配置了预编译热词时，run-task 应在 payload.parameters 里带 vocabulary_id（D31）。
    #[tokio::test]
    async fn sends_vocabulary_id_when_configured() {
        use std::time::Duration;
        use tokio::net::TcpListener;
        use tokio::sync::oneshot;
        use tokio_tungstenite::accept_async;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = oneshot::channel();

        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp).await.unwrap();
            if let Some(Ok(Message::Text(text))) = ws.next().await {
                let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                let _ = tx.send(value);
            }
            let _ = ws.next().await;
        });

        let (_tx_audio, rx_audio) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        let (tx_events, _rx_events) = tokio::sync::mpsc::channel(8);
        let is_running = Arc::new(AtomicBool::new(true));
        let client = tokio::spawn(QwenAsrClient::run_duplex(
            "sk-test".to_string(),
            "qwen-audio-3.1-asr-flash-streaming".to_string(),
            format!("ws://{addr}/api-ws/v1/inference"),
            vec!["zh".to_string()],
            Some("vocab-bivox-1".to_string()),
            None,
            rx_audio,
            tx_events,
            is_running.clone(),
        ));

        let value = tokio::time::timeout(Duration::from_secs(3), rx)
            .await
            .expect("应收到 run-task")
            .expect("run-task 解析失败");
        assert_eq!(
            value["payload"]["parameters"]["vocabulary_id"], "vocab-bivox-1",
            "vocabulary_id 必须放在 payload.parameters 里（D31）"
        );
        assert_eq!(
            value["payload"]["parameters"]["language_hints"],
            json!(["zh"]),
            "vocabulary_id 与 language_hints 可以同时使用"
        );

        is_running.store(false, Ordering::SeqCst);
        client.abort();
        server.abort();
    }
}
