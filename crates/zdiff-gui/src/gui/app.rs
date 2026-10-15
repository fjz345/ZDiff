use eframe::egui::{self, Layout, PointerButton, containers::menu::MenuConfig};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
};
use zcommon::find::FindQuery;
use zcommon::presets::UserPresets;
use zcommon_ui::common::show_custom_popup;
use zcommon_ui::find_popup::{FindPopupEvent, find_popup};
use zcommon_ui::h_scroll::{self, WheelModifier};
use zcommon_ui::keymap::{Action, Shortcut, menu_item};
use zcommon_ui::keymap_editor::KeymapEditor;
use zcommon_ui::shared_keymap::load_shared;
use zcommon_ui::style_window::StyleWindow;
use zdiff::{
    diff_builder::DiffBuilderOptions,
    myers::{DiffPhases, MyersDiffAlgorithm},
    universal_path::{DepotRev, UniversalPath},
};

use eframe::epaint::Pos2;
use egui_tiles::Tile;

use crate::{
    cli::{self, LaunchRequest},
    diff_ctx::{DiffProcessor, UpdateDiffRowsInput},
    diff_phases::diff_phases_menu,
    file::{FileProcessor, LoadedFile},
    keybindings::{FileAction, Keybindings, QuickDiffPaths, ui_keybindings},
    language_icon::{emoji_renders, language_icon, show_language_icon},
    p4::{
        P4Command, P4Profiles, P4Runner, list_shelf, local_path_of_depot_file, set_p4_slot_profile,
        ui_p4_profiles, update_p4_profiles,
    },
    phase_trace_window::ui_phase_trace,
    revert::{
        self, HistoryStep, PendingP4Edit, RevertHistory, RevertRecord, RevertRefusal, RevertTarget,
        RevertWrite, WriteRefusal,
    },
    toggle_all::{OptionSet, ToggleAll},
    ui_egui::{
        diff_pane::{FileDiffPane, FileDiffPaneCtx, wheel_rects_id},
        file_role::FileRole,
        merge_pane::{self, Closing, MergeNames, MergeRequest, MergeView, OpenMergeForm},
        panes::{Pane, TreeBehavior},
        side_viewers::SideViewers,
        syntax::DetectedLanguage,
        text_mode::TextMode,
    },
    viewer::{
        FfmpegSetting, LoadedSide, SharedKinds, ViewerKind, ViewerOverride, ViewerResolution,
        Viewers,
        audio::{AudioDiffProcessor, AudioSettings},
        audio_meta::AudioMetaProcessor,
        conflict_count, decode_fallback, ffmpeg_fallback,
        hex::{HexDiffProcessor, parse_hex_offset},
        hex_strings::{HexMode, HexSettings, HexStrings},
        image::{ImageDiffProcessor, ImageSettings},
        image_decode_fallback, resolve_viewer, ui_extension_map, ui_ffmpeg_setting,
        video_frames::VideoFrameProcessor,
        video_meta::{VideoMetaProcessor, VideoTab},
    },
};

#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AppStateCtx {
    pub file_1: FileProcessor,
    pub file_2: FileProcessor,

    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub diff_processor: DiffProcessor,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub hex_processor: HexDiffProcessor,
    #[cfg_attr(feature = "serde", serde(default))]
    pub hex_settings: HexSettings,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub hex_strings: HexStrings,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub strings_active: bool,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub image_processor: ImageDiffProcessor,
    #[cfg_attr(feature = "serde", serde(default))]
    pub image_settings: ImageSettings,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub audio_processor: AudioDiffProcessor,
    #[cfg_attr(feature = "serde", serde(default))]
    pub audio_settings: AudioSettings,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub audio_meta: AudioMetaProcessor,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub video_frames: VideoFrameProcessor,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub video_meta: VideoMetaProcessor,
    /// Resolved each frame from the loaded files; `None` while neither side is loaded.
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub viewers: Option<Viewers>,
    /// Not persisted: paths aren't either, so a restart always opens a new pair.
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub viewer_override: ViewerOverride,
    /// The last logged reason the pair isn't in the viewer it asked for.
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub viewer_fallback: Option<String>,
    // Pre-shared-table viewer map, kept in the save until migrated into the kind file.
    #[cfg_attr(
        feature = "serde",
        serde(
            default,
            rename = "extension_map",
            skip_serializing_if = "Option::is_none"
        )
    )]
    pub legacy_extension_map: Option<BTreeMap<String, ViewerKind>>,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub kinds: SharedKinds,
    #[cfg_attr(feature = "serde", serde(default))]
    pub ffmpeg: FfmpegSetting,
    /// Undo and redo of reverts; in memory only.
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub revert_history: RevertHistory,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub toggle_all: ToggleAll,

    pub diff_options: DiffBuilderOptions,

    pub myers_diff_algorithm: MyersDiffAlgorithm,
    // A new key, so saves pinned to the old "rs" default load as Auto.
    #[cfg_attr(feature = "serde", serde(default))]
    pub code_language_override: Option<String>,
    pub code_language_custom: String,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub detected_languages: [DetectedLanguage; 2],

    // ### Keybindings
    pub keybindings: Keybindings,

    #[cfg_attr(feature = "serde", serde(default))]
    pub p4_profiles: P4Profiles,
    #[cfg_attr(feature = "serde", serde(default))]
    pub regex_ignore_presets: UserPresets,

    // ### UI TEMP
    pub scroll_left: f32,
    pub scroll_right: f32,
    // Saves from before the setting load linked, like a fresh state.
    #[cfg_attr(feature = "serde", serde(default = "default_h_scroll_linked"))]
    pub h_scroll_linked: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    pub h_scroll_modifier: WheelModifier,
    #[cfg_attr(feature = "serde", serde(default))]
    pub word_wrap: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    pub text_mode: TextMode,

    #[cfg_attr(feature = "serde", serde(skip))]
    pub goto_open: bool,
    #[cfg_attr(feature = "serde", serde(skip))]
    pub goto_input: String,
    #[cfg_attr(feature = "serde", serde(skip))]
    pub find_open: bool,
    #[cfg_attr(feature = "serde", serde(skip))]
    pub find_query: FindQuery,
    #[cfg_attr(feature = "serde", serde(skip))]
    pub find_error: Option<String>,
}

fn default_h_scroll_linked() -> bool {
    true
}

impl Default for AppStateCtx {
    fn default() -> Self {
        Self {
            file_1: Default::default(),
            file_2: Default::default(),
            diff_options: Default::default(),
            diff_processor: Default::default(),
            hex_processor: Default::default(),
            hex_settings: Default::default(),
            hex_strings: Default::default(),
            strings_active: false,
            image_processor: Default::default(),
            image_settings: Default::default(),
            audio_processor: Default::default(),
            audio_settings: Default::default(),
            audio_meta: Default::default(),
            video_frames: Default::default(),
            video_meta: Default::default(),
            viewers: None,
            viewer_override: Default::default(),
            viewer_fallback: None,
            legacy_extension_map: None,
            kinds: Default::default(),
            ffmpeg: Default::default(),
            revert_history: Default::default(),
            toggle_all: Default::default(),
            scroll_left: Default::default(),
            scroll_right: Default::default(),
            h_scroll_linked: default_h_scroll_linked(),
            h_scroll_modifier: WheelModifier::Shift,
            word_wrap: false,
            text_mode: Default::default(),
            goto_open: Default::default(),
            find_open: Default::default(),
            goto_input: Default::default(),
            find_query: Default::default(),
            find_error: None,
            keybindings: Default::default(),
            p4_profiles: Default::default(),
            regex_ignore_presets: Default::default(),
            myers_diff_algorithm: Default::default(),
            code_language_override: None,
            code_language_custom: "".into(),
            detected_languages: Default::default(),
        }
    }
}

impl AppStateCtx {
    /// The hex pair is shown as the text diff of its extracted strings.
    fn strings_view(&self) -> bool {
        self.viewers == Some(Viewers::Pair(ViewerKind::Hex))
            && self.hex_settings.mode == HexMode::Strings
    }
}

#[derive(Debug, Serialize, Deserialize)]
// #[serde(bound(serialize = "", deserialize = "T: RawToken"))]
enum AppState {
    Startup(AppStateCtx),
    Idle(AppStateCtx),
    Exit(AppStateCtx),
}

impl AppState {
    fn variant_name(&self) -> &'static str {
        match self {
            Self::Startup(_) => "Startup",
            Self::Idle(_) => "Idle",
            Self::Exit(_) => "Exit",
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        AppState::Startup(AppStateCtx::default())
    }
}

impl AppState {
    fn into_ctx(self) -> AppStateCtx {
        match self {
            AppState::Startup(ctx) | AppState::Idle(ctx) | AppState::Exit(ctx) => ctx,
        }
    }
    fn ctx_mut(&mut self) -> &mut AppStateCtx {
        match self {
            AppState::Startup(ctx) | AppState::Idle(ctx) | AppState::Exit(ctx) => ctx,
        }
    }
}

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

