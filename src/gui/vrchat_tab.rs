//! VRChat 页：对方语音的采集目标（`[vrchat] process`）与 OSC 聊天框（D33/D38）。
//!
//! 两种写回语义共存，页面内分开写：
//! - `process` 是文本框 + 「应用」：它进配置指纹，改了会按 D20 重启实时管线；
//! - OSC 开关/端口/显示模式是改值即保存、不重启（`ChatboxService` 通过 watch 实时读取）。

use eframe::egui;

use crate::app::{BivoxApp, Tab};
use crate::config::{
    DEFAULT_VRCHAT_PROCESS, MAX_VRCHAT_RETENTION_SECS, MIN_VRCHAT_RETENTION_SECS, VrchatDisplay,
    VrchatOscConfig,
};

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

/// OSC 聊天框设置：改值即保存、不重启实时管线（D33/D42）。
fn show_osc(app: &mut BivoxApp, ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("OSC 聊天框").strong());
    ui.label(egui::RichText::new("把实时字幕发送到 VRChat 的 OSC 聊天框。").weak());

    let configured = current(app);
    let mut osc = configured.clone();
    let mut save = false;

    if ui.checkbox(&mut osc.enabled, "启用 OSC 聊天框").changed() {
        save = true;
    }
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        ui.label("发送端口");
        let response = ui.add(
            egui::DragValue::new(&mut osc.port)
                .range(1..=65535)
                .speed(1.0)
                .max_decimals(0)
                // 编辑中的文本由 egui 暂存；这里的值每帧从配置重建，
                // 若边输入边写回会导致 EditState 失效、输入被重置。
                .update_while_editing(false),
        );
        let enter = response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        // 拖动中的每帧变化不写盘；失焦/回车提交时 DragValue 才改值（changed）。
        let committed = response.drag_stopped()
            || response.lost_focus()
            || enter
            || (response.changed() && !response.dragged());
        if osc.port != configured.port && committed {
            save = true;
        }
        ui.label(egui::RichText::new("VRChat 默认 9000").weak());
    });

    ui.add_space(8.0);
    ui.label("显示模式");
    for mode in [
        VrchatDisplay::Source,
        VrchatDisplay::Bilingual,
        VrchatDisplay::Translation,
    ] {
        if ui
            .radio_value(&mut osc.display, mode, display_label(mode))
            .changed()
        {
            save = true;
        }
    }

    ui.add_space(8.0);
    if ui.checkbox(&mut osc.show_history, "显示历史句").changed() {
        save = true;
    }
    ui.horizontal(|ui| {
        ui.label("保留时长");
        let response = ui
            .add(
                egui::DragValue::new(&mut osc.retention_secs)
                    .range(MIN_VRCHAT_RETENTION_SECS..=MAX_VRCHAT_RETENTION_SECS)
                    .speed(1.0)
                    .max_decimals(0)
                    .suffix(" 秒")
                    // 同上：失焦/回车时才把文本写回，避免每帧重建的值让输入框回到旧值。
                    .update_while_editing(false),
            )
            .on_hover_text(
                "VRChat 设置里的聊天框有效期可配 2–60 秒（默认 30 秒）；\
                 保留时长超过它时，文本会先被 VRChat 清掉。",
            );
        let enter = response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        // 失焦后的提交发生在下一帧（此时 response 已是按钮而不是 TextEdit），
        // 所以必须用 changed() 捕获这一帧的值变化。
        let committed = response.drag_stopped()
            || response.lost_focus()
            || enter
            || (response.changed() && !response.dragged());
        if osc.retention_secs != configured.retention_secs && committed {
            save = true;
        }
    });

    if save && osc != configured {
        app.update_vrchat_settings(
            osc.enabled,
            osc.port,
            osc.display,
            osc.show_history,
            osc.retention_secs,
        );
    }

    ui.add_space(10.0);
    if ui
        .add_enabled(configured.enabled, egui::Button::new("发送测试消息"))
        .clicked()
    {
        app.send_test_message();
    }
}
