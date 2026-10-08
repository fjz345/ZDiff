use eframe::egui::{self, Layout, PointerButton, TextBuffer};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use zcommon::{hash::HashService, logger::LogBuffer, ui_egui::common::show_custom_popup};
use zhashdiff::{
    comparison::{PathComparissonMethod, compare_paths},
    external_diff_tool::{DiffPair, DiffToolConfig, LegacyDiffToolConfig, expand_template},
    filter::PathFilter,
    fs::FileSystemModel,
};

use crate::ui_egui::{
    duplicate_files_pane::DuplicateFilesPane,
    fs_tree::{FileCompareCache, FileSystemView, VisibleRowTwoFolderDiff},
    panes::{Pane, PathDiffView, TreeBehavior},
    path_diff_pane::PathDiffPane,
    tree_filter::{DiffFilter, arrange_two_folder_rows},
    tree_sort::TreeSort,
};
use eframe::epaint::Pos2;
use egui_tiles::Tile;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AppStateCtx {
    #[serde(skip)]
    pub hash_service: HashService,
    pub file_system_1_root_path: Option<PathBuf>,
    pub file_system_2_root_path: Option<PathBuf>,
    #[serde(skip)]
    pub file_system_model_1_view: Option<FileSystemView>,
    #[serde(skip)]
    pub file_system_model_2_view: Option<FileSystemView>,
    #[serde(skip)]
    two_folder_diff_visible_rows: Option<Vec<VisibleRowTwoFolderDiff>>,
    #[serde(skip)]
    two_folder_diff_compare_cache: FileCompareCache,
    /// The filter `two_folder_diff_visible_rows` were built with.
    #[serde(skip)]
    two_folder_diff_rows_filter: PathFilter,
    /// Applies to the two-folder diff rows.
    #[serde(default)]
    pub path_filter: PathFilter,
    /// The sort `two_folder_diff_visible_rows` were built with.
    #[serde(skip)]
    two_folder_diff_rows_sort: TreeSort,
    /// Sibling order of the two-folder diff rows.
    #[serde(default)]
    pub tree_sort: TreeSort,
    #[serde(skip)]
    two_folder_diff_rows_diff_filter: DiffFilter,
    #[serde(default)]
    pub diff_filter: DiffFilter,

    #[serde(skip)]
    pub active_conflict_hash: Option<String>,

    #[serde(skip)]
    conflict_map: HashMap<String, Vec<PathBuf>>,
    #[serde(skip)]
    conflict_map_resolved: HashMap<String, PathBuf>,

    diff_config: DiffToolConfig,
}

