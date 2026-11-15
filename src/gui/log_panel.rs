//! 底部日志面板：显示内存中的最近日志（上限 5000 行，界面只渲染最近 500 行）。

use eframe::egui;

use crate::app::BivoxApp;
use crate::log_sink::LogLine;

/// 界面每帧最多渲染的行数（避免大量 label 拖慢立即模式）。
const VISIBLE_LINES: usize = 500;

pub fn show(app: &mut BivoxApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("日志").strong());
        ui.label(
            egui::RichText::new(format!(
                "{} 条（显示最近 {} 条）",
                app.log.len(),
                VISIBLE_LINES
            ))
            .weak(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(format!("目录：{}", app.paths.log_dir.display())).weak());
        });
    });

    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.style_mut().override_text_style = Some(egui::TextStyle::Monospace);
            let skip = app.log.len().saturating_sub(VISIBLE_LINES);
            for line in app.log.iter().skip(skip) {
                ui.label(render(ui, line));
            }
        });
}

fn render(ui: &egui::Ui, line: &LogLine) -> egui::RichText {
    let color = match line.level {
        log::Level::Error => egui::Color32::from_rgb(0xe0, 0x4b, 0x4b),
        log::Level::Warn => egui::Color32::from_rgb(0xd8, 0x9a, 0x1b),
        log::Level::Info => ui.visuals().text_color(),
        log::Level::Debug | log::Level::Trace => egui::Color32::GRAY,
    };
    egui::RichText::new(format!("{} {:5} {}", line.time, line.level, line.message)).color(color)
}
