use std::sync::Arc;

use crate::{
    browser::{BrowserAction, DepotFolder, FolderBrowser, FolderSource, LocalFolder},
    clamped_cursor::ClampedCursor,
    diff_ctx::{
        BlockToggle, DiffRows, DiffStageTimes, FindHit, MinimalDiffCtx, ScrollSpan, insert_pin,
        pin_line_counts,
    },
    file::FileProcessor,
    keybindings::{FileAction, with_shortcut},
    revert::{self, RevertRequest, RevertTarget},
    ui_egui::{
        active_side::{ActiveSide, ActiveSideState, outline_stroke},
        file_role::FileRole,
        occurrence::{OccurrenceState, RowFind, find_ranges},
        panes::ZAppPane,
    },
    viewer::{
        ViewerKind,
        audio::{self, AudioDiffProcessor, AudioSettings},
        audio_meta::{self, AudioMetaProcessor},
        hex::{self, HexViewCtx},
        image::{self, ImageDiffProcessor},
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
    style::SharedRole,
    ui_egui::{
        h_scroll,
        keymap::{Action, Keymap},
        pathbar::{PathBarAction, path_bar},
        presets::presets_menu,
        style_window::role_color,
        table::{Rows, Split, SplitColumn, SplitTable, shared_width},
        toggle::{TOGGLE_BUTTON_SIZE, toggle_button},
    },
};
use zdiff::{
    cached_file::CachedFile,
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

    pub code_language: &'a str,

    pub scroll_to_row_span: &'a Option<ScrollSpan>,
    pub active_highlights: &'a Vec<usize>,
    pub highlight_side: Option<ActiveSide>,
    pub conflict_cursor: &'a mut ClampedCursor,
    pub find_cursor: &'a mut ClampedCursor,
    pub find_matcher: Option<&'a Matcher>,
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
    /// Set when the pair resolved to the Image viewer; the images replace the table rows.
    pub image_view: Option<&'a mut ImageDiffProcessor>,
    pub audio_view: Option<(&'a mut AudioDiffProcessor, &'a mut AudioSettings)>,
    pub media_view: Option<ViewerKind>,
    pub audio_meta: Option<&'a mut AudioMetaProcessor>,
    pub video_frames: Option<&'a mut VideoFrameProcessor>,
    pub video_meta: Option<&'a mut VideoMetaProcessor>,
    /// The toolbar's viewer switch for the current pair; `None` is Auto.
    pub viewer_override: &'a mut Option<ViewerKind>,
    /// Why the pair isn't in the viewer it asked for, if it isn't.
    pub viewer_fallback: Option<&'a str>,
    pub phase_overlay: bool,
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
}

#[derive(Default)]
struct PinInput {
    left: String,
    right: String,
    error: Option<String>,
}

/// Each side's widest row, measured once per rows and font.
struct ContentWidths {
    // Kept alive so the pointer comparison can't match a new allocation at the same address.
    rows: Arc<DiffRows>,
    font_id: egui::FontId,
    pixels_per_point: f32,
    widths: [f32; 2],
}

/// Row heights with wrap on, measured once per rows, font and wrap widths. Dropped while wrap
/// is off.
struct RowHeights {
    // Kept alive so the pointer comparison can't match a new allocation at the same address.
    rows: Arc<DiffRows>,
    font_id: egui::FontId,
    pixels_per_point: f32,
    text_widths: [f32; 2],
    heights: Vec<f32>,
}

/// The cached wrapped row heights, measured again when the rows, font or `text_widths` changed.
/// Measuring lays out every row that doesn't fit on one line, so a column resize on a large file
/// costs a full pass each frame it moves.
fn wrapped_heights<'c>(
    cache: &'c mut Option<RowHeights>,
    egui_ctx: &egui::Context,
    font_id: &egui::FontId,
    diff_ctx: &MinimalDiffCtx,
    text_widths: [f32; 2],
) -> &'c [f32] {
    let pixels_per_point = egui_ctx.pixels_per_point();
    let hit = cache.as_ref().is_some_and(|cached| {
        Arc::ptr_eq(&cached.rows, &diff_ctx.diff_rows)
            && cached.font_id == *font_id
            && cached.pixels_per_point == pixels_per_point
            && cached.text_widths == text_widths
    });
    if !hit {
        let heights = egui_ctx.fonts_mut(|fonts| {
            measure_wrapped_rows(
                fonts,
                font_id,
                &diff_ctx.diff_rows,
                diff_ctx.input.file_1.as_deref(),
                diff_ctx.input.file_2.as_deref(),
                text_widths,
            )
        });
        *cache = Some(RowHeights {
            rows: diff_ctx.diff_rows.clone(),
            font_id: font_id.clone(),
            pixels_per_point,
            text_widths,
            heights,
        });
    }
    &cache.as_ref().expect("filled above").heights
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
            occurrences: OccurrenceState::default(),
            pin_input: PinInput::default(),
            browsers: [None, None],
            split: Split::default(),
            header_height: 0.0,
            center_layout: None,
        }
    }

    fn content_widths(&mut self, ui: &egui::Ui, diff_ctx: &MinimalDiffCtx) -> [f32; 2] {
        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
        let pixels_per_point = ui.ctx().pixels_per_point();
        if let Some(cached) = &self.content_widths
            && Arc::ptr_eq(&cached.rows, &diff_ctx.diff_rows)
            && cached.font_id == font_id
            && cached.pixels_per_point == pixels_per_point
        {
            return cached.widths;
        }
        let widths = ui.fonts_mut(|fonts| {
            side_content_widths(
                &diff_ctx.diff_rows,
                diff_ctx.input.file_1.as_deref(),
                diff_ctx.input.file_2.as_deref(),
                |c| fonts.glyph_width(&font_id, c),
            )
        });
        self.content_widths = Some(ContentWidths {
            rows: diff_ctx.diff_rows.clone(),
            font_id,
            pixels_per_point,
            widths,
        });
        widths
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut FileDiffPaneCtx) -> egui_tiles::UiResponse {
        // No-op once registered.
        ui.ctx().add_plugin(CopyMarkerPlugin);
        self.occurrences.begin_frame(ui.ctx());

        let content_widths = match ctx.diff_ctx {
            Some(diff_ctx) => self.content_widths(ui, diff_ctx),
            None => [0.0; 2],
        };

        log::trace!("=================================================");

        let available_width = ui.available_width();
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
        let wrap = *ctx.word_wrap;
        if !wrap {
            self.row_heights = None;
        }
        let row_heights = &mut self.row_heights;
        let occurrences = &mut self.occurrences;
        let pin_input = &mut self.pin_input;
        let split = &mut self.split;
        let header_height = &mut self.header_height;
        let center_layout_cache = &mut self.center_layout;
        let mono_font = egui::TextStyle::Monospace.resolve(ui.style());
        let egui_ctx = ui.ctx().clone();
        let keymap = ctx.keymap;
        let hint = |action: FileAction, text: &str| with_shortcut(keymap, action, text);
        let action_hint = |action: FileAction| hint(action, action.label());

        ui.horizontal(|ui| {
            ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                viewer_override_combo(ui, ctx.viewer_override, ctx.viewer_fallback);
            });
            ui.separator();

            ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                toggle_button(
                    ui,
                    &mut ctx.diff_options.ignore.whitespace,
                    egui::RichText::new("W").strong(),
                    &hint(FileAction::IgnoreWhitespace, "Ignore Whitespace"),
                );
                toggle_button(
                    ui,
                    &mut ctx.diff_options.ignore.comments,
                    egui::RichText::new("C").strong(),
                    &hint(FileAction::IgnoreComments, "Ignore Comments"),
                );
                ignore_patterns_btn(
                    ui,
                    TOGGLE_BUTTON_SIZE,
                    &mut ctx.diff_options.ignore.patterns,
                    ctx.diff_options.ignore.regex_enabled,
                    ctx.regex_ignore_presets,
                    &hint(
                        FileAction::IgnoreRegex,
                        "Ignore Regex: text matching these patterns is ignored and dimmed",
                    ),
                );
                toggle_button(
                    ui,
                    &mut ctx.diff_options.highlight_rows,
                    egui::RichText::new("H").strong(),
                    &hint(FileAction::HighlightRows, "Highlight Rows"),
                );
                toggle_button(
                    ui,
                    &mut ctx.diff_options.ghost_rows,
                    "👻",
                    &hint(FileAction::GhostRows, "Ghost Rows"),
                );
                toggle_button(
                    ui,
                    ctx.h_scroll_linked,
                    "🔗",
                    &hint(FileAction::LinkHScroll, "Link Horizontal Scrolling"),
                );
                toggle_button(
                    ui,
                    ctx.word_wrap,
                    "↩",
                    &hint(FileAction::WordWrap, "Word Wrap"),
                );

                let mut active = ctx.diff_options.diff_only_with_extra_rows.is_some();
                if toggle_button(
                    ui,
                    &mut active,
                    egui::RichText::new("D").strong(),
                    &hint(FileAction::DiffOnly, "Diff Only"),
                ) {
                    ctx.diff_options.diff_only_with_extra_rows = active.then_some(2);
                }
                match &mut ctx.diff_options.diff_only_with_extra_rows {
                    Some(diff_rows_num) => {
                        if ui
                            .button("<")
                            .on_hover_text(action_hint(FileAction::FewerContextRows))
                            .clicked()
                        {
                            *diff_rows_num = diff_rows_num.saturating_sub(1);
                        }
                        if ui
                            .button(format!("{}", diff_rows_num.to_string(),))
                            .clicked()
                        {}
                        if ui
                            .button(">")
                            .on_hover_text(action_hint(FileAction::MoreContextRows))
                            .clicked()
                        {
                            *diff_rows_num = (*diff_rows_num + 1).min(99);
                        }
                    }
                    None => {}
                }
            });
            ui.separator();

            ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if ui
                    .button("<")
                    .on_hover_text(action_hint(FileAction::PrevConflict))
                    .clicked()
                {
                    ctx.conflict_cursor.dec();
                }
                if ui
                    .button(format!(
                        "{}/{}",
                        ctx.conflict_cursor.get().to_string(),
                        ctx.conflict_cursor.get_max().to_string()
                    ))
                    .clicked()
                {}
                if ui
                    .button(">")
                    .on_hover_text(action_hint(FileAction::NextConflict))
                    .clicked()
                {
                    ctx.conflict_cursor.inc();
                }
            });
            ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if ui
                    .button("<")
                    .on_hover_text(action_hint(FileAction::PrevFind))
                    .clicked()
                {
                    ctx.find_cursor.dec();
                }
                let find_k = if ctx.find_count == 0 {
                    0
                } else {
                    ctx.find_cursor.get() + 1
                };
                if ui
                    .button(format!("{find_k} of {}", ctx.find_count))
                    .clicked()
                {}
                if ui
                    .button(">")
                    .on_hover_text(action_hint(FileAction::NextFind))
                    .clicked()
                {
                    ctx.find_cursor.inc();
                }
            });
            ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                pins_ui(ui, pin_input, ctx.pins, ctx.pins_request, ctx.diff_ctx);
            });
            // No diff ctx while a diff is in flight, so a partial time is never shown.
            if let Some(diff_ctx) = ctx.diff_ctx {
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    let (label, tooltip) =
                        diff_status_text(diff_ctx.num_add_deletes, &diff_ctx.stage_times);
                    let label = diff_status_job(
                        &label,
                        diff_ctx.num_add_deletes,
                        &egui::TextStyle::Body.resolve(ui.style()),
                        [
                            role_color(ui.ctx(), SharedRole::InsertText),
                            role_color(ui.ctx(), SharedRole::DeleteText),
                        ],
                    );
                    // Not selectable, so a drag-copy over the rows can't pick it up.
                    ui.add(egui::Label::new(label).selectable(false))
                        .on_hover_text(tooltip);
                });
            } else if let Some(hex_view) = &ctx.hex_view {
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(hex_view.status_text()).selectable(false));
                });
            } else if let Some(image_view) = &ctx.image_view {
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    let (text, size_mismatch) = image_view.status_text();
                    let mut text = egui::RichText::new(text);
                    if size_mismatch {
                        text = text.color(ui.visuals().warn_fg_color);
                    }
                    ui.add(egui::Label::new(text).selectable(false));
                });
            }
        });

        let diff_rows = ctx.diff_ctx.as_ref().and_then(|f| Some(&f.diff_rows));
        let source_path = ctx
            .diff_ctx
            .map(|f| f.input.file_1.as_ref().and_then(|f| Some(&f.path)))
            .unwrap_or_default();
        let target_path = ctx
            .diff_ctx
            .map(|f| f.input.file_2.as_ref().and_then(|f| Some(&f.path)))
            .unwrap_or_default();

        let press_pos = ui.input(|i| {
            i.pointer
                .primary_pressed()
                .then(|| i.pointer.interact_pos())
                .flatten()
        });
        if ctx.toggle_active_side {
            self.active_side.toggle();
        }
        let active_side =
            self.active_side
                .begin_frame(press_pos, source_path.is_some(), target_path.is_some());
        *ctx.active_side = active_side;

        let mut waiting_for_diff = false;
        let mut do_not_render_diff = match (&diff_rows, source_path, target_path) {
            (Some(_), None, None) | (None, None, None) => true,
            (None, Some(_), None) => true,
            (None, None, Some(_)) => true,
            (Some(_), Some(_), None) | (Some(_), None, Some(_)) => false,
            (None, Some(_), Some(_)) => {
                waiting_for_diff = true;
                true
            }
            (Some(_), Some(_), Some(_)) => false,
        };
        waiting_for_diff |= ctx.diff_loading;
        do_not_render_diff |= waiting_for_diff;
        // The hex and image viewers draw the loaded bytes and need no text diff.
        let other_viewer =
            ctx.hex_view.is_some() || ctx.image_view.is_some() || ctx.media_view.is_some();
        if other_viewer {
            waiting_for_diff = false;
            do_not_render_diff = false;
        }
        let diff_rows_len = diff_rows.map(|f| f.len()).unwrap_or_else(|| 0);

        ui.add_space(4.0);
        ui.style_mut().override_text_style = Some(egui::TextStyle::Monospace);
        ui.spacing_mut().item_spacing.y = 0.0;

        let table_height = ui.available_height();

        let mut side_rects = [egui::Rect::NOTHING; 2];
        let mut table_rect = egui::Rect::NOTHING;
        let mut header_rects = [egui::Rect::NOTHING; 2];
        let mut browse: [Option<String>; 2] = [None, None];
        ui.vertical(|ui| {
            ui.set_min_width(available_width);

            if table_height > 0.0 && (diff_rows_len > 0 || other_viewer) {
                table_rect = ui.allocate_ui(egui::vec2(ui.available_width(), table_height), |ui| {
                    egui::Frame::default()
                        .fill(role_color(ui.ctx(), FileRole::TableBackground))
                        .show(ui, |ui| {
                            let area = ui.available_rect_before_wrap();
                            let button_padding = ui.spacing().button_padding.x;
                            let center = center_layout(
                                center_layout_cache,
                                &mono_font,
                                ui.ctx().pixels_per_point(),
                                || {
                                    ui.fonts_mut(|fonts| {
                                        CenterLayout::measure(
                                            |c| fonts.glyph_width(&mono_font, c),
                                            button_padding,
                                        )
                                    })
                                },
                            );
                            let side_widths = split.widths(shared_width(
                                area.width(),
                                center.width(),
                                ui.spacing().item_spacing.x,
                            ));
                            let mut table = SplitTable::new("file_diff_table", &mut *split)
                                .center_width(center.width());
                            // Images are drawn below the header (the path editors), in the
                            // table's columns, outside its scroll area so the wheel zooms.
                            let mut image_left_width = None;

                            if let Some(ScrollSpan { start, maybe_end }) = &ctx.scroll_to_row_span {
                                log::trace!("scroll_to_row_span: ({:?}, {:?})", start, maybe_end);
                                table = table.scroll_to_row(*start, Some(egui::Align::Min));
                            }

                            let editable_path_test =
                                |ui: &mut egui::Ui,
                                 id: egui::Id,
                                 file: &UniversalPath,
                                 label: Option<&str>,
                                 is_valid: bool,
                                 spinner_active: bool|
                                 -> Option<UniversalPath> {
                                    let original_path = file.to_string();
                                    let shown = label.unwrap_or(&original_path).to_string();

                                    let mut text = ui.memory_mut(|mem| {
                                        mem.data
                                            .get_temp::<String>(id)
                                            .unwrap_or_else(|| shown.clone())
                                    });

                                    let response = ui
                                        .scope(|ui| {
                                            if !is_valid {
                                                let bg = egui::Color32::from_rgb(45, 10, 10);
                                                let stroke =
                                                    egui::Stroke::new(1.0, egui::Color32::RED);

                                                {
                                                    let visuals = &mut ui.visuals_mut();
                                                    visuals.text_edit_bg_color = Some(bg);
                                                }
                                                let visuals = &mut ui.visuals_mut().widgets;

                                                visuals.inactive.bg_fill = bg;
                                                visuals.inactive.bg_stroke = stroke;

                                                visuals.hovered.bg_fill = bg;
                                                visuals.hovered.bg_stroke = stroke;

                                                visuals.active.bg_fill = bg;
                                                visuals.active.bg_stroke = stroke;
                                            }

                                            let response = ui.horizontal(|ui| {
                                                let spinner = egui::Spinner::new();
                                                let res = ui.add_sized(
                                                    [
                                                        ui.available_width(),
                                                        ui.spacing().interact_size.y,
                                                    ],
                                                    egui::TextEdit::singleline(&mut text).id(id),
                                                );
                                                if spinner_active {
                                                    let spinner_size = res.rect.size().y * 0.5;
                                                    let spinner_x = res.rect.right() - spinner_size;
                                                    let spinner_pos = egui::Pos2::new(
                                                        spinner_x,
                                                        res.rect.top() + spinner_size,
                                                    );
                                                    let spinner_rect = egui::Rect::from_center_size(
                                                        spinner_pos,
                                                        Vec2::new(spinner_size, spinner_size),
                                                    );
                                                    spinner.paint_at(ui, spinner_rect);
                                                }

                                                res
                                            });
                                            response.inner
                                        })
                                        .inner;

                                    if response.changed() {
                                        ui.memory_mut(|mem| mem.data.insert_temp(id, text.clone()));
                                    } else if response.gained_focus() && label.is_some() {
                                        ui.memory_mut(|mem| {
                                            mem.data.insert_temp(id, original_path.clone())
                                        });
                                    }

                                    // lost_focus not called correctly, quick fix: https://github.com/emilk/egui/issues/2142
                                    // Does not handle holding down mouse on another text field.....
                                    if response.lost_focus() || response.clicked_elsewhere() {
                                        ui.memory_mut(|mem| mem.data.remove::<String>(id));

                                        // clicked_elsewhere also fires unfocused, with the label as text.
                                        if text != original_path && text != shown {
                                            return Some(UniversalPath::from(&text));
                                        }
                                    }

                                    match (label.is_some(), is_valid) {
                                        (true, true) => {
                                            response.on_hover_text(&original_path);
                                        }
                                        (true, false) => {
                                            response.on_hover_text(format!(
                                                "{original_path}\nInvalid path"
                                            ));
                                        }
                                        (false, false) => {
                                            response.on_hover_text("Invalid path");
                                        }
                                        (false, true) => {}
                                    }

                                    None
                                };
                            let dim = file_path_dim(
                                [&ctx.file_source_path, &ctx.file_target_path],
                                ctx.file_source_label.is_some() || ctx.file_target_label.is_some(),
                            );
                            let file_path_bar = |ui: &mut egui::Ui,
                                                 id_salt: &str,
                                                 path: &UniversalPath,
                                                 label: Option<&str>,
                                                 root: Option<&UniversalPath>,
                                                 is_valid: bool,
                                                 spinner_active: bool|
                             -> (Option<UniversalPath>, Option<String>) {
                                let segments = file_path_segments(path, label);
                                let full_path = path.to_string();
                                let error = match (label, is_valid) {
                                    (_, true) => None,
                                    (Some(_), false) => Some(format!("{full_path}\nInvalid path")),
                                    (None, false) => Some("Invalid path".to_owned()),
                                };
                                let bar = ui.scope(|ui| {
                                    path_bar(
                                        ui,
                                        id_salt,
                                        &segments,
                                        dim,
                                        |i| i + 1 < segments.len(),
                                        error.as_deref(),
                                        true,
                                    )
                                });
                                let rect = bar.response.rect;
                                if label.is_some() && is_valid && ui.rect_contains_pointer(rect) {
                                    bar.response.show_tooltip_text(&full_path);
                                }
                                if spinner_active {
                                    let spinner_size = rect.height() * 0.5;
                                    let spinner_rect = egui::Rect::from_center_size(
                                        egui::pos2(
                                            rect.right() - spinner_size,
                                            rect.top() + spinner_size,
                                        ),
                                        Vec2::splat(spinner_size),
                                    );
                                    egui::Spinner::new().paint_at(ui, spinner_rect);
                                }
                                match bar.inner {
                                    Some(PathBarAction::Segment(i)) => {
                                        (None, Some(segments[i].path.clone()))
                                    }
                                    Some(PathBarAction::Commit(text)) => {
                                        (file_path_request(&text, path, root), None)
                                    }
                                    None => (None, None),
                                }
                            };
                            let header = |column: SplitColumn, ui: &mut egui::Ui| {
                                // Clipped cells truncate text; the path bars lay out as before.
                                ui.style_mut().wrap_mode = None;
                                match column {
                                    SplitColumn::Left => {
                                        header_rects[0] = ui
                                            .vertical(|ui| {
                                                *ctx.set_file_1_root_request = editable_path_test(
                                                    ui,
                                                    "file_1_path_editor_path".into(),
                                                    &ctx.file_source_root.clone().unwrap_or_default(),
                                                    None,
                                                    ctx.file_source_root_valid,
                                                    ctx.file_source_loading,
                                                );
                                                ui.separator();
                                                (*ctx.load_file_1_request, browse[0]) = file_path_bar(
                                                    ui,
                                                    "file_1_path_bar",
                                                    &ctx.file_source_path,
                                                    ctx.file_source_label.as_deref(),
                                                    ctx.file_source_root.as_ref(),
                                                    ctx.file_source_path_valid,
                                                    ctx.file_source_loading,
                                                );
                                                ui.separator();
                                            })
                                            .response
                                            .rect;
                                    }
                                    SplitColumn::Center => {}
                                    SplitColumn::Right => {
                                        header_rects[1] = ui
                                            .vertical(|ui| {
                                                *ctx.set_file_2_root_request = editable_path_test(
                                                    ui,
                                                    "file_2_path_editor_path".into(),
                                                    &ctx.file_target_root.clone().unwrap_or_default(),
                                                    None,
                                                    ctx.file_target_root_valid,
                                                    ctx.file_target_loading,
                                                );
                                                ui.separator();
                                                (*ctx.load_file_2_request, browse[1]) = file_path_bar(
                                                    ui,
                                                    "file_2_path_bar",
                                                    &ctx.file_target_path,
                                                    ctx.file_target_label.as_deref(),
                                                    ctx.file_target_root.as_ref(),
                                                    ctx.file_target_path_valid,
                                                    ctx.file_target_loading,
                                                );
                                                ui.separator();
                                            })
                                            .response
                                            .rect;
                                    }
                                }
                            };
                            let header_only = ctx.image_view.is_some()
                                || ctx.media_view.is_some()
                                || do_not_render_diff;
                            let mut offsets = [*ctx.scroll_left, *ctx.scroll_right];
                            // Wrapped text fits its column, so nothing scrolls sideways. Hex rows
                            // have a fixed width, images pan.
                            if !(wrap || header_only || ctx.hex_view.is_some()) {
                                table = table.h_scroll(
                                    &mut offsets,
                                    *ctx.h_scroll_linked,
                                    content_widths,
                                );
                            }
                            let rows = if let Some(hex_view) = &ctx.hex_view {
                                Rows::Uniform {
                                    height: row_height,
                                    count: hex::row_count(hex_view),
                                }
                            } else if header_only {
                                Rows::Uniform {
                                    height: row_height,
                                    count: 0,
                                }
                            } else {
                                let heights = match ctx.diff_ctx {
                                    Some(diff_ctx) if wrap => Some(wrapped_heights(
                                        row_heights,
                                        &egui_ctx,
                                        &mono_font,
                                        diff_ctx,
                                        side_widths.map(wrap_width),
                                    )),
                                    _ => None,
                                };
                                text_rows(heights, row_height, diff_rows_len)
                            };

                                    // Once per frame, not per row: a temp side gets no revert buttons.
                                    let temp_root = std::env::temp_dir();
                                    let shown_pins = ctx.diff_ctx.map_or(&[][..], shown_pins);
                                    let symbol_colors = [
                                        role_color(&egui_ctx, SharedRole::DeleteText),
                                        role_color(&egui_ctx, SharedRole::InsertText),
                                        role_color(&egui_ctx, FileRole::Modified),
                                    ];
                                    let overlay = ctx
                                        .diff_ctx
                                        .filter(|_| ctx.phase_overlay)
                                        .and_then(|d| d.phase_trace.as_deref());
                                    let cell = |row_index: usize, column: SplitColumn, ui: &mut egui::Ui| {
                                            if let Some(hex_view) = &ctx.hex_view {
                                                hex::cell(ui, hex_view, row_index, column);
                                                return;
                                            }
                                            if let Some(rows) = diff_rows {
                                                let diff_row = &rows[row_index];
                                                let is_highlighted =
                                                    ctx.active_highlights.contains(&row_index);
                                                let highlight_side = ctx.highlight_side;
                                                let lit = |side| {
                                                    is_highlighted
                                                        && highlight_side.is_none_or(|s| s == side)
                                                };
                                                let accent_line_num = |side| {
                                                    is_highlighted && highlight_side == Some(side)
                                                };
                                                let (find_matcher, find_hit) =
                                                    (ctx.find_matcher, ctx.find_hit);

                                                match column {
                                                SplitColumn::Left | SplitColumn::Right => {
                                                    let (side, content) = if column == SplitColumn::Left {
                                                        (ActiveSide::Left, &diff_row.left)
                                                    } else {
                                                        (ActiveSide::Right, &diff_row.right)
                                                    };
                                                    let width = ui.max_rect().width();
                                                        Self::render_side_row(
                                                            ui,
                                                            side,
                                                            row_index,
                                                            occurrences,
                                                            ctx.diff_ctx
                                                                .map(|f| f.input.file_1.clone())
                                                                .unwrap_or_default(),
                                                            ctx.diff_ctx
                                                                .map(|f| f.input.file_2.clone())
                                                                .unwrap_or_default(),
                                                            content,
                                                            width,
                                                            wrap,
                                                            lit(side),
                                                            accent_line_num(side),
                                                            row_find(
                                                                find_matcher,
                                                                find_hit,
                                                                side,
                                                                row_index,
                                                                content,
                                                            ),
                                                            active_side == side,
                                                            ctx.code_language,
                                                            overlay,
                                                        );
                                                    let pinned = is_pinned(shown_pins, side, content);
                                                    if pinned || overlay.is_some() {
                                                        let cell_ui = ui.new_child(
                                                            UiBuilder::new().max_rect(unscrolled_cell(ui)),
                                                        );
                                                        if pinned {
                                                            paint_pin_marker(&cell_ui);
                                                        }
                                                        if let Some(trace) = overlay {
                                                            paint_trace_gutter(&cell_ui, trace, side, content);
                                                        }
                                                    }
                                                }

                                                SplitColumn::Center => {
                                                    ui.style_mut().wrap_mode = None;
                                                    let [expand_slot, toggle_slot, left_slot, symbol_slot, right_slot] =
                                                        center.slots(ui.max_rect(), ui.spacing().interact_size.y);
                                                    let diff_ctx = ctx.diff_ctx;
                                                    let hunk = diff_ctx.and_then(|d| {
                                                        revert::hunk_starting_at(&d.precomputed_diffs, row_index)
                                                            .map(|hunk| (d, hunk))
                                                    });
                                                    let mut revert_button = |ui: &mut egui::Ui, slot: egui::Rect, target: RevertTarget, label: &str, hover: &str| {
                                                        let Some((diff_ctx, hunk)) = hunk else {
                                                            return;
                                                        };
                                                        if revert::check_revert(diff_ctx, target, &temp_root).is_ok()
                                                            && ui.put(slot, egui::Button::new(label)).on_hover_text(hover).clicked()
                                                        {
                                                            *ctx.revert_request = Some(RevertRequest { hunk, target });
                                                        }
                                                    };
                                                    // A collapsed row, or the first row of an expanded block.
                                                    let block = diff_ctx.and_then(|d| {
                                                        d.row_blocks
                                                            .binary_search_by_key(&row_index, |b| b.rows.start)
                                                            .ok()
                                                            .map(|i| &d.row_blocks[i])
                                                    });
                                                    if let Some(block) = block {
                                                        let mut toggle_button = |slot: egui::Rect, label: &str, hover: &str, toggle: BlockToggle| {
                                                            if ui.put(slot, egui::Button::new(label)).on_hover_text(hover).clicked() {
                                                                *ctx.block_toggle_request = Some((block.key, toggle));
                                                            }
                                                        };
                                                        if !block.expanded {
                                                            toggle_button(expand_slot, "+", "Expand the collapsed rows", BlockToggle::Expand);
                                                        }
                                                        // Only on a fully collapsed block: once rows are revealed, the
                                                        // scope expansion has been done or wouldn't add anything.
                                                        if !block.expanded && block.rows.len() == 1 {
                                                            toggle_button(toggle_slot, "{", "Expand up to the line opening the scope of the change below", BlockToggle::ExpandToScope);
                                                        } else {
                                                            toggle_button(toggle_slot, "-", "Collapse these rows again", BlockToggle::Collapse);
                                                        }
                                                    }
                                                    revert_button(ui, left_slot, RevertTarget::Left, "<", "Replace this hunk in the left file with the right side");
                                                    if let Some((symbol, color)) = row_symbol(diff_row, symbol_colors) {
                                                        ui.put(
                                                            symbol_slot,
                                                            egui::Label::new(egui::RichText::new(symbol).color(color))
                                                                .selectable(false),
                                                        );
                                                    }
                                                    revert_button(ui, right_slot, RevertTarget::Right, ">", "Replace this hunk in the right file with the left side");
                                                }
                                                }
                                            }
                                        };
                            // The table fills what it is given, so header-only gets just the header.
                            // One child id in both cases keeps the path bars' state across a switch.
                            let rect = if header_only {
                                egui::Rect::from_min_size(
                                    area.min,
                                    egui::vec2(area.width(), *header_height),
                                )
                            } else {
                                area
                            };
                            let mut table_ui = ui.new_child(
                                UiBuilder::new()
                                    .max_rect(rect)
                                    .id_salt("file_diff_table_ui"),
                            );
                            let output =
                                table.show(&mut table_ui, *header_height, header, rows, cell);
                            ui.advance_cursor_after_rect(rect);
                            [*ctx.scroll_left, *ctx.scroll_right] = offsets;
                            let measured = header_rects[0].height().max(header_rects[1].height());
                            if measured != *header_height {
                                *header_height = measured;
                                ui.ctx().request_repaint();
                            }
                            if ctx.image_view.is_some() || ctx.video_frames.is_some() {
                                image_left_width = Some(output.side_rects[0].width());
                            } else if !header_only {
                                side_rects = output.side_rects;
                            }
                            if let (Some(image_view), Some(left_width)) =
                                (ctx.image_view.as_deref_mut(), image_left_width)
                            {
                                image::show(ui, image_view, left_width);
                            }
                            if let (Some(meta), Some(frames), Some(left_width)) = (
                                ctx.video_meta.as_deref_mut(),
                                ctx.video_frames.as_deref_mut(),
                                image_left_width,
                            ) {
                                let audio = ctx
                                    .audio_view
                                    .as_mut()
                                    .map(|(processor, settings)| (&mut **processor, &mut **settings));
                                video_meta::show(ui, meta, frames, left_width, audio);
                            } else if let Some((processor, settings)) = ctx.audio_view.as_mut() {
                                audio::show(ui, processor, settings);
                            } else if let Some(kind) = ctx.media_view {
                                media_placeholder(
                                    ui,
                                    kind,
                                    &ctx.file_source_path,
                                    &ctx.file_target_path,
                                );
                            }
                            if let Some(processor) = ctx.audio_meta.as_deref_mut() {
                                audio_meta::show(ui, processor);
                            }
                        });
                })
                .response
                .rect;
            }

            if waiting_for_diff {
                ui.vertical_centered(|ui| {
                    // Hack to simulate ui.centered_and_justified to make spinner work
                    let height = ui.available_height();
                    ui.add_space(height / 2.0 - 30.0);

                    ui.label("Waiting for diff results...");
                    ui.add_space(8.0);
                    ui.add(egui::Spinner::new().size(64.0));
                });
            } else if do_not_render_diff {
                ui.centered_and_justified(|ui| {
                    if let Some(error) = ctx.launch_error {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    } else {
                        ui.label("Load Source & Target files to see diff.");
                    }
                });
            }
        });

        for (i, side) in [ActiveSide::Left, ActiveSide::Right]
            .into_iter()
            .enumerate()
        {
            if let Some(folder) = browse[i].take() {
                // Opened anew even when one is open: the side may have switched local <-> depot.
                let source: Arc<dyn FolderSource> =
                    if UniversalPath::from(folder.as_str()).is_depot() {
                        Arc::new(DepotFolder)
                    } else {
                        Arc::new(LocalFolder)
                    };
                self.browsers[i] = Some(FolderBrowser::open(&egui_ctx, folder, source));
            }
            // The table and its path bars are skipped while no diff is shown.
            let Some(browser) = self.browsers[i]
                .as_mut()
                .filter(|_| table_rect.is_positive())
            else {
                continue;
            };
            let rect = egui::Rect::from_x_y_ranges(
                header_rects[i].x_range(),
                header_rects[i].bottom()..=table_rect.bottom(),
            );
            // An area of its own, above the rows, so they and the table's scroll get no input.
            let action = egui::Area::new(ui.id().with(("folder_browser", i)))
                .fixed_pos(rect.min)
                .constrain(false)
                .show(&egui_ctx, |ui| {
                    egui::Frame::new()
                        .fill(ui.visuals().panel_fill)
                        .stroke(ui.visuals().window_stroke())
                        .inner_margin(4.0)
                        .show(ui, |ui| {
                            let size = (rect.size() - Vec2::splat(8.0)).max(Vec2::ZERO);
                            ui.set_min_size(size);
                            ui.set_max_size(size);
                            browser.ui(ui)
                        })
                        .inner
                })
                .inner;
            match action {
                Some(BrowserAction::Close) => self.browsers[i] = None,
                Some(BrowserAction::Pick(picked)) => {
                    self.browsers[i] = None;
                    let paths = [&ctx.file_source_path, &ctx.file_target_path];
                    let roots = [ctx.file_source_root.as_ref(), ctx.file_target_root.as_ref()];
                    let [left, right] = browser_pick(side, &picked, paths, roots);
                    if left.is_some() {
                        *ctx.load_file_1_request = left;
                    }
                    if right.is_some() {
                        *ctx.load_file_2_request = right;
                    }
                }
                None => {}
            }
        }
        // Checked after every path bar ran: one that ends an edit consumes its Esc.
        if self.browsers.iter().any(Option::is_some)
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.browsers = [None, None];
        }

        let active_rect = side_rects[h_scroll::side_index(active_side)];
        if active_rect.is_positive() {
            ui.painter().rect_stroke(
                active_rect,
                0.0,
                outline_stroke(),
                egui::StrokeKind::Inside,
            );
        }
        let [left_rect, right_rect] = side_rects;
        self.active_side.end_frame(left_rect, right_rect);
        self.occurrences.end_frame(ui.ctx());
        if !other_viewer {
            ui.ctx()
                .data_mut(|data| data.insert_temp(wheel_rects_id(), side_rects));
        }

        handle_drops(
            ui,
            &mut ctx.load_file_1_request,
            &mut ctx.load_file_2_request,
            left_rect,
            right_rect,
        );

        egui_tiles::UiResponse::None
    }

    pub(super) fn render_side_row<T: RawTokenTrait>(
        ui: &mut egui::Ui,
        side: ActiveSide,
        row_index: usize,
        occurrences: &mut OccurrenceState,
        file_source: Option<Arc<CachedFile<T>>>,
        file_target: Option<Arc<CachedFile<T>>>,
        content: &LineContent,
        width: f32,
        wrap: bool,
        is_highlighted: bool,
        accent_line_num: bool,
        find: RowFind,
        selectable: bool,
        code_language: &str,
        trace: Option<&PhaseTrace>,
    ) {
        let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
        // Wrapped rows fill their cell, which is as tall as the row's taller side.
        let cell_h = if wrap {
            ui.max_rect().height().max(row_h)
        } else {
            row_h
        };

        let (rect, _) = ui.allocate_at_least(egui::vec2(width, cell_h), egui::Sense::hover());
        let mut extended_rect = rect.clone();
        extended_rect.extend_with_x(9999999.0);

        match content {
            LineContent::Code {
                tokens,
                line_num,
                bg,
                changed_chars,
            } => {
                ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                    let add_contents = |ui: &mut egui::Ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;

                        let line_num_str = if *line_num > 0 {
                            line_num.to_string()
                        } else {
                            String::new()
                        };

                        let line_num_text = egui::RichText::new(&line_num_str).size(10.0);
                        let line_num_text = if accent_line_num {
                            line_num_text
                                .color(role_color(ui.ctx(), FileRole::GotoLineNumber))
                                .strong()
                        } else {
                            line_num_text.color(role_color(ui.ctx(), FileRole::LineNumber))
                        };
                        ui.add_sized(
                            [GUTTER_WIDTH, row_h],
                            egui::Label::new(line_num_text).selectable(false),
                        );

                        ui.add_space(GUTTER_GAP);

                        let row_text = build_row_text(
                            tokens,
                            changed_chars,
                            file_source.as_deref(),
                            file_target.as_deref(),
                        );

                        let theme = egui_extras::syntax_highlighting::CodeTheme::from_memory(
                            ui.ctx(),
                            ui.style(),
                        );
                        let mut layout_job = egui_extras::syntax_highlighting::highlight(
                            ui.ctx(),
                            ui.style(),
                            &theme,
                            &row_text.text,
                            code_language,
                        );
                        let style_ctx = ui.ctx().clone();
                        let color = |role| role_color(&style_ctx, role);
                        // Text matched by an ignore pattern is dimmed over the syntax colors.
                        recolor_ranges(
                            &mut layout_job,
                            &row_text.dimmed,
                            color(FileRole::DimmedText),
                        );
                        let changed_bg = match side {
                            ActiveSide::Left => color(FileRole::DeleteChangedChars),
                            ActiveSide::Right => color(FileRole::InsertChangedChars),
                        };
                        highlight_ranges(&mut layout_job, &row_text.changed_chars, changed_bg);
                        let found = occurrences.ranges(side, row_index, &row_text.text);
                        highlight_ranges(&mut layout_job, &found, color(FileRole::Occurrence));
                        // After the occurrences, so a find match shows over one.
                        let (find_all, find_current) = find_ranges(&row_text.text, find);
                        highlight_ranges(&mut layout_job, &find_all, color(FileRole::Find));
                        if let Some(current) = find_current {
                            highlight_ranges(
                                &mut layout_job,
                                &[current],
                                color(FileRole::FindCurrent),
                            );
                        }

                        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
                        let wrap_at = if wrap {
                            wrap_width(width)
                        } else {
                            f32::INFINITY
                        };
                        let ghost_color = color(FileRole::GhostText);
                        let side_text = ui.fonts_mut(|fonts| {
                            layout_side_text(
                                fonts,
                                layout_job,
                                &row_text.ghosts,
                                ghost_color,
                                &font_id,
                                wrap_at,
                            )
                        });
                        let galley = side_text.galley.clone();
                        // The hit area spans the rest of the row, not just the text, so a press
                        // anywhere in the block starts a selection. Drag sense on both sides:
                        // egui hit-tests against the previous frame, so the side that becomes
                        // selectable on a press needs it already, or that press is lost.
                        let hit_size = egui::vec2(
                            galley.size().x.max(ui.available_width()),
                            side_text.height().max(cell_h),
                        );
                        let (hit_rect, response) =
                            ui.allocate_exact_size(hit_size, egui::Sense::click_and_drag());
                        let text_color = ui.style().visuals.text_color();
                        if selectable {
                            let marker = if tokens.iter().all(|(_, _, is_ghost)| *is_ghost) {
                                Some(COPY_MARKER_NO_LINE)
                            } else if row_text.text.is_empty() {
                                Some(COPY_MARKER_BLANK_LINE)
                            } else {
                                None
                            };
                            if marker.is_none() {
                                occurrences.track(
                                    ui,
                                    &response,
                                    hit_rect.left_top(),
                                    &galley,
                                    side,
                                    row_index,
                                );
                            }
                            let galley = match marker {
                                Some(marker) => copy_marker_galley(ui, marker),
                                None => galley,
                            };
                            let galley = if wrap {
                                reach_down_to(galley, hit_rect.height())
                            } else {
                                galley
                            };
                            LabelSelectionState::label_text_selection(
                                ui,
                                &response,
                                hit_rect.left_top(),
                                galley,
                                text_color,
                                egui::Stroke::NONE,
                            );
                        } else {
                            ui.painter().add(egui::epaint::TextShape::new(
                                hit_rect.left_top(),
                                galley,
                                text_color,
                            ));
                        }
                        for (pos, ghost_galley) in side_text.ghosts {
                            ui.painter().add(egui::epaint::TextShape::new(
                                hit_rect.left_top() + pos,
                                ghost_galley,
                                text_color,
                            ));
                        }

                        let bg = match bg {
                            RowBg::None => egui::Color32::TRANSPARENT,
                            RowBg::Delete => role_color(ui.ctx(), SharedRole::DeleteBackground),
                            RowBg::Insert => role_color(ui.ctx(), SharedRole::InsertBackground),
                            RowBg::Custom(zdiff::diff_builder::Color32([r, g, b, a])) => {
                                egui::Color32::from_rgba_unmultiplied(*r, *g, *b, *a)
                            }
                        };
                        ui.painter().rect_filled(extended_rect, 0.0, bg);
                        if is_highlighted {
                            ui.painter()
                                .rect_filled(extended_rect, 0.0, color(FileRole::GotoRow));
                        }
                        if let Some(trace) = trace {
                            let outlines = token_hunk_outlines(
                                trace,
                                tokens,
                                &row_text,
                                file_source.as_deref(),
                                file_target.as_deref(),
                            );
                            paint_trace_row(
                                ui.painter(),
                                &outlines,
                                &row_text,
                                &side_text.galley,
                                hit_rect.left_top(),
                            );
                        }
                    };
                    // A wrapped row's line number stays on its first line.
                    if wrap {
                        ui.horizontal_top(add_contents);
                    } else {
                        ui.horizontal_centered(add_contents);
                    }
                });
            }
            LineContent::Void => {
                let fill = if is_highlighted {
                    role_color(ui.ctx(), FileRole::GotoRow)
                } else {
                    role_color(ui.ctx(), FileRole::VoidRow)
                };
                ui.painter().rect_filled(extended_rect, 0.0, fill);

                if selectable {
                    // Full-row hit area like Code rows, so a press here starts a selection too.
                    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                        let (hit_rect, response) =
                            ui.allocate_exact_size(rect.size(), egui::Sense::click_and_drag());
                        let marker = copy_marker_galley(ui, COPY_MARKER_NO_LINE);
                        let marker = if wrap {
                            reach_down_to(marker, hit_rect.height())
                        } else {
                            marker
                        };
                        LabelSelectionState::label_text_selection(
                            ui,
                            &response,
                            hit_rect.left_top(),
                            marker,
                            egui::Color32::TRANSPARENT,
                            egui::Stroke::NONE,
                        );
                    });
                }
            }
            LineContent::Collapsed => {
                let fill = if is_highlighted {
                    egui::Color32::from_rgba_unmultiplied(38, 79, 120, 100) // Clear blue highlight
                } else {
                    egui::Color32::from_rgb(37, 43, 54) // Subdued slate/steel background
                };
                ui.painter().rect_filled(extended_rect, 0.0, fill);

                ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;

                        ui.add_sized(
                            [GUTTER_WIDTH, row_h],
                            egui::Label::new(
                            egui::RichText::new("|") // Mid-line ellipsis fits a gutter better than "-"
                            .color(egui::Color32::from_gray(100))
                            .size(12.0))
                            .selectable(false)
                        );

                        ui.add_space(GUTTER_GAP);

                        ui.add(
                            egui::Label::new(
                                egui::RichText::new("Collapsed context")
                                    .color(egui::Color32::from_gray(130))
                                    .size(11.0),
                            )
                            .selectable(false),
                        );

                        if selectable {
                            let galley = copy_marker_galley(ui, COPY_MARKER_NO_LINE);
                            let (hit_rect, response) = ui
                                .allocate_exact_size(galley.size(), egui::Sense::click_and_drag());
                            LabelSelectionState::label_text_selection(
                                ui,
                                &response,
                                hit_rect.left_top(),
                                galley,
                                egui::Color32::TRANSPARENT,
                                egui::Stroke::NONE,
                            );
                        }
                    });
                });
            }
        }
    }
}

