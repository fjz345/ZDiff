use eframe::egui;
use zcommon_ui::table::Rows;
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

/// The running sum of `heights` for `row_at`: `heights.len() + 1` entries, the first 0, each the
/// bottom of the row before it.
pub fn row_tops(heights: &[f32]) -> Vec<f32> {
    let mut tops = Vec::with_capacity(heights.len() + 1);
    let mut top = 0.0;
    tops.push(top);
    for height in heights {
        top += height;
        tops.push(top);
    }
    tops
}

/// The row position (whole part is the row, fraction how far into it) at pixel `y` below the
/// first row's top. Clamped to `0..=row count`. `tops` is `row_tops` of the `Heights`; unused for
/// `Uniform`.
pub fn row_at(rows: &Rows<'_>, tops: &[f32], y: f32) -> f32 {
    match rows {
        Rows::Uniform { height, count } => {
            if *height > 0.0 {
                (y / height).clamp(0.0, *count as f32)
            } else {
                0.0
            }
        }
        Rows::Heights(heights) => {
            assert_eq!(
                tops.len(),
                heights.len() + 1,
                "tops is not row_tops(heights)"
            );
            let idx = tops[1..].partition_point(|bottom| *bottom <= y);
            let Some(height) = heights.get(idx) else {
                return heights.len() as f32;
            };
            let into = if *height > 0.0 {
                (y - tops[idx]) / height
            } else {
                0.0
            };
            idx as f32 + into.max(0.0)
        }
    }
}

/// `bin_changes` kept until the spans, the length or the bin count change.
#[derive(Default)]
pub struct BinCache {
    spans: Vec<ChangeSpan>,
    len: usize,
    bins: Vec<Option<ChangeKind>>,
    built: bool,
    #[cfg(test)]
    builds: usize,
}

impl BinCache {
    pub fn get(&mut self, spans: &[ChangeSpan], len: usize, bins: usize) -> &[Option<ChangeKind>] {
        if !(self.built && self.len == len && self.bins.len() == bins && self.spans == spans) {
            self.spans.clear();
            self.spans.extend_from_slice(spans);
            self.len = len;
            self.bins = bin_changes(spans, len, bins);
            self.built = true;
            #[cfg(test)]
            {
                self.builds += 1;
            }
        }
        &self.bins
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
    bin_cache: &mut BinCache,
    len: usize,
    viewport: (f32, f32),
    colors: &RulerColors,
) -> Option<usize> {
    let response = ui.interact(rect, id, egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);

    let bins = bin_cache.get(spans, len, rect.height().floor() as usize);
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
            egui::Stroke::new(1.0_f32, text.gamma_multiply(0.7)),
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

    fn linear_row_at(heights: &[f32], y: f32) -> f32 {
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

    #[test]
    fn uniform_rows_map_pixels_to_rows() {
        let rows = Rows::Uniform {
            height: 10.0,
            count: 5,
        };
        assert_eq!(row_at(&rows, &[], 0.0), 0.0);
        assert_eq!(row_at(&rows, &[], 25.0), 2.5);
        assert_eq!(row_at(&rows, &[], 999.0), 5.0);
        assert_eq!(row_at(&rows, &[], -4.0), 0.0);
    }

    #[test]
    fn tall_rows_map_pixels_to_rows_by_their_own_height() {
        let heights = [10.0, 30.0, 10.0];
        let rows = Rows::Heights(&heights);
        let tops = row_tops(&heights);
        assert_eq!(row_at(&rows, &tops, 5.0), 0.5);
        assert_eq!(row_at(&rows, &tops, 25.0), 1.5);
        assert_eq!(row_at(&rows, &tops, 45.0), 2.5);
        assert_eq!(row_at(&rows, &tops, 500.0), 3.0);
    }

    #[test]
    fn wrapped_row_at_agrees_with_the_linear_scan() {
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f32 / (1u64 << 53) as f32
        };
        for _ in 0..200 {
            let count = (next() * 40.0) as usize;
            let heights: Vec<f32> = (0..count)
                .map(|_| {
                    if next() < 0.1 {
                        0.0
                    } else {
                        1.0 + next() * 60.0
                    }
                })
                .collect();
            let tops = row_tops(&heights);
            let total = heights.iter().sum::<f32>();
            let rows = Rows::Heights(&heights);
            for _ in 0..50 {
                let y = (next() * 1.2 - 0.1) * total;
                assert_eq!(row_at(&rows, &tops, y), linear_row_at(&heights, y), "y {y}");
            }
            for &top in &tops {
                assert_eq!(row_at(&rows, &tops, top), linear_row_at(&heights, top));
            }
        }
    }

    #[test]
    fn unchanged_spans_and_bin_count_reuse_the_cached_bins() {
        let spans = [span(1, 2, ChangeKind::Deleted)];
        let mut cache = BinCache::default();
        assert_eq!(cache.get(&spans, 4, 8), bin_changes(&spans, 4, 8));
        assert_eq!(cache.builds, 1);
        cache.get(&spans, 4, 8);
        assert_eq!(cache.builds, 1);
    }

    #[test]
    fn changed_spans_length_or_bin_count_rebuild_the_bins() {
        let spans = [span(1, 2, ChangeKind::Deleted)];
        let other = [span(1, 2, ChangeKind::Inserted)];
        let mut cache = BinCache::default();
        cache.get(&spans, 4, 8);
        assert_eq!(cache.get(&other, 4, 8), bin_changes(&other, 4, 8));
        assert_eq!(cache.get(&other, 5, 8), bin_changes(&other, 5, 8));
        assert_eq!(cache.get(&other, 5, 4), bin_changes(&other, 5, 4));
        assert_eq!(cache.builds, 4);
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
