//! Two-sided table layout: left side, fixed-width center strip, right side. The apps draw the
//! cells; this module owns the column widths, the movable split and horizontal scrolling.

use eframe::egui::{self, UiBuilder, scroll_area::ScrollBarVisibility};
use egui_extras::{Column, TableBuilder, TableRow};

use super::h_scroll;

pub const MIN_SIDE_WIDTH: f32 = 100.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    #[default]
    Left,
    Right,
}

/// The left side's share of the width both sides split. Session-only: not persisted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Split {
    pub ratio: f32,
}

impl Default for Split {
    fn default() -> Self {
        Self { ratio: 0.5 }
    }
}

impl Split {
    pub fn widths(self, shared: f32) -> [f32; 2] {
        let left = clamp_left(shared * self.ratio, shared);
        [left, shared - left]
    }

    pub fn drag(&mut self, delta: f32, shared: f32) {
        if shared <= 0.0 {
            return;
        }
        let left = clamp_left(self.widths(shared)[0] + delta, shared);
        self.ratio = left / shared;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Below two minimum widths, both sides get half.
fn clamp_left(left: f32, shared: f32) -> f32 {
    let min = MIN_SIDE_WIDTH.min(shared * 0.5);
    left.clamp(min, shared - min)
}

/// The width both sides split: the table's width minus the center and the column spacing.
pub fn shared_width(total: f32, center: f32, spacing: f32) -> f32 {
    (total - center - 2.0 * spacing).max(0.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitColumn {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy)]
pub enum Rows<'a> {
    Uniform { height: f32, count: usize },
    Heights(&'a [f32]),
}

struct HScroll<'a> {
    offsets: &'a mut [f32; 2],
    linked: bool,
    content_widths: [f32; 2],
}

pub struct SplitTableOutput {
    /// Each side's body viewport, below the header.
    pub side_rects: [egui::Rect; 2],
    /// The body viewport across all three columns, and how far the rows are scrolled up in it.
    pub body_rect: egui::Rect,
    pub scroll_offset: f32,
}

pub struct SplitTable<'a> {
    id_salt: egui::Id,
    split: &'a mut Split,
    center_width: f32,
    h_scroll: Option<HScroll<'a>>,
    scroll_to_row: Option<(usize, Option<egui::Align>)>,
    scroll_offset: Option<f32>,
    striped: bool,
    selected_row: Option<usize>,
    single: bool,
}

impl<'a> SplitTable<'a> {
    pub fn new(id_salt: impl std::hash::Hash, split: &'a mut Split) -> Self {
        Self {
            id_salt: egui::Id::new(id_salt),
            split,
            center_width: 0.0,
            h_scroll: None,
            scroll_to_row: None,
            scroll_offset: None,
            striped: false,
            selected_row: None,
            single: false,
        }
    }

    /// One column over the full width: no center, no right side, no split. Cells and the header
    /// are asked for `SplitColumn::Left` only, and the right side rect is empty.
    pub fn single_column(mut self) -> Self {
        self.single = true;
        self
    }

    /// Fixed; measured by the app from the widest center content, not per frame.
    pub fn center_width(mut self, width: f32) -> Self {
        self.center_width = width;
        self
    }

    /// Scrolls each side's body sideways by `offsets`, with a bar under a side whose content is
    /// wider than it. Without this the sides don't scroll (e.g. wrapped text).
    pub fn h_scroll(
        mut self,
        offsets: &'a mut [f32; 2],
        linked: bool,
        content_widths: [f32; 2],
    ) -> Self {
        self.h_scroll = Some(HScroll {
            offsets,
            linked,
            content_widths,
        });
        self
    }

    pub fn scroll_to_row(mut self, row: usize, align: Option<egui::Align>) -> Self {
        self.scroll_to_row = Some((row, align));
        self
    }

    pub fn vertical_scroll_offset(mut self, offset: f32) -> Self {
        self.scroll_offset = Some(offset);
        self
    }

    pub fn striped(mut self, striped: bool) -> Self {
        self.striped = striped;
        self
    }

    pub fn selected_row(mut self, row: Option<usize>) -> Self {
        self.selected_row = row;
        self
    }

