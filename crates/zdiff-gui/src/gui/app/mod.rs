mod input;
mod launch;
mod menu;
mod nav;
mod quickdiff;
mod revert_write;
mod state;
mod ui;
#[cfg(test)]
mod frame_tests;
use launch::{LaunchOutcome, apply_launch};
use zcommon_ui::window_memory::WindowMemory;
use state::AppState;

use eframe::egui::{self};
use serde::{Deserialize, Serialize};
use std::{
    env,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};
use zcommon_ui::frame_stats::FrameStats;
use zcommon_ui::h_scroll::{self};

use zcommon_ui::keymap_editor::KeymapEditor;
use zcommon_ui::shared_keymap::load_shared;
use zcommon_ui::style_window::StyleWindow;

use crate::{
    cli::{self},
    diff_ctx::UpdateDiffRowsInput,
    file::LoadedFile,
    keybindings::FileAction,
    p4::update_p4_profiles,
    revert::{PendingP4Edit, RevertTarget},
    ui_egui::{
        diff_pane::{FileDiffPane, wheel_rects_id},
        merge_pane::{MergeView, OpenMergeForm},
        panes::Pane,
    },
    viewer::{
        LoadedSide, SharedKinds, ViewerKind, ViewerResolution, Viewers, conflict_count,
        decode_fallback, ffmpeg_fallback, image_decode_fallback, resolve_viewer,
        video_meta::VideoTab,
    },
};

#[derive(Serialize, Deserialize)]
// #[serde(bound(serialize = "", deserialize = "T: RawToken"))]
pub struct ZApp {
    #[serde(default = "default_zoom", deserialize_with = "deserialize_zoom")]
    zoom: f32,
    // Option > Hack to avoid cloning state when matching &mut self.state in update loop
    state: Option<AppState>,
    tree: egui_tiles::Tree<Pane>,

    #[serde(skip)]
    open_shortcuts_window: bool,
    #[serde(skip)]
    keymap_editor: KeymapEditor<FileAction>,
    #[serde(skip)]
    style_window: StyleWindow,
    #[serde(skip)]
    open_universal_path_window: bool,
    #[serde(skip)]
    open_viewers_window: bool,
    /// The diff collects the phase trace while it is open.
    #[serde(skip)]
    open_phase_trace_window: bool,
    #[serde(skip)]
    phase_overlay: bool,
    /// A revert waiting on the p4 edit prompt.
    #[serde(skip)]
    pending_p4_edit: Option<(RevertTarget, PendingP4Edit)>,
    #[serde(skip)]
    toggle_active_side: bool,
    #[serde(skip)]
    open_help_window: bool,
    #[serde(skip)]
    launch_error: Option<String>,
    /// Shown instead of the file diff while open.
    #[serde(skip)]
    merge: Option<MergeView>,
    #[serde(skip)]
    open_merge_form: OpenMergeForm,
    /// Read by main once the window closed: the exit code of the last closed merge view.
    #[serde(skip)]
    exit_code: Arc<AtomicU8>,
    #[serde(skip)]
    window_title: Option<String>,
    #[serde(skip)]
    window_memory: WindowMemory,
    #[serde(skip)]
    frame_stats: FrameStats,
    #[serde(default)]
    show_frame_breakdown: bool,
}

fn default_zoom() -> f32 {
    1.0
}

fn clamp_zoom(zoom: f32) -> f32 {
    zoom.clamp(0.5, 3.0)
}

fn deserialize_zoom<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<f32, D::Error> {
    f32::deserialize(deserializer).map(clamp_zoom)
}

impl ZApp {
    pub fn report_exit_code_to(&mut self, exit_code: Arc<AtomicU8>) {
        self.exit_code = exit_code;
    }

    pub fn remember_window_from(&mut self, storage: Option<&dyn eframe::Storage>) {
        self.window_memory = WindowMemory::load(storage);
    }

