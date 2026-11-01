//! 语音合成页：TTS 配置与自定义音色编辑器（M3 / D11）。
//!
//! 左栏是音色草稿列表，右栏编辑选中音色：显示名、配置键名、语言声线与样例音频。
//! 草稿只在点「保存到 config.toml」后写回（`[tts.custom_voices]`，同时校验并备份）；
//! 保存后自动重载配置（运行中会按 D20 自动重启实时管线）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use eframe::egui;

use crate::app::{ConfigState, KaolsApp};
use crate::language::CLONING_LANGUAGES;
use crate::voice::editor::{config_audio_path, SampleDraft, VoiceDraft};

const RED: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x4b, 0x4b);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xd8, 0x9a, 0x1b);
const GREEN: egui::Color32 = egui::Color32::from_rgb(0x3c, 0xc9, 0x6f);

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.heading("语音合成");
    ui.label(
        egui::RichText::new(
            "自定义音色在这里编辑：显示名、各语言声线与样例音频。\
             点「保存到 config.toml」才写回；运行中保存会自动重启实时管线以应用新配置。",
        )
        .weak(),
    );
    ui.add_space(6.0);

    let active_voice = match &app.config {
        ConfigState::Loaded(config) => config.tts.voice.clone(),
        ConfigState::Invalid { .. } => String::new(),
    };
    let exe_dir = app.paths.exe_dir.clone();

    let mut clicked_key: Option<String> = None;
    let mut add_new = false;
    let mut save = false;
    let mut reset = false;
    // 新增音色后，下一次绘制把列表滚到底（新音色追加在末尾）
    let scroll_to_end = app.voice_list_scroll_to_end;
    egui::Panel::left("tts_voice_list_panel")
        .resizable(true)
        .default_size(280.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("自定义音色").strong());
                if app.voice_drafts_dirty() {
                    ui.label(egui::RichText::new("● 未保存").color(AMBER).small());
                }
            });
            let dirty = app.voice_drafts_dirty();
            ui.horizontal(|ui| {
                if ui.button("＋ 新增音色").clicked() {
                    add_new = true;
                }
                if ui.add_enabled(dirty, egui::Button::new("保存")).clicked() {
                    save = true;
                }
                if ui
                    .add_enabled(dirty, egui::Button::new("放弃修改"))
                    .clicked()
                {
                    reset = true;
                }
            });
            if dirty {
                ui.label(egui::RichText::new("有未保存修改：点「保存」写回 config.toml").weak());
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
                        let is_active = active_voice == format!("custom:{}", draft.key.as_str());
                        // 新增音色后把列表滚到末尾（最后一项底部对齐）
                        let scroll_here = scroll_to_end && index + 1 == app.voice_drafts.len();
                        if voice_item(
                            ui,
                            index,
                            draft,
                            &languages,
                            is_selected,
                            is_active,
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

    // 保存 / 放弃修改作用于整份草稿，不依赖「当前选中项」：
    // 删掉选中的音色后列表会变空，这两个按钮也必须继续可用。
    if save {
        app.save_voice_drafts();
    }
    if reset {
        // 放弃修改后保持选中：记录当前选中项的行号（草稿顺序与配置排序一致），
        // 重置后按行号找回；编辑过键名/显示名都不影响行号。
        let wanted_index = app
            .selected_voice
            .as_ref()
            .and_then(|key| app.voice_drafts.iter().position(|draft| &draft.key == key));
        app.reset_voice_drafts();
        // 放弃修改时一并清掉上一次保存失败的错误提示
        app.voice_edit_error = None;
        if let Some(index) = wanted_index {
            if let Some(draft) = app.voice_drafts.get(index) {
                app.selected_voice = Some(draft.key.clone());
            }
        }
    }

    let Some(key) = app.selected_voice.clone() else {
        return;
    };
    let Some(index) = app.voice_drafts.iter().position(|draft| draft.key == key) else {
        app.selected_voice = None;
        return;
    };

    let mut draft = app.voice_drafts[index].clone();
    let mut set_active = false;
    let mut delete_voice = false;
    // 样本操作在滚出闭包后统一应用（避免同时借用 app）
    let mut move_up: Option<usize> = None;
    let mut move_down: Option<usize> = None;
    let mut remove_sample: Option<usize> = None;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("tts_voice_detail_scroll")
        .show(ui, |ui| {
            ui.add_space(4.0);
            let is_active = active_voice == format!("custom:{key}");
            ui.horizontal_wrapped(|ui| {
                ui.heading(if draft.display_name.trim().is_empty() {
                    draft.key.as_str()
                } else {
                    draft.display_name.as_str()
                });
                if is_active {
                    ui.colored_label(GREEN, "当前生效音色");
                }
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
            edit_samples(
                ui,
                &mut draft,
                &exe_dir,
                &mut move_up,
                &mut move_down,
                &mut remove_sample,
            );

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
                });
            }

            ui.add_space(10.0);
            ui.separator();
            if let Some(error) = &app.voice_edit_error {
                ui.colored_label(RED, format!("保存失败：{error}"));
            }
            let dirty = app.voice_drafts_dirty();
            let config_loaded = matches!(app.config, ConfigState::Loaded(_));
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !dirty && config_loaded && !is_active,
                        egui::Button::new("设为当前音色"),
                    )
                    .clicked()
                {
                    set_active = true;
                }
                if ui.button("删除这个音色").clicked() {
                    delete_voice = true;
                }
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
    if let Some(i) = move_up {
        app.voice_drafts[index].samples.swap(i, i - 1);
    }
    if let Some(i) = move_down {
        app.voice_drafts[index].samples.swap(i, i + 1);
    }
    if let Some(i) = remove_sample {
        app.voice_drafts[index].samples.remove(i);
    }

    if set_active {
        app.set_active_voice_custom(&current_key);
    }
    if delete_voice {
        app.voice_drafts.remove(index);
        app.voice_edit_error = None;
        app.selected_voice = None;
    }
}

/// 编辑样本列表；返回需要在调用方应用的操作（避免同时借用 `app`）。
fn edit_samples(
    ui: &mut egui::Ui,
    draft: &mut VoiceDraft,
    exe_dir: &Path,
    move_up: &mut Option<usize>,
    move_down: &mut Option<usize>,
    remove_sample: &mut Option<usize>,
) {
    let count = draft.samples.len();
    for (sample_index, sample) in draft.samples.iter_mut().enumerate() {
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("样本 {}", sample_index + 1)).strong());
            if sample_index == 0 {
                ui.label(egui::RichText::new("默认声线").weak());
            }
        });

        ui.horizontal(|ui| {
            ui.label("语言");
            let mut current = sample.language.clone();
            egui::ComboBox::from_id_salt(("voice_lang", draft.key.as_str(), sample_index))
                .selected_text(current.as_str())
                .width(80.0)
                .show_ui(ui, |ui| {
                    for code in CLONING_LANGUAGES {
                        ui.selectable_value(&mut current, (*code).to_string(), *code);
                    }
                });
            sample.language = current;
            ui.label(egui::RichText::new("音频").weak());
            ui.add(
                egui::TextEdit::singleline(&mut sample.audio)
                    .desired_width(320.0)
                    .hint_text("样例音频路径（相对 exe 目录或绝对路径）"),
            );
            if ui.button("选择…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("音频", &["wav", "mp3", "m4a", "flac", "ogg"])
                    .pick_file()
                {
                    sample.audio = config_audio_path(exe_dir, &path);
                }
            }
        });

        if !sample.audio.trim().is_empty() {
            let resolved = resolve_audio(exe_dir, &sample.audio);
            if !resolved.is_file() {
                ui.colored_label(AMBER, format!("⚠ 音频文件不存在：{}", resolved.display()));
            }
        }

        ui.horizontal(|ui| {
            if sample_index > 0 && ui.small_button("↑ 上移").clicked() {
                *move_up = Some(sample_index);
            }
            if sample_index + 1 < count && ui.small_button("↓ 下移").clicked() {
                *move_down = Some(sample_index);
            }
            if count > 1 && ui.small_button("删除").clicked() {
                *remove_sample = Some(sample_index);
            }
        });
    }
}

