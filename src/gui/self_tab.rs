//! 「自己」页：我方线路的麦克风、语言列表、翻译与合成开关（D38）。
//!
//! 只改草稿，点右上角「保存」由 `KaolsApp::save_self_tab` 一次写回 `[self]`；
//! 「放弃更改」恢复配置文件里的值。所有字段都进配置指纹，运行中保存按 D20 热重启。
//!
//! 音色、合成模型与中文方言不在这里：它们属于「语音合成」页（`[tts]`）。

use eframe::egui;

use crate::app::KaolsApp;
use crate::gui::language_list;
use crate::language::TRANSLATION_LANGUAGES;

const RED: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x4b, 0x4b);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xd8, 0x9a, 0x1b);

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("self_tab_scroll")
        .show(ui, |ui| {
            // 语言列表靠拖动手柄排序：鼠标划过标签文字时不能进入“选中文字”状态
            ui.style_mut().interaction.selectable_labels = false;
            show_content(app, ui);
        });
}

fn show_content(app: &mut KaolsApp, ui: &mut egui::Ui) {
    let mut save = false;
    let mut reset = false;
    ui.horizontal(|ui| {
        ui.heading("自己");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let dirty = app.self_tab_dirty();
            if ui.add_enabled(dirty, egui::Button::new("保存")).clicked() {
                save = true;
            }
            if ui
                .add_enabled(dirty, egui::Button::new("放弃更改"))
                .clicked()
            {
                reset = true;
            }
            if dirty {
                ui.label(egui::RichText::new("● 未保存").color(AMBER).small());
            }
        });
    });
    if save {
        app.save_self_tab();
    }
    if reset {
        app.reset_self_tab();
    }
    if let Some(error) = &app.self_edit_error {
        ui.colored_label(RED, format!("保存失败：{error}"));
    }
    ui.label(
        egui::RichText::new(
            "本页是我方线路的专属设置：麦克风、说的语言、要不要翻译、要不要合成语音。\
             共用的识别模型/热词在「语音识别」页，音色在「语音合成」页。",
        )
        .weak(),
    );

    ui.add_space(8.0);
    ui.checkbox(&mut app.self_mic, "采集麦克风");
    ui.label(egui::RichText::new("关闭后不录音、不上传，只能用实时页底部的打字输入。").weak());

    ui.add_space(10.0);
    language_list::show(ui, "self_languages", &mut app.self_languages);

    ui.add_space(10.0);
    ui.checkbox(&mut app.self_translate, "把我说的话翻译成目标语言");
    let mut target = app.self_target.clone();
    ui.add_enabled_ui(app.self_translate, |ui| {
        ui.horizontal(|ui| {
            ui.label("目标语言");
            egui::ComboBox::from_id_salt("self_target")
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
    app.self_target = target;

    ui.add_space(10.0);
    ui.checkbox(&mut app.self_tts, "合成语音并播放");
    ui.label(
        egui::RichText::new("关闭后不解析音色、不建 TTS，只上屏（仍可发 OSC 聊天框）。").weak(),
    );
}
