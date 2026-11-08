//! 语音合成页：页头（保存/放弃更改）、两栏布局编排与自定义音色草稿的增删保存（D11/D22，
//! 见 docs/decisions.md）。
//!
//! 页面按职责拆成三个文件：
//! - 本文件：页头、两栏布局编排、自定义音色草稿列表的选中/新增/删除与保存；
//! - `tts_current_voice`：顶部「当前音色」选择（现有音色 / 自定义音色）；
//! - `tts_voice_editor`：自定义音色的样本编辑、拖拽排序与语速滑条。
//!
//! 顶部选择「当前音色」：现有音色（输入框，填内置音色名或任意 voice_id）或
//! 自定义音色（下拉，写 `custom:<键名>`），右上角「保存」同时写回 `[tts] voice`
//! 与 `[tts.custom_voices]`。
//! 下方左栏是自定义音色草稿列表，右栏编辑选中音色：显示名、配置键名、语言声线、样例音频
//! 与每个样本的语速。
//! 草稿只在点「保存」后写回（`[tts.custom_voices]`，同时校验并备份）；
//! 保存后自动重载配置（运行中会按 D20 自动重启实时管线）。

use std::collections::HashSet;

use eframe::egui;

use crate::app::KaolsApp;
use crate::config::DEFAULT_SAMPLE_RATE;
use crate::gui::{tts_current_voice, tts_voice_editor};
use crate::language::CLONING_LANGUAGES;
use crate::voice::editor::{SampleDraft, VoiceDraft};