impl AppStateCtx {
    /// Rebuilds the two-folder rows when they were dropped or the filter or sort changed since
    /// they were built. Here rather than at the edit, so the rows are never missing mid-frame.
    fn build_two_folder_diff_rows_if_stale(&mut self) {
        if self.two_folder_diff_rows_filter != self.path_filter
            || self.two_folder_diff_rows_sort != self.tree_sort
            || self.two_folder_diff_rows_diff_filter != self.diff_filter
        {
            self.two_folder_diff_visible_rows = None;
        }
        if self.two_folder_diff_visible_rows.is_none()
            && (self.file_system_model_1_view.is_some() || self.file_system_model_2_view.is_some())
        {
            self.two_folder_diff_visible_rows = FileSystemView::build_two_folder_diff_rows(
                self.file_system_model_1_view.as_ref(),
                self.file_system_model_2_view.as_ref(),
                &self.path_filter,
                &mut self.two_folder_diff_compare_cache,
                |path_1, path_2| compare_paths(path_1, path_2, &PathComparissonMethod::CrC),
            )
            .ok()
            .map(|rows| arrange_two_folder_rows(rows, self.diff_filter, self.tree_sort));
            self.two_folder_diff_rows_filter = self.path_filter.clone();
            self.two_folder_diff_rows_sort = self.tree_sort;
            self.two_folder_diff_rows_diff_filter = self.diff_filter;
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
enum AppState {
    Startup(AppStateCtx),
    Idle(AppStateCtx),
    Exit(),
}

impl Default for AppState {
    fn default() -> Self {
        AppState::Startup(AppStateCtx::default())
    }
}

#[derive(Serialize, Deserialize)]
pub struct ZApp {
    #[serde(default = "default_zoom", deserialize_with = "deserialize_zoom")]
    zoom: f32,
    // Option > Hack to avoid cloning state when matching &mut self.state in update loop
    state: Option<AppState>,
    tree: egui_tiles::Tree<Pane>,
    #[serde(skip)]
    log_buffer: LogBuffer,

    open_dir_window_1: bool,
    open_dir_window_2: bool,

    diff_mode_path_diff: bool,

    open_external_diff_window: bool,
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
    pub fn request_init(&mut self) {
        // if self.state.ctx().hash_service.is_none() {
        // self.state.ctx().hash_service = HashService::default();
        // }
        self.state = self.state.take().map(|state| match state {
            AppState::Idle(ctx) => AppState::Startup(ctx),
            state => state,
        });
    }

    pub fn new(log_buffer: LogBuffer) -> Self {
        Self {
            zoom: default_zoom(),
            state: Some(AppState::default()),
            tree: Self::create_tree(),
            log_buffer: log_buffer,
            open_dir_window_1: false,
            open_dir_window_2: false,
            diff_mode_path_diff: true,
            open_external_diff_window: false,
        }
    }

    fn startup(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let visuals: egui::Visuals = egui::Visuals::dark();
        ctx.set_visuals(visuals);
        ctx.set_theme(egui::Theme::Dark);
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

        // let tile_console = tiles.insert_pane(Pane::Log(LogPane {
        //     title: Some("Log".to_string()),
        //     scroll_to_bottom: true,
        // }));

        let tile_path_diff = tiles.insert_pane(Pane::PathDiff(PathDiffPane::new(Some(
            "Path Diff".to_string(),
        ))));

        let tile_duplicate_file = tiles.insert_pane(Pane::DuplicateFiles(DuplicateFilesPane::new(
            Some("Duplicate Files".to_string()),
        )));

        // let master_tile = tiles.insert_horizontal_tile(vec![tile_duplicate_file]);
        let master_tile = tiles.insert_horizontal_tile(vec![tile_path_diff, tile_duplicate_file]);
        tabs.push(tiles.insert_vertical_tile(vec![master_tile]));

        tiles.set_visible(tile_duplicate_file, false);
        let root = tiles.insert_tab_tile(tabs);

        egui_tiles::Tree::new("my_tree", root, tiles)
    }

    fn show_menu(&mut self, ui: &mut egui::Ui, app_ctx: &mut AppStateCtx) {
        ui.horizontal(|ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.menu_button("File", |ui| {
                    if ui.button("Open Folder 1").clicked() {
                        self.open_dir_window_1 = true;
                    }
                    if ui.button("Open Folder 2").clicked() {
                        self.open_dir_window_2 = true;
                    }
                });

                ui.menu_button("Options", |ui| {
                    if ui.button("Set external diff tool").clicked() {
                        self.open_external_diff_window = true;
                    }
                    if ui.button("Setup colors").clicked() {
                        log::error!("NYI");
                    }
                    if ui.button("Set Theme").clicked() {
                        log::error!("NYI");
                    }
                });

                let diff_mode_text = if self.diff_mode_path_diff {
                    "Change to Duplicate Diff"
                } else {
                    "Change to Path Diff"
                };
                ui.menu_button("Diff Mode", |ui| {
                    if ui.button(diff_mode_text).clicked() {
                        self.diff_mode_path_diff = !self.diff_mode_path_diff;
                    }
                });
                ui.menu_button("Debug", |ui| {
                    if ui.button("Clear Root Paths").clicked() {
                        app_ctx.file_system_1_root_path = None;
                        app_ctx.file_system_2_root_path = None;
                    }
                    if ui.button("Clear FileSystemModels").clicked() {
                        app_ctx.file_system_model_1_view = None;
                        app_ctx.file_system_model_2_view = None;
                    }
                    #[cfg(debug_assertions)]
                    {
                        ui.separator();
                        for (label, p1, p2) in [
                            (
                                "Open test_similar_1 vs test_similar_2",
                                "test_similar_1",
                                "test_similar_2",
                            ),
                            (
                                "Open test_similar_2 vs test_similar_2_one_deeper",
                                "test_similar_2",
                                "test_similar_2_one_deeper",
                            ),
                            ("Open 1 vs test_similar_1/1", "1", "test_similar_1/1"),
                        ] {
                            if ui.button(label).clicked() {
                                let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                                    .join("../../test");
                                let (p1, p2) = (base.join(p1), base.join(p2));
                                if let Some(missing) = [&p1, &p2].into_iter().find(|p| !p.is_dir())
                                {
                                    log::warn!("{label}: missing fixture {}", missing.display());
                                } else {
                                    app_ctx.file_system_1_root_path = Some(p1);
                                    app_ctx.file_system_2_root_path = Some(p2);
                                }
                            }
                        }
                    }
                });
            });
        });
        if self.open_external_diff_window {
            show_custom_popup(
                ui.ctx(),
                &mut self.open_external_diff_window,
                "Option - External Diff Tool",
                true,
                |ui| {
                    ui.label("Defaults: ");
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            if ui.button("ZDiff").clicked() {
                                app_ctx.diff_config = LegacyDiffToolConfig::default_zdiff().into()
                            }
                            if ui.button("Tortoise").clicked() {
                                app_ctx.diff_config =
                                    LegacyDiffToolConfig::default_tortoise().into()
                            }
                        });
                    });

