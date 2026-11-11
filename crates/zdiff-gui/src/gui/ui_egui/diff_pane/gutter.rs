use super::*;

/// Line number column of a code row, and the gap between it and the row text.
pub(super) const GUTTER_WIDTH: f32 = 35.0;
pub(super) const OFFSET_GUTTER_WIDTH: f32 = 60.0;
pub(super) const GUTTER_GAP: f32 = 4.0;
pub(super) const CENTER_GAP: f32 = 2.0;

pub(super) const UNIFIED_SIGN_WIDTH: f32 = 14.0;

const LINE_NUMBER_SIZE: f32 = 10.0;
const LINE_NUMBER_PADDING: f32 = 4.0;

pub(super) fn line_number_font(style: &egui::Style) -> egui::FontId {
    egui::FontId {
        size: LINE_NUMBER_SIZE,
        ..egui::TextStyle::Monospace.resolve(style)
    }
}

pub(super) fn line_number_width(max_line: usize, digit_width: f32) -> f32 {
    let digits = max_line.checked_ilog10().map_or(1, |log| log + 1);
    (digits as f32 * digit_width + LINE_NUMBER_PADDING).max(GUTTER_WIDTH)
}

/// Fits the largest line number of either file, so the column keeps its width while scrolling.
pub(in crate::ui_egui) fn line_number_width_for<T: RawTokenTrait>(
    ui: &egui::Ui,
    files: [Option<&CachedFile<T>>; 2],
) -> f32 {
    let max_line = files
        .into_iter()
        .flatten()
        .map(|file| file.metadata.num_lines())
        .max()
        .unwrap_or(0);
    let font = line_number_font(ui.style());
    let digit_width = ui.fonts_mut(|fonts| {
        ('0'..='9')
            .map(|digit| fonts.glyph_width(&font, digit))
            .fold(0.0, f32::max)
    });
    line_number_width(max_line, digit_width)
}

/// What a row's gutter shows: its own line number, the byte offset of its string (Hex Strings
/// mode), or both files' line numbers and the `-`/`+` of the unified column.
#[derive(Clone, Copy)]
pub(super) enum Gutter {
    Single {
        number_width: f32,
    },
    Offset(Option<usize>),
    Unified {
        left: Option<usize>,
        right: Option<usize>,
        sign: Option<(char, egui::Color32)>,
        number_width: f32,
    },
}

impl Gutter {
    pub(super) fn width(self) -> f32 {
        match self {
            Self::Single { number_width } => number_width,
            Self::Offset(_) => OFFSET_GUTTER_WIDTH,
            Self::Unified { number_width, .. } => 2.0 * number_width + UNIFIED_SIGN_WIDTH,
        }
    }

    pub(super) fn text_width(self, cell_width: f32) -> f32 {
        wrap_width(cell_width, self.width())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_egui::copy_harness::measure_in_fresh_context;

    fn widest_glyph(chars: &str) -> f32 {
        measure_in_fresh_context(|fonts, _| {
            let font = line_number_font(&egui::Style {
                override_text_style: Some(egui::TextStyle::Monospace),
                ..Default::default()
            });
            chars
                .chars()
                .map(|c| fonts.glyph_width(&font, c))
                .fold(0.0, f32::max)
        })
    }

    #[test]
    fn line_numbers_of_up_to_five_digits_keep_the_default_width() {
        let digit_width = widest_glyph("0123456789");
        for max_line in [0, 1, 9, 10, 99_999] {
            assert_eq!(line_number_width(max_line, digit_width), GUTTER_WIDTH);
        }
    }

    #[test]
    fn the_line_number_column_grows_with_each_digit_and_fits_them_all() {
        let digit_width = widest_glyph("0123456789");
        let maxima = [99_999, 100_000, 1_000_000, 12_345_678, usize::MAX];
        let widths = maxima.map(|max_line| line_number_width(max_line, digit_width));

        assert!(widths.is_sorted_by(|a, b| a < b), "{widths:?}");
        for (max_line, width) in maxima.into_iter().zip(widths) {
            let digits = max_line.to_string().len() as f32;
            assert!(digits * digit_width < width, "{max_line}: {width}");
        }
    }

    #[test]
    fn line_numbers_are_measured_in_the_font_the_rows_paint_them_in() {
        let rows_style = egui::Style {
            override_text_style: Some(egui::TextStyle::Monospace),
            ..Default::default()
        };

        assert_eq!(
            line_number_font(&egui::Style::default()),
            line_number_font(&rows_style)
        );
    }

    #[test]
    fn an_eight_hex_digit_string_offset_fits_its_column() {
        let hex_digit_width = widest_glyph("0123456789ABCDEF");

        assert!(8.0 * hex_digit_width + LINE_NUMBER_PADDING <= OFFSET_GUTTER_WIDTH);
    }
}
