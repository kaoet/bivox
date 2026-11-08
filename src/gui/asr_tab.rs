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
            // 语言列表靠拖动手柄排序：鼠标划过标签文字时不能进入“选中文字”状态，
            // 否则拖动会把文字选成蓝色并干扰拖拽（输入框不受这个开关影响）。
            ui.style_mut().interaction.selectable_labels = false;
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
             预编译热词会在下次会话启动时自动创建/更新到百炼，首次创建可能需要几十秒。",
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

/// 「语言列表」：有序、首个为默认；拖动左侧手柄排序，支持删除与追加。
fn show_languages(app: &mut KaolsApp, ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("语言列表（有序，首个为默认）").strong());
    let mut languages = app.asr_languages.clone();
    let count = languages.len();

    // 拖拽排序：与自定义音色样本编辑器一致——手柄按下时把下标存进 egui 临时数据，
    // 松手时按指针位置插入；拖动过程中在目标插入位置画一条指示线。
    let drag_id = ui.make_persistent_id("asr_language_drag");
    let mut drag_from = ui.data(|data| data.get_temp::<usize>(drag_id));
    let mut released = false;
    let mut block_rects: Vec<egui::Rect> = Vec::with_capacity(count);
    let mut remove: Option<usize> = None;

    for (index, code) in languages.iter().enumerate() {
        let mut block_rect: Option<egui::Rect> = None;
        ui.horizontal(|ui| {
            let row_left = ui.cursor().left();
            // 先给手柄列留出宽度，内容排版后再把手柄放到内容块垂直中心
            ui.add_space(18.0);
            let right = ui.horizontal(|ui| {
                ui.label(language_label(code));
                if index == 0 {
                    ui.label(egui::RichText::new("默认语言").weak());
                }
                // 只剩一种语言时禁止删除，避免空列表
                if count > 1 && ui.button("删除").clicked() {
                    remove = Some(index);
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
                .on_hover_text("拖动调整语言顺序");
            if handle.drag_started() {
                drag_from = Some(index);
                ui.data_mut(|data| data.insert_temp(drag_id, index));
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
                let len = languages.len();
                let mut to = insert_at;
                if from < to {
                    to -= 1;
                }
                let to = to.min(len.saturating_sub(1));
                if from < len && from != to {
                    let language = languages.remove(from);
                    languages.insert(to, language);
                }
                ui.data_mut(|data| data.remove_temp::<usize>(drag_id));
            }
        }
    }
    if let Some(index) = remove {
        if languages.len() > 1 && index < languages.len() {
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

/// 「预编译热词」：多行文本（一行一个）；标题右侧显示归一化后的条数 / 上限。
fn show_hotwords(app: &mut KaolsApp, ui: &mut egui::Ui) {
    let lines: Vec<String> = app.asr_hotwords.lines().map(str::to_string).collect();
    let count = normalize_hotwords(&lines).len();
    let over_limit = count > MAX_ASR_HOTWORDS;

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("预编译热词").strong());
        let counter = egui::RichText::new(format!("({count}/{MAX_ASR_HOTWORDS})")).small();
        if over_limit {
            ui.colored_label(RED, counter);
        } else {
            ui.label(counter.weak());
        }
    });
    ui.add(
        egui::TextEdit::multiline(&mut app.asr_hotwords)
            .desired_rows(6)
            .desired_width(f32::INFINITY)
            .hint_text("一行一个热词，例如：原神"),
    );
}