                    let default_tool = app_ctx.diff_config.default_tool.clone();
                    let Some(tool) = app_ctx.diff_config.tool_mut(&default_tool) else {
                        ui.label(format!("Default tool {default_tool:?} is missing"));
                        return;
                    };

                    let mut text_edit = tool.exe_path.to_string_lossy();
                    ui.label("Path: ");
                    ui.text_edit_singleline(&mut text_edit);
                    tool.exe_path = PathBuf::from(text_edit.as_str());

                    ui.label("Arguments ({left} {right} {left_name} {right_name}): ");
                    ui.text_edit_singleline(&mut tool.template);
                    let sample = DiffPair {
                        left: "left",
                        right: "right",
                        left_name: "left_name",
                        right_name: "right_name",
                    };
                    if let Err(err) = expand_template(&tool.template, &sample) {
                        ui.colored_label(ui.visuals().error_fg_color, err.to_string());
                    }
                },
            );
        }
    }

    fn ui(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame, app_ctx: &mut AppStateCtx) {
        // Read before any widget runs: a single-line TextEdit gives up focus on Enter without
        // consuming the key, so a later check would hand that Enter to the tree cursor.
        let keyboard_taken = ctx.wants_keyboard_input();
        egui::CentralPanel::default().show(ctx, |ui| {
            self.show_menu(ui, app_ctx);

            ui.separator();

            ui.with_layout(Layout::left_to_right(egui::Align::Min), |ui| {
                let mut diff_action_triggered = false;
                let mut path_diff_view = PathDiffView {
                    file_system_1_view: &mut app_ctx.file_system_model_1_view,
                    file_system_2_view: &mut app_ctx.file_system_model_2_view,
                    file_system_1_root_path: &mut app_ctx.file_system_1_root_path,
                    file_system_2_root_path: &mut app_ctx.file_system_2_root_path,
                    visible_rows: &mut app_ctx.two_folder_diff_visible_rows,
                };
                let mut behavior = TreeBehavior {
                    log_buffer: self.log_buffer.clone(),
                    hash_service: &mut app_ctx.hash_service,

                    active_conflict_hash: &mut app_ctx.active_conflict_hash,
                    conflict_map: &mut app_ctx.conflict_map,
                    conflict_map_resolved: &mut app_ctx.conflict_map_resolved,
                    diff_action_pressed: &mut diff_action_triggered,

                    diff_tool_config: &app_ctx.diff_config,
                    path_filter: &mut app_ctx.path_filter,
                    tree_sort: &mut app_ctx.tree_sort,
                    diff_filter: &mut app_ctx.diff_filter,
                    path_diff_view: &mut path_diff_view,
                    keyboard_taken,
                };

                self.tree.ui(&mut behavior, ui);

                for (_tile_id, tile) in self.tree.tiles.iter() {
                    if let Tile::Pane(Pane::DuplicateFiles(_file_explorer)) = tile {
                        let built_rows_len = app_ctx
                            .two_folder_diff_visible_rows
                            .as_ref()
                            .and_then(|f| Some(f.len()))
                            .unwrap_or(0);
                        let source_path =
                            app_ctx.file_system_1_root_path.clone().unwrap_or_default();
                        let target_path =
                            app_ctx.file_system_2_root_path.clone().unwrap_or_default();

                        let new_title = format!(
                            "ZHashDiff [{}] - {}, {}",
                            built_rows_len,
                            source_path.display(),
                            target_path.display()
                        );

                        ctx.send_viewport_cmd(egui::ViewportCommand::Title(new_title));

                        break;
                    }
                    if let Tile::Pane(Pane::DuplicateFiles(_file_explorer)) = tile {
                        let out_ctx = behavior.create_duplicate_files_ctx();
                        let count_files_and_folders = out_ctx
                            .path_diff_view
                            .file_system_1_view
                            .as_ref()
                            .and_then(|f| Some(f.file_system.total_files_and_folders()))
                            .unwrap_or_default();

                        let active = out_ctx.hash_service.count_active_hashes();
                        let total_pending = out_ctx.hash_service.count_hash_queue() + active;
                        let waiting = total_pending.saturating_sub(active);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                            "ZHashDiff - {} files/folders ({} active, {} queued)",
                            count_files_and_folders, active, waiting
                        )));

                        break;
                    }
                }
            });
        });
    }

    fn request_shutdown(&mut self) {
        self.state = Some(AppState::Exit());
    }

    fn process_ctx_inputs(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut user_quit: bool = false;
        {
            let _input_ctx = ctx.input(|r| {
                // Esc
                if r.key_down(egui::Key::Escape) {
                    user_quit = true;
                }

                // DoubleLeftClick
                if r.pointer.button_double_clicked(PointerButton::Primary) {
                    let mouse_pos = r.pointer.interact_pos().unwrap();
                    log::info!("double click @({},{})", mouse_pos.x, mouse_pos.y);
                }

                if r.pointer.button_clicked(PointerButton::Middle) {
                    let mouse_pos: Pos2 = r.pointer.interact_pos().unwrap();

                    log::info!("middle click @({},{})", mouse_pos.x, mouse_pos.y);
                }
            });
        }

        if user_quit {
            self.request_shutdown();
        }
    }

    // Quick hacky funtion to determine if path_diff is open or duplicate_diff is open
    fn is_path_diff(&self) -> bool {
        for (tileid, tile) in self.tree.tiles.iter() {
            if let Tile::Pane(Pane::PathDiff(_path_diff)) = tile {
                if self.tree.is_visible(*tileid) {
                    return true;
                }
            } else if let Tile::Pane(Pane::DuplicateFiles(_file_explorer)) = tile {
                if self.tree.is_visible(*tileid) {
                    return false;
                }
            }
        }
        panic!("PathDiff or DuplicateFiles should always be active");
    }
    fn set_path_diff_visible(&mut self, visible: bool) {
        let mut found_tile_id = None;
        for (tileid, tile) in self.tree.tiles.iter() {
            if let Tile::Pane(Pane::PathDiff(..)) = tile {
                found_tile_id = Some(tileid);
                break;
            }
        }
        self.tree
            .set_visible(*found_tile_id.expect("Should always find"), visible);
    }
    fn set_duplicate_files_visible(&mut self, visible: bool) {
        let mut found_tile_id = None;
        for (tileid, tile) in self.tree.tiles.iter() {
            if let Tile::Pane(Pane::DuplicateFiles(..)) = tile {
                found_tile_id = Some(tileid);
                break;
            }
        }
        self.tree
            .set_visible(*found_tile_id.expect("Should always find"), visible);
    }
}

