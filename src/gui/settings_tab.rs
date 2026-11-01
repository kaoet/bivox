//! 设置页：展示便携路径与配置状态，提供「重新加载配置」「打开程序目录」。
//!
//! 写回能力（API Key / 设备 / 音色）在 M1、M3 实现（D3）。

use eframe::egui;

use crate::app::{ConfigState, KaolsApp};
use crate::gui::widgets;

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.heading("设置");
            ui.label(
                "只支持便携模式：配置、音色缓存、日志都放在程序（exe）所在目录，\
                 不读取 %APPDATA%、不读取当前工作目录。",
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
            ui.add_space(8.0);
            ui.separator();

            match &app.config {
                ConfigState::Invalid { error } => {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xe0, 0x4b, 0x4b),
                        "配置无效，实时功能不可用：",
                    );
                    ui.monospace(error.as_str());
                }
                ConfigState::Loaded(config) => {
                    if app.config_created {
                        ui.colored_label(
                            egui::Color32::from_rgb(0x3c, 0xc9, 0x6f),
                            "已在程序目录生成默认 config.toml，请填写 [api] key 后点「重新加载配置」。",
                        );
                        ui.add_space(6.0);
                    }

                    let key_state = if config.api.key.is_some() {
                        "已配置"
                    } else {
                        "未配置"
                    };
                    widgets::kv_row(ui, "API Key", key_state);
                    widgets::kv_row(ui, "接入域名", config.api.workspace_host.as_str());
                    widgets::kv_row(ui, "识别模型", config.asr.model.as_str());
                    widgets::kv_row(ui, "识别语言", &config.asr.languages.join(", "));
                    widgets::kv_row(ui, "合成模型", config.tts.model.as_str());
                    widgets::kv_row(ui, "当前音色", config.tts.voice.as_str());
                    widgets::kv_row(ui, "自定义音色", &format!("{} 个", config.tts.custom_voices.len()));
                }
            }
        });
}
