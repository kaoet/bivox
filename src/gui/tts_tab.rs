//! 语音合成页：当前音色选择 + 自定义音色编辑器（M3 / D11）。
//!
//! 顶部选择「当前音色」：现有音色（输入框，填内置音色名或任意 voice_id）或
//! 自定义音色（下拉，写 `custom:<键名>`），点「应用」写回 `[tts] voice`。
//! 下方左栏是自定义音色草稿列表，右栏编辑选中音色：显示名、配置键名、语言声线与样例音频。
//! 草稿只在点「保存」后写回（`[tts.custom_voices]`，同时校验并备份）；
//! 保存后自动重载配置（运行中会按 D20 自动重启实时管线）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use eframe::egui;

use crate::app::{KaolsApp, VoiceMode};
use crate::language::CLONING_LANGUAGES;
use crate::voice::editor::{config_audio_path, SampleDraft, VoiceDraft};

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

    draw_current_voice(app, ui);
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
                        if voice_item(ui, index, draft, &languages, is_selected, scroll_here) {
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
                    edit_samples(ui, &mut draft, &exe_dir, &mut remove_sample);

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

/// 顶部「当前音色」：Radio 二选一（未选中的控件禁用）+ 显式「应用」。
fn draw_current_voice(app: &mut KaolsApp, ui: &mut egui::Ui) {
    let mut mode = app.voice_mode;
    let mut existing = app.voice_input.clone();
    let mut custom_key = app.voice_custom_key.clone();
    let options: Vec<(String, String)> = app
        .voice_drafts
        .iter()
        .map(|draft| {
            let name = if draft.display_name.trim().is_empty() {
                draft.key.clone()
            } else {
                format!("{}（{}）", draft.display_name, draft.key)
            };
            (draft.key.clone(), name)
        })
        .collect();

    ui.horizontal(|ui| {
        ui.radio_value(&mut mode, VoiceMode::Existing, "现有音色");
        ui.add_enabled(
            mode == VoiceMode::Existing,
            egui::TextEdit::singleline(&mut existing)
                .desired_width(340.0)
                .hint_text("内置音色或任意 voice_id，如 qiaoxiaojiao_v3.1"),
        );
    });
    ui.horizontal(|ui| {
        ui.radio_value(&mut mode, VoiceMode::Custom, "自定义音色");
        ui.add_enabled_ui(mode == VoiceMode::Custom, |ui| {
            let selected_label = options
                .iter()
                .find(|(key, _)| key == &custom_key)
                .map(|(_, label)| label.clone())
                .unwrap_or_else(|| {
                    if custom_key.is_empty() {
                        "（没有自定义音色）".to_string()
                    } else {
                        custom_key.clone()
                    }
                });
            egui::ComboBox::from_id_salt("current_voice_custom")
                .selected_text(selected_label)
                .width(320.0)
                .show_ui(ui, |ui| {
                    if options.is_empty() {
                        ui.label(egui::RichText::new("还没有自定义音色").weak());
                    }
                    for (key, label) in &options {
                        ui.selectable_value(&mut custom_key, key.clone(), label);
                    }
                });
        });
    });

    app.voice_mode = mode;
    app.voice_input = existing;
    app.voice_custom_key = custom_key;
}

/// 编辑样本列表；返回需要在调用方应用的操作（避免同时借用 `app`）。
fn edit_samples(
    ui: &mut egui::Ui,
    draft: &mut VoiceDraft,
    exe_dir: &Path,
    remove_sample: &mut Option<usize>,
) {
    let count = draft.samples.len();
    // 拖拽排序：手柄按下时把下标存进 egui 临时数据，松手时按指针位置插入
    let drag_id = ui.make_persistent_id(("voice_sample_drag", draft.key.as_str()));
    let mut drag_from = ui.data(|data| data.get_temp::<usize>(drag_id));
    let mut released = false;
    let mut block_rects: Vec<egui::Rect> = Vec::with_capacity(count);

    for (sample_index, sample) in draft.samples.iter_mut().enumerate() {
        ui.separator();
        // 左侧：拖拽手柄（与右侧内容块垂直居中）；右侧：标题行 + 设置行
        let mut block_rect: Option<egui::Rect> = None;
        ui.horizontal(|ui| {
            let row_left = ui.cursor().left();
            // 先给手柄列留出宽度，右侧内容正常排版后再把手柄放到内容块垂直中心
            ui.add_space(18.0);
            let right = ui.vertical(|ui| {
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
                            .desired_width(280.0)
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
                    // 删除按钮放在设置行末尾
                    if count > 1 && ui.button("删除").clicked() {
                        *remove_sample = Some(sample_index);
                    }
                });

                if !sample.audio.trim().is_empty() {
                    let resolved = resolve_audio(exe_dir, &sample.audio);
                    if !resolved.is_file() {
                        ui.colored_label(
                            AMBER,
                            format!("⚠ 音频文件不存在：{}", resolved.display()),
                        );
                    }
                }
            });
            block_rect = Some(right.response.rect);

            let handle_rect = egui::Rect::from_center_size(
                egui::pos2(row_left + 8.0, right.response.rect.center().y),
                egui::vec2(16.0, 20.0),
            );
            let handle = ui
                .put(
                    handle_rect,
                    egui::Label::new(egui::RichText::new("≡").weak()).sense(egui::Sense::drag()),
                )
                .on_hover_cursor(egui::CursorIcon::Grab)
                .on_hover_text("拖动调整样本顺序");
            if handle.drag_started() {
                drag_from = Some(sample_index);
                ui.data_mut(|data| data.insert_temp(drag_id, sample_index));
            }
            if handle.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            if handle.drag_stopped() {
                released = true;
                ui.data_mut(|data| data.remove_temp::<usize>(drag_id));
            }
        });
        if let Some(rect) = block_rect {
            block_rects.push(rect);
        }
    }

    // 松手时按指针所在位置插入；拖动过程中在目标插入位置画一条指示线。
    // 指示线画在“整块样本”（标题行+设置行）的边界上，避免出现在块内部造成歧义。
    if let Some(from) = drag_from {
        if let Some(pointer) = ui.ctx().pointer_latest_pos() {
            let insert_at = block_rects
                .iter()
                .position(|rect| pointer.y < rect.center().y)
                .unwrap_or(block_rects.len());
            let stroke = egui::Stroke::new(2.0, ui.visuals().selection.stroke.color);
            let y = block_rects
                .get(insert_at)
                .map(|rect| rect.top())
                .or_else(|| block_rects.last().map(|rect| rect.bottom()));
            if let Some(y) = y {
                let x = block_rects
                    .first()
                    .map(|rect| egui::Rangef::new(rect.left(), ui.max_rect().right()))
                    .unwrap_or(egui::Rangef::new(0.0, ui.max_rect().right()));
                ui.painter().hline(x, y, stroke);
            }
            if released {
                let len = draft.samples.len();
                let mut to = insert_at;
                if from < to {
                    to -= 1;
                }
                let to = to.min(len.saturating_sub(1));
                if from < len && from != to {
                    let sample = draft.samples.remove(from);
                    draft.samples.insert(to, sample);
                }
                ui.data_mut(|data| data.remove_temp::<usize>(drag_id));
            }
        }
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
            });
        });
    });
    let response = ui.interact(
        inner.response.rect,
        // 用行号做 id：编辑配置键名时 id 不会跟着变
        ui.scope_id().with(("tts_voice_item", index)),
        egui::Sense::click(),
    );
    if scroll_here {
        // 让外层 ScrollArea 把这一项滚到可见区域底部
        ui.scroll_to_rect(inner.response.rect, Some(egui::Align::BOTTOM));
    }
    response.clicked()
}