/// Line number column of a code row, and the gap between it and the row text.
const GUTTER_WIDTH: f32 = 35.0;
const GUTTER_GAP: f32 = 4.0;
const CENTER_GAP: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct CenterLayout {
    button: f32,
    symbol: f32,
}

impl CenterLayout {
    fn measure(mut glyph_width: impl FnMut(char) -> f32, button_padding: f32) -> Self {
        let mut widest = |chars: &[char]| chars.iter().map(|&c| glyph_width(c)).fold(0.0, f32::max);
        Self {
            button: widest(&['+', '{', '-', '<', '>']) + 2.0 * button_padding,
            symbol: widest(&['≠', '-', '+']),
        }
    }

    fn width(self) -> f32 {
        4.0 * self.button + self.symbol + 4.0 * CENTER_GAP
    }

    /// Expand, scope or collapse, revert-left, symbol, revert-right; at most `height` tall,
    /// centered in `cell`.
    fn slots(self, cell: egui::Rect, height: f32) -> [egui::Rect; 5] {
        let half = height.min(cell.height()) / 2.0;
        let y = egui::Rangef::new(cell.center().y - half, cell.center().y + half);
        let mut left = cell.left();
        let button = self.button;
        [button, button, button, self.symbol, button].map(|width| {
            let slot = egui::Rect::from_x_y_ranges(left..=left + width, y);
            left += width + CENTER_GAP;
            slot
        })
    }
}

