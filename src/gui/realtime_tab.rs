//! 实时页：不放分段的对话列表——最新一条在最上面，越早越靠下；
//! 列表顶部对齐、字号放大、页面不显示任何标题（D16/D39）。
//!
//! 每行布局：最左是语言代码，然后是本句文本；未完成的消息按「已送入 / 尚未送入」分成两种颜色
//! 且不显示复制按钮；已完成的消息在**悬停整行**时于右端显示复制图标。
//!
//! 消息可以只有一行（未翻译），也可以两行：原文小字弱色行在上、译文大字行在下（D26）；
//! `show_source = false` 时只渲染主行。每条消息带来源（我方/他人），P7 会改成左右分栏气泡。

use eframe::egui;

use crate::app::KaolsApp;

/// 译文字号（比正文大，方便远看）。
const SUBTITLE_FONT_SIZE: f32 = 24.0;
/// 原文小字号。
const SOURCE_FONT_SIZE: f32 = 15.0;
/// 语言代码字号。
const LANGUAGE_FONT_SIZE: f32 = 13.0;
/// 左侧语言列宽（当前句与历史句共用，保证文本左端对齐；够 3 字母代码如 `fil`）。
const LANGUAGE_COL_WIDTH: f32 = 32.0;
/// 右侧复制图标占位宽（只为历史居留出，避免图标压住文字）。
const COPY_SLOT_WIDTH: f32 = 24.0;
/// 复制图标点击区边长。
const COPY_BUTTON_SIZE: f32 = 20.0;
/// 「尚未送入」部分的文字颜色衰减系数。
const UNSENT_COLOR_SCALE: f32 = 0.45;
/// 历史原文行的弱化系数。
const SOURCE_DIM_SCALE: f32 = 0.7;

/// 一行字幕。
struct Row<'a> {
    /// 本行语言代码（空串 = 尚未判定，显示 `…`）。
    language: &'a str,
    text: &'a str,
    /// `Some(sent_bytes)` = 当前正在处理的行（分色显示进度，不可复制）。
    sent_bytes: Option<usize>,
    font_size: f32,
    /// 原文行用弱色。
    dim: bool,
    /// 历史行可复制（悬停时显示复制图标）。
    copy: bool,
}

impl<'a> Row<'a> {
    fn current(
        language: &'a str,
        text: &'a str,
        sent_bytes: usize,
        font_size: f32,
        dim: bool,
    ) -> Self {
        Self {
            language,
            text,
            sent_bytes: Some(sent_bytes),
            font_size,
            dim,
            copy: false,
        }
    }

    fn history(language: &'a str, text: &'a str, font_size: f32, dim: bool, copy: bool) -> Self {
        Self {
            language,
            text,
            sent_bytes: None,
            font_size,
            dim,
            copy,
        }
    }
}

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    // 底部输入行先占位（嵌套 Panel 从当前 Ui 底部切走输入行高度），剩余空间给字幕列表。
    super::typed_input::show(app, ui);
    show_subtitles(app, ui);
}