impl<'a> ZApp {
    pub fn report_exit_code_to(&mut self, exit_code: Arc<AtomicU8>) {
        self.exit_code = exit_code;
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

                    let args: Vec<String> = env::args().skip(1).collect();
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
        }
    }

    fn startup(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.style_window.load(ctx);
        log::debug!("pixels_per_point {}", ctx.pixels_per_point());
        log::debug!(
            "native_pixels_per_point {:?}",
            ctx.native_pixels_per_point()
        );
        ctx.set_zoom_factor(self.zoom);
        // ctx.set_debug_on_hover(true);
    }

    fn sync_zoom(&mut self, ctx: &egui::Context) {
        let zoom = clamp_zoom(ctx.zoom_factor());
        if zoom != ctx.zoom_factor() {
            ctx.set_zoom_factor(zoom);
        }
        self.zoom = zoom;
    }

    fn create_tree() -> egui_tiles::Tree<Pane> {
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

    fn open_file_picker(tx: mpsc::Sender<UniversalPath>) {
        std::thread::spawn(move || {
            if let Some(path) = rfd::FileDialog::new().pick_file() {
                if let Err(e) = tx.send(UniversalPath::from(path)) {
                    log::error!("Failed to send path: {e}");
                }
            }
        });
    }

    fn refresh_file_contents(file_1: &mut FileProcessor, file_2: &mut FileProcessor) {
        file_1.invalidate_cache_file();
        file_2.invalidate_cache_file();
    }

    /// Records a written revert for undo and reloads both sides; reloads the target when it was
    /// stale.
    fn finish_revert_write(
        result: Result<RevertRecord, WriteRefusal>,
        path: &Path,
        target: RevertTarget,
        revert_history: &mut RevertHistory,
        diff_processor: &mut DiffProcessor,
        file_1: &mut FileProcessor,
        file_2: &mut FileProcessor,
    ) {
        match result {
            Ok(record) => {
                revert_history.record(record);
                diff_processor.reset_ctx();
                Self::refresh_file_contents(file_1, file_2);
            }
            Err(WriteRefusal::Stale) => {
                log::error!(
                    "Revert refused: {} changed on disk since it was loaded. Reloading it.",
                    path.display()
                );
                diff_processor.reset_ctx();
                match target {
                    RevertTarget::Left => file_1.invalidate_cache_file(),
                    RevertTarget::Right => file_2.invalidate_cache_file(),
                }
            }
            Err(e) => log::error!("Revert refused for {}: {}", path.display(), e),
        }
    }

    /// Undoes or redoes the latest revert in this diff. Both sides reload after a write, and
    /// after a refusal for a file that changed on disk.
    fn step_revert_history(app_ctx: &mut AppStateCtx, step: HistoryStep) {
        // A path picked or set by a Quick Diff since the last frame makes this another diff.
        let (path_1, path_2) = (
            app_ctx.file_1.get_full_path(),
            app_ctx.file_2.get_full_path(),
        );
        app_ctx.revert_history.observe_pair(&path_1, &path_2);
        let (name, since) = match step {
            HistoryStep::Undo => ("Undo", "the revert"),
            HistoryStep::Redo => ("Redo", "the undo"),
        };
        let stepped = app_ctx
            .revert_history
            .step(step, revert::write_history_step(&std::env::temp_dir()));
        let reload = match stepped {
            None => {
                log::info!("{name}: no revert to {}", name.to_lowercase());
                false
            }
            Some((path, Ok(()))) => {
                log::info!("{name} of a revert written to {}", path.display());
                true
            }
            Some((path, Err(WriteRefusal::Stale))) => {
                log::error!(
                    "{name} refused: {} changed on disk since {since}. Reloading it.",
                    path.display()
                );
                true
            }
            Some((path, Err(e))) => {
                log::error!("{name} refused for {}: {}", path.display(), e);
                false
            }
        };
        if reload {
            app_ctx.diff_processor.reset_ctx();
            Self::refresh_file_contents(&mut app_ctx.file_1, &mut app_ctx.file_2);
        }
    }

    fn show_menu(
        &mut self,
        ui: &mut egui::Ui,
        file_1: &mut FileProcessor,
        file_2: &mut FileProcessor,
        diff_processor: &mut DiffProcessor,
        find_open: &mut bool,
        goto_open: &mut bool,
        scroll_left: &mut f32,
        scroll_right: &mut f32,
        keybindings: &mut Keybindings,
        p4_profiles: &mut P4Profiles,
        kinds: &mut SharedKinds,
        ffmpeg: &mut FfmpegSetting,
        myers_diff_algorithm: &mut MyersDiffAlgorithm,
        diff_phases: &mut DiffPhases,
        code_language_override: &mut Option<String>,
        code_language_custom: &mut String,
        detected_languages: [Option<&str>; 2],
        h_scroll_modifier: &mut WheelModifier,
    ) {
        let keymap = &keybindings.keymap;
        ui.horizontal(|ui| {
            egui::MenuBar::new()
                .config(
                    MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
                )
                .ui(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    ui.menu_button("File", |ui| {
                        if menu_item(ui, "Open Source", keymap, FileAction::OpenFileSource)
                            .clicked()
                        {
                            Self::open_file_picker(file_1.get_tx());
                        }
                        if menu_item(ui, "Open Target", keymap, FileAction::OpenFileTarget)
                            .clicked()
                        {
                            Self::open_file_picker(file_2.get_tx());
                        }
                        if ui.button("Open Merge...").clicked() {
                            self.open_merge_form.open = true;
                            ui.close();
                        }
                        if menu_item(ui, "Swap Source/Target", keymap, FileAction::SwapSides)
                            .clicked()
                        {
                            swap_sides(file_1, file_2, scroll_left, scroll_right, diff_processor);
                        }
                        if menu_item(
                            ui,
                            "Diff Shelved File Against Base",
                            keymap,
                            FileAction::DiffShelfBase,
                        )
                        .clicked()
                        {
                            run_diff_against_shelf_base(file_1, file_2);
                        }
                        if menu_item(ui, "Find", keymap, FileAction::Find).clicked() {
                            *find_open = true;
                        }
                        if menu_item(ui, "Goto", keymap, FileAction::Goto).clicked() {
                            *goto_open = true;
                        }
                    });

                    ui.menu_button("Options", |ui| {
                        ui.menu_button("Myers Algo", |ui| {
                            ui.radio_value(
                                myers_diff_algorithm,
                                MyersDiffAlgorithm::Trace,
                                "Trace",
                            );
                            ui.radio_value(
                                myers_diff_algorithm,
                                MyersDiffAlgorithm::Linear,
                                "Linear",
                            );
                            ui.radio_value(
                                myers_diff_algorithm,
                                MyersDiffAlgorithm::LinearMT,
                                "LinearMT",
                            );
                        });
                        diff_phases_menu(ui, diff_phases);
                        ui.menu_button("Code Language", |ui| {
                            let ctx = ui.ctx().clone();
                            let icon_for = |code: &str| {
                                language_icon(code, |emoji| emoji_renders(&ctx, emoji))
                            };
                            let auto_label = match detected_languages {
                                [Some(a), Some(b)] if a != b => format!("Auto ({a} / {b})"),
                                [Some(a), _] | [_, Some(a)] => format!("Auto ({a})"),
                                [None, None] => "Auto (plain text)".to_string(),
                            };
                            if ui
                                .radio(code_language_override.is_none(), auto_label)
                                .clicked()
                            {
                                *code_language_override = None;
                            }
                            ui.separator();
                            for (code, label) in [
                                ("rs", "Rust (.rs)"),
                                ("py", "Python (.py)"),
                                ("cpp", "C++ (.cpp)"),
                                ("js", "JavaScript (.js)"),
                                ("json", "JSON (.json)"),
                                ("xml", "XML (.xml)"),
                                ("md", "Markdown (.md)"),
                            ] {
                                ui.horizontal(|ui| {
                                    show_language_icon(ui, icon_for(code));
                                    if ui
                                        .radio(
                                            code_language_override.as_deref() == Some(code),
                                            label,
                                        )
                                        .clicked()
                                    {
                                        *code_language_override = Some(code.to_string());
                                    }
                                });
                            }
                            ui.separator();
                            ui.horizontal(|ui| {
                                show_language_icon(ui, icon_for(""));
                                if ui
                                    .radio(
                                        code_language_override.as_deref()
                                            == Some(code_language_custom.as_str())
                                            && !code_language_custom.is_empty(),
                                        "Use Custom",
                                    )
                                    .clicked()
                                {
                                    *code_language_override =
                                        Some(code_language_custom.trim().to_string());
                                }
                                let text_edit = egui::TextEdit::singleline(code_language_custom)
                                    .hint_text("Custom (e.g. go, html)");
                                let res = ui.add(text_edit);
                                if res.changed() && !code_language_custom.is_empty() {
                                    *code_language_override =
                                        Some(code_language_custom.trim().to_string());
                                }
                            });
                        });
                        if let Some(code) = code_language_override {
                            *code = code.trim_matches('.').to_string();
                        }
                        ui.menu_button("Horizontal Scroll Wheel", |ui| {
                            ui.radio_value(h_scroll_modifier, WheelModifier::Shift, "Shift+Wheel");
                            ui.radio_value(h_scroll_modifier, WheelModifier::Ctrl, "Ctrl+Wheel");
                        });
                        if menu_item(ui, "P4Config", keymap, FileAction::OpenUniversalPath)
                            .clicked()
                        {
                            self.open_universal_path_window = true;
                        }
                        if menu_item(
                            ui,
                            "Keyboard Shortcuts",
                            keymap,
                            FileAction::OpenKeybindings,
                        )
                        .clicked()
                        {
                            self.open_shortcuts_window = true;
                        }
                        if ui.button("File Viewers").clicked() {
                            self.open_viewers_window = true;
                            ui.close();
                        }
                        if ui.button("Style").clicked() {
                            self.style_window.open(ui.ctx());
                            ui.close();
                        }
                    });

                    ui.menu_button("Debug", |ui| {
                        if ui.button("Clear File Paths").clicked() {
                            file_1.set_path(UniversalPath::default());
                            file_2.set_path(UniversalPath::default());
                            diff_processor.reset_ctx();
                            ui.close();
                        }
                        if menu_item(ui, "Clear Cached Files", keymap, FileAction::RefreshDiff)
                            .clicked()
                        {
                            diff_processor.reset_ctx();
                            Self::refresh_file_contents(file_1, file_2);
                        }
                        if menu_item(
                            ui,
                            "Clear Diff Rows",
                            keymap,
                            FileAction::RefreshDiffRowsOnly,
                        )
                        .clicked()
                        {
                            diff_processor.reset_ctx();
                        }
                        if ui.button("Phase Trace").clicked() {
                            self.open_phase_trace_window = true;
                            ui.close();
                        }
                        ui.checkbox(&mut self.phase_overlay, "Phase Overlay");
                        #[cfg(debug_assertions)]
                        {
                            let load_btn = |ui: &mut egui::Ui,
                                            label: &str,
                                            file_1: &mut FileProcessor,
                                            file_2: &mut FileProcessor,
                                            diff_processor: &mut DiffProcessor,
                                            p1: &str,
                                            p2: &str| {
                                if ui.button(label).clicked() {
                                    ui.close();
                                    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
                                    let (p1, p2) = (base.join(p1), base.join(p2));
                                    if let Some(missing) =
                                        [&p1, &p2].into_iter().find(|p| !p.exists())
                                    {
                                        log::warn!(
                                            "{label}: missing fixture {}",
                                            missing.display()
                                        );
                                        return;
                                    }

                                    file_1.set_root("".into());
                                    file_2.set_root("".into());
                                    file_1.set_path(UniversalPath::from(p1));
                                    file_2.set_path(UniversalPath::from(p2));

                                    diff_processor.reset_ctx();
                                }
                            };

                            load_btn(
                                ui,
                                "Load $A",
                                file_1,
                                file_2,
                                diff_processor,
                                "../../test/rust_files_diff_1/advanced_rust.rs",
                                "../../test/rust_files_diff_1/advanced_rust_2.rs",
                            );

                            load_btn(
                                ui,
                                "Load $B",
                                file_1,
                                file_2,
                                diff_processor,
                                "../../test/rust_files_diff_1/imgui.1.91.1.h",
                                "../../test/rust_files_diff_1/imgui.h",
                            );

                            load_btn(
                                ui,
                                "Load $C",
                                file_1,
                                file_2,
                                diff_processor,
                                "../../test/test_ignore_whitespace_simple/1.txt",
                                "../../test/test_ignore_whitespace_simple/2.txt",
                            );

                            load_btn(
                                ui,
                                "Load $D",
                                file_1,
                                file_2,
                                diff_processor,
                                "../../test/test_ignore_whitespace_extreme_simple/1.txt",
                                "../../test/test_ignore_whitespace_extreme_simple/2.txt",
                            );

                            ui.separator();

                            load_btn(
                                ui,
                                "Open Text Logs",
                                file_1,
                                file_2,
                                diff_processor,
                                "../../test/test_log/1.txt",
                                "../../test/test_log/2.txt",
                            );

                            load_btn(
                                ui,
                                "Open Hex Binaries",
                                file_1,
                                file_2,
                                diff_processor,
                                "../../test/1/2/2_battery-crates_all_A1-Mini - Copy (4).3mf",
                                "../../test/test_similar_2/1/2/AAA.3mf",
                            );

                            load_btn(
                                ui,
                                "Open PNG Pair",
                                file_1,
                                file_2,
                                diff_processor,
                                "../../test/test_similar_1/different_hash.png",
                                "../../test/test_similar_2/different_hash.png",
                            );

                            load_btn(
                                ui,
                                "Open PNG vs Text",
                                file_1,
                                file_2,
                                diff_processor,
                                "../../test/1/admin_icon.png",
                                "../../test/test_log/1.txt",
                            );
                        }
                    });
                });
        });

        if self.open_shortcuts_window {
            show_custom_popup(
                ui.ctx(),
                &mut self.open_shortcuts_window,
                "Option - Shortcuts",
                true,
                |ui| {
                    ui_keybindings(
                        ui,
                        keybindings,
                        &mut self.keymap_editor,
                        &p4_profiles.profiles,
                    );
                },
            );
        }
        if !self.open_shortcuts_window {
            self.keymap_editor.cancel_capture();
        }
        if self.open_viewers_window {
            show_custom_popup(
                ui.ctx(),
                &mut self.open_viewers_window,
                "Option - File Viewers",
                true,
                |ui| {
                    ui_ffmpeg_setting(ui, ffmpeg);
                    ui.separator();
                    let edits = ui_extension_map(ui, &kinds.table);
                    kinds.apply(edits);
                },
            );
        }
        self.style_window.show::<FileRole>(ui.ctx(), "File diff");
        if self.open_help_window {
            show_custom_popup(
                ui.ctx(),
                &mut self.open_help_window,
                "Command line",
                true,
                |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(cli::HELP).monospace()));
                },
            );
        }
        if self.open_phase_trace_window {
            let trace = diff_processor
                .get_minimal_diff_ctx()
                .and_then(|ctx| ctx.phase_trace);
            show_custom_popup(
                ui.ctx(),
                &mut self.open_phase_trace_window,
                "Debug - Phase Trace",
                false,
                |ui| ui_phase_trace(ui, trace.as_deref()),
            );
        }
        if self.open_universal_path_window {
            show_custom_popup(
                ui.ctx(),
                &mut self.open_universal_path_window,
                "Option - P4Config",
                true,
                |ui| {
                    let before = p4_profiles.clone();
                    ui_p4_profiles(ui, p4_profiles);
                    if *p4_profiles != before {
                        // Not the profiles themselves: they hold passwords.
                        log::info!("P4 profiles changed");
                        update_p4_profiles(p4_profiles.clone());
                    }
                },
            );
        }
    }

    fn ui(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame, app_ctx: &mut AppStateCtx) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let AppStateCtx {
                scroll_left,
                scroll_right,
                h_scroll_linked,
                h_scroll_modifier,
                word_wrap,
                text_mode,
                diff_options,
                file_1,
                file_2,
                goto_open,
                find_open,
                goto_input,
                find_query,
                find_error,
                keybindings,
                myers_diff_algorithm,
                diff_processor,
                hex_processor,
                hex_settings,
                hex_strings,
                strings_active: _,
                image_processor,
                image_settings,
                audio_processor,
                audio_settings,
                audio_meta,
                video_frames,
                video_meta,
                viewers,
                viewer_override,
                viewer_fallback,
                kinds,
                ffmpeg,
                legacy_extension_map: _,
                revert_history,
                toggle_all,
                code_language_override,
                code_language_custom,
                detected_languages,
                p4_profiles,
                regex_ignore_presets,
            } = app_ctx;
            let detected = [
                detected_languages[0].get(file_1.get_cached_file().as_deref()),
                detected_languages[1].get(file_2.get_cached_file().as_deref()),
            ];
            let pair_kind = viewers.and_then(Viewers::pair);
            let sides = match *viewers {
                Some(Viewers::Sides(sides)) => Some(sides),
                _ => None,
            };
            let side_shows = |kind| sides.is_some_and(|s| s.contains(&kind));
            let is_hex = pair_kind == Some(ViewerKind::Hex);
            let strings_view = is_hex && hex_settings.mode == HexMode::Strings;
            let hex_bytes = is_hex && !strings_view;
            let goto_hex =
                hex_bytes || (side_shows(ViewerKind::Hex) && !side_shows(ViewerKind::Text));
            let is_image = pair_kind == Some(ViewerKind::Image);
            let media_view =
                pair_kind.filter(|k| matches!(k, ViewerKind::Audio | ViewerKind::Video));
            let video_audio =
                media_view == Some(ViewerKind::Video) && video_meta.tab == VideoTab::Audio;
            self.show_menu(
                ui,
                file_1,
                file_2,
                diff_processor,
                find_open,
                goto_open,
                scroll_left,
                scroll_right,
                keybindings,
                p4_profiles,
                kinds,
                ffmpeg,
                myers_diff_algorithm,
                &mut diff_options.phases,
                code_language_override,
                code_language_custom,
                detected,
                h_scroll_modifier,
            );
            let code_language = match code_language_override.as_deref() {
                Some(code) => [code; 2],
                None => detected.map(|language| language.unwrap_or("")),
            };

            ui.separator();

            let mut goto_window_open = *goto_open;
            show_custom_popup(ctx, &mut goto_window_open, "Goto", true, |ui| {
                // Hex gotos a byte offset, in decimal or 0x hex.
                if goto_hex {
                    goto_input.retain(|c| c.is_ascii_hexdigit() || c == 'x' || c == 'X');
                } else {
                    goto_input.retain(|c| c.is_ascii_digit());
                }
                let response = ui.add(
                    egui::TextEdit::singleline(goto_input)
                        .desired_width(40.0)
                        .hint_text(if goto_hex { "offset" } else { "#" }),
                );
                response.request_focus();
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    if goto_hex {
                        match parse_hex_offset(goto_input, hex_processor.max_len()) {
                            Ok(offset) => {
                                hex_processor.goto(offset);
                                *goto_open = false;
                            }
                            Err(e) => log::error!("Goto offset {:?}: {}", goto_input, e),
                        }
                    } else if let Ok(line_number) = goto_input.parse::<usize>() {
                        diff_processor.update_goto(Some(line_number));
                        *goto_open = false;
                    }
                }
            });
            if !goto_window_open {
                goto_input.clear();
                *goto_open = goto_window_open;
            }
            if *find_open {
                let event = find_popup(ctx, find_query, find_error.as_deref(), None, |_| false);
                match event {
                    Some(FindPopupEvent::Changed | FindPopupEvent::Submitted) => {
                        *find_error = diff_processor
                            .update_find_query(find_query)
                            .err()
                            .map(|e| e.to_string());
                        if event == Some(FindPopupEvent::Submitted) && find_error.is_none() {
                            *find_open = false;
                        }
                    }
                    Some(FindPopupEvent::Closed) => *find_open = false,
                    None => {}
                }
            }

            // The text diff's state is stale while another viewer shows the pair.
            let text_scroll = side_shows(ViewerKind::Text)
                .then(|| diff_processor.get_scroll_to_row())
                .flatten();
            let hex_scroll = side_shows(ViewerKind::Hex)
                .then(|| hex_processor.scroll_to_row(0))
                .flatten();
            let scroll_to_rows = &if sides.is_some() {
                None
            } else if hex_bytes {
                hex_processor.scroll_to_row(diff_processor.conflict_cursor.get())
            } else if is_image || media_view.is_some() {
                None
            } else {
                diff_processor.get_scroll_to_row()
            };
            if let Some(scroll_to) = scroll_to_rows {
                log::info!("Navigating to line: {:?}", scroll_to);
            }

            let active_highlights = diff_processor.active_highlights.clone();
            let mut conflict_cursor = diff_processor.conflict_cursor.clone();
            let pins = diff_processor.pins().to_vec();
            let mut pins_request = None;
            let mut find_cursor = diff_processor.find_cursor.clone();
            let highlight_side = diff_processor.highlight_side;
            let find_matcher = diff_processor.find_ctx.matcher().cloned();
            let find_hit = diff_processor.current_find_hit();
            let find_count = diff_processor.find_ctx.hits().len();
            let mut active_side = diff_processor.active_side;

            if file_1.get_full_path() != UniversalPath::default()
                || file_2.get_full_path() != UniversalPath::default()
            {
                self.launch_error = None;
            }
            // Rows of strings from before a changed minimum length would show without offsets.
            let strings_pending = strings_view && hex_strings.sides().is_none();
            let diff_ctx = if hex_bytes || strings_pending || is_image || media_view.is_some() {
                None
            } else {
                diff_processor.get_minimal_diff_ctx()
            };
            let (diff_ctx, side_text) = match sides {
                Some(_) => (None, diff_ctx.filter(|_| side_shows(ViewerKind::Text))),
                None => (diff_ctx, None),
            };
            let string_offsets = match hex_strings.sides().filter(|_| strings_view) {
                Some(sides) => sides
                    .each_ref()
                    .map(|s| s.as_ref().map_or(&[][..], |s| &s.offsets[..])),
                None => [&[][..]; 2],
            };
            let mut block_toggle_request = None;
            let mut toggle_all_request = false;
            let mut behavior = TreeBehavior {
                ctx_file_diff: FileDiffPaneCtx {
                    diff_ctx: diff_ctx.as_ref(),
                    scroll_left: scroll_left,
                    scroll_right: scroll_right,
                    h_scroll_linked,
                    word_wrap,
                    text_mode,
                    diff_options: diff_options,
                    regex_ignore_presets,
                    scroll_to_row_span: &scroll_to_rows,
                    load_file_1_request: &mut None,
                    load_file_2_request: &mut None,
                    set_file_1_root_request: &mut None,
                    set_file_2_root_request: &mut None,
                    file_source_path: file_1.get_full_path(),
                    file_target_path: file_2.get_full_path(),
                    file_source_label: file_1.label().map(str::to_owned),
                    file_target_label: file_2.label().map(str::to_owned),
                    launch_error: self.launch_error.as_deref(),
                    file_source_root: file_1.get_root(),
                    file_target_root: file_2.get_root(),
                    file_source_root_valid: file_1.get_loading_path().is_some()
                        || FileProcessor::is_root_valid(
                            &file_1.get_root().unwrap_or_default(),
                            &&file_1.get_full_path(),
                        ),
                    file_target_root_valid: file_2.get_loading_path().is_some()
                        || FileProcessor::is_root_valid(
                            &file_1.get_root().unwrap_or_default(),
                            &file_1.get_full_path(),
                        ),
                    file_source_path_valid: file_1.get_loading_path().is_some()
                        || file_1.get_loaded_file().is_some(),
                    file_target_path_valid: file_2.get_loading_path().is_some()
                        || file_2.get_loaded_file().is_some(),
                    file_source_load_error: file_1.load_error().map(str::to_owned),
                    file_source_loading: file_1.get_loading_path().is_some(),
                    file_target_load_error: file_2.load_error().map(str::to_owned),
                    file_target_loading: file_2.get_loading_path().is_some(),
                    active_highlights: &active_highlights,
                    highlight_side,
                    conflict_cursor: &mut conflict_cursor,
                    pins: &pins,
                    pins_request: &mut pins_request,
                    find_cursor: &mut find_cursor,
                    find_matcher: find_matcher.as_ref(),
                    find_hit,
                    find_count,
                    active_side: &mut active_side,
                    toggle_active_side: std::mem::take(&mut self.toggle_active_side),
                    diff_loading: diff_processor.is_in_progress() || strings_pending,
                    code_language,
                    revert_request: &mut None,
                    block_toggle_request: &mut block_toggle_request,
                    toggle_all_held: toggle_all.is_held(),
                    toggle_all_request: &mut toggle_all_request,
                    hex_view: hex_bytes.then(|| hex_processor.view_ctx()),
                    hex_mode: is_hex.then_some(&mut *hex_settings),
                    string_offsets,
                    image_view: (is_image || side_shows(ViewerKind::Image))
                        .then_some(image_processor),
                    image_settings: Some(image_settings),
                    audio_view: (media_view == Some(ViewerKind::Audio)
                        || video_audio
                        || side_shows(ViewerKind::Audio))
                    .then_some((audio_processor, audio_settings)),
                    media_view,
                    audio_meta: (media_view == Some(ViewerKind::Audio)).then_some(audio_meta),
                    video_frames: (media_view == Some(ViewerKind::Video)
                        || side_shows(ViewerKind::Video))
                    .then_some(video_frames),
                    video_meta: (media_view == Some(ViewerKind::Video)).then_some(video_meta),
                    side_viewers: None,
                    viewer_override: &mut viewer_override.kind,
                    viewer_fallback: viewer_fallback.as_deref(),
                    phase_overlay: self.phase_overlay,
                    keymap: &keybindings.keymap,
                },
            };
            if let Some(kinds) = sides {
                let file_diff = &mut behavior.ctx_file_diff;
                file_diff.side_viewers = Some(SideViewers {
                    kinds,
                    text: side_text.as_ref(),
                    text_scroll,
                    hex: side_shows(ViewerKind::Hex).then(|| hex_processor.view_ctx()),
                    hex_scroll,
                    image: file_diff.image_view.take(),
                    image_settings: file_diff.image_settings.take(),
                    audio: file_diff.audio_view.take(),
                    video: file_diff.video_frames.take(),
                });
            }

            if let Some(request) = self.open_merge_form.ui(ctx) {
                self.merge = Some(MergeView::new(request, *myers_diff_algorithm));
            }
            let close_requested = ctx.input(|i| i.viewport().close_requested());
            let mut close_merge = None;
            if close_requested && let Some(merge) = &mut self.merge {
                close_merge = merge.request_close(Closing::App);
                if close_merge.is_none() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
            }
            ui.with_layout(
                Layout::left_to_right(egui::Align::Min),
                |ui| match &mut self.merge {
                    Some(merge) => {
                        let closed = merge.ui(ui, code_language_override.as_deref());
                        close_merge = close_merge.or(closed);
                    }
                    None => self.tree.ui(&mut behavior, ui),
                },
            );
            if let Some(closing) = close_merge {
                let merge = self.merge.take().expect("only an open merge closes");
                self.exit_code.store(merge.exit_code(), Ordering::Relaxed);
                // The prompt's close request was cancelled frames ago, so it is made again.
                if closing == Closing::App && !close_requested {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }

            // TODO: Remove clones
            let clone_find = behavior.ctx_file_diff.find_cursor.clone();
            let clone_active_side = *behavior.ctx_file_diff.active_side;
            let clone_conflict = behavior.ctx_file_diff.conflict_cursor.clone();

            if let Some(file_path) = behavior.ctx_file_diff.set_file_1_root_request.take() {
                log::debug!("Set new root from root_request_1: {}", file_path);
                app_ctx.file_1.set_root(file_path);
            }
            if let Some(file_path) = behavior.ctx_file_diff.set_file_2_root_request.take() {
                log::debug!("Set new root from root_request_2: {}", file_path);
                app_ctx.file_2.set_root(file_path);
            }

            if let Some(file_path) = behavior.ctx_file_diff.load_file_1_request.take() {
                log::debug!("Set new path from file_1_request: {}", file_path);
                app_ctx.file_1.set_path(file_path);
            }
            if let Some(file_path) = behavior.ctx_file_diff.load_file_2_request.take() {
                log::debug!("Set new path from file_2_request: {}", file_path);
                app_ctx.file_2.set_path(file_path);
            }
            if let Some(revert_request) = *behavior.ctx_file_diff.revert_request {
                log::debug!(
                    "new revert request: hunk {} into {:?}",
                    revert_request.hunk,
                    revert_request.target
                );
                // Plan against the files the shown rows were built from, not the latest loads.
                let planned = behavior
                    .ctx_file_diff
                    .diff_ctx
                    .ok_or(RevertRefusal::NoSuchHunk)
                    .and_then(|diff_ctx| {
                        revert::plan_hunk_revert(diff_ctx, revert_request, &std::env::temp_dir())
                    });
                match planned {
                    Ok(planned) => {
                        let path = planned.path.clone();
                        let written = revert::write_revert(
                            planned,
                            &std::env::temp_dir(),
                            &P4Command::new(false).for_file(UniversalPath::Local(path.clone())),
                        );
                        let result = match written {
                            Ok(RevertWrite::NeedsP4Edit(pending)) => {
                                log::info!(
                                    "{} is read-only and Perforce-managed, asking to p4 edit it",
                                    path.display()
                                );
                                self.pending_p4_edit = Some((revert_request.target, pending));
                                None
                            }
                            Ok(RevertWrite::Written(record)) => Some(Ok(record)),
                            Err(e) => Some(Err(e)),
                        };
                        if let Some(result) = result {
                            Self::finish_revert_write(
                                result,
                                &path,
                                revert_request.target,
                                revert_history,
                                diff_processor,
                                &mut app_ctx.file_1,
                                &mut app_ctx.file_2,
                            );
                        }
                    }
                    Err(refusal) => {
                        log::error!(
                            "Revert refused: {} (hunk {} into {:?})",
                            refusal,
                            revert_request.hunk,
                            revert_request.target
                        );
                    }
                }
            }

            if let Some((target, pending)) = self.pending_p4_edit.take() {
                let mut confirmed = None;
                let modal = egui::Modal::new(egui::Id::new("p4_edit_prompt")).show(ctx, |ui| {
                    ui.heading("Check out for edit?");
                    ui.label(format!(
                        "{} is read-only and managed by Perforce.",
                        pending.path().display()
                    ));
                    ui.label("Run p4 edit on it and apply the revert?");
                    ui.horizontal(|ui| {
                        if ui.button("p4 edit and revert").clicked() {
                            confirmed = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            confirmed = Some(false);
                        }
                    });
                });
                // Escape or a click outside the prompt declines.
                if modal.should_close() {
                    confirmed.get_or_insert(false);
                }
                match confirmed {
                    Some(true) => {
                        let path = pending.path().to_path_buf();
                        let p4 = P4Command::new(false).for_file(UniversalPath::Local(path.clone()));
                        let result = pending.confirm(&std::env::temp_dir(), &p4);
                        Self::finish_revert_write(
                            result,
                            &path,
                            target,
                            revert_history,
                            diff_processor,
                            &mut app_ctx.file_1,
                            &mut app_ctx.file_2,
                        );
                    }
                    Some(false) => {
                        log::info!(
                            "Revert cancelled: p4 edit declined for {}",
                            pending.path().display()
                        );
                    }
                    None => self.pending_p4_edit = Some((target, pending)),
                }
            }

            drop(behavior);
            if let Some(pins) = pins_request {
                diff_processor.set_pins(pins);
            }
            diff_processor.find_cursor = clone_find;
            diff_processor.active_side = clone_active_side;
            diff_processor.conflict_cursor = clone_conflict;
            if let Some((key, toggle)) = block_toggle_request {
                diff_processor.toggle_block(key, toggle);
                ctx.request_repaint();
            }
            if toggle_all_request {
                toggle_all_options(
                    toggle_all,
                    &mut app_ctx.diff_options,
                    &mut app_ctx.h_scroll_linked,
                    &mut app_ctx.word_wrap,
                );
            }

            for (_tile_id, tile) in self.tree.tiles.iter() {
                if let Tile::Pane(Pane::FileDiff(..)) = tile {
                    let source = app_ctx.file_1.display_name();
                    let target = app_ctx.file_2.display_name();
                    let total_adds = app_ctx
                        .diff_processor
                        .get_minimal_diff_ctx()
                        .as_ref()
                        .and_then(|f| Some(f.num_add_deletes))
                        .unwrap_or_default()
                        .0;
                    let total_deletes = app_ctx
                        .diff_processor
                        .get_minimal_diff_ctx()
                        .and_then(|f| Some(f.num_add_deletes))
                        .unwrap_or_default()
                        .1;
                    let unit = app_ctx
                        .diff_processor
                        .get_minimal_diff_ctx()
                        .and_then(|f| f.change_unit)
                        .map_or(String::new(), |unit| format!(" {}", unit.label()));
                    let counts = if sides.is_some() {
                        "types differ".to_string()
                    } else if is_hex {
                        "hex".to_string()
                    } else if is_image {
                        "image".to_string()
                    } else if let Some(kind) = media_view {
                        kind.name().to_lowercase()
                    } else {
                        format!("+{}/-{}{}", total_adds, total_deletes, unit)
                    };
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                        "zdiff [{}] - {}, {}",
                        counts, source, target
                    )));
                    break;
                }
            }
        });
    }

    fn request_shutdown(&mut self) {
        if let Some(state) = self.state.take() {
            let ctx = state.into_ctx();
            self.state = Some(AppState::Exit(ctx));
        }
    }

    fn process_ctx_inputs(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // The editor takes the captured key press only later in this frame, when it is drawn.
        let capturing = self.keymap_editor.is_capturing();
        let app_state_ctx = self
            .state
            .as_mut()
            .expect("State was not valid while processing inputs")
            .ctx_mut();
        let user_quit: bool = false;
        // Read outside ctx.input: Context methods inside its closure can deadlock.
        let text_focused = ctx.wants_keyboard_input();
        let mut history_step = None;
        let mut diff_local_file = false;
        let mut diff_shelf_base = false;
        let mut tab_pressed = false;
        let mut esc_pressed = false;
        let mut save_merge = false;
        {
            let _input_ctx = ctx.input(|r| {
                if capturing {
                    return;
                }
                tab_pressed = r.key_pressed(egui::Key::Tab);
                esc_pressed = r.key_pressed(egui::Key::Escape);

                // DoubleLeftClick
                if r.pointer.button_double_clicked(PointerButton::Primary) {
                    let mouse_pos = r.pointer.interact_pos().unwrap();
                    log::info!("double click @({},{})", mouse_pos.x, mouse_pos.y);
                }
                if r.pointer.button_clicked(PointerButton::Middle) {
                    let mouse_pos: Pos2 = r.pointer.interact_pos().unwrap();
                    log::info!("middle click @({},{})", mouse_pos.x, mouse_pos.y);
                }

                let actions = app_state_ctx
                    .keybindings
                    .keymap
                    .dispatch(&r.events, text_focused);
                for action in actions {
                    log::info!("Action triggered: {}", action.label());
                    match action {
                        FileAction::OpenFileSource => {
                            Self::open_file_picker(app_state_ctx.file_1.get_tx())
                        }
                        FileAction::OpenFileTarget => {
                            Self::open_file_picker(app_state_ctx.file_2.get_tx())
                        }
                        FileAction::RefreshDiff => {
                            app_state_ctx.diff_processor.reset_ctx();
                            Self::refresh_file_contents(
                                &mut app_state_ctx.file_1,
                                &mut app_state_ctx.file_2,
                            );
                        }
                        FileAction::RefreshDiffRowsOnly => {
                            app_state_ctx.diff_processor.reset_ctx();
                        }
                        FileAction::OpenKeybindings => self.open_shortcuts_window = true,
                        FileAction::OpenUniversalPath => self.open_universal_path_window = true,
                        FileAction::Find => app_state_ctx.find_open = true,
                        FileAction::Goto => app_state_ctx.goto_open = true,
                        FileAction::SaveMerge => save_merge = true,
                        FileAction::NextFrame | FileAction::PrevFrame => {
                            if app_state_ctx.video_meta.tab == VideoTab::Frames {
                                app_state_ctx
                                    .video_frames
                                    .step(action == FileAction::NextFrame);
                            }
                        }
                        FileAction::NextConflict | FileAction::PrevConflict
                            if let Some(merge) = &mut self.merge =>
                        {
                            merge.navigate(action == FileAction::NextConflict);
                        }
                        FileAction::NextConflict => {
                            app_state_ctx.diff_processor.conflict_cursor.inc();
                            log::info!(
                                "ConflictCursor++ @{}",
                                app_state_ctx.diff_processor.conflict_cursor.get()
                            );
                        }
                        FileAction::PrevConflict => {
                            app_state_ctx.diff_processor.conflict_cursor.dec();
                            log::info!(
                                "ConflictCursor-- @{}",
                                app_state_ctx.diff_processor.conflict_cursor.get()
                            );
                        }
                        // The popup takes Enter; stepping here would move the kept hits under an error.
                        FileAction::NextFind if !app_state_ctx.find_open => {
                            app_state_ctx.diff_processor.find_cursor.inc();
                            log::info!(
                                "FindCursor++ @{}",
                                app_state_ctx.diff_processor.find_cursor.get()
                            );
                        }
                        FileAction::PrevFind if !app_state_ctx.find_open => {
                            app_state_ctx.diff_processor.find_cursor.dec();
                            log::info!(
                                "FindCursor-- @{}",
                                app_state_ctx.diff_processor.find_cursor.get()
                            );
                        }
                        FileAction::NextFind | FileAction::PrevFind => {}
                        FileAction::RevisionGraph => match &app_state_ctx.file_1.get_full_path() {
                            file @ UniversalPath::Depot(..) => {
                                match P4Command::open_revision_graph(file) {
                                    Ok(_) => {
                                        log::info!("Revision graph returned Ok");
                                    }
                                    Err(e) => log::error!("Failed to open revision graph: {e}"),
                                }
                            }
                            UniversalPath::Local(path_buf) | UniversalPath::Git(path_buf, _) => {
                                log::info!(
                                    "Can not open revision graph for local path {}",
                                    path_buf.display()
                                );
                            }
                        },
                        FileAction::TimelapseView => match &app_state_ctx.file_1.get_full_path() {
                            file @ UniversalPath::Depot(..) => {
                                match P4Command::open_timelapse_view(file) {
                                    Ok(_) => {
                                        log::info!("Timelapse view returned Ok");
                                    }
                                    Err(e) => log::error!("Failed to open timelapse view: {e}"),
                                }
                            }
                            UniversalPath::Local(path_buf) | UniversalPath::Git(path_buf, _) => {
                                log::info!(
                                    "Can not open timelapse view for local path {}",
                                    path_buf.display()
                                );
                            }
                        },
                        FileAction::UndoRevert => history_step = Some(HistoryStep::Undo),
                        FileAction::Redo => history_step = Some(HistoryStep::Redo),
                        FileAction::DiffLocalFile => diff_local_file = true,
                        FileAction::DiffShelfBase => diff_shelf_base = true,
                        FileAction::SwapSides => swap_sides(
                            &mut app_state_ctx.file_1,
                            &mut app_state_ctx.file_2,
                            &mut app_state_ctx.scroll_left,
                            &mut app_state_ctx.scroll_right,
                            &mut app_state_ctx.diff_processor,
                        ),
                        FileAction::IgnoreWhitespace
                        | FileAction::IgnoreComments
                        | FileAction::IgnoreRegex
                        | FileAction::HighlightRows
                        | FileAction::GhostRows
                        | FileAction::LinkHScroll
                        | FileAction::WordWrap
                        | FileAction::DiffOnly
                        | FileAction::FewerContextRows
                        | FileAction::MoreContextRows => toggle_option(app_state_ctx, action),
                        FileAction::ToggleAllOptions => toggle_all_options(
                            &mut app_state_ctx.toggle_all,
                            &mut app_state_ctx.diff_options,
                            &mut app_state_ctx.h_scroll_linked,
                            &mut app_state_ctx.word_wrap,
                        ),
                    }
                }
                let current = OptionSet::read(
                    &app_state_ctx.diff_options,
                    app_state_ctx.h_scroll_linked,
                    app_state_ctx.word_wrap,
                );
                app_state_ctx.toggle_all.observe(current);

                let handle_kb = |opt: &Option<Shortcut>, func: &mut dyn FnMut(Shortcut)| {
                    if let Some(kb) = opt {
                        if kb.matches(r) {
                            log::info!("Shortcut triggered: {}", kb.format());
                            func(*kb);
                        }
                    }
                };

                for (i, (kb, path)) in app_state_ctx
                    .keybindings
                    .user_quick_diffs
                    .iter()
                    .enumerate()
                {
                    handle_kb(kb, &mut |kb| {
                        log::info!(
                            "User Quick Diff Shortcut [{}] triggered: {}",
                            i + 1,
                            kb.format()
                        );

                        if let Err(refusal) = apply_quick_diff(
                            path,
                            &mut app_state_ctx.file_1,
                            &mut app_state_ctx.file_2,
                            &std::env::temp_dir(),
                        ) {
                            log::warn!(
                                "User Quick Diff Shortcut [{}] disabled: {}:\n{}\n{}",
                                i + 1,
                                refusal,
                                app_state_ctx.file_1.get_full_path(),
                                app_state_ctx.file_2.get_full_path()
                            );
                        } else {
                            // Before the sides load: their fetches read it on the loader thread.
                            set_p4_slot_profile(path.p4_profile);
                            if path.source.is_some() {
                                log::info!(
                                    "User Quick Diff Shortcut set paths:\nSource: {}\nTarget: {}",
                                    app_state_ctx.file_1.get_full_path(),
                                    app_state_ctx.file_2.get_full_path()
                                );
                            } else {
                                log::info!(
                                    "User Quick Diff Shortcut set paths:\nTarget: {}",
                                    app_state_ctx.file_2.get_full_path()
                                );
                            }
                        }
                    });
                }
            });
        }
        if tab_pressed && !text_focused {
            // begin_pass already made Tab a focus move; cancel it before any widget takes it.
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
            self.toggle_active_side = true;
        }
        if esc_pressed
            && !app_state_ctx.find_open
            && !app_state_ctx.goto_open
            && self.pending_p4_edit.is_none()
        {
            app_state_ctx.diff_processor.clear_results();
            app_state_ctx.hex_processor.clear_highlight();
        }
        // After the input closure: this writes to disk.
        if save_merge {
            match &mut self.merge {
                Some(merge) => merge.request_save(),
                None => log::info!("Save Merge: no merge is open"),
            }
        }
        if let Some(step) = history_step {
            Self::step_revert_history(app_state_ctx, step);
        }
        // After the input closure too: p4 can take a while.
        if diff_local_file {
            let p4 = P4Command::new(false).for_file(app_state_ctx.file_1.get_full_path());
            match diff_against_local_file(&mut app_state_ctx.file_1, &mut app_state_ctx.file_2, &p4)
            {
                Ok(local) => log::info!("Diffing against the local file {}", local.display()),
                Err(refusal @ LocalFileRefusal::LocalSource(_)) => {
                    log::info!("Can not diff against the local file: {refusal}")
                }
                Err(refusal) => log::error!("Can not diff against the local file: {refusal}"),
            }
        }
        if diff_shelf_base {
            run_diff_against_shelf_base(&mut app_state_ctx.file_1, &mut app_state_ctx.file_2);
        }

        if user_quit {
            self.request_shutdown();
        }
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
        #[cfg(feature = "serde")]
        if let Ok(json) = serde_json::to_string(self) {
            storage.set_string(eframe::APP_KEY, json);
            log::debug!("App state saved");
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
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

        let current_state = self
            .state
            .take()
            .expect("State should always be valid during update");

        let next_state = match current_state {
            AppState::Startup(state_ctx) => {
                self.startup(ctx, frame);
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.centered_and_justified(|ui| ui.label("Loading..."));
                });
                AppState::Idle(state_ctx)
            }

            AppState::Idle(mut state) => {
                self.sync_zoom(ctx);

                let loaded_1 = state.file_1.get_loaded_file();
                let loaded_2 = state.file_2.get_loaded_file();
                let path_1 = state.file_1.get_full_path();
                let path_2 = state.file_2.get_full_path();
                state.viewer_override.observe_pair(&path_1, &path_2);
                state.revert_history.observe_pair(&path_1, &path_2);
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
                // A video pair decodes its audio only while the Audio tab is open.
                let viewers = resolution.as_ref().map(|r| r.viewers);
                let video_audio = viewers == Some(Viewers::Pair(ViewerKind::Video))
                    && state.video_meta.tab == VideoTab::Audio;
                let [audio_1, audio_2] = if video_audio {
                    [loaded_1.clone(), loaded_2.clone()]
                } else {
                    files_for(&resolution, ViewerKind::Audio)
                };
                if (audio_1.is_some() || audio_2.is_some())
                    && let Ok(ffmpeg) = state.ffmpeg.lookup()
                {
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
                if state.hex_processor.request(hex_1, hex_2)
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
                let strings_files = state
                    .hex_strings
                    .sides()
                    .map(|sides| sides.each_ref().map(|s| s.as_ref().map(|s| s.file.clone())));

                let is_text = if strings_view {
                    strings_files.is_some()
                } else {
                    state.viewers.is_none_or(|v| v.shows(ViewerKind::Text))
                };
                let cached = [
                    state.file_1.get_cached_file().clone(),
                    state.file_2.get_cached_file().clone(),
                ];
                // A mismatched pair's text side is diffed alone, so its rows are all one side's.
                // Pins pair lines of both sides, and the cursor may hold another viewer's stop.
                // The strings of a hex pair are other lines than the pins' files.
                let ([text_1, text_2], pins) = match (&strings_files, state.viewers) {
                    (Some(files), _) => (files.clone(), Vec::new()),
                    (None, Some(viewers @ Viewers::Sides(_))) => {
                        state.diff_processor.conflict_cursor.set(0);
                        (viewers.files_for(ViewerKind::Text, cached), Vec::new())
                    }
                    _ => (cached, state.diff_processor.pins().to_vec()),
                };

                let update_input = UpdateDiffRowsInput {
                    file_1: text_1,
                    file_2: text_2,
                    options: state.diff_options.clone(),
                    myers_diff_algorithm: state.myers_diff_algorithm.clone(),
                    pins,
                    collect_trace: self.open_phase_trace_window || self.phase_overlay,
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

#[derive(Debug, PartialEq, Eq)]
enum LaunchOutcome {
    Opened,
    Merge(Box<MergeRequest>, MergeNames),
    Help,
    Error(String),
}

fn apply_launch(
    file_1: &mut FileProcessor,
    file_2: &mut FileProcessor,
    launch: Result<cli::Launch, cli::LaunchError>,
) -> LaunchOutcome {
    let launch = match launch {
        Ok(launch) => launch,
        Err(err) => {
            log::error!("{err}");
            return LaunchOutcome::Error(err.to_string());
        }
    };
    for warning in &launch.warnings {
        log::warn!("{warning}");
    }
    let (left, right) = match launch.request {
        LaunchRequest::Diff { left, right } => (left, right),
        LaunchRequest::Merge {
            base,
            left,
            right,
            output,
        } => {
            if let Err(err) = merge_pane::check_output(&output.path) {
                log::error!("{err}");
                return LaunchOutcome::Error(err);
            }
            let request = MergeRequest {
                base: Some(base.path),
                theirs: left.path,
                yours: right.path,
                output: output.path,
            };
            let names = MergeNames {
                base: base.label,
                theirs: left.label,
                yours: right.label,
                output: output.label,
            };
            return LaunchOutcome::Merge(Box::new(request), names);
        }
        LaunchRequest::Help => return LaunchOutcome::Help,
    };
    // set_path clears the label, so the label goes second.
    for (file, side) in [(file_1, left), (file_2, right)] {
        file.set_path(side.path);
        file.set_label(side.label);
    }
    LaunchOutcome::Opened
}

fn swap_sides(
    file_1: &mut FileProcessor,
    file_2: &mut FileProcessor,
    scroll_left: &mut f32,
    scroll_right: &mut f32,
    diff_processor: &mut DiffProcessor,
) {
    std::mem::swap(file_1, file_2);
    std::mem::swap(scroll_left, scroll_right);

    diff_processor.reset_ctx();
}

fn toggle_all_options(
    toggle_all: &mut ToggleAll,
    options: &mut DiffBuilderOptions,
    h_scroll_linked: &mut bool,
    word_wrap: &mut bool,
) {
    let current = OptionSet::read(options, *h_scroll_linked, *word_wrap);
    toggle_all
        .press(current)
        .apply(options, h_scroll_linked, word_wrap);
}

// Mirrors the toolbar buttons in diff_pane.
fn toggle_option(ctx: &mut AppStateCtx, action: FileAction) {
    let options = &mut ctx.diff_options;
    let flip = |b: &mut bool| *b = !*b;
    match action {
        FileAction::IgnoreWhitespace => flip(&mut options.ignore.whitespace),
        FileAction::IgnoreComments => flip(&mut options.ignore.comments),
        FileAction::IgnoreRegex => flip(&mut options.ignore.regex_enabled),
        FileAction::HighlightRows => flip(&mut options.highlight_rows),
        FileAction::GhostRows => flip(&mut options.ghost_rows),
        FileAction::LinkHScroll => flip(&mut ctx.h_scroll_linked),
        FileAction::WordWrap => flip(&mut ctx.word_wrap),
        FileAction::DiffOnly => {
            options.diff_only_with_extra_rows = match options.diff_only_with_extra_rows {
                Some(_) => None,
                None => Some(2),
            }
        }
        FileAction::FewerContextRows => {
            if let Some(rows) = &mut options.diff_only_with_extra_rows {
                *rows = rows.saturating_sub(1);
            }
        }
        FileAction::MoreContextRows => {
            if let Some(rows) = &mut options.diff_only_with_extra_rows {
                *rows = (*rows + 1).min(99);
            }
        }
        other => panic!("{other:?} is not an option toggle"),
    }
}

/// Why a Quick Diff left both sides as they were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuickDiffRefusal {
    BothTemp,
}

impl std::fmt::Display for QuickDiffRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            QuickDiffRefusal::BothTemp => {
                "both sides are temporary files, so neither has a workspace path to diff against"
            }
        })
    }
}

