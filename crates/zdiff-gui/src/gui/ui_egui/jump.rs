use std::ops::Range;

use eframe::egui;

use crate::diff_ctx::ScrollSpan;

pub fn span_rows(span: &ScrollSpan) -> Range<usize> {
    span.start..span.maybe_end.unwrap_or(span.start) + 1
}

pub fn jump_target(
    rows: Range<usize>,
    row_height: impl Fn(usize) -> f32,
    viewport: f32,
    center: bool,
) -> (usize, egui::Align) {
    let top = (rows.start, egui::Align::Min);
    if !center {
        return top;
    }
    let span_height: f32 = rows.clone().map(&row_height).sum();
    if span_height > viewport {
        return top;
    }
    let mut bottom = 0.0;
    rows.into_iter()
        .find(|&row| {
            bottom += row_height(row);
            bottom >= span_height / 2.0
        })
        .map_or(top, |row| (row, egui::Align::Center))
}

pub fn jump_offset(rows: Range<usize>, row_height: f32, viewport: f32, center: bool) -> f32 {
    let top = rows.start as f32 * row_height;
    let span_height = rows.len() as f32 * row_height;
    if !center || span_height > viewport {
        return top;
    }
    (top + (span_height - viewport) / 2.0).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Align;

    #[test]
    fn a_row_lands_on_top_without_centering() {
        assert_eq!(
            jump_target(40..41, |_| 15.0, 300.0, false),
            (40, Align::Min)
        );
        assert_eq!(
            jump_target(40..44, |_| 15.0, 300.0, false),
            (40, Align::Min)
        );
        assert_eq!(jump_offset(40..44, 15.0, 300.0, false), 600.0);
    }

    #[test]
    fn a_single_row_is_centered() {
        assert_eq!(
            jump_target(40..41, |_| 15.0, 300.0, true),
            (40, Align::Center)
        );
        assert_eq!(
            jump_offset(40..41, 15.0, 300.0, true),
            40.0 * 15.0 + 7.5 - 150.0
        );
    }

    #[test]
    fn a_span_is_centered_on_its_middle_row() {
        assert_eq!(
            jump_target(40..43, |_| 15.0, 300.0, true),
            (41, Align::Center)
        );
        assert_eq!(
            jump_target(40..42, |_| 15.0, 300.0, true),
            (40, Align::Center)
        );
        assert_eq!(jump_offset(40..42, 15.0, 300.0, true), 41.0 * 15.0 - 150.0);
    }

    #[test]
    fn a_span_with_wrapped_rows_is_centered_on_the_row_holding_its_middle() {
        let heights = [15.0, 15.0, 90.0, 15.0];
        assert_eq!(
            jump_target(0..4, |row| heights[row], 300.0, true),
            (2, Align::Center)
        );
    }

    #[test]
    fn a_span_taller_than_the_view_starts_at_the_top() {
        assert_eq!(jump_target(40..61, |_| 15.0, 300.0, true), (40, Align::Min));
        assert_eq!(jump_offset(40..61, 15.0, 300.0, true), 600.0);
        let heights = [15.0, 200.0, 200.0];
        assert_eq!(
            jump_target(0..3, |row| heights[row], 300.0, true),
            (0, Align::Min)
        );
    }

    #[test]
    fn a_span_that_fills_the_view_exactly_is_still_centered() {
        assert_eq!(
            jump_target(40..60, |_| 15.0, 300.0, true),
            (49, Align::Center)
        );
        assert_eq!(jump_offset(40..60, 15.0, 300.0, true), 600.0);
    }

    #[test]
    fn centering_a_row_near_the_start_does_not_scroll_above_it() {
        assert_eq!(jump_offset(2..3, 15.0, 300.0, true), 0.0);
    }

    #[test]
    fn a_span_covers_its_inclusive_end() {
        let span = |start, maybe_end| ScrollSpan {
            start,
            maybe_end,
            side: None,
        };
        assert_eq!(span_rows(&span(5, None)), 5..6);
        assert_eq!(span_rows(&span(5, Some(8))), 5..9);
    }
}