struct CenterLayoutCache {
    font_id: egui::FontId,
    pixels_per_point: f32,
    layout: CenterLayout,
}

fn center_layout(
    cache: &mut Option<CenterLayoutCache>,
    font_id: &egui::FontId,
    pixels_per_point: f32,
    measure: impl FnOnce() -> CenterLayout,
) -> CenterLayout {
    match cache {
        Some(cached)
            if cached.font_id == *font_id && cached.pixels_per_point == pixels_per_point =>
        {
            cached.layout
        }
        _ => {
            let layout = measure();
            *cache = Some(CenterLayoutCache {
                font_id: font_id.clone(),
                pixels_per_point,
                layout,
            });
            layout
        }
    }
}

fn row_symbol(
    row: &DiffRow,
    [delete, insert, modified]: [egui::Color32; 3],
) -> Option<(&'static str, egui::Color32)> {
    let has_op = |content: &LineContent, op: DiffOp| match content {
        LineContent::Code { tokens, .. } => tokens
            .iter()
            .any(|(result, _, _)| !result.hide_in_diff && result.operation == op),
        LineContent::Void | LineContent::Collapsed => false,
    };
    let has_delete = has_op(&row.left, DiffOp::Delete) || has_op(&row.right, DiffOp::Delete);
    let has_insert = has_op(&row.left, DiffOp::Insert) || has_op(&row.right, DiffOp::Insert);
    match (has_delete, has_insert) {
        (true, true) => Some(("≠", modified)),
        (true, false) => Some(("-", delete)),
        (false, true) => Some(("+", insert)),
        (false, false) => None,
    }
}

const PIN_MARKER: egui::Color32 = egui::Color32::from_rgb(90, 160, 255);

fn shown_pins(diff_ctx: &MinimalDiffCtx) -> &[Pin] {
    if diff_ctx.ignored_pins.is_some() {
        &[]
    } else {
        &diff_ctx.input.pins
    }
}

fn is_pinned(pins: &[Pin], side: ActiveSide, content: &LineContent) -> bool {
    let LineContent::Code { line_num, .. } = content else {
        return false;
    };
    let Ok(line) = usize::try_from(line_num - 1) else {
        return false;
    };
    pins.iter().any(|pin| match side {
        ActiveSide::Left => pin.source == line,
        ActiveSide::Right => pin.target == line,
    })
}

fn paint_pin_marker(ui: &egui::Ui) {
    let rect = ui.max_rect();
    let bar = egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height()));
    ui.painter().rect_filled(bar, 0.0, PIN_MARKER);
}