    pub fn show(
        self,
        ui: &mut egui::Ui,
        header_height: f32,
        mut header: impl FnMut(SplitColumn, &mut egui::Ui),
        rows: Rows<'_>,
        mut cell: impl FnMut(usize, SplitColumn, &mut egui::Ui),
    ) -> SplitTableOutput {
        let Self {
            id_salt,
            split,
            center_width,
            mut h_scroll,
            scroll_to_row,
            scroll_offset: set_scroll_offset,
            striped,
            selected_row,
            single,
        } = self;
        let spacing = ui.spacing().item_spacing.x;
        let area = ui.available_rect_before_wrap();
        let shared = shared_width(area.width(), center_width, spacing);
        let side_x = |widths: [f32; 2]| {
            let right_min = area.min.x + widths[0] + 2.0 * spacing + center_width;
            [
                egui::Rangef::new(area.min.x, area.min.x + widths[0]),
                egui::Rangef::new(right_min, right_min + widths[1]),
            ]
        };

        if single {
            // Linking would scroll a right side that has no content.
            if let Some(hs) = &mut h_scroll {
                hs.linked = false;
            }
        } else {
            let x = side_x(split.widths(shared));
            let strip_rect = egui::Rect::from_x_y_ranges(x[0].max..=x[1].min, area.y_range());
            // Registered before the cells, so the center's buttons are on top and keep their clicks.
            let strip = ui.interact(
                strip_rect,
                ui.id().with((id_salt, "split_strip")),
                egui::Sense::click_and_drag(),
            );
            if strip.hovered() || strip.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if strip.double_clicked() {
                split.reset();
            } else if strip.dragged() {
                split.drag(strip.drag_delta().x, shared);
            }
        }
        let (widths, x) = if single {
            let whole = egui::Rangef::new(area.min.x, area.max.x);
            let none = egui::Rangef::new(area.max.x, area.max.x);
            ([area.width(), 0.0], [whole, none])
        } else {
            let widths = split.widths(shared);
            (widths, side_x(widths))
        };

        let max = h_scroll.as_ref().map(|hs| {
            [
                h_scroll::max_offset(hs.content_widths[0], widths[0]),
                h_scroll::max_offset(hs.content_widths[1], widths[1]),
            ]
        });
        let bars = match (&h_scroll, max) {
            (Some(hs), Some(max)) => h_scroll::bar_ranges(max, hs.linked),
            _ => [None; 2],
        };
        let bar_height = if bars.iter().any(Option::is_some) {
            egui::style::ScrollStyle::solid().allocated_width()
        } else {
            0.0
        };

        if let (Some(hs), Some(max)) = (&mut h_scroll, max) {
            let mut offsets = h_scroll::clamp(*hs.offsets, max, hs.linked);
            let (wheel_delta, hover_pos) =
                ui.input(|i| (i.smooth_scroll_delta.x, i.pointer.hover_pos()));
            let body_y = egui::Rangef::new(area.min.y + header_height, area.max.y - bar_height);
            let wheel_side = hover_pos
                .filter(|pos| wheel_delta != 0.0 && body_y.contains(pos.y))
                .and_then(|pos| {
                    [Side::Left, Side::Right]
                        .into_iter()
                        .find(|&side| x[h_scroll::side_index(side)].contains(pos.x))
                });
            if let Some(side) = wheel_side {
                let offset = offsets[h_scroll::side_index(side)] - wheel_delta;
                offsets = h_scroll::set(offsets, side, offset, max, hs.linked);
            }
            *hs.offsets = offsets;
        }
        let offsets = h_scroll.as_ref().map_or([0.0; 2], |hs| *hs.offsets);

        // The bar is allocated after the table, so egui puts item spacing between them.
        let bar_gap = if bar_height > 0.0 {
            ui.spacing().item_spacing.y
        } else {
            0.0
        };
        let table_size = egui::vec2(
            area.width(),
            (area.height() - bar_height - bar_gap).max(0.0),
        );
        let (body_rect, scroll_offset) = ui
            .allocate_ui(table_size, |ui| {
                let mut table = TableBuilder::new(ui)
                    .id_salt(id_salt)
                    .striped(striped)
                    .resizable(false)
                    // The strip's drag moves the split; a body drag would take it over.
                    .drag_to_scroll(false)
                    .auto_shrink([false, false])
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::exact(widths[0]).clip(true));
                if !single {
                    table = table
                        .column(Column::exact(center_width).clip(true))
                        .column(Column::exact(widths[1]).clip(true));
                }
                if let Some((row, align)) = scroll_to_row {
                    table = table.scroll_to_row(row, align);
                }
                if let Some(offset) = set_scroll_offset {
                    table = table.vertical_scroll_offset(offset);
                }
                let mut add_row = |mut row: TableRow<'_, '_>| {
                    let i = row.index();
                    row.set_selected(selected_row == Some(i));
                    row.col(|ui| {
                        show_scrolled(ui, offsets[0], |ui| cell(i, SplitColumn::Left, ui))
                    });
                    if single {
                        return;
                    }
                    row.col(|ui| cell(i, SplitColumn::Center, ui));
                    row.col(|ui| {
                        show_scrolled(ui, offsets[1], |ui| cell(i, SplitColumn::Right, ui))
                    });
                };
                let output = table
                    .header(header_height, |mut row| {
                        let columns: &[SplitColumn] = if single {
                            &[SplitColumn::Left]
                        } else {
                            &[SplitColumn::Left, SplitColumn::Center, SplitColumn::Right]
                        };
                        for &column in columns {
                            row.col(|ui| header(column, ui));
                        }
                    })
                    .body(|body| match rows {
                        Rows::Uniform { height, count } => body.rows(height, count, &mut add_row),
                        Rows::Heights(heights) => {
                            body.heterogeneous_rows(heights.iter().copied(), &mut add_row)
                        }
                    });
                (output.inner_rect, output.state.offset.y)
            })
            .inner;
        let side_rects = x.map(|x| egui::Rect::from_x_y_ranges(x, body_rect.y_range()));

