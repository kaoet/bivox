//! 顶部工具栏：会话状态、开始/停止（M2）。

use eframe::egui;

use crate::app::{ConfigState, KaolsApp};
use crate::events::SessionState;

/// 状态灯颜色。
fn state_color(state: SessionState) -> egui::Color32 {
    match state {
        SessionState::Idle => egui::Color32::GRAY,
        SessionState::Starting | SessionState::Stopping => {
            egui::Color32::from_rgb(0xd8, 0x9a, 0x1b)
        }
        SessionState::Running => egui::Color32::from_rgb(0x3c, 0xc9, 0x6f),
        SessionState::Failed => egui::Color32::from_rgb(0xe0, 0x4b, 0x4b),
    }
}

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.heading("kaols");
        ui.label(egui::RichText::new("超低延迟实时识别与合成").weak());
        ui.separator();

        ui.colored_label(state_color(app.state), format!("● {}", app.state.label()));
        ui.separator();

        let can_start = matches!(app.state, SessionState::Idle | SessionState::Failed);
        let can_stop = matches!(app.state, SessionState::Starting | SessionState::Running);

        if ui
            .add_enabled(can_start, egui::Button::new("开始"))
            .clicked()
        {
            let ctx = ui.ctx().clone();
            app.start_session(ctx);
        }
        if ui
            .add_enabled(can_stop, egui::Button::new("停止"))
            .clicked()
        {
            app.stop_session();
        }

        if matches!(app.config, ConfigState::Invalid { .. }) {
            ui.label(egui::RichText::new("配置无效，无法开始").weak());
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).weak());
        });
    });
    ui.add_space(4.0);
}