/// Points the sides at a Quick Diff slot's paths. A slot without a target path diffs the same
/// root-relative path under its target root. That path comes from the left side, or from the
/// right side when the left one is a temp copy, as when p4 launches an external diff. With both
/// sides temp, nothing changes.
fn apply_quick_diff(
    slot: &QuickDiffPaths,
    file_1: &mut FileProcessor,
    file_2: &mut FileProcessor,
    temp_root: &Path,
) -> Result<(), QuickDiffRefusal> {
    let is_temp = |file: &mut FileProcessor| file.get_full_path().is_temp(temp_root);
    if is_temp(file_1) && is_temp(file_2) {
        return Err(QuickDiffRefusal::BothTemp);
    }
    if let Some((source_root, source_path)) = &slot.source {
        file_1.set_root(UniversalPath::from(source_root));
        file_1.set_path(UniversalPath::from(source_path));
    }

    // Don't set any target paths if root & path is ""
    if !(slot.target.0.is_empty() && slot.target.1.is_empty()) {
        let target_path = if slot.target.1.is_empty() {
            // The sides as they are now, so a slot source counts as the left side. Read before
            // file_2 gets the target root.
            let identity = if is_temp(file_1) {
                &mut *file_2
            } else {
                &mut *file_1
            };
            &identity.get_path().to_string()
        } else {
            &slot.target.1
        };

        file_2.set_root(UniversalPath::from(&slot.target.0));
        file_2.set_path(UniversalPath::from(target_path));
    }
    Ok(())
}

