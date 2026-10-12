use eframe::egui;
use zcommon::ui_egui::table::Rows;
use zdiff::{
    diff_builder::{DiffRow, LineContent},
    diff_ir::DiffOp,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    Inserted,
    Deleted,
    Changed,
}

/// `start..end` over the overview's length. A later kind in the order outranks an earlier one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeSpan {
    pub start: usize,
    pub end: usize,
    pub kind: ChangeKind,
}

/// The strongest change kind of each of `bins` equal parts of `0..len`. A span marks every bin
/// it overlaps, so a span smaller than a bin still shows.
pub fn bin_changes(spans: &[ChangeSpan], len: usize, bins: usize) -> Vec<Option<ChangeKind>> {
    let mut out = vec![None; bins];
    if len == 0 || bins == 0 {
        return out;
    }
    for span in spans {
        let end = span.end.min(len);
        if span.start >= end {
            continue;
        }
        let first = span.start * bins / len;
        let last = (end * bins).div_ceil(len).min(bins);
        for bin in &mut out[first..last] {
            *bin = (*bin).max(Some(span.kind));
        }
    }
    out
}

fn side_changed(content: &LineContent) -> bool {
    match content {
        LineContent::Code { tokens, .. } => tokens.iter().any(|(res, _, ghost)| {
            !ghost && !res.hide_in_diff && !matches!(res.operation, DiffOp::Equal(_))
        }),
        _ => false,
    }
}

pub fn row_change_spans(rows: &[DiffRow]) -> Vec<ChangeSpan> {
    let mut spans: Vec<ChangeSpan> = Vec::new();
    for (idx, row) in rows.iter().enumerate() {
        let kind = match (side_changed(&row.left), side_changed(&row.right)) {
            (false, false) => continue,
            (true, false) => ChangeKind::Deleted,
            (false, true) => ChangeKind::Inserted,
            (true, true) => ChangeKind::Changed,
        };
        match spans.last_mut() {
            Some(last) if last.end == idx && last.kind == kind => last.end = idx + 1,
            _ => spans.push(ChangeSpan {
                start: idx,
                end: idx + 1,
                kind,
            }),
        }
    }
    spans
}

/// The row position (whole part is the row, fraction how far into it) at pixel `y` below the
/// first row's top. Clamped to `0..=row count`.
pub fn row_at(rows: &Rows<'_>, y: f32) -> f32 {
    match rows {
        Rows::Uniform { height, count } => {
            if *height > 0.0 {
                (y / height).clamp(0.0, *count as f32)
            } else {
                0.0
            }
        }
        Rows::Heights(heights) => {
            let mut top = 0.0;
            for (idx, height) in heights.iter().enumerate() {
                if y < top + height {
                    let into = if *height > 0.0 {
                        (y - top) / height
                    } else {
                        0.0
                    };
                    return idx as f32 + into.max(0.0);
                }
                top += height;
            }
            heights.len() as f32
        }
    }
}

/// The row at `fraction` (0 top, 1 bottom) of an overview of `len` rows.
pub fn row_at_fraction(fraction: f32, len: usize) -> usize {
    ((fraction.clamp(0.0, 1.0) * len as f32) as usize).min(len.saturating_sub(1))
}

pub const RULER_WIDTH: f32 = 14.0;

pub struct RulerColors {
    pub inserted: egui::Color32,
    pub deleted: egui::Color32,
    pub changed: egui::Color32,
}

impl RulerColors {
    fn of(&self, kind: ChangeKind) -> egui::Color32 {
        match kind {
            ChangeKind::Inserted => self.inserted,
            ChangeKind::Deleted => self.deleted,
            ChangeKind::Changed => self.changed,
        }
    }
}

/// Draws the change bins of `len` rows in `rect` with the rows `viewport` (top, bottom as row
/// positions) marked. Returns the row under a press or drag.
pub fn show(
    ui: &mut egui::Ui,
    id: egui::Id,
    rect: egui::Rect,
    spans: &[ChangeSpan],
    len: usize,
    viewport: (f32, f32),
    colors: &RulerColors,
) -> Option<usize> {
    let response = ui.interact(rect, id, egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);

    let bins = bin_changes(spans, len, rect.height().floor() as usize);
    let mut bin = 0;
    while bin < bins.len() {
        let Some(kind) = bins[bin] else {
            bin += 1;
            continue;
        };
        let run = bins[bin..].iter().take_while(|k| **k == Some(kind)).count();
        let band = egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.top() + bin as f32),
            egui::pos2(rect.right(), rect.top() + (bin + run) as f32),
        );
        painter.rect_filled(band, 0.0, colors.of(kind));
        bin += run;
    }

    if len > 0 {
        let y = |row: f32| rect.top() + row / len as f32 * rect.height();
        let top = y(viewport.0);
        let bottom = y(viewport.1).max(top + 3.0).min(rect.bottom());
        let mark = egui::Rect::from_min_max(
            egui::pos2(rect.left(), top.min(bottom - 3.0)),
            egui::pos2(rect.right(), bottom),
        );
        let text = ui.visuals().text_color();
        painter.rect_filled(mark, 0.0, text.gamma_multiply(0.18));
        painter.rect_stroke(
            mark,
            0.0,
            egui::Stroke::new(1.0, text.gamma_multiply(0.7)),
            egui::StrokeKind::Inside,
        );
    }

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if !(response.is_pointer_button_down_on() || response.clicked()) || rect.height() <= 0.0 {
        return None;
    }
    let pos = response.interact_pointer_pos()?;
    Some(row_at_fraction((pos.y - rect.top()) / rect.height(), len)).filter(|_| len > 0)
}

