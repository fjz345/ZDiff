mod center;
mod collapsed;
mod copy;
mod gutter;
mod highlight;
mod layout;
mod line_menu;
mod path_bar;
mod pins;
mod row;
mod ruler;
mod toolbar;
mod trace;
mod ui;
mod unified;

use center::*;
use collapsed::*;
use copy::*;
use gutter::*;
use highlight::*;
use layout::*;
pub(crate) use line_menu::line_text;
use line_menu::*;
use path_bar::*;
use pins::*;
use row::*;
use ruler::*;
use toolbar::*;
use trace::*;
use unified::*;
pub(super) use copy::CopyMarkerPlugin;
pub(super) use layout::wrap_width;
#[cfg(test)]
pub(super) use layout::{measure_wrapped_rows, side_content_widths, wrapped_row_heights};
pub(super) use row::{RowCtx, text_rows};
pub use toolbar::REGEX_IGNORE_PRESETS;

use std::sync::Arc;

use crate::{
    browser::{BrowserAction, DepotFolder, FolderBrowser, FolderSource, LocalFolder},
    clamped_cursor::ClampedCursor,
    diff_ctx::{
        BlockToggle, DiffRows, DiffStageTimes, FindHit, MinimalDiffCtx, RowBlock, ScrollSpan,
        insert_pin, pin_line_counts,
    },
    diff_phases::{Marks, diff_marks_checkboxes, in_scopes},
    file::FileProcessor,
    keybindings::FileAction,
    revert::{self, RevertRequest, RevertTarget},
    ui_egui::{
        active_side::{ActiveSide, ActiveSideState, outline_stroke, shown_sides},
        file_role::FileRole,
        occurrence::{OccurrenceState, RowFind, find_ranges},
        overview::{
            self, BinCache, ChangeKind, ChangeSpan, RULER_WIDTH, RulerColors, row_at,
            row_change_spans, row_tops,
        },
        panes::ZAppPane,
        revision_dropdown::revision_dropdown,
        side_viewers::{self, SideViewers},
        syntax,
        text_mode::TextMode,
        unified::{UnifiedKind, UnifiedRow, single_file_rows, unified_rows, unified_scroll_index},
    },
    viewer::{
        ViewerKind,
        audio::{self, AudioDiffProcessor, AudioSettings},
        audio_meta::{self, AudioMetaProcessor},
        hex::{self, HexViewCtx},
        hex_strings::{HexMode, HexSettings, hex_mode_ui, line_offset},
        image::{self, ImageDiffProcessor, ImageSettings},
        media_placeholder,
        video_frames::VideoFrameProcessor,
        video_meta::{self, VideoMetaProcessor},
    },
};
use eframe::egui::{self, Layout, TextEdit, UiBuilder, Vec2, text_selection::LabelSelectionState};
use serde::{Deserialize, Serialize};
use zcommon::{
    find::Matcher,
    path::{PathSegment, common_parent, split_path},
    presets::{BuiltinPreset, UserPresets},
    style::{Rgba8, RowLook, SharedRole, SyntaxCategory, stripe_segments},
};
use zcommon_ui::{
    h_scroll,
    keymap::{Action, Keymap},
    pathbar::{PathBarAction, path_bar},
    presets::presets_menu,
    style_window::{collapsed_look, role_color, syntax_colors, void_look},
    table::{Rows, Split, SplitColumn, SplitTable, shared_width},
    toggle::{TOGGLE_BUTTON_SIZE, toggle_button},
};
use zdiff::{
    cached_file::CachedFile,
    change_count::ChangeUnit,
    diff_builder::{DiffBuilderOptions, DiffRow, IsDimmed, IsGhost, LineContent, RowBg},
    diff_ir::{DiffOp, DiffResult},
    ignore::IgnorePatterns,
    lexer::RawTokenTrait,
    myers::{MyersNumAddDelete, Pin},
    row_text::{GhostInsertion, RowText, build_row_text},
    trace::{PhaseTrace, TraceSpan},
    universal_path::UniversalPath,
};