/// Why the local-file shortcut left the target as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LocalFileRefusal {
    LocalSource(PathBuf),
    /// p4 failed, or the depot file isn't mapped.
    Where(String),
    Missing(PathBuf),
}

impl std::fmt::Display for LocalFileRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocalFileRefusal::LocalSource(path) => write!(
                f,
                "the source {} is a local file, not a depot path",
                path.display()
            ),
            LocalFileRefusal::Where(e) => write!(f, "p4 where: {e}"),
            LocalFileRefusal::Missing(path) => {
                write!(f, "the local file {} doesn't exist", path.display())
            }
        }
    }
}

/// Points the target at the workspace file that the source depot file maps to, and returns it.
/// The source stays. On a refusal nothing changes.
fn diff_against_local_file(
    file_1: &mut FileProcessor,
    file_2: &mut FileProcessor,
    p4: &impl P4Runner,
) -> Result<PathBuf, LocalFileRefusal> {
    let source = file_1.get_full_path();
    if let UniversalPath::Local(path) = source {
        return Err(LocalFileRefusal::LocalSource(path));
    }
    let local = local_path_of_depot_file(&source, p4).map_err(LocalFileRefusal::Where)?;
    if !local.is_file() {
        return Err(LocalFileRefusal::Missing(local));
    }
    // The target's root belonged to its previous file; a depot root would be prefixed to this one.
    file_2.set_root("".into());
    file_2.set_path(UniversalPath::Local(local.clone()));
    Ok(local)
}