const LINE_HUNK_BAR: egui::Color32 = egui::Color32::from_rgb(230, 140, 40);
const SCOPE_PAIR_COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(255, 120, 200),
    egui::Color32::from_rgb(120, 220, 255),
    egui::Color32::from_rgb(255, 220, 90),
    egui::Color32::from_rgb(160, 255, 120),
    egui::Color32::from_rgb(200, 150, 255),
    egui::Color32::from_rgb(255, 150, 100),
];
const TOKEN_DELETE_OUTLINE: egui::Color32 = egui::Color32::from_rgb(255, 90, 90);
const TOKEN_INSERT_OUTLINE: egui::Color32 = egui::Color32::from_rgb(90, 230, 90);
const CHAR_UNDERLINE: egui::Color32 = egui::Color32::from_rgb(0, 220, 255);

/// Scope pairs are numbered like the Phase Trace window's table.
fn paint_trace_gutter(ui: &egui::Ui, trace: &PhaseTrace, side: ActiveSide, content: &LineContent) {
    let LineContent::Code { line_num, .. } = content else {
        return;
    };
    let Ok(line) = usize::try_from(line_num - 1) else {
        return;
    };
    let lines = |span: &TraceSpan| match side {
        ActiveSide::Left => span.source_lines.clone(),
        ActiveSide::Right => span.target_lines.clone(),
    };
    let rect = ui.max_rect();
    let painter = ui.painter();
    if trace
        .line_hunks
        .iter()
        .any(|hunk| lines(hunk).contains(&line))
    {
        let bar = egui::Rect::from_min_size(
            rect.min + egui::vec2(4.0, 0.0),
            egui::vec2(2.0, rect.height()),
        );
        painter.rect_filled(bar, 0.0, LINE_HUNK_BAR);
    }
    let x = rect.left() + GUTTER_WIDTH;
    for (i, pair) in trace.scope_pairs.iter().enumerate() {
        let lines = lines(pair);
        if !lines.contains(&line) {
            continue;
        }
        let stroke = egui::Stroke::new(1.5, SCOPE_PAIR_COLORS[i % SCOPE_PAIR_COLORS.len()]);
        let (first, last) = (line == lines.start, line + 1 == lines.end);
        let top = if first { rect.top() + 1.0 } else { rect.top() };
        let bottom = if last {
            rect.bottom() - 1.0
        } else {
            rect.bottom()
        };
        painter.vline(x, top..=bottom, stroke);
        if first {
            painter.hline(x..=x + GUTTER_GAP, top, stroke);
            let number = painter.layout_no_wrap(
                (i + 1).to_string(),
                egui::FontId::monospace(9.0),
                stroke.color,
            );
            let pos = egui::pos2(x - 1.0 - number.size().x, rect.top());
            let behind = egui::Rect::from_min_size(pos, number.size());
            painter.rect_filled(behind, 0.0, egui::Color32::from_black_alpha(200));
            painter.galley(pos, number, stroke.color);
        }
        if last {
            painter.hline(x..=x + GUTTER_GAP, bottom, stroke);
        }
    }
}

fn token_hunk_outlines<T: RawTokenTrait>(
    trace: &PhaseTrace,
    tokens: &[(DiffResult, IsDimmed, IsGhost)],
    row_text: &RowText,
    file_source: Option<&CachedFile<T>>,
    file_target: Option<&CachedFile<T>>,
) -> Vec<(std::ops::Range<usize>, egui::Color32)> {
    let in_token_hunk = |idx: Option<u32>, side: fn(&TraceSpan) -> &std::ops::Range<usize>| {
        idx.is_some_and(|i| {
            let i = i as usize;
            trace
                .token_hunks
                .iter()
                .any(|hunk| side(&hunk.span).contains(&i))
        })
    };
    let mut outlines = Vec::new();
    let mut start = 0;
    for token in tokens {
        let result = &token.0;
        // One token at a time, so each range is exactly what build_row_text adds to the row.
        let row = build_row_text(std::slice::from_ref(token), &[], file_source, file_target);
        let len = row.text.len();
        let range = start..start + len;
        start += len;
        if len == 0 || result.hide_in_diff {
            continue;
        }
        match result.operation {
            DiffOp::Delete if in_token_hunk(result.token_source_idx, |s| &s.source) => {
                outlines.push((range, TOKEN_DELETE_OUTLINE));
            }
            DiffOp::Insert if in_token_hunk(result.token_target_idx, |s| &s.target) => {
                outlines.push((range, TOKEN_INSERT_OUTLINE));
            }
            _ => {}
        }
    }
    debug_assert_eq!(start, row_text.text.len());
    outlines
}

/// `origin` is where `galley` was painted.
fn paint_trace_row(
    painter: &egui::Painter,
    outlines: &[(std::ops::Range<usize>, egui::Color32)],
    row_text: &RowText,
    galley: &egui::Galley,
    origin: egui::Pos2,
) {
    for (range, color) in outlines {
        for rect in text_range_rects(galley, &row_text.text, range.clone()) {
            painter.rect_stroke(
                rect.translate(origin.to_vec2()),
                0.0,
                egui::Stroke::new(1.0, *color),
                egui::StrokeKind::Inside,
            );
        }
    }
    for range in &row_text.changed_chars {
        for rect in text_range_rects(galley, &row_text.text, range.clone()) {
            let rect = rect.translate(origin.to_vec2());
            painter.hline(
                rect.x_range(),
                rect.bottom() - 1.0,
                egui::Stroke::new(1.5, CHAR_UNDERLINE),
            );
        }
    }
}

/// One galley-relative rect per laid-out row that the byte range of `text` covers.
fn text_range_rects(
    galley: &egui::Galley,
    text: &str,
    range: std::ops::Range<usize>,
) -> Vec<egui::Rect> {
    let start = text[..range.start].chars().count();
    let end = text[..range.end].chars().count();
    let mut rects = Vec::new();
    let mut row_start = 0;
    for row in &galley.rows {
        let row_end = row_start + row.glyphs.len();
        let (a, b) = (start.max(row_start), end.min(row_end));
        if a < b {
            let first = &row.glyphs[a - row_start];
            let last = &row.glyphs[b - 1 - row_start];
            rects.push(egui::Rect::from_x_y_ranges(
                row.pos.x + first.pos.x..=row.pos.x + last.pos.x + last.advance_width,
                row.pos.y..=row.pos.y + row.size.y,
            ));
        }
        row_start = row_end;
    }
    rects
}

fn pins_ui(
    ui: &mut egui::Ui,
    input: &mut PinInput,
    pins: &[Pin],
    pins_request: &mut Option<Vec<Pin>>,
    diff_ctx: Option<&MinimalDiffCtx>,
) {
    ui.label("Pin");
    let left = ui.add(
        TextEdit::singleline(&mut input.left)
            .hint_text("L")
            .desired_width(35.0),
    );
    let right = ui.add(
        TextEdit::singleline(&mut input.right)
            .hint_text("R")
            .desired_width(35.0),
    );
    if left.changed() || right.changed() {
        input.error = None;
    }
    let entered =
        (left.lost_focus() || right.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if ui.button("Add pin").clicked() || entered {
        let added = parse_pin(&input.left, &input.right).and_then(|pin| {
            let counts = diff_ctx
                .and_then(|d| pin_line_counts(&d.input))
                .ok_or("pins need both files loaded")?;
            insert_pin(pins, pin, counts).map_err(|err| err.to_string())
        });
        match added {
            Ok(pins) => {
                *pins_request = Some(pins);
                *input = PinInput::default();
            }
            Err(reason) => input.error = Some(reason),
        }
    }
    if !pins.is_empty() {
        ui.menu_button(format!("{} pinned", pins.len()), |ui| {
            for (i, pin) in pins.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("{} : {}", pin.source + 1, pin.target + 1));
                    if ui.small_button("x").on_hover_text("Remove pin").clicked() {
                        let mut kept = pins.to_vec();
                        kept.remove(i);
                        *pins_request = Some(kept);
                    }
                });
            }
            if ui.button("Clear all").clicked() {
                *pins_request = Some(Vec::new());
                ui.close();
            }
        });
    }
    let ignored = diff_ctx.and_then(|d| d.ignored_pins.as_ref());
    if let Some(reason) = &input.error {
        ui.colored_label(ui.visuals().warn_fg_color, reason);
    } else if let Some(err) = ignored {
        ui.colored_label(ui.visuals().warn_fg_color, format!("Pins ignored: {err}"));
    }
}

/// The 1-based line fields as a 0-based pin.
fn parse_pin(left: &str, right: &str) -> Result<Pin, String> {
    let line = |text: &str| match text.trim().parse::<usize>() {
        Ok(line) if line > 0 => Ok(line - 1),
        _ => Err(format!("\"{}\" is not a line number", text.trim())),
    };
    Ok(Pin {
        source: line(left)?,
        target: line(right)?,
    })
}

/// Lines hidden in a collapsed block share another line's row, hence the line check.
fn row_find<'a>(
    matcher: Option<&'a Matcher>,
    hit: Option<FindHit>,
    side: ActiveSide,
    row: usize,
    content: &LineContent,
) -> RowFind<'a> {
    let line_num = match content {
        LineContent::Code { line_num, .. } => usize::try_from(*line_num).ok(),
        _ => None,
    };
    let current = hit
        .filter(|hit| hit.side == side && hit.row == row && line_num == Some(hit.line + 1))
        .map(|hit| hit.ordinal);
    RowFind { matcher, current }
}

/// Width a side's text wraps at in a cell `cell_width` wide: what the gutter leaves.
pub(super) fn wrap_width(cell_width: f32) -> f32 {
    cell_width - GUTTER_WIDTH - GUTTER_GAP
}

/// Width of each side's widest row: gutter, text and ghost text. Measured over all rows, since
/// the table only lays out the visible ones.
pub(super) fn side_content_widths<T: RawTokenTrait>(
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
pub(super) fn wrapped_row_heights<T: RawTokenTrait>(
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
pub(super) fn measure_wrapped_rows<T: RawTokenTrait>(
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

/// A code row's text as drawn. `galley` holds only the real text, so only that can be selected
/// and copied; inline ghosts sit in gaps left in it. Ghost positions are relative to the galley.
struct SideText {
    galley: Arc<egui::Galley>,
    ghosts: Vec<(Vec2, Arc<egui::Galley>)>,
}

impl SideText {
    fn height(&self) -> f32 {
        self.ghosts
            .iter()
            .fold(self.galley.size().y, |height, (pos, ghost)| {
                height.max(pos.y + ghost.size().y)
            })
    }
}

/// Lays out a code row's text `job` and its ghosts, wrapped at `wrap_width` (infinite for one
/// line). Drawing and the wrapped row heights both go through here, so a row is exactly as tall
/// as what is drawn in it. Syntax colors don't move monospace glyphs, so `job` may be plain.
fn layout_side_text(
    fonts: &mut egui::epaint::FontsView<'_>,
    mut job: egui::text::LayoutJob,
    ghosts: &[GhostInsertion],
    ghost_color: egui::Color32,
    font_id: &egui::FontId,
    wrap_width: f32,
) -> SideText {
    let text_len = job.text.len();
    let (inline, trailing): (Vec<_>, Vec<_>) = ghosts
        .iter()
        .partition(|ghost| ghost.byte_offset < text_len);

    let inline_galleys: Vec<Arc<egui::Galley>> = inline
        .iter()
        .map(|ghost| fonts.layout_no_wrap(ghost.text.clone(), font_id.clone(), ghost_color))
        .collect();
    let gaps: Vec<(usize, f32)> = inline
        .iter()
        .zip(&inline_galleys)
        .map(|(ghost, galley)| (ghost.byte_offset, galley.size().x))
        .collect();
    insert_ghost_gaps(&mut job, &gaps);
    let text = job.text.clone();
    job.wrap.max_width = wrap_width;
    let galley = fonts.layout_job(job);

    let mut placed: Vec<_> = ghost_positions(&galley, &text, &gaps)
        .into_iter()
        .zip(inline_galleys)
        .collect();
    if !trailing.is_empty() {
        let last_row = galley.rows.last().expect("a galley has at least one row");
        // One job continuing the last row, wrapping like the text. The minimum height keeps an
        // empty first row (nothing fits after the text) from collapsing onto the text.
        let mut tail = egui::text::LayoutJob::default();
        for (i, ghost) in trailing.iter().enumerate() {
            let leading_space = if i == 0 { last_row.size.x } else { 0.0 };
            tail.append(
                &ghost.text,
                leading_space,
                egui::TextFormat::simple(font_id.clone(), ghost_color),
            );
        }
        tail.first_row_min_height = last_row.height();
        tail.wrap.max_width = wrap_width;
        placed.push((egui::vec2(0.0, last_row.pos.y), fonts.layout_job(tail)));
    }
    SideText {
        galley,
        ghosts: placed,
    }
}

/// Lays out the text rows: one line each, or each its wrapped height. A `scroll_to_row` on the
/// table goes by the same heights.
pub(super) fn text_rows(
    wrapped_heights: Option<&[f32]>,
    row_height: f32,
    num_rows: usize,
) -> Rows<'_> {
    match wrapped_heights {
        Some(heights) => Rows::Heights(heights),
        None => Rows::Uniform {
            height: row_height,
            count: num_rows,
        },
    }
}

/// The cell a side's row is drawn in before `SplitTable` shifts it sideways. The shifted ui still
/// clips to the cell, so its clip rect has the cell's x range.
fn unscrolled_cell(ui: &egui::Ui) -> egui::Rect {
    egui::Rect::from_x_y_ranges(ui.clip_rect().x_range(), ui.max_rect().y_range())
}

pub const REGEX_IGNORE_PRESETS: &[BuiltinPreset] = &[
    BuiltinPreset {
        name: "Time",
        text: r"\b\d{1,2}:\d{2}:\d{2}([.,]\d+)?\b",
    },
    BuiltinPreset {
        name: "Date",
        text: r"\b\d{4}-\d{2}-\d{2}\b",
    },
    BuiltinPreset {
        name: "ISO timestamp",
        text: r"\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}([.,]\d+)?(Z|[+-]\d{2}:?\d{2})?",
    },
    BuiltinPreset {
        name: "Bracketed time",
        text: r"\[[^\]]*\d\d:\d\d:\d\d[^\]]*\]",
    },
    BuiltinPreset {
        name: "UE log prefix",
        text: r"^\[\d{4}\.\d\d\.\d\d-\d\d\.\d\d\.\d\d:\d{3}\]\[\s*\d+\]",
    },
    BuiltinPreset {
        name: "GUID",
        text: r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b",
    },
    BuiltinPreset {
        name: "Hex address",
        text: r"\b0x[0-9a-fA-F]+\b",
    },
];

/// Toolbar button with a popup editing the ignore patterns. Patterns are recompiled only when the
/// text changes; invalid ones are listed under the field and skipped by the diff.
fn ignore_patterns_btn(
    ui: &mut egui::Ui,
    button_size: Vec2,
    patterns: &mut IgnorePatterns,
    enabled: bool,
    presets: &mut UserPresets,
    tooltip: &str,
) {
    let btn = egui::Button::new(egui::RichText::new("R").strong())
        .selected(enabled && !patterns.is_empty());
    let response = ui.add_sized(button_size, btn).on_hover_text(tooltip);
    let open_id = response.id.with("ignore_patterns_open");
    let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    if response.clicked() {
        open = !open;
    }
    // The preset save dialog is a modal drawn from inside this popup: clicks in it land outside
    // the popup, and a modal leaves Esc to open popups. Closing the popup would drop the dialog.
    let dialog_open = open && ui.ctx().memory(|m| m.top_modal_layer()).is_some();
    if dialog_open {
        ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    }
    let mut keep_open = false;
    egui::Popup::from_response(&response)
        .open_bool(&mut open)
        .close_behavior(if dialog_open {
            egui::PopupCloseBehavior::IgnoreClicks
        } else {
            egui::PopupCloseBehavior::CloseOnClickOutside
        })
        .show(|ui| {
            ui.label("One regex per line, matched per line. Ignores tokens a match fully covers.");
            let picked = presets_menu(
                ui,
                "ignore_regex_presets",
                REGEX_IGNORE_PRESETS,
                presets,
                patterns.text(),
            );
            if let Some(text) = picked {
                *patterns = IgnorePatterns::new(text);
            }
            // A click in the Presets submenu (a foreground layer) closes its parents too.
            let ctx = ui.ctx();
            let on_popup = ctx
                .pointer_interact_pos()
                .and_then(|pos| ctx.layer_id_at(pos))
                .is_some_and(|layer| layer.order == egui::Order::Foreground);
            keep_open = ui.will_parent_close()
                && on_popup
                && !ui.input(|i| i.key_pressed(egui::Key::Escape));
            let mut text = patterns.text().to_owned();
            let edit = TextEdit::multiline(&mut text)
                .code_editor()
                .desired_rows(4)
                .hint_text(r"\d\d:\d\d:\d\d");
            if ui.add(edit).changed() {
                *patterns = IgnorePatterns::new(text);
            }
            for error in patterns.errors() {
                let message = format!("Line {}: {}", error.line + 1, error.message);
                let message = egui::RichText::new(message).monospace();
                ui.colored_label(ui.visuals().error_fg_color, message);
            }
        });
    ui.data_mut(|d| d.insert_temp(open_id, open || keep_open));
}

/// Auto / Text / Hex for the current pair. A warning sign carries the fallback reason when the
/// pair is shown in another viewer than it asked for.
fn viewer_override_combo(
    ui: &mut egui::Ui,
    viewer_override: &mut Option<ViewerKind>,
    fallback: Option<&str>,
) {
    let name = |kind: Option<ViewerKind>| kind.map_or("Auto", ViewerKind::name);
    egui::ComboBox::from_id_salt("viewer_override")
        .selected_text(name(*viewer_override))
        .width(50.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(viewer_override, None, name(None));
            for kind in ViewerKind::ALL {
                ui.selectable_value(viewer_override, Some(kind), name(Some(kind)));
            }
        })
        .response
        .on_hover_text("Viewer for this pair; resets when another pair is opened");
    if let Some(reason) = fallback {
        ui.add(egui::Label::new("⚠").selectable(false))
            .on_hover_text(reason);
    }
}

/// Label and per-stage tooltip of a completed diff. Plain text so the segment can move into a
/// shared status line.
fn diff_status_text(
    num_add_deletes: MyersNumAddDelete,
    times: &DiffStageTimes,
) -> (String, String) {
    let (adds, deletes) = num_add_deletes;
    (
        format!("+{adds}/-{deletes}  {:.1?}", times.total()),
        format!(
            "Line diff: {:.1?}\nToken diff: {:.1?}\nIR: {:.1?}\nRows: {:.1?}",
            times.line_diff, times.token_diff, times.diff_ir, times.diff_rows
        ),
    )
}

// `label` is diff_status_text's, which leads with the counts.
fn diff_status_job(
    label: &str,
    (adds, deletes): MyersNumAddDelete,
    font_id: &egui::FontId,
    [insert, delete]: [egui::Color32; 2],
) -> egui::text::LayoutJob {
    let (adds, deletes) = (format!("+{adds}"), format!("-{deletes}"));
    let counts = format!("{adds}/{deletes}");
    assert!(
        label.starts_with(&counts),
        "{label:?} doesn't lead with {counts:?}"
    );
    let mut job = egui::text::LayoutJob::default();
    for (text, color) in [
        (adds.as_str(), insert),
        ("/", egui::Color32::PLACEHOLDER),
        (deletes.as_str(), delete),
        (&label[counts.len()..], egui::Color32::PLACEHOLDER),
    ] {
        job.append(text, 0.0, egui::TextFormat::simple(font_id.clone(), color));
    }
    job
}

/// Rows that are not file text get a one-character selectable label holding one of these
/// noncharacters. egui's cross-label copy adds a blank line for any vertical gap between copied
/// labels (and skips empty ones), so without them a Void or ghost-only row would copy as a blank
/// line and consecutive real blank lines would collapse into one. `CopyMarkerPlugin` removes them.
const COPY_MARKER_NO_LINE: char = '\u{FDD0}';
const COPY_MARKER_BLANK_LINE: char = '\u{FDD1}';

fn strip_copy_markers(copied: &str) -> String {
    let (no_line, blank) = (COPY_MARKER_NO_LINE.to_string(), COPY_MARKER_BLANK_LINE.to_string());
    copied
        .split('\n')
        .filter(|line| *line != no_line)
        .map(|line| if line == blank { "" } else { line })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) struct CopyMarkerPlugin;

impl egui::Plugin for CopyMarkerPlugin {
    fn debug_name(&self) -> &'static str {
        "CopyMarkerPlugin"
    }

    fn output_hook(&mut self, output: &mut egui::FullOutput) {
        output.platform_output.commands.retain_mut(|command| match command {
            egui::OutputCommand::CopyText(text)
                if text.contains([COPY_MARKER_NO_LINE, COPY_MARKER_BLANK_LINE]) =>
            {
                *text = strip_copy_markers(text);
                !text.is_empty()
            }
            _ => true,
        });
    }
}

