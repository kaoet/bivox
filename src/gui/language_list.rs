//! 有序语言列表编辑器：「自己」「他人」两页共用同一套交互（D38）。
//!
//! 首个语言是默认语言；拖动行首「≡」手柄排序、行尾「删除」、末尾下拉追加。
//! 控件只就地修改传入的 `Vec<String>`，不读写配置——保存由各页签的「保存」负责。

use eframe::egui;

use crate::language::{ASR_LANGUAGES, language_display_name};

const ORANGE: egui::Color32 = egui::Color32::from_rgb(0xe6, 0x82, 0x00);

/// 语言下拉/列表里的显示文本：中文名 + 代码；未知代码由 `language_display_name` 原样返回。
pub fn language_label(code: &str) -> String {
    format!("{}（{code}）", language_display_name(code))
}

/// 绘制语言列表编辑区（就地修改 `languages`）。
///
/// `id_salt` 区分同一窗口里的多个编辑器（拖拽状态与「添加语言」下拉的 id）。
pub fn show(ui: &mut egui::Ui, id_salt: &str, languages: &mut Vec<String>) {
    ui.label(egui::RichText::new("语言列表（有序，首个为默认）").strong());
    let count = languages.len();

    // 拖拽排序：手柄按下时把下标存进 egui 临时数据，松手时按指针位置插入；
    // 拖动过程中在目标插入位置画一条指示线。
    let drag_id = ui.make_persistent_id((id_salt, "drag"));
    let mut drag_from = ui.data(|data| data.get_temp::<usize>(drag_id));
    let mut released = false;
    let mut block_rects: Vec<egui::Rect> = Vec::with_capacity(count);
    let mut remove: Option<usize> = None;

    for (index, code) in languages.iter().enumerate() {
        let mut block_rect: Option<egui::Rect> = None;
        ui.horizontal(|ui| {
            let row_left = ui.cursor().left();
            // 先给手柄列留出宽度，内容排版后再把手柄放到内容块垂直中心
            ui.add_space(18.0);
            let right = ui.horizontal(|ui| {
                ui.label(language_label(code));
                if index == 0 {
                    ui.label(egui::RichText::new("默认语言").weak());
                }
                // 只剩一种语言时禁止删除，避免空列表
                if count > 1 && ui.button("删除").clicked() {
                    remove = Some(index);
                }
            });
            block_rect = Some(right.response.rect);

            let handle_rect = egui::Rect::from_center_size(
                egui::pos2(row_left + 8.0, right.response.rect.center().y),
                egui::vec2(16.0, 20.0),
            );
            let handle = ui
                .put(
                    handle_rect,
                    egui::Label::new(egui::RichText::new("≡").weak()).sense(egui::Sense::drag()),
                )
                .on_hover_cursor(egui::CursorIcon::Grab)
                .on_hover_text("拖动调整语言顺序");
            if handle.drag_started() {
                drag_from = Some(index);
                ui.data_mut(|data| data.insert_temp(drag_id, index));
            }
            if handle.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            if handle.drag_stopped() {
                released = true;
                ui.data_mut(|data| data.remove_temp::<usize>(drag_id));
            }
        });
        if let Some(rect) = block_rect {
            block_rects.push(rect);
        }
    }

    if let Some(from) = drag_from {
        if let Some(pointer) = ui.ctx().pointer_latest_pos() {
            let insert_at = block_rects
                .iter()
                .position(|rect| pointer.y < rect.center().y)
                .unwrap_or(block_rects.len());
            let stroke = egui::Stroke::new(2.0, ui.visuals().selection.stroke.color);
            let y = block_rects
                .get(insert_at)
                .map(|rect| rect.top())
                .or_else(|| block_rects.last().map(|rect| rect.bottom()));
            if let Some(y) = y {
                let x = block_rects
                    .first()
                    .map(|rect| egui::Rangef::new(rect.left(), ui.max_rect().right()))
                    .unwrap_or(egui::Rangef::new(0.0, ui.max_rect().right()));
                ui.painter().hline(x, y, stroke);
            }
            if released {
                let len = languages.len();
                let mut to = insert_at;
                if from < to {
                    to -= 1;
                }
                let to = to.min(len.saturating_sub(1));
                if from < len && from != to {
                    let language = languages.remove(from);
                    languages.insert(to, language);
                }
                ui.data_mut(|data| data.remove_temp::<usize>(drag_id));
            }
        }
    }
    if let Some(index) = remove {
        if languages.len() > 1 && index < languages.len() {
            languages.remove(index);
        }
    }

    // 只列出尚未加入的语言；选中即追加到列表末尾
    let mut added: Option<String> = None;
    ui.horizontal(|ui| {
        ui.label("添加语言");
        egui::ComboBox::from_id_salt((id_salt, "add"))
            .selected_text("选择要添加的语言…")
            .width(200.0)
            .show_ui(ui, |ui| {
                let mut any = false;
                for code in ASR_LANGUAGES.iter().copied() {
                    if languages.iter().any(|lang| lang.as_str() == code) {
                        continue;
                    }
                    any = true;
                    if ui.selectable_label(false, language_label(code)).clicked() {
                        added = Some(code.to_string());
                    }
                }
                if !any {
                    ui.label(egui::RichText::new("已加入全部支持的语言").weak());
                }
            });
    });
    if let Some(code) = added {
        languages.push(code);
    }

    if languages.len() > 4 {
        ui.colored_label(
            ORANGE,
            format!(
                "⚠ 当前 {} 种语言：ASR 接口只使用前 4 个作为 language_hints，\
                 本地语言判定仍使用完整列表。",
                languages.len()
            ),
        );
    }
}
