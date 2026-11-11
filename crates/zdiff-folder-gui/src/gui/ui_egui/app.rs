use eframe::egui::{self, Layout, PointerButton, containers::menu::MenuConfig};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use zcommon::{
    find::{FindError, Matcher},
    hash::HashService,
    job::Job,
    kinds::KindTable,
    logger::{self, LogBuffer},
    universal_path::UniversalPath,
    vcs::{git::GitCommand, p4::P4Command},
};
use zcommon_ui::{
    find_popup::{FindPopupEvent, find_popup},
    frame_stats::{self, FrameStats},
    keymap::{Keymap, menu_item},
    shared_keymap::load_shared,
    style_window::StyleWindow,
};
use zdiff_folder::{
    external_diff_tool::DiffToolConfig, filter::PathFilter, fs::FileSystemModel, scan::ScanOutput,
    uasset::BlueprintDetector, vcs_source::scan_side,
};

use crate::ui_egui::{
    duplicate_files_pane::DuplicateFilesPane,
    export::{folder_diff_report, save_report},
    folder_keymap::{self, FolderAction, KeybindingsWindow},
    fs_tree::{
        FileSystemView, FolderRole, HScrollLink, RowsId, VisibleRowTwoFolderDiff,
        apply_blueprint_results,
    },
    panes::{Pane, PathDiffView, TreeBehavior},
    path_diff_pane::{FilterPresets, PathDiffPane},
    revision_open,
    roots::{LaunchArgs, Side, apply_launch_args, clear_root, set_root},
    status_line::{
        RowCounts, ScanSummary, ToolMessage, count_rows, counts_text, ui_status_summary,
        ui_tool_message,
    },
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
    #[serde(skip)]
    row_counts: Option<(RowsId, RowCounts)>,
    #[serde(skip)]
    two_folder_diff_arranged_rows: Option<Vec<VisibleRowTwoFolderDiff>>,
    #[serde(skip)]
    tree_compare: TreeCompare,
    #[serde(skip)]
    blueprint_detector: BlueprintDetector,
    #[serde(skip)]
    blueprint_results: Vec<(PathBuf, bool)>,
    #[serde(skip)]
    blueprints_applied: Option<Instant>,
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
    #[serde(default)]
    pub sides_swapped: bool,
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
    #[serde(default, deserialize_with = "folder_keymap::deserialize_keymap")]
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
                let rows = self.build_rows(true);
                // Before the state filter and find: the compare covers the pairs they hide, so
                // Equal still compares Pending pairs and find never restarts it.
                if let Some(rows) = &rows {
                    self.tree_compare.start(
                        rows,
                        [
                            self.file_system_model_1_view.as_ref(),
                            self.file_system_model_2_view.as_ref(),
                        ],
                        Instant::now(),
                    );
                }
                self.two_folder_diff_arranged_rows = rows
                    .map(|rows| arrange_two_folder_rows(rows, self.diff_filter, self.tree_sort));
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
            && let Some(rows) = self.build_rows(false)
        {
            self.tree_compare.start(
                &rows,
                [
                    self.file_system_model_1_view.as_ref(),
                    self.file_system_model_2_view.as_ref(),
                ],
                Instant::now(),
            );
        }
    }

    fn export_folder_diff(&mut self) {
        let Some(rows) = self.two_folder_diff_visible_rows.as_deref() else {
            self.tool_message = Some(ToolMessage {
                status: Some("Export: no folder diff is shown".to_owned()),
                ..ToolMessage::default()
            });
            return;
        };
        let counts = counts_text(&count_rows(rows), self.tolerance.enabled);
        let roots = [
            self.file_system_1_root_path.as_deref(),
            self.file_system_2_root_path.as_deref(),
        ];
        let text = folder_diff_report(roots, &counts, rows);
        if let Some(message) = save_report(&text, "folder-diff.txt") {
            self.tool_message = Some(message);
        }
    }

    fn narrow_rows_to_find(&mut self) {
        let flat = self.diff_filter.is_flat(self.tree_sort);
        let arranged = self
            .two_folder_diff_arranged_rows
            .take()
            .or_else(|| self.two_folder_diff_visible_rows.take());
        let (rows, arranged, matches) = match arranged {
            Some(arranged) => {
                let (rows, arranged, matches) = narrow_to_find(arranged, &self.find_applied, flat);
                (Some(rows), arranged, matches)
            }
            None => (None, None, None),
        };
        self.two_folder_diff_visible_rows = rows;
        self.two_folder_diff_arranged_rows = arranged;
        self.two_folder_diff_rows_id = RowsId::next();
        self.find_matches = matches;
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
                    .then(|| detector.check(&path(), meta))
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
                let patch_start = Instant::now();
                if self.two_folder_diff_visible_rows.is_some()
                    && let Some(fresh) = self.build_rows(false)
                    && let Some(rows) = self
                        .two_folder_diff_arranged_rows
                        .as_mut()
                        .or(self.two_folder_diff_visible_rows.as_mut())
                {
                    patch_row_states(rows, fresh);
                    self.narrow_rows_to_find();
                }
                let cost = patch_start.elapsed();
                self.tree_compare.patched(now + cost, cost);
            }
            CompareUpdate::Finished => self.two_folder_diff_visible_rows = None,
        }
    }

    fn poll_blueprints_pending(&mut self, now: Instant) -> bool {
        self.blueprint_results
            .extend(self.blueprint_detector.poll());
        let due = self
            .blueprints_applied
            .is_none_or(|applied| now >= applied + BLUEPRINT_APPLY_INTERVAL);
        if due && !self.blueprint_results.is_empty() {
            self.blueprints_applied = Some(now);
            let results = std::mem::take(&mut self.blueprint_results);
            if self.two_folder_diff_visible_rows.is_some()
                && let Some(rows) = self
                    .two_folder_diff_arranged_rows
                    .as_mut()
                    .or(self.two_folder_diff_visible_rows.as_mut())
            {
                let root = |view: &Option<FileSystemView>| {
                    view.as_ref()
                        .map(|v| v.file_system.path(v.file_system.get_root_node_id()))
                };
                let roots = [
                    root(&self.file_system_model_1_view),
                    root(&self.file_system_model_2_view),
                ];
                apply_blueprint_results(rows, [roots[0].as_deref(), roots[1].as_deref()], &results);
                self.narrow_rows_to_find();
            }
        }
        self.blueprint_detector.pending() > 0 || !self.blueprint_results.is_empty()
    }

    fn refresh(&mut self) {
        // A running scan keeps its flag (ScanAction::Keep), so it would scan again when done.
        for scan in [&mut self.file_system_1_scan, &mut self.file_system_2_scan] {
            if !scan.is_scanning() {
                scan.request_rescan();
            }
        }
    }

    fn clear_roots(&mut self) {
        clear_root(&mut self.file_system_1_root_path);
        clear_root(&mut self.file_system_2_root_path);
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

/// The rows `find` shows, the rows it narrowed and its match count; all rows, None and None
/// without a query.
fn narrow_to_find(
    rows: Vec<VisibleRowTwoFolderDiff>,
    find: &FolderFind,
    flat: bool,
) -> (
    Vec<VisibleRowTwoFolderDiff>,
    Option<Vec<VisibleRowTwoFolderDiff>>,
    Option<usize>,
) {
    let Ok(matcher) = Matcher::new(&find.query) else {
        return (rows, None, None);
    };
    let mask = find_mask(&rows, &matcher, find, flat);
    let shown = rows
        .iter()
        .zip(mask.hidden)
        .filter(|(_, hidden)| !hidden)
        .map(|(row, _)| row.clone())
        .collect();
    (shown, Some(rows), Some(mask.matches))
}

/// One background scan of a side's root. Dropping it cancels the scan.
#[derive(Debug)]
pub struct ScanJob {
    root: PathBuf,
    started: Instant,
    found: Arc<AtomicUsize>,
    job: Job<Option<io::Result<ScanOutput>>>,
}

impl ScanJob {
    fn start(root: PathBuf, repaint: egui::Context) -> Self {
        let found = Arc::new(AtomicUsize::new(0));
        let mut job = Job::new(move || repaint.request_repaint());
        let (work_root, work_found) = (root.clone(), found.clone());
        job.start(move |ctx| {
            let p4 = P4Command::new(false).for_file(UniversalPath::from(&*work_root));
            let output = scan_side(
                &GitCommand::default(),
                &p4,
                &work_root,
                ctx.cancel_flag(),
                &work_found,
            );
            ctx.send(output);
        });
        Self {
            root,
            started: Instant::now(),
            found,
            job,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn found(&self) -> usize {
        self.found.load(Ordering::Relaxed)
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
    view.map(|view| view.file_system.path(view.file_system.get_root_node_id()))
}

/// A rescan of the shown root keeps the collapsed state of every path still present.
fn install_scanned(old: Option<&FileSystemView>, model: FileSystemModel) -> FileSystemView {
    let mut view = FileSystemView::new(Arc::new(model));
    let Some(old) = old.filter(|old| {
        !old.collapsed.is_empty() && shown_root(Some(old)) == shown_root(Some(&view))
    }) else {
        return view;
    };
    let old_collapsed: HashMap<PathBuf, bool> = old
        .collapsed
        .iter()
        .filter(|&(&id, _)| old.file_system.get_node(id).is_some())
        .map(|(&id, &collapsed)| (old.file_system.path(id), collapsed))
        .collect();
    for id in view.file_system.iter_nodes() {
        if let Some(&collapsed) = old_collapsed.get(&view.file_system.path(id)) {
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

    let Some(job) = &mut scan.job else {
        return;
    };
    let running = job.job.is_running();
    let Some(result) = job.job.poll().pop() else {
        if !running {
            log::error!(
                "{side}: the scan of {} stopped without a result",
                job.root.display()
            );
            scan.job = None;
            revert_root(side, root, view.as_ref());
        }
        return;
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
    let progress = scan_progress_text(side, job.root(), job.found());
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

fn scan_progress_text(side: Side, root: &Path, found: usize) -> String {
    let lister = match UniversalPath::from(root) {
        UniversalPath::Depot(..) => Some("p4"),
        UniversalPath::Git(..) => Some("git"),
        UniversalPath::Local(_) => None,
    };
    match lister {
        // A listing is one command, so nothing is found until it returns.
        Some(lister) if found == 0 => format!("Listing {side} from {lister}: {}", root.display()),
        _ => format!("Scanning {side}: {found} entries in {}", root.display()),
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
    #[serde(skip, default = "restored_log_buffer")]
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
    #[serde(skip)]
    export_requested: bool,
    #[serde(skip)]
    title: String,
    #[serde(skip)]
    frame_stats: FrameStats,
    #[serde(default)]
    show_frame_breakdown: bool,
}

const LONG_FRAME: Duration = Duration::from_millis(200);
const SCAN_PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const BLUEPRINT_APPLY_INTERVAL: Duration = Duration::from_millis(250);

fn default_zoom() -> f32 {
    1.0
}

fn clamp_zoom(zoom: f32) -> f32 {
    zoom.clamp(0.5, 3.0)
}

fn deserialize_zoom<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<f32, D::Error> {
    f32::deserialize(deserializer).map(clamp_zoom)
}

fn restored_log_buffer() -> LogBuffer {
    logger::global_log_buffer().unwrap_or_default()
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
            export_requested: false,
            title: String::new(),
            frame_stats: FrameStats::default(),
            show_frame_breakdown: false,
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
            egui::MenuBar::new()
                .config(
                    MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
                )
                .ui(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    ui.menu_button("File", |ui| {
                        if menu_item(
                            ui,
                            "Open Folder 1",
                            &app_ctx.keymap,
                            FolderAction::OpenFolder1,
                        )
                        .clicked()
                        {
                            self.open_dir_window_1 = true;
                        }
                        if menu_item(
                            ui,
                            "Open Folder 2",
                            &app_ctx.keymap,
                            FolderAction::OpenFolder2,
                        )
                        .clicked()
                        {
                            self.open_dir_window_2 = true;
                        }
                        if menu_item(ui, "Swap Sides", &app_ctx.keymap, FolderAction::SwapSides)
                            .clicked()
                        {
                            app_ctx.sides_swapped = !app_ctx.sides_swapped;
                        }
                        if menu_item(ui, "Refresh", &app_ctx.keymap, FolderAction::Refresh)
                            .clicked()
                        {
                            app_ctx.refresh();
                            ui.ctx().request_repaint();
                        }
                        if menu_item(
                            ui,
                            "Clear Folder Paths",
                            &app_ctx.keymap,
                            FolderAction::ClearPaths,
                        )
                        .clicked()
                        {
                            app_ctx.clear_roots();
                        }
                        ui.add_enabled_ui(self.diff_mode_path_diff, |ui| {
                            if menu_item(ui, "Find", &app_ctx.keymap, FolderAction::Find).clicked()
                            {
                                self.find_open = true;
                            }
                        });
                        let export_label = if self.diff_mode_path_diff {
                            "Export Folder Diff..."
                        } else {
                            "Export Duplicates..."
                        };
                        if menu_item(ui, export_label, &app_ctx.keymap, FolderAction::Export)
                            .clicked()
                        {
                            self.export_requested = true;
                            ui.ctx().request_repaint();
                        }
                    });

                    ui.menu_button("Options", |ui| {
                        if menu_item(
                            ui,
                            "External Diff Tools",
                            &app_ctx.keymap,
                            FolderAction::OpenDiffTools,
                        )
                        .clicked()
                        {
                            self.tools_window.open = true;
                        }
                        if menu_item(
                            ui,
                            "Keyboard Shortcuts",
                            &app_ctx.keymap,
                            FolderAction::OpenKeybindings,
                        )
                        .clicked()
                        {
                            self.keybindings_window.open = true;
                        }
                        if menu_item(ui, "Style", &app_ctx.keymap, FolderAction::OpenStyle)
                            .clicked()
                        {
                            self.style_window.open(ui.ctx());
                        }
                        ui.checkbox(&mut self.show_frame_breakdown, "Frame Time Breakdown");
                    });

                    let diff_mode_text = if self.diff_mode_path_diff {
                        "Change to Duplicate Diff"
                    } else {
                        "Change to Path Diff"
                    };
                    ui.menu_button("Diff Mode", |ui| {
                        if menu_item(
                            ui,
                            diff_mode_text,
                            &app_ctx.keymap,
                            FolderAction::ToggleDiffMode,
                        )
                        .clicked()
                        {
                            self.diff_mode_path_diff = !self.diff_mode_path_diff;
                        }
                    });
                    #[cfg(debug_assertions)]
                    ui.menu_button("Debug", |ui| {
                        if ui.button("Clear FileSystemModels").clicked() {
                            app_ctx.file_system_model_1_view = None;
                            app_ctx.file_system_model_2_view = None;
                            app_ctx.two_folder_diff_visible_rows = None;
                        }
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
                    });
                    frame_stats::fps_label(ui, &self.frame_stats, &mut self.show_frame_breakdown);
                });
        });
        if self.show_frame_breakdown {
            frame_stats::breakdown_window(
                ui.ctx(),
                &self.frame_stats,
                &mut self.show_frame_breakdown,
            );
        }
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
        let mut actions = if capturing {
            Vec::new()
        } else {
            ctx.input(|i| app_ctx.keymap.dispatch(&i.events, keyboard_taken))
        };
        // The menu draws after this frame's actions are taken, so its export runs next frame.
        if std::mem::take(&mut self.export_requested) {
            actions.push(FolderAction::Export);
        }
        if actions.contains(&FolderAction::OpenFolder1) {
            self.open_dir_window_1 = true;
        }
        if actions.contains(&FolderAction::OpenFolder2) {
            self.open_dir_window_2 = true;
        }
        if actions.contains(&FolderAction::OpenKeybindings) {
            self.keybindings_window.open = true;
        }
        if actions.contains(&FolderAction::Refresh) {
            app_ctx.refresh();
            ctx.request_repaint();
        }
        if actions.contains(&FolderAction::SwapSides) {
            app_ctx.sides_swapped = !app_ctx.sides_swapped;
        }
        if actions.contains(&FolderAction::ClearPaths) {
            app_ctx.clear_roots();
        }
        if actions.contains(&FolderAction::OpenStyle) {
            self.style_window.open(ctx);
        }
        if actions.contains(&FolderAction::OpenDiffTools) {
            self.tools_window.open = true;
        }
        if actions.contains(&FolderAction::ToggleDiffMode) {
            self.diff_mode_path_diff = !self.diff_mode_path_diff;
        }
        if actions.contains(&FolderAction::Find) && self.diff_mode_path_diff {
            self.find_open = true;
        }
        if actions.contains(&FolderAction::Export) && self.diff_mode_path_diff {
            app_ctx.export_folder_diff();
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
            if let Some(rows) = app_ctx.two_folder_diff_visible_rows.as_deref()
                && app_ctx
                    .row_counts
                    .as_ref()
                    .is_none_or(|(counted, _)| *counted != app_ctx.two_folder_diff_rows_id)
            {
                app_ctx.row_counts = Some((app_ctx.two_folder_diff_rows_id, count_rows(rows)));
            }
            let row_counts = app_ctx
                .two_folder_diff_visible_rows
                .as_ref()
                .and(app_ctx.row_counts.as_ref().map(|(_, counts)| counts));
            egui::TopBottomPanel::bottom("status_line").show(ctx, |ui| {
                ui_status_summary(
                    ui,
                    row_counts,
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
        self.frame_stats.mark("other");
        egui::CentralPanel::default().show(ctx, |ui| {
            self.show_menu(ui, app_ctx);
            self.frame_stats.mark("menu");

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
                    sides_swapped: app_ctx.sides_swapped,
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
                self.frame_stats.mark("panes");

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
                            "ZDiffFolder [{}] - {}, {}",
                            built_rows_len,
                            source_path.display(),
                            target_path.display()
                        );

                        // Every viewport command requests a repaint, so an unchanged title
                        // would keep the app redrawing while idle.
                        if new_title != self.title {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Title(new_title.clone()));
                            self.title = new_title;
                        }

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
            self.frame_stats.mark("other");
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

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        revision_open::remove_all();
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.frame_stats.begin_frame(ctx, frame);
        let mut app_state = self
            .state
            .take()
            .expect("state should be valid before and after update()");
        app_state = match app_state {
            AppState::Startup(mut state_ctx) => {
                self.startup(ctx, frame);
                load_shared(&mut state_ctx.keymap);
                log::info!("STARTUP COMPLETE, TRANSITIONING TO IDLE");
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label("Loading...");
                    });
                });
                ctx.request_repaint();
                AppState::Idle(state_ctx)
            }
            AppState::Idle(mut state) => {
                let frame_start = Instant::now();
                self.sync_zoom(ctx);
                if ctx.input(|i| i.events.contains(&egui::Event::WindowFocused(true))) {
                    load_shared(&mut state.keymap);
                }
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
                state.tree_compare.set_repaint(ctx);
                state.update_compare(Instant::now());
                self.frame_stats.mark("jobs");
                state.build_two_folder_diff_rows_if_stale();
                self.frame_stats.mark("rows");
                if state.file_system_1_scan.is_scanning()
                    || state.file_system_2_scan.is_scanning()
                    || state.tree_compare.is_comparing()
                {
                    ctx.request_repaint_after(SCAN_PROGRESS_INTERVAL);
                }
                if state.poll_blueprints_pending(Instant::now()) {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                let fetching = revision_open::is_fetching();
                if let Some(outcome) = revision_open::take_outcome() {
                    state.tool_message = (outcome != ToolMessage::default()).then_some(outcome);
                }
                if fetching {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                self.frame_stats.mark("jobs");

                self.ui(ctx, frame, &mut state);

                self.process_ctx_inputs(ctx, frame);
                self.frame_stats.mark("inputs");
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
        scan_progress_text, update_side_scan,
    };
    use crate::ui_egui::folder_keymap::FolderAction;
    use crate::ui_egui::fs_tree::{DiffState, FileSystemView, HScrollLink};
    use crate::ui_egui::path_diff_pane::{BLACKLIST_PRESETS, FilterField, FilterPresets};
    use crate::ui_egui::roots::{Side, clear_root, set_root};
    use crate::ui_egui::tree_compare::PairResult;
    use crate::ui_egui::tree_filter::{DiffFilter, DuplicatesFilter, StateFilter};
    use crate::ui_egui::tree_find::FolderFind;
    use crate::ui_egui::tree_likeness::FolderLikeness;
    use crate::ui_egui::tree_sort::{SortKey, TreeSort};
    use crate::ui_egui::tree_tolerance::{SizeDelta, Tolerance};
    use eframe::egui;
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize},
        },
        time::{Duration, Instant},
    };
    use tempfile::{TempDir, tempdir};
    use zcommon::hash::HashService;
    use zcommon::logger::{LogBuffer, LogCollector};
    use zcommon_ui::keymap::{Action, Keymap, Shortcut};
    use zdiff_folder::likeness::{Likeness, Measure};
    use zdiff_folder::{
        comparison::{PairCompare, PairState},
        filter::{PathFilter, PatternList},
        fs::FileSystemModel,
        scan::{ContentHash, FlatEntry, FlatListing, HashKind, scan},
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

    #[test]
    fn a_patch_during_the_compare_updates_the_folder_likeness() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            fs::create_dir_all(root.join("sub")).unwrap();
            fs::create_dir_all(root.join("other")).unwrap();
            fs::write(root.join("sub/a.txt"), "a").unwrap();
            fs::write(root.join("other/c.txt"), "c").unwrap();
        }
        fs::write(left.path().join("sub/b.txt"), "left b").unwrap();
        fs::write(right.path().join("sub/b.txt"), "right b").unwrap();
        let mut state = AppStateCtx {
            likeness_bar: true,
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        state.build_two_folder_diff_rows_if_stale();
        let start = Instant::now();
        let (sender, paths) = state.tree_compare.pause(start);
        let send = |rel_path: &str, state: PairState, ratio: Option<f64>| {
            let i = paths.iter().position(|path| path == rel_path).unwrap();
            let result = PairResult {
                compare: Some(PairCompare { state, error: None }),
                likeness: ratio.map(|ratio| {
                    Ok(Likeness {
                        ratio,
                        measure: Measure::Bytes,
                        reason: None,
                    })
                }),
            };
            sender.send((i, result)).unwrap();
        };

        send("sub/a.txt", PairState::Same, None);
        send("sub/b.txt", PairState::Different, Some(0.5));
        state.update_compare(start + Duration::from_secs(1));

        assert!(state.tree_compare.is_comparing());
        let rows = state.two_folder_diff_visible_rows.as_ref().unwrap();
        let folder = |rel_path: &str| {
            let row = rows.iter().find(|row| row.rel_path == rel_path).unwrap();
            row.folder_likeness
        };
        assert_eq!(folder("sub"), Some(FolderLikeness::Known(4.5 / 8.0)));
        assert_eq!(folder(""), Some(FolderLikeness::Unknown));
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
    fn a_keymap_saved_with_the_old_f5_refresh_default_moves_to_the_new_default() {
        let f5 = Shortcut {
            key: egui::Key::F5,
            modifiers: egui::Modifiers::NONE,
        };
        let custom = Shortcut {
            key: egui::Key::F6,
            modifiers: egui::Modifiers::NONE,
        };
        let restore = |refresh: Vec<Shortcut>| {
            let mut state = AppStateCtx::default();
            *state.keymap.shortcuts_mut(FolderAction::Refresh) = refresh;
            let json = serde_json::to_string(&state).unwrap();
            serde_json::from_str::<AppStateCtx>(&json).unwrap().keymap
        };

        assert_eq!(
            restore(vec![f5]).shortcuts(FolderAction::Refresh),
            FolderAction::Refresh.defaults()
        );
        assert_eq!(
            restore(vec![custom]).shortcuts(FolderAction::Refresh),
            [custom]
        );
        assert_eq!(
            restore(vec![custom, f5]).shortcuts(FolderAction::Refresh),
            [custom, f5]
        );
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
    fn a_restored_app_shows_the_global_log_buffer() {
        let global = LogCollector::init().expect("no other logger in the test binary");
        let json = serde_json::to_string(&ZApp::new(global.clone())).unwrap();
        let restored: ZApp = serde_json::from_str(&json).unwrap();
        assert!(Arc::ptr_eq(&restored.log_buffer, &global));
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
    fn the_state_filter_and_show_parents_survive_a_restart() {
        for states in [StateFilter::Diff, StateFilter::Equal] {
            let mut state = AppStateCtx::default();
            state.diff_filter = DiffFilter {
                states,
                show_parents: false,
            };

            let json = serde_json::to_string(&state).unwrap();
            let restored: AppStateCtx = serde_json::from_str(&json).unwrap();

            assert_eq!(restored.diff_filter, state.diff_filter);
        }
    }

    #[test]
    fn a_state_saved_before_diff_only_existed_shows_every_row_with_parents() {
        let mut json: serde_json::Value = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut().unwrap().remove("diff_filter").unwrap();

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();

        assert_eq!(
            restored.diff_filter,
            DiffFilter {
                states: StateFilter::All,
                show_parents: true,
            }
        );
    }

    #[test]
    fn a_saved_diff_only_loads_as_the_diff_state_filter() {
        for (diff_only, states) in [(true, StateFilter::Diff), (false, StateFilter::All)] {
            let mut json: serde_json::Value = serde_json::to_value(AppStateCtx::default()).unwrap();
            json["diff_filter"] =
                serde_json::json!({ "diff_only": diff_only, "show_parents": false });

            let restored: AppStateCtx = serde_json::from_value(json).unwrap();

            assert_eq!(
                restored.diff_filter,
                DiffFilter {
                    states,
                    show_parents: false,
                }
            );
        }
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
    fn swapped_sides_survive_a_restart() {
        let state = AppStateCtx {
            sides_swapped: true,
            ..Default::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();

        assert!(restored.sides_swapped);
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

        state.diff_filter.states = StateFilter::Diff;
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "z", "z/b.txt"]);

        state.diff_filter.show_parents = false;
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "z/b.txt"]);

        state.diff_filter.states = StateFilter::Equal;
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "a.txt"]);

        state.diff_filter.states = StateFilter::All;
        state.build_two_folder_diff_rows_if_stale();
        assert_eq!(row_paths(&state), ["", "z", "z/b.txt", "a.txt"]);
    }

    #[test]
    fn the_equal_filter_on_fresh_trees_compares_the_pairs_it_hides_while_pending() {
        let (_left, _right, mut state) = b_different_trees();
        state.diff_filter.states = StateFilter::Equal;
        state.likeness_bar = true;

        finish_compare(&mut state);

        assert_eq!(row_paths(&state), ["", "a.txt"]);
        let views = [
            state.file_system_model_1_view.as_ref().unwrap(),
            state.file_system_model_2_view.as_ref().unwrap(),
        ];
        let b = views.map(|view| {
            let root = view.file_system.path(view.file_system.get_root_node_id());
            let id = view.file_system.find_path(root.join("b.txt")).unwrap();
            view.file_system.get_node(id).unwrap()
        });
        assert!(state.tree_compare.likeness_of("b.txt", b).is_some());
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
            .map(|view| view.file_system.path(view.file_system.get_root_node_id()))
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
    fn a_vcs_scan_with_nothing_found_yet_says_it_is_listing() {
        let depot = Path::new("//depot/main@12");
        let git = Path::new(r"D:\repo\src@git:main");
        let local = Path::new(r"D:\repo\src");
        assert_eq!(
            scan_progress_text(Side::Folder1, depot, 0),
            "Listing Folder 1 from p4: //depot/main@12"
        );
        assert_eq!(
            scan_progress_text(Side::Folder2, git, 0),
            r"Listing Folder 2 from git: D:\repo\src@git:main"
        );
        assert_eq!(
            scan_progress_text(Side::Folder1, depot, 7),
            "Scanning Folder 1: 7 entries in //depot/main@12"
        );
        assert_eq!(
            scan_progress_text(Side::Folder1, local, 0),
            r"Scanning Folder 1: 0 entries in D:\repo\src"
        );
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

    const FRAME_AT_60_HZ: Duration = Duration::from_micros(16_667);

    fn in_memory_view(root: &Path, files: Vec<FlatEntry>) -> FileSystemView {
        let listing = FlatListing::new(root, files, Vec::new());
        let output = scan(
            &listing,
            root,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )
        .unwrap()
        .unwrap();
        FileSystemView::new(Arc::new(output.model))
    }

    fn files_50k(side: usize) -> Vec<FlatEntry> {
        (0..50_000)
            .map(|i| FlatEntry {
                rel_path: format!(
                    "dir_{:02}/sub_{:02}/leaf_{:02}/some_longer_file_name_{:04}.txt",
                    i / 5000,
                    i / 500 % 10,
                    i / 50 % 10,
                    i % 50
                ),
                size: 1,
                hash: Some(ContentHash {
                    kind: HashKind::GitBlob,
                    value: if i % 20 == 0 {
                        format!("{i} on side {side}")
                    } else {
                        i.to_string()
                    },
                }),
            })
            .collect()
    }

    fn median_frame(
        ctx: &egui::Context,
        app: &mut ZApp,
        frames: usize,
        events: impl Fn() -> Vec<egui::Event>,
    ) -> Duration {
        let mut frame = eframe::Frame::_new_kittest();
        let mut times: Vec<Duration> = (0..frames)
            .map(|_| {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1600.0, 1000.0),
                    )),
                    events: events(),
                    ..Default::default()
                };
                let start = Instant::now();
                let output = ctx.run(input, |ctx| eframe::App::update(app, ctx, &mut frame));
                let _ = ctx.tessellate(output.shapes, output.pixels_per_point);
                start.elapsed()
            })
            .collect();
        times.sort();
        times[frames / 2]
    }

    #[test]
    fn idle_and_scrolling_frames_of_a_100k_file_folder_diff_fit_a_60_hz_frame() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        let mut state = AppStateCtx {
            file_system_1_root_path: Some(left.path().to_path_buf()),
            file_system_2_root_path: Some(right.path().to_path_buf()),
            file_system_model_1_view: Some(in_memory_view(left.path(), files_50k(1))),
            file_system_model_2_view: Some(in_memory_view(right.path(), files_50k(2))),
            ..Default::default()
        };
        finish_compare(&mut state);
        let mut app = ZApp::new(LogBuffer::default());
        app.state = Some(AppState::Idle(state));
        let ctx = egui::Context::default();

        median_frame(&ctx, &mut app, 3, Vec::new);
        let idle = median_frame(&ctx, &mut app, 31, Vec::new);
        let scrolling = median_frame(&ctx, &mut app, 31, || {
            vec![
                egui::Event::PointerMoved(egui::pos2(400.0, 500.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Page,
                    delta: egui::vec2(0.0, -1.0),
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        });

        assert!(idle < FRAME_AT_60_HZ, "idle frame took {idle:?}");
        assert!(
            scrolling < FRAME_AT_60_HZ,
            "scrolling frame took {scrolling:?}"
        );
    }

    #[test]
    fn idle_and_pointer_moved_frames_of_a_100k_file_diff_expanded_to_its_diffs_fit_a_60_hz_frame() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        let mut state = AppStateCtx {
            file_system_1_root_path: Some(left.path().to_path_buf()),
            file_system_2_root_path: Some(right.path().to_path_buf()),
            file_system_model_1_view: Some(in_memory_view(left.path(), files_50k(1))),
            file_system_model_2_view: Some(in_memory_view(right.path(), files_50k(2))),
            ..Default::default()
        };
        finish_compare(&mut state);
        let rows = state.two_folder_diff_visible_rows.clone().unwrap();
        let views = [
            state.file_system_model_1_view.as_mut().unwrap(),
            state.file_system_model_2_view.as_mut().unwrap(),
        ];
        for row in rows.iter() {
            if let DiffState::Same(first, second) = row.diff_state {
                views[0].collapsed.insert(first, true);
                views[1].collapsed.insert(second, true);
            }
        }
        let mut app = ZApp::new(LogBuffer::default());
        app.state = Some(AppState::Idle(state));
        let ctx = egui::Context::default();

        median_frame(&ctx, &mut app, 3, Vec::new);
        let idle = median_frame(&ctx, &mut app, 31, Vec::new);
        let pointer_moved = median_frame(&ctx, &mut app, 31, || {
            vec![egui::Event::PointerMoved(egui::pos2(400.0, 500.0))]
        });

        assert!(idle < FRAME_AT_60_HZ, "idle frame took {idle:?}");
        assert!(
            pointer_moved < FRAME_AT_60_HZ,
            "pointer-moved frame took {pointer_moved:?}"
        );
    }

    fn frame_texts(
        ctx: &egui::Context,
        app: &mut ZApp,
        screen: egui::Vec2,
        events: Vec<egui::Event>,
    ) -> Vec<String> {
        fn collect(shape: &egui::Shape, texts: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => texts.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, texts)),
                _ => {}
            }
        }
        let mut frame = eframe::Frame::_new_kittest();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
            events,
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| eframe::App::update(app, ctx, &mut frame));
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut texts);
        }
        texts
    }

    #[test]
    fn pressing_the_folder_tree_scroll_bar_scrolls_at_a_fractional_zoom() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        let mut state = AppStateCtx {
            file_system_1_root_path: Some(left.path().to_path_buf()),
            file_system_2_root_path: Some(right.path().to_path_buf()),
            file_system_model_1_view: Some(in_memory_view(left.path(), files_50k(1))),
            file_system_model_2_view: Some(in_memory_view(right.path(), files_50k(2))),
            ..Default::default()
        };
        finish_compare(&mut state);
        let mut app = ZApp::new(LogBuffer::default());
        app.state = Some(AppState::Idle(state));
        let ctx = egui::Context::default();
        ctx.set_zoom_factor(1.25);
        let screen = egui::vec2(1600.0, 1000.0);
        let bar = egui::pos2(screen.x - 10.0, screen.y / 2.0);
        let shows_first_folder = |texts: &[String]| texts.iter().any(|t| t.ends_with("dir_00"));

        for _ in 0..3 {
            frame_texts(&ctx, &mut app, screen, Vec::new());
        }
        let before = frame_texts(&ctx, &mut app, screen, vec![egui::Event::PointerMoved(bar)]);
        for pressed in [true, false] {
            frame_texts(
                &ctx,
                &mut app,
                screen,
                vec![egui::Event::PointerButton {
                    pos: bar,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        let after = frame_texts(&ctx, &mut app, screen, Vec::new());

        assert!(shows_first_folder(&before));
        assert!(!shows_first_folder(&after));
    }

    fn pending_files() -> Vec<FlatEntry> {
        (0..3000)
            .map(|i| {
                let (dir, sub) = (i / 300, i / 30 % 10);
                FlatEntry {
                    rel_path: format!("dir_{dir}/sub_{sub}/f_{dir}_{sub}_{:02}.txt", i % 30),
                    size: 1,
                    hash: None,
                }
            })
            .collect()
    }

    fn files_in_view(texts: Vec<String>) -> Vec<String> {
        texts
            .into_iter()
            .filter(|text| text.starts_with("f_"))
            .collect()
    }

    fn folder_of(file: &str) -> &str {
        file.rsplit_once('_').unwrap().0
    }

    fn top_files_across_the_compare_finish(
        diff_filter: DiffFilter,
        tree_sort: TreeSort,
    ) -> (String, Vec<String>) {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        let mut state = AppStateCtx {
            file_system_1_root_path: Some(left.path().to_path_buf()),
            file_system_2_root_path: Some(right.path().to_path_buf()),
            file_system_model_1_view: Some(in_memory_view(left.path(), pending_files())),
            file_system_model_2_view: Some(in_memory_view(right.path(), pending_files())),
            diff_filter,
            tree_sort,
            ..Default::default()
        };
        state.build_two_folder_diff_rows_if_stale();
        let (sender, paths) = state.tree_compare.pause(Instant::now());
        let mut app = ZApp::new(LogBuffer::default());
        app.state = Some(AppState::Idle(state));
        let ctx = egui::Context::default();
        let screen = egui::vec2(1600.0, 1000.0);
        let wheel = || {
            vec![
                egui::Event::PointerMoved(egui::pos2(400.0, 500.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Page,
                    delta: egui::vec2(0.0, -1.0),
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        for _ in 0..3 {
            frame_texts(&ctx, &mut app, screen, wheel());
        }
        for _ in 0..120 {
            frame_texts(&ctx, &mut app, screen, Vec::new());
        }
        let before = files_in_view(frame_texts(&ctx, &mut app, screen, Vec::new())).remove(0);

        for (i, path) in paths.iter().enumerate() {
            let state = if path.ends_with("0.txt") {
                PairState::Different
            } else {
                PairState::Same
            };
            let result = PairResult {
                compare: Some(PairCompare { state, error: None }),
                likeness: None,
            };
            sender.send((i, result)).unwrap();
        }
        drop(sender);
        let comparing = |app: &ZApp| match &app.state {
            Some(AppState::Idle(state)) => state.tree_compare.is_comparing(),
            _ => unreachable!("the app is idle"),
        };
        while comparing(&app) {
            frame_texts(&ctx, &mut app, screen, Vec::new());
        }
        let mut after = files_in_view(frame_texts(&ctx, &mut app, screen, Vec::new()));
        after.truncate(4);
        (before, after)
    }

    #[test]
    fn the_compare_finishing_under_the_diff_filter_keeps_the_scrolled_to_folder_in_view() {
        let diff_only = DiffFilter {
            states: StateFilter::Diff,
            show_parents: true,
        };

        let (before, after) = top_files_across_the_compare_finish(diff_only, TreeSort::default());

        assert!(!before.starts_with("f_0_0_"), "scrolled: {before}");
        assert!(
            after
                .iter()
                .any(|file| folder_of(file) == folder_of(&before)),
            "{before} -> {after:?}"
        );
    }

    #[test]
    fn the_compare_finishing_under_a_state_sort_keeps_the_top_row_in_place() {
        let by_state = TreeSort::default().clicked(SortKey::State);

        let (before, after) = top_files_across_the_compare_finish(DiffFilter::default(), by_state);

        assert!(!before.starts_with("f_0_0_"), "scrolled: {before}");
        assert!(after.contains(&before), "{before} -> {after:?}");
    }

    fn repaint_delay(ctx: &egui::Context, app: &mut ZApp) -> Duration {
        let mut frame = eframe::Frame::_new_kittest();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| eframe::App::update(app, ctx, &mut frame));
        output.viewport_output[&egui::ViewportId::ROOT].repaint_delay
    }

    fn settled_repaint_delay(app: &mut ZApp) -> Duration {
        let ctx = egui::Context::default();
        for _ in 0..5 {
            repaint_delay(&ctx, app);
        }
        repaint_delay(&ctx, app)
    }

    fn shown_app(hash_service: HashService) -> (TempDir, TempDir, ZApp) {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            fs::write(root.join("a.txt"), "same size").unwrap();
            fs::write(root.join("b.txt"), "same_size").unwrap();
        }
        let mut state = AppStateCtx {
            hash_service,
            file_system_1_root_path: Some(left.path().to_path_buf()),
            file_system_2_root_path: Some(right.path().to_path_buf()),
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        finish_compare(&mut state);
        let mut app = ZApp::new(LogBuffer::default());
        app.state = Some(AppState::Idle(state));
        (left, right, app)
    }

    #[test]
    fn an_idle_folder_diff_stops_repainting() {
        let (_left, _right, mut app) = shown_app(HashService::new(0));

        assert_eq!(settled_repaint_delay(&mut app), Duration::MAX);
    }

    #[test]
    fn an_idle_folder_diff_with_the_frame_time_breakdown_open_stops_repainting() {
        let (_left, _right, mut app) = shown_app(HashService::new(0));
        app.show_frame_breakdown = true;
        let ctx = egui::Context::default();
        for _ in 0..20 {
            repaint_delay(&ctx, &mut app);
        }

        assert_eq!(repaint_delay(&ctx, &mut app), Duration::MAX);
    }

    #[test]
    fn the_fps_label_turning_idle_costs_one_frame() {
        let (_left, _right, mut app) = shown_app(HashService::new(0));
        app.show_frame_breakdown = true;
        let ctx = egui::Context::default();
        for _ in 0..20 {
            repaint_delay(&ctx, &mut app);
        }

        std::thread::sleep(Duration::from_millis(1300));
        assert!(ctx.has_requested_repaint());
        let idle_frame = repaint_delay(&ctx, &mut app);
        std::thread::sleep(Duration::from_millis(1300));

        assert_eq!(idle_frame, Duration::MAX);
        assert!(!ctx.has_requested_repaint());
    }

    #[test]
    fn a_queued_hash_keeps_the_duplicate_tab_polling() {
        let (_left, _right, mut app) = shown_app(HashService::new(0));
        app.diff_mode_path_diff = false;

        assert!(settled_repaint_delay(&mut app) <= Duration::from_millis(100));
    }

    #[test]
    fn an_idle_duplicate_tab_stops_repainting() {
        let (_left, _right, mut app) = shown_app(HashService::new(1));
        app.diff_mode_path_diff = false;
        let ctx = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(10);
        while repaint_delay(&ctx, &mut app) != Duration::MAX {
            assert!(
                Instant::now() < deadline,
                "the duplicate tab kept repainting"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
