use super::*;
use std::sync::Weak;

use crate::diff_ctx::STAGE_CACHE_ENTRIES;

/// Results per rows for as many rows as the stage caches keep, so toggling back to cached rows
/// measures nothing.
pub(super) struct RowsMemo<T> {
    // `Weak` frees the rows but keeps their address from matching a new allocation.
    entries: Vec<(Weak<DiffRows>, T)>,
}

impl<T> Default for RowsMemo<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<T> RowsMemo<T> {
    pub(super) fn get_or_insert_with(
        &mut self,
        rows: &Arc<DiffRows>,
        valid: impl Fn(&T) -> bool,
        make: impl FnOnce() -> T,
    ) -> &mut T {
        self.entries.retain(|(weak, _)| weak.strong_count() > 0);
        let found = self
            .entries
            .iter()
            .position(|(weak, value)| weak.as_ptr() == Arc::as_ptr(rows) && valid(value));
        let entry = match found {
            Some(index) => self.entries.remove(index),
            None => {
                if self.entries.len() == STAGE_CACHE_ENTRIES {
                    self.entries.remove(0);
                }
                (Arc::downgrade(rows), make())
            }
        };
        self.entries.push(entry);
        &mut self.entries.last_mut().expect("pushed above").1
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Each side's widest row, measured once per rows and font.
pub(super) struct ContentWidths {
    font_id: egui::FontId,
    pixels_per_point: f32,
    widths: [f32; 2],
}

/// Row heights with wrap on, measured once per rows, font and wrap widths. Dropped while wrap
/// is off.
pub(super) struct RowHeights {
    font_id: egui::FontId,
    pixels_per_point: f32,
    natural_widths: Vec<[f32; 2]>,
    text_widths: [f32; 2],
    pub(super) heights: Vec<f32>,
    pub(super) tops: Vec<f32>,
}

/// The cached wrapped row heights. A `text_widths` change lays out only the rows too wide for
/// the new widths; new rows, font or zoom measure every row's one-line width again.
pub(super) fn wrapped_heights<'c, T: RawTokenTrait>(
    cache: &'c mut RowsMemo<RowHeights>,
    egui_ctx: &egui::Context,
    font_id: &egui::FontId,
    rows: &Arc<DiffRows>,
    file_source: Option<&CachedFile<T>>,
    file_target: Option<&CachedFile<T>>,
    text_widths: [f32; 2],
) -> &'c RowHeights {
    let _scope = zcommon_ui::frame_stats::scope("Wrapped row heights");
    let pixels_per_point = egui_ctx.pixels_per_point();
    let cached = cache.get_or_insert_with(
        rows,
        |cached| cached.font_id == *font_id && cached.pixels_per_point == pixels_per_point,
        || {
            let (natural_widths, heights) = egui_ctx.fonts_mut(|fonts| {
                let natural_widths =
                    natural_row_widths(fonts, font_id, rows, file_source, file_target);
                let heights = measure_wrapped_rows(
                    fonts,
                    font_id,
                    rows,
                    file_source,
                    file_target,
                    &natural_widths,
                    text_widths,
                );
                (natural_widths, heights)
            });
            RowHeights {
                font_id: font_id.clone(),
                pixels_per_point,
                natural_widths,
                text_widths,
                tops: row_tops(&heights),
                heights,
            }
        },
    );
    if cached.text_widths != text_widths {
        cached.heights = egui_ctx.fonts_mut(|fonts| {
            measure_wrapped_rows(
                fonts,
                font_id,
                rows,
                file_source,
                file_target,
                &cached.natural_widths,
                text_widths,
            )
        });
        cached.tops = row_tops(&cached.heights);
        cached.text_widths = text_widths;
    }
    cached
}

