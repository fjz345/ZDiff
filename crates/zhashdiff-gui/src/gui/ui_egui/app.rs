use eframe::egui::{self, Layout, PointerButton};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use zcommon::{
    find::{FindError, Matcher},
    hash::HashService,
    kinds::KindTable,
    logger::LogBuffer,
    ui_egui::{
        find_popup::{FindPopupEvent, find_popup},
        keymap::Keymap,
        style_window::StyleWindow,
    },
    universal_path::UniversalPath,
    vcs::{git::GitCommand, p4::P4Command},
};
use zhashdiff::{
    external_diff_tool::DiffToolConfig,
    filter::PathFilter,
    fs::{FileSystemModel, FsNodeId},
    scan::ScanOutput,
    uasset::BlueprintDetector,
    vcs_source::scan_side,
};

use crate::ui_egui::{
    duplicate_files_pane::DuplicateFilesPane,
    folder_keymap::{FolderAction, KeybindingsWindow},
    fs_tree::{
        FileSystemView, FolderRole, HScrollLink, RowsId, VisibleRowTwoFolderDiff,
        apply_blueprint_results,
    },
    panes::{Pane, PathDiffView, TreeBehavior},
    path_diff_pane::{FilterPresets, PathDiffPane},
    roots::{LaunchArgs, Side, apply_launch_args, clear_root, set_root},
    status_line::{ScanSummary, ToolMessage, ui_status_summary, ui_tool_message},
    tools_window::ToolsWindow,
    tree_compare::{CompareUpdate, PairLikeness, TreeCompare, patch_row_states},
    tree_filter::{DiffFilter, DuplicatesFilter, arrange_two_folder_rows},
    tree_find::{FindChipEvent, FolderFind, TreeFindView, find_mask},
    tree_sort::TreeSort,
    tree_tolerance::Tolerance,
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
    file_system_1_scan: SideScan,
    #[serde(skip)]
    file_system_2_scan: SideScan,
    #[serde(skip)]
    two_folder_diff_visible_rows: Option<Vec<VisibleRowTwoFolderDiff>>,
    #[serde(skip)]
    two_folder_diff_rows_id: RowsId,
    /// The visible rows before find narrows them. Current only while the visible rows are set.
    #[serde(skip)]
    two_folder_diff_arranged_rows: Option<Vec<VisibleRowTwoFolderDiff>>,
    #[serde(skip)]
    tree_compare: TreeCompare,
    #[serde(skip)]
    blueprint_detector: BlueprintDetector,
    /// The filter `two_folder_diff_visible_rows` were built with.
    #[serde(skip)]
    two_folder_diff_rows_filter: PathFilter,
    /// Applies to the two-folder diff rows.
    #[serde(default)]
    pub path_filter: PathFilter,
    #[serde(default)]
    pub filter_presets: FilterPresets,
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
    #[serde(default)]
    pub duplicates_filter: DuplicatesFilter,
    #[serde(default)]
    pub likeness_bar: bool,
    #[serde(skip)]
    two_folder_diff_rows_tolerance: Tolerance,
    #[serde(default)]
    pub tolerance: Tolerance,
    #[serde(skip)]
    folder_find: FolderFind,
    /// The last valid `folder_find`: an invalid regex keeps the filter it replaces.
    #[serde(skip)]
    find_applied: FolderFind,
    #[serde(skip)]
    find_matcher: Option<Matcher>,
    #[serde(skip)]
    find_error: Option<String>,
    #[serde(skip)]
    two_folder_diff_rows_find: FolderFind,
    #[serde(skip)]
    find_matches: Option<usize>,
    #[serde(default)]
    pub h_scroll_link: HScrollLink,

    #[serde(skip)]
    conflict_map: HashMap<String, Vec<PathBuf>>,
    #[serde(skip)]
    conflict_map_resolved: HashMap<String, PathBuf>,

    diff_config: DiffToolConfig,
    #[serde(skip)]
    tool_message: Option<ToolMessage>,
    #[serde(default)]
    keymap: Keymap<FolderAction>,
}

impl AppStateCtx {
    /// Rebuilds the two-folder rows when they were dropped or the filter or sort changed since
    /// they were built. Here rather than at the edit, so the rows are never missing mid-frame.
    fn build_two_folder_diff_rows_if_stale(&mut self) {
        self.tree_compare.retain_for([
            self.file_system_model_1_view.as_ref(),
            self.file_system_model_2_view.as_ref(),
        ]);
        let measure_likeness = self.likeness_bar || self.tolerance.uses_likeness();
        let likeness_changed = self.tree_compare.set_likeness(measure_likeness);
        if self.two_folder_diff_rows_filter != self.path_filter
            || self.two_folder_diff_rows_sort != self.tree_sort
            || self.two_folder_diff_rows_diff_filter != self.diff_filter
            || self.two_folder_diff_rows_tolerance != self.tolerance
        {
            self.two_folder_diff_visible_rows = None;
        }
        if self.two_folder_diff_visible_rows.is_none() {
            if self.file_system_model_1_view.is_some() || self.file_system_model_2_view.is_some() {
                self.two_folder_diff_arranged_rows = self
                    .build_rows(true)
                    .map(|rows| arrange_two_folder_rows(rows, self.diff_filter, self.tree_sort));
                // Before find: the compare covers the pairs find hides, so find never restarts it.
                if let Some(rows) = &self.two_folder_diff_arranged_rows {
                    self.tree_compare.start(
                        rows,
                        [
                            self.file_system_model_1_view.as_ref(),
                            self.file_system_model_2_view.as_ref(),
                        ],
                        Instant::now(),
                    );
                }
                self.narrow_rows_to_find();
                self.two_folder_diff_rows_filter = self.path_filter.clone();
                self.two_folder_diff_rows_sort = self.tree_sort;
                self.two_folder_diff_rows_diff_filter = self.diff_filter;
                self.two_folder_diff_rows_tolerance = self.tolerance;
                log::info!(
                    "Two-folder rows rebuilt: {} rows",
                    self.two_folder_diff_visible_rows
                        .as_ref()
                        .map_or(0, Vec::len)
                );
            }
            return;
        }
        if self.two_folder_diff_rows_find != self.find_applied {
            self.narrow_rows_to_find();
        }
        if likeness_changed
            && measure_likeness
            && let Some(rows) = &self.two_folder_diff_arranged_rows
        {
            self.tree_compare.start(
                rows,
                [
                    self.file_system_model_1_view.as_ref(),
                    self.file_system_model_2_view.as_ref(),
                ],
                Instant::now(),
            );
        }
    }

    fn narrow_rows_to_find(&mut self) {
        let flat = self.diff_filter.is_flat(self.tree_sort);
        let (rows, matches) = self
            .two_folder_diff_arranged_rows
            .as_deref()
            .map(|rows| narrow_to_find(rows, &self.find_applied, flat))
            .unzip();
        self.two_folder_diff_visible_rows = rows;
        self.two_folder_diff_rows_id = RowsId::next();
        self.find_matches = matches.flatten();
        self.two_folder_diff_rows_find = self.find_applied.clone();
    }

    fn apply_find(&mut self) {
        let matcher = match Matcher::new(&self.folder_find.query) {
            Ok(matcher) => Some(matcher),
            Err(FindError::EmptyQuery) => None,
            Err(e) => {
                self.find_error = Some(e.to_string());
                return;
            }
        };
        self.find_error = None;
        self.find_applied = self.folder_find.clone();
        self.find_matcher = matcher;
    }

