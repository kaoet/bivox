//! 语音合成页：语音合成（TTS）相关的全部配置。
//!
//! 当前实现自定义音色的两栏布局：左侧音色列表、右侧选中音色的详情（详情为占位，
//! M3 再接入样本增删改与保存）。本页只读 `config.toml`，不访问百炼接口、
//! 不读写 `voice_cache.json`（D11）。

use eframe::egui;

use crate::app::{ConfigState, KaolsApp};

/// 左侧列表项（从配置做一份快照，避免界面渲染期间同时借用 `app` 的只读与可变部分）。
struct VoiceEntry {
    key: String,
    display: String,
    languages: String,
}

/// 右侧详情（同样是快照）。
struct VoiceDetail {
    key: String,
    display: String,
    languages: String,
    /// `(语言, 音频路径)`，顺序即配置里的顺序（第一个是默认声线）。
    samples: Vec<(String, String)>,
    /// 是否是 `[tts] voice` 当前引用的音色。
    is_active: bool,
    tts_model: String,
}

/// 本页需要的数据快照。
struct Snapshot {
    entries: Vec<VoiceEntry>,
    details: Vec<VoiceDetail>,
}

impl Snapshot {
    fn from_config(config: &crate::config::AppConfig) -> Self {
        let active = config.custom_voice_name().map(str::to_string);

        let mut details: Vec<VoiceDetail> = config
            .tts
            .custom_voices
            .iter()
            .map(|(key, voice)| {
                let languages = voice
                    .samples
                    .iter()
                    .map(|sample| sample.language.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let samples = voice
                    .samples
                    .iter()
                    .map(|sample| {
                        (
                            sample.language.clone(),
                            config.resolve_path(&sample.audio).display().to_string(),
                        )
                    })
                    .collect();

                VoiceDetail {
                    key: key.clone(),
                    display: voice.display_name(key),
                    languages,
                    samples,
                    is_active: active.as_deref() == Some(key.as_str()),
                    tts_model: config.tts.model.clone(),
                }
            })
            .collect();
        details.sort_by(|a, b| a.display.cmp(&b.display));

        let entries = details
            .iter()
            .map(|detail| VoiceEntry {
                key: detail.key.clone(),
                display: detail.display.clone(),
                languages: detail.languages.clone(),
            })
            .collect();

        Self { entries, details }
    }

    fn detail(&self, key: &str) -> Option<&VoiceDetail> {
        self.details.iter().find(|detail| detail.key == key)
    }
}

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.heading("语音合成");
    ui.label(
        egui::RichText::new(
            "这里配置语音合成：音色、模型、语言等。当前先实现自定义音色的浏览与选择，\
             编辑能力在 M3 接入。",
        )
        .weak(),
    );
    ui.add_space(6.0);

    let snapshot = match &app.config {
        ConfigState::Loaded(config) => Snapshot::from_config(config),
        ConfigState::Invalid { error } => {
            ui.colored_label(
                egui::Color32::from_rgb(0xe0, 0x4b, 0x4b),
                format!("配置无效，无法读取音色：{error}"),
            );
            return;
        }
    };

    // 选中的音色如果已不存在（配置被改/重新加载），清空选择，右侧自然空着。
    if let Some(selected) = &app.selected_voice {
        if snapshot.detail(selected).is_none() {
            app.selected_voice = None;
        }
    }

    let selected = app.selected_voice.clone();
    let mut clicked: Option<String> = None;

    // 左侧列表：`Panel::left` 会从当前 Ui 里切出左栏，画完后剩余的 Ui 就是右栏。
    egui::Panel::left("tts_voice_list_panel")
        .resizable(true)
        .default_size(300.0)
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("自定义音色").strong());
            ui.add_space(2.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .id_salt("tts_voice_list_scroll")
                .show(ui, |ui| {
                    if snapshot.entries.is_empty() {
                        ui.label(egui::RichText::new("配置里还没有自定义音色。").weak());
                    }
                    for entry in &snapshot.entries {
                        let is_selected = selected.as_deref() == Some(entry.key.as_str());
                        if voice_item(ui, entry, is_selected) {
                            clicked = Some(entry.key.clone());
                        }
                    }
                });
        });

    if let Some(key) = clicked {
        app.selected_voice = Some(key);
    }

    // 右侧详情：没选中就什么都不画。
    let current = app.selected_voice.clone();
    if let Some(detail) = current.as_deref().and_then(|key| snapshot.detail(key)) {
        show_detail(ui, detail);
    }
}

/// 一行音色列表项：第一行「名称 + weak 键名」，第二行 weak 语言列表；整行可点击。
fn voice_item(ui: &mut egui::Ui, entry: &VoiceEntry, selected: bool) -> bool {
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
        // 让整行撑满左栏宽度，选中高亮才不会只有文字那么宽。
        ui.set_width(ui.available_width());
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(&entry.display));
                ui.label(egui::RichText::new(&entry.key).weak());
            });
            ui.label(egui::RichText::new(&entry.languages).weak());
        });
    });

    ui.interact(
        inner.response.rect,
        ui.scope_id().with(("tts_voice_item", &entry.key)),
        egui::Sense::click(),
    )
    .clicked()
}

/// 右侧详情（占位内容，后续 M3 细化）。
fn show_detail(ui: &mut egui::Ui, detail: &VoiceDetail) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("tts_voice_detail_scroll")
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.heading(&detail.display);
                if detail.is_active {
                    ui.colored_label(egui::Color32::from_rgb(0x3c, 0xc9, 0x6f), "当前生效音色");
                }
            });
            ui.label(egui::RichText::new(format!("配置键名：{}", detail.key)).weak());
            ui.add_space(6.0);

            crate::gui::widgets::kv_row(ui, "支持语言", &detail.languages);
            crate::gui::widgets::kv_row(ui, "合成模型", &detail.tts_model);
            crate::gui::widgets::kv_row(ui, "样本数量", &format!("{} 个", detail.samples.len()));

            ui.add_space(8.0);
            ui.label(egui::RichText::new("声线样本（有序，第一个是默认声线）").strong());
            ui.add_space(2.0);
            for (index, (language, audio)) in detail.samples.iter().enumerate() {
                ui.monospace(format!("{}. {} → {}", index + 1, language, audio));
            }

            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(
                    "（占位详情：M3 会在这里接入样本增删改、语言选择与音频文件选择。）",
                )
                .weak(),
            );
        });
}