impl FileDiffPane {
    pub(super) fn content_widths(
        &mut self,
        ui: &egui::Ui,
        rows: &Arc<DiffRows>,
        diff_ctx: &MinimalDiffCtx,
    ) -> [f32; 2] {
        let _scope = zcommon_ui::frame_stats::scope("Content widths");
        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
        let pixels_per_point = ui.ctx().pixels_per_point();
        self.content_widths
            .get_or_insert_with(
                rows,
                |cached| cached.font_id == font_id && cached.pixels_per_point == pixels_per_point,
                || ContentWidths {
                    widths: ui.fonts_mut(|fonts| {
                        let mut glyph_width = ascii_cached_glyph_width(fonts, &font_id);
                        side_content_widths(
                            rows,
                            diff_ctx.input.file_1.as_deref(),
                            diff_ctx.input.file_2.as_deref(),
                            &mut glyph_width,
                        )
                    }),
                    font_id: font_id.clone(),
                    pixels_per_point,
                },
            )
            .widths
    }
}

/// `glyph_width` with a table for ASCII: a font lookup per char dominated measuring every row.
fn ascii_cached_glyph_width<'f>(
    fonts: &'f mut egui::epaint::FontsView<'_>,
    font_id: &'f egui::FontId,
) -> impl FnMut(char) -> f32 + 'f {
    let ascii: [f32; 128] =
        std::array::from_fn(|c| fonts.glyph_width(font_id, char::from(c as u8)));
    move |c| match ascii.get(c as usize) {
        Some(width) => *width,
        None => fonts.glyph_width(font_id, c),
    }
}

/// Width a side's text wraps at in a cell `cell_width` wide: what the gutter leaves.
pub(in crate::ui_egui) fn wrap_width(cell_width: f32, gutter_width: f32) -> f32 {
    cell_width - gutter_width - GUTTER_GAP
}

/// Width of each side's widest row: gutter, text and ghost text. Measured over all rows, since
/// the table only lays out the visible ones.
pub(in crate::ui_egui) fn side_content_widths<T: RawTokenTrait>(
    rows: &[DiffRow],
    file_source: Option<&CachedFile<T>>,
    file_target: Option<&CachedFile<T>>,
    mut glyph_width: impl FnMut(char) -> f32,
) -> [f32; 2] {
    let mut row_width = |content: &LineContent| match content {
        LineContent::Code { tokens, .. } => {
            let row_text = build_row_text(tokens, &[], file_source, file_target);
            GUTTER_WIDTH + GUTTER_GAP + one_line_width(&row_text, &mut glyph_width)
        }
        LineContent::Void | LineContent::Collapsed => 0.0,
    };
    rows.iter().fold([0.0; 2], |[left, right], row| {
        [
            left.max(row_width(&row.left)),
            right.max(row_width(&row.right)),
        ]
    })
}

fn one_line_width(row_text: &RowText, mut glyph_width: impl FnMut(char) -> f32) -> f32 {
    std::iter::once(&row_text.text)
        .chain(row_text.ghosts.iter().map(|ghost| &ghost.text))
        .map(|text| h_scroll::text_width(text, &mut glyph_width))
        .sum()
}

pub(in crate::ui_egui) fn natural_row_widths<T: RawTokenTrait>(
    fonts: &mut egui::epaint::FontsView<'_>,
    font_id: &egui::FontId,
    rows: &[DiffRow],
    file_source: Option<&CachedFile<T>>,
    file_target: Option<&CachedFile<T>>,
) -> Vec<[f32; 2]> {
    let mut glyph_width = ascii_cached_glyph_width(fonts, font_id);
    let mut side_width = |content: &LineContent| match content {
        LineContent::Code { tokens, .. } => {
            let row_text = build_row_text(tokens, &[], file_source, file_target);
            one_line_width(&row_text, &mut glyph_width)
        }
        LineContent::Void | LineContent::Collapsed => 0.0,
    };
    rows.iter()
        .map(|row| [side_width(&row.left), side_width(&row.right)])
        .collect()
}

