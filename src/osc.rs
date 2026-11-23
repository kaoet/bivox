//! VRChat OSC 聊天框后台服务（D33/D39/D42/D44）。
//!
//! 独立于 GUI 重绘：会话把 `PipelineEvent` 克隆一份发到这里，本任务维护自己的
//! [`Timeline`]（只取我方投影）、1 秒文本节流、typing 跳变和显式清空。GUI 顶栏的
//! 「清除 VRChat 聊天框」通过命令通道触发手动清空（D44）：除了立即发空串，还要丢
//! 弃镜像时间线里的消息，否则下一次合成就把历史句重新发出去。
//!
//! 关键约束（2026-11-08 实测）：
//! - `/chatbox/input` 文本最多 1 条/秒；900ms 长跑会跳字；
//! - `/chatbox/typing` 不占文本限流；
//! - 聊天框在最后一个文本包后约 30 秒自然过期；D42 起客户端按 `retention_secs`
//!   主动清空，历史句也只在保留时长内显示。

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use log::{debug, warn};
use rosc::{OscMessage, OscPacket, OscType, encoder};
use tokio::sync::{mpsc, watch};

use crate::chatbox::{self, ChatboxText};
use crate::config::{AppConfig, VrchatDisplay};
use crate::events::{PipelineEvent, SessionState, Speaker};
use crate::timeline::Timeline;

/// 两次文本发送之间的最小间隔（实测 1000ms 稳定，900ms 长跑跳字）。
pub const CHATBOX_TEXT_INTERVAL: Duration = Duration::from_secs(1);

/// 测试消息立即发送，不参与文本节流。
const TEST_MESSAGE: &str = "Bivox OSC 测试：你好，VRChat！";

/// 当前 OSC 配置（由 GUI 写回后通过 watch 实时同步）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatboxSettings {
    pub enabled: bool,
    pub port: u16,
    pub display: VrchatDisplay,
    pub translate_enabled: bool,
    /// 是否在聊天框里同时显示历史句（D42）。
    pub show_history: bool,
    /// 句终后的保留时长：历史句年龄上限，也是最后一句的存活时长（D42）。
    pub retention: Duration,
}

impl ChatboxSettings {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            enabled: config.vrchat.osc.enabled,
            port: config.vrchat.osc.port,
            display: config.vrchat.osc.display,
            translate_enabled: config.local.translate,
            show_history: config.vrchat.osc.show_history,
            retention: Duration::from_secs(u64::from(config.vrchat.osc.retention_secs)),
        }
    }
}

impl Default for ChatboxSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: crate::config::DEFAULT_VRCHAT_OSC_PORT,
            display: VrchatDisplay::default(),
            translate_enabled: false,
            show_history: false,
            retention: Duration::from_secs(u64::from(crate::config::DEFAULT_VRCHAT_RETENTION_SECS)),
        }
    }
}

/// GUI 持有的服务句柄。
#[derive(Clone)]
pub struct ChatboxService {
    settings_tx: watch::Sender<ChatboxSettings>,
    events_tx: mpsc::UnboundedSender<PipelineEvent>,
    command_tx: mpsc::UnboundedSender<Command>,
}

enum Command {
    SendTest,
    /// 手动清除聊天框（含历史句，D44）。
    ForceClear,
    Shutdown,
}

impl ChatboxService {
    /// 启动常驻后台任务。即使 OSC 关闭也保持空转，方便运行中打开开关立即显示当前句。
    pub fn spawn(runtime: &tokio::runtime::Handle, settings: ChatboxSettings) -> Self {
        let (settings_tx, settings_rx) = watch::channel(settings);
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (command_tx, command_rx) = mpsc::unbounded_channel();

        runtime.spawn(async move {
            run(settings_rx, events_rx, command_rx).await;
        });

        Self {
            settings_tx,
            events_tx,
            command_tx,
        }
    }

    /// 交给会话的 `PipelineEvent` 发送端。
    pub fn event_sender(&self) -> mpsc::UnboundedSender<PipelineEvent> {
        self.events_tx.clone()
    }