impl eframe::App for ZApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        #[cfg(feature = "serde")]
        if let Ok(json) = serde_json::to_string(self) {
            storage.set_string(eframe::APP_KEY, json);
            log::debug!("App state saved");
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let mut app_state = self
            .state
            .take()
            .expect("state should be valid before and after update()");
        app_state = match app_state {
            AppState::Startup(state_ctx) => {
                self.startup(ctx, frame);
                log::info!("STARTUP COMPLETE, TRANSITIONING TO IDLE");
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label("Loading...");
                    });
                });
                AppState::Idle(state_ctx)
            }
            AppState::Idle(mut state) => {
                self.sync_zoom(ctx);
                if self.is_path_diff() != self.diff_mode_path_diff {
                    if self.is_path_diff() {
                        self.set_path_diff_visible(false);
                        self.set_duplicate_files_visible(true);
                    } else {
                        self.set_duplicate_files_visible(false);
                        self.set_path_diff_visible(true);
                    }
                }
                // Handle folder dialogs
                if self.open_dir_window_1 {
                    self.open_dir_window_1 = false;
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        state.file_system_1_root_path = Some(path);
                    }
                }
                if self.open_dir_window_2 {
                    self.open_dir_window_2 = false;
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        state.file_system_2_root_path = Some(path);
                    }
                }
                // Invalidate state
                if let Some(path) = &state.file_system_1_root_path {
                    let needs_reload = state.file_system_model_1_view.as_ref().map_or(true, |v| {
                        v.file_system.get_root().as_path().as_ref() != path
                    });
                    if needs_reload {
                        match FileSystemModel::new(path) {
                            Ok(new_model) => {
                                state.file_system_model_1_view =
                                    Some(FileSystemView::new(Arc::new(new_model)));
                                state.two_folder_diff_visible_rows = None;
                            }
                            Err(e) => {
                                log::error!("{e}");
                                state.file_system_1_root_path = None;
                            }
                        }
                    }
                } else {
                    state.file_system_model_1_view = None;
                    state.two_folder_diff_visible_rows = None;
                }
                if let Some(path) = &state.file_system_2_root_path {
                    let needs_reload = state.file_system_model_2_view.as_ref().map_or(true, |v| {
                        v.file_system.get_root().as_path().as_ref() != path
                    });
                    if needs_reload {
                        match FileSystemModel::new(path) {
                            Ok(new_model) => {
                                state.file_system_model_2_view =
                                    Some(FileSystemView::new(Arc::new(new_model)));
                                state.two_folder_diff_visible_rows = None;
                            }
                            Err(e) => {
                                log::error!("{e}");
                                state.file_system_2_root_path = None;
                            }
                        }
                    }
                } else {
                    state.file_system_model_2_view = None;
                    state.two_folder_diff_visible_rows = None;
                }
                state.build_two_folder_diff_rows_if_stale();

                self.ui(ctx, frame, &mut state);

                self.process_ctx_inputs(ctx, frame);
                AppState::Idle(state)
            }
            AppState::Exit() => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                log::trace!("send_viewport_cmd sent");

                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label("Exiting...");
                    });
                });
                AppState::Exit()
            }
        };
        self.state = Some(app_state);
    }
}