/// Invisible selectable stand-in for a row that holds no file text; see `COPY_MARKER_NO_LINE`.
fn copy_marker_galley(ui: &egui::Ui, marker: char) -> Arc<egui::Galley> {
    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
    ui.fonts_mut(|fonts| {
        fonts.layout_no_wrap(marker.to_string(), font_id, egui::Color32::TRANSPARENT)
    })
}

/// `galley` with its rect stretched down to `height`, for selection. egui's cross-label copy adds
/// a blank line when the gap between two copied galleys is over half a row, so a side shorter
/// than its wrapped row has to reach the bottom of its cell. The text itself doesn't move.
fn reach_down_to(galley: Arc<egui::Galley>, height: f32) -> Arc<egui::Galley> {
    if galley.rect.height() >= height {
        return galley;
    }
    let mut galley = Arc::unwrap_or_clone(galley);
    galley.rect.max.y = galley.rect.min.y + height;
    Arc::new(galley)
}

/// Ghost text is visual-only: it is kept out of the label text so egui can neither select nor
/// copy it, and each ghost is a blank gap in the layout job instead. `ghosts` is (byte offset
/// into the real text, ghost width) in row order. Ghosts at the end of the text need no gap
/// because nothing follows them.
fn insert_ghost_gaps(job: &mut egui::text::LayoutJob, ghosts: &[(usize, f32)]) {
    for &(offset, width) in ghosts {
        if offset >= job.text.len() {
            continue;
        }
        let mut i = job
            .sections
            .iter()
            .position(|s| s.byte_range.contains(&offset))
            .expect("layout sections cover the whole text");
        let section = &mut job.sections[i];
        if section.byte_range.start < offset {
            let mut tail = section.clone();
            tail.leading_space = 0.0;
            tail.byte_range.start = offset;
            section.byte_range.end = offset;
            job.sections.insert(i + 1, tail);
            i += 1;
        }
        job.sections[i].leading_space += width;
    }
}

/// Sets the color of `ranges` (byte ranges into the job's text), splitting sections at their ends.
fn recolor_ranges(
    job: &mut egui::text::LayoutJob,
    ranges: &[std::ops::Range<usize>],
    color: egui::Color32,
) {
    let mut split_at = |offset: usize| {
        if let Some(i) = job
            .sections
            .iter()
            .position(|s| s.byte_range.start < offset && offset < s.byte_range.end)
        {
            let section = &mut job.sections[i];
            let mut tail = section.clone();
            tail.leading_space = 0.0;
            tail.byte_range.start = offset;
            section.byte_range.end = offset;
            job.sections.insert(i + 1, tail);
        }
    };
    for range in ranges {
        split_at(range.start);
        split_at(range.end);
    }
    for section in &mut job.sections {
        let r = &section.byte_range;
        if ranges.iter().any(|range| range.start <= r.start && r.end <= range.end) {
            section.format.color = color;
        }
    }
}

/// Sets the background of `ranges` (byte ranges into the job's text, ordered by start, possibly
/// overlapping), splitting sections at their ends. One pass, as a row can have many matches.
fn highlight_ranges(
    job: &mut egui::text::LayoutJob,
    ranges: &[std::ops::Range<usize>],
    background: egui::Color32,
) {
    if ranges.is_empty() {
        return;
    }
    let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range.clone()),
        }
    }
    let mut merged = merged.into_iter().peekable();
    let mut sections = Vec::with_capacity(job.sections.len() + 2 * merged.len());
    for section in job.sections.drain(..) {
        let std::ops::Range { mut start, end } = section.byte_range.clone();
        if start == end {
            sections.push(section);
            continue;
        }
        let mut leading_space = section.leading_space;
        while start < end {
            while merged.next_if(|r| r.end <= start).is_some() {}
            let (piece_end, highlighted) = match merged.peek() {
                Some(r) if r.start <= start => (end.min(r.end), true),
                Some(r) => (end.min(r.start), false),
                None => (end, false),
            };
            let mut piece = section.clone();
            piece.byte_range = start..piece_end;
            // The gap belongs before the section's first piece only.
            piece.leading_space = std::mem::take(&mut leading_space);
            if highlighted {
                piece.format.background = background;
            }
            sections.push(piece);
            start = piece_end;
        }
    }
    job.sections = sections;
}

/// Position of each inline ghost relative to the galley origin: inside its gap, which is just
/// before the glyph at its offset (`ghosts` as for `insert_ghost_gaps`, all inside the text).
/// Ghosts sharing an offset sit side by side in one gap. When the text wraps right at a gap, the
/// new row starts at the glyph and the gap is gone, so the ghost goes at the end of the row
/// before, where the gap would have been.
fn ghost_positions(galley: &egui::Galley, text: &str, ghosts: &[(usize, f32)]) -> Vec<Vec2> {
    ghosts
        .iter()
        .enumerate()
        .map(|(i, &(offset, _))| {
            let same_offset = |(o, _): &&(usize, f32)| *o == offset;
            let gap_width: f32 = ghosts.iter().filter(same_offset).map(|g| g.1).sum();
            let before_in_gap: f32 = ghosts[..i].iter().filter(same_offset).map(|g| g.1).sum();
            let mut char_index = text[..offset].chars().count();
            let (row_index, row) = galley
                .rows
                .iter()
                .enumerate()
                .find(|(_, row)| {
                    let found = char_index < row.glyphs.len();
                    if !found {
                        char_index -= row.glyphs.len();
                    }
                    found
                })
                .expect("an inline ghost's offset is inside the text");
            let gap_start = if char_index == 0 && row_index > 0 {
                let before = &galley.rows[row_index - 1];
                egui::vec2(before.size.x, before.pos.y)
            } else {
                egui::vec2(row.glyphs[char_index].pos.x - gap_width, row.pos.y)
            };
            gap_start + egui::vec2(before_in_gap, 0.0)
        })
        .collect()
}

fn handle_drops(
    ui: &egui::Ui,
    load_file_1_request: &mut Option<UniversalPath>,
    load_file_2_request: &mut Option<UniversalPath>,
    rect1: egui::Rect,
    rect2: egui::Rect,
) -> bool {
    // assert!(
    //     load_file_1_request.is_none() && load_file_2_request.is_none(),
    //     "File load requests should be None when handling drops"
    // );

    // let mut should_draw = false;
    let did_drop = ui.input(|i| {
        if let Some(drop_pos) = i.pointer.hover_pos() {
            // should_draw = true;
            for dropped_file in &i.raw.dropped_files {
                if let Some(path) = &dropped_file.path {
                    if rect1.contains(drop_pos) {
                        *load_file_1_request = Some(UniversalPath::from(path.clone()));
                        log::info!("File dropped on left pane: {}", path.display());
                        return true;
                    } else if rect2.contains(drop_pos) {
                        *load_file_2_request = Some(UniversalPath::from(path.clone()));
                        log::info!("File dropped on right pane: {}", path.display());
                        return true;
                    }
                    break;
                }
            }
        }
        return false;
    });
    // if should_draw {
    //     ui.painter()
    //         .debug_rect(rect1, egui::Color32::RED, "Left Drop Zone");
    //     ui.painter()
    //         .debug_rect(rect2, egui::Color32::BLUE, "Right Drop Zone");
    // }

    return did_drop;
}

fn file_path_segments(path: &UniversalPath, label: Option<&str>) -> Vec<PathSegment> {
    match label {
        Some(label) => vec![PathSegment {
            name: label.to_owned(),
            path: path.to_string(),
        }],
        None => split_path(&path.to_string()),
    }
}

fn file_path_dim([a, b]: [&UniversalPath; 2], labelled: bool) -> usize {
    if labelled {
        return 0;
    }
    common_parent(&a.to_string(), true, &b.to_string(), true)
}

fn file_path_request(
    text: &str,
    current: &UniversalPath,
    root: Option<&UniversalPath>,
) -> Option<UniversalPath> {
    let text = text.trim().trim_matches('"');
    if text == current.to_string() {
        return None;
    }
    let path = UniversalPath::from(text);
    // set_path appends a depot path to a depot root, so send it root-relative like the old field.
    let relative = root
        .filter(|root| root.is_depot() && path.is_depot())
        .and_then(|root| FileProcessor::strip_root_prefix(root, &path));
    Some(relative.map_or(path, |relative| UniversalPath::from(relative.to_string())))
}

fn browser_pick(
    side: ActiveSide,
    picked: &str,
    paths: [&UniversalPath; 2],
    roots: [Option<&UniversalPath>; 2],
) -> [Option<UniversalPath>; 2] {
    let i = h_scroll::side_index(side);
    let mut requests = [None, None];
    requests[i] = file_path_request(picked, paths[i], roots[i]);
    requests
}

#[cfg(test)]
mod tests {
    mod path_bar {
        use zcommon::path::PathSegment;
        use zdiff::universal_path::UniversalPath;

        use super::super::{browser_pick, file_path_dim, file_path_request, file_path_segments};
        use crate::file::FileProcessor;
        use crate::ui_egui::active_side::ActiveSide;

        fn committed(root: Option<&str>, current: &str, text: &str) -> UniversalPath {
            let mut file = FileProcessor::new();
            let root = root.map(UniversalPath::from);
            if let Some(root) = &root {
                file.set_root(root.clone());
            }
            file.set_path(UniversalPath::from(current));
            let current = file.get_full_path();
            let request = file_path_request(text, &current, root.as_ref()).unwrap();
            file.set_path(request);
            file.get_full_path()
        }

