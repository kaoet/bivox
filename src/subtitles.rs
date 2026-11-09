//! 字幕状态机：把 ASR / 翻译 / 打字输入事件归约成「当前句 + 历史句」。
//!
//! GUI 实时页与 VRChat OSC 聊天框共用这一份逻辑（D16/D26/D33）：
//! - 普通模式：`current` 是 ASR 当前句；
//! - 翻译模式：`current` 是源文，`current_translation` 是累计译文，句终由 Translation 事件负责；
//! - `history` 上限 `MAX_SUBTITLES`，`sentence_id` 只用于丢弃同一句的重复最终结果。
//!
//! 本模块是纯逻辑：不依赖 egui、网络或时间，便于单测。

/// 字幕历史保留上限（D16）。
pub const MAX_SUBTITLES: usize = 500;

/// 一条已结束的字幕。
///
/// 普通模式：`text` 是 ASR 文本，`source` / `source_language` 为空。
/// 翻译模式：`text` 是译文、`language` 是目标语言；`source` 是原文、`source_language` 是源语言（D26）。
#[derive(Debug, Clone)]
pub struct SubtitleLine {
    pub language: String,
    pub text: String,
    pub source: String,
    pub source_language: String,
    pub sentence_id: Option<u64>,
}

/// 实时字幕状态（当前句 + 历史）。
#[derive(Debug, Default)]
pub struct SubtitleState {
    /// 当前会话是否处于翻译模式（启动会话时按配置设置）。
    pub translation_mode: bool,
    /// 当前句原文（普通模式 = ASR 文本；翻译模式 = 源文）。
    pub current: String,
    /// 当前句原文已送入下一步（普通模式 = TTS；翻译模式 = 翻译）的字节前缀。
    pub sent_bytes: usize,
    /// 翻译模式：当前句累计译文。
    pub current_translation: String,
    /// 翻译模式：当前句译文已送入 TTS 的字节前缀。
    pub translation_sent_bytes: usize,
    /// 当前句语言（普通模式 = ASR 语言；翻译模式 = 目标语言）。
    pub language: Option<String>,
    /// 翻译模式：当前句源语言。
    pub source_language: Option<String>,
    /// 已结束的句子（上限 `MAX_SUBTITLES`）。
    pub history: Vec<SubtitleLine>,
    /// 上一条已落历史的句子 id：只用于丢弃「同一句被服务端重复下发」的最终结果。
    pub last_finalized_sentence_id: Option<u64>,
}

/// 把「已送入前缀长度」夹到字符边界，避免切片 panic。
fn clamp_prefix_len(text: &str, sent: usize) -> usize {
    let mut sent = sent.min(text.len());
    while sent > 0 && !text.is_char_boundary(sent) {
        sent -= 1;
    }
    sent
}

impl SubtitleState {
    /// 会话启动时设置当前模式（只影响本次会话；历史保留）。
    pub fn set_translation_mode(&mut self, enabled: bool) {
        self.translation_mode = enabled;
    }

    /// 新会话开始时只清掉「当前句」，**保留历史**（停止/重启管线不清空已识别字幕）。
    pub fn reset_current(&mut self) {
        self.current.clear();
        self.sent_bytes = 0;
        self.current_translation.clear();
        self.translation_sent_bytes = 0;
        self.language = None;
        self.source_language = None;
        self.last_finalized_sentence_id = None;
    }

    /// 打字输入句：提交即进历史（不经过 ASR 的 `sentence_id` 去重）。
    pub fn push_typed(&mut self, text: String, language: String) {
        if text.is_empty() {
            return;
        }
        self.push_line(SubtitleLine {
            language,
            text,
            source: String::new(),
            source_language: String::new(),
            sentence_id: None,
        });
    }

    fn push_line(&mut self, line: SubtitleLine) {
        self.history.push(line);
        if self.history.len() > MAX_SUBTITLES {
            self.history.remove(0);
        }
    }

