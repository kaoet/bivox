//! 拖动排序：行首「≡」手柄 + 目标插入位置的指示线。
//!
//! 语言列表（`language_list`）与自定义音色样本（`tts_voice_editor`）共用这套交互。
//! 本模块不持有列表数据：每行 `row()` 画手柄与内容，循环结束后 `finish()` 返回
//! 要移动的 `(from, to)`，由调用方在自己的 `Vec` 上做 `remove` + `insert`
//! （避免同时借用 `BivoxApp`）。

use eframe::egui;

/// 手柄列宽度：先留出这段空白，内容排版后再把手柄放到内容块的垂直中心。
const HANDLE_COLUMN_WIDTH: f32 = 18.0;
/// 手柄的拖动区域（比字形大一圈，便于抓住）。
const HANDLE_SIZE: egui::Vec2 = egui::vec2(16.0, 20.0);

/// 一次列表拖动排序的状态。
pub(crate) struct DragReorder {
    /// egui 临时数据里的键：拖动中的起止行跨帧保存，否则松手那一帧拿不到 `from`。
    id: egui::Id,
    from: Option<usize>,
    released: bool,
    /// 每行内容块的矩形：决定指示线位置与「插入到第几行」。
    rects: Vec<egui::Rect>,
}

impl DragReorder {
    /// `id_source` 在同一次绘制里唯一即可（与其它控件的 id 不冲突）。
    pub(crate) fn new(ui: &egui::Ui, id_source: impl std::hash::Hash + std::fmt::Debug) -> Self {
        let id = ui.make_persistent_id(("gui_drag_reorder", id_source));
        Self {
            id,
            from: ui.data(|data| data.get_temp::<usize>(id)),
            released: false,
            rects: Vec::new(),
        }
    }

    /// 画一行：`add_contents` 只负责右侧内容，左侧自动放「≡」手柄。
    pub(crate) fn row(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        hover_text: &str,
        add_contents: impl FnOnce(&mut egui::Ui) -> egui::Response,
    ) {
        ui.horizontal(|ui| {
            let row_left = ui.cursor().left();
            ui.add_space(HANDLE_COLUMN_WIDTH);
            let block_rect = add_contents(ui).rect;

            let handle_rect = egui::Rect::from_center_size(
                egui::pos2(row_left + 8.0, block_rect.center().y),
                HANDLE_SIZE,
            );
            let handle = ui
                .put(
                    handle_rect,
                    egui::Label::new(egui::RichText::new("≡").weak()).sense(egui::Sense::drag()),
                )
                .on_hover_cursor(egui::CursorIcon::Grab)
                .on_hover_text(hover_text);
            if handle.drag_started() {
                self.from = Some(index);
                ui.data_mut(|data| data.insert_temp(self.id, index));
            }
            if handle.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            if handle.drag_stopped() {
                self.released = true;
                ui.data_mut(|data| data.remove_temp::<usize>(self.id));
            }
            self.rects.push(block_rect);
        });
    }

    /// 画插入位置指示线；松手当帧返回 `(from, to)`，位置没变则返回 `None`。
    ///
    /// 指示线画在目标行的**边界**上（而不是行内部），避免看起来像"插入到行中间"。
    pub(crate) fn finish(self, ui: &mut egui::Ui) -> Option<(usize, usize)> {
        let from = self.from?;
        let pointer = ui.ctx().pointer_latest_pos()?;

        let insert_at = self
            .rects
            .iter()
            .position(|rect| pointer.y < rect.center().y)
            .unwrap_or(self.rects.len());
        let stroke = egui::Stroke::new(2.0, ui.visuals().selection.stroke.color);
        let y = self
            .rects
            .get(insert_at)
            .map(|rect| rect.top())
            .or_else(|| self.rects.last().map(|rect| rect.bottom()));
        if let Some(y) = y {
            let x = self
                .rects
                .first()
                .map(|rect| egui::Rangef::new(rect.left(), ui.max_rect().right()))
                .unwrap_or(egui::Rangef::new(0.0, ui.max_rect().right()));
            ui.painter().hline(x, y, stroke);
        }

        if !self.released {
            return None;
        }
        ui.data_mut(|data| data.remove_temp::<usize>(self.id));

        // `insert_at` 是「插到第几个位置」，转成下标：向下移要减一，末尾要夹到 len-1。
        let len = self.rects.len();
        let mut to = insert_at;
        if from < to {
            to -= 1;
        }
        let to = to.min(len.saturating_sub(1));
        (from < len && from != to).then_some((from, to))
    }
}
