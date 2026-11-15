//! VRChat 页：对方语音的采集目标（`[vrchat] process`）与 OSC 聊天框（D33/D38）。
//!
//! 两种写回语义共存，页面内分开写：
//! - `process` 是文本框 + 「应用」：它进配置指纹，改了会按 D20 重启实时管线；
//! - OSC 开关/端口/显示模式是改值即保存、不重启（`ChatboxService` 通过 watch 实时读取）。

use eframe::egui;

use crate::app::{BivoxApp, Tab};
use crate::config::{DEFAULT_VRCHAT_PROCESS, VrchatDisplay, VrchatOscConfig};

const RED: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x4b, 0x4b);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xd8, 0x9a, 0x1b);

fn current(app: &BivoxApp) -> VrchatOscConfig {
    app.config.vrchat.osc.clone()
}

fn display_label(display: VrchatDisplay) -> &'static str {
    match display {
        VrchatDisplay::Source => "只显示原文",
        VrchatDisplay::Bilingual => "显示原文和译文",
        VrchatDisplay::Translation => "只显示译文",
    }
}

pub fn show(app: &mut BivoxApp, ui: &mut egui::Ui) {
    ui.heading("VRChat");
    ui.label(
        egui::RichText::new("对方语音从本机哪个进程采集，以及字幕要不要发到 VRChat 聊天框。")
            .weak(),
    );
    if let Some(error) = app.config_write_error_for(Tab::Vrchat) {
        ui.colored_label(RED, format!("保存失败：{error}"));
    }
    ui.add_space(8.0);

    show_process(app, ui);
    ui.separator();
    show_osc(app, ui);
}

/// 对方语音的采集目标：文本框 + 「应用」（运行中应用会重启实时管线）。
fn show_process(app: &mut BivoxApp, ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("对方语音").strong());
    ui.horizontal(|ui| {
        ui.label("监听进程");
        ui.add(
            egui::TextEdit::singleline(&mut app.vrchat_process)
                .desired_width(220.0)
                .hint_text(DEFAULT_VRCHAT_PROCESS),
        );
        let dirty = app.vrchat_process.trim() != app.config.vrchat.process.as_str();
        let can_apply = dirty && !app.vrchat_process.trim().is_empty();
        if ui
            .add_enabled(can_apply, egui::Button::new("应用"))
            .clicked()
        {
            app.save_vrchat_process();
        }
        if dirty {
            ui.label(egui::RichText::new("● 未保存").color(AMBER).small());
        }
    });
    ui.label(
        egui::RichText::new(
            "按 exe 文件名匹配进程树（进程环回，D36）。运行中应用会重启实时管线；\
             进程不在时只告警并自动重连，不会中断会话。",
        )
        .weak(),
    );
    ui.add_space(8.0);
}

/// OSC 聊天框设置：改值即保存、不重启实时管线。
fn show_osc(app: &mut BivoxApp, ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("OSC 聊天框").strong());
    ui.label(egui::RichText::new("把实时字幕发送到 VRChat 的 OSC 聊天框。").weak());

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
