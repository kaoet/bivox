//! 对话时间线状态机：把两条线路的 ASR / 翻译 / 打字输入事件归约成「消息列表」。
//!
//! GUI 实时页与 VRChat OSC 聊天框共用这份逻辑（D16/D26/D33/D39/D42）：
//! - 每条消息带来源 [`Speaker`]；同一时刻每条线路最多一条「未完成」消息（`open = true`）；
//! - `messages[0]` 是最新一条；新消息插到最前面，未完成消息**原地更新**（不改变位置）；
//! - 普通模式：`text` 就是 ASR 文本；翻译模式：`text` 是累计译文、`source` 是原文；
//! - `sent_bytes` / `source_sent_bytes` 分别是主行/副行已送入下游的字节前缀（界面分色）；
//! - 定稿时记录 `finished_at`，供 OSC 聊天框按保留时长裁剪历史句（D42）；
//! - 历史上限 [`MAX_MESSAGES`]，按 `(来源, sentence_id)` 去重同一句的重复最终结果。
//!
//! 本模块是纯逻辑：不依赖 egui、网络，也不读取系统时钟；`now` 由调用方传入，便于单测。

use std::time::Instant;

use crate::events::Speaker;

/// 时间线保留的消息上限（D16）。
pub const MAX_MESSAGES: usize = 500;

/// 一条消息：未完成时随 ASR/翻译修订，句终后定稿。
#[derive(Debug, Clone)]
pub struct Message {
    /// 谁说的。
    pub speaker: Speaker,
    /// 未完成（正在识别/翻译）：界面按进度分色，且不可复制。
    pub open: bool,
    /// 主行文本：译文；未翻译时就是原文。
    pub text: String,
    /// 主行语言：译文语言；未翻译时是识别语言。
    pub language: String,
    /// 主行已送入下游（TTS 或翻译）的字节前缀。
    pub sent_bytes: usize,
    /// 副行原文（翻译模式）；普通模式为空。
    pub source: String,
    /// 副行语言（识别语言）。
    pub source_language: String,
    /// 副行已送入翻译的字节前缀（翻译模式）。
    pub source_sent_bytes: usize,
    /// 服务端句子 id（用于丢弃同一句的重复最终结果）。
    pub sentence_id: Option<u64>,
    /// 句终（定稿）时刻；未完成消息为 `None`（D42）。
    ///
    /// OSC 聊天框用它判断历史句是否超出保留时长。
    pub finished_at: Option<Instant>,
}

/// 某一条线路的视图：聊天框只镜像我方，用它取出「当前句 + 历史句」。
#[derive(Debug, Clone)]
pub struct SpeakerView<'a> {
    /// 该线路是否开启翻译（决定 `text` 是译文还是原文）。
    pub translation: bool,
    /// 当前未完成的消息。
    pub current: Option<&'a Message>,
    /// 该线路所有已完成的消息，新 → 旧；`history[0]` 就是最近一条历史句。
    pub history: Vec<&'a Message>,
}

/// 两条线路共用的对话时间线。
#[derive(Debug, Default)]
pub struct Timeline {
    /// 每条线路是否处于翻译模式（按 [`Speaker`] 索引）。
    translation: [bool; 2],
    /// 消息列表：索引 0 = 最新。
    messages: Vec<Message>,
    /// 每条线路最后一条已定稿的句子 id（只用于丢弃重复最终结果）。
    last_finalized: [Option<u64>; 2],
}

impl Timeline {
    /// 会话启动时设置某条线路的翻译模式（只影响本次会话；历史保留）。
    pub fn set_translation(&mut self, speaker: Speaker, enabled: bool) {
        self.translation[speaker.index()] = enabled;
    }

    /// 消息列表（索引 0 = 最新）。
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// 某条线路是否开启翻译。
    pub fn translation(&self, speaker: Speaker) -> bool {
        self.translation[speaker.index()]
    }

    /// 新会话/停止会话时清掉所有「未完成」消息，历史保留（D16）。
    pub fn reset_open(&mut self) {
        self.messages.retain(|message| !message.open);
        self.last_finalized = [None; 2];
    }