fn resolve_audio(exe_dir: &Path, audio: &str) -> PathBuf {
    let path = Path::new(audio.trim());
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        exe_dir.join(path)
    }
}

fn unique_voice_key(drafts: &[VoiceDraft]) -> String {
    for index in 1.. {
        let key = format!("voice{index}");
        if !drafts.iter().any(|draft| draft.key == key) {
            return key;
        }
    }
    unreachable!()
}

fn voice_item(
    ui: &mut egui::Ui,
    index: usize,
    draft: &VoiceDraft,
    languages: &str,
    selected: bool,
    is_active: bool,
    scroll_here: bool,
) -> bool {
    let fill = if selected {
        ui.visuals().selection.bg_fill
    } else {
        egui::Color32::TRANSPARENT
    };
    let frame = egui::Frame::NONE
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(6, 4))
        .corner_radius(egui::CornerRadius::same(4));
    let inner = frame.show(ui, |ui| {
        // 列表项整行都可以点：不要让文字变成可选（否则悬停会出现 I 形光标）
        ui.style_mut().interaction.selectable_labels = false;
        ui.set_width(ui.available_width());
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                let name = if draft.display_name.trim().is_empty() {
                    draft.key.as_str()
                } else {
                    draft.display_name.as_str()
                };
                ui.label(name);
                ui.label(egui::RichText::new(&draft.key).weak());
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(languages).weak());
                if is_active {
                    ui.colored_label(GREEN, "当前生效");
                }
            });
        });
    });
    let response = ui
        .interact(
            inner.response.rect,
            // 用行号做 id：编辑配置键名时 id 不会跟着变
            ui.scope_id().with(("tts_voice_item", index)),
            egui::Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if scroll_here {
        // 让外层 ScrollArea 把这一项滚到可见区域底部
        ui.scroll_to_rect(inner.response.rect, Some(egui::Align::BOTTOM));
    }
    response.clicked()
}
