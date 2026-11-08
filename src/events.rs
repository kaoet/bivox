//! 后台管线 → GUI 的事件契约（决策 D6，见 docs/decisions.md）。
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
    /// `sent_bytes`：`text` 中已送入下一步的字节前缀长度（一定是字符边界），
    /// 界面据此把"已合成 / 尚未合成"两部分用不同颜色显示。
    /// `language`：本句已判定的语言代码（首个 chunk 送出前为 `None`）。
    /// `sentence_id`：服务端句子 id（重复下发同一句时相同，用于字幕去重）。
    ///
    /// 翻译模式下本事件只用于刷新源文与"已送翻译"前缀（`sent_bytes` 改为统计已送翻译的
    /// 源文长度），`is_sentence_end` 恒为 `false`，句终由 [`PipelineEvent::Translation`]
    /// 负责；`language` 是源语言而非目标语言（D26）。
    Asr {
        text: String,
        is_sentence_end: bool,
        sent_bytes: usize,
        language: Option<String>,
        sentence_id: Option<u64>,
    },
    /// 翻译模式当前句的累计译文（会持续增长；`is_sentence_end` 表示这句结束）。
    ///
    /// `sent_bytes`：`text` 中已送入 TTS 的字节前缀长度（一定是字符边界）。
    /// `language`：目标语言代码；`sentence_id`：源 ASR 句的服务端 id（用于字幕去重）。
    Translation {
        text: String,
        sent_bytes: usize,
        is_sentence_end: bool,
        language: String,
        sentence_id: Option<u64>,
    },
    /// 用户在「实时」页输入框提交的一句**打字输入**，已交给 TTS 排队朗读。
    ///
    /// `language`：本句实际使用的 TTS 语言（与 ASR 句共用同一个粘性语言判定）。
    /// 它与 ASR 字幕走同一套历史列表，但不按 `sentence_id` 去重。
    TypedSentence {
        text: String,
        language: String,
    },
    /// 本地 VAD 是否处于语音段（顶栏指示器用）。
    VadActive(bool),
    /// 用户可感知、但不影响会话继续运行的告警（黄色横幅）。
    ///
    /// 典型场景：配置了预编译热词但注册/更新失败，本次识别降级为不带热词继续跑（D31）。
    Warning {
        context: String,
        detail: String,
    },
    Error {
        context: String,
        detail: String,
    },
}