    /// 打字输入句：提交即进历史（不经过 ASR 的 `sentence_id` 去重）。
    pub fn push_typed(&mut self, text: String, language: String, now: Instant) {
        if text.is_empty() {
            return;
        }
        self.insert_before_history(Message {
            speaker: Speaker::Local,
            open: false,
            text,
            language,
            sent_bytes: 0,
            source: String::new(),
            source_language: String::new(),
            source_sent_bytes: 0,
            sentence_id: None,
            finished_at: Some(now),
        });
    }

    /// 把一条 ASR 事件并入时间线（纯逻辑，便于单测）。
    ///
    /// 普通模式：
    /// - 非空文本：刷新当前消息的主行，记录已送 TTS 的字节前缀与语言；
    /// - 空文本 + 句终：ASR 重置信号，丢弃当前消息并忘掉该线路的上一句 id；
    /// - 句终：当前消息定稿；**同一 `sentence_id` 重复下发只记一次**，
    ///   不同 id 的相同文本是两次真实发音，必须各自保留。
    ///
    /// 翻译模式：只刷新原文与「已送翻译」前缀，句终由 [`Self::apply_translation`] 负责（D26）。
    pub(crate) fn apply_asr(
        &mut self,
        speaker: Speaker,
        text: &str,
        is_sentence_end: bool,
        sent: usize,
        language: Option<String>,
        sentence_id: Option<u64>,
        now: Instant,
    ) {
        if self.translation(speaker) {
            self.apply_source(speaker, text, sent, language);
            return;
        }

        if text.is_empty() {
            if is_sentence_end {
                self.remove_open(speaker);
                self.last_finalized[speaker.index()] = None;
            }
            return;
        }

        let message = self.open_message(speaker);
        message.text.clear();
        message.text.push_str(text);
        message.sent_bytes = clamp_prefix_len(&message.text, sent);
        if let Some(language) = language {
            message.language = language;
        }

        if is_sentence_end {
            self.finalize_open(speaker, sentence_id, now);
        }
    }

    /// 翻译模式：刷新当前消息的原文；`sent` 是已送翻译的源文字节前缀。
    fn apply_source(
        &mut self,
        speaker: Speaker,
        text: &str,
        sent: usize,
        language: Option<String>,
    ) {
        if text.is_empty() {
            return;
        }
        let message = self.open_message(speaker);
        message.source.clear();
        message.source.push_str(text);
        message.source_sent_bytes = clamp_prefix_len(&message.source, sent);
        if let Some(language) = language {
            message.source_language = language;
        }
    }

    /// 翻译模式：并入一条译文事件；`is_sentence_end` 时把当前消息定稿。
    ///
    /// 去重、上限与普通模式一致；`language` 是译文语言，`source_language` 来自此前的源文事件。
    pub(crate) fn apply_translation(
        &mut self,
        speaker: Speaker,
        text: &str,
        sent: usize,
        is_sentence_end: bool,
        language: String,
        sentence_id: Option<u64>,
        now: Instant,
    ) {
        if !text.is_empty() {
            let message = self.open_message(speaker);
            message.text.clear();
            message.text.push_str(text);
            message.sent_bytes = clamp_prefix_len(&message.text, sent);
            message.language = language;
        }

        if is_sentence_end {
            self.finalize_open(speaker, sentence_id, now);
        }
    }

