//! 后台管线 → GUI 的事件契约（GUI_MIGRATION.md D6）。
//!
//! 所有后台任务只通过这条通道向界面汇报；界面每帧排空并转成状态/字幕/日志。
//! 事件里不带 `egui` 类型，保持可测试、可脱机回放。

/// 会话状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// 未启动，或已停止。
    Idle,
    /// 正在解析音色/打开设备（可能包含网络请求）。
    Starting,
    /// 管线已建立并运行。
    Running,
    /// 已请求停止，等待后台任务收尾。
    Stopping,
    /// 启动或运行失败（详见 `PipelineEvent::Error`）。
    Failed,
}

impl SessionState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "空闲",
            Self::Starting => "启动中",
            Self::Running => "运行中",
            Self::Stopping => "停止中",
            Self::Failed => "已失败",
        }
    }
}

/// 后台 → 界面的消息。
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    StateChanged(SessionState),
    Devices {
        input: String,
        output: String,
    },
    VoiceInfo {
        display: String,
    },
    /// ASR 当前句（会持续修订；`is_sentence_end` 表示这句结束）。
    ///
    /// `sent_bytes`：`text` 中已送入 TTS 的字节前缀长度（一定是字符边界），
    /// 界面据此把"已合成 / 尚未合成"两部分用不同颜色显示。
    /// `language`：本句已判定的语言代码（首个 chunk 送出前为 `None`）。
    Asr {
        text: String,
        is_sentence_end: bool,
        sent_bytes: usize,
        language: Option<String>,
    },
    Error {
        context: String,
        detail: String,
    },
}