    /// Reads no file: pairs without a known state are Pending.
    fn build_rows(&mut self, check_blueprints: bool) -> Option<Vec<VisibleRowTwoFolderDiff>> {
        let compare = &self.tree_compare;
        let detector = &mut self.blueprint_detector;
        FileSystemView::build_two_folder_diff_rows(
            self.file_system_model_1_view.as_ref(),
            self.file_system_model_2_view.as_ref(),
            &self.path_filter,
            &KindTable::default(),
            &self.tolerance,
            |rel_path, nodes| {
                (
                    compare.state_of(rel_path, nodes),
                    compare
                        .likeness_of(rel_path, nodes)
                        .and_then(PairLikeness::ratio),
                )
            },
            |path, meta| {
                check_blueprints
                    .then(|| detector.check(path, meta))
                    .flatten()
            },
        )
        .ok()
    }

    /// While a compare runs the rows keep their order and only take the new states, so a state
    /// sort doesn't move them; the finished compare rebuilds them sorted.
    fn update_compare(&mut self, now: Instant) {
        self.tree_compare.retain_for([
            self.file_system_model_1_view.as_ref(),
            self.file_system_model_2_view.as_ref(),
        ]);
        match self.tree_compare.poll(now) {
            CompareUpdate::Idle => {}
            CompareUpdate::Patch => {
                if self.two_folder_diff_visible_rows.is_some()
                    && let Some(fresh) = self.build_rows(false)
                    && let Some(rows) = self.two_folder_diff_arranged_rows.as_mut()
                {
                    patch_row_states(rows, fresh);
                    self.narrow_rows_to_find();
                }
            }
            CompareUpdate::Finished => self.two_folder_diff_visible_rows = None,
        }
    }

    /// True while uasset checks are still pending.
    fn poll_blueprint_detector(&mut self) -> bool {
        let results = self.blueprint_detector.poll();
        if self.two_folder_diff_visible_rows.is_some()
            && let Some(rows) = self.two_folder_diff_arranged_rows.as_mut()
            && !results.is_empty()
        {
            let root = |view: &Option<FileSystemView>| {
                view.as_ref()
                    .map(|v| v.file_system.get_root().as_path().as_ref().to_path_buf())
            };
            let roots = [
                root(&self.file_system_model_1_view),
                root(&self.file_system_model_2_view),
            ];
            apply_blueprint_results(rows, [roots[0].as_deref(), roots[1].as_deref()], &results);
            self.narrow_rows_to_find();
        }
        self.blueprint_detector.pending() > 0
    }

    fn refresh(&mut self) {
        // A running scan keeps its flag (ScanAction::Keep), so it would scan again when done.
        for scan in [&mut self.file_system_1_scan, &mut self.file_system_2_scan] {
            if !scan.is_scanning() {
                scan.request_rescan();
            }
        }
    }

    fn update_scans(&mut self, repaint: &egui::Context) {
        update_side_scan(
            Side::Folder1,
            &mut self.file_system_1_root_path,
            &mut self.file_system_model_1_view,
            &mut self.file_system_1_scan,
            &mut self.two_folder_diff_visible_rows,
            repaint,
        );
        update_side_scan(
            Side::Folder2,
            &mut self.file_system_2_root_path,
            &mut self.file_system_model_2_view,
            &mut self.file_system_2_scan,
            &mut self.two_folder_diff_visible_rows,
            repaint,
        );
    }
}

/// The rows `find` shows and its match count; all rows and None without a query.
fn narrow_to_find(
    rows: &[VisibleRowTwoFolderDiff],
    find: &FolderFind,
    flat: bool,
) -> (Vec<VisibleRowTwoFolderDiff>, Option<usize>) {
    let Ok(matcher) = Matcher::new(&find.query) else {
        return (rows.to_vec(), None);
    };
    let mask = find_mask(rows, &matcher, find, flat);
    let rows = rows
        .iter()
        .zip(mask.hidden)
        .filter(|(_, hidden)| !hidden)
        .map(|(row, _)| row.clone())
        .collect();
    (rows, Some(mask.matches))
}

/// One background scan of a side's root. Dropping it cancels the scan.
#[derive(Debug)]
pub struct ScanJob {
    root: PathBuf,
    cancel: Arc<AtomicBool>,
    started: Instant,
    found: Arc<AtomicUsize>,
    result: mpsc::Receiver<Option<io::Result<ScanOutput>>>,
}

impl ScanJob {
    fn start(root: PathBuf, repaint: egui::Context) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let found = Arc::new(AtomicUsize::new(0));
        let (sender, result) = mpsc::channel();
        let (thread_root, thread_cancel, thread_found) =
            (root.clone(), cancel.clone(), found.clone());
        std::thread::Builder::new()
            .name("folder scan".into())
            .spawn(move || {
                let p4 = P4Command::new(false).for_file(UniversalPath::from(thread_root.as_path()));
                let output = scan_side(
                    &GitCommand::default(),
                    &p4,
                    &thread_root,
                    &thread_cancel,
                    &thread_found,
                );
                // The job was dropped when the receiver is gone; nobody wants the result.
                let _ = sender.send(output);
                repaint.request_repaint();
            })
            .expect("failed to spawn the folder scan thread");
        Self {
            root,
            started: Instant::now(),
            cancel,
            found,
            result,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn found(&self) -> usize {
        self.found.load(Ordering::Relaxed)
    }
}

impl Drop for ScanJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug, Default)]
pub struct SideScan {
    job: Option<ScanJob>,
    rescan: bool,
    shown: Option<ScanSummary>,
}

impl SideScan {
    pub fn request_rescan(&mut self) {
        self.rescan = true;
    }

    pub fn job(&self) -> Option<&ScanJob> {
        self.job.as_ref()
    }

    pub fn is_scanning(&self) -> bool {
        self.job.is_some()
    }

    pub fn shown(&self) -> Option<&ScanSummary> {
        self.shown.as_ref()
    }
}

#[derive(Debug, PartialEq)]
enum ScanAction {
    Keep,
    Start(PathBuf),
    Stop,
    Clear,
}

fn scan_action(
    root: Option<&Path>,
    shown: Option<&Path>,
    job: Option<&Path>,
    rescan: bool,
) -> ScanAction {
    let Some(root) = root else {
        return ScanAction::Clear;
    };
    if job == Some(root) {
        ScanAction::Keep
    } else if shown == Some(root) && !rescan {
        ScanAction::Stop
    } else {
        ScanAction::Start(root.to_path_buf())
    }
}

fn shown_root(view: Option<&FileSystemView>) -> Option<PathBuf> {
    view.map(|view| view.file_system.get_root().as_path().as_ref().to_path_buf())
}

/// A rescan of the shown root keeps the collapsed state of every path still present.
fn install_scanned(old: Option<&FileSystemView>, model: FileSystemModel) -> FileSystemView {
    let mut view = FileSystemView::new(Arc::new(model));
    let Some(old) = old.filter(|old| shown_root(Some(old)) == shown_root(Some(&view))) else {
        return view;
    };
    let node_path = |model: &FileSystemModel, id: FsNodeId| {
        model
            .get_node(id)
            .expect("iter_nodes yields valid node ids")
            .as_path()
            .as_ref()
            .to_path_buf()
    };
    let old_ids: HashMap<PathBuf, FsNodeId> = old
        .file_system
        .iter_nodes()
        .map(|id| (node_path(&old.file_system, id), id))
        .collect();
    for id in view.file_system.iter_nodes() {
        let Some(old_id) = old_ids.get(&node_path(&view.file_system, id)) else {
            continue;
        };
        if let Some(&collapsed) = old.collapsed.get(old_id) {
            view.collapsed.insert(id, collapsed);
        }
    }
    view
}

