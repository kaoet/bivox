//! VRChat 聊天框文本合成与截断（纯逻辑，D33/D39/D42）。
//!
//! 输入是某条线路的 [`SpeakerView`]（与 GUI 同一份时间线状态里的我方投影）与
//! [`ChatboxOptions`]，输出要发给 `/chatbox/input` 的文本。这里不碰网络；当前时间由
//! `now` 传入，便于单测。
//!
//! 组合规则（D42）：
//! - 优先级：当前句 → 最近历史句 → 第二近 → …；历史句只在 `age ≤ retention` 时参与；
//! - 从 144 UTF-16 码元 / 9 个显示行出发逐句装填；整句放不下就按剩余配额截断
//!   （保尾 + 前导 `…`）后停止，因此至多一条句子被截断；
//! - 显示顺序为「当前句在上，历史向下越来越旧」；关闭历史句时最多装 1 块；
//! - 双语每句是一个块（译文\n原文），剩余行数为奇数时译文多一行；保留软借用。
//!
//! 截断规则（2026-11-09 实测后）：
//! - VRChat 最多 144 个 UTF-16 码元 / 9 个**显示行**（包含自动换行）；
//! - 默认 UI 下每行约 24 个 ASCII、15 个中文、12 个 emoji；超过 9 行时 VRChat
//!   保留**前 9 行**、裁掉尾部，因此客户端必须主动删头保尾；
//! - **保留句尾**（最新内容），被截掉的句头用前导 `…` 代替，不再在末尾加省略号；
//! - 双语按软 5:4 配额：字符约 79/64（正文 143 个码元），显示行默认 5/4；
//!   任一侧用不完时把额度借给另一侧，只有两侧都超过配额时才严格 5/4；

use std::time::{Duration, Instant};

use crate::config::VrchatDisplay;
use crate::timeline::{Message, SpeakerView};

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
    /// 没有可显示内容；发空串清空聊天框。
    Clear,
}

/// 一次合成使用的显示设置（D42）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatboxOptions {
    /// 翻译模式下的显示内容；非翻译模式固定显示 ASR 原文。
    pub display: VrchatDisplay,
    /// 是否在聊天框里同时保留历史句。
    pub show_history: bool,
    /// 句终后的保留时长：历史句年龄上限，也是最后一句的存活时长。
    pub retention: Duration,
}

/// 根据某条线路的时间线视图与显示设置合成聊天框文本。
///
/// 调用方传入 `timeline.speaker_view(Speaker::Local)`：聊天框只镜像我方。
pub fn compose(view: &SpeakerView<'_>, options: ChatboxOptions, now: Instant) -> ChatboxText {
    let mut blocks = Vec::new();
    let mut units_left = CHATBOX_MAX_UTF16_UNITS;
    let mut lines_left = CHATBOX_MAX_LINES;
    // 关闭历史句时最多装一块（当前句，或当前句不可渲染时的最后一条历史句）。
    let max_blocks = if options.show_history { usize::MAX } else { 1 };

    if let Some(current) = view.current
        && let Some(truncated) = add_block(
            current,
            view.translation,
            options.display,
            &mut units_left,
            &mut lines_left,
            &mut blocks,
        )
        && (truncated || blocks.len() >= max_blocks)
    {
        return finish(blocks);
    }

    for message in &view.history {
        if blocks.len() >= max_blocks {
            break;
        }
        let Some(finished_at) = message.finished_at else {
            continue;
        };
        if now.saturating_duration_since(finished_at) > options.retention {
            // 历史按新→旧排列，后面的只会更旧。
            break;
        }
        if let Some(truncated) = add_block(
            message,
            view.translation,
            options.display,
            &mut units_left,
            &mut lines_left,
            &mut blocks,
        ) && (truncated || blocks.len() >= max_blocks)
        {
            break;
        }
    }

    finish(blocks)
}

fn finish(blocks: Vec<String>) -> ChatboxText {
    if blocks.is_empty() {
        ChatboxText::Clear
    } else {
        ChatboxText::Text(blocks.join("\n"))
    }
}

/// 把一个消息块装入剩余预算；放不下时按剩余配额截断。
///
/// 返回 `None` 表示这条消息在当前显示模式下不可渲染（例如只显示译文但译文未到），
/// 调用方应继续尝试下一条；返回 `Some(truncated)` 表示已追加一块。
fn add_block(
    message: &Message,
    translation_mode: bool,
    display: VrchatDisplay,
    units_left: &mut usize,
    lines_left: &mut usize,
    blocks: &mut Vec<String>,
) -> Option<bool> {
    let separator = usize::from(!blocks.is_empty());
    if *lines_left == 0 || *units_left <= separator {
        return None;
    }
    let (block, truncated) = fit_message(
        message,
        translation_mode,
        display,
        *units_left - separator,
        *lines_left,
    )?;
    if !has_visible_text(&block) {
        return None;
    }
    *units_left -= separator + utf16_len(&block);
    *lines_left = lines_left.saturating_sub(visual_line_count(&block));
    blocks.push(block);
    Some(truncated)
}