    pub fn update_settings(&self, settings: ChatboxSettings) {
        // 发送端仍在服务里；失败只可能是任务已退出，忽略即可。
        let _ = self.settings_tx.send(settings);
    }

    /// 立即发一条测试消息；返回 false 表示服务已退出。
    pub fn send_test(&self) -> bool {
        self.command_tx.send(Command::SendTest).is_ok()
    }

    /// 立即清空聊天框（含历史句，D44）；返回 false 表示服务已退出。
    pub fn force_clear(&self) -> bool {
        self.command_tx.send(Command::ForceClear).is_ok()
    }

    /// 关窗时尽力清空聊天框并退出任务。
    pub fn shutdown(&self) {
        let _ = self.command_tx.send(Command::Shutdown);
    }
}

async fn run(
    mut settings_rx: watch::Receiver<ChatboxSettings>,
    mut events_rx: mpsc::UnboundedReceiver<PipelineEvent>,
    mut command_rx: mpsc::UnboundedReceiver<Command>,
) {
    let mut runtime = Runtime::new(settings_rx.borrow().clone());
    let mut events_open = true;
    let mut ticker = tokio::time::interval(Duration::from_millis(200));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;

            command = command_rx.recv() => {
                match command {
                    Some(Command::SendTest) => runtime.send_test(),
                    Some(Command::ForceClear) => runtime.force_clear(Instant::now()),
                    Some(Command::Shutdown) => {
                        runtime.shutdown(Instant::now());
                        break;
                    }
                    None => break,
                }
            }
            changed = settings_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let settings = settings_rx.borrow().clone();
                runtime.update_settings(settings, Instant::now());
            }
            event = events_rx.recv(), if events_open => {
                match event {
                    Some(event) => runtime.on_event(event, Instant::now()),
                    None => events_open = false,
                }
            }
            _ = ticker.tick() => runtime.tick(Instant::now()),
        }
    }
}

struct Runtime {
    settings: ChatboxSettings,
    sender: ChatboxSender,
    /// 与我方线路同一套时间线；聊天框只镜像我方（D33/D39）。
    timeline: Timeline,
    session_active: bool,
    last_sent_text: String,
    last_text_sent_at: Option<Instant>,
    typing_sent: bool,
}

impl Runtime {
    fn new(settings: ChatboxSettings) -> Self {
        let mut timeline = Timeline::default();
        timeline.set_translation(Speaker::Local, settings.translate_enabled);
        let sender = ChatboxSender::new(settings.port);
        Self {
            settings,
            sender,
            timeline,
            session_active: false,
            last_sent_text: String::new(),
            last_text_sent_at: None,
            typing_sent: false,
        }
    }

    fn update_settings(&mut self, settings: ChatboxSettings, now: Instant) {
        if settings.port != self.settings.port {
            self.sender.clear();
            self.sender.send_typing(false);
            self.sender = ChatboxSender::new(settings.port);
            self.last_sent_text.clear();
            self.last_text_sent_at = None;
            self.typing_sent = false;
        }

        let was_enabled = self.settings.enabled;
        self.settings = settings;
        if !self.settings.enabled && was_enabled {
            self.clear(now);
        } else if self.settings.enabled && !was_enabled {
            self.last_sent_text.clear();
            self.last_text_sent_at = None;
            self.typing_sent = false;
        }
        self.tick(now);
    }