/// Back to the root of the shown view, so the next frame does not scan the abandoned root again.
fn revert_root(side: Side, root: &mut Option<PathBuf>, view: Option<&FileSystemView>) {
    match shown_root(view) {
        Some(shown) => {
            if set_root(root, side, &shown).is_err() {
                clear_root(root);
            }
        }
        None => clear_root(root),
    }
}

pub(crate) fn update_side_scan(
    side: Side,
    root: &mut Option<PathBuf>,
    view: &mut Option<FileSystemView>,
    scan: &mut SideScan,
    rows: &mut Option<Vec<VisibleRowTwoFolderDiff>>,
    repaint: &egui::Context,
) {
    let action = scan_action(
        root.as_deref(),
        shown_root(view.as_ref()).as_deref(),
        scan.job.as_ref().map(ScanJob::root),
        scan.rescan,
    );
    match action {
        ScanAction::Keep => {}
        ScanAction::Start(path) => {
            scan.job = Some(ScanJob::start(path, repaint.clone()));
            scan.rescan = false;
            return;
        }
        ScanAction::Stop => scan.job = None,
        ScanAction::Clear => {
            scan.job = None;
            scan.rescan = false;
            scan.shown = None;
            if view.take().is_some() {
                *rows = None;
            }
        }
    }

    let Some(job) = &scan.job else {
        return;
    };
    let result = match job.result.try_recv() {
        Ok(result) => result,
        Err(mpsc::TryRecvError::Empty) => return,
        Err(mpsc::TryRecvError::Disconnected) => {
            log::error!(
                "{side}: the scan of {} stopped without a result",
                job.root.display()
            );
            scan.job = None;
            revert_root(side, root, view.as_ref());
            return;
        }
    };
    let job = scan.job.take().expect("checked above");
    match result {
        Some(Ok(output)) => {
            if !output.skipped.is_empty() {
                log::warn!("{side}: {} entries skipped", output.skipped.len());
            }
            scan.shown = Some(ScanSummary {
                elapsed: job.started.elapsed(),
                skipped: output.skipped,
            });
            *view = Some(install_scanned(view.as_ref(), output.model));
            *rows = None;
        }
        Some(Err(e)) => {
            log::error!("{side}: {}: {e}", job.root.display());
            revert_root(side, root, view.as_ref());
        }
        None => {
            log::info!("{side}: scan of {} cancelled", job.root.display());
            revert_root(side, root, view.as_ref());
        }
    }
}

/// Keeps the previous view, or the empty state when there was none.
pub(crate) fn cancel_scan(
    side: Side,
    root: &mut Option<PathBuf>,
    view: Option<&FileSystemView>,
    scan: &mut SideScan,
) {
    if let Some(job) = scan.job.take() {
        log::info!("{side}: scan of {} cancelled", job.root.display());
    }
    scan.rescan = false;
    revert_root(side, root, view);
}

fn ui_scan_progress(
    ui: &mut egui::Ui,
    side: Side,
    root: &mut Option<PathBuf>,
    view: Option<&FileSystemView>,
    scan: &mut SideScan,
) {
    let Some(job) = scan.job() else {
        return;
    };
    let progress = format!(
        "Scanning {side}: {} entries in {}",
        job.found(),
        job.root().display()
    );
    let cancelled = ui
        .horizontal(|ui| {
            ui.spinner();
            ui.label(progress);
            ui.button("Cancel").clicked()
        })
        .inner;
    if cancelled {
        cancel_scan(side, root, view, scan);
    }
}

