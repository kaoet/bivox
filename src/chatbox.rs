//! VRChat 聊天框文本合成与截断（纯逻辑，D33）。
//!
//! 输入是 [`SubtitleState`]（与 GUI 同一份字幕状态）与 `[vrchat.osc] display`，
//! 输出要发给 `/chatbox/input` 的文本。这里不碰网络、不依赖时间，便于单测。
//!
//! 截断规则（2026-11-08 实测后）：
//! - VRChat 最多 144 字符 / 9 行；
//! - **保留句尾**（最新内容），被截掉的句头用前导 `…` 代替，不再在末尾加省略号；
//! - 双语时译文优先分配预算，源文用剩余预算，两条都保尾。

use crate::config::VrchatDisplay;
use crate::subtitles::SubtitleState;

/// VRChat ChatBox 的字符预算（按 UTF-16 码元计，最保守）。
pub const CHATBOX_MAX_UTF16_UNITS: usize = 144;
/// VRChat ChatBox 的最大显示行数。
pub const CHATBOX_MAX_LINES: usize = 9;

/// 一次合成结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatboxText {
    /// 要发送的文本（已截断）。
    Text(String),
    /// 没有可更新的内容；保持聊天框现状（不要发空串）。
    Keep,
}

/// 根据字幕状态与显示模式合成聊天框文本。
pub fn compose(state: &SubtitleState, display: VrchatDisplay) -> ChatboxText {
    if !state.translation_mode {
        let text = if !state.current.is_empty() {
            &state.current
        } else if let Some(line) = state.history.last() {
            &line.text
        } else {
            return ChatboxText::Keep;
        };
        return text_or_keep(fit_single(text));
    }

    let has_current = !state.current.is_empty() || !state.current_translation.is_empty();
    let (source, translation) = if has_current {
        (state.current.as_str(), state.current_translation.as_str())
    } else if let Some(line) = state.history.last() {
        (line.source.as_str(), line.text.as_str())
    } else {
        return ChatboxText::Keep;
    };

    let text = match display {
        VrchatDisplay::Source => fit_single(source),
        VrchatDisplay::Translation => fit_single(translation),
        VrchatDisplay::Bilingual => fit_bilingual(translation, source),
    };
    text_or_keep(text)
}

fn text_or_keep(text: String) -> ChatboxText {
    if text.is_empty() {
        ChatboxText::Keep
    } else {
        ChatboxText::Text(text)
    }
}

/// 把任意文本截成 VRChat 可接受的形式（保尾、9 行、144 UTF-16 码元）。
///
/// 对外主要给测试与调试用；正常路径由 [`compose`] 调用。
pub fn truncate_chatbox(text: &str) -> String {
    fit_single(text)
}

fn fit_single(text: &str) -> String {
    fit_tail(text, CHATBOX_MAX_UTF16_UNITS, CHATBOX_MAX_LINES)
}

fn fit_bilingual(translation: &str, source: &str) -> String {
    let translation = translation.trim();
    let source = source.trim();

    if translation.is_empty() {
        return fit_single(source);
    }
    if source.is_empty() {
        return fit_single(translation);
    }

    let translation = fit_tail(translation, CHATBOX_MAX_UTF16_UNITS, CHATBOX_MAX_LINES);
    if translation.is_empty() {
        return fit_single(source);
    }

    let used = utf16_len(&translation);
    let translation_lines = line_count(&translation);
    if used >= CHATBOX_MAX_UTF16_UNITS || translation_lines >= CHATBOX_MAX_LINES {
        return translation;
    }

    // 1 个码元留给两行之间的换行。
    let remaining = CHATBOX_MAX_UTF16_UNITS - used - 1;
    let remaining_lines = CHATBOX_MAX_LINES - translation_lines;
    if remaining <= 1 || remaining_lines == 0 {
        return translation;
    }

    let source = fit_tail(source, remaining, remaining_lines);
    if source.is_empty() {
        translation
    } else {
        format!("{translation}\n{source}")
    }
}

/// 保留文本尾部，最多 `max_units` 个 UTF-16 码元、`max_lines` 行。
///
/// 若发生截断（丢行或丢字符），在保留内容前加 `…`；`…` 也计入预算。
fn fit_tail(text: &str, max_units: usize, max_lines: usize) -> String {
    if max_units == 0 || max_lines == 0 {
        return String::new();
    }

    let mut lines = normalize_lines(text);
    if lines.is_empty() {
        return String::new();
    }

    let mut head_cut = false;
    if lines.len() > max_lines {
        lines = lines.split_off(lines.len() - max_lines);
        head_cut = true;
    }
    let joined = lines.join("\n");

    if !head_cut && utf16_len(&joined) <= max_units {
        return joined;
    }

    let body_budget = max_units.saturating_sub(1);
    let body = tail_utf16(&joined, body_budget);
    if body.is_empty() {
        // 只放得下省略号时，至少留下省略号。
        return "…".to_string();
    }
    format!("…{body}")
}