    /// 把一条 ASR 事件并入字幕状态（纯逻辑，便于单测）。
    ///
    /// 普通模式：
    /// - 非空文本：刷新当前句，记录已送 TTS 的字节前缀与语言；
    /// - 空文本 + 句终：ASR 重置信号，清空当前句（同时忘掉上一句 id）；
    /// - 句终：当前句（连同语言代码）落进历史；**同一 `sentence_id` 重复下发只记一次**，
    ///   不同 id 的相同文本是两次真实发音，必须各自保留；超过 `MAX_SUBTITLES` 丢最旧的。
    ///
    /// 翻译模式：ASR 只负责刷新源文与「已送翻译」的源文前缀，句终由
    /// [`Self::apply_translation`] 负责（D26）。
    pub(crate) fn apply_asr(
        &mut self,
        text: &str,
        is_sentence_end: bool,
        sent: usize,
        language: Option<String>,
        sentence_id: Option<u64>,
    ) {
        if self.translation_mode {
            self.apply_source(text, sent, language);
            return;
        }

        if text.is_empty() {
            if is_sentence_end {
                self.current.clear();
                self.sent_bytes = 0;
                self.language = None;
                self.last_finalized_sentence_id = None;
            }
        } else {
            self.current.clear();
            self.current.push_str(text);
            self.sent_bytes = clamp_prefix_len(&self.current, sent);
            // 语言在本句首个 chunk 送出前是未知的；一旦判定就沿用本句
            if language.is_some() {
                self.language = language;
            }
        }

        if is_sentence_end && !self.current.is_empty() {
            let text = std::mem::take(&mut self.current);
            self.sent_bytes = 0;
            let language = self.language.take().unwrap_or_default();
            let duplicated =
                sentence_id.is_some() && self.last_finalized_sentence_id == sentence_id;
            if let Some(id) = sentence_id {
                self.last_finalized_sentence_id = Some(id);
            }
            if !duplicated {
                self.push_line(SubtitleLine {
                    language,
                    text,
                    source: String::new(),
                    source_language: String::new(),
                    sentence_id,
                });
            }
        }
    }

    /// 翻译模式：刷新当前句源文，`sent` 是已送翻译的源文字节前缀。
    fn apply_source(&mut self, text: &str, sent: usize, language: Option<String>) {
        if text.is_empty() {
            return;
        }
        self.current.clear();
        self.current.push_str(text);
        self.sent_bytes = clamp_prefix_len(&self.current, sent);
        if language.is_some() {
            self.source_language = language;
        }
    }

    /// 翻译模式：并入一条译文事件；`is_sentence_end` 时把当前句（原文 + 译文）落进历史。
    ///
    /// 去重、上限与普通模式一致；`language` 是目标语言，`source_language` 来自此前的源文事件。
    pub(crate) fn apply_translation(
        &mut self,
        text: &str,
        sent: usize,
        is_sentence_end: bool,
        language: String,
        sentence_id: Option<u64>,
    ) {
        if !text.is_empty() {
            self.current_translation.clear();
            self.current_translation.push_str(text);
            self.translation_sent_bytes = clamp_prefix_len(&self.current_translation, sent);
            self.language = Some(language.clone());
        }

        if !is_sentence_end {
            return;
        }

        let duplicated = sentence_id.is_some() && self.last_finalized_sentence_id == sentence_id;
        if let Some(id) = sentence_id {
            self.last_finalized_sentence_id = Some(id);
        }
        let translation = std::mem::take(&mut self.current_translation);
        let source = std::mem::take(&mut self.current);
        let source_language = self.source_language.take().unwrap_or_default();
        let target_language = self.language.take().unwrap_or(language);
        self.sent_bytes = 0;
        self.translation_sent_bytes = 0;

        if !duplicated && !translation.is_empty() {
            self.push_line(SubtitleLine {
                language: target_language,
                text: translation,
                source,
                source_language,
                sentence_id,
            });
        }
    }
}