    fn on_event(&mut self, event: PipelineEvent, now: Instant) {
        match event {
            PipelineEvent::StateChanged(SessionState::Running) => {
                self.session_active = true;
                self.timeline = Timeline::default();
                self.timeline
                    .set_translation(Speaker::Local, self.settings.translate_enabled);
                self.last_sent_text.clear();
                self.last_text_sent_at = None;
                self.typing_sent = false;
                if self.settings.enabled {
                    // 新会话开始先清掉上一个会话可能残留的文本，并记录为一次文本发送，
                    // 避免紧接着的第一条 ASR 文本和清空挤在同一个 1 秒窗口里。
                    self.clear(now);
                }
                self.tick(now);
            }
            PipelineEvent::StateChanged(SessionState::Idle | SessionState::Failed) => {
                self.session_active = false;
                self.timeline = Timeline::default();
                if self.settings.enabled {
                    self.clear(now);
                } else {
                    self.last_sent_text.clear();
                    self.last_text_sent_at = None;
                    self.typing_sent = false;
                }
            }
            PipelineEvent::StateChanged(_) => {}
            PipelineEvent::Asr {
                speaker,
                text,
                is_sentence_end,
                sent_bytes,
                language,
                sentence_id,
            } => {
                // 只镜像我方；他人线路的文本不进聊天框（D39）。
                if speaker != Speaker::Local {
                    return;
                }
                let had_current = self.has_current();
                self.timeline.apply_asr(
                    speaker,
                    &text,
                    is_sentence_end,
                    sent_bytes,
                    language,
                    sentence_id,
                    now,
                );
                if !had_current && self.has_current() {
                    // 新的一段显示：允许同样的文本再次显示（上一段可能已自然过期）。
                    self.last_sent_text.clear();
                }
                self.tick(now);
            }
            PipelineEvent::Translation {
                speaker,
                text,
                sent_bytes,
                is_sentence_end,
                language,
                sentence_id,
            } => {
                if speaker != Speaker::Local {
                    return;
                }
                let had_current = self.has_current();
                self.timeline.apply_translation(
                    speaker,
                    &text,
                    sent_bytes,
                    is_sentence_end,
                    language,
                    sentence_id,
                    now,
                );
                if !had_current && self.has_current() {
                    self.last_sent_text.clear();
                }
                self.tick(now);
            }
            PipelineEvent::TypedSentence { text, language } => {
                self.timeline.push_typed(text, language, now);
                self.last_sent_text.clear();
                self.tick(now);
            }
            PipelineEvent::Devices { .. }
            | PipelineEvent::VoiceInfo { .. }
            | PipelineEvent::VadActive { .. }
            | PipelineEvent::OthersStatus(_)
            | PipelineEvent::LineError { .. }
            | PipelineEvent::Warning { .. }
            | PipelineEvent::Error { .. } => {}
        }
    }

    fn tick(&mut self, now: Instant) {
        if !self.settings.enabled || !self.session_active {
            if !self.last_sent_text.is_empty() {
                self.sender.clear();
                self.last_sent_text.clear();
            }
            if self.typing_sent {
                self.sender.send_typing(false);
                self.typing_sent = false;
            }
            return;
        }

        // typing 只跟“当前句是否存在”有关，与 display 模式无关。
        let options = chatbox::ChatboxOptions {
            display: self.settings.display,
            show_history: self.settings.show_history,
            retention: self.settings.retention,
        };
        let (current_exists, composed) = {
            let view = self.timeline.speaker_view(Speaker::Local);
            let current_exists = view
                .current
                .is_some_and(|message| !message.text.is_empty() || !message.source.is_empty());
            (current_exists, chatbox::compose(&view, options, now))
        };
        if current_exists != self.typing_sent {
            self.sender.send_typing(current_exists);
            self.typing_sent = current_exists;
        }

        // `Clear` = 没有可显示内容（D42）；空串同样是清空命令，靠下面的去重避免重复发送。
        let desired = match composed {
            ChatboxText::Text(text) => text,
            ChatboxText::Clear => String::new(),
        };

        if desired == self.last_sent_text {
            return;
        }
        if self
            .last_text_sent_at
            .is_some_and(|last| now.duration_since(last) < CHATBOX_TEXT_INTERVAL)
        {
            return;
        }

        if self.sender.send_text(&desired) {
            self.last_sent_text = desired;
            self.last_text_sent_at = Some(now);
        }
    }

    fn send_test(&mut self) {
        if self.settings.enabled {
            // 测试消息立即发送，不走 1 秒文本窗口，也不改变 typing。
            self.sender.send_text(TEST_MESSAGE);
        }
    }

