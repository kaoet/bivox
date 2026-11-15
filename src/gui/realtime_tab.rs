//! 实时页：气泡时间线（D16/D39）。最新一条在最上面，越早越靠下。
//!
//! 布局：我方靠右、他人靠左；气泡宽度贴合内容、上限为可用宽度的一部分，长句在气泡内换行。
//! 气泡内容自上而下是「原文小字行（翻译模式且显示原文）」→「主行大字」；语言代码画在气泡外侧、朝对话中线的一边。
//! 未完成的消息按「已送入 / 尚未送入」分色；所有文本（原文与译文）都可以拖动选中、Ctrl+C 复制，
//! 不提供复制按钮。

use eframe::egui;

use crate::app::KaolsApp;
use crate::events::Speaker;
use crate::timeline::Message;

/// 主行字号（比正文大，方便远看）。
const MAIN_FONT_SIZE: f32 = 20.0;
/// 原文小字号。
const SOURCE_FONT_SIZE: f32 = 13.0;
/// 语言代码字号。
const LANGUAGE_FONT_SIZE: f32 = 11.0;
/// 语言标签与气泡之间的间距（标签在气泡外侧）。
const LANGUAGE_LABEL_GAP: f32 = 6.0;
/// 原文行与主行之间的间距。
const SOURCE_GAP: f32 = 3.0;
/// 气泡最大宽度占总宽度的比例。
const BUBBLE_WIDTH_RATIO: f32 = 0.72;
/// 气泡最小内容宽度。
const BUBBLE_MIN_WIDTH: f32 = 120.0;
/// 气泡内边距。
const BUBBLE_PADDING_X: f32 = 11.0;
const BUBBLE_PADDING_Y: f32 = 8.0;
/// 气泡圆角。
const BUBBLE_CORNER_RADIUS: u8 = 10;
/// 两条消息之间的间距。
const BUBBLE_GAP: f32 = 6.0;
/// 「尚未送入」部分的文字颜色衰减系数。
const UNSENT_COLOR_SCALE: f32 = 0.45;
/// 原文行的弱化系数。
const SOURCE_DIM_SCALE: f32 = 0.72;

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    // 底部输入行先占位（嵌套 Panel 从当前 Ui 底部切走输入行高度），剩余空间给气泡列表。
    super::typed_input::show(app, ui);
    show_timeline(app, ui);
}

/// 气泡列表：从底部输入行上方剩余的空间向下填满。
fn show_timeline(app: &mut KaolsApp, ui: &mut egui::Ui) {
    let show_source = app.translate_show_source();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("subtitle_list_scroll")
        .show(ui, |ui| {
            // ScrollArea 的内容 Ui 会继承外层 bottom_up 布局，必须显式改为 top_down，
            // 否则空状态提示等会贴到字幕区底部。
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                if app.timeline.messages().is_empty() {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("（点工具栏「开始」后，说话或打字即可看到字幕）")
                            .weak(),
                    );
                    return;
                }

                // 时间线本身就是「最新在前」，直接按顺序渲染。
                for (index, message) in app.timeline.messages().iter().enumerate() {
                    if index > 0 {
                        ui.add_space(BUBBLE_GAP);
                    }
                    message_bubble(ui, message, show_source, index);
                }
            });
        });
}