fn normalize_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect();

    while lines.first().is_some_and(|line| line.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }

    let mut collapsed = Vec::with_capacity(lines.len());
    let mut previous_empty = false;
    for line in lines {
        let empty = line.is_empty();
        if empty && previous_empty {
            continue;
        }
        previous_empty = empty;
        collapsed.push(line);
    }
    collapsed
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn line_count(text: &str) -> usize {
    text.split('\n').count()
}

/// 从尾部保留最多 `max_units` 个 UTF-16 码元（不切断 char / 代理对）。
fn tail_utf16(text: &str, max_units: usize) -> String {
    let mut used = 0usize;
    let mut start = text.len();
    for (index, ch) in text.char_indices().rev() {
        let units = ch.len_utf16();
        if used + units > max_units {
            break;
        }
        used += units;
        start = index;
    }
    text[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitles::{SubtitleLine, SubtitleState};

    fn line(text: &str, source: &str) -> SubtitleLine {
        SubtitleLine {
            language: "en".to_string(),
            text: text.to_string(),
            source: source.to_string(),
            source_language: "zh".to_string(),
            sentence_id: None,
        }
    }

    #[test]
    fn truncate_keeps_tail_and_prefixes_ellipsis() {
        let text = "A".repeat(200);
        let fitted = truncate_chatbox(&text);

        assert!(fitted.starts_with('…'), "{fitted}");
        assert!(fitted.ends_with('A'), "{fitted}");
        assert!(utf16_len(&fitted) <= CHATBOX_MAX_UTF16_UNITS);
        assert_eq!(utf16_len(&fitted), CHATBOX_MAX_UTF16_UNITS);
    }

    #[test]
    fn truncate_keeps_last_lines() {
        let text = (0..15)
            .map(|index| format!("line-{index:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let fitted = truncate_chatbox(&text);

        assert_eq!(fitted.lines().count(), CHATBOX_MAX_LINES);
        assert!(fitted.starts_with('…'), "{fitted}");
        assert!(fitted.contains("line-14"), "{fitted}");
        assert!(!fitted.contains("line-00"), "{fitted}");
    }

    #[test]
    fn bilingual_keeps_both_tails_within_budget() {
        let translation = format!("{}TAIL-TRANSLATION", "译".repeat(60));
        let source = format!("{}TAIL-SOURCE", "原".repeat(60));
        let fitted = fit_bilingual(&translation, &source);

        assert!(utf16_len(&fitted) <= CHATBOX_MAX_UTF16_UNITS, "{fitted}");
        assert!(fitted.contains("TAIL-TRANSLATION"), "{fitted}");
        assert!(fitted.contains("TAIL-SOURCE"), "{fitted}");
        assert!(
            fitted
                .lines()
                .nth(1)
                .is_some_and(|line| line.starts_with('…')),
            "源文被截断时应保尾并加前导省略号: {fitted}"
        );
    }

    #[test]
    fn bilingual_translation_that_fills_budget_omits_source() {
        let translation = "译".repeat(200);
        let fitted = fit_bilingual(&translation, "源文");

        assert!(utf16_len(&fitted) <= CHATBOX_MAX_UTF16_UNITS);
        assert!(!fitted.contains("源文"), "{fitted}");
    }

    #[test]
    fn compose_prefers_current_then_last_history() {
        let mut state = SubtitleState::default();
        assert_eq!(compose(&state, VrchatDisplay::Source), ChatboxText::Keep);

        state.history.push(line("旧句。", ""));
        assert_eq!(
            compose(&state, VrchatDisplay::Source),
            ChatboxText::Text("旧句。".to_string())
        );

        state.current = "当前句。".to_string();
        assert_eq!(
            compose(&state, VrchatDisplay::Source),
            ChatboxText::Text("当前句。".to_string())
        );
    }

    #[test]
    fn compose_translation_modes() {
        let mut state = SubtitleState::default();
        state.set_translation_mode(true);
        state.current = "源文".to_string();
        state.current_translation = "translation".to_string();

        assert_eq!(
            compose(&state, VrchatDisplay::Source),
            ChatboxText::Text("源文".to_string())
        );
        assert_eq!(
            compose(&state, VrchatDisplay::Translation),
            ChatboxText::Text("translation".to_string())
        );
        assert_eq!(
            compose(&state, VrchatDisplay::Bilingual),
            ChatboxText::Text("translation\n源文".to_string())
        );

        // 只显示译文且译文未到：不要回退显示原文。
        state.current_translation.clear();
        assert_eq!(
            compose(&state, VrchatDisplay::Translation),
            ChatboxText::Keep
        );
        assert_eq!(
            compose(&state, VrchatDisplay::Bilingual),
            ChatboxText::Text("源文".to_string())
        );
    }

    #[test]
    fn emoji_truncation_does_not_split_surrogate_pair() {
        let text = "😀".repeat(200);
        let fitted = truncate_chatbox(&text);
        assert!(fitted.starts_with('…'));
        assert!(utf16_len(&fitted) <= CHATBOX_MAX_UTF16_UNITS);
        // 结果必须是合法 UTF-8（能构造出 String 就已经满足），再确认没有半个 emoji。
        assert!(fitted.chars().all(|ch| ch == '…' || ch == '😀'));
    }
}
