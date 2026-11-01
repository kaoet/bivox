//! 顶部工具栏：会话状态、开始/停止（M2 接入实时管线）。
//!
//! 设备选择已移到「音频」页，这里不再显示设备。

use eframe::egui;

use crate::app::{KaolsApp, SessionState};

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.heading("kaols");
        ui.label(egui::RichText::new("超低延迟实时识别与合成").weak());
        ui.separator();

        ui.colored_label(app.state.color(), format!("● {}", app.state.label()));
        ui.separator();

        // M2：接 PipelineSession 的 start/stop
        let running = matches!(app.state, SessionState::Running | SessionState::Starting);
        let _ = ui.add_enabled(!running, egui::Button::new("开始"));
        let _ = ui.add_enabled(running, egui::Button::new("停止"));
        ui.label(egui::RichText::new("（M2 接入实时管线）").weak());

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).weak());
        });
    });
    ui.add_space(4.0);
}