pub struct FileDiffPaneCtx<'a> {
    // need this because file_source can be none and we want to keep displaying
    // the text edit with the old path if even if it could not load a cachedfile
    pub file_source_path: UniversalPath,
    pub file_target_path: UniversalPath,
    pub file_source_label: Option<String>,
    pub file_target_label: Option<String>,
    pub file_source_root: Option<UniversalPath>,
    pub file_target_root: Option<UniversalPath>,
    pub file_source_root_valid: bool,
    pub file_target_root_valid: bool,
    pub file_source_path_valid: bool,
    pub file_target_path_valid: bool,
    pub file_source_load_error: Option<String>,
    pub file_target_load_error: Option<String>,
    pub file_source_loading: bool,
    pub file_target_loading: bool,
    pub diff_loading: bool,
    pub launch_error: Option<&'a str>,

    pub diff_ctx: Option<&'a MinimalDiffCtx>,
    pub diff_options: &'a mut DiffBuilderOptions,
    pub regex_ignore_presets: &'a mut UserPresets,
    pub scroll_left: &'a mut f32,
    pub scroll_right: &'a mut f32,
    /// Both sides scroll horizontally together.
    pub h_scroll_linked: &'a mut bool,
    /// Long lines wrap; rows then grow to the taller side.
    pub word_wrap: &'a mut bool,
    pub toggle_all_held: bool,
    pub toggle_all_request: &'a mut bool,
    pub text_mode: &'a mut TextMode,

    pub code_language: [&'a str; 2],

    pub scroll_to_row_span: &'a Option<ScrollSpan>,
    pub active_highlights: &'a [usize],
    pub highlight_side: Option<ActiveSide>,
    pub conflict_cursor: &'a mut ClampedCursor,
    pub find_cursor: &'a mut ClampedCursor,
    pub find_matchers: [Option<&'a Matcher>; 2],
    pub find_hit: Option<FindHit>,
    pub find_count: usize,
    pub active_side: &'a mut ActiveSide,
    pub toggle_active_side: bool,
    pub load_file_1_request: &'a mut Option<UniversalPath>,
    pub load_file_2_request: &'a mut Option<UniversalPath>,
    pub set_file_1_root_request: &'a mut Option<UniversalPath>,
    pub set_file_2_root_request: &'a mut Option<UniversalPath>,
    pub pins: &'a [Pin],
    /// The whole pin list after an add, remove or clear.
    pub pins_request: &'a mut Option<Vec<Pin>>,
    pub revert_request: &'a mut Option<RevertRequest>,
    /// `RowBlock::key` of the block whose button was clicked, and what the button asks for.
    pub block_toggle_request: &'a mut Option<(usize, BlockToggle)>,
    /// Set when the pair resolved to the Hex viewer; the table then shows hex rows.
    pub hex_view: Option<HexViewCtx<'a>>,
    /// Set when the pair resolved to the Hex viewer. In Strings mode the table shows the text diff
    /// of the extracted strings, with `string_offsets` in the gutter.
    pub hex_mode: Option<&'a mut HexSettings>,
    pub string_offsets: [&'a [usize]; 2],
    /// Set when the pair resolved to the Image viewer; the images replace the table rows.
    pub image_view: Option<&'a mut ImageDiffProcessor>,
    pub image_settings: Option<&'a mut ImageSettings>,
    pub audio_view: Option<(&'a mut AudioDiffProcessor, &'a mut AudioSettings)>,
    pub media_view: Option<ViewerKind>,
    pub audio_meta: Option<&'a mut AudioMetaProcessor>,
    pub video_frames: Option<&'a mut VideoFrameProcessor>,
    pub video_meta: Option<&'a mut VideoMetaProcessor>,
    pub side_viewers: Option<SideViewers<'a>>,
    /// The toolbar's viewer switch for the current pair; `None` is Auto.
    pub viewer_override: &'a mut Option<ViewerKind>,
    /// Why the pair isn't in the viewer it asked for, if it isn't.
    pub viewer_fallback: Option<&'a str>,
    pub phase_overlay: bool,
    pub sides_swapped: bool,
    pub keymap: &'a Keymap<FileAction>,
}

#[derive(Serialize, Deserialize)]
pub struct FileDiffPane {
    pub title: Option<String>,
    #[serde(skip)]
    active_side: ActiveSideState,
    #[serde(skip)]
    content_widths: Option<ContentWidths>,
    #[serde(skip)]
    row_heights: Option<RowHeights>,
    #[serde(skip)]
    ruler_spans: Option<RulerSpans>,
    #[serde(skip)]
    ruler_bins: BinCache,
    #[serde(skip)]
    occurrences: OccurrenceState,
    #[serde(skip)]
    pin_input: PinInput,
    #[serde(skip)]
    browsers: [Option<FolderBrowser>; 2],
    #[serde(skip)]
    split: Split,
    /// Measured from the last frame's header: the header cells clip to the height they're given.
    #[serde(skip)]
    header_height: f32,
    #[serde(skip)]
    center_layout: Option<CenterLayoutCache>,
    #[serde(skip)]
    unified: Option<Arc<UnifiedView>>,
}

/// The text rows' rects of the last drawn frame, taken by the app's raw input hook, which runs
/// before the pane.
pub fn wheel_rects_id() -> egui::Id {
    egui::Id::new("diff_pane_wheel_rects")
}

impl ZAppPane for FileDiffPane {
    fn title(&self) -> String {
        self.title.clone().unwrap_or(format!("Pane"))
    }
}

impl FileDiffPane {
    pub fn new(title: Option<String>) -> Self {
        Self {
            title,
            active_side: ActiveSideState::default(),
            content_widths: None,
            row_heights: None,
            ruler_spans: None,
            ruler_bins: BinCache::default(),
            occurrences: OccurrenceState::default(),
            pin_input: PinInput::default(),
            browsers: [None, None],
            split: Split::default(),
            header_height: 0.0,
            center_layout: None,
            unified: None,
        }
    }

}

#[cfg(test)]
mod tests {
    

    
    use zcommon_ui::table::Split;
    

    

    #[test]
    fn the_split_is_not_persisted() {
        let mut pane = super::FileDiffPane::new(Some("Diff".to_owned()));
        pane.split.drag(150.0, 1000.0);
        assert_ne!(pane.split, Split::default());

        let json = serde_json::to_string(&pane).unwrap();
        let restored: super::FileDiffPane = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.title.as_deref(), Some("Diff"));
        assert_eq!(restored.split, Split::default());
    }

}

#[cfg(test)]
mod test_support {
    use zdiff::diff_builder::{DiffRow, LineContent};

    use crate::ui_egui::copy_harness::measure_in_fresh_context;

    pub(super) fn collapsed_row_between(row: &DiffRow) -> DiffRow {
        DiffRow {
            left: LineContent::Collapsed,
            right: LineContent::Collapsed,
            ..row.clone()
        }
    }

    pub(super) fn char_width() -> f32 {
        measure_in_fresh_context(|fonts, font_id| fonts.glyph_width(font_id, 'x'))
    }
}