/// 一条消息气泡：按来源决定左右，悬停显示复制图标。
fn message_bubble(ui: &mut egui::Ui, message: &Message, show_source: bool, index: usize) {
    // 刚建立、还没收到任何文本的消息不占位。
    if message.text.is_empty() && message.source.is_empty() {
        return;
    }

    let is_self = message.speaker == Speaker::Local;
    let max_width = (ui.available_width() * BUBBLE_WIDTH_RATIO).max(BUBBLE_MIN_WIDTH);
    let wrap_width = (max_width - 2.0 * BUBBLE_PADDING_X).max(64.0);

    // 半透明底：我方偏强调色，他人偏中性；未完成时整体更暗，暗示仍在变化。
    let base_fill = if is_self {
        ui.visuals().selection.bg_fill.gamma_multiply(0.55)
    } else {
        ui.visuals().widgets.inactive.bg_fill
    };
    let fill = if message.open {
        base_fill.gamma_multiply(0.75)
    } else {
        base_fill
    };

    let normal = ui.visuals().text_color();
    let source_color = normal.gamma_multiply(SOURCE_DIM_SCALE);

    // 先排版好两行文本、量出气泡尺寸；文本行随后用 `Label` 放置，才能拖动选中。
    let source_job = (show_source && !message.source.is_empty()).then(|| {
        row_job(
            &message.source,
            message.open,
            message.source_sent_bytes,
            SOURCE_FONT_SIZE,
            source_color,
            wrap_width,
        )
    });
    let main_job = (!message.text.is_empty()).then(|| {
        row_job(
            &message.text,
            message.open,
            message.sent_bytes,
            MAIN_FONT_SIZE,
            normal,
            wrap_width,
        )
    });
    let source_size = source_job
        .as_ref()
        .map(|job| ui.painter().layout_job(job.clone()).size());
    let main_size = main_job
        .as_ref()
        .map(|job| ui.painter().layout_job(job.clone()).size());

    let source_h = source_size.map_or(0.0, |size| size.y);
    let main_h = main_size.map_or(0.0, |size| size.y);
    let gap = if source_h > 0.0 && main_h > 0.0 {
        SOURCE_GAP
    } else {
        0.0
    };
    let text_width = source_size
        .map_or(0.0, |size| size.x)
        .max(main_size.map_or(0.0, |size| size.x))
        .max(BUBBLE_MIN_WIDTH - 2.0 * BUBBLE_PADDING_X);

    let content_size = egui::vec2(
        text_width + 2.0 * BUBBLE_PADDING_X,
        source_h + gap + main_h + 2.0 * BUBBLE_PADDING_Y,
    );

    // 我方靠右、他人靠左：外层布局负责，气泡内容始终左对齐。
    let align = if is_self {
        egui::Layout::right_to_left(egui::Align::TOP)
    } else {
        egui::Layout::left_to_right(egui::Align::TOP)
    };

    ui.with_layout(align, |ui| {
        let (rect, response) = ui.allocate_exact_size(content_size, egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(BUBBLE_CORNER_RADIUS), fill);

        // 文字统一放在气泡内边距里，宽度固定成量好的内容宽度：
        // `Label` 支持拖动选中与 Ctrl+C，`Sense::hover` 的气泡本身不抢拖拽。
        let text_rect = egui::Rect::from_min_size(
            rect.min + egui::vec2(BUBBLE_PADDING_X, BUBBLE_PADDING_Y),
            egui::vec2(text_width, source_h + gap + main_h),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(text_rect)
                .layout(egui::Layout::top_down(egui::Align::Min))
                .id_salt(("bubble_text", index)),
            |ui| {
                ui.set_width(text_width);
                if let Some(job) = source_job {
                    ui.add(egui::Label::new(job).selectable(true));
                    ui.add_space(gap);
                }
                if let Some(job) = main_job {
                    ui.add(egui::Label::new(job).selectable(true));
                }
            },
        );

        // 语言代码画在气泡外侧、朝着对话中线的那一边：他人气泡在右、我方气泡在左。
        // 这样气泡内不用单独留一行给标签（下面 `label_language` 里是取值规则）。
        let label_pos = if is_self {
            egui::pos2(rect.left() - LANGUAGE_LABEL_GAP, rect.center().y)
        } else {
            egui::pos2(rect.right() + LANGUAGE_LABEL_GAP, rect.center().y)
        };
        ui.painter().text(
            label_pos,
            if is_self {
                egui::Align2::RIGHT_CENTER
            } else {
                egui::Align2::LEFT_CENTER
            },
            label_language(message),
            egui::FontId::monospace(LANGUAGE_FONT_SIZE),
            normal.gamma_multiply(0.5),
        );

        // 未完成的气泡只给一句悬停提示（不抢拖拽，选中的文本仍可复制）。
        if message.open {
            response.on_hover_text("正在识别 / 翻译…");
        }
    });
}

/// 生成一行文本的排版任务：未完成时按「已送入 / 尚未送入」分色，已完成时单色。
///
/// 同一份 job 既用来量气泡尺寸，也直接交给 `Label` 渲染（可拖动选中、Ctrl+C 复制）。
fn row_job(
    text: &str,
    open: bool,
    sent_bytes: usize,
    font_size: f32,
    color: egui::Color32,
    wrap_width: f32,
) -> egui::text::LayoutJob {
    let font_id = egui::FontId::proportional(font_size);
    let mut job = egui::text::LayoutJob::default();
    let sent = if open {
        clamp_prefix(text, sent_bytes)
    } else {
        text.len()
    };

    job.append(
        &text[..sent],
        0.0,
        egui::TextFormat {
            font_id: font_id.clone(),
            color,
            ..Default::default()
        },
    );
    if sent < text.len() {
        job.append(
            &text[sent..],
            0.0,
            egui::TextFormat {
                font_id,
                color: color.gamma_multiply(UNSENT_COLOR_SCALE),
                ..Default::default()
            },
        );
    }
    job.wrap.max_width = wrap_width;
    job
}

/// 气泡外侧的语言代码该取哪个字段。
///
/// 翻译模式下消息的 `language` 是译文语言、`source_language` 是原文语言；用户要看的是
/// 「对方/自己说的是什么语言」，所以优先取原文语言。未翻译的消息 `source_language`
/// 为空，回落到识别语言（两者此时相同语义）。
fn label_language(message: &Message) -> &str {
    let language = if message.source_language.is_empty() {
        &message.language
    } else {
        &message.source_language
    };
    if language.is_empty() { "…" } else { language }
}

/// 把「已送入前缀长度」夹到字符边界，避免切片 panic。
fn clamp_prefix(text: &str, sent: usize) -> usize {
    let mut sent = sent.min(text.len());
    while sent > 0 && !text.is_char_boundary(sent) {
        sent -= 1;
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::Message;

    fn message(language: &str, source_language: &str) -> Message {
        Message {
            speaker: Speaker::Local,
            open: false,
            text: "hello".to_string(),
            language: language.to_string(),
            sent_bytes: 5,
            source: "こんにちは".to_string(),
            source_language: source_language.to_string(),
            source_sent_bytes: 15,
            sentence_id: Some(1),
        }
    }

    #[test]
    fn language_label_shows_source_language_in_translation_mode() {
        // 翻译模式：主行是译文（ja），角标要显示原文语言（zh）。
        assert_eq!(label_language(&message("ja", "zh")), "zh");
    }

    #[test]
    fn language_label_falls_back_to_text_language() {
        // 未翻译：source_language 为空，用主行语言。
        assert_eq!(label_language(&message("zh", "")), "zh");
        // 两者都未知：显示占位符。
        assert_eq!(label_language(&message("", "")), "…");
    }
}