        if let (Some(hs), Some(max)) = (&mut h_scroll, max)
            && bar_height > 0.0
        {
            let bar_y = ui
                .allocate_space(egui::vec2(area.width(), bar_height))
                .1
                .y_range();
            let mut offsets = *hs.offsets;
            if hs.linked {
                let rect = egui::Rect::from_x_y_ranges(x[0].min..=x[1].max, bar_y);
                if let Some(range) = bars[0]
                    && let Some(offset) =
                        h_scroll_bar(ui, rect, (id_salt, "linked"), offsets[0], range)
                {
                    offsets = h_scroll::set(offsets, Side::Left, offset, max, true);
                }
            } else {
                for side in [Side::Left, Side::Right] {
                    let i = h_scroll::side_index(side);
                    let rect = egui::Rect::from_x_y_ranges(x[i], bar_y);
                    if let Some(range) = bars[i]
                        && let Some(offset) =
                            h_scroll_bar(ui, rect, (id_salt, i), offsets[i], range)
                    {
                        offsets = h_scroll::set(offsets, side, offset, max, false);
                    }
                }
            }
            if offsets != *hs.offsets {
                *hs.offsets = offsets;
                ui.ctx().request_repaint();
            }
        }

        SplitTableOutput {
            side_rects,
            body_rect,
            scroll_offset,
        }
    }
}

/// Draws `add_contents` scrolled left by `offset` and clipped to the cell. The cell counts as
/// used, not the content in it, so the column doesn't grow to the row's width.
pub fn show_scrolled(ui: &mut egui::Ui, offset: f32, add_contents: impl FnOnce(&mut egui::Ui)) {
    let cell = ui.max_rect();
    let shifted = egui::Rect::from_min_size(
        cell.min - egui::vec2(offset, 0.0),
        cell.size() + egui::vec2(offset, 0.0),
    );
    let mut child = ui.new_child(UiBuilder::new().max_rect(shifted));
    // Clipping also limits hit-testing, so shifted content can't take clicks from the center.
    // Vertically the cell's own clip reaches into the row gaps, so row fills have no gaps.
    let clip = ui.clip_rect();
    child.set_clip_rect(egui::Rect::from_x_y_ranges(
        cell.x_range().intersection(clip.x_range()),
        clip.y_range(),
    ));
    add_contents(&mut child);
    ui.advance_cursor_after_rect(cell);
}