/// Each row's height with wrap on: the taller of its two sides, and never less than one line.
/// `side_height(side, text)` measures one side's code row, side 0 being the left.
pub(in crate::ui_egui) fn wrapped_row_heights<T: RawTokenTrait>(
    rows: &[DiffRow],
    file_source: Option<&CachedFile<T>>,
    file_target: Option<&CachedFile<T>>,
    line_height: f32,
    mut may_wrap: impl FnMut(usize, usize) -> bool,
    mut side_height: impl FnMut(usize, &RowText) -> f32,
) -> Vec<f32> {
    rows.iter()
        .enumerate()
        .map(|(row_index, row)| {
            [&row.left, &row.right].into_iter().enumerate().fold(
                line_height,
                |height, (side, content)| match content {
                    LineContent::Code { tokens, .. } if may_wrap(row_index, side) => {
                        let row_text = build_row_text(tokens, &[], file_source, file_target);
                        height.max(side_height(side, &row_text))
                    }
                    LineContent::Code { .. } | LineContent::Void | LineContent::Collapsed => height,
                },
            )
        })
        .collect()
}

/// `wrapped_row_heights` measured with egui's layout, each side's text wrapped at its
/// `text_widths` entry (the column width without the gutter).
pub(in crate::ui_egui) fn measure_wrapped_rows<T: RawTokenTrait>(
    fonts: &mut egui::epaint::FontsView<'_>,
    font_id: &egui::FontId,
    rows: &[DiffRow],
    file_source: Option<&CachedFile<T>>,
    file_target: Option<&CachedFile<T>>,
    natural_widths: &[[f32; 2]],
    text_widths: [f32; 2],
) -> Vec<f32> {
    assert_eq!(natural_widths.len(), rows.len());
    let line_height = fonts.row_height(font_id);
    // The glyph sum is never short of the one-line layout (see `side_content_widths`), so a row
    // that fits by it needs no layout. The margin covers pixel rounding.
    let may_wrap = |row: usize, side: usize| natural_widths[row][side] + 1.0 >= text_widths[side];
    let side_height = |side: usize, row_text: &RowText| {
        let width = text_widths[side];
        let job = egui::text::LayoutJob::single_section(
            row_text.text.clone(),
            egui::TextFormat::simple(font_id.clone(), egui::Color32::PLACEHOLDER),
        );
        layout_side_text(
            fonts,
            job,
            &row_text.ghosts,
            egui::Color32::PLACEHOLDER,
            font_id,
            width,
        )
        .height()
    };
    wrapped_row_heights(
        rows,
        file_source,
        file_target,
        line_height,
        may_wrap,
        side_height,
    )
}