const RED: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x4b, 0x4b);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xd8, 0x9a, 0x1b);

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    // Tab 级操作：保存 / 放弃更改 作用于「当前音色 + 自定义音色草稿」全部改动
    let mut save = false;
    let mut reset = false;
    ui.horizontal(|ui| {
        ui.heading("语音合成");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let dirty = app.voice_drafts_dirty() || app.voice_input_dirty();
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
        app.save_tts_tab();
    }
    if reset {
        app.reset_tts_tab();
    }
    if let Some(error) = &app.voice_edit_error {
        ui.colored_label(RED, format!("保存失败：{error}"));
    }
    ui.label(
        egui::RichText::new(
            "上方选择「当前音色」；下方编辑自定义音色。改动后点右上角「保存」一次写回 config.toml\
             （运行中会自动重启实时管线），或点「放弃更改」还原。",
        )
        .weak(),
    );
    ui.add_space(6.0);

    tts_current_voice::show(app, ui);
    ui.add_space(6.0);

    let exe_dir = app.paths.exe_dir.clone();

    let mut clicked_key: Option<String> = None;
    let mut add_new = false;
    // 新增音色后，下一次绘制把列表滚到底（新音色追加在末尾）
    let scroll_to_end = app.voice_list_scroll_to_end;
    // 左侧面板只限制最大宽度（保证右侧编辑区不被挤没）；最小宽度保持很小，
    // 让面板按指针宽度收缩——内容最小宽度若大于指针宽度，egui 会把面板钳在
    // 较大值而内容仍按指针宽度布局，右侧就会出现空白。
    const LEFT_MIN_WIDTH: f32 = 110.0;
    const RIGHT_MIN_WIDTH: f32 = 360.0;
    let left_max_width = (ui.available_width() - RIGHT_MIN_WIDTH).max(LEFT_MIN_WIDTH);
    egui::Panel::left("tts_voice_list_panel")
        .resizable(true)
        .default_size(280.0)
        .min_size(LEFT_MIN_WIDTH)
        .max_size(left_max_width)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("自定义音色").strong());
            });
            if ui.button("＋ 新增音色").clicked() {
                add_new = true;
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .id_salt("tts_voice_list_scroll")
                .show(ui, |ui| {
                    if app.voice_drafts.is_empty() {
                        ui.label(egui::RichText::new("还没有自定义音色。").weak());
                    }
                    let selected = app.selected_voice.clone();
                    for (index, draft) in app.voice_drafts.iter().enumerate() {
                        let languages = draft
                            .samples
                            .iter()
                            .map(|sample| sample.language.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        let is_selected = selected.as_deref() == Some(draft.key.as_str());
                        // 新增音色后把列表滚到末尾（最后一项底部对齐）
                        let scroll_here = scroll_to_end && index + 1 == app.voice_drafts.len();
                        if tts_voice_editor::voice_item(
                            ui,
                            index,
                            draft,
                            &languages,
                            is_selected,
                            scroll_here,
                        ) {
                            clicked_key = Some(draft.key.clone());
                        }
                    }
                });
        });

    if add_new {
        let key = unique_voice_key(&app.voice_drafts);
        // 新音色的第一个样本固定用中文，后续可在样本行里改语言
        let language = "zh".to_string();
        app.voice_drafts.push(VoiceDraft {
            key: key.clone(),
            display_name: String::new(),
            samples: vec![SampleDraft {
                language,
                audio: String::new(),
                rate: DEFAULT_SAMPLE_RATE,
            }],
        });
        app.selected_voice = Some(key);
        app.voice_edit_error = None;
        // 新音色追加在列表末尾：等它真正画出来后的下一帧再把列表滚到底
        app.voice_list_scroll_to_end = true;
    }
    if let Some(key) = clicked_key {
        app.selected_voice = Some(key);
    }
    // 已经用 stick_to_bottom 画过一帧（新音色出现在末尾）后清除滚动标记
    if !add_new {
        app.voice_list_scroll_to_end = false;
    }

    let Some(key) = app.selected_voice.clone() else {
        return;
    };
    let Some(index) = app.voice_drafts.iter().position(|draft| draft.key == key) else {
        app.selected_voice = None;
        return;
    };

    let mut draft = app.voice_drafts[index].clone();
    let mut delete_voice = false;
    // 样本删除在滚出闭包后统一应用（避免同时借用 app）；排序在 edit_samples 内直接改草稿
    let mut remove_sample: Option<usize> = None;

    // 右侧内容与中间分割线之间留出间距
    egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: 12,
            right: 0,
            top: 0,
            bottom: 0,
        })
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .id_salt("tts_voice_detail_scroll")
                .show(ui, |ui| {
                    // 这里的标题/说明都是普通标签，不参与文本选择（否则拖动样本时
                    // 鼠标划过会把文字选成蓝色）。输入框不受这个开关影响。
                    ui.style_mut().interaction.selectable_labels = false;
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.heading(if draft.display_name.trim().is_empty() {
                            draft.key.as_str()
                        } else {
                            draft.display_name.as_str()
                        });
                    });
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        ui.label("显示名");
                        ui.add(
                            egui::TextEdit::singleline(&mut draft.display_name)
                                .desired_width(300.0)
                                .hint_text("留空则显示配置键名"),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("配置键名");
                        ui.add(egui::TextEdit::singleline(&mut draft.key).desired_width(180.0));
                        ui.label(egui::RichText::new("用于 [tts] voice = custom:<键名>").weak());
                    });

                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("声线样本（有序，第一个是默认声线）").strong());
                    tts_voice_editor::edit_samples(ui, &mut draft, &exe_dir, &mut remove_sample);

                    if ui.button("＋ 添加样本").clicked() {
                        let used: HashSet<&str> = draft
                            .samples
                            .iter()
                            .map(|sample| sample.language.as_str())
                            .collect();
                        let language = CLONING_LANGUAGES
                            .iter()
                            .find(|code| !used.contains(**code))
                            .copied()
                            .unwrap_or("zh");
                        draft.samples.push(SampleDraft {
                            language: language.to_string(),
                            audio: String::new(),
                            rate: DEFAULT_SAMPLE_RATE,
                        });
                    }

                    ui.add_space(10.0);
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("删除这个音色").clicked() {
                            delete_voice = true;
                        }
                    });
                });
        });

    // 把编辑后的草稿写回，并处理样本移动/删除（在草稿上进行）
    app.voice_drafts[index] = draft;
    // 配置键名可能刚被编辑：选中状态是按「键名」记录的，必须同步更新，
    // 否则下一帧列表找不到选中项，会取消选中并让右栏详情消失。
    let current_key = app.voice_drafts[index].key.clone();
    if app.selected_voice.as_deref() != Some(current_key.as_str()) {
        app.selected_voice = Some(current_key.clone());
    }
    // 顶部「自定义音色」下拉若正选中这个音色，键名被改名后要跟着走，
    // 否则下一帧在下拉选项里找不到旧键名，会退化成显示原始键名。
    if app.voice_custom_key == key && app.voice_custom_key != current_key {
        app.voice_custom_key = current_key.clone();
    }
    if let Some(i) = remove_sample {
        app.voice_drafts[index].samples.remove(i);
    }

    if delete_voice {
        app.voice_drafts.remove(index);
        app.voice_edit_error = None;
        app.selected_voice = None;
        // 顶部下拉选中的就是这个被删掉的音色时，换到剩余第一个（没有则清空）
        if app.voice_custom_key == key {
            app.voice_custom_key = app
                .voice_drafts
                .first()
                .map(|draft| draft.key.clone())
                .unwrap_or_default();
        }
    }
}

/// 生成一个当前草稿里没用过的配置键名（新增音色时用）。
fn unique_voice_key(drafts: &[VoiceDraft]) -> String {
    for index in 1.. {
        let key = format!("voice{index}");
        if !drafts.iter().any(|draft| draft.key == key) {
            return key;
        }
    }
    unreachable!()
}