/// 字幕列表：从底部输入行上方剩余的空间向下填满。
fn show_subtitles(app: &mut KaolsApp, ui: &mut egui::Ui) {
    let show_source = app.translate_show_source();

    // 显示顺序：时间线本身就是「最新在前」，直接按顺序渲染。
    // 每条消息 1–2 行：原文小字行在上（翻译模式且 `show_source`），主行在下。
    let mut entries: Vec<Vec<Row<'_>>> = Vec::with_capacity(app.timeline.messages().len());
    for message in app.timeline.messages() {
        let mut rows = Vec::with_capacity(2);
        if show_source && !message.source.is_empty() {
            rows.push(if message.open {
                Row::current(
                    &message.source_language,
                    &message.source,
                    message.source_sent_bytes,
                    SOURCE_FONT_SIZE,
                    false,
                )
            } else {
                Row::history(
                    &message.source_language,
                    &message.source,
                    SOURCE_FONT_SIZE,
                    true,
                    true,
                )
            });
        }
        if !message.text.is_empty() {
            rows.push(if message.open {
                Row::current(
                    &message.language,
                    &message.text,
                    message.sent_bytes,
                    SUBTITLE_FONT_SIZE,
                    false,
                )
            } else {
                Row::history(
                    &message.language,
                    &message.text,
                    SUBTITLE_FONT_SIZE,
                    false,
                    true,
                )
            });
        }
        // 刚建立、还没收到任何文本的消息不占位。
        if !rows.is_empty() {
            entries.push(rows);
        }
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("subtitle_list_scroll")
        .show(ui, |ui| {
            // ScrollArea 的内容 Ui 会继承外层 bottom_up 布局，必须显式改为 top_down，
            // 否则空状态提示等会贴到字幕区底部。
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                // 0.37 的 ScrollArea 没有 stick_to_top；默认滚动偏移按像素保存，
                // 停在顶部（偏移 0）时新消息插入顶部不会把视图顶走，效果就是 stick top。
                if entries.is_empty() {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("（点工具栏「开始」后，说话或打字即可看到字幕）")
                            .weak(),
                    );
                    return;
                }

                let normal_color = ui.visuals().text_color();
                let language_color = normal_color.gamma_multiply(0.55);

                for (index, rows) in entries.iter().enumerate() {
                    if index > 0 {
                        ui.separator();
                    }
                    for (row_index, row) in rows.iter().enumerate() {
                        if row_index > 0 {
                            ui.add_space(2.0);
                        }
                        let inner = ui.horizontal_top(|ui| {
                            language_column(ui, row.language, language_color);
                            show_text_row(ui, row, normal_color);
                        });

                        // 历史行：只有鼠标悬停这一行时，才在右端显示复制图标。
                        if row.copy {
                            // 注意不能拿 inner.response.rect 当整行矩形：水平布局里
                            // allocate_ui 只按内容尺寸结算，短句时矩形会短于整行。
                            // 用外层 Ui 的 max_rect（整行宽）拼竖直范围才是整行。
                            let row_band = egui::Rect::from_x_y_ranges(
                                ui.max_rect().x_range(),
                                inner.response.rect.y_range(),
                            );
                            let hovered = ui
                                .ctx()
                                .pointer_hover_pos()
                                .is_some_and(|pos| row_band.contains(pos));
                            if hovered {
                                let center = egui::pos2(
                                    row_band.right() - COPY_SLOT_WIDTH * 0.55,
                                    row_band.center().y,
                                );
                                let icon_rect = egui::Rect::from_center_size(
                                    center,
                                    egui::vec2(COPY_BUTTON_SIZE, COPY_BUTTON_SIZE),
                                );
                                copy_icon_button(ui, icon_rect, row.text);
                            }
                        }

                        ui.add_space(if row_index + 1 == rows.len() {
                            4.0
                        } else {
                            0.0
                        });
                    }
                }
            });
        });
}

/// 画一行文本：当前句按已送入字节前缀分色，历史行整行单色。
fn show_text_row(ui: &mut egui::Ui, row: &Row<'_>, normal_color: egui::Color32) {
    let primary_color = if row.dim {
        normal_color.gamma_multiply(SOURCE_DIM_SCALE)
    } else {
        normal_color
    };
    let unsent_color = primary_color.gamma_multiply(UNSENT_COLOR_SCALE);
    let font_id = egui::FontId::proportional(row.font_size);

    match row.sent_bytes {
        // 当前句：已送前缀用正常色，其余用弱色。
        Some(sent_bytes) => {
            let sent = clamp_prefix(row.text, sent_bytes);
            let mut job = egui::text::LayoutJob::default();
            job.append(
                &row.text[..sent],
                0.0,
                egui::TextFormat {
                    font_id: font_id.clone(),
                    color: primary_color,
                    ..Default::default()
                },
            );
            job.append(
                &row.text[sent..],
                0.0,
                egui::TextFormat {
                    font_id,
                    color: unsent_color,
                    ..Default::default()
                },
            );
            // 必须显式 .wrap()：水平布局里 Label 的默认 wrap_mode 是 Extend，
            // 会把我们设好的 max_width 覆盖成 INFINITY（长句不换行、直接溢出）。
            job.wrap.max_width = ui.available_width();
            ui.add(egui::Label::new(job).wrap());
        }
        // 历史句：右侧为复制图标留出空位，文本按剩余宽度换行。
        None => {
            let text_width = if row.copy {
                (ui.available_width() - COPY_SLOT_WIDTH - ui.spacing().item_spacing.x).max(40.0)
            } else {
                ui.available_width()
            };
            ui.allocate_ui_with_layout(
                egui::vec2(text_width, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(row.text)
                                .size(row.font_size)
                                .color(primary_color),
                        )
                        .wrap(),
                    );
                },
            );
            if row.copy {
                // 把右侧空位真正占掉，行矩形才包含图标位置（悬停判定/图标不压字）
                ui.add_space(COPY_SLOT_WIDTH);
            }
        }
    }
}