/// The cell a side's row is drawn in before `SplitTable` shifts it sideways. The shifted ui still
/// clips to the cell, so its clip rect has the cell's x range.
pub(super) fn unscrolled_cell(ui: &egui::Ui) -> egui::Rect {
    egui::Rect::from_x_y_ranges(ui.clip_rect().x_range(), ui.max_rect().y_range())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{char_width, collapsed_row};
    use super::*;

    use zdiff::diff_builder::{DiffBuilderOptions, LineContent};

    use crate::ui_egui::copy_harness::{CopyHarness, measure_in_fresh_context};
    // One point per char, so a width is the gutter plus a char count.
    fn char_count_widths(source: &str, target: &str) -> [f32; 2] {
        CopyHarness::new(source, target, &DiffBuilderOptions::default()).content_widths(|_| 1.0)
    }

    #[test]
    fn content_width_is_the_widest_rows_gutter_and_text_on_each_side() {
        let text_x = GUTTER_WIDTH + GUTTER_GAP;
        let text = "short\nthe longest line\nend\n";
        assert_eq!(char_count_widths(text, text), [text_x + 16.0; 2]);
        // An inserted line is a ghost on the other side.
        assert_eq!(
            char_count_widths("short\n", "short\nadded line\n"),
            [text_x + 10.0; 2]
        );
        // No rows, no extent.
        assert_eq!(char_count_widths("", ""), [0.0; 2]);
    }

    #[test]
    fn content_width_includes_ghost_text() {
        let text_x = GUTTER_WIDTH + GUTTER_GAP;
        // Each side shows its own text plus the other side's changed token as a ghost.
        let widths = char_count_widths("let x = old(1);\n", "let x = brand_new(1);\n");
        assert_eq!(widths, [text_x + 15.0 + 9.0, text_x + 21.0 + 3.0]);
    }

    // Ten points per line, wrapping every `per_line` chars of a side's text.
    fn char_wrapped_heights(harness: &CopyHarness, per_line: [usize; 2]) -> Vec<f32> {
        harness.wrapped_row_heights(
            10.0,
            |_, _| true,
            |side, row_text| row_text.text.chars().count().div_ceil(per_line[side]) as f32 * 10.0,
        )
    }

    #[test]
    fn a_side_that_cannot_wrap_takes_one_line_unmeasured() {
        let text = format!("short\n{}\n", "x".repeat(25));
        let harness = CopyHarness::new(&text, &text, &DiffBuilderOptions::default());
        let mut measured = Vec::new();

        let heights = harness.wrapped_row_heights(
            10.0,
            |row, side| (row, side) == (1, 0),
            |side, row_text| {
                measured.push((side, row_text.text.clone()));
                50.0
            },
        );

        assert_eq!(heights, [10.0, 50.0]);
        assert_eq!(measured, [(0, "x".repeat(25))]);
    }

    #[test]
    fn a_width_change_measures_the_cached_rows_at_the_new_width() {
        let line = "let words = [alpha, beta, gamma, delta, epsilon, zeta, eta, theta];";
        let source = format!("{line}\nshort\n");
        let target = format!("{}\nshort\n", line.replace("theta", "iota"));
        let harness = CopyHarness::new(&source, &target, &DiffBuilderOptions::default());
        let rows = Arc::new(harness.rows().to_vec());
        let [file_source, file_target] = harness.files();
        let ctx = egui::Context::default();
        let mut cache = RowsMemo::default();
        let mut heights_at = |text_widths| {
            let mut heights = Vec::new();
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                let font_id = egui::TextStyle::Monospace.resolve(&ctx.style());
                let cached = wrapped_heights(
                    &mut cache,
                    ctx,
                    &font_id,
                    &rows,
                    Some(file_source),
                    Some(file_target),
                    text_widths,
                );
                assert_eq!(cached.tops, row_tops(&cached.heights));
                heights = cached.heights.clone();
            });
            heights
        };

        for chars in [1000.0, 20.0, 30.0, 1000.0] {
            let text_widths = [chars * char_width(), 1000.0];
            assert_eq!(
                heights_at(text_widths),
                harness.measured_row_heights(text_widths).0,
                "at {chars} chars"
            );
        }
    }

    #[test]
    fn the_rows_memo_serves_revisited_rows_until_they_are_dropped_or_evicted() {
        let mut memo = RowsMemo::default();
        let mut made = 0;
        let mut get = |memo: &mut RowsMemo<usize>, rows: &Arc<DiffRows>, valid: bool| {
            *memo.get_or_insert_with(
                rows,
                |_| valid,
                || {
                    made += 1;
                    made
                },
            )
        };
        let first = Arc::new(DiffRows::new());
        let second = Arc::new(DiffRows::new());

        assert_eq!(get(&mut memo, &first, true), 1);
        assert_eq!(get(&mut memo, &second, true), 2);
        assert_eq!(get(&mut memo, &first, true), 1);
        assert_eq!(get(&mut memo, &first, false), 3);

        drop(second);
        let others: Vec<Arc<DiffRows>> = (0..STAGE_CACHE_ENTRIES)
            .map(|_| Arc::new(DiffRows::new()))
            .collect();
        for rows in &others {
            get(&mut memo, rows, true);
        }
        assert_eq!(memo.entries.len(), STAGE_CACHE_ENTRIES);
        assert_eq!(get(&mut memo, &others[0], true), 4);
        assert_eq!(get(&mut memo, &first, true), 4 + STAGE_CACHE_ENTRIES);
    }

    #[test]
    fn a_wrapped_row_is_as_tall_as_its_taller_side() {
        let text = format!("short\n{}\n", "x".repeat(25));
        let harness = CopyHarness::new(&text, &text, &DiffBuilderOptions::default());

        assert_eq!(char_wrapped_heights(&harness, [10, 5]), [10.0, 50.0]);
        assert_eq!(char_wrapped_heights(&harness, [5, 10]), [10.0, 50.0]);
        assert_eq!(char_wrapped_heights(&harness, [30, 30]), [10.0, 10.0]);
    }

    #[test]
    fn a_wrapped_row_is_never_less_than_one_line() {
        // Ghost rows off, so the inserted line is a Void row on the left.
        let options = DiffBuilderOptions {
            ghost_rows: false,
            ..Default::default()
        };
        let target = format!("a\n\n{}\nb\n", "y".repeat(25));
        let mut harness = CopyHarness::new("a\n\nb\n", &target, &options);
        assert!(matches!(harness.rows()[2].left, LineContent::Void));

        // A blank line has no text and still takes a line; a Void side takes the other side's.
        assert_eq!(
            char_wrapped_heights(&harness, [10, 10]),
            [10.0, 10.0, 30.0, 10.0]
        );

        let collapsed = collapsed_row();
        harness.set_row(2, collapsed);
        assert_eq!(char_wrapped_heights(&harness, [10, 10])[2], 10.0);
    }

    /// Height of `text` laid out by egui on its own, wrapped at `width`.
    fn egui_wrapped_height(text: &str, width: f32) -> f32 {
        measure_in_fresh_context(|fonts, font_id| {
            let job = eframe::egui::text::LayoutJob::simple(
                text.to_owned(),
                font_id.clone(),
                eframe::egui::Color32::WHITE,
                width,
            );
            fonts.layout_job(job).size().y
        })
    }

    #[test]
    fn measured_rows_wrap_where_egui_wraps_the_text() {
        let line = "let words = [alpha, beta, gamma, delta, epsilon, zeta, eta, theta];";
        let text = format!("{line}\nshort\n");
        let harness = CopyHarness::new(&text, &text, &DiffBuilderOptions::default());
        let width = 20.0 * char_width();

        let (heights, line_height) = harness.measured_row_heights([width, 1000.0]);

        assert_eq!(heights[0], egui_wrapped_height(line, width));
        assert!(heights[0] >= 3.0 * line_height, "{heights:?}");
        assert_eq!(heights[1], line_height);
        // Wide enough for every line: one line each.
        let (heights, _) = harness.measured_row_heights([1000.0, 1000.0]);
        assert_eq!(heights, [line_height; 2]);
    }

    #[test]
    fn trailing_ghost_text_wraps_after_the_text() {
        let source = "let x = 1;\n";
        let target = "let x = 1; // a long trailing comment that the left side shows as a ghost\n";
        let harness = CopyHarness::new(source, target, &DiffBuilderOptions::default());
        let LineContent::Code { tokens, .. } = &harness.rows()[0].left else {
            panic!("expected a code row: {:?}", harness.rows());
        };
        assert!(tokens.iter().any(|(_, _, is_ghost)| *is_ghost));
        let width = 20.0 * char_width();

        // The right side is one line, so the row is as tall as the left: its text and the ghost
        // continuing after it wrap like the right side's identical line.
        let (heights, line_height) = harness.measured_row_heights([width, 1000.0]);

        assert_eq!(heights[0], egui_wrapped_height(target.trim_end(), width));
        assert!(heights[0] >= 3.0 * line_height, "{heights:?}");
    }
}