fn fit_message(
    message: &Message,
    translation_mode: bool,
    display: VrchatDisplay,
    units: usize,
    lines: usize,
) -> Option<(String, bool)> {
    if !translation_mode {
        return fit_single_side(&message.text, units, lines);
    }
    match display {
        VrchatDisplay::Source => fit_single_side(&message.source, units, lines),
        VrchatDisplay::Translation => fit_single_side(&message.text, units, lines),
        VrchatDisplay::Bilingual => fit_bilingual(&message.text, &message.source, units, lines),
    }
}

fn fit_single_side(text: &str, units: usize, lines: usize) -> Option<(String, bool)> {
    let natural = normalize_block(text);
    if natural.is_empty() {
        return None;
    }
    if utf16_len(&natural) <= units && visual_line_count(&natural) <= lines {
        return Some((natural, false));
    }
    let fitted = fit_tail(text, units, lines);
    if fitted.is_empty() {
        return None;
    }
    Some((fitted, true))
}

/// 合成一个双语块；`(block, truncated)`。
///
/// 内部换行占 1 个码元；剩余行数按 `ceil/floor` 分给译文/原文（奇数时译文多一行），
/// 字符按同一比例给基础配额。保留软借用：一侧自然用量低于自己的份额时，把剩余额度
/// 借给另一侧（D33/D42）。
fn fit_bilingual(
    translation: &str,
    source: &str,
    max_units: usize,
    max_lines: usize,
) -> Option<(String, bool)> {
    let translation_text = normalize_block(translation);
    let source_text = normalize_block(source);

    match (translation_text.is_empty(), source_text.is_empty()) {
        (true, true) => return None,
        (true, false) => return fit_single_side(&source_text, max_units, max_lines),
        (false, true) => return fit_single_side(&translation_text, max_units, max_lines),
        (false, false) => {}
    }

    let natural = format!("{translation_text}\n{source_text}");
    if utf16_len(&natural) <= max_units && visual_line_count(&natural) <= max_lines {
        return Some((natural, false));
    }
    if max_lines == 0 || max_units < 2 {
        return None;
    }

    let content_budget = max_units - 1;
    let translation_share = max_lines.div_ceil(2);
    let source_share = max_lines / 2;
    let base_translation = content_budget * translation_share / max_lines;
    let base_source = content_budget - base_translation;
    let natural_translation = utf16_len(&translation_text);
    let natural_source = utf16_len(&source_text);

    let translation_units = if natural_translation <= base_translation {
        natural_translation
    } else if natural_source <= base_source {
        content_budget.saturating_sub(natural_source)
    } else {
        base_translation
    };
    let source_units = content_budget.saturating_sub(translation_units);

    // 先按字符配额裁一次，再用实际视觉行数分配行配额。
    let translation_fit = fit_tail(&translation_text, translation_units, max_lines);
    let source_fit = fit_tail(&source_text, source_units, max_lines);
    let translation_visible = has_visible_text(&translation_fit);
    let source_visible = has_visible_text(&source_fit);
    if !translation_visible && !source_visible {
        return None;
    }
    // 有一侧被挤没时，这句话已经算被截断（只是保留了另一侧的完整内容），
    // 不应再在这句之后继续装更旧的历史句。
    if !translation_visible {
        return fit_single_side(&source_text, max_units, max_lines).map(|(block, _)| (block, true));
    }
    if !source_visible {
        return fit_single_side(&translation_text, max_units, max_lines)
            .map(|(block, _)| (block, true));
    }

    let translation_lines = visual_line_count(&translation_fit);
    let source_lines = visual_line_count(&source_fit);
    if translation_lines + source_lines <= max_lines {
        return Some((format!("{translation_fit}\n{source_fit}"), true));
    }

    // 奇数行时译文多一行；哪边用不到自己的份额就把行数借给另一边。
    let (translation_cap, source_cap) = if translation_lines < translation_share {
        (translation_lines, max_lines - translation_lines)
    } else if source_lines < source_share {
        (max_lines - source_lines, source_lines)
    } else {
        (translation_share, source_share)
    };

    let translation_fit = fit_tail(&translation_text, translation_units, translation_cap);
    let source_fit = fit_tail(&source_text, source_units, source_cap);
    match (
        has_visible_text(&translation_fit),
        has_visible_text(&source_fit),
    ) {
        (true, true) => Some((format!("{translation_fit}\n{source_fit}"), true)),
        (true, false) => {
            fit_single_side(&translation_text, max_units, max_lines).map(|(block, _)| (block, true))
        }
        (false, true) => {
            fit_single_side(&source_text, max_units, max_lines).map(|(block, _)| (block, true))
        }
        (false, false) => None,
    }
}

