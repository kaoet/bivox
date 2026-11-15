//! 语音合成页顶部「当前音色」选择：现有音色（输入框）/ 自定义音色（下拉）。
//!
//! 只读写 [`BivoxApp`] 里的草稿字段（`voice_mode` / `voice_input` / `voice_custom_key`），
//! 保存与放弃由 [`super::tts_tab`] 统一处理（决策 D22，见 docs/decisions.md）。

use eframe::egui;

use crate::app::{BivoxApp, VoiceMode};

/// 顶部「当前音色」：Radio 二选一（未选中的控件禁用）。
pub fn show(app: &mut BivoxApp, ui: &mut egui::Ui) {
    let mut mode = app.voice_mode;
    let mut existing = app.voice_input.clone();
    let mut custom_key = app.voice_custom_key.clone();
    let options: Vec<(String, String)> = app
        .voice_drafts
        .iter()
        .map(|draft| {
            let name = if draft.display_name.trim().is_empty() {
                draft.key.clone()
            } else {
                format!("{}（{}）", draft.display_name, draft.key)
            };
            (draft.key.clone(), name)
        })
        .collect();

    ui.horizontal(|ui| {
        ui.radio_value(&mut mode, VoiceMode::Existing, "现有音色");
        ui.add_enabled(
            mode == VoiceMode::Existing,
            egui::TextEdit::singleline(&mut existing)
                .desired_width(340.0)
                .hint_text("内置音色或任意 voice_id，如 qiaoxiaojiao_v3.1"),
        );
    });
    ui.horizontal(|ui| {
        ui.radio_value(&mut mode, VoiceMode::Custom, "自定义音色");
        ui.add_enabled_ui(mode == VoiceMode::Custom, |ui| {
            let selected_label = options
                .iter()
                .find(|(key, _)| key == &custom_key)
                .map(|(_, label)| label.clone())
                .unwrap_or_else(|| {
                    if custom_key.is_empty() {
                        "（没有自定义音色）".to_string()
                    } else {
                        custom_key.clone()
                    }
                });
            egui::ComboBox::from_id_salt("current_voice_custom")
                .selected_text(selected_label)
                .width(320.0)
                .show_ui(ui, |ui| {
                    if options.is_empty() {
                        ui.label(egui::RichText::new("还没有自定义音色").weak());
                    }
                    for (key, label) in &options {
                        ui.selectable_value(&mut custom_key, key.clone(), label);
                    }
                });
        });
    });

    app.voice_mode = mode;
    app.voice_input = existing;
    app.voice_custom_key = custom_key;
}
