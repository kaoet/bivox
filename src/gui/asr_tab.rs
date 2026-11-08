//! 语音识别页：识别模型、有序语言列表与预编译热词（D22/D31/D32，见 docs/decisions.md）。
//!
//! 本页是三组 `[asr]` 字段的草稿编辑区：`model` / `languages` / `hotwords`。
//! 改动只写本页草稿，点右上角「保存」后由 `KaolsApp::save_asr_tab` 合并成一次
//! `toml_edit` 写回（一次校验 + 一份 `.bak`）；「放弃更改」恢复配置文件里的值。
//! 运行中保存会按 D20 重启实时管线、丢失当前句；热词列表在下一次会话启动时
//! 自动创建/更新为百炼预编译热词列表（D31）。
//!
//! 布局只用 `ui.horizontal` 与 `ScrollArea`，不用 [`egui::Grid`]：Grid 会触发
//! multi-pass，而 eframe 的 `__screenshot` 调试特性会抓到空帧。

use eframe::egui;

use crate::app::{ConfigState, KaolsApp};
use crate::config::{normalize_hotwords, DEFAULT_ASR_MODEL, MAX_ASR_HOTWORDS};
use crate::language::{language_display_name, ASR_LANGUAGES};

const RED: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x4b, 0x4b);
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xd8, 0x9a, 0x1b);
const ORANGE: egui::Color32 = egui::Color32::from_rgb(0xe6, 0x82, 0x00);

/// 语言列表里的显示文本：中文名 + 代码；未知代码由 `language_display_name` 原样返回。
fn language_label(code: &str) -> String {
    format!("{}（{code}）", language_display_name(code))
}

pub fn show(app: &mut KaolsApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("asr_tab_scroll")
        .show(ui, |ui| {
            show_content(app, ui);
        });
}

fn show_content(app: &mut KaolsApp, ui: &mut egui::Ui) {
    // Tab 级操作：保存 / 放弃更改 作用于「模型 + 语言列表 + 热词」全部改动
    let mut save = false;
    let mut reset = false;
    ui.horizontal(|ui| {
        ui.heading("语音识别");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let dirty = app.asr_tab_dirty();
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
        app.save_asr_tab();
    }
    if reset {
        app.reset_asr_tab();
    }
    if let Some(error) = &app.asr_edit_error {
        ui.colored_label(RED, format!("保存失败：{error}"));
    }
    ui.label(
        egui::RichText::new(
            "本页改动点右上角「保存」一次写回 [asr] 的 model / languages / hotwords；\
             运行中保存会自动重启实时管线并丢失当前句；预编译热词会在下次会话启动时\
             自动创建/更新到百炼，首次创建可能需要几十秒。",
        )
        .weak(),
    );

    if let ConfigState::Invalid { error } = &app.config {
        ui.add_space(6.0);
        ui.colored_label(
            RED,
            format!("配置无效，无法保存语音识别设置：{error}；请先到「设置」页修正配置。"),
        );
        return;
    }

    ui.add_space(6.0);
    if app.session.is_some() {
        ui.colored_label(AMBER, "⚠ 运行中保存会自动重启实时管线，当前句会丢失。");
    }

    ui.add_space(8.0);
    show_model(app, ui);
    ui.add_space(10.0);
    show_languages(app, ui);
    ui.add_space(10.0);
    show_hotwords(app, ui);
    ui.add_space(8.0);
}

/// 「识别模型」：单行输入框 + 恢复默认。
fn show_model(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label("识别模型");
        ui.add(egui::TextEdit::singleline(&mut app.asr_model).desired_width(380.0));
        if ui.button("恢复默认").clicked() {
            app.asr_model = DEFAULT_ASR_MODEL.to_string();
        }
    });
    ui.label(
        egui::RichText::new(
            "模型需与百炼支持的实时识别模型一致；更换模型会重建对应的预编译热词列表。",
        )
        .weak(),
    );
}

/// 「语言列表」：有序、首个为默认；支持上移/下移/删除与追加。
fn show_languages(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("语言列表（有序，首个为默认）").strong());
    let mut languages = app.asr_languages.clone();

    let mut move_up: Option<usize> = None;
    let mut move_down: Option<usize> = None;
    let mut remove: Option<usize> = None;
    for (index, code) in languages.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(format!("{}.", index + 1));
            ui.label(language_label(code));
            // 首个不能上移、末个不能下移；只剩一种语言时禁止删除，避免空列表
            if ui.add_enabled(index > 0, egui::Button::new("↑")).clicked() {
                move_up = Some(index);
            }
            if ui
                .add_enabled(index + 1 < languages.len(), egui::Button::new("↓"))
                .clicked()
            {
                move_down = Some(index);
            }
            if ui
                .add_enabled(languages.len() > 1, egui::Button::new("删除"))
                .clicked()
            {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = move_up {
        if index > 0 {
            languages.swap(index, index - 1);
        }
    }
    if let Some(index) = move_down {
        if index + 1 < languages.len() {
            languages.swap(index, index + 1);
        }
    }
    if let Some(index) = remove {
        if languages.len() > 1 {
            languages.remove(index);
        }
    }

    // 只列出尚未加入的语言；选中即追加到列表末尾
    let mut added: Option<String> = None;
    ui.horizontal(|ui| {
        ui.label("添加语言");
        egui::ComboBox::from_id_salt("asr_add_language")
            .selected_text("选择要添加的语言…")
            .width(200.0)
            .show_ui(ui, |ui| {
                let mut any = false;
                for code in ASR_LANGUAGES.iter().copied() {
                    if languages.iter().any(|lang| lang.as_str() == code) {
                        continue;
                    }
                    any = true;
                    if ui.selectable_label(false, language_label(code)).clicked() {
                        added = Some(code.to_string());
                    }
                }
                if !any {
                    ui.label(egui::RichText::new("已加入全部支持的语言").weak());
                }
            });
    });
    if let Some(code) = added {
        languages.push(code);
    }

    if languages.len() > 4 {
        ui.colored_label(
            ORANGE,
            format!(
                "⚠ 当前 {} 种语言：ASR 接口只使用前 4 个作为 language_hints，\
                 本地语言判定仍使用完整列表。",
                languages.len()
            ),
        );
    }

    app.asr_languages = languages;
}

/// 「预编译热词」：多行文本（一行一个）+ 归一化条数与上限提示。
fn show_hotwords(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("预编译热词").strong());
    ui.add(
        egui::TextEdit::multiline(&mut app.asr_hotwords)
            .desired_rows(6)
            .desired_width(f32::INFINITY)
            .hint_text("一行一个热词，例如：原神"),
    );

    let lines: Vec<String> = app.asr_hotwords.lines().map(str::to_string).collect();
    let hotwords = normalize_hotwords(&lines);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!(
                "归一化后 {} 条（空行与重复项自动忽略）",
                hotwords.len()
            ))
            .weak(),
        );
        if hotwords.len() > MAX_ASR_HOTWORDS {
            ui.colored_label(RED, format!("超过上限 {MAX_ASR_HOTWORDS} 条，保存会被拒绝"));
        }
    });
    ui.label(
        egui::RichText::new(
            "热词权重固定为 50（百炼超级热词，实测召回更好）、不限定语种；仅主业务空间支持。\
             因为每个词表最多 50 个超级热词，本页最多 50 条。单条限制：含非 ASCII 字符时\
             最多 15 个字符（空格计入），纯 ASCII 时最多 7 个空格分隔片段。修改热词后\
             建议间隔 ≥5 分钟再改（百炼限制）。",
        )
        .weak(),
    );
}
