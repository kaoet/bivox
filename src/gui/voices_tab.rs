//! 音色页：M0 只读展示 `[tts.custom_voices]`；编辑能力在 M3 实现。
//!
//! 注意（D11）：本页只读写 `config.toml`，不访问百炼接口、不读写 `voice_cache.json`。

use eframe::egui;

use crate::app::{ConfigState, KaolsApp};

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.heading("自定义音色");
    ui.label(
        "本页只管理 config.toml 里的 [tts.custom_voices]（增删改在 M3 实现）；\
         百炼账号里的远程音色由独立调试工具管理。",
    );
    ui.add_space(8.0);

    match &app.config {
        ConfigState::Invalid { error } => {
            ui.colored_label(
                egui::Color32::from_rgb(0xe0, 0x4b, 0x4b),
                format!("配置无效，无法读取音色：{error}"),
            );
        }
        ConfigState::Loaded(config) => {
            if config.tts.custom_voices.is_empty() {
                ui.label(egui::RichText::new("配置中还没有自定义音色。").weak());
            } else {
                for (name, voice) in &config.tts.custom_voices {
                    ui.group(|ui| {
                        ui.label(egui::RichText::new(voice.display_name(name)).strong());
                        ui.label(egui::RichText::new(format!("键名：{name}")).weak());
                        for sample in &voice.samples {
                            ui.monospace(format!(
                                "{} → {}",
                                sample.language,
                                sample.audio.display()
                            ));
                        }
                    });
                }
            }
        }
    }
}
