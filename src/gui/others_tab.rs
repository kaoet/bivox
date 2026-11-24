//! 「他人」页：他人线路的开关、语言列表与翻译（D38）。
//!
//! 音源（从哪个进程采集）不在这里：那是「VRChat」页的 `[vrchat] process`。
//! 只改草稿，点右上角「保存」由 `BivoxApp::save_others_tab` 一次写回 `[others]`；
//! 所有字段都进配置指纹，运行中保存按 D20 热重启。

use eframe::egui;

use crate::app::BivoxApp;
use crate::gui::{language_list, widgets};
use crate::language::TRANSLATION_LANGUAGES;

pub fn show(app: &mut BivoxApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("others_tab_scroll")
        .show(ui, |ui| {
            // 语言列表靠拖动手柄排序：鼠标划过标签文字时不能进入“选中文字”状态
            ui.style_mut().interaction.selectable_labels = false;
            show_content(app, ui);
        });
}

fn show_content(app: &mut BivoxApp, ui: &mut egui::Ui) {
    let action = widgets::save_bar(ui, "他人", app.others_tab_dirty());
    if action.save {
        app.save_others_tab();
    }
    if action.reset {
        app.reset_others_tab();
    }
    widgets::error_line(ui, app.others_edit_error.as_deref());
    ui.label(
        egui::RichText::new(
            "本页是他人线路的专属设置：说的话从「VRChat」页配置的进程采集，只上屏，\
             不发 OSC、不做 TTS。进程不可达时会自动重连。",
        )
        .weak(),
    );

    ui.add_space(8.0);
    let mut others_enabled = app.config.others.enabled;
    if ui.checkbox(&mut others_enabled, "侦听他人语音").changed() {
        app.set_others_enabled(others_enabled);
    }
    ui.label(
        egui::RichText::new(
            "要采集的进程在「VRChat」页设置；关闭时不采集、不建第二条识别连接。切换立即生效，无需保存。",
        )
        .weak(),
    );

    ui.add_space(10.0);
    language_list::show(ui, "others_languages", &mut app.others_languages);

    ui.add_space(10.0);
    ui.checkbox(&mut app.others_translate, "把他人说的话翻译成目标语言");
    let mut target = app.others_target.clone();
    ui.add_enabled_ui(app.others_translate, |ui| {
        ui.horizontal(|ui| {
            ui.label("目标语言");
            egui::ComboBox::from_id_salt("others_target")
                .selected_text(language_list::language_label(&target))
                .width(200.0)
                .show_ui(ui, |ui| {
                    for code in TRANSLATION_LANGUAGES {
                        ui.selectable_value(
                            &mut target,
                            (*code).to_string(),
                            language_list::language_label(code),
                        );
                    }
                });
        });
    });
    app.others_target = target;
}
