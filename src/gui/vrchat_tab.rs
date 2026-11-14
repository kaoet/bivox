//! VRChat 页：OSC 聊天框开关、端口、显示模式与发送测试消息（D33）。
//!
//! 所有改动立即写回 `[vrchat.osc]`，不进配置指纹、不重启实时管线；
//! 页面刻意保持精简：没有状态行、typing 说明或隐私提示。

use eframe::egui;

use crate::app::{KaolsApp, Tab};
use crate::config::{VrchatDisplay, VrchatOscConfig};

const RED: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x4b, 0x4b);

fn current(app: &KaolsApp) -> VrchatOscConfig {
    app.config.vrchat.osc.clone()
}

fn display_label(display: VrchatDisplay) -> &'static str {
    match display {
        VrchatDisplay::Source => "只显示原文",
        VrchatDisplay::Bilingual => "显示原文和译文",
        VrchatDisplay::Translation => "只显示译文",
    }
}

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.heading("VRChat");
    ui.label(egui::RichText::new("把实时字幕发送到 VRChat 的 OSC 聊天框。").weak());
    if let Some(error) = app.config_write_error_for(Tab::Vrchat) {
        ui.colored_label(RED, format!("保存失败：{error}"));
    }
    ui.add_space(8.0);

    let configured = current(app);

    let mut enabled = configured.enabled;
    let mut port = configured.port;
    let mut display = configured.display;

    let enabled_changed = ui.checkbox(&mut enabled, "启用 OSC 聊天框").changed();
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.label("发送端口");
        let response = ui.add(
            egui::DragValue::new(&mut port)
                .range(1..=65535)
                .speed(1.0)
                .max_decimals(0),
        );
        let enter = response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        let port_committed =
            port != configured.port && (response.lost_focus() || response.drag_stopped() || enter);
        ui.label(egui::RichText::new("VRChat 默认 9000").weak());

        if enabled_changed || port_committed || display != configured.display {
            if enabled != configured.enabled
                || port != configured.port
                || display != configured.display
            {
                app.update_vrchat_settings(enabled, port, display);
            }
        }
    });

    ui.add_space(8.0);
    ui.label("显示模式");
    for mode in [
        VrchatDisplay::Source,
        VrchatDisplay::Bilingual,
        VrchatDisplay::Translation,
    ] {
        if ui
            .radio_value(&mut display, mode, display_label(mode))
            .changed()
        {
            app.update_vrchat_settings(enabled, port, display);
        }
    }

    ui.add_space(10.0);
    if ui
        .add_enabled(configured.enabled, egui::Button::new("发送测试消息"))
        .clicked()
    {
        app.send_test_message();
    }
}
