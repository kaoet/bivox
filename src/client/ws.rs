//! 百炼 WebSocket 客户端的公共底座。
//!
//! ASR（run-task 协议）与 TTS（run-task 协议）的业务事件完全不同，
//! 但"鉴权建连 → 协议握手 → 收发 → 断线退避重连"的骨架是一致的。
//! 本模块只抽这条骨架，协议细节仍留在各自客户端里。

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, Stream, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{Interval, MissedTickBehavior};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::client::AsrEvent;

pub(crate) type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub(crate) type WsRead = SplitStream<WsStream>;
pub(crate) type WsWrite = SplitSink<WsStream, Message>;

/// WebSocket 建连 / 握手超时
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 连接级保活周期。
///
/// 实测：inference 连接（run-task 协议）在连续约 60s 没有任何帧时会被服务端关闭。
/// WebSocket Ping 是协议帧，不产生任何音频/文本 Token，能让连接续命（实测 240s/360s 仍存活）。
pub(crate) const KEEPALIVE_PERIOD: Duration = Duration::from_secs(20);
/// 重连指数退避：初始间隔与上限
const BASE_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(8);
/// 会话存活超过该时长后，下次断连重新从最小退避开始
const BACKOFF_RESET_AFTER: Duration = Duration::from_secs(10);

/// 建立到百炼业务空间域名的 WebSocket 连接（统一鉴权头 + 建连超时）
pub(crate) async fn connect_authenticated(api_key: &str, url: &str) -> Result<WsStream> {
    let mut request = url
        .into_client_request()
        .context("创建 WebSocket 请求失败")?;

    let auth_val = format!("Bearer {}", api_key);
    request.headers_mut().insert(
        "Authorization",
        auth_val.parse().context("无效的 Authorization Header")?,
    );
    request.headers_mut().insert(
        "X-DashScope-DataInspection",
        HeaderValue::from_static("enable"),
    );

    let (stream, _) = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(request))
        .await
        .context("连接百炼 WebSocket 超时")?
        .context("连接百炼 WebSocket 端点失败")?;

    Ok(stream)
}

/// WebSocket 连接级保活器：定期在写半边发送 Ping 帧。
///
/// 只解决"连接空闲被服务端断开"，不能替代业务层心跳：
/// run-task 任务启动后若没有音频，仍会以 `request timeout after 23 seconds` 失败。
pub(crate) struct KeepAlive {
    interval: Interval,
}

impl KeepAlive {
    pub(crate) fn new() -> Self {
        Self::with_period(KEEPALIVE_PERIOD)
    }

    pub(crate) fn with_period(period: Duration) -> Self {
        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // interval 的第一次 tick 默认立即完成；reset 后改为 period 之后才触发
        interval.reset();
        Self { interval }
    }

    /// 等待到下一个保活周期
    pub(crate) async fn tick(&mut self) {
        self.interval.tick().await;
    }
}

impl Default for KeepAlive {
    fn default() -> Self {
        Self::new()
    }
}

/// 发送一帧 WS Ping（忽略失败：连接是否已死由读半边/业务分支负责判定）
pub(crate) async fn send_ping(write: &mut WsWrite) {
    let _ = write.send(Message::Ping(Vec::new())).await;
}

/// 持续读取 JSON 文本事件，直到 `predicate` 命中并返回该事件。
///
/// 二进制消息与非 JSON 文本会被忽略；超时、服务端关闭、连接异常与流结束均返回错误。
/// 用于各协议的握手阶段（等待 `task-started` / `session.created` 等）。
pub(crate) async fn wait_for_json<S, F>(
    stream: &mut S,
    timeout: Duration,
    mut predicate: F,
) -> Result<Value>
where
    S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
    F: FnMut(&Value) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("等待服务端事件超时");
        }

        let msg = tokio::time::timeout(remaining, stream.next())
            .await
            .context("等待服务端事件超时")?;

        match msg {
            Some(Ok(Message::Text(text))) => {
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    if predicate(&value) {
                        return Ok(value);
                    }
                }
            }
            Some(Ok(Message::Close(_))) => bail!("WebSocket 在握手完成前被服务端关闭"),
            Some(Err(e)) => bail!("WebSocket 握手期间连接异常: {:?}", e),
            Some(Ok(_)) => {}
            None => bail!("WebSocket 在握手完成前结束"),
        }
    }
}

/// 单次会话的结束方式
pub(crate) enum SessionEnd {
    /// 正常运行结束（进程退出 / 音频或文本通道关闭）
    Shutdown,
    /// 异常断开，需要重连
    Disconnected(String),
}

/// 可重连会话的共享状态：每次重连前由框架回调做清理
pub(crate) trait SessionState {
    /// 断线重连前调用：丢弃积压数据、通知下游重置等
    fn before_reconnect(&mut self) {}
}

/// 一个能用断线重连框架驱动的流式会话
pub(crate) trait ReconnectingSession {
    type State: SessionState + Send;

    /// 跑一次完整会话：正常收尾返回 `Shutdown`，断线返回 `Disconnected`，
    /// 建连/握手失败返回 `Err`（三者都会触发框架的退避重连，除非是 `Shutdown`）。
    fn run_once<'a>(
        state: &'a mut Self::State,
        is_running: &'a AtomicBool,
    ) -> Pin<Box<dyn Future<Output = Result<SessionEnd>> + Send + 'a>>;
}