        #[test]
        fn segments_cover_the_full_path() {
            let names: Vec<String> =
                file_path_segments(&UniversalPath::from(r"C:\ws\src\main.rs"), None)
                    .into_iter()
                    .map(|s| s.name)
                    .collect();
            assert_eq!(names, [r"C:\", "ws", "src", "main.rs"]);
        }

        #[test]
        fn a_label_is_one_segment_standing_for_the_full_path() {
            let path = UniversalPath::from(r"C:\Temp\p4v\main.rs#3");
            assert_eq!(
                file_path_segments(&path, Some("main.rs#3 (depot)")),
                [PathSegment {
                    name: "main.rs#3 (depot)".to_owned(),
                    path: path.to_string(),
                }]
            );
        }

        #[test]
        fn dim_follows_either_sides_path() {
            let a = UniversalPath::from(r"C:\ws\src\main.rs");
            let b = UniversalPath::from(r"C:\ws\src\lib.rs");
            let c = UniversalPath::from(r"C:\ws\docs\readme.md");
            let d = UniversalPath::from(r"D:\ws\src\main.rs");
            assert_eq!(file_path_dim([&a, &b], false), 3);
            assert_eq!(file_path_dim([&a, &c], false), 2);
            assert_eq!(file_path_dim([&c, &b], false), 2);
            assert_eq!(file_path_dim([&d, &b], false), 0);
            assert_eq!(
                file_path_dim(
                    [
                        &UniversalPath::from("//depot/main/a.rs#3"),
                        &UniversalPath::from("//depot/main/b.rs")
                    ],
                    false
                ),
                2
            );
        }

        #[test]
        fn nothing_is_dimmed_with_a_label() {
            let a = UniversalPath::from(r"C:\ws\src\main.rs");
            assert_eq!(file_path_dim([&a, &a], true), 0);
        }

        #[test]
        fn unchanged_text_is_not_a_request() {
            let path = UniversalPath::from(r"C:\ws\src\main.rs");
            assert_eq!(file_path_request(r"C:\ws\src\main.rs", &path, None), None);
            let depot = UniversalPath::from("//depot/main/a.rs#3");
            assert_eq!(
                file_path_request(" //depot/main/a.rs#3 ", &depot, None),
                None
            );
        }

        #[test]
        fn committed_full_path_without_a_root_loads_it() {
            assert_eq!(
                committed(None, r"C:\ws\a.rs", r#""C:\other dir\b.rs" "#),
                UniversalPath::from(r"C:\other dir\b.rs")
            );
            assert_eq!(
                committed(None, r"C:\ws\a.rs", "//depot/main/b.rs#2"),
                UniversalPath::from("//depot/main/b.rs#2")
            );
        }

        #[test]
        fn committed_full_path_under_a_local_root_loads_it() {
            assert_eq!(
                committed(Some(r"C:\ws"), r"C:\ws\src\a.rs", r"C:\ws\src\b.rs"),
                UniversalPath::from(r"C:\ws\src\b.rs")
            );
            assert_eq!(
                committed(Some(r"C:\ws"), r"C:\ws\src\a.rs", r"C:\other\b.rs"),
                UniversalPath::from(r"C:\other\b.rs")
            );
        }

        #[test]
        fn committed_full_path_under_a_depot_root_loads_it() {
            assert_eq!(
                committed(
                    Some("//depot/main"),
                    "//depot/main/src/a.rs",
                    "//depot/main/src/b.rs"
                ),
                UniversalPath::from("//depot/main/src/b.rs")
            );
            assert_eq!(
                committed(
                    Some("//depot/main"),
                    "//depot/main/src/a.rs",
                    "//depot/main/src/b.rs#4"
                )
                .to_string(),
                "//depot/main/src/b.rs#4"
            );
            assert_eq!(
                committed(
                    Some("//depot/main"),
                    "//depot/main/src/a.rs",
                    "//depot/dev/src/a.rs"
                ),
                UniversalPath::from("//depot/dev/src/a.rs")
            );
        }

        #[test]
        fn a_picked_file_loads_into_that_side_only() {
            let left = UniversalPath::from(r"C:\ws\src\a.rs");
            let right = UniversalPath::from(r"C:\ws\src\b.rs");
            assert_eq!(
                browser_pick(
                    ActiveSide::Left,
                    r"C:\ws\c.rs",
                    [&left, &right],
                    [None, None]
                ),
                [Some(UniversalPath::from(r"C:\ws\c.rs")), None]
            );
            assert_eq!(
                browser_pick(
                    ActiveSide::Right,
                    r"C:\ws\c.rs",
                    [&left, &right],
                    [None, None]
                ),
                [None, Some(UniversalPath::from(r"C:\ws\c.rs"))]
            );
        }

        #[test]
        fn picking_the_loaded_file_is_no_request() {
            let left = UniversalPath::from(r"C:\ws\src\a.rs");
            let right = UniversalPath::from(r"C:\ws\src\b.rs");
            assert_eq!(
                browser_pick(
                    ActiveSide::Right,
                    r"C:\ws\src\b.rs",
                    [&left, &right],
                    [None, None]
                ),
                [None, None]
            );
        }

        #[test]
        fn a_picked_file_is_the_same_request_as_typing_it() {
            let root = UniversalPath::from("//depot/main");
            let left = UniversalPath::from("//depot/main/src/a.rs");
            let right = UniversalPath::from(r"C:\ws\b.rs");
            let picked = "//depot/main/src/c.rs";
            assert_eq!(
                browser_pick(
                    ActiveSide::Left,
                    picked,
                    [&left, &right],
                    [Some(&root), None]
                ),
                [file_path_request(picked, &left, Some(&root)), None]
            );
        }

        #[test]
        fn a_picked_depot_file_opens_at_head() {
            let left = UniversalPath::from("//depot/main/a.rs#3");
            let right = UniversalPath::from(r"C:\ws\b.rs");
            for picked in ["//depot/main/a.rs", "//depot/main/c.rs"] {
                assert_eq!(
                    browser_pick(ActiveSide::Left, picked, [&left, &right], [None, None]),
                    [Some(UniversalPath::Depot(picked.into(), None)), None]
                );
            }
        }
    }

    use std::time::Duration;

    use eframe::egui;
    use zcommon::ui_egui::table::{Split, SplitColumn, SplitTable};
    use zdiff::{
        diff_builder::{DiffBuilderOptions, DiffRow, LineContent, RowBg},
        diff_ir::{DiffOp, DiffResult},
        ignore::{IgnoreOptions, IgnorePatterns},
    };

    use crate::{
        diff_ctx::DiffStageTimes,
        ui_egui::copy_harness::{CopyHarness, Side, measure_in_fresh_context},
    };

    use super::{
        CENTER_GAP, COPY_MARKER_BLANK_LINE, COPY_MARKER_NO_LINE, CenterLayout, GUTTER_GAP,
        GUTTER_WIDTH, GhostInsertion, diff_status_job, diff_status_text, highlight_ranges,
        insert_ghost_gaps, layout_side_text, recolor_ranges, row_find, strip_copy_markers,
    };

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

    #[test]
    fn the_center_layout_is_measured_once_per_font_and_zoom() {
        let mut cache = None;
        let mut measured = 0;
        for (size, pixels_per_point) in [
            (12.0, 1.0),
            (12.0, 1.0),
            (14.0, 1.0),
            (14.0, 1.0),
            (14.0, 1.5),
            (14.0, 1.5),
        ] {
            super::center_layout(
                &mut cache,
                &egui::FontId::monospace(size),
                pixels_per_point,
                || {
                    measured += 1;
                    CenterLayout {
                        button: size,
                        symbol: size,
                    }
                },
            );
        }
        assert_eq!(measured, 3);
    }

    #[test]
    fn the_center_column_fits_its_widest_buttons_and_symbol() {
        let layout = CenterLayout::measure(|c| if c == '≠' { 9.0 } else { 7.0 }, 4.0);
        assert_eq!(
            layout,
            CenterLayout {
                button: 15.0,
                symbol: 9.0
            }
        );
        assert_eq!(layout.width(), 4.0 * 15.0 + 9.0 + 4.0 * CENTER_GAP);
    }

    #[test]
    fn center_slots_are_toggles_revert_left_symbol_revert_right_edge_to_edge() {
        let layout = CenterLayout {
            button: 15.0,
            symbol: 9.0,
        };
        let cell =
            egui::Rect::from_min_size(egui::pos2(100.0, 40.0), egui::vec2(layout.width(), 14.0));
        let slots = layout.slots(cell, 18.0);

        assert_eq!(slots.map(|s| s.width()), [15.0, 15.0, 15.0, 9.0, 15.0]);
        assert_eq!(slots[0].left(), cell.left());
        assert_eq!(slots[4].right(), cell.right());
        for pair in slots.windows(2) {
            assert_eq!(pair[1].left() - pair[0].right(), CENTER_GAP);
        }
        for slot in slots {
            assert_eq!(slot.y_range(), cell.y_range());
        }
    }

    #[test]
    fn center_slots_in_a_tall_row_are_one_button_high_and_centered() {
        let layout = CenterLayout {
            button: 15.0,
            symbol: 9.0,
        };
        let cell =
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(layout.width(), 50.0));
        for slot in layout.slots(cell, 18.0) {
            assert_eq!(slot.height(), 18.0);
            assert_eq!(slot.center().y, cell.center().y);
        }
    }

    fn code(ops: &[(DiffOp, bool)]) -> LineContent {
        LineContent::Code {
            tokens: ops
                .iter()
                .map(|(operation, hide_in_diff)| {
                    (
                        DiffResult {
                            operation: operation.clone(),
                            token_source_idx: None,
                            token_target_idx: None,
                            hide_in_diff: *hide_in_diff,
                        },
                        false,
                        false,
                    )
                })
                .collect(),
            line_num: 1,
            bg: RowBg::None,
            changed_chars: Vec::new(),
        }
    }

    #[test]
    fn only_changed_rows_get_a_colored_symbol() {
        use eframe::egui::Color32;
        let (delete, insert, modified) = (Color32::RED, Color32::GREEN, Color32::YELLOW);
        let symbol =
            |left, right| super::row_symbol(&DiffRow { left, right }, [delete, insert, modified]);
        const EQUAL: (DiffOp, bool) = (DiffOp::Equal(true), false);
        const DELETE: (DiffOp, bool) = (DiffOp::Delete, false);
        const INSERT: (DiffOp, bool) = (DiffOp::Insert, false);

        assert_eq!(
            symbol(code(&[EQUAL, DELETE]), LineContent::Void),
            Some(("-", delete))
        );
        assert_eq!(
            symbol(LineContent::Void, code(&[INSERT])),
            Some(("+", insert))
        );
        assert_eq!(
            symbol(code(&[EQUAL, DELETE]), code(&[EQUAL, INSERT])),
            Some(("≠", modified))
        );
        assert_eq!(symbol(code(&[EQUAL]), code(&[EQUAL])), None);
        assert_eq!(
            symbol(code(&[(DiffOp::Delete, true)]), LineContent::Void),
            None
        );
        assert_eq!(symbol(LineContent::Collapsed, LineContent::Collapsed), None);
        assert_eq!(symbol(LineContent::Void, LineContent::Void), None);
    }

    #[test]
    fn pin_fields_are_one_based_line_numbers() {
        use zdiff::myers::Pin;
        assert_eq!(
            super::parse_pin("1", " 12 "),
            Ok(Pin {
                source: 0,
                target: 11
            })
        );
        assert!(super::parse_pin("0", "3").is_err());
        assert!(super::parse_pin("2", "").is_err());
        assert!(super::parse_pin("x", "3").is_err());
    }

    #[test]
    fn pinned_lines_are_marked_on_their_own_side() {
        use crate::ui_egui::active_side::ActiveSide::{Left, Right};
        use zdiff::myers::Pin;
        let code = |line_num| LineContent::Code {
            tokens: Vec::new(),
            line_num,
            bg: RowBg::None,
            changed_chars: Vec::new(),
        };
        let pins = [Pin {
            source: 2,
            target: 6,
        }];
        assert!(super::is_pinned(&pins, Left, &code(3)));
        assert!(super::is_pinned(&pins, Right, &code(7)));
        assert!(!super::is_pinned(&pins, Right, &code(3)));
        assert!(!super::is_pinned(&pins, Left, &code(7)));
        assert!(!super::is_pinned(&pins, Left, &code(-1)));
        assert!(!super::is_pinned(&pins, Left, &LineContent::Void));
    }

    #[test]
    fn the_current_find_hit_paints_only_on_its_own_side_row_and_line() {
        use crate::{diff_ctx::FindHit, ui_egui::active_side::ActiveSide};
        let code = |line_num| LineContent::Code {
            tokens: Vec::new(),
            line_num,
            bg: RowBg::None,
            changed_chars: Vec::new(),
        };
        let hit = FindHit {
            side: ActiveSide::Right,
            line: 9,
            ordinal: 2,
            row: 4,
        };
        let current = |side, row, content: &LineContent| {
            row_find(None, Some(hit), side, row, content).current
        };

        assert_eq!(current(ActiveSide::Right, 4, &code(10)), Some(2));
        assert_eq!(current(ActiveSide::Left, 4, &code(10)), None);
        assert_eq!(current(ActiveSide::Right, 5, &code(10)), None);
        assert_eq!(current(ActiveSide::Right, 4, &code(3)), None);
        assert_eq!(current(ActiveSide::Right, 4, &LineContent::Void), None);
        assert_eq!(
            row_find(None, None, ActiveSide::Right, 4, &code(10)).current,
            None
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn ignore_comments_is_persisted_with_the_diff_options() {
        let options = DiffBuilderOptions {
            ignore: IgnoreOptions {
                comments: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let json = serde_json::to_value(&options).unwrap();
        assert_eq!(json["ignore_comments"], true);
        let loaded: DiffBuilderOptions = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(loaded, options);

        // State saved before the option existed still loads, with the option off.
        let mut old = json;
        old.as_object_mut().unwrap().remove("ignore_comments");
        let loaded: DiffBuilderOptions = serde_json::from_value(old).unwrap();
        assert_eq!(loaded, DiffBuilderOptions::default());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn ignore_patterns_are_persisted_with_the_diff_options() {
        // The invalid line is kept, so a typo isn't lost on restart.
        let text = "\\d\\d:\\d\\d\n(";
        let options = DiffBuilderOptions {
            ignore: IgnoreOptions {
                patterns: IgnorePatterns::new(text),
                ..Default::default()
            },
            ..Default::default()
        };
        let json = serde_json::to_value(&options).unwrap();
        assert_eq!(json["ignore_regex"], text);
        let loaded: DiffBuilderOptions = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(loaded, options);
        // Loading compiles the patterns again.
        assert!(!loaded.ignore.patterns.is_empty());
        assert_eq!(loaded.ignore.patterns.errors().len(), 1);

        // State saved before the option existed still loads, with no patterns.
        let mut old = json;
        old.as_object_mut().unwrap().remove("ignore_regex");
        let loaded: DiffBuilderOptions = serde_json::from_value(old).unwrap();
        assert_eq!(loaded, DiffBuilderOptions::default());
    }

    fn ignored_runs<'a>(patterns: &IgnorePatterns, text: &'a str) -> Vec<&'a str> {
        use zdiff::lexer::{LexerDefault, RawToken};
        let tokens = LexerDefault::<RawToken>::new(text).parse();
        let flags = patterns.flags(&tokens, text);
        let mut runs = Vec::new();
        let mut run: Option<std::ops::Range<usize>> = None;
        for (token, flag) in tokens.iter().zip(flags) {
            match (&mut run, flag) {
                (Some(r), true) => r.end = token.span.end,
                (None, true) => run = Some(token.span.clone()),
                (Some(_), false) => runs.push(&text[run.take().unwrap()]),
                (None, false) => {}
            }
        }
        runs.extend(run.map(|r| &text[r]));
        runs
    }

    #[test]
    fn regex_ignore_presets_compile_and_match_only_their_sample() {
        let samples: &[(&str, &str, &[&str])] = &[
            (
                "Time",
                "LogTemp: 12:34:56.789 started, took 3:04:05\n",
                &["12:34:56.789", "3:04:05"],
            ),
            ("Date", "Build 2024-03-15 v1.2-3\n", &["2024-03-15"]),
            (
                "ISO timestamp",
                "at 2024-03-15T10:20:30.123Z done\nat 2024-03-15 10:20:30+01:00 done\n",
                &["2024-03-15T10:20:30.123Z", "2024-03-15 10:20:30+01:00"],
            ),
            (
                "Bracketed time",
                "[2024-03-15 10:20:30.5] [Info] Loaded\n",
                &["[2024-03-15 10:20:30.5]"],
            ),
            (
                "UE log prefix",
                "[2024.03.15-10.20.30:123][  0]LogTemp: Display: [1]\n",
                &["[2024.03.15-10.20.30:123][  0]"],
            ),
            (
                "GUID",
                "Guid=3F2504E0-4F89-11D3-9A0C-0305E82C3301 id-1234\n",
                &["3F2504E0-4F89-11D3-9A0C-0305E82C3301"],
            ),
            (
                "Hex address",
                "at 0x7ff6a1b2c3d4 in main+42\n",
                &["0x7ff6a1b2c3d4"],
            ),
        ];
        assert_eq!(super::REGEX_IGNORE_PRESETS.len(), samples.len());
        for preset in super::REGEX_IGNORE_PRESETS {
            let (_, text, expected) = samples
                .iter()
                .find(|(name, ..)| *name == preset.name)
                .unwrap_or_else(|| panic!("no sample for preset \"{}\"", preset.name));
            let patterns = IgnorePatterns::new(preset.text);
            assert_eq!(patterns.errors(), [], "{}", preset.name);
            assert!(!patterns.is_empty(), "{}", preset.name);
            assert_eq!(ignored_runs(&patterns, text), *expected, "{}", preset.name);
        }
    }

    #[test]
    fn copy_markers_drop_non_file_rows_and_keep_blank_lines() {
        let (no_line, blank) = (COPY_MARKER_NO_LINE.to_string(), COPY_MARKER_BLANK_LINE.to_string());
        let copied = [no_line.as_str(), "a", &no_line, &blank, &blank, "b", &no_line].join("\n");

        assert_eq!(strip_copy_markers(&copied), "a\n\n\nb");
    }

    fn job_with_one_section(text: &str) -> eframe::egui::text::LayoutJob {
        eframe::egui::text::LayoutJob::simple_singleline(
            text.to_owned(),
            eframe::egui::FontId::monospace(12.0),
            eframe::egui::Color32::WHITE,
        )
    }

    #[test]
    fn ghost_gap_splits_the_section_without_changing_the_text() {
        let mut job = job_with_one_section("abcd");

        insert_ghost_gaps(&mut job, &[(2, 7.0)]);

        assert_eq!(job.text, "abcd");
        let sections: Vec<_> = job
            .sections
            .iter()
            .map(|s| (s.byte_range.clone(), s.leading_space))
            .collect();
        assert_eq!(sections, [(0..2, 0.0), (2..4, 7.0)]);
    }

    #[test]
    fn recolor_splits_sections_at_the_range_ends_without_changing_the_text() {
        use eframe::egui::Color32;
        let dim = Color32::from_rgba_unmultiplied(1, 2, 3, 4);
        let colors = |job: &eframe::egui::text::LayoutJob| -> Vec<_> {
            job.sections
                .iter()
                .map(|s| (s.byte_range.clone(), s.format.color))
                .collect()
        };

        let mut job = job_with_one_section("abcdef");
        recolor_ranges(&mut job, &[1..3, 5..6], dim);
        assert_eq!(job.text, "abcdef");
        assert_eq!(
            colors(&job),
            [
                (0..1, Color32::WHITE),
                (1..3, dim),
                (3..5, Color32::WHITE),
                (5..6, dim)
            ]
        );

        // A range across a section boundary recolors both parts.
        let mut job = job_with_one_section("abc");
        job.append("def", 0.0, job.sections[0].format.clone());
        recolor_ranges(&mut job, &[2..4], dim);
        assert_eq!(
            colors(&job),
            [
                (0..2, Color32::WHITE),
                (2..3, dim),
                (3..4, dim),
                (4..6, Color32::WHITE)
            ]
        );
    }

    #[test]
    fn highlight_splits_sections_once_and_merges_overlapping_ranges() {
        use eframe::egui::Color32;
        let bg = Color32::from_rgba_unmultiplied(1, 2, 3, 4);
        let backgrounds = |job: &eframe::egui::text::LayoutJob| -> Vec<_> {
            job.sections
                .iter()
                .map(|s| (s.byte_range.clone(), s.format.background, s.leading_space))
                .collect()
        };
        let none = Color32::TRANSPARENT;

        // Overlapping matches ("aa" in "aaaa") light up as one run; a later match stays apart.
        let mut job = job_with_one_section("aaaa-aa");
        job.sections[0].leading_space = 7.0;
        highlight_ranges(&mut job, &[0..2, 1..3, 2..4, 5..7], bg);
        assert_eq!(job.text, "aaaa-aa");
        assert_eq!(
            backgrounds(&job),
            [(0..4, bg, 7.0), (4..5, none, 0.0), (5..7, bg, 0.0)]
        );

        // A range across a section boundary highlights both parts; the second section keeps its
        // own leading space and color.
        let mut job = job_with_one_section("abc");
        let mut format = job.sections[0].format.clone();
        format.color = Color32::RED;
        job.append("def", 2.0, format);
        highlight_ranges(&mut job, &[2..4], bg);
        assert_eq!(
            backgrounds(&job),
            [
                (0..2, none, 0.0),
                (2..3, bg, 0.0),
                (3..4, bg, 2.0),
                (4..6, none, 0.0)
            ]
        );
        assert_eq!(job.sections[2].format.color, Color32::RED);
        assert_eq!(job.sections[3].format.color, Color32::RED);

        // No ranges leaves the job as it was.
        let mut job = job_with_one_section("abc");
        highlight_ranges(&mut job, &[], bg);
        assert_eq!(backgrounds(&job), [(0..3, none, 0.0)]);
    }

    #[test]
    fn ghost_gaps_at_the_same_offset_add_up_and_trailing_ghosts_add_no_gap() {
        let mut job = job_with_one_section("abcd");

        insert_ghost_gaps(&mut job, &[(0, 1.0), (0, 2.0), (4, 5.0)]);

        let sections: Vec<_> = job
            .sections
            .iter()
            .map(|s| (s.byte_range.clone(), s.leading_space))
            .collect();
        assert_eq!(sections, [(0..4, 3.0)]);
    }

    #[test]
    fn diff_status_shows_counts_and_total_time_with_each_stage_in_the_tooltip() {
        let times = DiffStageTimes {
            line_diff: Duration::from_micros(12_340),
            token_diff: Duration::from_micros(1_000),
            diff_ir: Duration::from_micros(500),
            diff_rows: Duration::from_millis(2),
        };

        let (label, tooltip) = diff_status_text((7, 3), &times);

        assert_eq!(label, "+7/-3  15.8ms");
        assert_eq!(
            tooltip,
            "Line diff: 12.3ms\nToken diff: 1.0ms\nIR: 500.0µs\nRows: 2.0ms"
        );
    }

    #[test]
    fn diff_status_counts_use_the_insert_and_delete_colors() {
        use eframe::egui::{Color32, FontId};
        let (insert, delete, rest) = (Color32::GREEN, Color32::RED, Color32::PLACEHOLDER);

        let job = diff_status_job(
            "+17/-3  15.8ms",
            (17, 3),
            &FontId::default(),
            [insert, delete],
        );

        assert_eq!(job.text, "+17/-3  15.8ms");
        let sections: Vec<_> = job
            .sections
            .iter()
            .map(|s| (s.byte_range.clone(), s.format.color))
            .collect();
        assert_eq!(
            sections,
            [(0..3, insert), (3..4, rest), (4..6, delete), (6..14, rest)]
        );
    }

    const SOURCE: &str = "fn main() {\n    let x = 1;\n    let y = 2;\n}\n";
    const TARGET: &str = "fn main() {\n    let x = 1;\n    let z = 3 + 4;\n}\n";

    #[test]
    fn single_row_selection_copies_that_rows_text() {
        let mut harness = CopyHarness::new(SOURCE, TARGET, &DiffBuilderOptions::default());

        let copied = harness.drag_and_copy(Side::Left, (1, 4), (1, 9));
        assert_eq!(copied.as_deref(), Some("let x"));

        let copied = harness.drag_and_copy(Side::Right, (2, 4), (2, 9));
        assert_eq!(copied.as_deref(), Some("let z"));
    }


    // Same on both sides except the last line, so rows 0..=2 carry no ghosts and are identical
    // on both sides.
    const SHARED_SOURCE: &str = "fn main() {\n    foo(bar, baz);\n    let x = 1;\n}\nold\n";
    const SHARED_TARGET: &str = "fn main() {\n    foo(bar, baz);\n    let x = 1;\n}\nnew\n";

    fn shared_harness() -> CopyHarness {
        CopyHarness::new(SHARED_SOURCE, SHARED_TARGET, &DiffBuilderOptions::default())
    }

    #[test]
    fn multi_row_selection_copies_exact_file_text() {
        let mut harness = shared_harness();

        let expected = Some("fn main() {\n    foo(bar, baz);\n    let x = 1;\n}");
        let copied = harness.drag_and_copy(Side::Left, (0, 0), (3, 1));
        assert_eq!(copied.as_deref(), expected);
        let copied = harness.drag_and_copy(Side::Right, (0, 0), (3, 1));
        assert_eq!(copied.as_deref(), expected);
    }

    #[test]
    fn multi_row_selection_keeps_punctuation_spacing() {
        let mut harness = shared_harness();

        let copied = harness.drag_and_copy(Side::Left, (1, 4), (1, 18));
        assert_eq!(copied.as_deref(), Some("foo(bar, baz);"));
    }

    #[test]
    fn partial_first_and_last_row() {
        let mut harness = shared_harness();

        let copied = harness.drag_and_copy(Side::Left, (0, 3), (2, 9));
        assert_eq!(
            copied.as_deref(),
            Some("main() {\n    foo(bar, baz);\n    let x")
        );
    }

    #[test]
    fn upward_drag_copies_same_text_as_downward() {
        let mut harness = shared_harness();

        let down = harness.drag_and_copy(Side::Left, (0, 3), (2, 9));
        let up = harness.drag_and_copy(Side::Left, (2, 9), (0, 3));
        assert_eq!(up, down);
    }

    #[test]
    fn trailing_whitespace_is_kept() {
        let mut harness = CopyHarness::new("a  \nb\n", "a  \nb\n", &DiffBuilderOptions::default());

        let copied = harness.drag_and_copy(Side::Left, (0, 0), (1, 1));
        assert_eq!(copied.as_deref(), Some("a  \nb"));
    }

    #[test]
    fn crlf_rows_are_joined_with_lf() {
        let mut harness = CopyHarness::new("a\r\nb\r\n", "a\r\nb\r\n", &DiffBuilderOptions::default());

        let copied = harness.drag_and_copy(Side::Left, (0, 0), (1, 1));
        assert_eq!(copied.as_deref(), Some("a\nb"));
    }

    fn collapsed_row_between(row: &DiffRow) -> DiffRow {
        DiffRow {
            left: LineContent::Collapsed,
            right: LineContent::Collapsed,
            ..row.clone()
        }
    }

    /// Phantom blank lines from rows without a label are a known gap, handled with the ghost
    /// fidelity work; this only pins that no placeholder text reaches the clipboard.
    fn assert_only_real_text(copied: Option<String>) {
        let copied = copied.expect("copy command emitted");
        let without_newlines: String = copied.chars().filter(|c| *c != '\n').collect();
        assert_eq!(without_newlines, "fn main() {    let x = 1;");
    }

    #[test]
    fn collapsed_row_contributes_nothing() {
        let mut harness = shared_harness();
        let collapsed = collapsed_row_between(&harness.rows()[1]);
        harness.set_row(1, collapsed);

        assert_only_real_text(harness.drag_and_copy(Side::Left, (0, 0), (2, 14)));
    }

    #[test]
    fn copy_skips_a_collapsed_block_and_includes_it_once_expanded() {
        use std::{
            collections::BTreeMap,
            sync::{Arc, atomic::AtomicBool},
        };

        use crate::diff_ctx::{expand_rows, finalize_diff_rows};

        // Lines 1 and 12 differ, so one context row each leaves lines 3..=10 collapsed.
        let source: String = (1..=12).map(|n| format!("line {n}\n")).collect();
        let target = source
            .replace("line 1\n", "edit 1\n")
            .replace("line 12\n", "edit 12\n");
        let options = DiffBuilderOptions {
            diff_only_with_extra_rows: Some(1),
            ..Default::default()
        };
        let mut harness = CopyHarness::new(&source, &target, &options);
        let (collapsed, _, blocks) = finalize_diff_rows(
            harness.rows().to_vec(),
            &options,
            &Arc::new(AtomicBool::new(false)),
        )
        .expect("not cancelled");
        let [ref block] = blocks[..] else {
            panic!("expected one collapsed block: {collapsed:#?}");
        };

        harness.set_rows(collapsed.clone());
        let copied = harness
            .drag_and_copy(Side::Left, (1, 0), (3, 7))
            .expect("copy command emitted");
        let without_newlines: String = copied.chars().filter(|c| *c != '\n').collect();
        assert_eq!(without_newlines, "line 2line 11");

        let (expanded, _) = expand_rows(&collapsed, &blocks, &BTreeMap::from([(block.row, 0)]));
        harness.set_rows(expanded);
        let expected: Vec<String> = (2..=11).map(|n| format!("line {n}")).collect();
        let copied = harness.drag_and_copy(Side::Left, (1, 0), (10, 7));
        assert_eq!(copied, Some(expected.join("\n")));
    }

    #[test]
    fn void_row_contributes_nothing() {
        let mut harness = shared_harness();
        let void = DiffRow {
            left: LineContent::Void,
            right: LineContent::Void,
            ..harness.rows()[1].clone()
        };
        harness.set_row(1, void);

        assert_only_real_text(harness.drag_and_copy(Side::Left, (0, 0), (2, 14)));
    }


    fn options_variants() -> [(&'static str, DiffBuilderOptions); 2] {
        [
            ("default", DiffBuilderOptions::default()),
            (
                "ignore_whitespace",
                DiffBuilderOptions {
                    ignore: IgnoreOptions {
                        whitespace: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ),
        ]
    }

    #[test]
    fn inline_ghost_tokens_add_no_characters() {
        for (name, options) in options_variants() {
            let mut harness = CopyHarness::new("let x = old(1);\nkeep\n", "let x = new(1);\nkeep\n", &options);

            let copied = harness.drag_and_copy(Side::Left, (0, 0), (1, 4));
            assert_eq!(copied.as_deref(), Some("let x = old(1);\nkeep"), "left, {name}");
            let copied = harness.drag_and_copy(Side::Right, (0, 0), (1, 4));
            assert_eq!(copied.as_deref(), Some("let x = new(1);\nkeep"), "right, {name}");
        }
    }

    #[test]
    fn inline_ghost_row_copies_partial_real_text_on_either_side_of_the_ghost() {
        let mut harness = CopyHarness::new("let x = old(1);\nkeep\n", "let x = new(1);\nkeep\n", &DiffBuilderOptions::default());

        let copied = harness.drag_and_copy(Side::Left, (0, 4), (0, 12));
        assert_eq!(copied.as_deref(), Some("x = old("));
        let copied = harness.drag_and_copy(Side::Left, (0, 9), (1, 2));
        assert_eq!(copied.as_deref(), Some("ld(1);\nke"));
    }

    #[test]
    fn selection_spanning_a_ghost_only_row_joins_the_surrounding_real_lines() {
        for (name, options) in options_variants() {
            let mut harness = CopyHarness::new("a\nb\n", "a\nx\nb\n", &options);

            let copied = harness.drag_and_copy(Side::Left, (0, 0), (2, 1));
            assert_eq!(copied.as_deref(), Some("a\nb"), "left, {name}");
            let copied = harness.drag_and_copy(Side::Right, (0, 0), (2, 1));
            assert_eq!(copied.as_deref(), Some("a\nx\nb"), "right, {name}");
        }
    }

    #[test]
    fn real_blank_line_inside_a_selection_is_preserved() {
        for (name, options) in options_variants() {
            let mut harness = CopyHarness::new("a\n\nb\n", "a\n\nb\n", &options);

            let copied = harness.drag_and_copy(Side::Left, (0, 0), (2, 1));
            assert_eq!(copied.as_deref(), Some("a\n\nb"), "{name}");
        }
    }

    #[test]
    fn hidden_whitespace_tokens_are_copied_in_ignore_whitespace_mode() {
        let options = DiffBuilderOptions {
            ignore: IgnoreOptions {
                whitespace: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut harness = CopyHarness::new("a  b\nc\n", "a b\nc\n", &options);

        let copied = harness.drag_and_copy(Side::Left, (0, 0), (1, 1));
        assert_eq!(copied.as_deref(), Some("a  b\nc"));
        let copied = harness.drag_and_copy(Side::Right, (0, 0), (1, 1));
        assert_eq!(copied.as_deref(), Some("a b\nc"));
    }

    const REAL_SOURCE: &str = "trait Processor {\n    fn run(&self);\n}\n\nstruct Item {\n    id: u64,\n    inner: u8,\n}\n\nfn go() {\n    println!(\"hello\");\n    old_body();\n}\n";
    const REAL_TARGET: &str = "trait NewProcessor {\n    fn run(&self);\n}\n\nstruct Item {\n    id: usize,\n}\n\nfn go() {\n    println!(\"hello world\");\n    new_body();\n    more();\n}\n";

    #[test]
    fn whole_diff_copies_each_files_exact_text() {
        for (name, options) in options_variants() {
            let mut harness = CopyHarness::new(REAL_SOURCE, REAL_TARGET, &options);
            let last = harness.rows().len() - 1;

            let copied = harness.drag_and_copy(Side::Left, (0, 0), (last, 1));
            assert_eq!(copied.as_deref(), Some(REAL_SOURCE.trim_end()), "left, {name}");
            let copied = harness.drag_and_copy(Side::Right, (0, 0), (last, 1));
            assert_eq!(copied.as_deref(), Some(REAL_TARGET.trim_end()), "right, {name}");
        }
    }

    // Each side has one line the other lacks. Ghost rows are off so the missing line is a Void
    // row, not ghost text, and a side's copy therefore holds only its own marker.
    const SIDES_SOURCE: &str = "top\nLEFTONLY\nm1\nm2\nm3\nbottom\n";
    const SIDES_TARGET: &str = "top\nm1\nm2\nm3\nRIGHTONLY\nbottom\n";

    fn sides_harness() -> CopyHarness {
        let options = DiffBuilderOptions {
            ghost_rows: false,
            ..Default::default()
        };
        CopyHarness::new(SIDES_SOURCE, SIDES_TARGET, &options)
    }

    fn code_rows(harness: &CopyHarness, side: Side) -> (usize, usize) {
        let is_code = |row: &DiffRow| {
            matches!(
                match side {
                    Side::Left => &row.left,
                    Side::Right => &row.right,
                },
                LineContent::Code { .. }
            )
        };
        let rows = harness.rows();
        let first = rows.iter().position(is_code).expect("side has code rows");
        let last = rows.iter().rposition(is_code).expect("side has code rows");
        (first, last)
    }

    fn whole_side(harness: &mut CopyHarness, side: Side) -> String {
        let (first, last) = code_rows(harness, side);
        harness
            .drag_and_copy(side, (first, 0), (last, usize::MAX))
            .expect("selection exists")
    }

    fn assert_only(copied: &str, own: &str, other: &str) {
        assert!(copied.contains(own), "missing {own}: {copied:?}");
        assert!(!copied.contains(other), "contains {other}: {copied:?}");
    }

    #[test]
    fn first_press_on_the_inactive_side_starts_a_selection_there() {
        let mut harness = sides_harness();

        // Left is active at startup, so this press is the first one on the inactive side.
        let copied = whole_side(&mut harness, Side::Right);
        assert_only(&copied, "RIGHTONLY", "LEFTONLY");
    }

    #[test]
    fn switching_sides_leaves_only_the_new_sides_selection() {
        let mut harness = sides_harness();

        assert_only(&whole_side(&mut harness, Side::Left), "LEFTONLY", "RIGHTONLY");
        assert_only(&whole_side(&mut harness, Side::Right), "RIGHTONLY", "LEFTONLY");
        assert_only(&whole_side(&mut harness, Side::Left), "LEFTONLY", "RIGHTONLY");
    }

    #[test]
    fn drag_ending_on_the_opposite_side_copies_only_the_active_side() {
        let mut harness = sides_harness();
        let (left_first, left_last) = code_rows(&harness, Side::Left);
        let (right_first, right_last) = code_rows(&harness, Side::Right);

        let copied = harness
            .drag_across_and_copy(
                (Side::Left, (left_first, 0)),
                (Side::Right, (right_last, 3)),
            )
            .expect("selection exists");
        assert!(!copied.contains("RIGHTONLY"), "{copied:?}");

        let copied = harness
            .drag_across_and_copy(
                (Side::Right, (right_first, 0)),
                (Side::Left, (left_last, 3)),
            )
            .expect("selection exists");
        assert!(!copied.contains("LEFTONLY"), "{copied:?}");
    }

    #[test]
    fn press_in_the_blank_part_of_a_row_starts_a_selection() {
        let mut harness = shared_harness();

        let copied = harness.drag_from_blank_and_copy(Side::Left, 0, (2, 5));
        assert_eq!(
            copied.as_deref(),
            Some("fn main() {\n    foo(bar, baz);\n    l")
        );
    }

    #[test]
    fn escape_clears_the_selection() {
        let mut harness = sides_harness();

        whole_side(&mut harness, Side::Left);
        harness.press_escape();
        assert_eq!(harness.copy(), None);
    }

    #[test]
    fn recompute_leaves_a_valid_or_cleared_selection() {
        let mut harness = sides_harness();
        whole_side(&mut harness, Side::Left);

        harness.set_row(
            2,
            DiffRow {
                left: LineContent::Void,
                right: LineContent::Void,
            },
        );
        if let Some(copied) = harness.copy() {
            for line in copied.lines().filter(|l| !l.is_empty()) {
                assert!(SIDES_SOURCE.lines().any(|s| s == line), "garbled line {line:?}");
            }
        }
    }

    #[test]
    fn scrolled_rows_select_by_their_visible_text() {
        let mut harness = CopyHarness::new(SOURCE, TARGET, &DiffBuilderOptions::default());
        // The gutter and the first two columns are out of view.
        harness.set_h_offset(GUTTER_WIDTH + GUTTER_GAP + 15.0);

        let copied = harness.drag_and_copy(Side::Left, (1, 4), (2, 9));
        assert_eq!(copied.as_deref(), Some("let x = 1;\n    let y"));
    }

    #[test]
    fn a_press_on_scrolled_out_text_goes_to_the_row_in_view_there() {
        let mut harness = CopyHarness::new(SOURCE, TARGET, &DiffBuilderOptions::default());
        harness.set_h_offset(GUTTER_WIDTH + GUTTER_GAP + 15.0);

        // The right row's first column is scrolled out over the left side, whose row takes the
        // press: the drag copies left text, never the right row's.
        let copied = harness.drag_across_and_copy((Side::Right, (2, 0)), (Side::Left, (2, 9)));
        assert_eq!(copied.as_deref(), Some(" = 2;"));
    }

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

    fn char_width() -> f32 {
        measure_in_fresh_context(|fonts, font_id| fonts.glyph_width(font_id, 'x'))
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

    /// Lays out `text` with one inline ghost at `offset`, wrapped at `width_chars` columns.
    /// Returns the ghost's position, the char width and the line height.
    fn inline_ghost_position(
        text: &str,
        offset: usize,
        ghost: &str,
        width_chars: f32,
    ) -> (eframe::egui::Vec2, f32, f32) {
        measure_in_fresh_context(|fonts, font_id| {
            let char_width = fonts.glyph_width(font_id, 'x');
            let job = eframe::egui::text::LayoutJob::single_section(
                text.to_owned(),
                eframe::egui::TextFormat::simple(font_id.clone(), eframe::egui::Color32::WHITE),
            );
            let ghosts = [GhostInsertion {
                byte_offset: offset,
                text: ghost.to_owned(),
            }];
            let side_text = layout_side_text(
                fonts,
                job,
                &ghosts,
                eframe::egui::Color32::WHITE,
                font_id,
                width_chars * char_width,
            );
            let [(pos, _)] = side_text.ghosts[..] else {
                panic!("expected one ghost");
            };
            (pos, char_width, fonts.row_height(font_id))
        })
    }

    fn assert_near(actual: eframe::egui::Vec2, expected: eframe::egui::Vec2) {
        assert!(
            (actual - expected).length() < 0.5,
            "{actual:?} vs {expected:?}"
        );
    }

    #[test]
    fn an_inline_ghost_on_a_wrapped_row_sits_before_its_glyph_on_that_row() {
        // "aaaa bbbb " fills the first row; "cc " then the ghost, then "dd" on the second.
        let (pos, char_width, line_height) =
            inline_ghost_position("aaaa bbbb cc dd", 13, "X", 10.5);

        assert_near(pos, eframe::egui::vec2(3.0 * char_width, line_height));
    }

    #[test]
    fn an_inline_ghost_where_the_text_wraps_goes_at_the_end_of_the_row_before() {
        // The gap pushes "cccc" to the next row, which starts at its first glyph, gap gone.
        let (pos, char_width, _) = inline_ghost_position("aaaa bbbb cccc", 10, "XYZW", 10.5);

        assert_near(pos, eframe::egui::vec2(10.0 * char_width, 0.0));
    }

    const LONG_LINE: &str = "let words = [alpha, beta, gamma, delta, epsilon, zeta, eta, theta];";

    /// A column that wraps `LONG_LINE` onto at least three rows.
    fn narrow_column() -> f32 {
        GUTTER_WIDTH + GUTTER_GAP + 20.0 * char_width()
    }

    #[test]
    fn a_wrapped_line_copies_as_the_one_file_line() {
        let text = format!("{LONG_LINE}\nnext\n");
        let mut harness = CopyHarness::new(&text, &text, &DiffBuilderOptions::default());
        harness.set_wrap([narrow_column(); 2]);

        let copied = harness.drag_and_copy(Side::Left, (0, 0), (1, 4));
        assert_eq!(copied, Some(format!("{LONG_LINE}\nnext")));
        // From the first visual row of the line into its last one.
        let copied = harness.drag_and_copy(Side::Right, (0, 4), (0, 60));
        assert_eq!(copied.as_deref(), Some(&LONG_LINE[4..60]));
    }

    #[test]
    fn a_short_side_in_a_tall_row_copies_without_a_blank_line() {
        let text = format!("{LONG_LINE}\nnext\n");
        let mut harness = CopyHarness::new(&text, &text, &DiffBuilderOptions::default());
        // The left wraps and makes row 0 tall; the right's copy of it is one line.
        harness.set_wrap([narrow_column(), 1000.0]);

        let copied = harness.drag_and_copy(Side::Right, (0, 0), (1, 4));
        assert_eq!(copied, Some(format!("{LONG_LINE}\nnext")));
    }

    #[test]
    fn a_void_side_in_a_tall_row_copies_nothing() {
        let options = DiffBuilderOptions {
            ghost_rows: false,
            ..Default::default()
        };
        let target = format!("top\n{LONG_LINE}\nbottom\n");
        let mut harness = CopyHarness::new("top\nbottom\n", &target, &options);
        assert!(matches!(harness.rows()[1].left, LineContent::Void));
        harness.set_wrap([narrow_column(); 2]);

        let copied = harness.drag_and_copy(Side::Left, (0, 0), (2, 6));
        assert_eq!(copied.as_deref(), Some("top\nbottom"));
        let copied = harness.drag_and_copy(Side::Right, (0, 0), (2, 6));
        assert_eq!(copied, Some(target.trim_end().to_owned()));
    }

    #[test]
    fn copy_with_wrap_on_gives_each_files_exact_text() {
        for (name, options) in options_variants() {
            let mut harness = CopyHarness::new(REAL_SOURCE, REAL_TARGET, &options);
            harness.set_wrap([GUTTER_WIDTH + GUTTER_GAP + 12.0 * char_width(); 2]);
            let last = harness.rows().len() - 1;

            let copied = harness.drag_and_copy(Side::Left, (0, 0), (last, 1));
            assert_eq!(
                copied.as_deref(),
                Some(REAL_SOURCE.trim_end()),
                "left, {name}"
            );
            let copied = harness.drag_and_copy(Side::Right, (0, 0), (last, 1));
            assert_eq!(
                copied.as_deref(),
                Some(REAL_TARGET.trim_end()),
                "right, {name}"
            );
        }
    }

    /// Top of the table body and the rect of row `target` after jumping to it, as the pane jumps
    /// to a conflict, find result or line.
    fn jump_to_row(
        wrapped_heights: Option<&[f32]>,
        num_rows: usize,
        target: usize,
    ) -> (f32, eframe::egui::Rect) {
        use eframe::egui;
        let ctx = egui::Context::default();
        let mut body_top = f32::NAN;
        let mut target_rect = egui::Rect::NOTHING;
        // The first frame lays out and asks for the scroll; the next ones apply it.
        for frame in 0..3 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 400.0),
                )),
                time: Some(frame as f64),
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    body_top = ui.cursor().top();
                    SplitTable::new("jump", &mut Split::default())
                        .scroll_to_row(target, Some(egui::Align::Min))
                        .show(
                            ui,
                            0.0,
                            |_, _| {},
                            super::text_rows(wrapped_heights, 15.0, num_rows),
                            |index, column, ui| {
                                if index == target && column == SplitColumn::Left {
                                    target_rect = ui.max_rect();
                                }
                            },
                        );
                });
            });
        }
        (body_top, target_rect)
    }

    #[test]
    fn jumping_to_a_row_lands_on_it_with_wrap_on() {
        // Every third row wraps onto three lines.
        let heights: Vec<f32> = (0..200)
            .map(|i| if i % 3 == 0 { 45.0 } else { 15.0 })
            .collect();

        for target in [1, 30, 100, 151] {
            let (top, rect) = jump_to_row(Some(&heights), heights.len(), target);
            assert!(
                (rect.top() - top).abs() < 1.0,
                "row {target}: {rect:?} vs top {top}"
            );
            assert_eq!(rect.height(), heights[target], "row {target}");
        }
        // Wrap off: one line per row.
        let (top, rect) = jump_to_row(None, 200, 100);
        assert!((rect.top() - top).abs() < 1.0, "{rect:?} vs top {top}");
        assert_eq!(rect.height(), 15.0);
    }

    // Identical on both sides except the last line, so the shared rows carry no ghosts.
    const OCCURRENCE_SOURCE: &str =
        "let value = 1;\nlet other = value + value;\nfoo(value);\nold\n";
    const OCCURRENCE_TARGET: &str =
        "let value = 1;\nlet other = value + value;\nfoo(value);\nnew\n";

    fn occurrence_harness() -> CopyHarness {
        CopyHarness::new(
            OCCURRENCE_SOURCE,
            OCCURRENCE_TARGET,
            &DiffBuilderOptions::default(),
        )
    }

    #[test]
    fn a_single_row_drag_searches_for_exactly_the_text_egui_copies() {
        let mut harness = occurrence_harness();
        for (side, from, to) in [
            (Side::Left, (0, 4), (0, 9)),
            (Side::Left, (1, 14), (1, 6)),
            (Side::Right, (2, 0), (2, 5)),
        ] {
            harness.drag(side, from, to);
            let copied = harness.copy();
            assert!(copied.is_some(), "{side:?} {from:?}..{to:?}");
            assert_eq!(
                harness.occurrence_needle(),
                copied,
                "{side:?} {from:?}..{to:?}"
            );
        }
    }

    #[test]
    fn a_double_clicked_word_is_searched_for() {
        let mut harness = occurrence_harness();

        harness.double_click(Side::Left, (1, 14));

        assert_eq!(harness.occurrence_needle().as_deref(), Some("value"));
        assert_eq!(harness.copy().as_deref(), Some("value"));
    }

    #[test]
    fn every_other_occurrence_is_highlighted_on_both_sides() {
        let mut harness = occurrence_harness();

        // The second "value" in row 1.
        harness.drag(Side::Left, (1, 20), (1, 25));
        assert_eq!(harness.occurrence_needle().as_deref(), Some("value"));

        assert_eq!(harness.occurrence_ranges(Side::Left, 0), [4..9]);
        assert_eq!(harness.occurrence_ranges(Side::Left, 1), [12..17]);
        assert_eq!(harness.occurrence_ranges(Side::Left, 2), [4..9]);
        assert!(harness.occurrence_ranges(Side::Left, 3).is_empty());
        // The other side's identical row has no selection, so both of its matches show.
        assert_eq!(harness.occurrence_ranges(Side::Right, 1), [12..17, 20..25]);
        assert_eq!(harness.occurrence_ranges(Side::Right, 2), [4..9]);
    }

    #[test]
    fn a_selection_across_rows_searches_for_nothing() {
        let mut harness = occurrence_harness();

        // Through the row first, so a selection in it exists before the drag leaves it.
        harness.drag_through(Side::Left, &[(0, 4), (0, 9), (1, 9)]);

        assert_eq!(harness.copy().as_deref(), Some("value = 1;\nlet other"));
        assert_eq!(harness.occurrence_needle(), None);
        assert!(harness.occurrence_ranges(Side::Left, 2).is_empty());
    }

    #[test]
    fn short_and_whitespace_only_selections_search_for_nothing() {
        let mut harness = CopyHarness::new(
            "let value = 1;\n    value;\n",
            "let value = 1;\n    value;\n",
            &DiffBuilderOptions::default(),
        );

        harness.drag(Side::Left, (0, 4), (0, 5));
        assert_eq!(harness.copy().as_deref(), Some("v"));
        assert_eq!(harness.occurrence_needle(), None);

        harness.drag(Side::Left, (1, 0), (1, 4));
        assert_eq!(harness.copy().as_deref(), Some("    "));
        assert_eq!(harness.occurrence_needle(), None);

        harness.drag(Side::Left, (1, 3), (1, 6));
        assert_eq!(harness.occurrence_needle().as_deref(), Some(" va"));
    }

    #[test]
    fn the_highlight_clears_with_the_selection() {
        let mut harness = occurrence_harness();

        harness.drag(Side::Left, (0, 4), (0, 9));
        assert_eq!(harness.occurrence_needle().as_deref(), Some("value"));
        harness.press_escape();
        assert_eq!(harness.occurrence_needle(), None);
        assert!(harness.occurrence_ranges(Side::Left, 1).is_empty());

        harness.drag(Side::Left, (0, 4), (0, 9));
        assert_eq!(harness.occurrence_needle().as_deref(), Some("value"));
        harness.click_outside_the_rows();
        assert_eq!(harness.occurrence_needle(), None);

        // A plain click in a row selects nothing.
        harness.drag(Side::Left, (0, 4), (0, 9));
        harness.drag(Side::Left, (1, 4), (1, 4));
        assert_eq!(harness.occurrence_needle(), None);
    }

    #[test]
    fn only_the_visible_rows_are_searched() {
        let source: String = (0..2000)
            .map(|i| format!("let value_{i} = {i};\n"))
            .collect();
        let harness = CopyHarness::new(&source, &source, &DiffBuilderOptions::default());

        let height = 400.0;
        let (searched, row_h) = harness.rows_searched_in_table("value", height);

        assert!(searched > 0);
        assert!(
            searched as f32 <= height / row_h + 2.0,
            "{searched} rows searched of {} at {row_h} per row",
            harness.rows().len()
        );
    }

    #[test]
    fn harness_builds_rows_with_ghost_tokens() {
        let harness = CopyHarness::new(SOURCE, TARGET, &DiffBuilderOptions::default());

        let has_ghost = harness.rows().iter().any(|row| {
            [&row.left, &row.right].into_iter().any(|side| match side {
                LineContent::Code { tokens, .. } => tokens.iter().any(|(_, _, is_ghost)| *is_ghost),
                _ => false,
            })
        });
        assert!(has_ghost, "expected at least one ghost token in {:?}", harness.rows());
    }
}