/// Why the shelf action left both sides as they were.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ShelfBaseRefusal {
    NoShelfSide,
    Listing(String),
    NotShelved(String),
    NoBase { depot_file: String, action: String },
}

impl std::fmt::Display for ShelfBaseRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShelfBaseRefusal::NoShelfSide => write!(f, "neither side is a shelved file (@=n)"),
            ShelfBaseRefusal::Listing(e) => f.write_str(e),
            ShelfBaseRefusal::NotShelved(file) => write!(f, "{file} is not in the shelf"),
            ShelfBaseRefusal::NoBase { depot_file, action } => {
                write!(
                    f,
                    "{depot_file} is shelved as {action}: it has no base revision"
                )
            }
        }
    }
}

fn shelved_file(path: &UniversalPath) -> Option<(&str, u32)> {
    match path {
        UniversalPath::Depot(file, Some(DepotRev::Shelf(change))) => Some((file, *change)),
        _ => None,
    }
}

/// Points the side that is not at `@=n` at the base revision the shelf lists for the other side's
/// file, and returns it. A shelved source wins over a shelved target. On a refusal nothing changes.
fn diff_against_shelf_base(
    file_1: &mut FileProcessor,
    file_2: &mut FileProcessor,
    p4: &impl P4Runner,
) -> Result<UniversalPath, ShelfBaseRefusal> {
    let (path_1, path_2) = (file_1.get_full_path(), file_2.get_full_path());
    let ((depot_file, change), other) = match (shelved_file(&path_1), shelved_file(&path_2)) {
        (Some(shelved), _) => (shelved, file_2),
        (None, Some(shelved)) => (shelved, file_1),
        (None, None) => return Err(ShelfBaseRefusal::NoShelfSide),
    };
    let shelf = list_shelf(p4, change).map_err(ShelfBaseRefusal::Listing)?;
    let entry = shelf
        .iter()
        .find(|entry| entry.depot_file == depot_file)
        .ok_or_else(|| ShelfBaseRefusal::NotShelved(depot_file.to_owned()))?;
    let base = entry.base_rev.ok_or_else(|| ShelfBaseRefusal::NoBase {
        depot_file: entry.depot_file.clone(),
        action: entry.action.clone(),
    })?;
    let base = UniversalPath::Depot(entry.depot_file.clone(), Some(DepotRev::Rev(base)));
    // The other side's root belonged to its previous file; it would be prefixed to this one.
    other.clear_root();
    other.set_path(base.clone());
    Ok(base)
}

