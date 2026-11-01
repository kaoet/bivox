//! 实时页：不分段的字幕列表——最新的一句（正在合成的）在最上面，越早越靠下；
//! 列表顶部对齐、字号放大、页面不显示任何标题（D16）。
//!
//! 每行布局：最左是语言代码，然后是本句文本；当前句按「已送入 TTS / 尚未送入」
//! 分成两种颜色且不显示复制按钮；历史句在**悬停整行**时于右端显示复制图标。

use eframe::egui;

use crate::app::KaolsApp;

/// 字幕字号（比正文大，方便远看）。
const SUBTITLE_FONT_SIZE: f32 = 24.0;
/// 语言代码字号。
const LANGUAGE_FONT_SIZE: f32 = 13.0;
/// 左侧语言列宽（当前句与历史句共用，保证文本左端对齐）。
const LANGUAGE_COL_WIDTH: f32 = 24.0;
/// 右侧复制图标占位宽（只为历史居留出，避免图标压住文字）。
const COPY_SLOT_WIDTH: f32 = 24.0;
/// 复制图标点击区边长。
const COPY_BUTTON_SIZE: f32 = 20.0;
/// 「尚未送入 TTS」部分的文字颜色衰减系数。
const UNSENT_COLOR_SCALE: f32 = 0.45;

/// 一行字幕。
struct Row<'a> {
    /// 本句语言代码（当前句在首个 chunk 前可能未知）。
    language: Option<&'a str>,
    text: &'a str,
    /// `Some(sent_bytes)` = 当前正在合成的句子（已送 TTS 的字节前缀）。
    sent_bytes: Option<usize>,
}

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    // 显示顺序：当前句（ASR 持续修订中）在最上，历史句按 新 → 旧 往下排。
    let mut rows: Vec<Row<'_>> = Vec::with_capacity(app.subtitles.history.len() + 1);
    if !app.subtitles.current.is_empty() {
        rows.push(Row {
            language: app.subtitles.language.as_deref(),
            text: app.subtitles.current.as_str(),
            sent_bytes: Some(app.subtitles.sent_bytes),
        });
    }
    rows.extend(app.subtitles.history.iter().rev().map(|line| Row {
        language: Some(line.language.as_str()),
        text: line.text.as_str(),
        sent_bytes: None,
    }));

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("subtitle_list_scroll")
        .show(ui, |ui| {
            // 0.37 的 ScrollArea 没有 stick_to_top；默认滚动偏移按像素保存，
            // 停在顶部（偏移 0）时新字幕插入顶部不会把视图顶走，效果就是 stick top。
            if rows.is_empty() {
                ui.add_space(16.0);
                ui.label(
                    egui::RichText::new("（点工具栏「开始」后，说话即可看到识别字幕）").weak(),
                );
                return;
            }

            let font_id = egui::FontId::proportional(SUBTITLE_FONT_SIZE);
            let normal_color = ui.visuals().text_color();
            let unsent_color = normal_color.gamma_multiply(UNSENT_COLOR_SCALE);
            let language_color = normal_color.gamma_multiply(0.55);

            for (index, row) in rows.iter().enumerate() {
                if index > 0 {
                    ui.separator();
                }

                let inner = ui.horizontal_top(|ui| {
                    language_column(ui, row.language, language_color);

                    match row.sent_bytes {
                        // 当前句：不显示复制按钮；已送 TTS 的前缀用正常色，其余用弱色。
                        Some(sent_bytes) => {
                            let mut sent = sent_bytes.min(row.text.len());
                            while sent > 0 && !row.text.is_char_boundary(sent) {
                                sent -= 1;
                            }

                            let mut job = egui::text::LayoutJob::default();
                            job.append(
                                &row.text[..sent],
                                0.0,
                                egui::TextFormat {
                                    font_id: font_id.clone(),
                                    color: normal_color,
                                    ..Default::default()
                                },
                            );
                            job.append(
                                &row.text[sent..],
                                0.0,
                                egui::TextFormat {
                                    font_id: font_id.clone(),
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
                            let text_width = (ui.available_width()
                                - COPY_SLOT_WIDTH
                                - ui.spacing().item_spacing.x)
                                .max(40.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(text_width, 0.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(row.text).size(SUBTITLE_FONT_SIZE),
                                        )
                                        .wrap(),
                                    );
                                },
                            );
                            // 把右侧空位真正占掉，行矩形才包含图标位置（悬停判定/图标不压字）
                            ui.add_space(COPY_SLOT_WIDTH);
                        }
                    }
                });

                // 历史句：只有鼠标悬停这一行时，才在右端显示复制图标。
                if row.sent_bytes.is_none() {
                    let row_rect = inner.response.rect;
                    // 用指针位置判断整行悬停（不注册额外交互层，避免和文本/hit-test 打架）
                    let hovered = ui
                        .ctx()
                        .pointer_hover_pos()
                        .is_some_and(|pos| row_rect.contains(pos));
                    if hovered {
                        let center = egui::pos2(
                            row_rect.right() - COPY_SLOT_WIDTH * 0.55,
                            row_rect.center().y,
                        );
                        let icon_rect = egui::Rect::from_center_size(
                            center,
                            egui::vec2(COPY_BUTTON_SIZE, COPY_BUTTON_SIZE),
                        );
                        copy_icon_button(ui, icon_rect, row.text);
                    }
                }

                ui.add_space(4.0);
            }
        });
}

/// 左侧语言代码列：固定宽度，保证当前句与历史句的文本左端对齐。
///
/// 注意：不能用 `allocate_ui_with_layout` + 标签实现——外层拿的是子 Ui 的 `min_rect`
/// （会缩到内容宽度），语言代码 "…" 比 "ja" 窄时文本就会左移几个像素。这里显式
/// 分配固定宽度并自己画文字，宽度才是稳定的。
fn language_column(ui: &mut egui::Ui, language: Option<&str>, color: egui::Color32) {
    let (rect, _response) = ui.allocate_exact_size(
        egui::vec2(LANGUAGE_COL_WIDTH, ui.spacing().interact_size.y),
        egui::Sense::hover(),
    );
    let code = language.unwrap_or("…");
    ui.painter().text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        code,
        egui::FontId::monospace(LANGUAGE_FONT_SIZE),
        color,
    );
}

/// 复制图标按钮（无文字）：悬停高亮，点击复制整句。
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
