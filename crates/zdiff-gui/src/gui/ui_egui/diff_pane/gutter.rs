use super::*;

/// Line number column of a code row, and the gap between it and the row text.
pub(super) const GUTTER_WIDTH: f32 = 35.0;
pub(super) const OFFSET_GUTTER_WIDTH: f32 = 60.0;
pub(super) const GUTTER_GAP: f32 = 4.0;
pub(super) const CENTER_GAP: f32 = 2.0;

pub(super) const UNIFIED_SIGN_WIDTH: f32 = 14.0;

pub(super) fn unified_gutter_width() -> f32 {
    2.0 * GUTTER_WIDTH + UNIFIED_SIGN_WIDTH
}

pub(super) fn unified_text_width(cell_width: f32) -> f32 {
    cell_width - unified_gutter_width() - GUTTER_GAP
}

/// What a row's gutter shows: its own line number, the byte offset of its string (Hex Strings
/// mode), or both files' line numbers and the `-`/`+` of the unified column.
#[derive(Clone, Copy)]
pub(super) enum Gutter {
    Single,
    Offset(Option<usize>),
    Unified {
        left: Option<usize>,
        right: Option<usize>,
        sign: Option<(char, egui::Color32)>,
    },
}

impl Gutter {
    pub(super) fn width(self) -> f32 {
        match self {
            Self::Single => GUTTER_WIDTH,
            Self::Offset(_) => OFFSET_GUTTER_WIDTH,
            Self::Unified { .. } => unified_gutter_width(),
        }
    }

    pub(super) fn text_width(self, cell_width: f32) -> f32 {
        match self {
            Self::Single => wrap_width(cell_width),
            Self::Offset(_) => cell_width - OFFSET_GUTTER_WIDTH - GUTTER_GAP,
            Self::Unified { .. } => unified_text_width(cell_width),
        }
    }
}

