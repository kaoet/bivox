//! 顶部工具栏：状态、开始/停止（M2 接入）、设备展示（M1 接入下拉框）。

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

        ui.separator();

        // M1：换成设备下拉框（只存完整设备名，精确匹配，见 D12）
        ui.label(format!(
            "输入：{}",
            app.input_device.as_deref().unwrap_or("系统默认")
        ));
        ui.label(format!(
            "输出：{}",
            app.output_device.as_deref().unwrap_or("系统默认")
        ));

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).weak());
        });
    });
    ui.add_space(4.0);
}
