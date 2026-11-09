//! VRChat 聊天框文本合成与截断（纯逻辑，D33）。
//!
//! 输入是 [`SubtitleState`]（与 GUI 同一份字幕状态）与 `[vrchat.osc] display`，
//! 输出要发给 `/chatbox/input` 的文本。这里不碰网络、不依赖时间，便于单测。
//!
//! 截断规则（2026-11-09 实测后）：
//! - VRChat 最多 144 个 UTF-16 码元 / 9 个**显示行**（包含自动换行）；
//! - 默认 UI 下每行约 24 个 ASCII、15 个中文、12 个 emoji；超过 9 行时 VRChat
//!   保留**前 9 行**、裁掉尾部，因此客户端必须主动删头保尾；
//! - **保留句尾**（最新内容），被截掉的句头用前导 `…` 代替，不再在末尾加省略号；
//! - 双语按软 5:4 配额：字符约 79/64（正文 143 个码元），显示行默认 5/4；
//!   任一侧用不完时把额度借给另一侧，只有两侧都超过配额时才严格 5/4；

use crate::config::VrchatDisplay;
use crate::subtitles::SubtitleState;

/// VRChat ChatBox 的字符预算（按 UTF-16 码元计，最保守）。
pub const CHATBOX_MAX_UTF16_UNITS: usize = 144;
/// VRChat ChatBox 的最大显示行数。
pub const CHATBOX_MAX_LINES: usize = 9;

/// 实测：默认 UI 下每行约 24 个 ASCII 字符。
/// 用整数单位避免浮点误差：ASCII=3，CJK=5，emoji=6，行宽 72。
/// 这比实际略保守（实测 CJK 约 15 字/行，该模型取 14），保证不会因自动换行
/// 把尾部挤出 9 行。
const CHATBOX_LINE_UNITS: u32 = 72;
const ASCII_UNITS: u32 = 3;
const CJK_UNITS: u32 = 5;
const EMOJI_UNITS: u32 = 6;

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

    // 两段之间的换行占 1 个码元；剩余 143 个按 5:4 分给译文/原文。
    // 先按软配额分字符：哪一边用不完就把余额借给另一边。
    let content_budget = CHATBOX_MAX_UTF16_UNITS - 1;
    let base_translation = content_budget * 5 / 9;
    let base_source = content_budget - base_translation;
    let natural_translation = utf16_len(&normalize_lines(translation).join("\n"));
    let natural_source = utf16_len(&normalize_lines(source).join("\n"));

    let translation_units = if natural_translation <= base_translation {
        natural_translation
    } else if natural_source <= base_source {
        content_budget.saturating_sub(natural_source)
    } else {
        base_translation
    };
    let source_units = content_budget.saturating_sub(translation_units);

    // 先按字符配额裁一次，再用实际视觉行数分配 5/4 行配额。
    let translation = fit_tail(translation, translation_units, CHATBOX_MAX_LINES);
    let source = fit_tail(source, source_units, CHATBOX_MAX_LINES);
    if translation.is_empty() {
        return source;
    }
    if source.is_empty() {
        return translation;
    }

    let translation_lines = visual_line_count(&translation);
    let source_lines = visual_line_count(&source);
    if translation_lines + source_lines <= CHATBOX_MAX_LINES {
        return format!("{translation}\n{source}");
    }

    // 软 5/4：默认译文 5 行、原文 4 行；哪边用不到 5/4 行就把行数借给另一边。
    let (translation_cap, source_cap) = if translation_lines < 5 {
        (translation_lines, CHATBOX_MAX_LINES - translation_lines)
    } else if source_lines < 4 {
        (CHATBOX_MAX_LINES - source_lines, source_lines)
    } else {
        (5, 4)
    };

    let translation = fit_tail(&translation, translation_units, translation_cap);
    let source = fit_tail(&source, source_units, source_cap);
    if translation.is_empty() {
        source
    } else if source.is_empty() {
        translation
    } else {
        format!("{translation}\n{source}")
    }
}

/// 保留文本尾部，最多 `max_units` 个 UTF-16 码元、`max_lines` 个**实际显示行**。
///
/// 先按实测行宽估算自动换行（VRChat 的 9 行上限包含 word wrap），再从头部丢弃；
/// 若发生截断（丢行或丢字符），在保留内容前加 `…`。
fn fit_tail(text: &str, max_units: usize, max_lines: usize) -> String {
    if max_units == 0 || max_lines == 0 {
        return String::new();
    }

    let normalized = normalize_lines(text);
    if normalized.is_empty() {
        return String::new();
    }

    let mut body = normalized.join("\n");
    let mut cut = false;

    if utf16_len(&body) > max_units {
        body = tail_utf16(&body, max_units);
        cut = true;
    }

    let ranges = visual_line_ranges(&body);
    if ranges.len() > max_lines {
        let start = ranges[ranges.len() - max_lines].0;
        body = body[start..].to_string();
        cut = true;
    }

    if cut {
        add_head_ellipsis(body, max_units)
    } else {
        body
    }
}