    /// 某条线路的「当前消息 + 所有历史消息」（聊天框投影）。
    pub fn speaker_view(&self, speaker: Speaker) -> SpeakerView<'_> {
        let current = self
            .messages
            .iter()
            .find(|message| message.open && message.speaker == speaker);
        let history = self
            .messages
            .iter()
            .filter(|message| !message.open && message.speaker == speaker)
            .collect();
        SpeakerView {
            translation: self.translation(speaker),
            current,
            history,
        }
    }

    /// 找到或新建某条线路的未完成消息（新消息插到最前面）。
    fn open_message(&mut self, speaker: Speaker) -> &mut Message {
        if let Some(index) = self.open_index(speaker) {
            return &mut self.messages[index];
        }
        self.insert_front(Message {
            speaker,
            open: true,
            text: String::new(),
            language: String::new(),
            sent_bytes: 0,
            source: String::new(),
            source_language: String::new(),
            source_sent_bytes: 0,
            sentence_id: None,
            finished_at: None,
        });
        &mut self.messages[0]
    }

    fn open_index(&self, speaker: Speaker) -> Option<usize> {
        self.messages
            .iter()
            .position(|message| message.open && message.speaker == speaker)
    }

    /// 丢弃某条线路的未完成消息（不落历史）。
    fn remove_open(&mut self, speaker: Speaker) {
        if let Some(index) = self.open_index(speaker) {
            self.messages.remove(index);
        }
    }

    /// 把某条线路的未完成消息定稿：重复的最终结果或空译文直接丢弃。
    ///
    /// 定稿后消息要沉到所有未完成消息之后——未完成消息永远占据列表最前面，
    /// 这样「当前句」不会被别的新消息顶下去，只有它自己定稿时才让位（D39）。
    fn finalize_open(&mut self, speaker: Speaker, sentence_id: Option<u64>, now: Instant) {
        let Some(index) = self.open_index(speaker) else {
            return;
        };
        let slot = speaker.index();
        let duplicated = sentence_id.is_some() && self.last_finalized[slot] == sentence_id;
        if let Some(id) = sentence_id {
            self.last_finalized[slot] = Some(id);
        }

        let message = &mut self.messages[index];
        if duplicated || message.text.is_empty() {
            self.messages.remove(index);
            return;
        }
        message.open = false;
        message.sent_bytes = 0;
        message.source_sent_bytes = 0;
        message.sentence_id = sentence_id;
        message.finished_at = Some(now);

        // 剩下的未完成消息仍排在最前，把这条挪到它们后面。
        let remaining_open = self.messages.iter().filter(|message| message.open).count();
        if index != remaining_open {
            let message = self.messages.remove(index);
            self.messages.insert(remaining_open, message);
        }
    }

    /// 插到最前面并裁剪到上限（丢最旧的）。
    fn insert_front(&mut self, message: Message) {
        self.messages.insert(0, message);
        self.messages.truncate(MAX_MESSAGES);
    }

    /// 插到第一条历史消息之前（未完成消息之后），并裁剪到上限。
    fn insert_before_history(&mut self, message: Message) {
        let index = self.open_count();
        self.messages.insert(index, message);
        self.messages.truncate(MAX_MESSAGES);
    }

    /// 未完成消息数量：它们永远连续排在列表最前面。
    fn open_count(&self) -> usize {
        self.messages
            .iter()
            .take_while(|message| message.open)
            .count()
    }
}