/// Returns the offset when the bar moved it.
fn h_scroll_bar(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id_salt: impl std::hash::Hash,
    offset: f32,
    range: f32,
) -> Option<f32> {
    if !rect.is_positive() {
        return None;
    }
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
    child.spacing_mut().scroll = egui::style::ScrollStyle::solid();
    let output = egui::ScrollArea::horizontal()
        .id_salt(("split_table_h_scroll_bar", id_salt))
        .auto_shrink([false, true])
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
        .scroll_offset(egui::vec2(offset, 0.0))
        .show(&mut child, |ui| {
            ui.allocate_space(egui::vec2(rect.width() + range, 0.0));
        });
    let moved = output.state.offset.x;
    (moved != offset).then_some(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_width_leaves_out_the_center_and_both_gaps() {
        assert_eq!(shared_width(1000.0, 20.0, 4.0), 972.0);
        assert_eq!(shared_width(10.0, 20.0, 4.0), 0.0);
    }

    #[test]
    fn a_new_split_is_in_the_middle() {
        assert_eq!(Split::default().widths(980.0), [490.0, 490.0]);
    }

    #[test]
    fn widths_follow_the_ratio() {
        assert_eq!(Split { ratio: 0.25 }.widths(1000.0), [250.0, 750.0]);
        assert_eq!(Split { ratio: 0.75 }.widths(1000.0), [750.0, 250.0]);
    }

    #[test]
    fn each_side_keeps_its_minimum_width() {
        assert_eq!(Split { ratio: 0.01 }.widths(1000.0), [100.0, 900.0]);
        assert_eq!(Split { ratio: 0.0 }.widths(1000.0), [100.0, 900.0]);
        assert_eq!(Split { ratio: 0.99 }.widths(1000.0), [900.0, 100.0]);
        assert_eq!(Split { ratio: 1.0 }.widths(1000.0), [900.0, 100.0]);
    }

    #[test]
    fn too_narrow_for_both_minimums_splits_evenly() {
        assert_eq!(Split { ratio: 0.25 }.widths(150.0), [75.0, 75.0]);
        assert_eq!(Split { ratio: 0.25 }.widths(0.0), [0.0, 0.0]);
    }

    #[test]
    fn dragging_moves_the_left_width_by_the_delta() {
        let mut split = Split::default();
        split.drag(50.0, 1000.0);
        assert_eq!(split.widths(1000.0), [550.0, 450.0]);
        split.drag(-200.0, 1000.0);
        assert_eq!(split.widths(1000.0), [350.0, 650.0]);
    }

    #[test]
    fn dragging_past_a_minimum_stops_at_it() {
        let mut split = Split::default();
        split.drag(-1000.0, 1000.0);
        assert_eq!(split.ratio, 0.1);
        assert_eq!(split.widths(1000.0), [100.0, 900.0]);
        split.drag(5000.0, 1000.0);
        assert_eq!(split.widths(1000.0), [900.0, 100.0]);
    }

    #[test]
    fn dragging_with_no_width_keeps_the_ratio() {
        let mut split = Split { ratio: 0.25 };
        split.drag(30.0, 0.0);
        assert_eq!(split, Split { ratio: 0.25 });
    }

    #[test]
    fn reset_returns_to_the_middle() {
        let mut split = Split::default();
        split.drag(120.0, 1000.0);
        split.reset();
        assert_eq!(split, Split::default());
    }

    #[test]
    fn a_resize_keeps_the_ratio() {
        let mut split = Split::default();
        split.drag(250.0, 1000.0);
        assert_eq!(split.widths(2000.0), [1500.0, 500.0]);
        assert_eq!(split.widths(300.0), [200.0, 100.0]);
        assert_eq!(split.widths(1000.0), [750.0, 250.0]);
    }

    fn area_and_used_height(content_width: f32) -> (f32, f32) {
        let ctx = egui::Context::default();
        let mut split = Split::default();
        let mut offsets = [0.0; 2];
        let mut heights = (0.0, 0.0);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let area = ui.available_height();
                let top = ui.cursor().top();
                SplitTable::new("table", &mut split)
                    .center_width(20.0)
                    .h_scroll(&mut offsets, false, [content_width; 2])
                    .show(
                        ui,
                        20.0,
                        |_, _| {},
                        Rows::Uniform {
                            height: 16.0,
                            count: 1000,
                        },
                        |_, _, _| {},
                    );
                heights = (area, ui.min_rect().bottom() - top);
            });
        });
        heights
    }

    #[test]
    fn the_table_fills_its_area_exactly_without_scroll_bars() {
        let (area, used) = area_and_used_height(10.0);
        assert_eq!(used, area);
    }

    #[test]
    fn the_table_fills_its_area_exactly_with_scroll_bars() {
        let (area, used) = area_and_used_height(10_000.0);
        assert_eq!(used, area);
    }

    #[test]
    fn a_single_column_table_asks_for_the_left_cell_only_over_the_whole_width() {
        let ctx = egui::Context::default();
        let mut split = Split::default();
        let mut offsets = [0.0; 2];
        let mut columns = Vec::new();
        let mut header_columns = Vec::new();
        let mut output = None;
        let mut area_width = 0.0;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                area_width = ui.available_width();
                output = Some(
                    SplitTable::new("table", &mut split)
                        .single_column()
                        .h_scroll(&mut offsets, true, [10_000.0; 2])
                        .show(
                            ui,
                            20.0,
                            |column, _| header_columns.push(column),
                            Rows::Uniform {
                                height: 16.0,
                                count: 3,
                            },
                            |_, column, _| columns.push(column),
                        ),
                );
            });
        });
        let output = output.unwrap();
        assert_eq!(header_columns, [SplitColumn::Left]);
        assert!(!columns.is_empty());
        assert!(columns.iter().all(|&column| column == SplitColumn::Left));
        assert_eq!(output.side_rects[0].width(), area_width);
        assert!(!output.side_rects[1].is_positive());
        assert_eq!(offsets[1], 0.0);
    }
}