/// 按 VRChat 实际显示宽度（编码见上面的单位常量）做贪心换行，返回每个视觉行的
/// 字节区间；**不插入换行符**，只用于判断是否超过 9 行。
fn visual_line_ranges(text: &str) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut width = 0u32;
    for (index, ch) in text.char_indices() {
        if ch == '\n' {
            ranges.push((start, index));
            start = index + 1;
            width = 0;
            continue;
        }
        let units = char_units(ch);
        if width + units > CHATBOX_LINE_UNITS && index > start {
            ranges.push((start, index));
            start = index;
            width = 0;
        }
        width += units;
    }
    ranges.push((start, text.len()));
    ranges
}

/// 在保留内容前加 `…`，并保证不因省略号多出一行 / 超过 144 码元。
///
/// 用 `…` **替换**首字符而不是插入：显示宽度与 UTF-16 码元数不会增加
/// （CJK/emoji 反而更小），因此不会把行数或 144 码元预算挤爆。
fn add_head_ellipsis(mut text: String, max_units: usize) -> String {
    if text.is_empty() {
        return if max_units == 0 {
            String::new()
        } else {
            "…".to_string()
        };
    }

    let Some(first) = text.chars().next() else {
        return String::new();
    };
    text.replace_range(0..first.len_utf8(), "…");

    while utf16_len(&text) > max_units {
        let Some((index, _)) = text.char_indices().nth(1) else {
            break;
        };
        text.remove(index);
    }
    text
}

fn visual_line_count(text: &str) -> usize {
    visual_line_ranges(text).len()
}

fn char_units(ch: char) -> u32 {
    let code = ch as u32;
    // 零宽修饰符不占显示宽度。
    if matches!(code, 0x200D | 0xFE0E | 0xFE0F | 0x1F3FB..=0x1F3FF) {
        return 0;
    }
    if is_emoji(code) {
        return EMOJI_UNITS;
    }
    if is_cjk(code) {
        return CJK_UNITS;
    }
    ASCII_UNITS
}

fn is_emoji(code: u32) -> bool {
    matches!(
        code,
        0x2300..=0x23FF
            | 0x2600..=0x27BF
            | 0x2B00..=0x2BFF
            | 0x1F000..=0x1FAFF
            | 0x1FC00..=0x1FFFD
    )
}

