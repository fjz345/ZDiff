use super::*;

/// Each side's widest row, measured once per rows and font.
pub(super) struct ContentWidths {
    // Kept alive so the pointer comparison can't match a new allocation at the same address.
    pub(super) rows: Arc<DiffRows>,
    pub(super) font_id: egui::FontId,
    pub(super) pixels_per_point: f32,
    pub(super) widths: [f32; 2],
}

/// Row heights with wrap on, measured once per rows, font and wrap widths. Dropped while wrap
/// is off.
pub(super) struct RowHeights {
    // Kept alive so the pointer comparison can't match a new allocation at the same address.
    pub(super) rows: Arc<DiffRows>,
    pub(super) font_id: egui::FontId,
    pub(super) pixels_per_point: f32,
    pub(super) text_widths: [f32; 2],
    pub(super) heights: Vec<f32>,
    pub(super) tops: Vec<f32>,
}

/// The cached wrapped row heights, measured again when the rows, font or `text_widths` changed.
/// Measuring lays out every row that doesn't fit on one line, so a column resize on a large file
/// costs a full pass each frame it moves.
pub(super) fn wrapped_heights<'c>(
    cache: &'c mut Option<RowHeights>,
    egui_ctx: &egui::Context,
    font_id: &egui::FontId,
    rows: &Arc<DiffRows>,
    diff_ctx: &MinimalDiffCtx,
    text_widths: [f32; 2],
) -> &'c RowHeights {
    let pixels_per_point = egui_ctx.pixels_per_point();
    let hit = cache.as_ref().is_some_and(|cached| {
        Arc::ptr_eq(&cached.rows, rows)
            && cached.font_id == *font_id
            && cached.pixels_per_point == pixels_per_point
            && cached.text_widths == text_widths
    });
    if !hit {
        let heights = egui_ctx.fonts_mut(|fonts| {
            measure_wrapped_rows(
                fonts,
                font_id,
                rows,
                diff_ctx.input.file_1.as_deref(),
                diff_ctx.input.file_2.as_deref(),
                text_widths,
            )
        });
        *cache = Some(RowHeights {
            rows: rows.clone(),
            font_id: font_id.clone(),
            pixels_per_point,
            text_widths,
            tops: row_tops(&heights),
            heights,
        });
    }
    cache.as_ref().expect("filled above")
}

impl FileDiffPane {
    pub(super) fn content_widths(
        &mut self,
        ui: &egui::Ui,
        rows: &Arc<DiffRows>,
        diff_ctx: &MinimalDiffCtx,
    ) -> [f32; 2] {
        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
        let pixels_per_point = ui.ctx().pixels_per_point();
        if let Some(cached) = &self.content_widths
            && Arc::ptr_eq(&cached.rows, rows)
            && cached.font_id == font_id
            && cached.pixels_per_point == pixels_per_point
        {
            return cached.widths;
        }
        let widths = ui.fonts_mut(|fonts| {
            side_content_widths(
                rows,
                diff_ctx.input.file_1.as_deref(),
                diff_ctx.input.file_2.as_deref(),
                |c| fonts.glyph_width(&font_id, c),
            )
        });
        self.content_widths = Some(ContentWidths {
            rows: rows.clone(),
            font_id,
            pixels_per_point,
            widths,
        });
        widths
    }

}

/// Width a side's text wraps at in a cell `cell_width` wide: what the gutter leaves.
pub(in crate::ui_egui) fn wrap_width(cell_width: f32) -> f32 {
    cell_width - GUTTER_WIDTH - GUTTER_GAP
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
            let ghosts: f32 = row_text
                .ghosts
                .iter()
                .map(|ghost| h_scroll::text_width(&ghost.text, &mut glyph_width))
                .sum();
            GUTTER_WIDTH
                + GUTTER_GAP
                + h_scroll::text_width(&row_text.text, &mut glyph_width)
                + ghosts
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

/// Each row's height with wrap on: the taller of its two sides, and never less than one line.
/// `side_height(side, text)` measures one side's code row, side 0 being the left.
pub(in crate::ui_egui) fn wrapped_row_heights<T: RawTokenTrait>(
    rows: &[DiffRow],
    file_source: Option<&CachedFile<T>>,
    file_target: Option<&CachedFile<T>>,
    line_height: f32,
    mut side_height: impl FnMut(usize, &RowText) -> f32,
) -> Vec<f32> {
    rows.iter()
        .map(|row| {
            [&row.left, &row.right].into_iter().enumerate().fold(
                line_height,
                |height, (side, content)| match content {
                    LineContent::Code { tokens, .. } => {
                        let row_text = build_row_text(tokens, &[], file_source, file_target);
                        height.max(side_height(side, &row_text))
                    }
                    LineContent::Void | LineContent::Collapsed => height,
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
    text_widths: [f32; 2],
) -> Vec<f32> {
    let line_height = fonts.row_height(font_id);
    let side_height = |side: usize, row_text: &RowText| {
        let width = text_widths[side];
        // The glyph sum is never short of the one-line layout (see `side_content_widths`), so a
        // row that fits by it needs no layout. The margin covers pixel rounding.
        let mut glyph_width = |c| fonts.glyph_width(font_id, c);
        let one_line: f32 = std::iter::once(&row_text.text)
            .chain(row_text.ghosts.iter().map(|ghost| &ghost.text))
            .map(|text| h_scroll::text_width(text, &mut glyph_width))
            .sum();
        if one_line + 1.0 < width {
            return line_height;
        }
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
    wrapped_row_heights(rows, file_source, file_target, line_height, side_height)
}

/// The cell a side's row is drawn in before `SplitTable` shifts it sideways. The shifted ui still
/// clips to the cell, so its clip rect has the cell's x range.
pub(super) fn unscrolled_cell(ui: &egui::Ui) -> egui::Rect {
    egui::Rect::from_x_y_ranges(ui.clip_rect().x_range(), ui.max_rect().y_range())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_support::{char_width, collapsed_row_between};
    

    
    
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
        harness.wrapped_row_heights(10.0, |side, row_text| {
            row_text.text.chars().count().div_ceil(per_line[side]) as f32 * 10.0
        })
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

        let collapsed = collapsed_row_between(&harness.rows()[2]);
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