fn run_diff_against_shelf_base(file_1: &mut FileProcessor, file_2: &mut FileProcessor) {
    let source = file_1.get_full_path();
    let shelved = if shelved_file(&source).is_some() {
        source
    } else {
        file_2.get_full_path()
    };
    let p4 = P4Command::new(false).for_file(shelved);
    match diff_against_shelf_base(file_1, file_2, &p4) {
        Ok(base) => log::info!("Diffing against the shelf base {base}"),
        Err(refusal @ ShelfBaseRefusal::NoShelfSide) => {
            log::info!("Can not diff against the shelf base: {refusal}")
        }
        Err(refusal) => log::error!("Can not diff against the shelf base: {refusal}"),
    }
}

#[cfg(all(test, feature = "serde"))]
mod tests {
    use super::*;
    use crate::viewer::audio::AudioMode;
    use crate::viewer::image::ImageSampling;
    use zdiff::ignore::IgnorePatterns;

    #[test]
    fn an_old_viewer_map_is_read_and_saved_again_until_migrated() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut().unwrap().insert(
            "extension_map".into(),
            serde_json::json!({ ".ZBIN": "Hex", "png": "Image" }),
        );
        let mut restored: AppStateCtx = serde_json::from_value(json).unwrap();
        let old = BTreeMap::from([
            (".ZBIN".to_string(), ViewerKind::Hex),
            ("png".to_string(), ViewerKind::Image),
        ]);
        assert_eq!(restored.legacy_extension_map, Some(old.clone()));

        let resaved: AppStateCtx =
            serde_json::from_str(&serde_json::to_string(&restored).unwrap()).unwrap();
        assert_eq!(resaved.legacy_extension_map, Some(old));