fn ui_compare_progress(ui: &mut egui::Ui, compare: &mut TreeCompare) {
    let Some((compared, total)) = compare.progress() else {
        return;
    };
    let cancelled = ui
        .horizontal(|ui| {
            ui.spinner();
            ui.label(format!("Comparing: {compared} of {total} file pairs"));
            ui.button("Cancel").clicked()
        })
        .inner;
    if cancelled {
        compare.cancel();
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
pub(crate) fn finish_side_scan(
    side: Side,
    root: &mut Option<PathBuf>,
    view: &mut Option<FileSystemView>,
    scan: &mut SideScan,
    rows: &mut Option<Vec<VisibleRowTwoFolderDiff>>,
) {
    let repaint = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        update_side_scan(side, root, view, scan, rows, &repaint);
        if !scan.is_scanning() {
            return;
        }
        assert!(Instant::now() < deadline, "{side} scan did not finish");
        std::thread::sleep(Duration::from_millis(1));
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

    #[serde(skip)]
    tools_window: ToolsWindow,
    #[serde(skip)]
    keybindings_window: KeybindingsWindow,
    #[serde(skip)]
    style_window: StyleWindow,
    #[serde(skip)]
    find_open: bool,
}

const LONG_FRAME: Duration = Duration::from_millis(200);
const SCAN_PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

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

    pub fn apply_launch_args(&mut self, args: &LaunchArgs) {
        match self.state.as_mut().expect("state is set outside update()") {
            AppState::Startup(ctx) | AppState::Idle(ctx) => apply_launch_args(
                args,
                &mut ctx.file_system_1_root_path,
                &mut ctx.file_system_2_root_path,
            ),
            AppState::Exit() => log::warn!("Command-line folders ignored: the app is exiting"),
        }
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
            tools_window: ToolsWindow::default(),
            keybindings_window: KeybindingsWindow::default(),
            style_window: StyleWindow::default(),
            find_open: false,
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
                    if ui.button("External Diff Tools").clicked() {
                        self.tools_window.open = true;
                    }
                    if ui.button("Keybindings").clicked() {
                        self.keybindings_window.open = true;
                    }
                    if ui.button("Style").clicked() {
                        self.style_window.open(ui.ctx());
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
                        clear_root(&mut app_ctx.file_system_1_root_path);
                        clear_root(&mut app_ctx.file_system_2_root_path);
                    }
                    if ui.button("Clear FileSystemModels").clicked() {
                        app_ctx.file_system_model_1_view = None;
                        app_ctx.file_system_model_2_view = None;
                        app_ctx.two_folder_diff_visible_rows = None;
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
                                    let _ = set_root(
                                        &mut app_ctx.file_system_1_root_path,
                                        Side::Folder1,
                                        &p1,
                                    );
                                    let _ = set_root(
                                        &mut app_ctx.file_system_2_root_path,
                                        Side::Folder2,
                                        &p2,
                                    );
                                }
                            }
                        }
                    }
                });
            });

            let shown = app_ctx.file_system_model_1_view.is_some()
                || app_ctx.file_system_model_2_view.is_some();
            let mut refresh = ui.add_enabled(shown, egui::Button::new("Refresh"));
            if let Some(shortcut) = app_ctx.keymap.shortcuts(FolderAction::Refresh).first() {
                refresh = refresh.on_hover_text(shortcut.format());
            }
            if refresh.clicked() {
                app_ctx.refresh();
                ui.ctx().request_repaint();
            }
        });
        if self.tools_window.open {
            self.tools_window.show(ui.ctx(), &mut app_ctx.diff_config);
        }
        if self.keybindings_window.open {
            self.keybindings_window.show(ui.ctx(), &mut app_ctx.keymap);
        }
        self.style_window.show::<FolderRole>(ui.ctx(), "Folder");
    }

    fn ui(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame, app_ctx: &mut AppStateCtx) {
        // Read before any widget runs: a single-line TextEdit gives up focus on Enter without
        // consuming the key, so a later check would hand that Enter to the tree cursor.
        let capturing = self.keybindings_window.is_capturing();
        let keyboard_taken = ctx.wants_keyboard_input() || capturing;
        let actions = if capturing {
            Vec::new()
        } else {
            ctx.input(|i| app_ctx.keymap.dispatch(&i.events, keyboard_taken))
        };
        if actions.contains(&FolderAction::OpenKeybindings) {
            self.keybindings_window.open = true;
        }
        if actions.contains(&FolderAction::Refresh) {
            app_ctx.refresh();
            ctx.request_repaint();
        }
        if actions.contains(&FolderAction::Find) && self.diff_mode_path_diff {
            self.find_open = true;
        }
        if !self.diff_mode_path_diff {
            self.find_open = false;
        }
        if self.find_open {
            self.show_find(ctx, app_ctx);
        }
        let working = app_ctx.file_system_1_scan.is_scanning()
            || app_ctx.file_system_2_scan.is_scanning()
            || app_ctx.tree_compare.is_comparing();
        let shown = app_ctx.file_system_model_1_view.is_some()
            || app_ctx.file_system_model_2_view.is_some();
        if working {
            egui::TopBottomPanel::bottom("status_line").show(ctx, |ui| {
                ui_scan_progress(
                    ui,
                    Side::Folder1,
                    &mut app_ctx.file_system_1_root_path,
                    app_ctx.file_system_model_1_view.as_ref(),
                    &mut app_ctx.file_system_1_scan,
                );
                ui_scan_progress(
                    ui,
                    Side::Folder2,
                    &mut app_ctx.file_system_2_root_path,
                    app_ctx.file_system_model_2_view.as_ref(),
                    &mut app_ctx.file_system_2_scan,
                );
                ui_compare_progress(ui, &mut app_ctx.tree_compare);
                ui_tool_message(ui, app_ctx.tool_message.as_ref());
            });
        } else if shown {
            egui::TopBottomPanel::bottom("status_line").show(ctx, |ui| {
                ui_status_summary(
                    ui,
                    app_ctx.two_folder_diff_visible_rows.as_deref(),
                    [
                        app_ctx.file_system_1_scan.shown(),
                        app_ctx.file_system_2_scan.shown(),
                    ],
                    app_ctx.tree_compare.compare_time(),
                    app_ctx.tolerance.enabled,
                );
                ui_tool_message(ui, app_ctx.tool_message.as_ref());
            });
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            self.show_menu(ui, app_ctx);

            ui.separator();

            ui.with_layout(Layout::left_to_right(egui::Align::Min), |ui| {
                let mut diff_action_triggered = false;
                let mut path_diff_view = PathDiffView {
                    file_system_1_view: &mut app_ctx.file_system_model_1_view,
                    file_system_2_view: &mut app_ctx.file_system_model_2_view,
                    file_system_1_scan: &mut app_ctx.file_system_1_scan,
                    file_system_1_root_path: &mut app_ctx.file_system_1_root_path,
                    file_system_2_root_path: &mut app_ctx.file_system_2_root_path,
                    visible_rows: &mut app_ctx.two_folder_diff_visible_rows,
                    rows_id: app_ctx.two_folder_diff_rows_id,
                };
                let mut find_view = TreeFindView {
                    matcher: app_ctx.find_matcher.as_ref(),
                    match_path: app_ctx.find_applied.match_path,
                    chip: (!self.find_open && app_ctx.find_matcher.is_some())
                        .then_some(app_ctx.find_applied.query.text.as_str()),
                    chip_event: None,
                };
                let mut behavior = TreeBehavior {
                    log_buffer: self.log_buffer.clone(),
                    hash_service: &mut app_ctx.hash_service,

                    conflict_map: &mut app_ctx.conflict_map,
                    conflict_map_resolved: &mut app_ctx.conflict_map_resolved,
                    diff_action_pressed: &mut diff_action_triggered,

                    diff_tool_config: &app_ctx.diff_config,
                    tool_message: &mut app_ctx.tool_message,
                    path_filter: &mut app_ctx.path_filter,
                    filter_presets: &mut app_ctx.filter_presets,
                    tree_sort: &mut app_ctx.tree_sort,
                    diff_filter: &mut app_ctx.diff_filter,
                    duplicates_filter: &mut app_ctx.duplicates_filter,
                    likeness_bar: &mut app_ctx.likeness_bar,
                    tolerance: &mut app_ctx.tolerance,
                    tree_compare: &app_ctx.tree_compare,
                    h_scroll_link: &mut app_ctx.h_scroll_link,
                    find: &mut find_view,
                    path_diff_view: &mut path_diff_view,
                    keyboard_taken,
                    actions: &actions,
                };

                self.tree.ui(&mut behavior, ui);
                let chip_event = behavior.find.chip_event;

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

                match chip_event {
                    Some(FindChipEvent::Reopen) => self.find_open = true,
                    Some(FindChipEvent::Clear) => {
                        app_ctx.folder_find.query.text.clear();
                        app_ctx.apply_find();
                    }
                    None => {}
                }
                // The popup and the rows only change on the next update.
                if chip_event.is_some() {
                    ctx.request_repaint();
                }
            });
        });
    }

    fn show_find(&mut self, ctx: &egui::Context, app_ctx: &mut AppStateCtx) {
        let status = app_ctx.find_matches.map(|matches| match matches {
            0 => "No results".to_owned(),
            1 => "1 match".to_owned(),
            n => format!("{n} matches"),
        });
        let find = &mut app_ctx.folder_find;
        let event = find_popup(
            ctx,
            &mut find.query,
            app_ctx.find_error.as_deref(),
            status.as_deref(),
            |ui| {
                let path = ui.checkbox(&mut find.match_path, "Match path").changed();
                let contents = ui
                    .checkbox(&mut find.show_contents, "Show matched folder contents")
                    .changed();
                path || contents
            },
        );
        match event {
            Some(FindPopupEvent::Changed) => app_ctx.apply_find(),
            Some(FindPopupEvent::Submitted) => {
                app_ctx.apply_find();
                self.find_open = app_ctx.find_error.is_some();
            }
            Some(FindPopupEvent::Closed) => {
                self.find_open = false;
                // Else the app quits on the same Esc.
                ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            }
            None => {}
        }
    }

    fn request_shutdown(&mut self) {
        self.state = Some(AppState::Exit());
    }

    fn process_ctx_inputs(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut user_quit: bool = false;
        {
            let _input_ctx = ctx.input(|r| {
                // Esc, unless a widget consumed it (the path bar's cancel)
                if r.key_pressed(egui::Key::Escape) {
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
                let frame_start = Instant::now();
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
                        let _ = set_root(&mut state.file_system_1_root_path, Side::Folder1, &path);
                    }
                }
                if self.open_dir_window_2 {
                    self.open_dir_window_2 = false;
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        let _ = set_root(&mut state.file_system_2_root_path, Side::Folder2, &path);
                    }
                }
                state.update_scans(ctx);
                state.update_compare(Instant::now());
                state.build_two_folder_diff_rows_if_stale();
                if state.file_system_1_scan.is_scanning()
                    || state.file_system_2_scan.is_scanning()
                    || state.tree_compare.is_comparing()
                {
                    ctx.request_repaint_after(SCAN_PROGRESS_INTERVAL);
                }
                if state.poll_blueprint_detector() {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }

                self.ui(ctx, frame, &mut state);

                self.process_ctx_inputs(ctx, frame);
                let frame_time = frame_start.elapsed();
                if frame_time > LONG_FRAME {
                    log::warn!("UI frame took {} ms", frame_time.as_millis());
                }
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
    use super::{
        AppState, AppStateCtx, ScanAction, ZApp, cancel_scan, finish_side_scan, scan_action,
        update_side_scan,
    };
    use crate::ui_egui::folder_keymap::FolderAction;
    use crate::ui_egui::fs_tree::{DiffState, FileSystemView, HScrollLink};
    use crate::ui_egui::path_diff_pane::{BLACKLIST_PRESETS, FilterField, FilterPresets};
    use crate::ui_egui::roots::{Side, clear_root, set_root};
    use crate::ui_egui::tree_compare::PairResult;
    use crate::ui_egui::tree_filter::{DiffFilter, DuplicatesFilter};
    use crate::ui_egui::tree_find::FolderFind;
    use crate::ui_egui::tree_sort::{SortKey, TreeSort};
    use crate::ui_egui::tree_tolerance::{SizeDelta, Tolerance};
    use eframe::egui;
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::Arc,
        time::{Duration, Instant},
    };
    use tempfile::{TempDir, tempdir};
    use zcommon::{logger::LogBuffer, ui_egui::keymap::Keymap};
    use zhashdiff::{
        comparison::{PairCompare, PairState},
        filter::{PathFilter, PatternList},
        fs::FileSystemModel,
    };

    fn finish_compare(state: &mut AppStateCtx) {
        state.build_two_folder_diff_rows_if_stale();
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.tree_compare.is_comparing() {
            assert!(Instant::now() < deadline, "the compare did not finish");
            std::thread::sleep(Duration::from_millis(1));
            state.update_compare(Instant::now());
            state.build_two_folder_diff_rows_if_stale();
        }
    }

    #[test]
    fn rows_under_a_state_sort_keep_their_order_until_the_compare_finishes() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            fs::write(root.join("a.txt"), "a").unwrap();
        }
        fs::write(left.path().join("b.txt"), "left b").unwrap();
        fs::write(right.path().join("b.txt"), "right b").unwrap();
        let mut state = AppStateCtx {
            tree_sort: TreeSort::default().clicked(SortKey::State),
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "a.txt", "b.txt"]);
        let start = Instant::now();
        let (sender, paths) = state.tree_compare.pause(start);
        let send = |rel_path: &str, state: PairState| {
            let i = paths.iter().position(|path| path == rel_path).unwrap();
            let result = PairResult {
                compare: Some(PairCompare { state, error: None }),
                likeness: None,
            };
            sender.send((i, result)).unwrap();
        };

        send("b.txt", PairState::Different);
        state.update_compare(start + Duration::from_secs(1));
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "a.txt", "b.txt"]);
        let rows = state.two_folder_diff_visible_rows.as_ref().unwrap();
        assert!(matches!(rows[1].diff_state, DiffState::Pending(..)));
        assert!(matches!(rows[2].diff_state, DiffState::Different(..)));

        send("a.txt", PairState::Same);
        state.update_compare(start + Duration::from_secs(1));
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "b.txt", "a.txt"]);
    }

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
    fn the_keymap_survives_a_restart() {
        let mut state = AppStateCtx::default();
        state.keymap.shortcuts_mut(FolderAction::NextDiff).remove(0);
        state
            .keymap
            .shortcuts_mut(FolderAction::OpenKeybindings)
            .clear();

        let json = serde_json::to_string(&state).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.keymap, state.keymap);
        assert_ne!(restored.keymap, Keymap::default());
    }

    #[test]
    fn a_state_saved_before_the_keymap_existed_loads_the_default_keymap() {
        let mut json: serde_json::Value = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut().unwrap().remove("keymap");

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();

        assert_eq!(restored.keymap, Keymap::default());
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
    fn the_no_build_dirs_preset_hides_target_and_keeps_src() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            for dir in ["src", "target"] {
                fs::create_dir(root.join(dir)).unwrap();
                fs::write(root.join(dir).join("a.rs"), "a").unwrap();
            }
        }
        let mut state = AppStateCtx {
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        let preset = BLACKLIST_PRESETS
            .iter()
            .find(|p| p.name == "No build dirs")
            .unwrap();

        FilterField::Blacklist.set_text(&mut state.path_filter, preset.text);
        state.build_two_folder_diff_rows_if_stale();

        assert_eq!(row_paths(&state), ["", "src", "src/a.rs"]);
    }

    #[test]
    fn filter_presets_survive_a_restart() {
        let mut state = AppStateCtx::default();
        let (builtins, user) = FilterField::Blacklist.presets(&mut state.filter_presets);
        user.save(builtins, "Mine", "*.tmp").unwrap();
        let (builtins, user) = FilterField::Whitelist.presets(&mut state.filter_presets);
        user.save(builtins, "Shaders", "*.hlsl *.usf").unwrap();

        let json = serde_json::to_string(&state).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.filter_presets, state.filter_presets);
        assert_ne!(restored.filter_presets, FilterPresets::default());
    }

    #[test]
    fn a_state_saved_before_filter_presets_existed_loads_none() {
        let mut json: serde_json::Value = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("filter_presets")
            .unwrap();

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();

        assert_eq!(restored.filter_presets, FilterPresets::default());
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
    fn the_duplicates_bar_survives_a_restart() {
        let state = AppStateCtx {
            duplicates_filter: DuplicatesFilter {
                duplicates_only: true,
                show_parents: false,
                hash_column: true,
            },
            ..Default::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.duplicates_filter, state.duplicates_filter);
    }

    #[test]
    fn a_state_saved_before_the_duplicates_bar_existed_shows_every_row_without_hashes() {
        let mut json: serde_json::Value = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("duplicates_filter")
            .unwrap();

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();

        assert_eq!(
            restored.duplicates_filter,
            DuplicatesFilter {
                duplicates_only: false,
                show_parents: true,
                hash_column: false,
            }
        );
    }

    #[test]
    fn the_likeness_bar_survives_a_restart() {
        let state = AppStateCtx {
            likeness_bar: true,
            ..Default::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();

        assert!(restored.likeness_bar);
    }

    #[test]
    fn a_state_saved_before_the_likeness_bar_existed_loads_it_off() {
        let mut json: serde_json::Value = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("likeness_bar")
            .unwrap();

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();

        assert!(!restored.likeness_bar);
    }

    #[test]
    fn linked_horizontal_scroll_is_on_by_default() {
        assert!(AppStateCtx::default().h_scroll_link.0);
    }

    #[test]
    fn unlinked_horizontal_scroll_survives_a_restart() {
        let state = AppStateCtx {
            h_scroll_link: HScrollLink(false),
            ..Default::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();

        assert!(!restored.h_scroll_link.0);
    }

    #[test]
    fn a_state_saved_before_the_link_toggle_existed_loads_it_on() {
        let mut json: serde_json::Value = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("h_scroll_link")
            .unwrap();

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();

        assert!(restored.h_scroll_link.0);
    }

    #[test]
    fn turning_the_likeness_bar_on_measures_the_differing_pairs_on_the_next_update() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            fs::write(root.join("a.txt"), "a").unwrap();
        }
        fs::write(left.path().join("b.txt"), "left b").unwrap();
        fs::write(right.path().join("b.txt"), "right b").unwrap();
        let mut state = AppStateCtx {
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        finish_compare(&mut state);
        assert!(!state.tree_compare.is_comparing());

        state.likeness_bar = true;
        state.build_two_folder_diff_rows_if_stale();

        assert_eq!(state.tree_compare.queued(), [("b.txt", false, true)]);
    }

    #[test]
    fn the_tolerance_settings_survive_a_restart() {
        let state = AppStateCtx {
            tolerance: Tolerance {
                enabled: true,
                size: false,
                max_size_delta: SizeDelta::Bytes(12),
                same_size_and_time: true,
                likeness: true,
                min_likeness: 95.5,
            },
            ..Default::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.tolerance, state.tolerance);
    }

    #[test]
    fn a_state_saved_before_tolerance_existed_loads_it_off_with_a_1_percent_size_criterion() {
        let mut json: serde_json::Value = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut().unwrap().remove("tolerance").unwrap();

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();

        assert_eq!(
            restored.tolerance,
            Tolerance {
                enabled: false,
                size: true,
                max_size_delta: SizeDelta::Percent(1.0),
                same_size_and_time: false,
                likeness: false,
                min_likeness: 99.0,
            }
        );
    }

    #[test]
    fn toggling_tolerance_rebuilds_the_rows_and_off_restores_the_exact_state_without_a_compare() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            fs::write(root.join("a.txt"), "a").unwrap();
        }
        fs::write(left.path().join("b.txt"), "left b").unwrap();
        fs::write(right.path().join("b.txt"), "right b").unwrap();
        let mut state = AppStateCtx {
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        finish_compare(&mut state);
        let b_state = |state: &AppStateCtx| {
            let rows = state.two_folder_diff_visible_rows.as_ref().unwrap();
            let row = rows.iter().find(|row| row.rel_path == "b.txt").unwrap();
            row.diff_state.clone()
        };
        assert!(matches!(b_state(&state), DiffState::Different(..)));

        state.tolerance = Tolerance {
            enabled: true,
            max_size_delta: SizeDelta::Bytes(1),
            ..Tolerance::default()
        };
        state.build_two_folder_diff_rows_if_stale();
        assert!(matches!(b_state(&state), DiffState::Similar(..)));
        assert!(!state.tree_compare.is_comparing());

        state.tolerance.enabled = false;
        state.build_two_folder_diff_rows_if_stale();
        assert!(matches!(b_state(&state), DiffState::Different(..)));
        assert!(!state.tree_compare.is_comparing());
    }

    #[test]
    fn a_tolerance_saved_before_the_likeness_criterion_existed_loads_it_off_at_99_percent() {
        let mut json: serde_json::Value = serde_json::to_value(AppStateCtx::default()).unwrap();
        let tolerance = json["tolerance"].as_object_mut().unwrap();
        tolerance.remove("likeness").unwrap();
        tolerance.remove("min_likeness").unwrap();

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();

        assert!(!restored.tolerance.likeness);
        assert_eq!(restored.tolerance.min_likeness, 99.0);
    }

    fn b_different_trees() -> (TempDir, TempDir, AppStateCtx) {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            fs::write(root.join("a.txt"), "a").unwrap();
        }
        fs::write(left.path().join("b.txt"), "left b").unwrap();
        fs::write(right.path().join("b.txt"), "right b").unwrap();
        let state = AppStateCtx {
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        (left, right, state)
    }

    fn b_state(state: &AppStateCtx) -> DiffState {
        let rows = state.two_folder_diff_visible_rows.as_ref().unwrap();
        let row = rows.iter().find(|row| row.rel_path == "b.txt").unwrap();
        row.diff_state.clone()
    }

    const LIKENESS_ONLY: Tolerance = Tolerance {
        enabled: true,
        size: false,
        max_size_delta: SizeDelta::Percent(1.0),
        same_size_and_time: false,
        likeness: true,
        min_likeness: 0.0,
    };

    #[test]
    fn the_likeness_criterion_with_the_bar_off_queues_likeness_and_disabling_it_stops() {
        let (_left, _right, mut state) = b_different_trees();
        finish_compare(&mut state);

        state.tolerance = LIKENESS_ONLY;
        state.build_two_folder_diff_rows_if_stale();
        assert!(!state.likeness_bar);
        assert_eq!(state.tree_compare.queued(), [("b.txt", false, true)]);
        assert!(matches!(b_state(&state), DiffState::Different(..)));

        state.tolerance.likeness = false;
        state.build_two_folder_diff_rows_if_stale();
        assert!(!state.tree_compare.is_comparing());
        assert_eq!(state.tree_compare.queued(), []);
    }

    #[test]
    fn a_pair_passing_the_likeness_criterion_turns_similar_once_measured() {
        let (_left, _right, mut state) = b_different_trees();
        state.tolerance = LIKENESS_ONLY;

        finish_compare(&mut state);

        assert!(matches!(b_state(&state), DiffState::Similar(..)));
        state.tolerance.min_likeness = 1.0;
        state.build_two_folder_diff_rows_if_stale();
        assert!(matches!(b_state(&state), DiffState::Different(..)));
        assert!(!state.tree_compare.is_comparing());
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

        finish_compare(&mut state);
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

        finish_compare(&mut state);
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

    fn find_state(files: &[&str]) -> (TempDir, TempDir, AppStateCtx) {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            for file in files {
                let path = root.join(file);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, root.to_string_lossy().as_bytes()).unwrap();
            }
        }
        let state = AppStateCtx {
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        (left, right, state)
    }

    fn set_find(state: &mut AppStateCtx, text: &str) {
        state.folder_find.query.text = text.to_owned();
        state.apply_find();
    }

    const ART: [&str; 4] = [
        "art/ui/button.png",
        "art/ui/panel.png",
        "src/main.rs",
        "readme.md",
    ];

    #[test]
    fn a_find_query_narrows_the_rows_on_the_next_update_and_counts_the_matches() {
        let (_left, _right, mut state) = find_state(&ART);
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(state.find_matches, None);

        set_find(&mut state, "panel");
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "art", "art/ui", "art/ui/panel.png"]);
        assert_eq!(state.find_matches, Some(1));

        let built = state
            .two_folder_diff_visible_rows
            .as_ref()
            .unwrap()
            .as_ptr();
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(
            state
                .two_folder_diff_visible_rows
                .as_ref()
                .unwrap()
                .as_ptr(),
            built
        );

        state.folder_find.show_contents = true;
        set_find(&mut state, "ui");
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(
            row_paths(&state),
            ["", "art", "art/ui", "art/ui/button.png", "art/ui/panel.png"]
        );

        set_find(&mut state, "nothing");
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), [""]);
        assert_eq!(state.find_matches, Some(0));

        set_find(&mut state, "");
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state).len(), 8);
        assert_eq!(state.find_matches, None);
    }

    #[test]
    fn find_never_reveals_a_row_the_filter_hides() {
        let (_left, _right, mut state) = find_state(&["gen/a.obj", "keep/b.obj", "keep/c.txt"]);
        state.path_filter = blacklist("*.obj");

        set_find(&mut state, "obj");
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), [""]);
        assert_eq!(state.find_matches, Some(0));

        state.folder_find.show_contents = true;
        set_find(&mut state, "keep");
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "keep", "keep/c.txt"]);
    }

    #[test]
    fn an_invalid_regex_keeps_the_previous_find_and_reports_the_error() {
        let (_left, _right, mut state) = find_state(&ART);
        set_find(&mut state, "panel");
        state.build_two_folder_diff_rows_if_stale();

        state.folder_find.query.regex = true;
        set_find(&mut state, "panel(");
        state.build_two_folder_diff_rows_if_stale();
        assert!(state.find_error.as_deref().unwrap().contains("regex"));
        assert_eq!(row_paths(&state), ["", "art", "art/ui", "art/ui/panel.png"]);

        set_find(&mut state, "^main");
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(state.find_error, None);
        assert_eq!(row_paths(&state), ["", "src", "src/main.rs"]);
    }

    #[test]
    fn a_find_change_keeps_the_running_compare_which_covers_the_hidden_pairs() {
        let (_left, _right, mut state) = find_state(&ART);
        set_find(&mut state, "panel");
        state.build_two_folder_diff_rows_if_stale();
        let (sender, _) = state.tree_compare.pause(Instant::now());
        let queued: Vec<String> = state
            .tree_compare
            .queued()
            .into_iter()
            .map(|(rel_path, _, _)| rel_path.to_owned())
            .collect();
        assert_eq!(queued.len(), 4);

        set_find(&mut state, "main");
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "src", "src/main.rs"]);
        let result = PairResult {
            compare: Some(PairCompare {
                state: PairState::Different,
                error: None,
            }),
            likeness: None,
        };
        assert!(
            sender.send((0, result)).is_ok(),
            "the compare was restarted"
        );
    }

    #[test]
    fn a_pair_compared_while_find_hides_it_shows_its_state_when_find_reveals_it() {
        let (_left, _right, mut state) = find_state(&ART);
        set_find(&mut state, "panel");
        let start = Instant::now();
        state.build_two_folder_diff_rows_if_stale();
        let (sender, paths) = state.tree_compare.pause(start);
        let i = paths.iter().position(|path| path == "readme.md").unwrap();
        let result = PairResult {
            compare: Some(PairCompare {
                state: PairState::Different,
                error: None,
            }),
            likeness: None,
        };
        sender.send((i, result)).unwrap();
        state.update_compare(start + Duration::from_secs(1));
        assert!(state.tree_compare.is_comparing());

        set_find(&mut state, "readme");
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "readme.md"]);
        let rows = state.two_folder_diff_visible_rows.as_ref().unwrap();
        assert!(matches!(rows[1].diff_state, DiffState::Different(..)));
    }

    #[test]
    fn find_is_not_saved_so_it_is_empty_after_a_restart() {
        let (_left, _right, mut state) = find_state(&ART);
        state.folder_find.match_path = true;
        state.folder_find.show_contents = true;
        set_find(&mut state, "forgotten_query");
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(state.find_matches, Some(0));

        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("forgotten_query"));
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.folder_find, FolderFind::default());
        assert_eq!(restored.find_applied, FolderFind::default());
        assert_eq!(restored.find_matches, None);
    }

    fn finish_scans(state: &mut AppStateCtx) {
        finish_side_scan(
            Side::Folder1,
            &mut state.file_system_1_root_path,
            &mut state.file_system_model_1_view,
            &mut state.file_system_1_scan,
            &mut state.two_folder_diff_visible_rows,
        );
        finish_side_scan(
            Side::Folder2,
            &mut state.file_system_2_root_path,
            &mut state.file_system_model_2_view,
            &mut state.file_system_2_scan,
            &mut state.two_folder_diff_visible_rows,
        );
    }

    fn shown(view: &Option<FileSystemView>) -> Option<PathBuf> {
        view.as_ref()
            .map(|view| view.file_system.get_root().as_path().as_ref().to_path_buf())
    }

    fn loaded(root_1: &Path, root_2: &Path) -> AppStateCtx {
        let mut state = AppStateCtx::default();
        set_root(&mut state.file_system_1_root_path, Side::Folder1, root_1).unwrap();
        set_root(&mut state.file_system_2_root_path, Side::Folder2, root_2).unwrap();
        finish_scans(&mut state);
        state
    }

    fn update_side_1(state: &mut AppStateCtx) {
        update_side_scan(
            Side::Folder1,
            &mut state.file_system_1_root_path,
            &mut state.file_system_model_1_view,
            &mut state.file_system_1_scan,
            &mut state.two_folder_diff_visible_rows,
            &egui::Context::default(),
        );
    }

    fn cancel_side_1(state: &mut AppStateCtx) {
        cancel_scan(
            Side::Folder1,
            &mut state.file_system_1_root_path,
            state.file_system_model_1_view.as_ref(),
            &mut state.file_system_1_scan,
        );
    }

    #[test]
    fn a_side_without_a_root_is_cleared() {
        let a = Path::new("a");
        assert_eq!(
            scan_action(None, Some(a), Some(a), false),
            ScanAction::Clear
        );
        assert_eq!(scan_action(None, None, None, true), ScanAction::Clear);
    }

    #[test]
    fn a_new_root_starts_a_scan() {
        let (a, b) = (Path::new("a"), Path::new("b"));
        assert_eq!(
            scan_action(Some(b), Some(a), None, false),
            ScanAction::Start(b.into())
        );
        assert_eq!(
            scan_action(Some(b), None, None, false),
            ScanAction::Start(b.into())
        );
    }

    #[test]
    fn a_new_root_replaces_the_scan_of_another_root() {
        let (a, b, c) = (Path::new("a"), Path::new("b"), Path::new("c"));
        assert_eq!(
            scan_action(Some(b), Some(a), Some(c), false),
            ScanAction::Start(b.into())
        );
    }

    #[test]
    fn the_scan_of_the_root_keeps_running() {
        let (a, b) = (Path::new("a"), Path::new("b"));
        assert_eq!(
            scan_action(Some(b), Some(a), Some(b), false),
            ScanAction::Keep
        );
        assert_eq!(scan_action(Some(b), None, Some(b), false), ScanAction::Keep);
        assert_eq!(
            scan_action(Some(a), Some(a), Some(a), false),
            ScanAction::Keep
        );
    }

    #[test]
    fn the_shown_root_needs_no_scan() {
        let (a, b) = (Path::new("a"), Path::new("b"));
        assert_eq!(scan_action(Some(a), Some(a), None, false), ScanAction::Stop);
        assert_eq!(
            scan_action(Some(a), Some(a), Some(b), false),
            ScanAction::Stop
        );
    }

    #[test]
    fn a_rescan_scans_the_shown_root_again() {
        let a = Path::new("a");
        assert_eq!(
            scan_action(Some(a), Some(a), None, true),
            ScanAction::Start(a.into())
        );
    }

    #[test]
    fn launch_with_remembered_roots_scans_both_sides_in_the_background() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        fs::write(left.path().join("a.txt"), "a").unwrap();
        fs::write(right.path().join("b.txt"), "b").unwrap();
        let mut state = AppStateCtx::default();
        set_root(
            &mut state.file_system_1_root_path,
            Side::Folder1,
            left.path(),
        )
        .unwrap();
        set_root(
            &mut state.file_system_2_root_path,
            Side::Folder2,
            right.path(),
        )
        .unwrap();

        state.update_scans(&egui::Context::default());
        assert!(state.file_system_1_scan.is_scanning());
        assert!(state.file_system_2_scan.is_scanning());

        finish_scans(&mut state);
        assert_eq!(
            shown(&state.file_system_model_1_view),
            Some(left.path().into())
        );
        assert_eq!(
            shown(&state.file_system_model_2_view),
            Some(right.path().into())
        );
        let view_2 = state.file_system_model_2_view.as_ref().unwrap();
        assert!(
            view_2
                .file_system
                .find_path(right.path().join("b.txt"))
                .is_some()
        );
    }

    #[test]
    fn changing_one_root_rescans_only_that_side() {
        let (left, right, other) = (tempdir().unwrap(), tempdir().unwrap(), tempdir().unwrap());
        let mut state = loaded(left.path(), right.path());
        let model_1 = state
            .file_system_model_1_view
            .as_ref()
            .unwrap()
            .file_system
            .clone();
        state.build_two_folder_diff_rows_if_stale();

        set_root(
            &mut state.file_system_2_root_path,
            Side::Folder2,
            other.path(),
        )
        .unwrap();
        state.update_scans(&egui::Context::default());
        assert!(!state.file_system_1_scan.is_scanning());
        let job = state.file_system_2_scan.job().expect("side 2 scans");
        assert_eq!(job.root(), other.path());
        assert_eq!(
            shown(&state.file_system_model_2_view),
            Some(right.path().into())
        );
        assert!(state.two_folder_diff_visible_rows.is_some());

        finish_scans(&mut state);
        let view_1 = state.file_system_model_1_view.as_ref().unwrap();
        assert!(Arc::ptr_eq(&view_1.file_system, &model_1));
        assert_eq!(
            shown(&state.file_system_model_2_view),
            Some(other.path().into())
        );
        assert!(state.two_folder_diff_visible_rows.is_none());
    }

    #[test]
    fn cancelling_a_scan_keeps_the_previous_view() {
        let (left, right, other) = (tempdir().unwrap(), tempdir().unwrap(), tempdir().unwrap());
        let mut state = loaded(left.path(), right.path());
        let model_1 = state
            .file_system_model_1_view
            .as_ref()
            .unwrap()
            .file_system
            .clone();

        set_root(
            &mut state.file_system_1_root_path,
            Side::Folder1,
            other.path(),
        )
        .unwrap();
        update_side_1(&mut state);
        assert!(state.file_system_1_scan.is_scanning());
        cancel_side_1(&mut state);

        assert_eq!(state.file_system_1_root_path.as_deref(), Some(left.path()));
        assert!(!state.file_system_1_scan.is_scanning());
        update_side_1(&mut state);
        assert!(!state.file_system_1_scan.is_scanning());
        let view_1 = state.file_system_model_1_view.as_ref().unwrap();
        assert!(Arc::ptr_eq(&view_1.file_system, &model_1));
    }

    #[test]
    fn cancelling_the_first_scan_leaves_the_side_empty() {
        let dir = tempdir().unwrap();
        let mut state = AppStateCtx::default();
        set_root(
            &mut state.file_system_1_root_path,
            Side::Folder1,
            dir.path(),
        )
        .unwrap();
        update_side_1(&mut state);
        assert!(state.file_system_1_scan.is_scanning());
        cancel_side_1(&mut state);

        assert_eq!(state.file_system_1_root_path, None);
        update_side_1(&mut state);
        assert!(!state.file_system_1_scan.is_scanning());
        assert!(state.file_system_model_1_view.is_none());
    }

    #[test]
    fn a_failed_scan_keeps_the_previous_view() {
        let (left, right, gone) = (tempdir().unwrap(), tempdir().unwrap(), tempdir().unwrap());
        let mut state = loaded(left.path(), right.path());
        let gone: PathBuf = gone.path().into();
        set_root(&mut state.file_system_1_root_path, Side::Folder1, &gone).unwrap();
        fs::remove_dir(&gone).unwrap();

        finish_scans(&mut state);
        assert_eq!(state.file_system_1_root_path.as_deref(), Some(left.path()));
        assert_eq!(
            shown(&state.file_system_model_1_view),
            Some(left.path().into())
        );
    }

    #[test]
    fn clearing_a_root_drops_its_view_and_the_rows_once() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        let mut state = loaded(left.path(), right.path());
        state.build_two_folder_diff_rows_if_stale();

        clear_root(&mut state.file_system_2_root_path);
        state.update_scans(&egui::Context::default());
        assert!(state.file_system_model_2_view.is_none());
        assert!(state.two_folder_diff_visible_rows.is_none());

        state.build_two_folder_diff_rows_if_stale();
        state.update_scans(&egui::Context::default());
        assert!(state.two_folder_diff_visible_rows.is_some());
    }

    #[test]
    fn refresh_rescans_both_sides_and_keeps_the_collapsed_folders() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            fs::create_dir(root.join("sub")).unwrap();
            fs::write(root.join("sub").join("a.txt"), "a").unwrap();
        }
        let mut state = loaded(left.path(), right.path());
        let view_1 = state.file_system_model_1_view.as_mut().unwrap();
        let old_sub = view_1
            .file_system
            .find_path(left.path().join("sub"))
            .unwrap();
        view_1.collapsed.insert(old_sub, true);
        let models = [
            &state.file_system_model_1_view,
            &state.file_system_model_2_view,
        ]
        .map(|view| view.as_ref().unwrap().file_system.clone());
        for root in [left.path(), right.path()] {
            fs::write(root.join("0.txt"), "new").unwrap();
        }

        state.refresh();
        state.update_scans(&egui::Context::default());
        assert!(state.file_system_1_scan.is_scanning());
        assert!(state.file_system_2_scan.is_scanning());
        finish_scans(&mut state);

        let views = [
            state.file_system_model_1_view.as_ref().unwrap(),
            state.file_system_model_2_view.as_ref().unwrap(),
        ];
        for ((view, model), root) in views.iter().zip(&models).zip([left.path(), right.path()]) {
            assert!(!Arc::ptr_eq(&view.file_system, model));
            assert!(view.file_system.find_path(root.join("0.txt")).is_some());
        }
        let sub = |view: &FileSystemView, root: &Path| {
            view.file_system.find_path(root.join("sub")).unwrap()
        };
        assert_ne!(sub(views[0], left.path()), old_sub);
        assert!(views[0].is_collapsed(sub(views[0], left.path())));
        assert!(!views[1].is_collapsed(sub(views[1], right.path())));
    }

    #[test]
    fn refresh_during_a_scan_does_not_queue_another_scan() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        let mut state = loaded(left.path(), right.path());
        state.refresh();
        state.update_scans(&egui::Context::default());
        assert!(state.file_system_1_scan.is_scanning());

        state.refresh();
        finish_scans(&mut state);
        state.update_scans(&egui::Context::default());
        assert!(!state.file_system_1_scan.is_scanning());
        assert!(!state.file_system_2_scan.is_scanning());
    }
}
