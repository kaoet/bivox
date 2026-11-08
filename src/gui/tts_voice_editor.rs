//! 自定义音色编辑器：左栏列表项、右栏样本行、样本拖拽排序与语速滑条。
//!
//! 这些函数只操作传入的 `VoiceDraft` 与 egui 状态；草稿的保存/放弃由
//! [`super::tts_tab`] 统一处理（决策 D22，见 docs/decisions.md）。需要改列表的操作
//! （删除样本、拖动排序）通过参数或返回值交给调用方应用，避免同时借用 `KaolsApp`。

use std::path::{Path, PathBuf};

use eframe::egui;

use crate::config::{DEFAULT_SAMPLE_RATE, MAX_SAMPLE_RATE, MIN_SAMPLE_RATE};
use crate::language::CLONING_LANGUAGES;
use crate::voice::editor::{config_audio_path, VoiceDraft};

const AMBER: egui::Color32 = egui::Color32::from_rgb(0xd8, 0x9a, 0x1b);

/// 编辑样本列表；需要删除的样本下标写入 `remove_sample`，由调用方在借用结束后应用。
pub(crate) fn edit_samples(
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

                // 语速只作用于用这个样本合成的句子，因此放在样本块内单独一行
                sample_rate_slider(ui, &mut sample.rate);

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

/// 单个样本的语速滑条：0.5×–2.0×，步进 0.05，只影响使用该样本合成的句子。
fn sample_rate_slider(ui: &mut egui::Ui, rate: &mut f32) {
    ui.horizontal_wrapped(|ui| {
        ui.label("语速");
        ui.add(
            egui::Slider::new(rate, MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE)
                .step_by(0.05)
                .fixed_decimals(2)
                .suffix("×"),
        );
        if ui
            .button("重置")
            .on_hover_text(format!("恢复默认语速 {:.2}×", DEFAULT_SAMPLE_RATE))
            .clicked()
        {
            *rate = DEFAULT_SAMPLE_RATE;
        }
        ui.label(egui::RichText::new("只影响使用该样本合成的句子").weak());
    });
}

fn resolve_audio(exe_dir: &Path, audio: &str) -> PathBuf {
    let path = Path::new(audio.trim());
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        exe_dir.join(path)
    }
}

/// 左栏单个自定义音色列表项；返回是否被点击。
pub(crate) fn voice_item(
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