        restored.legacy_extension_map = None;
        let json = serde_json::to_value(&restored).unwrap();
        assert!(json.get("extension_map").is_none());
    }

    #[test]
    fn a_save_without_a_viewer_map_has_nothing_to_migrate() {
        let mut ctx = AppStateCtx::default();
        ctx.viewer_override.kind = Some(ViewerKind::Text);
        let json = serde_json::to_value(&ctx).unwrap();
        assert!(json.get("extension_map").is_none());
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.legacy_extension_map, None);
        // Paths aren't saved, so a restart opens a new pair and the override is gone.
        assert_eq!(restored.viewer_override.kind, None);
    }

    #[test]
    fn the_ffmpeg_path_is_saved_and_older_saves_have_none() {
        let mut ctx = AppStateCtx::default();
        ctx.ffmpeg.path = r"C:\tools\ffmpeg\bin".to_string();
        let mut json = serde_json::to_value(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(restored.ffmpeg.path, r"C:\tools\ffmpeg\bin");

        json.as_object_mut().unwrap().remove("ffmpeg");
        let older: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(older.ffmpeg.path, "");
    }

    #[test]
    fn the_image_sampling_is_saved_and_older_saves_have_nearest() {
        let mut ctx = AppStateCtx::default();
        ctx.image_settings.sampling = ImageSampling::Linear;
        let mut json = serde_json::to_value(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(restored.image_settings.sampling, ImageSampling::Linear);

        json.as_object_mut().unwrap().remove("image_settings");
        let older: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(older.image_settings.sampling, ImageSampling::Nearest);
    }

    #[test]
    fn the_audio_view_mode_is_saved_and_older_saves_have_lanes() {
        let mut ctx = AppStateCtx::default();
        ctx.audio_settings.mode = AudioMode::Overlay;
        let mut json = serde_json::to_value(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(restored.audio_settings.mode, AudioMode::Overlay);

        json.as_object_mut().unwrap().remove("audio_settings");
        let older: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(older.audio_settings.mode, AudioMode::Lanes);
    }

    #[test]
    fn pins_are_never_saved() {
        let mut ctx = AppStateCtx::default();
        ctx.diff_processor.set_pins(vec![zdiff::myers::Pin {
            source: 1,
            target: 2,
        }]);

        let json = serde_json::to_string(&ctx).unwrap();
        assert!(!json.contains("pin"), "{json}");
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert!(restored.diff_processor.pins().is_empty());
    }

    #[test]
    fn a_pivot_saved_by_an_older_version_is_discarded() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json["diff_options"].as_object_mut().unwrap().insert(
            "pivot_lines".into(),
            serde_json::json!({"left": 2, "right": 3}),
        );
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.diff_options, DiffBuilderOptions::default());
        assert!(restored.diff_processor.pins().is_empty());
        let saved_again = serde_json::to_string(&restored).unwrap();
        assert!(!saved_again.contains("pivot"), "{saved_again}");
    }

    #[test]
    fn horizontal_scroll_link_defaults_on_and_survives_a_restart() {
        assert!(AppStateCtx::default().h_scroll_linked);

        let mut ctx = AppStateCtx::default();
        ctx.h_scroll_linked = false;
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert!(!restored.h_scroll_linked);
    }

    #[test]
    fn horizontal_scroll_wheel_modifier_defaults_to_shift_and_survives_a_restart() {
        assert_eq!(
            AppStateCtx::default().h_scroll_modifier,
            WheelModifier::Shift
        );

        let mut ctx = AppStateCtx::default();
        ctx.h_scroll_modifier = WheelModifier::Ctrl;
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.h_scroll_modifier, WheelModifier::Ctrl);
    }

    #[test]
    fn a_save_without_the_horizontal_scroll_wheel_modifier_loads_shift() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("h_scroll_modifier")
            .unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.h_scroll_modifier, WheelModifier::Shift);
    }

    #[test]
    fn regex_ignore_presets_survive_a_restart() {
        use crate::ui_egui::diff_pane::REGEX_IGNORE_PRESETS;
        let mut ctx = AppStateCtx::default();
        ctx.regex_ignore_presets
            .save(REGEX_IGNORE_PRESETS, "Mine", r"\d+\nfoo")
            .unwrap();

        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.regex_ignore_presets, ctx.regex_ignore_presets);
        let user: Vec<_> = restored
            .regex_ignore_presets
            .list(REGEX_IGNORE_PRESETS)
            .filter(|e| !e.builtin)
            .map(|e| (e.name, e.text))
            .collect();
        assert_eq!(user, [("Mine", r"\d+\nfoo")]);
    }

    #[test]
    fn a_save_without_regex_ignore_presets_loads_none() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("regex_ignore_presets")
            .unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.regex_ignore_presets, UserPresets::default());
    }

    #[test]
    fn a_save_without_the_horizontal_scroll_link_loads_linked() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("h_scroll_linked")
            .unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert!(restored.h_scroll_linked);
    }

    #[test]
    fn zoom_survives_a_restart() {
        let mut app = ZApp::new();
        app.zoom = 1.7;
        let json = serde_json::to_string(&app).unwrap();
        let restored: ZApp = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.zoom, 1.7);
    }

    #[test]
    fn a_merge_is_never_saved() {
        let mut app = ZApp::new();
        app.merge = Some(MergeView::new(
            MergeRequest {
                base: None,
                theirs: UniversalPath::from("merge_theirs.txt"),
                yours: UniversalPath::from("merge_yours.txt"),
                output: UniversalPath::from("merge_output.txt"),
            },
            MyersDiffAlgorithm::Linear,
        ));
        let json = serde_json::to_string(&app).unwrap();
        for path in ["merge_theirs.txt", "merge_yours.txt", "merge_output.txt"] {
            assert!(!json.contains(path), "{json}");
        }
        let restored: ZApp = serde_json::from_str(&json).unwrap();
        assert!(restored.merge.is_none());
    }

    #[test]
    fn a_saved_zoom_out_of_range_loads_clamped() {
        let mut json = serde_json::to_value(ZApp::new()).unwrap();
        json["zoom"] = 10.0.into();
        assert_eq!(
            serde_json::from_value::<ZApp>(json.clone()).unwrap().zoom,
            3.0
        );
        json["zoom"] = 0.1.into();
        assert_eq!(serde_json::from_value::<ZApp>(json).unwrap().zoom, 0.5);
    }

    #[test]
    fn a_save_without_a_zoom_loads_at_100_percent() {
        let mut json = serde_json::to_value(ZApp::new()).unwrap();
        json.as_object_mut().unwrap().remove("zoom").unwrap();
        let restored: ZApp = serde_json::from_value(json).unwrap();
        assert_eq!(restored.zoom, 1.0);
    }

    #[test]
    fn diff_phases_survive_a_restart() {
        let mut ctx = AppStateCtx::default();
        ctx.diff_options.phases = DiffPhases {
            scope: true,
            line: false,
            token: true,
            character: true,
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.diff_options.phases, ctx.diff_options.phases);
    }

    #[test]
    fn a_save_without_diff_phases_loads_the_defaults() {
        let mut ctx = AppStateCtx::default();
        ctx.diff_options.ignore.whitespace = true;
        let mut json = serde_json::to_value(&ctx).unwrap();
        json["diff_options"]
            .as_object_mut()
            .unwrap()
            .remove("phases")
            .unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(
            restored.diff_options.phases,
            DiffBuilderOptions::default().phases
        );
        assert!(restored.diff_options.ignore.whitespace);
    }

    #[test]
    fn a_save_without_diff_marks_loads_only_the_line_mark() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        let options = json["diff_options"].as_object_mut().unwrap();
        for field in ["mark_scopes", "mark_lines", "mark_tokens", "mark_chars"] {
            options.remove(field).unwrap();
        }
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.diff_options, DiffBuilderOptions::default());
        assert!(restored.diff_options.highlight_rows && restored.diff_options.mark_lines);
    }

    #[test]
    fn swap_sides_swaps_the_files_and_their_scroll() {
        let mut ctx = AppStateCtx::default();
        let (a, b) = (UniversalPath::from("a.txt"), UniversalPath::from("b.txt"));
        ctx.file_1.set_path(a.clone());
        ctx.file_2.set_path(b.clone());
        ctx.scroll_left = 1.0;
        ctx.scroll_right = 2.0;
        ctx.diff_processor.conflict_cursor.set(3);
        swap_sides(
            &mut ctx.file_1,
            &mut ctx.file_2,
            &mut ctx.scroll_left,
            &mut ctx.scroll_right,
            &mut ctx.diff_processor,
        );
        assert_eq!(ctx.file_1.get_full_path(), b);
        assert_eq!(ctx.file_2.get_full_path(), a);
        assert_eq!((ctx.scroll_left, ctx.scroll_right), (2.0, 1.0));
        assert_eq!(ctx.diff_processor.conflict_cursor.get(), 0);
    }

    #[test]
    fn each_toggle_action_flips_its_option_and_back() {
        type Get = fn(&AppStateCtx) -> bool;
        let toggles: [(FileAction, Get); 8] = [
            (FileAction::IgnoreWhitespace, |c| {
                c.diff_options.ignore.whitespace
            }),
            (FileAction::IgnoreComments, |c| {
                c.diff_options.ignore.comments
            }),
            (FileAction::IgnoreRegex, |c| {
                c.diff_options.ignore.regex_enabled
            }),
            (FileAction::HighlightRows, |c| c.diff_options.highlight_rows),
            (FileAction::GhostRows, |c| c.diff_options.ghost_rows),
            (FileAction::LinkHScroll, |c| c.h_scroll_linked),
            (FileAction::WordWrap, |c| c.word_wrap),
            (FileAction::DiffOnly, |c| {
                c.diff_options.diff_only_with_extra_rows.is_some()
            }),
        ];
        for (action, get) in toggles {
            let mut ctx = AppStateCtx::default();
            let before = AppStateCtx::default();
            toggle_option(&mut ctx, action);
            assert_eq!(get(&ctx), !get(&before), "{action:?}");
            toggle_option(&mut ctx, action);
            assert_eq!(get(&ctx), get(&before), "{action:?}");
            assert_eq!(ctx.diff_options, before.diff_options, "{action:?}");
        }
    }

    #[test]
    fn regex_off_keeps_the_patterns() {
        let mut ctx = AppStateCtx::default();
        ctx.diff_options.ignore.patterns = IgnorePatterns::new(r"\d+");
        toggle_option(&mut ctx, FileAction::IgnoreRegex);
        assert!(!ctx.diff_options.ignore.regex_enabled);
        assert_eq!(ctx.diff_options.ignore.patterns.text(), r"\d+");
    }

    #[test]
    fn diff_only_turns_on_with_two_context_rows() {
        let mut ctx = AppStateCtx::default();
        toggle_option(&mut ctx, FileAction::DiffOnly);
        assert_eq!(ctx.diff_options.diff_only_with_extra_rows, Some(2));
        ctx.diff_options.diff_only_with_extra_rows = Some(5);
        toggle_option(&mut ctx, FileAction::DiffOnly);
        assert_eq!(ctx.diff_options.diff_only_with_extra_rows, None);
    }

    #[test]
    fn context_rows_step_between_0_and_99_in_diff_only() {
        let rows = |start: Option<usize>, action| {
            let mut ctx = AppStateCtx::default();
            ctx.diff_options.diff_only_with_extra_rows = start;
            toggle_option(&mut ctx, action);
            ctx.diff_options.diff_only_with_extra_rows
        };
        assert_eq!(rows(Some(2), FileAction::FewerContextRows), Some(1));
        assert_eq!(rows(Some(0), FileAction::FewerContextRows), Some(0));
        assert_eq!(rows(Some(2), FileAction::MoreContextRows), Some(3));
        assert_eq!(rows(Some(99), FileAction::MoreContextRows), Some(99));
        assert_eq!(rows(None, FileAction::FewerContextRows), None);
        assert_eq!(rows(None, FileAction::MoreContextRows), None);
    }

    #[test]
    fn regex_off_survives_a_restart_with_its_patterns() {
        let mut ctx = AppStateCtx::default();
        ctx.diff_options.ignore.patterns = IgnorePatterns::new(r"\d+");
        ctx.diff_options.ignore.regex_enabled = false;
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.diff_options.ignore, ctx.diff_options.ignore);
    }

    #[test]
    fn a_save_without_the_regex_flag_loads_with_regex_enabled() {
        let mut ctx = AppStateCtx::default();
        ctx.diff_options.ignore.patterns = IgnorePatterns::new(r"\d+");
        ctx.diff_options.ignore.regex_enabled = false;
        let mut json = serde_json::to_value(&ctx).unwrap();
        json["diff_options"]
            .as_object_mut()
            .unwrap()
            .remove("ignore_regex_enabled")
            .unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert!(restored.diff_options.ignore.regex_enabled);
        assert_eq!(restored.diff_options.ignore.patterns.text(), r"\d+");
    }

    #[test]
    fn word_wrap_defaults_off_and_survives_a_restart() {
        assert!(!AppStateCtx::default().word_wrap);

        let mut ctx = AppStateCtx::default();
        ctx.word_wrap = true;
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert!(restored.word_wrap);
    }

    #[test]
    fn text_mode_defaults_to_side_by_side_and_survives_a_restart() {
        assert_eq!(AppStateCtx::default().text_mode, TextMode::SideBySide);

        let mut ctx = AppStateCtx::default();
        ctx.text_mode = TextMode::Unified;
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.text_mode, TextMode::Unified);
    }

    #[test]
    fn the_chosen_single_file_side_survives_a_restart() {
        for mode in [TextMode::SingleLeft, TextMode::SingleRight] {
            let mut ctx = AppStateCtx::default();
            ctx.text_mode = mode;
            let json = serde_json::to_string(&ctx).unwrap();
            let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
            assert_eq!(restored.text_mode, mode);
        }
    }

    #[test]
    fn a_save_without_a_text_mode_loads_side_by_side() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut().unwrap().remove("text_mode").unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.text_mode, TextMode::SideBySide);
    }

    #[test]
    fn a_save_without_word_wrap_loads_unwrapped() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut().unwrap().remove("word_wrap").unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert!(!restored.word_wrap);
    }

    fn old_format_save(missing: &[&str]) -> serde_json::Value {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        let quick_diffs = json["keybindings"]["user_quick_diffs"].take();
        let mut old = serde_json::json!({ "user_quick_diffs": quick_diffs });
        for field in [
            "find",
            "undo_revert",
            "redo_revert",
            "redo_revert_alt",
            "diff_local_file",
        ] {
            if !missing.contains(&field) {
                old[field] = serde_json::Value::Null;
            }
        }
        json["keybindings"] = old;
        json
    }

    #[test]
    fn an_old_save_without_the_undo_keys_loads_the_defaults() {
        let json = old_format_save(&["undo_revert", "redo_revert", "redo_revert_alt"]);
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        let keymap = &restored.keybindings.keymap;
        assert!(!FileAction::UndoRevert.defaults().is_empty());
        assert_eq!(
            keymap.shortcuts(FileAction::UndoRevert),
            FileAction::UndoRevert.defaults()
        );
        assert_eq!(keymap.shortcuts(FileAction::Redo).len(), 2);
        assert_eq!(
            keymap.shortcuts(FileAction::Redo),
            FileAction::Redo.defaults()
        );
        assert_eq!(keymap.shortcuts(FileAction::DiffLocalFile), []);
    }

    #[test]
    fn undo_keys_survive_a_restart() {
        let mut ctx = AppStateCtx::default();
        let find = ctx.keybindings.keymap.shortcuts(FileAction::Find)[0];
        let keymap = &mut ctx.keybindings.keymap;
        keymap.shortcuts_mut(FileAction::UndoRevert).clear();
        keymap.shortcuts_mut(FileAction::Redo).insert(0, find);
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.keybindings.keymap, ctx.keybindings.keymap);
    }

    #[test]
    fn an_old_save_without_the_local_file_key_loads_the_default() {
        let json = old_format_save(&["diff_local_file"]);
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        let keymap = &restored.keybindings.keymap;
        assert!(!FileAction::DiffLocalFile.defaults().is_empty());
        assert_eq!(
            keymap.shortcuts(FileAction::DiffLocalFile),
            FileAction::DiffLocalFile.defaults()
        );
        assert_eq!(keymap.shortcuts(FileAction::UndoRevert), []);
    }

    #[test]
    fn the_local_file_key_survives_a_restart() {
        let mut ctx = AppStateCtx::default();
        let find = ctx.keybindings.keymap.shortcuts(FileAction::Find)[0];
        *ctx.keybindings
            .keymap
            .shortcuts_mut(FileAction::DiffLocalFile) = vec![find];
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored
                .keybindings
                .keymap
                .shortcuts(FileAction::DiffLocalFile),
            [find]
        );

        ctx.keybindings
            .keymap
            .shortcuts_mut(FileAction::DiffLocalFile)
            .clear();
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored
                .keybindings
                .keymap
                .shortcuts(FileAction::DiffLocalFile),
            []
        );
    }

    #[test]
    fn p4_profiles_and_slot_profiles_survive_a_restart() {
        let mut ctx = AppStateCtx::default();
        let id = ctx.p4_profiles.add("work".into());
        ctx.p4_profiles.profiles[0].config.port = "ssl:p4:1666".into();
        ctx.p4_profiles.default = Some(id);
        ctx.keybindings.user_quick_diffs[1].1.p4_profile = Some(id);

        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.p4_profiles, ctx.p4_profiles);
        let slot_profiles: Vec<_> = restored
            .keybindings
            .user_quick_diffs
            .iter()
            .map(|(_, slot)| slot.p4_profile)
            .collect();
        assert_eq!(slot_profiles, [None, Some(id), None, None]);
    }

    #[test]
    fn a_save_with_a_p4_password_loads_and_drops_it_on_the_next_save() {
        let mut ctx = AppStateCtx::default();
        ctx.p4_profiles.add("work".into());
        let mut json = serde_json::to_value(&ctx).unwrap();
        json["p4_profiles"]["profiles"][0]["config"]["password"] = "secret".into();

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.p4_profiles, ctx.p4_profiles);
        assert!(
            !serde_json::to_string(&restored)
                .unwrap()
                .contains("password")
        );
    }

    /// The single p4 config before profiles was never saved, so such a save has no profiles.
    #[test]
    fn a_save_without_p4_profiles_loads_with_none_and_auto_slots() {
        let mut ctx = AppStateCtx::default();
        ctx.p4_profiles.add("work".into());
        let mut json = serde_json::to_value(&ctx).unwrap();
        json.as_object_mut().unwrap().remove("p4_profiles").unwrap();
        for slot in json["keybindings"]["user_quick_diffs"]
            .as_array_mut()
            .unwrap()
        {
            slot[1]
                .as_object_mut()
                .unwrap()
                .remove("p4_profile")
                .unwrap();
        }
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.p4_profiles, P4Profiles::default());
        assert_eq!(restored.keybindings.user_quick_diffs.len(), 4);
        assert!(
            restored
                .keybindings
                .user_quick_diffs
                .iter()
                .all(|(_, slot)| slot.p4_profile.is_none())
        );
    }

    #[test]
    fn a_save_with_p4_profiles_but_no_next_id_adds_unused_ids() {
        let mut ctx = AppStateCtx::default();
        let id = ctx.p4_profiles.add("work".into());
        let mut json = serde_json::to_value(&ctx).unwrap();
        json["p4_profiles"]
            .as_object_mut()
            .unwrap()
            .remove("next_id")
            .unwrap();
        let mut restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_ne!(restored.p4_profiles.add("new".into()), id);
    }

    mod quick_diff {
        use super::*;

        const TEMP: &str = r"C:\Users\me\AppData\Local\Temp";
        const TEMP_COPY: &str = r"C:\Users\me\AppData\Local\Temp\p4v\main.rs#3";
        const WORKSPACE: &str = r"C:\ws";
        const WORKSPACE_FILE: &str = r"C:\ws\src\main.rs";

        /// A slot that diffs the open file's root-relative path under a depot root.
        fn depot_slot() -> QuickDiffPaths {
            QuickDiffPaths {
                target: ("//depot/main".into(), String::new()),
                source: None,
                p4_profile: None,
            }
        }

        fn side(root: Option<&str>, path: &str) -> FileProcessor {
            let mut file = FileProcessor::new();
            if let Some(root) = root {
                file.set_root(UniversalPath::from(root));
            }
            file.set_path(UniversalPath::from(path));
            file
        }

        fn apply(
            slot: &QuickDiffPaths,
            file_1: &mut FileProcessor,
            file_2: &mut FileProcessor,
        ) -> Result<(), QuickDiffRefusal> {
            apply_quick_diff(slot, file_1, file_2, Path::new(TEMP))
        }

        #[test]
        fn neither_temp_takes_the_left_sides_path() {
            let mut left = side(Some(WORKSPACE), WORKSPACE_FILE);
            let mut right = side(None, r"C:\other\lib.rs");
            assert_eq!(apply(&depot_slot(), &mut left, &mut right), Ok(()));
            assert_eq!(
                right.get_full_path(),
                UniversalPath::new("//depot/main/src/main.rs")
            );
            assert_eq!(left.get_full_path(), UniversalPath::new(WORKSPACE_FILE));
        }

        #[test]
        fn a_temp_left_side_takes_the_right_sides_path() {
            let mut left = side(None, TEMP_COPY);
            let mut right = side(Some(WORKSPACE), WORKSPACE_FILE);
            assert_eq!(apply(&depot_slot(), &mut left, &mut right), Ok(()));
            assert_eq!(
                right.get_full_path(),
                UniversalPath::new("//depot/main/src/main.rs")
            );
            assert_eq!(left.get_full_path(), UniversalPath::new(TEMP_COPY));
        }

        #[test]
        fn a_temp_right_side_takes_the_left_sides_path() {
            let mut left = side(Some(WORKSPACE), WORKSPACE_FILE);
            let mut right = side(None, TEMP_COPY);
            assert_eq!(apply(&depot_slot(), &mut left, &mut right), Ok(()));
            assert_eq!(
                right.get_full_path(),
                UniversalPath::new("//depot/main/src/main.rs")
            );
            assert_eq!(left.get_full_path(), UniversalPath::new(WORKSPACE_FILE));
        }

        #[test]
        fn both_temp_disables_every_slot_and_changes_nothing() {
            let other_copy = r"C:\Users\me\AppData\Local\Temp\p4v\main.rs#4";
            let slots = [
                depot_slot(),
                QuickDiffPaths {
                    target: ("//depot/main".into(), "src/main.rs".into()),
                    source: Some((WORKSPACE.into(), WORKSPACE_FILE.into())),
                    p4_profile: None,
                },
            ];
            for slot in slots {
                let mut left = side(None, TEMP_COPY);
                let mut right = side(None, other_copy);
                assert_eq!(
                    apply(&slot, &mut left, &mut right),
                    Err(QuickDiffRefusal::BothTemp),
                    "{slot:?}"
                );
                assert_eq!(left.get_full_path(), UniversalPath::new(TEMP_COPY));
                assert_eq!(left.get_root(), None);
                assert_eq!(right.get_full_path(), UniversalPath::new(other_copy));
                assert_eq!(right.get_root(), None);
            }
        }

        mod local_file {
            use super::*;
            use crate::p4::FakeP4;

            const DEPOT_FILE: &str = "//depot/main/src/main.rs#3";
            const TARGET: &str = r"C:\other\lib.rs";

            fn where_output(local: &Path) -> String {
                format!(
                    "... depotFile //depot/main/src/main.rs\n\
                     ... clientFile //ws/main/src/main.rs\n\
                     ... path {}\n\n",
                    local.display()
                )
            }

            fn assert_unchanged(left: &mut FileProcessor, right: &mut FileProcessor) {
                assert_eq!(left.get_full_path(), UniversalPath::new(DEPOT_FILE));
                assert_eq!(right.get_full_path(), UniversalPath::new(TARGET));
            }

            #[test]
            fn a_mapped_existing_file_becomes_the_target() {
                let dir = tempfile::tempdir().unwrap();
                let local = dir.path().join("my dir").join("main.rs");
                std::fs::create_dir_all(local.parent().unwrap()).unwrap();
                std::fs::write(&local, "fn main() {}").unwrap();
                let p4 = FakeP4::new(|_: &[&str]| Ok(where_output(&local)));

                let mut left = side(None, DEPOT_FILE);
                let mut right = side(None, TARGET);
                assert_eq!(
                    diff_against_local_file(&mut left, &mut right, &p4),
                    Ok(local.clone())
                );
                assert_eq!(left.get_full_path(), UniversalPath::new(DEPOT_FILE));
                assert_eq!(right.get_full_path(), UniversalPath::Local(local.clone()));
                assert_eq!(
                    p4.calls(),
                    vec![vec!["-ztag", "where", "//depot/main/src/main.rs"]]
                );
            }

            #[test]
            fn a_depot_root_on_the_target_is_dropped() {
                // A depot root (from a Quick Diff) would be prefixed to the local path.
                let dir = tempfile::tempdir().unwrap();
                let local = dir.path().join("main.rs");
                std::fs::write(&local, "fn main() {}").unwrap();
                let p4 = FakeP4::new(|_: &[&str]| Ok(where_output(&local)));

                let mut left = side(None, DEPOT_FILE);
                let mut right = side(Some("//depot/rel"), "//depot/rel/src/main.rs");
                assert!(diff_against_local_file(&mut left, &mut right, &p4).is_ok());
                assert_eq!(right.get_full_path(), UniversalPath::Local(local));
            }

            #[test]
            fn an_unmapped_file_changes_nothing() {
                // p4 exits 0 on "not in client view", with nothing on stdout.
                let p4 = FakeP4::new(|_: &[&str]| Ok(String::new()));
                let mut left = side(None, DEPOT_FILE);
                let mut right = side(None, TARGET);
                assert!(matches!(
                    diff_against_local_file(&mut left, &mut right, &p4),
                    Err(LocalFileRefusal::Where(_))
                ));
                assert_unchanged(&mut left, &mut right);

                let p4 = FakeP4::new(|_: &[&str]| Err("connect failed".to_string()));
                assert_eq!(
                    diff_against_local_file(&mut left, &mut right, &p4),
                    Err(LocalFileRefusal::Where("connect failed".into()))
                );
                assert_unchanged(&mut left, &mut right);
            }

            #[test]
            fn a_mapped_file_missing_on_disk_changes_nothing() {
                let dir = tempfile::tempdir().unwrap();
                let local = dir.path().join("main.rs");
                let p4 = FakeP4::new(|_: &[&str]| Ok(where_output(&local)));
                let mut left = side(None, DEPOT_FILE);
                let mut right = side(None, TARGET);
                assert_eq!(
                    diff_against_local_file(&mut left, &mut right, &p4),
                    Err(LocalFileRefusal::Missing(local.clone()))
                );
                assert_unchanged(&mut left, &mut right);

                // A directory isn't a file to diff.
                std::fs::create_dir(&local).unwrap();
                assert_eq!(
                    diff_against_local_file(&mut left, &mut right, &p4),
                    Err(LocalFileRefusal::Missing(local))
                );
                assert_unchanged(&mut left, &mut right);
            }

            #[test]
            fn a_local_source_changes_nothing_and_runs_no_p4() {
                let p4 = FakeP4::new(|_: &[&str]| panic!("p4 must not run"));
                let mut left = side(Some(WORKSPACE), WORKSPACE_FILE);
                let mut right = side(None, TARGET);
                assert_eq!(
                    diff_against_local_file(&mut left, &mut right, &p4),
                    Err(LocalFileRefusal::LocalSource(WORKSPACE_FILE.into()))
                );
                assert_eq!(left.get_full_path(), UniversalPath::new(WORKSPACE_FILE));
                assert_eq!(right.get_full_path(), UniversalPath::new(TARGET));
            }
        }

        mod shelf_base {
            use super::*;
            use crate::p4::FakeP4;

            const SHELVED: &str = "//depot/main/src/a b.rs@=1234";
            const BASE: &str = "//depot/main/src/a b.rs#4";
            const TARGET: &str = r"C:\other\lib.rs";

            fn describe(change: u32) -> String {
                format!(
                    "... change {change}\n\
                     ... desc Fix the thing.\n\
                     ... depotFile0 //depot/main/src/a b.rs\n\
                     ... action0 edit\n\
                     ... rev0 4\n\
                     ... depotFile1 //depot/main/src/new.rs\n\
                     ... action1 add\n\
                     ... rev1 1\n\n"
                )
            }

            #[test]
            fn the_target_becomes_the_base_revision_of_the_shelved_source() {
                let p4 = FakeP4::new(|_: &[&str]| Ok(describe(1234)));
                let mut left = side(None, SHELVED);
                let mut right = side(None, TARGET);
                assert_eq!(
                    diff_against_shelf_base(&mut left, &mut right, &p4),
                    Ok(UniversalPath::new(BASE))
                );
                assert_eq!(left.get_full_path(), UniversalPath::new(SHELVED));
                assert_eq!(right.get_full_path(), UniversalPath::new(BASE));
                assert_eq!(
                    p4.calls(),
                    vec![vec!["-ztag", "describe", "-S", "-s", "1234"]]
                );
            }

            #[test]
            fn a_shelved_target_sets_the_source() {
                let p4 = FakeP4::new(|_: &[&str]| Ok(describe(1234)));
                let mut left = side(None, TARGET);
                let mut right = side(None, SHELVED);
                assert!(diff_against_shelf_base(&mut left, &mut right, &p4).is_ok());
                assert_eq!(left.get_full_path(), UniversalPath::new(BASE));
                assert_eq!(right.get_full_path(), UniversalPath::new(SHELVED));
            }

            #[test]
            fn a_depot_root_on_the_other_side_is_dropped() {
                let p4 = FakeP4::new(|_: &[&str]| Ok(describe(1234)));
                let mut left = side(None, SHELVED);
                let mut right = side(Some("//depot/rel"), "//depot/rel/lib.rs");
                assert!(diff_against_shelf_base(&mut left, &mut right, &p4).is_ok());
                assert_eq!(right.get_full_path(), UniversalPath::new(BASE));
                assert_eq!(right.get_root(), None);
            }

            #[test]
            fn a_file_added_by_the_shelf_has_no_base() {
                let p4 = FakeP4::new(|_: &[&str]| Ok(describe(1234)));
                let mut left = side(None, "//depot/main/src/new.rs@=1234");
                let mut right = side(None, TARGET);
                assert_eq!(
                    diff_against_shelf_base(&mut left, &mut right, &p4),
                    Err(ShelfBaseRefusal::NoBase {
                        depot_file: "//depot/main/src/new.rs".into(),
                        action: "add".into()
                    })
                );
                assert_eq!(right.get_full_path(), UniversalPath::new(TARGET));
            }

            #[test]
            fn a_file_missing_from_the_shelf_changes_nothing() {
                let p4 = FakeP4::new(|_: &[&str]| Ok(describe(1234)));
                let mut left = side(None, "//depot/main/src/other.rs@=1234");
                let mut right = side(None, TARGET);
                assert_eq!(
                    diff_against_shelf_base(&mut left, &mut right, &p4),
                    Err(ShelfBaseRefusal::NotShelved(
                        "//depot/main/src/other.rs".into()
                    ))
                );
                assert_eq!(right.get_full_path(), UniversalPath::new(TARGET));
            }

            #[test]
            fn a_failing_p4_changes_nothing() {
                let p4 = FakeP4::new(|_: &[&str]| Err("connect failed".to_string()));
                let mut left = side(None, SHELVED);
                let mut right = side(None, TARGET);
                let refusal = diff_against_shelf_base(&mut left, &mut right, &p4).unwrap_err();
                assert!(
                    matches!(&refusal, ShelfBaseRefusal::Listing(e) if e.contains("connect failed")),
                    "{refusal:?}"
                );
                assert_eq!(left.get_full_path(), UniversalPath::new(SHELVED));
                assert_eq!(right.get_full_path(), UniversalPath::new(TARGET));
            }

            #[test]
            fn sides_without_a_shelf_change_nothing_and_run_no_p4() {
                let p4 = FakeP4::new(|_: &[&str]| panic!("p4 must not run"));
                for source in ["//depot/main/src/a b.rs#3", "//depot/main/src/a b.rs@1234"] {
                    let mut left = side(None, source);
                    let mut right = side(None, TARGET);
                    assert_eq!(
                        diff_against_shelf_base(&mut left, &mut right, &p4),
                        Err(ShelfBaseRefusal::NoShelfSide)
                    );
                    assert_eq!(left.get_full_path(), UniversalPath::new(source));
                    assert_eq!(right.get_full_path(), UniversalPath::new(TARGET));
                }
            }
        }

        #[test]
        fn a_slot_source_is_applied_before_the_path_is_taken() {
            // The source replaces the temp left side, so it supplies the path.
            let slot = QuickDiffPaths {
                source: Some((WORKSPACE.into(), r"C:\ws\src\lib.rs".into())),
                ..depot_slot()
            };
            let mut left = side(None, TEMP_COPY);
            let mut right = side(None, r"C:\other\main.rs");
            assert_eq!(apply(&slot, &mut left, &mut right), Ok(()));
            assert_eq!(
                left.get_full_path(),
                UniversalPath::new(r"C:\ws\src\lib.rs")
            );
            assert_eq!(
                right.get_full_path(),
                UniversalPath::new("//depot/main/src/lib.rs")
            );
        }
    }

    mod launch {
        use super::*;

        fn launch(args: &[&str]) -> (FileProcessor, FileProcessor, LaunchOutcome) {
            let mut left = FileProcessor::new();
            let mut right = FileProcessor::new();
            let outcome = apply_launch(
                &mut left,
                &mut right,
                cli::parse_launch_args(args.iter().copied()),
            );
            (left, right, outcome)
        }

        #[test]
        fn diff_sets_paths_and_labels() {
            let (mut left, mut right, outcome) = launch(&["-nl", "A", "-nr", "B", "x", "y"]);
            assert_eq!(outcome, LaunchOutcome::Opened);
            assert_eq!(left.get_full_path(), UniversalPath::new("x"));
            assert_eq!(right.get_full_path(), UniversalPath::new("y"));
            assert_eq!(left.label(), Some("A"));
            assert_eq!(right.label(), Some("B"));
        }

        #[test]
        fn help_opens_nothing() {
            let (mut left, _, outcome) = launch(&["--help"]);
            assert_eq!(outcome, LaunchOutcome::Help);
            assert_eq!(left.get_full_path(), UniversalPath::default());
        }

        #[test]
        fn error_is_returned_and_opens_nothing() {
            let (mut left, _, outcome) = launch(&["a", "b", "c"]);
            assert_eq!(
                outcome,
                LaunchOutcome::Error(cli::LaunchError::PositionalCount(3).to_string())
            );
            assert_eq!(left.get_full_path(), UniversalPath::default());
        }

        #[test]
        fn merge_opens_a_merge_with_labels_and_leaves_the_diff_sides() {
            let (mut left, mut right, outcome) = launch(&[
                "-nb", "B", "-nl", "T", "-nr", "Y", "-nm", "R", "b", "t", "y", "o",
            ]);
            assert_eq!(
                outcome,
                LaunchOutcome::Merge(
                    Box::new(MergeRequest {
                        base: Some(UniversalPath::new("b")),
                        theirs: UniversalPath::new("t"),
                        yours: UniversalPath::new("y"),
                        output: UniversalPath::new("o"),
                    }),
                    MergeNames {
                        base: Some("B".into()),
                        theirs: Some("T".into()),
                        yours: Some("Y".into()),
                        output: Some("R".into()),
                    }
                )
            );
            assert_eq!(left.get_full_path(), UniversalPath::default());
            assert_eq!(right.get_full_path(), UniversalPath::default());
        }

        #[test]
        fn merge_into_a_depot_output_is_an_error() {
            let (_, _, outcome) = launch(&["b", "t", "y", "//depot/o.txt"]);
            assert_eq!(
                outcome,
                LaunchOutcome::Error("Output must be a local path".to_string())
            );
        }

        #[test]
        fn merge_into_a_git_revision_output_is_an_error() {
            let (_, _, outcome) = launch(&["b", "t", "y", r"C:\repo\o.txt@git:main"]);
            assert_eq!(
                outcome,
                LaunchOutcome::Error("Output must be a local path".to_string())
            );
        }
    }
}