    /// 手动强制清空（D44）：立即发空串，并丢弃镜像时间线里的当前句与历史句。
    ///
    /// 只发空串不够：下一个 tick 会从 [`Timeline`] 重新合成旧历史（D42）。这里清消息
    /// 但保留翻译模式与最终句去重记忆（见 [`Timeline::clear_messages`]）。
    fn force_clear(&mut self, now: Instant) {
        self.timeline.clear_messages();
        if self.settings.enabled {
            // 复用既有清空路径：立即空串 + 关闭 typing，并重开 1 秒文本窗口。
            self.clear(now);
        }
    }

    fn shutdown(&mut self, now: Instant) {
        if self.settings.enabled {
            self.clear(now);
        }
    }

    fn clear(&mut self, now: Instant) {
        self.sender.clear();
        self.sender.send_typing(false);
        self.last_sent_text.clear();
        self.last_text_sent_at = Some(now);
        self.typing_sent = false;
    }

    fn has_current(&self) -> bool {
        self.timeline
            .speaker_view(Speaker::Local)
            .current
            .is_some_and(|message| !message.text.is_empty() || !message.source.is_empty())
    }
}

struct ChatboxSender {
    socket: Option<UdpSocket>,
    port: u16,
}

impl ChatboxSender {
    fn new(port: u16) -> Self {
        let socket = match UdpSocket::bind("127.0.0.1:0").and_then(|socket| {
            socket.set_nonblocking(true)?;
            Ok(socket)
        }) {
            Ok(socket) => Some(socket),
            Err(err) => {
                warn!("VRChat OSC：创建 UDP socket 失败：{err}");
                None
            }
        };
        Self { socket, port }
    }

    fn send_text(&self, text: &str) -> bool {
        match encode_chatbox(text) {
            Ok(bytes) => self.send(&bytes),
            Err(err) => {
                debug!("VRChat OSC：编码聊天框文本失败：{err}");
                false
            }
        }
    }

    fn send_typing(&self, typing: bool) -> bool {
        match encode_typing(typing) {
            Ok(bytes) => self.send(&bytes),
            Err(err) => {
                debug!("VRChat OSC：编码 typing 失败：{err}");
                false
            }
        }
    }

    fn clear(&self) {
        self.send_text("");
    }

    fn send(&self, bytes: &[u8]) -> bool {
        let Some(socket) = &self.socket else {
            return false;
        };
        let target = std::net::SocketAddr::from(([127, 0, 0, 1], self.port));
        match socket.send_to(bytes, target) {
            Ok(_) => true,
            Err(err) => {
                // UDP 无连接；VRChat 未运行不是可感知错误，只记 debug。
                debug!("VRChat OSC：发送到 {target} 失败：{err}");
                false
            }
        }
    }
}

pub(crate) fn encode_chatbox(text: &str) -> Result<Vec<u8>, rosc::OscError> {
    encoder::encode(&OscPacket::Message(OscMessage {
        addr: "/chatbox/input".to_string(),
        args: vec![
            OscType::String(text.to_string()),
            OscType::Bool(true),  // immediate：绕过键盘直接显示
            OscType::Bool(false), // notification sound：不响
        ],
    }))
}