/// 统一的重连驱动：指数退避（500ms → 8s），
/// 长时间存活的会话断连后重新从最小退避开始。
pub(crate) async fn run_with_reconnect<P>(
    name: &str,
    is_running: &AtomicBool,
    state: &mut P::State,
) -> Result<()>
where
    P: ReconnectingSession,
{
    let mut backoff = BASE_BACKOFF;
    let mut attempts: u64 = 0;

    while is_running.load(Ordering::Relaxed) {
        attempts += 1;
        let session_started_at = Instant::now();

        match P::run_once(state, is_running).await {
            Ok(SessionEnd::Shutdown) => return Ok(()),
            Ok(SessionEnd::Disconnected(reason)) => {
                eprintln!("⚠️ {name} 会话断开: {reason}");
            }
            Err(e) => {
                eprintln!("⚠️ {name} 建连/握手失败: {e:#}");
            }
        }

        if !is_running.load(Ordering::Relaxed) {
            return Ok(());
        }

        state.before_reconnect();

        if session_started_at.elapsed() >= BACKOFF_RESET_AFTER {
            backoff = BASE_BACKOFF;
        }

        eprintln!(
            "🔄 {:.1}s 后重连 {name}（累计第 {} 次连接尝试）...",
            backoff.as_secs_f64(),
            attempts
        );
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }

    Ok(())
}

/// 音频输入 / 识别事件输出：ASR 会话状态。
///
/// 断连期间采集端仍在 `try_send`，channel 里会积压过期音频；
/// 重连后回放旧语音只会让转写错位，因此在重连前丢弃，
/// 同时通知下游（chunker）丢弃半句上下文。
pub(crate) struct AudioStreamState {
    pub rx_audio: Receiver<Vec<u8>>,
    pub tx_events: Sender<AsrEvent>,
}

impl AudioStreamState {
    pub fn drain_and_reset(&mut self) {
        let mut dropped = 0usize;
        while self.rx_audio.try_recv().is_ok() {
            dropped += 1;
        }
        if dropped > 0 {
            eprintln!("🧹 已丢弃断连期间积压的 {} 个音频包", dropped);
        }

        // 空文本 + 句终 = 下游 chunker 的重置信号
        let _ = self.tx_events.try_send(AsrEvent {
            text: String::new(),
            is_sentence_end: true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::SinkExt;
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    #[tokio::test]
    async fn wait_for_json_scans_events_until_match() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp).await.unwrap();
            ws.send(Message::Text(r#"{"type":"session.created"}"#.into()))
                .await
                .unwrap();
            ws.send(Message::Text(r#"{"type":"session.updated"}"#.into()))
                .await
                .unwrap();
            // 保持连接直到客户端退出
            let _ = ws.next().await;
        });

        let mut stream =
            connect_authenticated("sk-test", &format!("ws://{addr}/api-ws/v1/inference"))
                .await
                .unwrap();
        let value = wait_for_json(&mut stream, Duration::from_secs(5), |v| {
            v["type"] == "session.updated"
        })
        .await
        .unwrap();

        assert_eq!(value["type"], "session.updated");
        drop(stream);
        server.abort();
    }

    struct MockSession;

    struct MockState {
        attempts: Arc<AtomicU32>,
    }

    impl SessionState for MockState {}

    impl ReconnectingSession for MockSession {
        type State = MockState;

        fn run_once<'a>(
            state: &'a mut MockState,
            _is_running: &'a AtomicBool,
        ) -> Pin<Box<dyn Future<Output = Result<SessionEnd>> + Send + 'a>> {
            Box::pin(async move {
                let attempt = state.attempts.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt < 3 {
                    anyhow::bail!("mock failure #{}", attempt);
                }
                Ok(SessionEnd::Shutdown)
            })
        }
    }

    #[tokio::test]
    async fn run_with_reconnect_retries_until_shutdown() {
        let is_running = AtomicBool::new(true);
        let attempts = Arc::new(AtomicU32::new(0));
        let mut state = MockState {
            attempts: attempts.clone(),
        };

        run_with_reconnect::<MockSession>("Mock", &is_running, &mut state)
            .await
            .unwrap();

        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    /// 保活器应真的发出 WS Ping 帧（服务端能收到），否则空闲连接仍会被关闭
    #[tokio::test]
    async fn keepalive_sends_ping_frames() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let pings = Arc::new(AtomicU32::new(0));
        let pings_server = pings.clone();

        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(tcp).await.unwrap();
            while let Some(Ok(msg)) = ws.next().await {
                if let Message::Ping(_) = msg {
                    pings_server.fetch_add(1, Ordering::SeqCst);
                }
            }
        });

        let stream = connect_authenticated("sk-test", &format!("ws://{addr}/api-ws/v1/inference"))
            .await
            .unwrap();
        let (mut write, _read) = stream.split();
        let mut keepalive = KeepAlive::with_period(Duration::from_millis(40));

        for _ in 0..3 {
            keepalive.tick().await;
            send_ping(&mut write).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            pings.load(Ordering::SeqCst) >= 2,
            "服务端应收到保活 Ping，实际 {}",
            pings.load(Ordering::SeqCst)
        );

        drop(write);
        server.abort();
    }
}