/// 把「已送入前缀长度」夹到字符边界，避免切片 panic。
fn clamp_prefix_len(text: &str, sent: usize) -> usize {
    let mut sent = sent.min(text.len());
    while sent > 0 && !text.is_char_boundary(sent) {
        sent -= 1;
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn asr(timeline: &mut Timeline, speaker: Speaker, text: &str, sent: usize, end: bool) {
        timeline.apply_asr(speaker, text, end, sent, None, None, Instant::now());
    }

    fn asr_with_id(
        timeline: &mut Timeline,
        speaker: Speaker,
        text: &str,
        end: bool,
        id: Option<u64>,
    ) {
        timeline.apply_asr(speaker, text, end, 0, None, id, Instant::now());
    }

    fn translation(
        timeline: &mut Timeline,
        speaker: Speaker,
        text: &str,
        sent: usize,
        end: bool,
        id: Option<u64>,
    ) {
        timeline.apply_translation(
            speaker,
            text,
            sent,
            end,
            "en".to_string(),
            id,
            Instant::now(),
        );
    }

    #[test]
    fn asr_revision_updates_one_open_message() {
        let mut timeline = Timeline::default();
        asr(&mut timeline, Speaker::Local, "你", 0, false);
        asr(&mut timeline, Speaker::Local, "你好", 0, false);
        assert_eq!(timeline.messages().len(), 1, "修订不应新增消息");
        assert!(timeline.messages()[0].open);
        assert_eq!(timeline.messages()[0].text, "你好");
    }

    #[test]
    fn sentence_end_finalizes_in_place() {
        let mut timeline = Timeline::default();
        asr(&mut timeline, Speaker::Local, "你好", 0, false);
        asr(&mut timeline, Speaker::Local, "你好。", 0, true);
        assert_eq!(timeline.messages().len(), 1);
        assert!(!timeline.messages()[0].open, "句终后定稿");

        // 下一句插到最前面
        asr(&mut timeline, Speaker::Local, "再见。", 0, true);
        assert_eq!(timeline.messages().len(), 2);
        assert_eq!(timeline.messages()[0].text, "再见。");
        assert_eq!(timeline.messages()[1].text, "你好。");
    }

    #[test]
    fn empty_sentence_end_clears_open_and_dedupe() {
        let mut timeline = Timeline::default();
        asr_with_id(&mut timeline, Speaker::Local, "你好。", true, Some(1));
        assert_eq!(timeline.messages().len(), 1);

        // 同一 id 再下发一次：被去重丢弃
        asr_with_id(&mut timeline, Speaker::Local, "你好。", true, Some(1));
        assert_eq!(timeline.messages().len(), 1);

        // 空文本 + 句终 = ASR 重置信号：丢弃未完成消息并忘掉上一句 id
        asr(&mut timeline, Speaker::Local, "半句", 0, false);
        asr(&mut timeline, Speaker::Local, "", 0, true);
        assert_eq!(timeline.messages().len(), 1, "未完成消息被丢弃，历史保留");
        asr_with_id(&mut timeline, Speaker::Local, "你好。", true, Some(1));
        assert_eq!(timeline.messages().len(), 2, "重置后同一 id 可以再次落库");
    }

    #[test]
    fn different_ids_with_same_text_are_both_kept() {
        let mut timeline = Timeline::default();
        asr_with_id(&mut timeline, Speaker::Local, "嗯。", true, Some(1));
        asr_with_id(&mut timeline, Speaker::Local, "嗯。", true, Some(2));
        assert_eq!(timeline.messages().len(), 2, "两次真实发音各自保留");
    }

    #[test]
    fn sent_prefix_is_clamped_to_char_boundary() {
        let mut timeline = Timeline::default();
        timeline.apply_asr(Speaker::Local, "你好", false, 3, None, None, Instant::now());
        assert_eq!(timeline.messages()[0].sent_bytes, 3);
        timeline.apply_asr(Speaker::Local, "你好", false, 4, None, None, Instant::now());
        assert_eq!(
            timeline.messages()[0].sent_bytes,
            3,
            "落在字符中间时退回上一个边界"
        );
    }

    #[test]
    fn translation_mode_builds_bilingual_open_message() {
        let mut timeline = Timeline::default();
        timeline.set_translation(Speaker::Local, true);
        timeline.apply_asr(
            Speaker::Local,
            "你好",
            false,
            6,
            Some("zh".to_string()),
            None,
            Instant::now(),
        );
        assert_eq!(timeline.messages()[0].source, "你好");
        assert_eq!(timeline.messages()[0].source_language, "zh");
        assert_eq!(timeline.messages()[0].source_sent_bytes, 6);
        assert!(timeline.messages()[0].text.is_empty());

        // 翻译模式下 ASR 的句终标记不负责收口，由 Translation 事件负责（D26）。
        timeline.apply_asr(
            Speaker::Local,
            "你好。",
            true,
            "你好。".len(),
            Some("zh".to_string()),
            Some(9),
            Instant::now(),
        );
        assert!(timeline.messages()[0].open, "ASR 句终不应定稿");

        translation(&mut timeline, Speaker::Local, "Hello", 0, false, None);
        translation(&mut timeline, Speaker::Local, "Hello.", 5, true, Some(7));
        let message = &timeline.messages()[0];
        assert!(!message.open);
        assert_eq!(message.text, "Hello.");
        assert_eq!(message.language, "en");
        assert_eq!(message.source, "你好。");
        assert_eq!(message.source_language, "zh");
        assert_eq!(message.sent_bytes, 0, "定稿后不再分色");
    }

    #[test]
    fn translation_mode_empty_translation_is_dropped() {
        let mut timeline = Timeline::default();
        timeline.set_translation(Speaker::Local, true);
        timeline.apply_asr(Speaker::Local, "你好", false, 6, None, None, Instant::now());
        translation(&mut timeline, Speaker::Local, "", 0, true, Some(1));
        assert!(timeline.messages().is_empty(), "空译文句终不落历史");
    }

    #[test]
    fn typed_sentence_goes_to_history_below_open_message() {
        let mut timeline = Timeline::default();
        asr(&mut timeline, Speaker::Local, "正在识别", 0, false);
        timeline.push_typed("打字输入。".to_string(), "ja".to_string(), Instant::now());
        assert_eq!(timeline.messages().len(), 2);
        assert!(timeline.messages()[0].open, "当前句仍钉在最上面");
        assert_eq!(timeline.messages()[1].text, "打字输入。");
        assert!(!timeline.messages()[1].open);
    }

    #[test]
    fn open_sentence_stays_on_top_while_other_side_finishes() {
        let mut timeline = Timeline::default();
        timeline.apply_asr(
            Speaker::Local,
            "我在说话",
            false,
            0,
            None,
            None,
            Instant::now(),
        );
        // 对方开始并立刻完成一句：不能把我方当前句顶下去
        timeline.apply_asr(
            Speaker::Others,
            "对方说话。",
            true,
            0,
            None,
            None,
            Instant::now(),
        );
        assert_eq!(timeline.messages().len(), 2);
        assert!(timeline.messages()[0].open, "我方当前句仍在最上面");
        assert_eq!(timeline.messages()[0].text, "我在说话");
        assert_eq!(timeline.messages()[1].text, "对方说话。");
        assert!(!timeline.messages()[1].open, "定稿的消息在未完成消息之后");

        // 我方定稿后才让位到历史区（仍是历史里最新的一条）
        timeline.apply_asr(
            Speaker::Local,
            "我在说话。",
            true,
            0,
            None,
            None,
            Instant::now(),
        );
        assert!(!timeline.messages()[0].open);
        assert_eq!(timeline.messages()[0].text, "我在说话。");
        assert_eq!(timeline.messages()[1].text, "对方说话。");
    }

    #[test]
    fn both_open_sentences_stay_below_each_other() {
        let mut timeline = Timeline::default();
        timeline.apply_asr(Speaker::Local, "我方", false, 0, None, None, Instant::now());
        timeline.apply_asr(
            Speaker::Others,
            "对方",
            false,
            0,
            None,
            None,
            Instant::now(),
        );
        // 两条未完成消息都在开头，后开始的一条在上面
        assert_eq!(timeline.messages().len(), 2);
        assert!(timeline.messages()[0].open);
        assert!(timeline.messages()[1].open);
        assert_eq!(timeline.messages()[0].text, "对方");
        assert_eq!(timeline.messages()[1].text, "我方");
    }

    #[test]
    fn two_speakers_interleave_with_independent_dedupe() {
        let mut timeline = Timeline::default();
        // 同一 sentence_id 来自两条线路：各自的去重记忆互不影响
        asr_with_id(&mut timeline, Speaker::Local, "自己说的。", true, Some(1));
        asr_with_id(&mut timeline, Speaker::Others, "别人说的。", true, Some(1));
        assert_eq!(timeline.messages().len(), 2);

        // 两条线路可以同时各有一条未完成消息
        asr(&mut timeline, Speaker::Local, "自己", 0, false);
        asr(&mut timeline, Speaker::Others, "别人", 0, false);
        assert_eq!(timeline.messages().len(), 4);
        assert_eq!(
            timeline
                .messages()
                .iter()
                .filter(|message| message.open)
                .count(),
            2
        );

        // 更新他人线路不会动到我方的未完成消息
        asr(&mut timeline, Speaker::Others, "别人说话", 0, false);
        assert_eq!(timeline.messages().len(), 4);
        let view = timeline.speaker_view(Speaker::Local);
        assert_eq!(
            view.current.map(|message| message.text.as_str()),
            Some("自己")
        );
    }

    #[test]
    fn speaker_view_returns_history_newest_first() {
        let mut timeline = Timeline::default();
        assert!(timeline.speaker_view(Speaker::Local).history.is_empty());

        asr_with_id(&mut timeline, Speaker::Local, "第一句。", true, Some(1));
        timeline.push_typed("第二句。".to_string(), "zh".to_string(), Instant::now());
        asr_with_id(&mut timeline, Speaker::Others, "别人的。", true, Some(1));

        let view = timeline.speaker_view(Speaker::Local);
        assert!(view.current.is_none());
        let texts: Vec<_> = view
            .history
            .iter()
            .map(|message| message.text.as_str())
            .collect();
        assert_eq!(texts, vec!["第二句。", "第一句。"]);
        assert!(!view.translation);

        timeline.set_translation(Speaker::Local, true);
        assert!(timeline.speaker_view(Speaker::Local).translation);
        assert!(!timeline.speaker_view(Speaker::Others).translation);
    }

    #[test]
    fn finalize_records_finished_at() {
        let mut timeline = Timeline::default();
        let t0 = Instant::now();
        timeline.apply_asr(Speaker::Local, "半句", false, 0, None, None, t0);
        assert!(
            timeline.messages()[0].finished_at.is_none(),
            "未完成消息没有句终时间"
        );

        let t1 = t0 + Duration::from_secs(1);
        timeline.apply_asr(Speaker::Local, "半句。", true, 0, None, None, t1);
        assert_eq!(timeline.messages()[0].finished_at, Some(t1));

        let t2 = t1 + Duration::from_secs(2);
        timeline.push_typed("打字。".to_string(), "zh".to_string(), t2);
        assert_eq!(timeline.messages()[0].finished_at, Some(t2));
    }

    #[test]
    fn translation_finalize_records_finished_at() {
        let mut timeline = Timeline::default();
        timeline.set_translation(Speaker::Local, true);
        let t0 = Instant::now();
        timeline.apply_asr(Speaker::Local, "源文", false, 0, None, None, t0);

        let t1 = t0 + Duration::from_millis(500);
        timeline.apply_translation(
            Speaker::Local,
            "译文",
            0,
            true,
            "en".to_string(),
            Some(1),
            t1,
        );
        let message = &timeline.messages()[0];
        assert!(!message.open);
        assert_eq!(message.finished_at, Some(t1));
    }

    #[test]
    fn history_is_capped() {
        let mut timeline = Timeline::default();
        for index in 0..=MAX_MESSAGES {
            timeline.push_typed(format!("第 {index} 句。"), "zh".to_string(), Instant::now());
        }
        assert_eq!(timeline.messages().len(), MAX_MESSAGES);
        assert_eq!(
            timeline.messages()[0].text,
            format!("第 {MAX_MESSAGES} 句。"),
            "最新的保留在最前"
        );
        assert_eq!(
            timeline.messages()[MAX_MESSAGES - 1].text,
            "第 1 句。".to_string(),
            "最旧的一条被裁掉"
        );
    }

    #[test]
    fn reset_open_keeps_history() {
        let mut timeline = Timeline::default();
        asr(&mut timeline, Speaker::Local, "第一句。", 0, true);
        asr(&mut timeline, Speaker::Local, "半句", 0, false);
        asr(&mut timeline, Speaker::Others, "别人的半句", 0, false);

        timeline.reset_open();
        assert_eq!(timeline.messages().len(), 1, "未完成消息清理掉，历史保留");
        assert_eq!(timeline.messages()[0].text, "第一句。");
    }
}