/// 把任意文本截成 VRChat 可接受的形式（保尾、9 行、144 UTF-16 码元）。
///
/// 对外主要给测试与调试用；正常路径由 [`compose`] 调用。
pub fn truncate_chatbox(text: &str) -> String {
    fit_tail(text, CHATBOX_MAX_UTF16_UNITS, CHATBOX_MAX_LINES)
}

/// 文本是否含有 `…` 和空白之外的可见内容。
fn has_visible_text(text: &str) -> bool {
    text.chars().any(|ch| ch != '…' && !ch.is_whitespace())
}

fn normalize_block(text: &str) -> String {
    normalize_lines(text).join("\n")
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
    use crate::events::Speaker;
    use crate::timeline::Timeline;
    use std::time::{Duration, Instant};

    /// 旧签名包装：关闭历史句、30 秒保留，便于复用原有单句用例。
    fn compose(view: &SpeakerView<'_>, display: VrchatDisplay) -> ChatboxText {
        super::compose(
            view,
            ChatboxOptions {
                display,
                show_history: false,
                retention: Duration::from_secs(30),
            },
            Instant::now(),
        )
    }

    /// 旧签名包装：完整预算下的双语块。
    fn fit_bilingual(translation: &str, source: &str) -> String {
        super::fit_bilingual(
            translation,
            source,
            CHATBOX_MAX_UTF16_UNITS,
            CHATBOX_MAX_LINES,
        )
        .expect("expected bilingual block")
        .0
    }

    fn compose_with(
        timeline: &Timeline,
        display: VrchatDisplay,
        show_history: bool,
        retention_secs: u64,
        now: Instant,
    ) -> ChatboxText {
        super::compose(
            &timeline.speaker_view(Speaker::Local),
            ChatboxOptions {
                display,
                show_history,
                retention: Duration::from_secs(retention_secs),
            },
            now,
        )
    }

    fn finalize(timeline: &mut Timeline, text: &str, at: Instant) {
        timeline.apply_asr(Speaker::Local, text, true, 0, None, None, at);
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
        let mut timeline = Timeline::default();
        assert_eq!(
            compose(
                &timeline.speaker_view(Speaker::Local),
                VrchatDisplay::Source
            ),
            ChatboxText::Clear
        );

        timeline.push_typed("旧句。".to_string(), "zh".to_string(), Instant::now());
        assert_eq!(
            compose(
                &timeline.speaker_view(Speaker::Local),
                VrchatDisplay::Source
            ),
            ChatboxText::Text("旧句。".to_string())
        );

        timeline.apply_asr(
            Speaker::Local,
            "当前句。",
            false,
            0,
            None,
            None,
            Instant::now(),
        );
        assert_eq!(
            compose(
                &timeline.speaker_view(Speaker::Local),
                VrchatDisplay::Source
            ),
            ChatboxText::Text("当前句。".to_string())
        );
    }

    #[test]
    fn compose_translation_modes() {
        let mut timeline = Timeline::default();
        timeline.set_translation(Speaker::Local, true);
        timeline.apply_asr(Speaker::Local, "源文", false, 0, None, None, Instant::now());

        // 译文还没到：只显示译文时没有可显示内容；双语只有原文。
        assert_eq!(
            compose(
                &timeline.speaker_view(Speaker::Local),
                VrchatDisplay::Translation
            ),
            ChatboxText::Clear
        );
        assert_eq!(
            compose(
                &timeline.speaker_view(Speaker::Local),
                VrchatDisplay::Bilingual
            ),
            ChatboxText::Text("源文".to_string())
        );

        timeline.apply_translation(
            Speaker::Local,
            "translation",
            0,
            false,
            "en".to_string(),
            None,
            Instant::now(),
        );
        assert_eq!(
            compose(
                &timeline.speaker_view(Speaker::Local),
                VrchatDisplay::Source
            ),
            ChatboxText::Text("源文".to_string())
        );
        assert_eq!(
            compose(
                &timeline.speaker_view(Speaker::Local),
                VrchatDisplay::Translation
            ),
            ChatboxText::Text("translation".to_string())
        );
        assert_eq!(
            compose(
                &timeline.speaker_view(Speaker::Local),
                VrchatDisplay::Bilingual
            ),
            ChatboxText::Text("translation\n源文".to_string())
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

    #[test]
    fn history_renders_current_then_newest_history() {
        let mut timeline = Timeline::default();
        let t0 = Instant::now();
        finalize(&mut timeline, "一号", t0);
        finalize(&mut timeline, "二号", t0 + Duration::from_secs(1));
        timeline.apply_asr(
            Speaker::Local,
            "当前",
            false,
            0,
            None,
            None,
            t0 + Duration::from_secs(2),
        );

        let now = t0 + Duration::from_secs(2);
        assert_eq!(
            compose_with(&timeline, VrchatDisplay::Source, true, 30, now),
            ChatboxText::Text("当前\n二号\n一号".to_string()),
            "当前句在上，历史向下越来越旧"
        );
        assert_eq!(
            compose_with(&timeline, VrchatDisplay::Source, false, 30, now),
            ChatboxText::Text("当前".to_string()),
            "关闭历史句时只显示当前句"
        );
    }

    #[test]
    fn history_drops_expired_sentences() {
        let mut timeline = Timeline::default();
        let t0 = Instant::now();
        finalize(&mut timeline, "旧句", t0);
        finalize(&mut timeline, "新句", t0 + Duration::from_secs(10));

        assert_eq!(
            compose_with(
                &timeline,
                VrchatDisplay::Source,
                true,
                30,
                t0 + Duration::from_secs(35)
            ),
            ChatboxText::Text("新句".to_string()),
            "超过保留时长的历史句不显示"
        );
        assert_eq!(
            compose_with(
                &timeline,
                VrchatDisplay::Source,
                true,
                30,
                t0 + Duration::from_secs(41)
            ),
            ChatboxText::Clear,
            "全部过期后清空"
        );
    }

    #[test]
    fn history_off_clears_last_sentence_after_retention() {
        let mut timeline = Timeline::default();
        let t0 = Instant::now();
        finalize(&mut timeline, "最后一句", t0);

        assert_eq!(
            compose_with(
                &timeline,
                VrchatDisplay::Source,
                false,
                15,
                t0 + Duration::from_secs(15)
            ),
            ChatboxText::Text("最后一句".to_string()),
            "正好到保留时长时仍显示"
        );
        assert_eq!(
            compose_with(
                &timeline,
                VrchatDisplay::Source,
                false,
                15,
                t0 + Duration::from_secs(16)
            ),
            ChatboxText::Clear,
            "关闭历史句时保留时长同样生效"
        );
    }

    #[test]
    fn history_truncates_overflowing_sentence_then_stops() {
        let mut timeline = Timeline::default();
        let t0 = Instant::now();
        timeline.push_typed("一号".to_string(), "zh".to_string(), t0);
        timeline.push_typed(
            "A".repeat(200),
            "en".to_string(),
            t0 + Duration::from_secs(1),
        );
        timeline.apply_asr(
            Speaker::Local,
            "当前",
            false,
            0,
            None,
            None,
            t0 + Duration::from_secs(2),
        );

        let now = t0 + Duration::from_secs(2);
        let ChatboxText::Text(text) = compose_with(&timeline, VrchatDisplay::Source, true, 30, now)
        else {
            panic!("expected text");
        };
        assert!(text.starts_with("当前\n…"), "{text}");
        assert!(text.contains('A'));
        assert!(!text.contains("一号"), "截断后不再装更旧的历史句: {text}");
        assert!(utf16_len(&text) <= CHATBOX_MAX_UTF16_UNITS, "{text}");
        assert!(visual_line_count(&text) <= CHATBOX_MAX_LINES, "{text}");
    }

    #[test]
    fn history_does_not_emit_ellipsis_only_block() {
        let mut timeline = Timeline::default();
        let t0 = Instant::now();
        finalize(&mut timeline, "历史", t0);
        let full = "A".repeat(CHATBOX_MAX_UTF16_UNITS);
        timeline.apply_asr(
            Speaker::Local,
            &full,
            false,
            0,
            None,
            None,
            t0 + Duration::from_secs(1),
        );

        assert_eq!(
            compose_with(
                &timeline,
                VrchatDisplay::Source,
                true,
                30,
                t0 + Duration::from_secs(1)
            ),
            ChatboxText::Text(full),
            "预算用尽时不发只有 … 的块"
        );
    }

    #[test]
    fn bilingual_history_uses_remaining_odd_lines_for_translation() {
        let mut timeline = Timeline::default();
        timeline.set_translation(Speaker::Local, true);
        let t0 = Instant::now();
        // 历史句：译文 4 行、原文 4 行。
        timeline.apply_asr(Speaker::Local, "S1\nS2\nS3\nS4", false, 0, None, None, t0);
        timeline.apply_translation(
            Speaker::Local,
            "T1\nT2\nT3\nT4",
            0,
            true,
            "en".to_string(),
            None,
            t0 + Duration::from_secs(1),
        );
        // 当前句 4 行（译文未到，bilingual 下只显示原文）。
        timeline.apply_asr(
            Speaker::Local,
            "C1\nC2\nC3\nC4",
            false,
            0,
            None,
            None,
            t0 + Duration::from_secs(2),
        );

        let now = t0 + Duration::from_secs(2);
        let ChatboxText::Text(text) =
            compose_with(&timeline, VrchatDisplay::Bilingual, true, 30, now)
        else {
            panic!("expected text");
        };
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), CHATBOX_MAX_LINES, "{text}");
        assert_eq!(&lines[..4], &["C1", "C2", "C3", "C4"], "{text}");
        // 剩余 5 行：译文 3 行、原文 2 行（奇数给译文多一行）。
        assert_eq!(lines[6], "T4", "{text}");
        assert_eq!(lines[8], "S4", "{text}");
        assert!(lines[4].starts_with('…'), "{text}");
        assert!(lines[7].starts_with('…'), "{text}");
    }

    #[test]
    fn bilingual_history_borrows_unused_lines_in_small_budget() {
        let mut timeline = Timeline::default();
        timeline.set_translation(Speaker::Local, true);
        let t0 = Instant::now();
        let source = (1..=10)
            .map(|index| format!("S{index:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        timeline.apply_asr(Speaker::Local, &source, false, 0, None, None, t0);
        timeline.apply_translation(
            Speaker::Local,
            "T1\nT2",
            0,
            true,
            "en".to_string(),
            None,
            t0 + Duration::from_secs(1),
        );
        timeline.apply_asr(
            Speaker::Local,
            "C1\nC2\nC3\nC4",
            false,
            0,
            None,
            None,
            t0 + Duration::from_secs(2),
        );

        let now = t0 + Duration::from_secs(2);
        let ChatboxText::Text(text) =
            compose_with(&timeline, VrchatDisplay::Bilingual, true, 30, now)
        else {
            panic!("expected text");
        };
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), CHATBOX_MAX_LINES, "{text}");
        // 译文只有 2 行，不截断；原文借用剩下的 3 行。
        assert_eq!(&lines[4..6], &["T1", "T2"], "{text}");
        assert!(lines[6].starts_with('…'), "{text}");
        assert_eq!(lines[8], "S10", "{text}");
    }

    #[test]
    fn translation_pending_current_skips_to_history() {
        let mut timeline = Timeline::default();
        timeline.set_translation(Speaker::Local, true);
        let t0 = Instant::now();
        timeline.apply_asr(Speaker::Local, "源1", false, 0, None, None, t0);
        timeline.apply_translation(
            Speaker::Local,
            "译1",
            0,
            true,
            "en".to_string(),
            Some(1),
            t0 + Duration::from_secs(1),
        );
        // 第二句已成为当前句，但译文还没到；第一句的译文已经定稿进入历史。
        timeline.apply_asr(
            Speaker::Local,
            "源2",
            false,
            0,
            None,
            None,
            t0 + Duration::from_secs(2),
        );

        let now = t0 + Duration::from_secs(2);
        assert_eq!(
            compose_with(&timeline, VrchatDisplay::Bilingual, true, 30, now),
            ChatboxText::Text("源2\n译1\n源1".to_string()),
            "当前句只有原文时，历史句仍带着译文显示"
        );
        assert_eq!(
            compose_with(&timeline, VrchatDisplay::Translation, true, 30, now),
            ChatboxText::Text("译1".to_string()),
            "只显示译文时跳过不可渲染的当前句，回退到历史句"
        );
        assert_eq!(
            compose_with(&timeline, VrchatDisplay::Translation, false, 30, now),
            ChatboxText::Text("译1".to_string()),
            "关闭历史句时回退到最后一条历史句"
        );

        timeline.apply_translation(
            Speaker::Local,
            "译2",
            0,
            true,
            "en".to_string(),
            Some(2),
            t0 + Duration::from_secs(3),
        );
        assert_eq!(
            compose_with(
                &timeline,
                VrchatDisplay::Bilingual,
                true,
                30,
                t0 + Duration::from_secs(3)
            ),
            ChatboxText::Text("译2\n源2\n译1\n源1".to_string()),
            "迟到的译文定稿后自动反映到当前块"
        );
    }
}