fn is_cjk(code: u32) -> bool {
    matches!(
        code,
        0x2E80..=0x2FFF
            | 0x3000..=0x303F
            | 0x3040..=0x30FF
            | 0x3100..=0x312F
            | 0x3130..=0x318F
            | 0x31A0..=0x31BF
            | 0x31F0..=0x31FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xA960..=0xA97F
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
            | 0xFE10..=0xFE1F
            | 0xFE30..=0xFE4F
            | 0xFF01..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1B000..=0x1B16F
    )
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
    fn wrapped_line_budget_keeps_tail_within_nine_lines() {
        // 144 ASCII 实测显示 6 行，应原样保留。
        let ascii = "A".repeat(144);
        assert_eq!(truncate_chatbox(&ascii), ascii);
        assert!(visual_line_count(&ascii) <= CHATBOX_MAX_LINES);

        // 纯中文 144 字符按实测宽度会超过 9 个显示行，必须裁头保尾。
        let cjk = format!("{}尾", "中".repeat(143));
        let fitted = truncate_chatbox(&cjk);
        assert!(fitted.ends_with('尾'), "{fitted}");
        assert!(fitted.starts_with('…'), "{fitted}");
        assert!(visual_line_count(&fitted) <= CHATBOX_MAX_LINES, "{fitted}");
        assert!(utf16_len(&fitted) <= CHATBOX_MAX_UTF16_UNITS);

        // 双语：译文 + 源文加起来也必须落在 9 个显示行内，且源文尾保留。
        let translation = format!("{}TRANS-TAIL", "译".repeat(65));
        let source = format!("{}SOURCE-TAIL", "源".repeat(65));
        let fitted = fit_bilingual(&translation, &source);
        assert!(fitted.contains("TRANS-TAIL"), "{fitted}");
        assert!(fitted.contains("SOURCE-TAIL"), "{fitted}");
        assert!(visual_line_count(&fitted) <= CHATBOX_MAX_LINES, "{fitted}");
        assert!(utf16_len(&fitted) <= CHATBOX_MAX_UTF16_UNITS);
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
    fn bilingual_soft_quota_borrows_unused_lines() {
        // 译文只有 2 行，原文 12 行：译文不占满 5 行，多出的 3 行借给原文。
        let translation = "译文 1\n译文 2";
        let source = (1..=12)
            .map(|index| format!("原文 {index:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let fitted = fit_bilingual(translation, &source);

        assert_eq!(fitted.lines().count(), CHATBOX_MAX_LINES, "{fitted}");
        assert!(fitted.starts_with("译文 1"), "{fitted}");
        assert!(fitted.contains("原文 12"), "{fitted}");
        assert!(!fitted.contains("原文 01"), "{fitted}");
        assert!(
            fitted
                .lines()
                .nth(2)
                .is_some_and(|line| line.starts_with('…')),
            "源文被裁时应保尾并加前导省略号: {fitted}"
        );
    }

    #[test]
    fn bilingual_both_over_quota_uses_five_four_lines() {
        // 两侧都超过配额：译文 5 行、原文 4 行。
        let translation = (1..=6)
            .map(|index| format!("T{index:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let source = (1..=6)
            .map(|index| format!("S{index:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let fitted = fit_bilingual(&translation, &source);
        let lines: Vec<_> = fitted.lines().collect();

        assert_eq!(lines.len(), CHATBOX_MAX_LINES, "{fitted}");
        assert!(lines[0].starts_with('…'), "译文被裁: {fitted}");
        assert!(lines[5].starts_with('…'), "源文被裁: {fitted}");
        assert!(lines[4].contains("T06"), "译文应保尾: {fitted}");
        assert!(lines[8].contains("S06"), "源文应保尾: {fitted}");
    }

    #[test]
    fn bilingual_exact_nine_lines_is_not_truncated() {
        // 6 + 3 正好 9 行：不应按 5/4 硬切。
        let translation = (1..=6)
            .map(|index| format!("T{index:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let source = (1..=3)
            .map(|index| format!("S{index:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let fitted = fit_bilingual(&translation, &source);

        assert_eq!(fitted.lines().count(), CHATBOX_MAX_LINES, "{fitted}");
        assert!(!fitted.contains('…'), "{fitted}");
        assert!(fitted.contains("T06"));
        assert!(fitted.contains("S03"));
    }

    #[test]
    fn bilingual_character_quota_is_five_four_and_keeps_tails() {
        // 两侧都远超字符预算：144 字符（含分隔换行）按约 5:4 分。
        let translation = format!("{}TRANS-TAIL", "T".repeat(200));
        let source = format!("{}SOURCE-TAIL", "S".repeat(200));
        let fitted = fit_bilingual(&translation, &source);
        let (translation, source) = fitted.split_once('\n').expect("bilingual separator");

        assert!(utf16_len(&fitted) <= CHATBOX_MAX_UTF16_UNITS, "{fitted}");
        assert!(translation.starts_with('…'), "{fitted}");
        assert!(source.starts_with('…'), "{fitted}");
        assert!(translation.ends_with("TRANS-TAIL"), "{fitted}");
        assert!(source.ends_with("SOURCE-TAIL"), "{fitted}");
        assert!(
            utf16_len(translation) > utf16_len(source),
            "字符预算应偏向译文: {fitted}"
        );
        assert!(visual_line_count(&fitted) <= CHATBOX_MAX_LINES, "{fitted}");
    }

    #[test]
    fn bilingual_short_side_lends_character_budget() {
        // 译文很短时，原文可以使用超过 4/9 的字符预算，但仍保留 5:4 的软上限语义。
        let translation = "短";
        let source = format!("{}SOURCE-TAIL", "S".repeat(200));
        let fitted = fit_bilingual(translation, &source);
        let (_, source_part) = fitted.split_once('\n').expect("bilingual separator");

        assert!(utf16_len(&fitted) <= CHATBOX_MAX_UTF16_UNITS);
        assert!(source_part.ends_with("SOURCE-TAIL"), "{fitted}");
        assert!(utf16_len(source_part) > 64, "{fitted}");
        assert!(visual_line_count(&fitted) <= CHATBOX_MAX_LINES, "{fitted}");
    }

    #[test]
    fn bilingual_short_source_still_gets_its_quota() {
        let translation = "译".repeat(200);
        let fitted = fit_bilingual(&translation, "源文");

        assert!(utf16_len(&fitted) <= CHATBOX_MAX_UTF16_UNITS);
        assert!(fitted.contains("源文"), "{fitted}");
        assert!(visual_line_count(&fitted) <= CHATBOX_MAX_LINES, "{fitted}");
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
