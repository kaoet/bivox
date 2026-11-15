//! 翻译页：两条线路**共用**的翻译设置（D27/D38）。
//!
//! 本页编辑 `[translate] model` / `granularity` / `show_source`；
//! 每条线路的翻译开关与目标语言在「自己」/「他人」页。
//! 改值即走类型化写回：`show_source` 只影响渲染、不参与配置指纹、改后立即生效；
//! 粒度变化会触发 D20（停旧会话 → 用新配置自动重启）。模型用「应用模型」按钮单独写回。

use eframe::egui;

use crate::app::{BivoxApp, Tab};
use crate::config::{DEFAULT_TRANSLATE_MODEL, TranslateGranularity};

const RED: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x4b, 0x4b);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xd8, 0x9a, 0x1b);

/// 本页编辑的当前值（都来自共用的 `[translate]`）。
struct Current {
    granularity: TranslateGranularity,
    show_source: bool,
}

fn current(app: &BivoxApp) -> Current {
    Current {
        granularity: app.config.translate.granularity,
        show_source: app.config.translate.show_source,
    }
}

pub fn show(app: &mut BivoxApp, ui: &mut egui::Ui) {
    ui.heading("翻译");
    ui.label(
        egui::RichText::new(
            "本页只放两条线路共用的翻译设置。要不要翻译、译成什么语言，在「自己」「他人」页设置。",
        )
        .weak(),
    );
    if let Some(error) = app.config_write_error_for(Tab::Translate) {
        ui.colored_label(RED, format!("保存失败：{error}"));
    }
    ui.add_space(8.0);

    let configured = current(app);

    let mut granularity = configured.granularity;
    let mut show_source = configured.show_source;

    ui.horizontal(|ui| {
        ui.label("翻译模型");
        ui.add(
            egui::TextEdit::singleline(&mut app.translate_model)
                .desired_width(380.0)
                .hint_text(DEFAULT_TRANSLATE_MODEL),
        );
        let can_apply = app.translate_model_dirty() && !app.translate_model.trim().is_empty();
        if ui
            .add_enabled(can_apply, egui::Button::new("应用模型"))
            .clicked()
        {
            app.save_translate_model();
        }
        if ui.button("恢复默认").clicked() {
            app.translate_model = DEFAULT_TRANSLATE_MODEL.to_string();
            app.save_translate_model();
        }
        if app.translate_model_dirty() {
            ui.label(egui::RichText::new("● 未保存").color(AMBER).small());
        }
    });
    ui.label(
        egui::RichText::new("模型需与百炼兼容接口一致；运行中应用会自动重启实时管线。").weak(),
    );
    ui.add_space(6.0);

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

    ui.add_space(6.0);
    ui.checkbox(&mut show_source, "字幕同时显示原文（弱色小字）");

    if granularity != configured.granularity || show_source != configured.show_source {
        app.update_translate_settings(granularity, show_source);
    }
}
