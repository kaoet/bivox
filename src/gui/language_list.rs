//! 有序语言列表编辑器：「自己」「他人」两页共用同一套交互（D38）。
//!
//! 首个语言是默认语言；拖动行首「≡」手柄排序、行尾「删除」、末尾下拉追加。
//! 控件只就地修改传入的 `Vec<String>`，不读写配置——保存由各页签的「保存」负责。

use eframe::egui;

use crate::gui::{drag_reorder::DragReorder, theme};
use crate::language::{ASR_LANGUAGES, language_display_name};

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
    let mut remove: Option<usize> = None;
    let mut drag = DragReorder::new(ui, (id_salt, "lang"));

    for (index, code) in languages.iter().enumerate() {
        drag.row(ui, index, "拖动调整语言顺序", |ui| {
            ui.horizontal(|ui| {
                ui.label(language_label(code));
                if index == 0 {
                    ui.label(egui::RichText::new("默认语言").weak());
                }
                // 只剩一种语言时禁止删除，避免空列表
                if count > 1 && ui.button("删除").clicked() {
                    remove = Some(index);
                }
            })
            .response
        });
    }

    if let Some((from, to)) = drag.finish(ui) {
        let language = languages.remove(from);
        languages.insert(to, language);
    }
    if let Some(index) = remove
        && languages.len() > 1
        && index < languages.len()
    {
        languages.remove(index);
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
            theme::ORANGE,
            format!(
                "⚠ 当前 {} 种语言：服务端可能只采信前 4 种作为识别语言提示，\
                 建议把最常用的语言排在前面；本地语言判定仍使用完整列表。",
                languages.len()
            ),
        );
    }
}
