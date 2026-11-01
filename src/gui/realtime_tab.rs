//! 实时页：M2 起在这里显示 ASR 字幕与 TTS 分块日志。

use eframe::egui;

use crate::app::KaolsApp;

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.heading("实时识别与合成");
    ui.add_space(6.0);
    ui.label("M2 阶段接入：ASR 字幕、语言判定、TTS 分块与播放状态。");
    ui.label(format!("当前状态：{}", app.state.label()));
    ui.add_space(12.0);

    ui.group(|ui| {
        ui.label(egui::RichText::new("语音识别结果").strong());
        ui.add_space(4.0);
        ui.label(egui::RichText::new("（等待接入管线）").weak());
    });
}
