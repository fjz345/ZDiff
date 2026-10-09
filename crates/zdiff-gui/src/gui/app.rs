use eframe::egui::{self, Layout, PointerButton, containers::menu::MenuConfig};
use serde::{Deserialize, Serialize};
use std::{
    env,
    path::{Path, PathBuf},
    sync::mpsc,
};
use zcommon::find::FindQuery;
use zcommon::presets::UserPresets;
use zcommon::ui_egui::common::show_custom_popup;
use zcommon::ui_egui::find_popup::{FindPopupEvent, find_popup};
use zcommon::ui_egui::h_scroll::{self, WheelModifier};
use zcommon::ui_egui::keymap::{Action, Shortcut};
use zcommon::ui_egui::keymap_editor::KeymapEditor;
use zdiff::{
    diff_builder::DiffBuilderOptions,
    lexer::{LEXER_MODE_DEFAULT, LEXER_MODE_GREEDY, LEXER_MODE_NEWLINE, LEXER_MODE_TOKENIZE},
    myers::{DiffPhases, MyersDiffAlgorithm},
    universal_path::UniversalPath,
};

use eframe::epaint::Pos2;
use egui_tiles::Tile;

use crate::{
    cli::{self, LaunchRequest},
    diff_ctx::{DiffProcessor, UpdateDiffRowsInput},
    diff_phases::diff_phases_menu,
    file::{FileProcessor, LoadedFile},
    keybindings::{FileAction, Keybindings, QuickDiffPaths, first_shortcut_text, ui_keybindings},
    p4::{
        P4Command, P4Profiles, P4Runner, local_path_of_depot_file, set_p4_slot_profile,
        ui_p4_profiles, update_p4_profiles,
    },
    phase_trace_window::ui_phase_trace,
    revert::{
        self, HistoryStep, PendingP4Edit, RevertHistory, RevertRecord, RevertRefusal, RevertTarget,
        RevertWrite, WriteRefusal,
    },
    ui_egui::{
        diff_pane::{FileDiffPane, FileDiffPaneCtx, wheel_rects_id},
        merge_pane::{MergeView, OpenMergeForm},
        panes::{Pane, TreeBehavior},
    },
    viewer::{
        ExtensionMap, LoadedSide, ViewerKind, ViewerOverride, conflict_count,
        hex::{HexDiffProcessor, parse_hex_offset},
        image::ImageDiffProcessor,
        image_decode_fallback, resolve_viewer, ui_extension_map,
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
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub image_processor: ImageDiffProcessor,
    /// Resolved each frame from the loaded files; `None` while neither side is loaded.
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub viewer_kind: Option<ViewerKind>,
    /// Not persisted: paths aren't either, so a restart always opens a new pair.
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub viewer_override: ViewerOverride,
    /// The last logged reason the pair isn't in the viewer it asked for.
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub viewer_fallback: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub extension_map: ExtensionMap,
    /// Undo and redo of reverts; in memory only.
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub revert_history: RevertHistory,

    pub diff_lexer_mode: u8,
    pub diff_options: DiffBuilderOptions,

    pub myers_diff_algorithm: MyersDiffAlgorithm,
    pub code_language: String,
    pub code_language_custom: String,

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
            image_processor: Default::default(),
            viewer_kind: None,
            viewer_override: Default::default(),
            viewer_fallback: None,
            extension_map: Default::default(),
            revert_history: Default::default(),
            scroll_left: Default::default(),
            scroll_right: Default::default(),
            h_scroll_linked: default_h_scroll_linked(),
            h_scroll_modifier: WheelModifier::Shift,
            word_wrap: false,
            goto_open: Default::default(),
            find_open: Default::default(),
            goto_input: Default::default(),
            find_query: Default::default(),
            find_error: None,
            diff_lexer_mode: LEXER_MODE_DEFAULT,
            keybindings: Default::default(),
            p4_profiles: Default::default(),
            regex_ignore_presets: Default::default(),
            myers_diff_algorithm: Default::default(),
            code_language: "rs".to_string(),
            code_language_custom: "".into(),
        }
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
                        LaunchOutcome::Help => self.open_help_window = true,
                        LaunchOutcome::Error(message) => self.launch_error = Some(message),
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
        }
    }

    fn startup(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let visuals: egui::Visuals = egui::Visuals::dark();
        ctx.set_visuals(visuals);
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
            if let Some(path) = pollster::block_on(rfd::AsyncFileDialog::new().pick_file()) {
                if let Err(e) = tx.send(UniversalPath::from(path.path().to_path_buf())) {
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
        lexer_mode: &mut u8,
        keybindings: &mut Keybindings,
        p4_profiles: &mut P4Profiles,
        extension_map: &mut ExtensionMap,
        myers_diff_algorithm: &mut MyersDiffAlgorithm,
        diff_phases: &mut DiffPhases,
        code_language: &mut String,
        code_language_custom: &mut String,
        h_scroll_modifier: &mut WheelModifier,
    ) {
        ui.horizontal(|ui| {
            egui::MenuBar::new()
                .config(
                    MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
                )
                .ui(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    ui.menu_button("File", |ui| {
                        if menu_item(ui, "Open Source", keybindings, FileAction::OpenFileSource)
                            .clicked()
                        {
                            Self::open_file_picker(file_1.get_tx());
                        }
                        if menu_item(ui, "Open Target", keybindings, FileAction::OpenFileTarget)
                            .clicked()
                        {
                            Self::open_file_picker(file_2.get_tx());
                        }
                        if ui.button("Open Merge...").clicked() {
                            self.open_merge_form.open = true;
                        }
                        if menu_item(ui, "Swap Source/Target", keybindings, FileAction::SwapSides)
                            .clicked()
                        {
                            swap_sides(file_1, file_2, scroll_left, scroll_right, diff_processor);
                        }
                        if menu_item(ui, "Find", keybindings, FileAction::Find).clicked() {
                            *find_open = true;
                        }
                        if menu_item(ui, "Goto", keybindings, FileAction::Goto).clicked() {
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
                        ui.menu_button("Lexer", |ui| {
                            ui.radio_value(lexer_mode, LEXER_MODE_GREEDY, "LexerGreedy");
                            ui.radio_value(lexer_mode, LEXER_MODE_TOKENIZE, "LexerTokenize");
                            ui.radio_value(lexer_mode, LEXER_MODE_NEWLINE, "LexerNewLine");
                        });
                        ui.menu_button("Code Language", |ui| {
                            ui.radio_value(code_language, "rs".to_string(), "Rust (.rs)");
                            ui.radio_value(code_language, "py".to_string(), "Python (.py)");
                            ui.radio_value(code_language, "cpp".to_string(), "C++ (.cpp)");
                            ui.radio_value(code_language, "js".to_string(), "JavaScript (.js)");
                            ui.radio_value(code_language, "json".to_string(), "JSON (.json)");
                            ui.radio_value(code_language, "md".to_string(), "Markdown (.md)");
                            ui.separator();
                            ui.horizontal(|ui| {
                                if ui
                                    .radio(
                                        code_language == code_language_custom
                                            && !code_language_custom.is_empty(),
                                        "Use Custom",
                                    )
                                    .clicked()
                                {
                                    *code_language = code_language_custom.trim().to_string();
                                }
                                let text_edit = egui::TextEdit::singleline(code_language_custom)
                                    .hint_text("Custom (e.g. go, html)");
                                let res = ui.add(text_edit);
                                if res.changed() && !code_language_custom.is_empty() {
                                    *code_language = code_language_custom.trim().to_string();
                                }
                            });
                        });
                        *code_language = code_language.trim_matches('.').to_string();
                        ui.menu_button("Horizontal Scroll Wheel", |ui| {
                            ui.radio_value(h_scroll_modifier, WheelModifier::Shift, "Shift+Wheel");
                            ui.radio_value(h_scroll_modifier, WheelModifier::Ctrl, "Ctrl+Wheel");
                        });
                        if menu_item(ui, "P4Config", keybindings, FileAction::OpenUniversalPath)
                            .clicked()
                        {
                            self.open_universal_path_window = true;
                        }
                        if menu_item(
                            ui,
                            "Keyboard Shortcuts",
                            keybindings,
                            FileAction::OpenKeybindings,
                        )
                        .clicked()
                        {
                            self.open_shortcuts_window = true;
                        }
                        if ui.button("File Viewers").clicked() {
                            self.open_viewers_window = true;
                        }
                    });

                    ui.menu_button("Debug", |ui| {
                        if ui.button("Clear File Paths").clicked() {
                            file_1.set_path(UniversalPath::default());
                            file_2.set_path(UniversalPath::default());
                            diff_processor.reset_ctx();
                        }
                        if menu_item(
                            ui,
                            "Clear Cached Files",
                            keybindings,
                            FileAction::RefreshDiff,
                        )
                        .clicked()
                        {
                            diff_processor.reset_ctx();
                            Self::refresh_file_contents(file_1, file_2);
                        }
                        if menu_item(
                            ui,
                            "Clear Diff Rows",
                            keybindings,
                            FileAction::RefreshDiffRowsOnly,
                        )
                        .clicked()
                        {
                            diff_processor.reset_ctx();
                        }
                        if ui.button("Phase Trace").clicked() {
                            self.open_phase_trace_window = true;
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
                    ui_extension_map(ui, extension_map);
                },
            );
        }
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
                diff_options,
                file_1,
                file_2,
                goto_open,
                find_open,
                goto_input,
                find_query,
                find_error,
                diff_lexer_mode: lexer_mode,
                keybindings,
                myers_diff_algorithm,
                diff_processor,
                hex_processor,
                image_processor,
                viewer_kind,
                viewer_override,
                viewer_fallback,
                extension_map,
                revert_history,
                code_language,
                code_language_custom,
                p4_profiles,
                regex_ignore_presets,
            } = app_ctx;
            let is_hex = *viewer_kind == Some(ViewerKind::Hex);
            let is_image = *viewer_kind == Some(ViewerKind::Image);
            self.show_menu(
                ui,
                file_1,
                file_2,
                diff_processor,
                find_open,
                goto_open,
                scroll_left,
                scroll_right,
                lexer_mode,
                keybindings,
                p4_profiles,
                extension_map,
                myers_diff_algorithm,
                &mut diff_options.phases,
                code_language,
                code_language_custom,
                h_scroll_modifier,
            );

            ui.separator();

            let mut goto_window_open = *goto_open;
            show_custom_popup(ctx, &mut goto_window_open, "Goto", true, |ui| {
                // Hex gotos a byte offset, in decimal or 0x hex.
                if is_hex {
                    goto_input.retain(|c| c.is_ascii_hexdigit() || c == 'x' || c == 'X');
                } else {
                    goto_input.retain(|c| c.is_ascii_digit());
                }
                let response = ui.add(
                    egui::TextEdit::singleline(goto_input)
                        .desired_width(40.0)
                        .hint_text(if is_hex { "offset" } else { "#" }),
                );
                response.request_focus();
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    if is_hex {
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
            let scroll_to_rows = &if is_hex {
                hex_processor.scroll_to_row(diff_processor.conflict_cursor.get())
            } else if is_image {
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
            let diff_ctx = if is_hex || is_image {
                None
            } else {
                diff_processor.get_minimal_diff_ctx()
            };
            let mut block_toggle_request = None;
            let mut behavior = TreeBehavior {
                ctx_file_diff: FileDiffPaneCtx {
                    diff_ctx: diff_ctx.as_ref(),
                    scroll_left: scroll_left,
                    scroll_right: scroll_right,
                    h_scroll_linked,
                    word_wrap,
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
                    file_source_loading: file_1.get_loading_path().is_some(),
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
                    diff_loading: diff_processor.is_in_progress(),
                    code_language,
                    revert_request: &mut None,
                    block_toggle_request: &mut block_toggle_request,
                    hex_view: is_hex.then(|| hex_processor.view_ctx()),
                    image_view: is_image.then_some(image_processor),
                    viewer_override: &mut viewer_override.kind,
                    viewer_fallback: viewer_fallback.as_deref(),
                    phase_overlay: self.phase_overlay,
                    keymap: &keybindings.keymap,
                },
            };

            if let Some(request) = self.open_merge_form.ui(ctx) {
                self.merge = Some(MergeView::new(request, *myers_diff_algorithm));
            }
            let mut close_merge = false;
            ui.with_layout(
                Layout::left_to_right(egui::Align::Min),
                |ui| match &mut self.merge {
                    Some(merge) => {
                        close_merge = merge.ui(ui, behavior.ctx_file_diff.code_language);
                    }
                    None => self.tree.ui(&mut behavior, ui),
                },
            );
            if close_merge {
                self.merge = None;
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
                    let counts = if is_hex {
                        "hex".to_string()
                    } else if is_image {
                        "image".to_string()
                    } else {
                        format!("+{}/-{}", total_adds, total_deletes)
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
        let mut tab_pressed = false;
        let mut esc_pressed = false;
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
                            UniversalPath::Local(path_buf) => {
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
                            UniversalPath::Local(path_buf) => {
                                log::info!(
                                    "Can not open timelapse view for local path {}",
                                    path_buf.display()
                                );
                            }
                        },
                        FileAction::UndoRevert => history_step = Some(HistoryStep::Undo),
                        FileAction::Redo => history_step = Some(HistoryStep::Redo),
                        FileAction::DiffLocalFile => diff_local_file = true,
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
                    }
                }

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
            let text_conflicts = if matches!(
                app_ctx.viewer_kind,
                Some(ViewerKind::Hex | ViewerKind::Image)
            ) {
                0
            } else {
                app_ctx
                    .diff_processor
                    .get_minimal_diff_ctx()
                    .as_ref()
                    .and_then(|f| Some(f.precomputed_diffs.len()))
                    .unwrap_or_default()
            };
            let conflict_max = conflict_count(
                app_ctx.viewer_kind,
                text_conflicts,
                app_ctx.hex_processor.nav_count(),
            );
            app_ctx.diff_processor.conflict_cursor.set_max(conflict_max);
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
                state.file_1.set_lexer_mode(state.diff_lexer_mode);
                state.file_2.set_lexer_mode(state.diff_lexer_mode);

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
                    &state.extension_map,
                    loaded_1.as_ref().map(side),
                    loaded_2.as_ref().map(side),
                );
                // Decoding follows the first resolution, so a failed decode stays cached (and
                // the pair stays in Hex) instead of being retried every frame.
                if resolution.as_ref().map(|r| r.kind) == Some(ViewerKind::Image) {
                    state
                        .image_processor
                        .request(loaded_1.clone(), loaded_2.clone());
                } else {
                    // Drops the old pair's pixels and textures.
                    state.image_processor.request(None, None);
                }
                state
                    .image_processor
                    .poll(ctx.input(|i| i.max_texture_side));
                let resolution =
                    resolution.map(|r| image_decode_fallback(r, &state.image_processor.failures()));
                // Resolved every frame, so only a changed reason is logged.
                let fallback = resolution.as_ref().and_then(|r| r.fallback.clone());
                if fallback != state.viewer_fallback {
                    if let Some(reason) = &fallback {
                        log::warn!("{}", reason);
                    }
                    state.viewer_fallback = fallback;
                }
                state.viewer_kind = resolution.map(|r| r.kind);
                let is_hex = state.viewer_kind == Some(ViewerKind::Hex);
                let is_text = matches!(state.viewer_kind, Some(ViewerKind::Text) | None);
                if is_hex {
                    // The cursor may hold the text diff's or the previous pair's stop.
                    if state.hex_processor.request(loaded_1, loaded_2) {
                        state.diff_processor.conflict_cursor.set(0);
                    }
                } else {
                    // Cancels a running compare and drops the old pair's bytes.
                    state.hex_processor.request(None, None);
                }
                state.hex_processor.poll();

                let update_input = UpdateDiffRowsInput {
                    file_1: state.file_1.get_cached_file().clone(),
                    file_2: state.file_2.get_cached_file().clone(),
                    options: state.diff_options.clone(),
                    myers_diff_algorithm: state.myers_diff_algorithm.clone(),
                    pins: state.diff_processor.pins().to_vec(),
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
                    && (state.file_1.get_cached_file().is_some()
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
        LaunchRequest::Merge { left, right, .. } => {
            log::warn!("merge is not supported yet; opening left and right as a diff");
            (left, right)
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

fn menu_item(
    ui: &mut egui::Ui,
    label: &str,
    keybindings: &Keybindings,
    action: FileAction,
) -> egui::Response {
    let mut button = egui::Button::new(label);
    if let Some(text) = first_shortcut_text(&keybindings.keymap, action) {
        button = button.shortcut_text(text);
    }
    ui.add(button)
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

#[cfg(all(test, feature = "serde"))]
mod tests {
    use super::*;
    use crate::ui_egui::merge_pane::MergeRequest;
    use zdiff::ignore::IgnorePatterns;

    #[test]
    fn extension_map_edits_survive_a_restart() {
        let mut ctx = AppStateCtx::default();
        ctx.extension_map.insert(".ZBIN", ViewerKind::Hex);
        ctx.viewer_override.kind = Some(ViewerKind::Text);

        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.extension_map, ctx.extension_map);
        assert_eq!(restored.extension_map.get("zbin"), Some(ViewerKind::Hex));
        // Paths aren't saved, so a restart opens a new pair and the override is gone.
        assert_eq!(restored.viewer_override.kind, None);
    }

    #[test]
    fn a_save_without_an_extension_map_loads_the_defaults() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("extension_map")
            .unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.extension_map, ExtensionMap::defaults());
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
        assert!(!json.contains("merge"), "{json}");
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
        assert_eq!(restored.diff_options.phases, DiffPhases::default());
        assert!(restored.diff_options.ignore.whitespace);
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
        ctx.p4_profiles.profiles[0].config.password = "pw".into();
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
    }
}
