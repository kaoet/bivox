//! 设置页：便携路径、API Key 写回、配置概览。
//!
//! 写回规则（GUI_MIGRATION.md D3）：本页只写 `[api] key`；音频设备在「音频」页设置。
//! 用 [`crate::config_edit`] 定点修改、写前校验、写前备份；其余字段只读展示。

use eframe::egui;

use crate::app::{ConfigState, KaolsApp};
use crate::config::AppConfig;
use crate::gui::widgets;

const RED: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x4b, 0x4b);

/// 从配置里提取的设置页只读信息（避免界面渲染时长期借用 `app.config`）。
struct ConfigSummary {
    workspace_host: String,
    asr_model: String,
    languages: String,
    tts_model: String,
    tts_voice: String,
    custom_voices: usize,
}

impl ConfigSummary {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            workspace_host: config.api.workspace_host.clone(),
            asr_model: config.asr.model.clone(),
            languages: config.asr.languages.join(", "),
            tts_model: config.tts.model.clone(),
            tts_voice: config.tts.voice.clone(),
            custom_voices: config.tts.custom_voices.len(),
        }
    }
}

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("settings_scroll")
        .show(ui, |ui| {
            ui.heading("设置");
            ui.label(
                egui::RichText::new(
                    "只支持便携模式：配置、音色缓存、日志都放在程序（exe）所在目录，\
                     不读取 %APPDATA%、不读取当前工作目录。",
                )
                .weak(),
            );
            ui.add_space(8.0);

            widgets::path_row(ui, "程序目录", &app.paths.exe_dir);
            widgets::path_row(ui, "配置文件", &app.paths.config_file);
            widgets::path_row(ui, "日志目录", &app.paths.log_dir);
            widgets::path_row(ui, "崩溃日志", &app.paths.crash_log);

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("重新加载配置").clicked() {
                    app.reload_config();
                }
                if ui.button("打开程序目录").clicked() {
                    app.open_exe_dir();
                }
            });

            ui.add_space(6.0);
            if let Some(error) = &app.config_write_error {
                ui.colored_label(RED, format!("写回配置失败：{error}"));
                ui.add_space(4.0);
            }
            show_api_key_section(app, ui);

            let summary = match &app.config {
                ConfigState::Loaded(config) => Some(ConfigSummary::from_config(config)),
                ConfigState::Invalid { .. } => None,
            };

            match summary {
                Some(summary) => {
                    ui.separator();
                    show_config_summary(ui, &summary);
                }
                None => {
                    ui.add_space(6.0);
                    ui.colored_label(RED, "配置无效，实时功能不可用；请先修正上面的 API Key。");
                    if let ConfigState::Invalid { error } = &app.config {
                        ui.monospace(error.as_str());
                    }
                }
            }
        });
}

/// API Key：输入框 + 保存按钮（写回 config.toml，明文保存并提示风险）。
fn show_api_key_section(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("API Key").strong());
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut app.api_key_input)
                .password(true)
                .desired_width(420.0)
                .hint_text("sk-..."),
        );
        if ui.button("保存 API Key").clicked() {
            app.save_api_key();
        }
    });
    ui.label(
        egui::RichText::new(
            "Key 以明文保存在 exe 同目录的 config.toml；整个程序目录含密钥，请勿分享或上传。",
        )
        .weak(),
    );
}

/// 只读配置概览。
fn show_config_summary(ui: &mut egui::Ui, summary: &ConfigSummary) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new("当前配置").strong());
    widgets::kv_row(ui, "接入域名", &summary.workspace_host);
    widgets::kv_row(ui, "识别模型", &summary.asr_model);
    widgets::kv_row(ui, "识别语言", &summary.languages);
    widgets::kv_row(ui, "合成模型", &summary.tts_model);
    widgets::kv_row(ui, "当前音色", &summary.tts_voice);
    widgets::kv_row(ui, "自定义音色", &format!("{} 个", summary.custom_voices));
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("音频设备在「音频」页设置；自定义音色在「语音合成」页查看。").weak(),
    );
}
