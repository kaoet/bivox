//! 自定义音色编辑器：左栏列表项、右栏样本行、样本拖拽排序与语速滑条。
//!
//! 这些函数只操作传入的 `VoiceDraft` 与 egui 状态；草稿的保存/放弃由
//! [`super::tts_tab`] 统一处理（决策 D22，见 docs/decisions.md）。需要改列表的操作
//! （删除样本、拖动排序）通过参数或返回值交给调用方应用，避免同时借用 `BivoxApp`。
//!
//! 拖动排序的交互与指示线由 [`super::drag_reorder`] 提供（语言列表共用同一套）。

use std::path::{Path, PathBuf};

use eframe::egui;

use crate::config::{DEFAULT_SAMPLE_RATE, MAX_SAMPLE_RATE, MIN_SAMPLE_RATE};
use crate::gui::{drag_reorder::DragReorder, theme};
use crate::language::CLONING_LANGUAGES;
use crate::voice::editor::{VoiceDraft, config_audio_path};

/// 编辑样本列表；需要删除的样本下标写入 `remove_sample`，由调用方在借用结束后应用。
pub(crate) fn edit_samples(
    ui: &mut egui::Ui,
    draft: &mut VoiceDraft,
    exe_dir: &Path,
    remove_sample: &mut Option<usize>,
) {
    let count = draft.samples.len();
    // 拖动状态的 id 与样本行里的 ComboBox 都要用键名；先克隆一份，避免与 `samples` 的可变借用冲突。
    let voice_key = draft.key.clone();
    let mut drag = DragReorder::new(ui, ("voice_sample_drag", voice_key.as_str()));

    for (sample_index, sample) in draft.samples.iter_mut().enumerate() {
        ui.separator();
        drag.row(ui, sample_index, "拖动调整样本顺序", |ui| {
            // 右侧：标题行 + 设置行
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("样本 {}", sample_index + 1)).strong());
                    if sample_index == 0 {
                        ui.label(egui::RichText::new("默认声线").weak());
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("语言");
                    let mut current = sample.language.clone();
                    egui::ComboBox::from_id_salt(("voice_lang", voice_key.as_str(), sample_index))
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
                    if ui.button("选择…").clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter("音频", &["wav", "mp3", "m4a", "flac", "ogg"])
                            .pick_file()
                    {
                        sample.audio = config_audio_path(exe_dir, &path);
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
                            theme::AMBER,
                            format!("⚠ 音频文件不存在：{}", resolved.display()),
                        );
                    }
                }
            })
            .response
        });
    }

    if let Some((from, to)) = drag.finish(ui) {
        let sample = draft.samples.remove(from);
        draft.samples.insert(to, sample);
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
                ui.label(draft.label());
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