#[cfg(test)]
mod tests {
    use zdiff::diff_builder::DiffBuilderOptions;

    use super::*;
    use crate::ui_egui::copy_harness::CopyHarness;

    fn span(start: usize, end: usize, kind: ChangeKind) -> ChangeSpan {
        ChangeSpan { start, end, kind }
    }

    fn spans_of(source: &str, target: &str) -> Vec<ChangeSpan> {
        let harness = CopyHarness::new(source, target, &DiffBuilderOptions::default());
        row_change_spans(harness.rows())
    }

    #[test]
    fn more_bins_than_lines_gives_each_line_its_share_of_bins() {
        let bins = bin_changes(&[span(1, 2, ChangeKind::Deleted)], 4, 8);
        let mut expected = vec![None; 8];
        expected[2] = Some(ChangeKind::Deleted);
        expected[3] = Some(ChangeKind::Deleted);
        assert_eq!(bins, expected);
    }

    #[test]
    fn more_lines_than_bins_marks_every_bin_a_span_overlaps() {
        let bins = bin_changes(&[span(2, 7, ChangeKind::Inserted)], 12, 4);
        assert_eq!(
            bins,
            [
                Some(ChangeKind::Inserted),
                Some(ChangeKind::Inserted),
                Some(ChangeKind::Inserted),
                None
            ]
        );
    }

    #[test]
    fn a_span_inside_one_bin_still_marks_it() {
        let bins = bin_changes(&[span(5, 6, ChangeKind::Changed)], 100, 2);
        assert_eq!(bins, [Some(ChangeKind::Changed), None]);
    }

    #[test]
    fn the_strongest_kind_wins_a_bin() {
        let spans = [
            span(0, 1, ChangeKind::Inserted),
            span(1, 2, ChangeKind::Changed),
            span(2, 3, ChangeKind::Deleted),
            span(10, 11, ChangeKind::Deleted),
            span(11, 12, ChangeKind::Inserted),
        ];
        let bins = bin_changes(&spans, 12, 2);
        assert_eq!(bins, [Some(ChangeKind::Changed), Some(ChangeKind::Deleted)]);
    }

    #[test]
    fn nothing_to_bin_gives_empty_bins() {
        assert_eq!(bin_changes(&[], 10, 3), [None, None, None]);
        assert_eq!(
            bin_changes(&[span(0, 1, ChangeKind::Changed)], 0, 3),
            [None; 3]
        );
        assert!(bin_changes(&[span(0, 1, ChangeKind::Changed)], 5, 0).is_empty());
    }

    #[test]
    fn a_span_past_the_length_is_clamped() {
        let bins = bin_changes(&[span(3, 99, ChangeKind::Deleted)], 4, 4);
        assert_eq!(bins, [None, None, None, Some(ChangeKind::Deleted)]);
    }

    #[test]
    fn a_removed_line_is_a_deleted_span() {
        assert_eq!(
            spans_of("a\nb\nc\n", "a\nc\n"),
            [span(1, 2, ChangeKind::Deleted)]
        );
    }

    #[test]
    fn an_added_line_is_an_inserted_span() {
        assert_eq!(
            spans_of("a\nc\n", "a\nb\nc\n"),
            [span(1, 2, ChangeKind::Inserted)]
        );
    }

    #[test]
    fn an_edited_line_is_a_changed_span() {
        assert_eq!(
            spans_of("a\nb\nc\n", "a\nX\nc\n"),
            [span(1, 2, ChangeKind::Changed)]
        );
    }

    #[test]
    fn adjacent_rows_of_one_kind_join_into_one_span() {
        assert_eq!(
            spans_of("a\nb\nc\nd\n", "a\nd\n"),
            [span(1, 3, ChangeKind::Deleted)]
        );
    }

    #[test]
    fn identical_files_have_no_spans() {
        assert!(spans_of("a\nb\n", "a\nb\n").is_empty());
    }

    #[test]
    fn uniform_rows_map_pixels_to_rows() {
        let rows = Rows::Uniform {
            height: 10.0,
            count: 5,
        };
        assert_eq!(row_at(&rows, 0.0), 0.0);
        assert_eq!(row_at(&rows, 25.0), 2.5);
        assert_eq!(row_at(&rows, 999.0), 5.0);
        assert_eq!(row_at(&rows, -4.0), 0.0);
    }

    #[test]
    fn tall_rows_map_pixels_to_rows_by_their_own_height() {
        let heights = [10.0, 30.0, 10.0];
        let rows = Rows::Heights(&heights);
        assert_eq!(row_at(&rows, 5.0), 0.5);
        assert_eq!(row_at(&rows, 25.0), 1.5);
        assert_eq!(row_at(&rows, 45.0), 2.5);
        assert_eq!(row_at(&rows, 500.0), 3.0);
    }

    #[test]
    fn a_click_fraction_picks_a_row_inside_the_file() {
        assert_eq!(row_at_fraction(0.0, 10), 0);
        assert_eq!(row_at_fraction(0.55, 10), 5);
        assert_eq!(row_at_fraction(1.0, 10), 9);
        assert_eq!(row_at_fraction(-1.0, 10), 0);
        assert_eq!(row_at_fraction(0.5, 0), 0);
    }
}
