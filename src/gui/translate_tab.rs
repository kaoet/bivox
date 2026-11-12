//! 翻译页：翻译模式开关、目标语言、粒度、是否显示原文（D27）。
//!
//! 本页只写 `config.toml` 中 `[translate]` 的 `enabled` / `target` / `granularity` /
//! `show_source`（D3 白名单），`model` 只读展示。改值即走 `config_edit` 写回：
//! - `show_source` 只影响渲染，不参与配置指纹，改后立即生效、不重启；
//! - 其余字段变化会触发 D20：停旧会话 → 用新配置自动重启，当前句会丢失。

use eframe::egui;

use crate::app::{ConfigState, KaolsApp};
use crate::config::TranslateGranularity;
use crate::gui::widgets;
use crate::language::{TRANSLATION_LANGUAGES, language_display_name};

/// 目标语言下拉的显示文本：中文名 + 代码，避免只显示代码时看不懂。
fn language_label(code: &str) -> String {
    format!("{}（{code}）", language_display_name(code))
}

/// 当前配置里的翻译设置（`None` = 配置无效）。
fn current(app: &KaolsApp) -> Option<crate::config::TranslateConfig> {
    match &app.config {
        ConfigState::Loaded(config) => Some(config.translate.clone()),
        ConfigState::Invalid { .. } => None,
    }
}

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.heading("翻译");
    ui.label(
        egui::RichText::new(
            "开启后，麦克风语音与打字输入都会先翻译成目标语言，再合成目标语言语音并以字幕显示；\
             字幕默认同时显示原文与译文。",
        )
        .weak(),
    );
    ui.add_space(8.0);

    let Some(configured) = current(app) else {
        ui.colored_label(
            egui::Color32::from_rgb(0xe0, 0x4b, 0x4b),
            "配置无效，无法保存翻译设置；请先到「设置」页修正配置。",
        );
        return;
    };

    let mut enabled = configured.enabled;
    let mut target = configured.target.clone();
    let mut granularity = configured.granularity;
    let mut show_source = configured.show_source;

    ui.checkbox(&mut enabled, "启用翻译模式");
    ui.add_space(6.0);

    // 未启用翻译时，目标语言与粒度没有意义，禁用（显示原文仍可改）。
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            ui.label("目标语言");
            egui::ComboBox::from_id_salt("translate_target")
                .selected_text(language_label(&target))
                .width(200.0)
                .show_ui(ui, |ui| {
                    for code in TRANSLATION_LANGUAGES {
                        ui.selectable_value(&mut target, (*code).to_string(), language_label(code));
                    }
                });
        });
        ui.add_space(4.0);
        ui.label("翻译粒度");
        ui.radio_value(
            &mut granularity,
            TranslateGranularity::Clause,
            "标点即翻（延迟低，句子可能偏碎）",
        );
        ui.radio_value(
            &mut granularity,
            TranslateGranularity::Sentence,
            "整句再翻（质量优先，延迟约一整句）",
        );
    });

    ui.add_space(6.0);
    ui.checkbox(&mut show_source, "字幕同时显示原文（弱色小字）");

    ui.add_space(10.0);
    widgets::kv_row(ui, "翻译模型", &configured.model);
    ui.label(
        egui::RichText::new("模型只读；如需更换，请手改 config.toml 后到「设置 → 重新加载配置」。")
            .weak(),
    );

    let restart_fields_changed = enabled != configured.enabled
        || target != configured.target
        || granularity != configured.granularity;

    if restart_fields_changed || show_source != configured.show_source {
        app.update_translate_settings(enabled, &target, granularity, show_source);
    }
}