#[cfg(test)]
mod tests {
    use super::{AppState, AppStateCtx, ZApp};
    use crate::ui_egui::fs_tree::FileSystemView;
    use crate::ui_egui::tree_filter::DiffFilter;
    use crate::ui_egui::tree_sort::{SortKey, TreeSort};
    use std::{fs, path::Path, sync::Arc};
    use tempfile::tempdir;
    use zcommon::logger::LogBuffer;
    use zhashdiff::{
        filter::{PathFilter, PatternList},
        fs::FileSystemModel,
    };

    fn blacklist(text: &str) -> PathFilter {
        PathFilter {
            blacklist: PatternList::new(text),
            ..Default::default()
        }
    }

    fn load_view(root: &Path) -> Option<FileSystemView> {
        Some(FileSystemView::new(Arc::new(
            FileSystemModel::new(root).unwrap(),
        )))
    }

    fn row_paths(state: &AppStateCtx) -> Vec<&str> {
        let rows = state.two_folder_diff_visible_rows.as_ref().unwrap();
        rows.iter().map(|r| r.rel_path.as_str()).collect()
    }

    #[test]
    fn the_filter_survives_a_restart() {
        let mut state = AppStateCtx::default();
        state.path_filter = blacklist("*.obj, target/");

        let json = serde_json::to_string(&state).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.path_filter, state.path_filter);
        assert!(restored.path_filter.is_blacklisted("a.OBJ", false));
        assert!(restored.path_filter.is_blacklisted("target", true));
    }

    #[test]
    fn a_state_saved_before_the_filter_existed_loads_with_an_empty_filter() {
        let mut json: serde_json::Value = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut().unwrap().remove("path_filter").unwrap();

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();

        assert_eq!(restored.path_filter, PathFilter::default());
    }

    #[test]
    fn editing_the_filter_rebuilds_the_rows_on_the_next_update() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            fs::write(root.join("a.txt"), "a").unwrap();
            fs::write(root.join("b.obj"), "b").unwrap();
        }
        let mut state = AppStateCtx::default();
        state.file_system_model_1_view = load_view(left.path());
        state.file_system_model_2_view = load_view(right.path());

        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "a.txt", "b.obj"]);

        state.path_filter = blacklist("*.obj");
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "a.txt"]);

        state.path_filter = PathFilter::default();
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "a.txt", "b.obj"]);
    }

    #[test]
    fn zoom_survives_a_restart() {
        let mut app = ZApp::new(LogBuffer::default());
        app.zoom = 1.7;
        let json = serde_json::to_string(&app).unwrap();
        let restored: ZApp = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.zoom, 1.7);
    }

    #[test]
    fn a_saved_zoom_out_of_range_loads_clamped() {
        let mut json = serde_json::to_value(ZApp::new(LogBuffer::default())).unwrap();
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
        let mut json = serde_json::to_value(ZApp::new(LogBuffer::default())).unwrap();
        json.as_object_mut().unwrap().remove("zoom").unwrap();
        let restored: ZApp = serde_json::from_value(json).unwrap();
        assert_eq!(restored.zoom, 1.0);
    }

    #[test]
    fn a_restored_idle_app_runs_startup_again() {
        let mut app = ZApp::new(LogBuffer::default());
        app.state = Some(AppState::Idle(AppStateCtx::default()));
        app.request_init();
        assert!(matches!(app.state, Some(AppState::Startup(_))));
    }

    #[test]
    fn the_sort_survives_a_restart() {
        let mut state = AppStateCtx::default();
        state.tree_sort = TreeSort::default()
            .clicked(SortKey::State)
            .clicked(SortKey::State);
        state.tree_sort.flat = true;

        let json = serde_json::to_string(&state).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();

        assert_eq!(
            restored.tree_sort,
            TreeSort {
                key: SortKey::State,
                descending: true,
                flat: true,
            }
        );
    }

    #[test]
    fn a_state_saved_before_the_sort_existed_loads_sorted_by_name_ascending() {
        let mut json: serde_json::Value = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut().unwrap().remove("tree_sort").unwrap();

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();

        assert_eq!(
            restored.tree_sort,
            TreeSort {
                key: SortKey::Name,
                descending: false,
                flat: false,
            }
        );
    }

    #[test]
    fn diff_only_and_show_parents_survive_a_restart() {
        let mut state = AppStateCtx::default();
        state.diff_filter = DiffFilter {
            diff_only: true,
            show_parents: false,
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.diff_filter, state.diff_filter);
    }

    #[test]
    fn a_state_saved_before_diff_only_existed_shows_every_row_with_parents() {
        let mut json: serde_json::Value = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut().unwrap().remove("diff_filter").unwrap();

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();

        assert_eq!(
            restored.diff_filter,
            DiffFilter {
                diff_only: false,
                show_parents: true,
            }
        );
    }

    #[test]
    fn toggling_diff_only_and_show_parents_rebuilds_the_rows_on_the_next_update() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            fs::create_dir(root.join("z")).unwrap();
            fs::write(root.join("a.txt"), "a").unwrap();
        }
        fs::write(left.path().join("z/b.txt"), "left b").unwrap();
        fs::write(right.path().join("z/b.txt"), "right b").unwrap();
        let mut state = AppStateCtx::default();
        state.file_system_model_1_view = load_view(left.path());
        state.file_system_model_2_view = load_view(right.path());

        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "z", "z/b.txt", "a.txt"]);

        state.diff_filter.diff_only = true;
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "z", "z/b.txt"]);

        state.diff_filter.show_parents = false;
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "z/b.txt"]);

        state.diff_filter.diff_only = false;
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "z", "z/b.txt", "a.txt"]);
    }

    #[test]
    fn rows_are_sorted_and_changing_the_sort_reorders_them_on_the_next_update() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            fs::create_dir(root.join("z")).unwrap();
            fs::write(root.join("z/c.txt"), "c").unwrap();
            fs::write(root.join("a.txt"), "a").unwrap();
        }
        fs::write(left.path().join("b.txt"), "left b").unwrap();
        fs::write(right.path().join("b.txt"), "right b").unwrap();
        let mut state = AppStateCtx::default();
        state.file_system_model_1_view = load_view(left.path());
        state.file_system_model_2_view = load_view(right.path());

        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "z", "z/c.txt", "a.txt", "b.txt"]);

        state.tree_sort = state.tree_sort.clicked(SortKey::Name);
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "z", "z/c.txt", "b.txt", "a.txt"]);

        state.tree_sort = state.tree_sort.clicked(SortKey::State);
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "z", "z/c.txt", "b.txt", "a.txt"]);

        state.tree_sort = state.tree_sort.clicked(SortKey::State);
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "z", "z/c.txt", "a.txt", "b.txt"]);
    }
}