pub(crate) fn encode_typing(typing: bool) -> Result<Vec<u8>, rosc::OscError> {
    encoder::encode(&OscPacket::Message(OscMessage {
        addr: "/chatbox/typing".to_string(),
        args: vec![OscType::Bool(typing)],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosc::decoder;

    fn decode(bytes: &[u8]) -> rosc::OscPacket {
        decoder::decode_udp(bytes).expect("valid OSC packet").1
    }

    #[test]
    fn chatbox_packet_has_expected_address_and_args() {
        let packet = decode(&encode_chatbox("hello").unwrap());
        let OscPacket::Message(message) = packet else {
            panic!("expected message");
        };
        assert_eq!(message.addr, "/chatbox/input");
        assert_eq!(message.args.len(), 3);
        assert_eq!(message.args[0], OscType::String("hello".to_string()));
        assert_eq!(message.args[1], OscType::Bool(true));
        assert_eq!(message.args[2], OscType::Bool(false));
    }

    #[test]
    fn typing_packet_has_expected_address_and_bool() {
        let packet = decode(&encode_typing(true).unwrap());
        let OscPacket::Message(message) = packet else {
            panic!("expected message");
        };
        assert_eq!(message.addr, "/chatbox/typing");
        assert_eq!(message.args, vec![OscType::Bool(true)]);
    }

    #[test]
    fn settings_follow_config() {
        let mut config = AppConfig::default();
        config.vrchat.osc.enabled = true;
        config.vrchat.osc.port = 9100;
        config.vrchat.osc.display = VrchatDisplay::Translation;
        config.vrchat.osc.show_history = true;
        config.vrchat.osc.retention_secs = 12;
        config.local.translate = true;
        let settings = ChatboxSettings::from_config(&config);
        assert!(settings.enabled);
        assert_eq!(settings.port, 9100);
        assert_eq!(settings.display, VrchatDisplay::Translation);
        assert!(settings.translate_enabled);
        assert!(settings.show_history);
        assert_eq!(settings.retention, Duration::from_secs(12));
    }

    #[test]
    fn runtime_throttles_text_and_keeps_typing_independent() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        listener
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();

        let mut runtime = Runtime::new(ChatboxSettings {
            enabled: true,
            port,
            display: VrchatDisplay::Source,
            translate_enabled: false,
            show_history: false,
            retention: Duration::from_secs(30),
        });
        let start = Instant::now();
        runtime.on_event(PipelineEvent::StateChanged(SessionState::Running), start);

        // Running 时先显式清空，并关闭上一次可能残留的 typing。
        let clear = recv_packet(&listener);
        assert!(is_text(&clear, ""));
        let typing_off = recv_packet(&listener);
        assert!(is_typing(&typing_off, false));

        // 第一条当前句：先 typing=true，再发文本。
        runtime.on_event(asr_event("hello"), start + Duration::from_secs(2));
        let typing = recv_packet(&listener);
        assert!(is_typing(&typing, true));
        let text = recv_packet(&listener);
        assert!(is_text(&text, "hello"));

        // 1 秒内文本变化：不发文本。
        runtime.on_event(asr_event("hello2"), start + Duration::from_millis(2500));
        assert!(recv_packet_opt(&listener).is_none(), "1 秒内不应再发文本");

        // 过了 1 秒窗口：发最新文本。
        runtime.on_event(asr_event("hello3"), start + Duration::from_secs(3));
        let text = recv_packet(&listener);
        assert!(is_text(&text, "hello3"));

        // 停止会话：显式清空。
        runtime.on_event(
            PipelineEvent::StateChanged(SessionState::Idle),
            start + Duration::from_secs(4),
        );
        let clear = recv_packet(&listener);
        assert!(is_text(&clear, ""));
    }

    fn asr_event(text: &str) -> PipelineEvent {
        PipelineEvent::Asr {
            speaker: Speaker::Local,
            text: text.to_string(),
            is_sentence_end: false,
            sent_bytes: 0,
            language: Some("en".to_string()),
            sentence_id: None,
        }
    }

    fn recv_packet(listener: &UdpSocket) -> Vec<u8> {
        let mut buffer = [0u8; 2048];
        let (len, _) = listener.recv_from(&mut buffer).expect("expected packet");
        buffer[..len].to_vec()
    }

    fn recv_packet_opt(listener: &UdpSocket) -> Option<Vec<u8>> {
        let mut buffer = [0u8; 2048];
        match listener.recv_from(&mut buffer) {
            Ok((len, _)) => Some(buffer[..len].to_vec()),
            Err(_) => None,
        }
    }

    fn is_text(packet: &[u8], expected: &str) -> bool {
        let OscPacket::Message(message) = decode(packet) else {
            return false;
        };
        message.addr == "/chatbox/input"
            && message.args.first() == Some(&OscType::String(expected.to_string()))
    }

    fn is_typing(packet: &[u8], expected: bool) -> bool {
        let OscPacket::Message(message) = decode(packet) else {
            return false;
        };
        message.addr == "/chatbox/typing" && message.args == vec![OscType::Bool(expected)]
    }

    fn asr_end_event(text: &str) -> PipelineEvent {
        asr_end_event_with_id(text, None)
    }

    fn asr_end_event_with_id(text: &str, sentence_id: Option<u64>) -> PipelineEvent {
        PipelineEvent::Asr {
            speaker: Speaker::Local,
            text: text.to_string(),
            is_sentence_end: true,
            sent_bytes: 0,
            language: Some("en".to_string()),
            sentence_id,
        }
    }

    #[test]
    fn runtime_shows_history_and_clears_after_retention() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        listener
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut runtime = Runtime::new(ChatboxSettings {
            enabled: true,
            port,
            display: VrchatDisplay::Source,
            translate_enabled: false,
            show_history: true,
            retention: Duration::from_secs(5),
        });
        let start = Instant::now();
        runtime.on_event(PipelineEvent::StateChanged(SessionState::Running), start);
        assert!(is_text(&recv_packet(&listener), ""));
        assert!(is_typing(&recv_packet(&listener), false));

        // 第一句在 +2s 定稿。
        runtime.on_event(asr_end_event("一"), start + Duration::from_secs(2));
        assert!(is_text(&recv_packet(&listener), "一"));

        // 第二句在 +3.5s 定稿：没有当前句，历史按新→旧。
        runtime.on_event(asr_end_event("二"), start + Duration::from_millis(3500));
        assert!(is_text(&recv_packet(&listener), "二\n一"));

        // +7.01s：第一句超过 5 秒，只剩第二句。
        runtime.tick(start + Duration::from_millis(7010));
        assert!(is_text(&recv_packet(&listener), "二"));

        // +8.51s：第二句也过期，主动清空。
        runtime.tick(start + Duration::from_millis(8510));
        assert!(is_text(&recv_packet(&listener), ""));
    }

    #[test]
    fn runtime_clears_last_sentence_after_retention_when_history_off() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        listener
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut runtime = Runtime::new(ChatboxSettings {
            enabled: true,
            port,
            display: VrchatDisplay::Source,
            translate_enabled: false,
            show_history: false,
            retention: Duration::from_secs(5),
        });
        let start = Instant::now();
        runtime.on_event(PipelineEvent::StateChanged(SessionState::Running), start);
        assert!(is_text(&recv_packet(&listener), ""));
        assert!(is_typing(&recv_packet(&listener), false));

        runtime.on_event(asr_end_event("一"), start + Duration::from_secs(2));
        assert!(is_text(&recv_packet(&listener), "一"));

        // 关闭历史句：第二句定稿后只显示它，不再附带第一句。
        runtime.on_event(asr_end_event("二"), start + Duration::from_millis(3500));
        assert!(is_text(&recv_packet(&listener), "二"));

        // 定稿 5 秒后主动清空（不依赖 VRChat 自然过期）。
        runtime.tick(start + Duration::from_millis(8600));
        assert!(is_text(&recv_packet(&listener), ""));
        assert!(recv_packet_opt(&listener).is_none(), "清空只发一次");
    }

    #[test]
    fn runtime_keeps_current_sentence_regardless_of_retention() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        listener
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut runtime = Runtime::new(ChatboxSettings {
            enabled: true,
            port,
            display: VrchatDisplay::Source,
            translate_enabled: false,
            show_history: false,
            retention: Duration::from_secs(5),
        });
        let start = Instant::now();
        runtime.on_event(PipelineEvent::StateChanged(SessionState::Running), start);
        assert!(is_text(&recv_packet(&listener), ""));
        assert!(is_typing(&recv_packet(&listener), false));

        runtime.on_event(asr_event("正在说"), start + Duration::from_secs(2));
        assert!(is_typing(&recv_packet(&listener), true));
        assert!(is_text(&recv_packet(&listener), "正在说"));

        // 当前句长时间未定稿也不受保留时长清空。
        runtime.tick(start + Duration::from_secs(100));
        assert!(recv_packet_opt(&listener).is_none());

        // 定稿后 typing 关闭；文本没变，不重复发。
        runtime.on_event(asr_end_event("正在说"), start + Duration::from_secs(101));
        assert!(is_typing(&recv_packet(&listener), false));
        assert!(recv_packet_opt(&listener).is_none());

        // 定稿 5 秒后清空。
        runtime.tick(start + Duration::from_secs(107));
        assert!(is_text(&recv_packet(&listener), ""));
    }

    #[test]
    fn force_clear_drops_current_and_history_without_resurrection() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        listener
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut runtime = Runtime::new(ChatboxSettings {
            enabled: true,
            port,
            display: VrchatDisplay::Source,
            translate_enabled: false,
            show_history: true,
            retention: Duration::from_secs(30),
        });
        let start = Instant::now();
        runtime.on_event(PipelineEvent::StateChanged(SessionState::Running), start);
        assert!(is_text(&recv_packet(&listener), ""));
        assert!(is_typing(&recv_packet(&listener), false));

        // 两句定稿后按「新 → 旧」显示历史句。
        runtime.on_event(asr_end_event("一"), start + Duration::from_secs(2));
        assert!(is_text(&recv_packet(&listener), "一"));
        runtime.on_event(asr_end_event("二"), start + Duration::from_secs(3));
        assert!(is_text(&recv_packet(&listener), "二\n一"));

        // 手动清除：立即空串，并关闭 typing。
        runtime.force_clear(start + Duration::from_secs(4));
        assert!(is_text(&recv_packet(&listener), ""));
        assert!(is_typing(&recv_packet(&listener), false));

        // 1 秒窗口过后也不会从镜像时间线重新合成历史句。
        runtime.tick(start + Duration::from_secs(10));
        assert!(recv_packet_opt(&listener).is_none(), "历史句不应复现");

        // 新句仍正常显示。
        runtime.on_event(asr_end_event("三"), start + Duration::from_secs(11));
        assert!(is_text(&recv_packet(&listener), "三"));
    }

    #[test]
    fn force_clear_keeps_duplicate_final_deduped() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        listener
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut runtime = Runtime::new(ChatboxSettings {
            enabled: true,
            port,
            display: VrchatDisplay::Source,
            translate_enabled: false,
            show_history: true,
            retention: Duration::from_secs(30),
        });
        let start = Instant::now();
        runtime.on_event(PipelineEvent::StateChanged(SessionState::Running), start);
        assert!(is_text(&recv_packet(&listener), ""));
        assert!(is_typing(&recv_packet(&listener), false));

        runtime.on_event(
            asr_end_event_with_id("一句。", Some(7)),
            start + Duration::from_secs(2),
        );
        assert!(is_text(&recv_packet(&listener), "一句。"));

        runtime.force_clear(start + Duration::from_secs(3));
        assert!(is_text(&recv_packet(&listener), ""));
        assert!(is_typing(&recv_packet(&listener), false));

        // 服务端重复下发的同一句最终结果：去重记忆仍在，不会把句子写回。
        runtime.on_event(
            asr_end_event_with_id("一句。", Some(7)),
            start + Duration::from_secs(5),
        );
        assert!(
            recv_packet_opt(&listener).is_none(),
            "重复最终结果不应复活已清除的句子"
        );
    }

    #[test]
    fn force_clear_when_disabled_sends_nothing() {
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        listener
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut runtime = Runtime::new(ChatboxSettings {
            enabled: false,
            port,
            ..ChatboxSettings::default()
        });

        runtime.force_clear(Instant::now());
        assert!(recv_packet_opt(&listener).is_none(), "OSC 关闭时不发包");
    }

    #[tokio::test]
    async fn service_force_clear_sends_empty_text() {
        let listener = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let service = ChatboxService::spawn(
            &tokio::runtime::Handle::current(),
            ChatboxSettings {
                enabled: true,
                port,
                ..ChatboxSettings::default()
            },
        );
        assert!(service.force_clear());

        let mut buffer = [0u8; 2048];
        let (len, _) =
            tokio::time::timeout(Duration::from_secs(2), listener.recv_from(&mut buffer))
                .await
                .expect("应在 2 秒内收到清空包")
                .expect("UDP 接收失败");
        assert!(is_text(&buffer[..len], ""));
        service.shutdown();
    }
}