/// 把「已送入前缀长度」夹到字符边界，避免切片 panic。
fn clamp_prefix(text: &str, sent: usize) -> usize {
    let mut sent = sent.min(text.len());
    while sent > 0 && !text.is_char_boundary(sent) {
        sent -= 1;
    }
    sent
}

/// 左侧语言代码列：固定宽度，保证当前句与历史句的文本左端对齐。
///
/// 注意：不能用 `allocate_ui_with_layout` + 标签实现——外层拿的是子 Ui 的 `min_rect`
/// （会缩到内容宽度），语言代码 "…" 比 "ja" 窄时文本就会左移几个像素。这里显式
/// 分配固定宽度并自己画文字，宽度才是稳定的。
fn language_column(ui: &mut egui::Ui, language: &str, color: egui::Color32) {
    let (rect, _response) = ui.allocate_exact_size(
        egui::vec2(LANGUAGE_COL_WIDTH, ui.spacing().interact_size.y),
        egui::Sense::hover(),
    );
    let code = if language.is_empty() { "…" } else { language };
    ui.painter().text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        code,
        egui::FontId::monospace(LANGUAGE_FONT_SIZE),
        color,
    );
}

/// 复制图标按钮（无文字）：悬停高亮，点击复制整行文本。
fn copy_icon_button(ui: &mut egui::Ui, rect: egui::Rect, text: &str) {
    // 用位置参与 id，避免两条文本内容相同的字幕共享交互状态
    let id = ui
        .scope_id()
        .with(("subtitle_copy", text, rect.min.x as i32, rect.min.y as i32));
    let response = ui.interact(rect, id, egui::Sense::click());

    let color = if response.hovered() {
        ui.visuals().text_color()
    } else {
        ui.visuals().text_color().gamma_multiply(0.55)
    };
    if response.hovered() {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(4),
            ui.visuals().widgets.hovered.bg_fill,
        );
    }
    paint_copy_icon(
        ui.painter(),
        rect.shrink(4.0),
        color,
        ui.visuals().panel_fill,
    );

    if response.clicked() {
        ui.ctx().copy_text(text.to_string());
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

/// 画一个「两张叠纸」的复制图标（不依赖字体是否带 emoji）。
fn paint_copy_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    color: egui::Color32,
    background: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.2, color);
    let corner = egui::CornerRadius::same(2);

    let back = egui::Rect::from_min_size(
        rect.min + egui::vec2(rect.width() * 0.25, rect.height() * 0.25),
        egui::vec2(rect.width() * 0.7, rect.height() * 0.7),
    );
    painter.rect_filled(back, corner, background);
    painter.rect_stroke(back, corner, stroke, egui::StrokeKind::Inside);

    let front = egui::Rect::from_min_size(
        rect.min,
        egui::vec2(rect.width() * 0.7, rect.height() * 0.7),
    );
    painter.rect_filled(front, corner, background);
    painter.rect_stroke(front, corner, stroke, egui::StrokeKind::Inside);
}