    pub fn request_init(&mut self) {
        log::info!(
            "Request init called with state: {}",
            self.state
                .as_ref()
                .and_then(|f| Some(f.variant_name()))
                .unwrap_or_default()
        );
        self.state = self
            .state
            .take()
            .map(|ctx| AppState::Startup(ctx.into_ctx()));

        if let Some(state) = &mut self.state {
            match state {
                AppState::Startup(ctx) | AppState::Idle(ctx) => {
                    update_p4_profiles(ctx.p4_profiles.clone());
                    ctx.kinds = SharedKinds::load(
                        zcommon::kinds::kinds_file_path(),
                        &mut ctx.legacy_extension_map,
                    );

                    let (_, args) = zcommon::console::take_log_flag(env::args().skip(1));
                    if args.is_empty() {
                        return;
                    }
                    match apply_launch(
                        &mut ctx.file_1,
                        &mut ctx.file_2,
                        cli::parse_launch_args(args),
                    ) {
                        LaunchOutcome::Opened => {}
                        LaunchOutcome::Merge(request, names) => {
                            self.merge = Some(
                                MergeView::new(*request, ctx.myers_diff_algorithm)
                                    .with_names(names),
                            );
                        }
                        LaunchOutcome::Help => self.open_help_window = true,
                        LaunchOutcome::Error(message) => {
                            // A tool trusting the exit code must not take a failed launch as a merge.
                            self.exit_code.store(1, Ordering::Relaxed);
                            self.launch_error = Some(message);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub fn new() -> Self {
        Self {
            zoom: default_zoom(),
            state: Some(AppState::default()),
            tree: Self::create_tree(),
            open_shortcuts_window: false,
            keymap_editor: KeymapEditor::default(),
            style_window: StyleWindow::default().with_row_looks().with_syntax(),
            open_universal_path_window: false,
            open_viewers_window: false,
            open_phase_trace_window: false,
            phase_overlay: false,
            pending_p4_edit: None,
            toggle_active_side: false,
            open_help_window: false,
            launch_error: None,
            merge: None,
            open_merge_form: OpenMergeForm::default(),
            exit_code: Arc::default(),
            window_title: None,
            window_memory: WindowMemory::default(),
            frame_stats: FrameStats::default(),
            show_frame_breakdown: false,
        }
    }

    pub(super) fn startup(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.style_window.load(ctx);
        log::debug!("pixels_per_point {}", ctx.pixels_per_point());
        log::debug!(
            "native_pixels_per_point {:?}",
            ctx.native_pixels_per_point()
        );
        ctx.set_zoom_factor(self.zoom);
        // ctx.set_debug_on_hover(true);
    }

    pub(super) fn sync_zoom(&mut self, ctx: &egui::Context) {
        let zoom = clamp_zoom(ctx.zoom_factor());
        if zoom != ctx.zoom_factor() {
            ctx.set_zoom_factor(zoom);
        }
        self.zoom = zoom;
    }

    pub(super) fn create_tree() -> egui_tiles::Tree<Pane> {
        let mut tiles = egui_tiles::Tiles::default();

        let mut tabs = vec![];

        let tile_path_diff = tiles.insert_pane(Pane::FileDiff(FileDiffPane::new(Some(
            "Path Diff".to_string(),
        ))));

        // let master_tile = tiles.insert_horizontal_tile(vec![tile_duplicate_file]);
        let master_tile = tiles.insert_horizontal_tile(vec![tile_path_diff]);
        tabs.push(tiles.insert_vertical_tile(vec![master_tile]));

        let root = tiles.insert_tab_tile(tabs);

        egui_tiles::Tree::new("my_tree", root, tiles)
    }
}

impl eframe::App for ZApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let Some(text_rects) = ctx.data_mut(|data| {
            let rects = data.get_temp::<[egui::Rect; 2]>(wheel_rects_id());
            data.remove::<[egui::Rect; 2]>(wheel_rects_id());
            rects
        }) else {
            return;
        };
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        h_scroll::remap_wheel_events(
            &mut raw_input.events,
            pointer,
            text_rects,
            state.ctx_mut().h_scroll_modifier,
        );
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.window_memory.keep_restorable(storage);
        #[cfg(feature = "serde")]
        if let Ok(json) = serde_json::to_string(self) {
            storage.set_string(eframe::APP_KEY, json);
            log::debug!("App state saved");
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.frame_stats.begin_frame(ctx, frame);
        // Update conflict cursor before input processing
        {
            let app_ctx = self
                .state
                .as_mut()
                .expect("State was not valid while processing inputs")
                .ctx_mut();
            let strings_view = app_ctx.strings_view();
            let text_conflicts = if !strings_view
                && app_ctx
                    .viewers
                    .is_some_and(|v| v != Viewers::Pair(ViewerKind::Text))
            {
                0
            } else {
                app_ctx
                    .diff_processor
                    .get_minimal_diff_ctx()
                    .as_ref()
                    .and_then(|f| Some(f.precomputed_diffs.len()))
                    .unwrap_or_default()
            };
            let hex_stops = if strings_view {
                text_conflicts
            } else {
                app_ctx.hex_processor.nav_count()
            };
            let conflict_max = conflict_count(app_ctx.viewers, text_conflicts, hex_stops);
            app_ctx.diff_processor.conflict_cursor.set_max(conflict_max);
        }

        let state = self
            .state
            .as_mut()
            .expect("State should always be valid during update");
        if matches!(state, AppState::Startup(_))
            || ctx.input(|i| i.events.contains(&egui::Event::WindowFocused(true)))
        {
            load_shared(&mut state.ctx_mut().keybindings.keymap);
        }

        self.process_ctx_inputs(ctx, frame);
        self.frame_stats.mark("inputs");

        let current_state = self
            .state
            .take()
            .expect("State should always be valid during update");

        let next_state = match current_state {
            AppState::Startup(mut state_ctx) => {
                self.startup(ctx, frame);
                state_ctx.file_1.set_repaint(ctx);
                state_ctx.file_2.set_repaint(ctx);
                state_ctx.diff_processor.set_repaint(ctx);
                // Nothing else schedules the first Idle frame, which starts the loads.
                ctx.request_repaint();
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.centered_and_justified(|ui| ui.label("Loading..."));
                });
                AppState::Idle(state_ctx)
            }

            AppState::Idle(mut state) => {
                self.sync_zoom(ctx);

                state.file_1.set_pdb_options(&state.pdb_options);
                state.file_2.set_pdb_options(&state.pdb_options);
                let loaded_1 = state.file_1.get_loaded_file();
                let loaded_2 = state.file_2.get_loaded_file();
                let path_1 = state.file_1.get_full_path();
                let path_2 = state.file_2.get_full_path();
                state.viewer_override.observe_pair(&path_1, &path_2);
                state.observe_history();
                fn side(f: &LoadedFile) -> LoadedSide<'_> {
                    LoadedSide {
                        path: f.path(),
                        sniffed: f.viewer_kind(),
                    }
                }
                let resolution = resolve_viewer(
                    state.viewer_override.kind,
                    &state.kinds.table,
                    loaded_1.as_ref().map(side),
                    loaded_2.as_ref().map(side),
                );
                let files_for = |resolution: &Option<ViewerResolution>, kind| {
                    resolution.as_ref().map_or([None, None], |r| {
                        r.viewers
                            .files_for(kind, [loaded_1.clone(), loaded_2.clone()])
                    })
                };
                // Decoding follows the first resolution, so a failed decode stays cached (and
                // the pair stays in Hex) instead of being retried every frame. No files drops
                // the old pair's pixels and textures.
                let [image_1, image_2] = files_for(&resolution, ViewerKind::Image);
                state.image_processor.request(image_1, image_2);
                state
                    .image_processor
                    .poll(ctx.input(|i| i.max_texture_side));
                let resolution =
                    resolution.map(|r| image_decode_fallback(r, &state.image_processor.failures()));
                // Like the image decode above: probing follows the first resolution.
                let video_files = files_for(&resolution, ViewerKind::Video);
                let video = video_files.iter().any(Option::is_some);
                let ffmpeg = video.then(|| state.ffmpeg.lookup().ok()).flatten();
                state.video_frames.request(video_files, ffmpeg);
                state.video_frames.poll(ctx.input(|i| i.max_texture_side));
                let resolution = resolution
                    .map(|r| decode_fallback(r, ViewerKind::Video, &state.video_frames.failures()));
                // Only a media pair looks ffmpeg up.
                let resolution = resolution.map(|r| {
                    if r.viewers.shows(ViewerKind::Audio) || r.viewers.shows(ViewerKind::Video) {
                        ffmpeg_fallback(r, state.ffmpeg.lookup())
                    } else {
                        r
                    }
                });
                // Video audio decodes on the first Audio tab open and stays until the pair changes.
                let viewers = resolution.as_ref().map(|r| r.viewers);
                let [audio_1, audio_2] = if viewers == Some(Viewers::Pair(ViewerKind::Video)) {
                    state.audio_processor.video_pair_files(
                        state.video_meta.tab == VideoTab::Audio,
                        [loaded_1.clone(), loaded_2.clone()],
                    )
                } else {
                    files_for(&resolution, ViewerKind::Audio)
                };
                if (audio_1.is_some() || audio_2.is_some())
                    && let Ok(ffmpeg) = state.ffmpeg.lookup()
                {
                    state.audio_processor.set_context(ctx);
                    state.audio_processor.request(ffmpeg, audio_1, audio_2);
                } else {
                    // Kills running decodes and drops the old pair's samples.
                    state.audio_processor.clear();
                }
                state.audio_processor.poll();
                let resolution = resolution.map(|r| {
                    decode_fallback(r, ViewerKind::Audio, &state.audio_processor.failures())
                });
                // Resolved every frame, so only a changed reason is logged.
                let fallback = resolution.as_ref().and_then(|r| r.fallback.clone());
                if fallback != state.viewer_fallback {
                    if let Some(reason) = &fallback {
                        log::warn!("{}", reason);
                    }
                    state.viewer_fallback = fallback;
                }
                state.viewers = resolution.as_ref().map(|r| r.viewers);
                // Metadata compares two files, so a mismatched pair has none.
                let audio_pair = match (state.viewers, &loaded_1, &loaded_2) {
                    (Some(Viewers::Pair(ViewerKind::Audio)), Some(a), Some(b)) => state
                        .ffmpeg
                        .lookup()
                        .ok()
                        .map(|ffmpeg| ([a.clone(), b.clone()], ffmpeg)),
                    _ => None,
                };
                state.audio_meta.request(audio_pair);
                let video_pair = match (state.viewers, &loaded_1, &loaded_2) {
                    (Some(Viewers::Pair(ViewerKind::Video)), Some(a), Some(b)) => state
                        .ffmpeg
                        .lookup()
                        .ok()
                        .map(|ffmpeg| ([a.clone(), b.clone()], ffmpeg)),
                    _ => None,
                };
                state.video_meta.request(video_pair);
                let [hex_1, hex_2] = files_for(&resolution, ViewerKind::Hex);
                let hex_files = [hex_1.clone(), hex_2.clone()];
                // The cursor may hold the text diff's or the previous pair's stop.
                if state.hex_processor.request(ctx, hex_1, hex_2)
                    && state.viewers == Some(Viewers::Pair(ViewerKind::Hex))
                {
                    state.diff_processor.conflict_cursor.set(0);
                }
                state.hex_processor.poll();

                // The text diff's state is the other mode's (or another pair's) on a switch.
                let strings_view = state.strings_view();
                if strings_view != state.strings_active {
                    state.strings_active = strings_view;
                    state.diff_processor.cancel_in_progress();
                    state.diff_processor.in_progress_input = None;
                    state.diff_processor.reset_ctx();
                }
                if strings_view {
                    state
                        .hex_strings
                        .request(ctx, hex_files, state.hex_settings.min_string_len);
                    state.hex_strings.poll();
                } else {
                    state.hex_strings.clear();
                }
                self.frame_stats.mark("viewers");
                let strings_files = state
                    .hex_strings
                    .sides()
                    .map(|sides| sides.each_ref().map(|s| s.as_ref().map(|s| s.file.clone())));

                let is_text = if strings_view {
                    strings_files.is_some()
                } else {
                    state.viewers.is_none_or(|v| v.shows(ViewerKind::Text))
                };
                let loaded = [state.file_1.get_loaded_file(), state.file_2.get_loaded_file()];
                let cached = loaded.each_ref().map(|f| f.as_ref().and_then(|f| f.text().cloned()));
                let dumps = loaded.each_ref().map(|f| f.as_ref().is_some_and(LoadedFile::is_dump));
                // A mismatched pair's text side is diffed alone, so its rows are all one side's.
                // Pins pair lines of both sides, and the cursor may hold another viewer's stop.
                // The strings of a hex pair are other lines than the pins' files.
                let ([text_1, text_2], pins, pdb_dumps) = match (&strings_files, state.viewers) {
                    (Some(files), _) => (files.clone(), Vec::new(), [false; 2]),
                    (None, Some(viewers @ Viewers::Sides(_))) => {
                        state.diff_processor.conflict_cursor.set(0);
                        let dumps = viewers.files_for(ViewerKind::Text, dumps.map(Some));
                        (
                            viewers.files_for(ViewerKind::Text, cached),
                            Vec::new(),
                            dumps.map(|dump| dump == Some(true)),
                        )
                    }
                    _ => (cached, state.diff_processor.pins().to_vec(), dumps),
                };

                let update_input = UpdateDiffRowsInput {
                    file_1: text_1,
                    file_2: text_2,
                    options: state.diff_options.clone(),
                    myers_diff_algorithm: state.myers_diff_algorithm.clone(),
                    pins,
                    collect_trace: self.open_phase_trace_window || self.phase_overlay,
                    pdb_dumps,
                    strings_view: strings_files.is_some(),
                };

                let diff_ctx_invalidated = if state.file_1.get_loading_path().is_some()
                    || state.file_2.get_loading_path().is_some()
                {
                    false
                } else if let Some(in_progress_input) = &state.diff_processor.in_progress_input {
                    *in_progress_input != update_input
                } else if let Some(diff_ctx) = state.diff_processor.get_minimal_diff_ctx() {
                    let input_equal = update_input == diff_ctx.input;

                    if !input_equal && !state.diff_processor.in_progress_input.is_some() {
                        log::debug!("diff_ctx invalidated!");
                    }
                    !input_equal
                } else {
                    !state.diff_processor.in_progress_input.is_some()
                };

                if diff_ctx_invalidated
                    && is_text
                    && (strings_files.is_some()
                        || state.file_1.get_cached_file().is_some()
                        || state.file_2.get_cached_file().is_some())
                {
                    state.diff_processor.request_update(update_input);
                }

                state.diff_processor.update();
                self.frame_stats.mark("diff");

                self.ui(ctx, frame, &mut state);

                AppState::Idle(state)
            }

            AppState::Exit(state) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                AppState::Exit(state)
            }
        };

        self.state = Some(next_state);
    }
}
