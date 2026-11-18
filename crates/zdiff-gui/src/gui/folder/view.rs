use eframe::egui::{self, Layout, PointerButton};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use zcommon::{
    find::{FindError, Matcher},
    hash::HashService,
    job::Job,
    kinds::KindTable,
    lru::LruCache,
    universal_path::UniversalPath,
    vcs::{git::GitCommand, p4::P4Command},
};
use zcommon_ui::{
    find_popup::{FindPopupEvent, find_popup},
    frame_stats,
    keymap::Keymap,
};
use zdiff_folder::{
    external_diff_tool::DiffToolConfig, filter::PathFilter, fs::FileSystemModel, scan::ScanOutput,
    uasset::BlueprintDetector, vcs_source::scan_side,
};

use crate::folder::{
    duplicate_files_pane::DuplicateFilesPane,
    export::{folder_diff_report, save_report},
    folder_keymap::{self, FolderAction, KeybindingsWindow},
    fs_tree::{FileSystemView, RowsId, VisibleRowTwoFolderDiff, apply_blueprint_results},
    panes::{PathDiffView, TreeBehavior, ViewChoice},
    path_diff_pane::{FilterPresets, PathDiffPane},
    revision_open,
    roots::Side,
    status_line::{
        RowCounts, ScanSummary, ToolMessage, count_rows, counts_text, ui_status_summary,
        ui_tool_message,
    },
    tools_window::ToolsWindow,
    tree_compare::{CompareUpdate, PairLikeness, TreeCompare, patch_row_states},
    tree_filter::{DiffFilter, DuplicatesFilter, arrange_two_folder_rows},
    tree_find::{FindChipEvent, FolderFind, TreeFindView, find_mask},
    tree_menu::RootRequests,
    tree_sort::TreeSort,
    tree_tolerance::Tolerance,
};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FolderState {
    #[serde(skip)]
    pub hash_service: HashService,
    #[serde(skip)]
    pub file_system_1_root_path: Option<PathBuf>,
    #[serde(skip)]
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
    #[serde(skip)]
    two_folder_diff_rows_tolerance: Tolerance,
    #[serde(default)]
    pub tolerance: Tolerance,
    #[serde(skip)]
    built_rows_cache: LruCache<BuiltRowsKey, Vec<VisibleRowTwoFolderDiff>>,
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

    #[serde(skip)]
    conflict_map: HashMap<String, Vec<PathBuf>>,
    #[serde(skip)]
    conflict_map_resolved: HashMap<String, PathBuf>,

    diff_config: DiffToolConfig,
    #[serde(skip)]
    tool_message: Option<ToolMessage>,
    #[serde(default, deserialize_with = "folder_keymap::deserialize_keymap")]
    keymap: Keymap<FolderAction>,
    #[serde(skip)]
    root_requests: RootRequests,
}

struct BuiltRowsKey {
    models: [Weak<FileSystemModel>; 2],
    path_filter: PathFilter,
    tolerance: Tolerance,
}

impl BuiltRowsKey {
    fn same_models(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.models[0], &other.models[0])
            && Weak::ptr_eq(&self.models[1], &other.models[1])
    }
}

impl PartialEq for BuiltRowsKey {
    fn eq(&self, other: &Self) -> bool {
        self.same_models(other)
            && self.path_filter == other.path_filter
            && self.tolerance == other.tolerance
    }
}

fn built_rows_bytes(rows: &[VisibleRowTwoFolderDiff]) -> usize {
    let paths: usize = rows.iter().map(|row| row.rel_path.len()).sum();
    paths + size_of_val(rows)
}

impl FolderState {
    fn rows_inputs_changed(&self) -> bool {
        self.two_folder_diff_rows_filter != self.path_filter
            || self.two_folder_diff_rows_sort != self.tree_sort
            || self.two_folder_diff_rows_diff_filter != self.diff_filter
            || self.two_folder_diff_rows_tolerance != self.tolerance
    }

    // Without a shown view the rebuild never records its inputs, so they would stay stale.
    fn rows_are_stale(&self) -> bool {
        let shown =
            self.file_system_model_1_view.is_some() || self.file_system_model_2_view.is_some();
        shown && (self.rows_inputs_changed() || self.two_folder_diff_rows_find != self.find_applied)
    }

    /// Rebuilds the two-folder rows when they were dropped or the filter or sort changed since
    /// they were built. Here rather than at the edit, so the rows are never missing mid-frame.
    fn build_two_folder_diff_rows_if_stale(&mut self) {
        self.tree_compare.retain_for([
            self.file_system_model_1_view.as_ref(),
            self.file_system_model_2_view.as_ref(),
        ]);
        let measure_likeness = self.likeness_bar || self.tolerance.uses_likeness();
        let likeness_changed = self.tree_compare.set_likeness(measure_likeness);
        if self.rows_inputs_changed() {
            self.two_folder_diff_visible_rows = None;
        }
        if self.two_folder_diff_visible_rows.is_none() {
            if self.file_system_model_1_view.is_some() || self.file_system_model_2_view.is_some() {
                let model = |view: &Option<FileSystemView>| {
                    view.as_ref()
                        .map_or_else(Weak::new, |v| Arc::downgrade(&v.file_system))
                };
                let key = BuiltRowsKey {
                    models: [
                        model(&self.file_system_model_1_view),
                        model(&self.file_system_model_2_view),
                    ],
                    path_filter: self.path_filter.clone(),
                    tolerance: self.tolerance,
                };
                self.built_rows_cache
                    .retain(|cached| cached.same_models(&key));
                let cached = self.built_rows_cache.get(&key).cloned();
                let built = cached.is_none();
                let rows = cached.or_else(|| self.build_rows(true));
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
                    // Rows still waiting on a compare or blueprint check get patched in place.
                    if built && !self.tree_compare.is_comparing() && !self.blueprints_pending() {
                        let bytes = built_rows_bytes(rows);
                        self.built_rows_cache.insert(key, rows.clone(), bytes);
                    }
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
        let _scope = frame_stats::scope("Find narrowing");
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
        let _scope = frame_stats::scope("Build rows");
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
        let update = self.tree_compare.poll(now);
        // A compare resumed after a cancel brings states the cached rows still show as Pending.
        if !matches!(update, CompareUpdate::Idle) {
            self.built_rows_cache.clear();
        }
        match update {
            CompareUpdate::Idle => {}
            CompareUpdate::Patch => {
                let _scope = frame_stats::scope("Patch row states");
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
            let _scope = frame_stats::scope("Apply blueprint results");
            self.blueprints_applied = Some(now);
            let results = std::mem::take(&mut self.blueprint_results);
            self.built_rows_cache.clear();
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
        self.blueprints_pending()
    }

    fn blueprints_pending(&self) -> bool {
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

    fn ui_status_rows_bottom_up(&mut self, ui: &mut egui::Ui) -> bool {
        let working = self.file_system_1_scan.is_scanning()
            || self.file_system_2_scan.is_scanning()
            || self.tree_compare.is_comparing();
        let shown =
            self.file_system_model_1_view.is_some() || self.file_system_model_2_view.is_some();
        if working {
            ui_tool_message(ui, self.tool_message.as_ref());
            ui_compare_progress(ui, &mut self.tree_compare);
            ui_scan_progress(ui, Side::Folder2, &mut self.file_system_2_scan);
            ui_scan_progress(ui, Side::Folder1, &mut self.file_system_1_scan);
        } else if shown {
            if let Some(rows) = self.two_folder_diff_visible_rows.as_deref()
                && self
                    .row_counts
                    .as_ref()
                    .is_none_or(|(counted, _)| *counted != self.two_folder_diff_rows_id)
            {
                self.row_counts = Some((self.two_folder_diff_rows_id, count_rows(rows)));
            }
            let row_counts = self
                .two_folder_diff_visible_rows
                .as_ref()
                .and(self.row_counts.as_ref().map(|(_, counts)| counts));
            ui_tool_message(ui, self.tool_message.as_ref());
            ui_status_summary(
                ui,
                row_counts,
                [
                    self.file_system_1_scan.shown(),
                    self.file_system_2_scan.shown(),
                ],
                self.tree_compare.compare_time(),
                self.tolerance.enabled,
            );
        }
        working || shown
    }

    fn scan_error(&self) -> Option<String> {
        let sides = [
            (Side::Folder1, &self.file_system_1_scan, &self.file_system_1_root_path),
            (Side::Folder2, &self.file_system_2_scan, &self.file_system_2_root_path),
        ];
        let lines: Vec<String> = sides
            .into_iter()
            .filter_map(|(side, scan, root)| Some((side, scan.error_for(root.as_deref())?)))
            .map(|(side, error)| format!("{side}: {error}"))
            .collect();
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    fn update_scans(&mut self, repaint: &egui::Context) {
        let _scope = frame_stats::scope("Scan polling");
        update_side_scan(
            Side::Folder1,
            self.file_system_1_root_path.as_deref(),
            &mut self.file_system_model_1_view,
            &mut self.file_system_1_scan,
            &mut self.two_folder_diff_visible_rows,
            repaint,
        );
        update_side_scan(
            Side::Folder2,
            self.file_system_2_root_path.as_deref(),
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
    cancel: bool,
    shown: Option<ScanSummary>,
    failed: Option<(PathBuf, String)>,
}

impl SideScan {
    pub fn request_rescan(&mut self) {
        self.rescan = true;
    }

    // Applied by the next poll: this frame's rows are still drawn from the view.
    pub fn request_cancel(&mut self) {
        self.cancel = true;
    }

    fn error_for(&self, root: Option<&Path>) -> Option<&str> {
        let (failed, error) = self.failed.as_ref()?;
        (root == Some(failed)).then_some(error)
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
    failed: Option<&Path>,
    rescan: bool,
) -> ScanAction {
    let Some(root) = root else {
        return ScanAction::Clear;
    };
    if job == Some(root) {
        ScanAction::Keep
    } else if (shown == Some(root) || failed == Some(root)) && !rescan {
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

fn fail_scan(
    side: Side,
    root: PathBuf,
    error: String,
    view: &mut Option<FileSystemView>,
    scan: &mut SideScan,
    rows: &mut Option<Vec<VisibleRowTwoFolderDiff>>,
) {
    log::error!("{side}: {}: {error}", root.display());
    scan.shown = None;
    scan.failed = Some((root, error));
    if view.take().is_some() {
        *rows = None;
    }
}

const SCAN_CANCELLED: &str = "Scan cancelled";

pub(crate) fn update_side_scan(
    side: Side,
    root: Option<&Path>,
    view: &mut Option<FileSystemView>,
    scan: &mut SideScan,
    rows: &mut Option<Vec<VisibleRowTwoFolderDiff>>,
    repaint: &egui::Context,
) {
    if std::mem::take(&mut scan.cancel)
        && let Some(job) = scan.job.take()
    {
        scan.rescan = false;
        fail_scan(side, job.root, SCAN_CANCELLED.to_owned(), view, scan, rows);
    }
    let action = scan_action(
        root,
        shown_root(view.as_ref()).as_deref(),
        scan.job.as_ref().map(ScanJob::root),
        scan.failed.as_ref().map(|(root, _)| root.as_path()),
        scan.rescan,
    );
    match action {
        ScanAction::Keep => {}
        ScanAction::Start(path) => {
            scan.job = Some(ScanJob::start(path, repaint.clone()));
            scan.rescan = false;
            scan.failed = None;
            return;
        }
        ScanAction::Stop => scan.job = None,
        ScanAction::Clear => {
            scan.job = None;
            scan.rescan = false;
            scan.shown = None;
            scan.failed = None;
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
            let job = scan.job.take().expect("checked above");
            let error = "the scan stopped without a result".to_owned();
            fail_scan(side, job.root, error, view, scan, rows);
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
        Some(Err(e)) => fail_scan(side, job.root, e.to_string(), view, scan, rows),
        None => fail_scan(side, job.root, SCAN_CANCELLED.to_owned(), view, scan, rows),
    }
}

fn ui_scan_progress(ui: &mut egui::Ui, side: Side, scan: &mut SideScan) {
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
        scan.request_cancel();
        ui.ctx().request_repaint();
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
    root: Option<&Path>,
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

pub fn root_of_side(side: &UniversalPath) -> Option<PathBuf> {
    match side {
        side if side.is_empty() => None,
        UniversalPath::Local(folder) => Some(folder.clone()),
        side => Some(PathBuf::from(side.to_string())),
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct FolderView {
    state: FolderState,
    panes: FolderPanes,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct FolderPanes {
    path_diff: PathDiffPane,
    duplicates: DuplicateFilesPane,
    view: ViewChoice,

    #[serde(skip)]
    tools_window: ToolsWindow,
    #[serde(skip)]
    keybindings_window: KeybindingsWindow,
    #[serde(skip)]
    find_open: bool,
    #[serde(skip)]
    last_discard_frame: Option<u64>,
    #[serde(skip)]
    polled: PolledInput,
}

#[derive(Default)]
struct PolledInput {
    actions: Vec<FolderAction>,
    keyboard_taken: bool,
}

const LONG_FRAME: Duration = Duration::from_millis(200);
const SCAN_PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const BLUEPRINT_APPLY_INTERVAL: Duration = Duration::from_millis(250);

// A drag changes the rows' inputs every frame; discarding each one would double its cost.
fn discard_stale_rows(stale: bool, last_discard_frame: &mut Option<u64>, frame: u64) -> bool {
    let discard = stale && last_discard_frame.is_none_or(|last| last + 1 < frame);
    if discard {
        *last_discard_frame = Some(frame);
    }
    discard
}

pub fn on_exit() {
    revision_open::remove_all();
}

impl FolderView {
    pub fn poll(&mut self, ctx: &egui::Context, map_claims_back: bool) {
        let Self { state, panes } = self;
        panes.poll(ctx, state, map_claims_back);
    }

    pub fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        let Self { state, panes } = self;
        panes.toolbar_ui(ui, state);
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, swapped: bool, h_scroll_linked: bool) {
        let frame_start = Instant::now();
        let Self { state, panes } = self;
        panes.ui(ui, state, swapped, h_scroll_linked);
        // The panes drew the old rows under the new flags; the next pass rebuilds
        // them before drawing, and only that pass is painted.
        if discard_stale_rows(
            state.rows_are_stale(),
            &mut panes.last_discard_frame,
            ui.ctx().cumulative_frame_nr(),
        ) {
            ui.ctx().request_discard("folder rows are stale");
        }
        let frame_time = frame_start.elapsed();
        if frame_time > LONG_FRAME {
            log::warn!("UI frame took {} ms", frame_time.as_millis());
        }
    }

    pub fn title_counts(&self) -> String {
        let rows = self.state.two_folder_diff_visible_rows.as_ref();
        rows.map_or(0, |rows| rows.len()).to_string()
    }

    pub fn keymap_mut(&mut self) -> &mut Keymap<FolderAction> {
        &mut self.state.keymap
    }

    pub fn keymap(&self) -> &Keymap<FolderAction> {
        &self.state.keymap
    }

    pub fn duplicates_shown(&self) -> bool {
        self.panes.view == ViewChoice::Duplicates
    }

    pub fn open_find(&mut self) {
        self.panes.find_open = !self.duplicates_shown();
    }

    pub fn request_export(&mut self) {
        self.panes.polled.actions.push(FolderAction::Export);
    }

    pub fn open_tools_window(&mut self) {
        self.panes.tools_window.open = true;
    }

    pub fn show_windows(&mut self, ctx: &egui::Context) {
        let Self { state, panes } = self;
        if panes.tools_window.open {
            panes.tools_window.show(ctx, &mut state.diff_config);
        }
        if panes.keybindings_window.open {
            panes.keybindings_window.show(ctx, &mut state.keymap);
        }
    }

    #[cfg(debug_assertions)]
    pub fn clear_models(&mut self) {
        self.state.file_system_model_1_view = None;
        self.state.file_system_model_2_view = None;
        self.state.two_folder_diff_visible_rows = None;
    }

    pub fn set_roots(&mut self, roots: [Option<PathBuf>; 2]) {
        let stored = [
            &mut self.state.file_system_1_root_path,
            &mut self.state.file_system_2_root_path,
        ];
        for (stored, root) in stored.into_iter().zip(roots) {
            let universal = |root: &Option<PathBuf>| root.as_deref().map(UniversalPath::from);
            if *stored != root && universal(stored) != universal(&root) {
                *stored = root;
            }
        }
    }

    pub fn side_error(&self, side: Side) -> Option<&str> {
        let state = &self.state;
        let (scan, root) = match side {
            Side::Folder1 => (&state.file_system_1_scan, &state.file_system_1_root_path),
            Side::Folder2 => (&state.file_system_2_scan, &state.file_system_2_root_path),
        };
        scan.error_for(root.as_deref())
    }

    pub fn is_scanning(&self, side: Side) -> bool {
        match side {
            Side::Folder1 => self.state.file_system_1_scan.is_scanning(),
            Side::Folder2 => self.state.file_system_2_scan.is_scanning(),
        }
    }

    pub fn refresh(&mut self) {
        self.state.refresh();
    }

    pub fn take_root_requests(&mut self) -> RootRequests {
        std::mem::take(&mut self.state.root_requests)
    }

    #[cfg(test)]
    pub(crate) fn find_open(&self) -> bool {
        self.panes.find_open
    }

    pub fn roots(&self) -> [Option<PathBuf>; 2] {
        [
            self.state.file_system_1_root_path.clone(),
            self.state.file_system_2_root_path.clone(),
        ]
    }
}

impl Default for FolderPanes {
    fn default() -> Self {
        Self {
            path_diff: PathDiffPane::new(None),
            duplicates: DuplicateFilesPane::new(None),
            view: ViewChoice::Tree,
            tools_window: ToolsWindow::default(),
            keybindings_window: KeybindingsWindow::default(),
            find_open: false,
            last_discard_frame: None,
            polled: PolledInput::default(),
        }
    }
}

impl FolderPanes {
    fn dispatch(
        &mut self,
        ctx: &egui::Context,
        keymap: &Keymap<FolderAction>,
        map_claims_back: bool,
    ) {
        // Read before any widget runs: a single-line TextEdit gives up focus on Enter without
        // consuming the key, so a later check would hand that Enter to the tree cursor.
        let capturing = self.keybindings_window.is_capturing();
        let keyboard_taken = ctx.wants_keyboard_input() || capturing;
        let actions = if capturing {
            Vec::new()
        } else {
            let _scope = frame_stats::scope("Folder shortcut dispatch");
            ctx.input(|i| {
                let claimed = |e: &egui::Event| {
                    map_claims_back
                        && matches!(
                            e,
                            egui::Event::PointerButton {
                                button: PointerButton::Extra1,
                                ..
                            }
                        )
                };
                let events: Vec<_> = i.events.iter().filter(|e| !claimed(e)).cloned().collect();
                keymap.dispatch(&events, keyboard_taken)
            })
        };
        self.polled = PolledInput {
            actions,
            keyboard_taken,
        };
    }

    fn apply_view_actions(&mut self) {
        let actions = &self.polled.actions;
        if actions.contains(&FolderAction::OpenKeybindings) {
            self.keybindings_window.open = true;
        }
        if actions.contains(&FolderAction::OpenDiffTools) {
            self.tools_window.open = true;
        }
        let toggled = |action| actions.contains(&action);
        let choice = match self.view {
            ViewChoice::Duplicates if toggled(FolderAction::ToggleDiffMode) => {
                Some(self.path_diff.view.into())
            }
            ViewChoice::Duplicates => None,
            _ if toggled(FolderAction::ToggleDiffMode) => Some(ViewChoice::Duplicates),
            ViewChoice::Tree if toggled(FolderAction::ToggleMap) => Some(ViewChoice::Map),
            ViewChoice::Map if toggled(FolderAction::ToggleMap) => Some(ViewChoice::Tree),
            ViewChoice::Tree | ViewChoice::Map => None,
        };
        if actions.contains(&FolderAction::Find) {
            self.find_open = true;
        }
        if let Some(choice) = choice {
            self.apply_view_choice(choice);
        }
        if self.view == ViewChoice::Duplicates {
            self.find_open = false;
        }
    }

    fn with_pane_ctx(
        &mut self,
        ctx: &egui::Context,
        app_ctx: &mut FolderState,
        draw: impl FnOnce(&mut PathDiffPane, &mut DuplicateFilesPane, &mut TreeBehavior),
    ) {
        let mut diff_action_triggered = false;
        let scan_error = app_ctx.scan_error();
        let scanning =
            app_ctx.file_system_1_scan.is_scanning() || app_ctx.file_system_2_scan.is_scanning();
        let mut path_diff_view = PathDiffView {
            scanning,
            file_system_1_view: &mut app_ctx.file_system_model_1_view,
            file_system_2_view: &mut app_ctx.file_system_model_2_view,
            file_system_1_scan: &mut app_ctx.file_system_1_scan,
            visible_rows: &mut app_ctx.two_folder_diff_visible_rows,
            rows_id: app_ctx.two_folder_diff_rows_id,
            scan_error,
            root_requests: &mut app_ctx.root_requests,
        };
        let mut find_view = TreeFindView {
            matcher: app_ctx.find_matcher.as_ref(),
            match_path: app_ctx.find_applied.match_path,
            chip: (!self.find_open && app_ctx.find_matcher.is_some())
                .then_some(app_ctx.find_applied.query.text.as_str()),
            chip_event: None,
        };
        let mut view_request = None;
        let mut behavior = TreeBehavior {
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
            find: &mut find_view,
            path_diff_view: &mut path_diff_view,
            keyboard_taken: self.polled.keyboard_taken,
            actions: &self.polled.actions,
            view_request: &mut view_request,
        };

        draw(&mut self.path_diff, &mut self.duplicates, &mut behavior);
        let chip_event = behavior.find.chip_event;
        if let Some(choice) = view_request {
            self.apply_view_choice(choice);
            ctx.request_repaint();
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
    }

    fn toolbar_ui(&mut self, ui: &mut egui::Ui, app_ctx: &mut FolderState) {
        let ctx = &ui.ctx().clone();
        let view = self.view;
        self.with_pane_ctx(ctx, app_ctx, |path_diff, duplicates, behavior| match view {
            ViewChoice::Duplicates => {
                let pane_ctx = &mut behavior.create_duplicate_files_ctx();
                duplicates.update(ctx, pane_ctx);
                duplicates.toolbar_ui(ui, pane_ctx);
            }
            ViewChoice::Tree | ViewChoice::Map => {
                path_diff.toolbar_ui(ui, &mut behavior.create_path_diff_ctx());
            }
        });
    }

    fn ui(
        &mut self,
        ui: &mut egui::Ui,
        app_ctx: &mut FolderState,
        swapped: bool,
        h_scroll_linked: bool,
    ) {
        let ctx = &ui.ctx().clone();
        let view = self.view;
        if self.polled.actions.contains(&FolderAction::Export) && view != ViewChoice::Duplicates {
            app_ctx.export_folder_diff();
        }
        if self.find_open {
            let _scope = frame_stats::scope("Find popup");
            self.show_find(ctx, app_ctx);
        }
        ui.with_layout(Layout::bottom_up(egui::Align::Min), |ui| {
            let timed = frame_stats::scope("Status line");
            if app_ctx.ui_status_rows_bottom_up(ui) {
                ui.separator();
            }
            drop(timed);
            ui.with_layout(Layout::top_down(egui::Align::Min), |ui| {
                let _scope = frame_stats::scope("Folder panes");
                self.with_pane_ctx(ctx, app_ctx, |path_diff, duplicates, behavior| match view {
                    ViewChoice::Duplicates => {
                        duplicates.body_ui(ui, &mut behavior.create_duplicate_files_ctx());
                    }
                    ViewChoice::Tree | ViewChoice::Map => path_diff.body_ui(
                        ui,
                        &mut behavior.create_path_diff_ctx(),
                        swapped,
                        h_scroll_linked,
                    ),
                });
            });
        });
        self.polled = PolledInput::default();
    }

    fn show_find(&mut self, ctx: &egui::Context, app_ctx: &mut FolderState) {
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

    fn apply_view_choice(&mut self, choice: ViewChoice) {
        self.view = choice;
        // `path_diff.view` is also the view ToggleDiffMode returns to from the duplicates.
        if let Some(view) = choice.pane_view() {
            self.path_diff.view = view;
        }
    }

    fn poll(&mut self, ctx: &egui::Context, state: &mut FolderState, map_claims_back: bool) {
        self.apply_view_choice(self.view);
        state.update_scans(ctx);
        state.tree_compare.set_repaint(ctx);
        state.update_compare(Instant::now());
        state.build_two_folder_diff_rows_if_stale();
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
        self.dispatch(ctx, &state.keymap, map_claims_back);
        self.apply_view_actions();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FolderPanes, FolderState, FolderView, ScanAction,
        SCAN_CANCELLED, discard_stale_rows, finish_side_scan, root_of_side, scan_action,
        scan_progress_text, update_side_scan,
    };
    use crate::app::ZApp;
    use crate::folder::folder_keymap::FolderAction;
    use crate::folder::fs_tree::{DiffState, FileSystemView};
    use crate::folder::panes::ViewChoice;
    use crate::folder::path_diff_pane::{BLACKLIST_PRESETS, FilterField, FilterPresets, PaneView};
    use crate::folder::roots::Side;
    use crate::folder::tree_compare::PairResult;
    use crate::folder::tree_filter::{DiffFilter, DuplicatesFilter, StateFilter};
    use crate::folder::tree_find::FolderFind;
    use crate::folder::tree_likeness::FolderLikeness;
    use crate::folder::tree_sort::{SortKey, TreeSort};
    use crate::folder::tree_tolerance::{SizeDelta, Tolerance};
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
    use zcommon_ui::keymap::{Action, Keymap, Shortcut};
    use zdiff_folder::likeness::{Likeness, Measure};
    use zdiff_folder::{
        comparison::{PairCompare, PairState},
        filter::{PathFilter, PatternList},
        fs::FileSystemModel,
        scan::{ContentHash, FlatEntry, FlatListing, HashKind, scan},
    };

    fn finish_compare(state: &mut FolderState) {
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
        let mut state = FolderState {
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
    fn a_path_filter_toggled_back_after_a_rescan_shows_the_rescanned_states() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            fs::write(root.join("a.txt"), "a").unwrap();
            fs::write(root.join("b.md"), "b").unwrap();
        }
        let mut state = FolderState {
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        finish_compare(&mut state);
        let a_state = |state: &FolderState| {
            let rows = state.two_folder_diff_visible_rows.as_ref().unwrap();
            let row = rows.iter().find(|row| row.rel_path == "a.txt").unwrap();
            row.diff_state.clone()
        };
        assert!(matches!(a_state(&state), DiffState::Same(..)));

        fs::write(right.path().join("a.txt"), "a changed").unwrap();
        state.file_system_model_2_view = load_view(right.path());
        state.path_filter = PathFilter {
            blacklist: PatternList::new("*.md"),
            ..Default::default()
        };
        finish_compare(&mut state);
        state.path_filter = PathFilter::default();
        finish_compare(&mut state);
        assert!(matches!(a_state(&state), DiffState::Different(..)));
    }

    #[test]
    fn a_path_filter_toggled_back_after_a_rescan_shows_the_rescanned_tree() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            fs::write(root.join("a.txt"), "a").unwrap();
            fs::write(root.join("b.md"), "b").unwrap();
        }
        let mut state = FolderState {
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        finish_compare(&mut state);

        fs::remove_file(right.path().join("a.txt")).unwrap();
        state.file_system_model_2_view = load_view(right.path());
        state.path_filter = PathFilter {
            blacklist: PatternList::new("*.md"),
            ..Default::default()
        };
        finish_compare(&mut state);
        state.path_filter = PathFilter::default();
        finish_compare(&mut state);
        let rows = state.two_folder_diff_visible_rows.as_ref().unwrap();
        let a = rows.iter().find(|row| row.rel_path == "a.txt").unwrap();
        assert!(matches!(a.diff_state, DiffState::OnlyInFirst(..)));
    }

    #[test]
    fn rows_built_after_a_cancelled_compare_take_its_resumed_results() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        fs::write(left.path().join("a.txt"), "left").unwrap();
        fs::write(right.path().join("a.txt"), "right").unwrap();
        let mut state = FolderState {
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        state.build_two_folder_diff_rows_if_stale();
        state.tree_compare.cancel();
        state.path_filter = PathFilter {
            blacklist: PatternList::new("*.md"),
            ..Default::default()
        };
        state.build_two_folder_diff_rows_if_stale();
        state.path_filter = PathFilter::default();
        state.build_two_folder_diff_rows_if_stale();

        state.likeness_bar = true;
        finish_compare(&mut state);
        let rows = state.two_folder_diff_visible_rows.as_ref().unwrap();
        let a = rows.iter().find(|row| row.rel_path == "a.txt").unwrap();
        assert!(matches!(a.diff_state, DiffState::Different(..)));
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
        let mut state = FolderState {
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

    fn row_paths(state: &FolderState) -> Vec<&str> {
        let rows = state.two_folder_diff_visible_rows.as_ref().unwrap();
        rows.iter().map(|r| r.rel_path.as_str()).collect()
    }

    #[test]
    fn the_filter_survives_a_restart() {
        let state = FolderState {
            path_filter: blacklist("*.obj, target/"),
            ..Default::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: FolderState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.path_filter, state.path_filter);
        assert!(restored.path_filter.is_blacklisted("a.OBJ", false));
        assert!(restored.path_filter.is_blacklisted("target", true));
    }

    #[test]
    fn a_state_saved_before_the_filter_existed_loads_with_an_empty_filter() {
        let mut json: serde_json::Value = serde_json::to_value(FolderState::default()).unwrap();
        json.as_object_mut().unwrap().remove("path_filter").unwrap();

        let restored: FolderState = serde_json::from_value(json).unwrap();

        assert_eq!(restored.path_filter, PathFilter::default());
    }

    #[test]
    fn the_keymap_survives_a_restart() {
        let mut state = FolderState::default();
        state.keymap.shortcuts_mut(FolderAction::NextDiff).remove(0);
        state
            .keymap
            .shortcuts_mut(FolderAction::OpenKeybindings)
            .clear();

        let json = serde_json::to_string(&state).unwrap();
        let restored: FolderState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.keymap, state.keymap);
        assert_ne!(restored.keymap, Keymap::default());
    }

    #[test]
    fn a_state_saved_before_the_keymap_existed_loads_the_default_keymap() {
        let mut json: serde_json::Value = serde_json::to_value(FolderState::default()).unwrap();
        json.as_object_mut().unwrap().remove("keymap");

        let restored: FolderState = serde_json::from_value(json).unwrap();

        assert_eq!(restored.keymap, Keymap::default());
    }

    #[test]
    fn a_keymap_saved_with_the_old_f5_refresh_default_moves_to_the_new_default() {
        let f5 = Shortcut::Key {
            key: egui::Key::F5,
            modifiers: egui::Modifiers::NONE,
        };
        let custom = Shortcut::Key {
            key: egui::Key::F6,
            modifiers: egui::Modifiers::NONE,
        };
        let restore = |refresh: Vec<Shortcut>| {
            let mut state = FolderState::default();
            *state.keymap.shortcuts_mut(FolderAction::Refresh) = refresh;
            let json = serde_json::to_string(&state).unwrap();
            serde_json::from_str::<FolderState>(&json).unwrap().keymap
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
        let mut state = FolderState {
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };

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
    fn the_no_build_dirs_preset_hides_target_and_keeps_src() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            for dir in ["src", "target"] {
                fs::create_dir(root.join(dir)).unwrap();
                fs::write(root.join(dir).join("a.rs"), "a").unwrap();
            }
        }
        let mut state = FolderState {
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
        let mut state = FolderState::default();
        let (builtins, user) = FilterField::Blacklist.presets(&mut state.filter_presets);
        user.save(builtins, "Mine", "*.tmp").unwrap();
        let (builtins, user) = FilterField::Whitelist.presets(&mut state.filter_presets);
        user.save(builtins, "Shaders", "*.hlsl *.usf").unwrap();

        let json = serde_json::to_string(&state).unwrap();
        let restored: FolderState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.filter_presets, state.filter_presets);
        assert_ne!(restored.filter_presets, FilterPresets::default());
    }

    #[test]
    fn a_state_saved_before_filter_presets_existed_loads_none() {
        let mut json: serde_json::Value = serde_json::to_value(FolderState::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("filter_presets")
            .unwrap();

        let restored: FolderState = serde_json::from_value(json).unwrap();

        assert_eq!(restored.filter_presets, FilterPresets::default());
    }

    #[test]
    fn the_sort_survives_a_restart() {
        let mut state = FolderState {
            tree_sort: TreeSort::default()
                .clicked(SortKey::State)
                .clicked(SortKey::State),
            ..Default::default()
        };
        state.tree_sort.flat = true;

        let json = serde_json::to_string(&state).unwrap();
        let restored: FolderState = serde_json::from_str(&json).unwrap();

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
        let mut json: serde_json::Value = serde_json::to_value(FolderState::default()).unwrap();
        json.as_object_mut().unwrap().remove("tree_sort").unwrap();

        let restored: FolderState = serde_json::from_value(json).unwrap();

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
            let state = FolderState {
                diff_filter: DiffFilter {
                    states,
                    show_parents: false,
                },
                ..Default::default()
            };

            let json = serde_json::to_string(&state).unwrap();
            let restored: FolderState = serde_json::from_str(&json).unwrap();

            assert_eq!(restored.diff_filter, state.diff_filter);
        }
    }

    #[test]
    fn a_state_saved_before_diff_only_existed_shows_every_row_with_parents() {
        let mut json: serde_json::Value = serde_json::to_value(FolderState::default()).unwrap();
        json.as_object_mut().unwrap().remove("diff_filter").unwrap();

        let restored: FolderState = serde_json::from_value(json).unwrap();

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
            let mut json: serde_json::Value = serde_json::to_value(FolderState::default()).unwrap();
            json["diff_filter"] =
                serde_json::json!({ "diff_only": diff_only, "show_parents": false });

            let restored: FolderState = serde_json::from_value(json).unwrap();

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
        let state = FolderState {
            duplicates_filter: DuplicatesFilter {
                duplicates_only: true,
                show_parents: false,
                hash_column: true,
            },
            ..Default::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: FolderState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.duplicates_filter, state.duplicates_filter);
    }

    #[test]
    fn a_state_saved_before_the_duplicates_bar_existed_shows_every_row_without_hashes() {
        let mut json: serde_json::Value = serde_json::to_value(FolderState::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("duplicates_filter")
            .unwrap();

        let restored: FolderState = serde_json::from_value(json).unwrap();

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
        let state = FolderState {
            likeness_bar: true,
            ..Default::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: FolderState = serde_json::from_str(&json).unwrap();

        assert!(restored.likeness_bar);
    }

    #[test]
    fn a_state_saved_before_the_likeness_bar_existed_loads_it_off() {
        let mut json: serde_json::Value = serde_json::to_value(FolderState::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("likeness_bar")
            .unwrap();

        let restored: FolderState = serde_json::from_value(json).unwrap();

        assert!(!restored.likeness_bar);
    }

    #[test]
    fn turning_the_likeness_bar_on_measures_the_differing_pairs_on_the_next_update() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            fs::write(root.join("a.txt"), "a").unwrap();
        }
        fs::write(left.path().join("b.txt"), "left b").unwrap();
        fs::write(right.path().join("b.txt"), "right b").unwrap();
        let mut state = FolderState {
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
        let state = FolderState {
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
        let restored: FolderState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.tolerance, state.tolerance);
    }

    #[test]
    fn a_state_saved_before_tolerance_existed_loads_it_off_with_a_1_percent_size_criterion() {
        let mut json: serde_json::Value = serde_json::to_value(FolderState::default()).unwrap();
        json.as_object_mut().unwrap().remove("tolerance").unwrap();

        let restored: FolderState = serde_json::from_value(json).unwrap();

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
        let mut state = FolderState {
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        finish_compare(&mut state);
        let b_state = |state: &FolderState| {
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
        let mut json: serde_json::Value = serde_json::to_value(FolderState::default()).unwrap();
        let tolerance = json["tolerance"].as_object_mut().unwrap();
        tolerance.remove("likeness").unwrap();
        tolerance.remove("min_likeness").unwrap();

        let restored: FolderState = serde_json::from_value(json).unwrap();

        assert!(!restored.tolerance.likeness);
        assert_eq!(restored.tolerance.min_likeness, 99.0);
    }

    fn b_different_trees() -> (TempDir, TempDir, FolderState) {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            fs::write(root.join("a.txt"), "a").unwrap();
        }
        fs::write(left.path().join("b.txt"), "left b").unwrap();
        fs::write(right.path().join("b.txt"), "right b").unwrap();
        let state = FolderState {
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        (left, right, state)
    }

    fn b_state(state: &FolderState) -> DiffState {
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
        let mut state = FolderState {
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };

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
        let mut state = FolderState {
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };

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

    fn find_state(files: &[&str]) -> (TempDir, TempDir, FolderState) {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            for file in files {
                let path = root.join(file);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, root.to_string_lossy().as_bytes()).unwrap();
            }
        }
        let state = FolderState {
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        (left, right, state)
    }

    fn set_find(state: &mut FolderState, text: &str) {
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
        let restored: FolderState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.folder_find, FolderFind::default());
        assert_eq!(restored.find_applied, FolderFind::default());
        assert_eq!(restored.find_matches, None);
    }

    fn finish_scans(state: &mut FolderState) {
        finish_side_scan(
            Side::Folder1,
            state.file_system_1_root_path.as_deref(),
            &mut state.file_system_model_1_view,
            &mut state.file_system_1_scan,
            &mut state.two_folder_diff_visible_rows,
        );
        finish_side_scan(
            Side::Folder2,
            state.file_system_2_root_path.as_deref(),
            &mut state.file_system_model_2_view,
            &mut state.file_system_2_scan,
            &mut state.two_folder_diff_visible_rows,
        );
    }

    fn shown(view: &Option<FileSystemView>) -> Option<PathBuf> {
        view.as_ref()
            .map(|view| view.file_system.path(view.file_system.get_root_node_id()))
    }

    fn loaded(root_1: &Path, root_2: &Path) -> FolderState {
        let mut state = FolderState::default();
        state.file_system_1_root_path = Some(root_1.into());
        state.file_system_2_root_path = Some(root_2.into());
        finish_scans(&mut state);
        state
    }

    fn update_side_1(state: &mut FolderState) {
        update_side_scan(
            Side::Folder1,
            state.file_system_1_root_path.as_deref(),
            &mut state.file_system_model_1_view,
            &mut state.file_system_1_scan,
            &mut state.two_folder_diff_visible_rows,
            &egui::Context::default(),
        );
    }

    #[test]
    fn a_side_without_a_root_is_cleared() {
        let a = Path::new("a");
        assert_eq!(
            scan_action(None, Some(a), Some(a), None, false),
            ScanAction::Clear
        );
        assert_eq!(scan_action(None, None, None, None, true), ScanAction::Clear);
    }

    #[test]
    fn a_new_root_starts_a_scan() {
        let (a, b) = (Path::new("a"), Path::new("b"));
        assert_eq!(
            scan_action(Some(b), Some(a), None, None, false),
            ScanAction::Start(b.into())
        );
        assert_eq!(
            scan_action(Some(b), None, None, None, false),
            ScanAction::Start(b.into())
        );
    }

    #[test]
    fn a_new_root_replaces_the_scan_of_another_root() {
        let (a, b, c) = (Path::new("a"), Path::new("b"), Path::new("c"));
        assert_eq!(
            scan_action(Some(b), Some(a), Some(c), None, false),
            ScanAction::Start(b.into())
        );
    }

    #[test]
    fn the_scan_of_the_root_keeps_running() {
        let (a, b) = (Path::new("a"), Path::new("b"));
        assert_eq!(
            scan_action(Some(b), Some(a), Some(b), None, false),
            ScanAction::Keep
        );
        assert_eq!(scan_action(Some(b), None, Some(b), None, false), ScanAction::Keep);
        assert_eq!(
            scan_action(Some(a), Some(a), Some(a), None, false),
            ScanAction::Keep
        );
    }

    #[test]
    fn the_shown_root_needs_no_scan() {
        let (a, b) = (Path::new("a"), Path::new("b"));
        assert_eq!(scan_action(Some(a), Some(a), None, None, false), ScanAction::Stop);
        assert_eq!(
            scan_action(Some(a), Some(a), Some(b), None, false),
            ScanAction::Stop
        );
    }

    #[test]
    fn a_rescan_scans_the_shown_root_again() {
        let a = Path::new("a");
        assert_eq!(
            scan_action(Some(a), Some(a), None, None, true),
            ScanAction::Start(a.into())
        );
    }

    #[test]
    fn roots_given_by_the_shell_scan_both_sides_in_the_background() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        fs::write(left.path().join("a.txt"), "a").unwrap();
        fs::write(right.path().join("b.txt"), "b").unwrap();
        let mut state = FolderState::default();
        state.file_system_1_root_path = Some(left.path().into());
        state.file_system_2_root_path = Some(right.path().into());

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

        state.file_system_2_root_path = Some(other.path().into());
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
    fn a_failed_scan_is_not_retried_while_the_root_stays() {
        let (a, b) = (Path::new("a"), Path::new("b"));
        assert_eq!(
            scan_action(Some(a), None, None, Some(a), false),
            ScanAction::Stop
        );
        assert_eq!(
            scan_action(Some(a), None, None, Some(a), true),
            ScanAction::Start(a.into())
        );
        assert_eq!(
            scan_action(Some(b), None, None, Some(a), false),
            ScanAction::Start(b.into())
        );
    }

    #[test]
    fn a_failed_scan_shows_its_error_and_is_not_retried_until_refresh_or_a_new_root() {
        let (left, right, gone) = (tempdir().unwrap(), tempdir().unwrap(), tempdir().unwrap());
        let mut state = loaded(left.path(), right.path());
        state.build_two_folder_diff_rows_if_stale();
        let gone: PathBuf = gone.path().into();
        fs::remove_dir(&gone).unwrap();
        state.file_system_1_root_path = Some(gone.clone());

        finish_scans(&mut state);
        assert_eq!(state.file_system_1_root_path.as_deref(), Some(&*gone));
        assert!(state.file_system_model_1_view.is_none());
        assert!(state.two_folder_diff_visible_rows.is_none());
        assert!(state.file_system_1_scan.error_for(Some(&gone)).is_some());
        assert_eq!(
            shown(&state.file_system_model_2_view),
            Some(right.path().into())
        );

        update_side_1(&mut state);
        assert!(!state.file_system_1_scan.is_scanning());

        state.refresh();
        update_side_1(&mut state);
        assert!(state.file_system_1_scan.is_scanning());
        assert_eq!(state.file_system_1_scan.error_for(Some(&gone)), None);
        finish_scans(&mut state);
        assert!(state.file_system_1_scan.error_for(Some(&gone)).is_some());

        state.file_system_1_root_path = Some(left.path().into());
        finish_scans(&mut state);
        assert_eq!(
            shown(&state.file_system_model_1_view),
            Some(left.path().into())
        );
        assert_eq!(state.file_system_1_scan.error_for(Some(left.path())), None);
    }

    #[test]
    fn a_cancelled_scan_leaves_the_side_with_an_error() {
        let (left, right, other) = (tempdir().unwrap(), tempdir().unwrap(), tempdir().unwrap());
        let mut state = loaded(left.path(), right.path());
        state.build_two_folder_diff_rows_if_stale();
        state.file_system_1_root_path = Some(other.path().into());
        update_side_1(&mut state);
        assert!(state.file_system_1_scan.is_scanning());

        state.file_system_1_scan.request_cancel();
        update_side_1(&mut state);

        assert_eq!(state.file_system_1_root_path.as_deref(), Some(other.path()));
        assert!(!state.file_system_1_scan.is_scanning());
        assert!(state.file_system_model_1_view.is_none());
        assert!(state.two_folder_diff_visible_rows.is_none());
        assert_eq!(
            state.file_system_1_scan.error_for(Some(other.path())),
            Some(SCAN_CANCELLED)
        );
        update_side_1(&mut state);
        assert!(!state.file_system_1_scan.is_scanning());
    }

    #[test]
    fn a_revision_picked_for_a_folder_side_gives_the_root_text_the_scan_dispatches_on() {
        use zcommon::universal_path::{DepotRev, GitRev, UniversalPath};

        let folder = PathBuf::from(r"D:\repo\src\gui");
        let git = |rev| UniversalPath::Git(folder.clone(), rev);
        let depot = |rev| UniversalPath::Depot("//depot/main/src".to_owned(), rev);
        for (picked, root) in [
            (git(GitRev::Rev("main".to_owned())), r"D:\repo\src\gui@git:main"),
            (
                git(GitRev::Rev("origin/dev".to_owned())),
                r"D:\repo\src\gui@git:origin/dev",
            ),
            (git(GitRev::Index), r"D:\repo\src\gui@git:index"),
            (UniversalPath::Local(folder.clone()), r"D:\repo\src\gui"),
            (depot(None), "//depot/main/src"),
            (depot(Some(DepotRev::Change(1234))), "//depot/main/src@1234"),
            (depot(Some(DepotRev::Shelf(77))), "//depot/main/src@=77"),
        ] {
            let side_root = root_of_side(&picked).expect("a side with a path has a root");
            assert_eq!(side_root, Path::new(root));
            assert_eq!(UniversalPath::from(&*side_root), picked);
        }
        assert_eq!(root_of_side(&UniversalPath::default()), None);
    }

    #[test]
    fn clearing_a_root_drops_its_view_and_the_rows_once() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        let mut state = loaded(left.path(), right.path());
        state.build_two_folder_diff_rows_if_stale();

        state.file_system_2_root_path = None;
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
            "Listing Left from p4: //depot/main@12"
        );
        assert_eq!(
            scan_progress_text(Side::Folder2, git, 0),
            r"Listing Right from git: D:\repo\src@git:main"
        );
        assert_eq!(
            scan_progress_text(Side::Folder1, depot, 7),
            "Scanning Left: 7 entries in //depot/main@12"
        );
        assert_eq!(
            scan_progress_text(Side::Folder1, local, 0),
            r"Scanning Left: 0 entries in D:\repo\src"
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
        let mut state = FolderState {
            file_system_1_root_path: Some(left.path().to_path_buf()),
            file_system_2_root_path: Some(right.path().to_path_buf()),
            file_system_model_1_view: Some(in_memory_view(left.path(), files_50k(1))),
            file_system_model_2_view: Some(in_memory_view(right.path(), files_50k(2))),
            ..Default::default()
        };
        finish_compare(&mut state);
        let mut app = idle_app(state);
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
        let mut state = FolderState {
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
        let mut app = idle_app(state);
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

    struct PaintedFrame {
        texts: Vec<(String, egui::Pos2)>,
        passes: usize,
    }

    impl PaintedFrame {
        fn steady_texts(&self) -> Vec<&(String, egui::Pos2)> {
            self.texts
                .iter()
                .filter(|(text, _)| !text.is_empty() && !text.contains(" fps") && text != "idle")
                .collect()
        }
    }

    fn painted_frame(
        ctx: &egui::Context,
        app: &mut ZApp,
        screen: egui::Vec2,
        events: Vec<egui::Event>,
    ) -> PaintedFrame {
        fn collect(shape: &egui::Shape, texts: &mut Vec<(String, egui::Pos2)>) {
            match shape {
                egui::Shape::Text(text) => texts.push((
                    text.galley.text().to_owned(),
                    text.visual_bounding_rect().center(),
                )),
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
        PaintedFrame {
            texts,
            passes: output.platform_output.num_completed_passes,
        }
    }

    fn frame_texts(
        ctx: &egui::Context,
        app: &mut ZApp,
        screen: egui::Vec2,
        events: Vec<egui::Event>,
    ) -> Vec<String> {
        painted_frame(ctx, app, screen, events)
            .texts
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    }

    #[test]
    fn pressing_the_folder_tree_scroll_bar_scrolls_at_a_fractional_zoom() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        let mut state = FolderState {
            file_system_1_root_path: Some(left.path().to_path_buf()),
            file_system_2_root_path: Some(right.path().to_path_buf()),
            file_system_model_1_view: Some(in_memory_view(left.path(), files_50k(1))),
            file_system_model_2_view: Some(in_memory_view(right.path(), files_50k(2))),
            ..Default::default()
        };
        finish_compare(&mut state);
        let mut app = idle_app(state);
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
        let mut state = FolderState {
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
        let mut app = idle_app(state);
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
        let comparing = |app: &ZApp| folder_state(app).tree_compare.is_comparing();
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

    const SCREEN: egui::Vec2 = egui::vec2(1600.0, 1000.0);

    fn nested_files(side: usize) -> Vec<FlatEntry> {
        pending_files()
            .into_iter()
            .enumerate()
            .map(|(i, entry)| FlatEntry {
                hash: Some(ContentHash {
                    kind: HashKind::GitBlob,
                    value: if i % 7 == 0 {
                        format!("{i} on side {side}")
                    } else {
                        i.to_string()
                    },
                }),
                ..entry
            })
            .collect()
    }

    fn nested_folder_diff(tree_sort: TreeSort) -> (TempDir, TempDir, FolderState) {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        let mut state = FolderState {
            file_system_1_root_path: Some(left.path().to_path_buf()),
            file_system_2_root_path: Some(right.path().to_path_buf()),
            file_system_model_1_view: Some(in_memory_view(left.path(), nested_files(1))),
            file_system_model_2_view: Some(in_memory_view(right.path(), nested_files(2))),
            tree_sort,
            ..Default::default()
        };
        finish_compare(&mut state);
        (left, right, state)
    }

    fn idle_app(state: FolderState) -> ZApp {
        ZApp::idle_showing_folder(FolderView {
            state,
            panes: FolderPanes::default(),
        })
    }

    fn folder_state(app: &ZApp) -> &FolderState {
        &app.folder.state
    }

    fn settled_frame(ctx: &egui::Context, app: &mut ZApp) -> PaintedFrame {
        for _ in 0..3 {
            painted_frame(ctx, app, SCREEN, Vec::new());
        }
        painted_frame(ctx, app, SCREEN, Vec::new())
    }

    fn text_pos(frame: &PaintedFrame, text: &str) -> egui::Pos2 {
        frame
            .texts
            .iter()
            .find(|(painted, _)| painted == text)
            .unwrap_or_else(|| panic!("{text:?} is not painted"))
            .1
    }

    fn click_frame(ctx: &egui::Context, app: &mut ZApp, pos: egui::Pos2) -> PaintedFrame {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        painted_frame(ctx, app, SCREEN, vec![egui::Event::PointerMoved(pos)]);
        painted_frame(ctx, app, SCREEN, vec![button(true)]);
        painted_frame(ctx, app, SCREEN, vec![button(false)])
    }

    fn click_frame_and_next(app: &mut ZApp, label: &str) -> [PaintedFrame; 3] {
        let ctx = egui::Context::default();
        let before = settled_frame(&ctx, app);
        let click = click_frame(&ctx, app, text_pos(&before, label));
        let next = painted_frame(&ctx, app, SCREEN, Vec::new());
        [before, click, next]
    }

    #[test]
    fn stale_rows_never_discard_two_frames_in_a_row() {
        let mut last = None;
        let discarded: Vec<bool> = (10..16)
            .map(|frame| discard_stale_rows(true, &mut last, frame))
            .collect();
        assert_eq!(discarded, [true, false, true, false, true, false]);
        assert!(!discard_stale_rows(false, &mut last, 20));
        assert!(discard_stale_rows(true, &mut last, 21));
    }

    #[test]
    fn toggling_flat_paints_the_flat_list_in_the_click_frame() {
        let (_left, _right, state) = nested_folder_diff(TreeSort::default());
        let mut app = idle_app(state);

        let [before, click, next] = click_frame_and_next(&mut app, "F");

        assert!(folder_state(&app).tree_sort.flat);
        assert_ne!(before.steady_texts(), next.steady_texts());
        assert_eq!(click.steady_texts(), next.steady_texts());
        assert_eq!(next.passes, 1);
    }

    #[test]
    fn toggling_flat_off_paints_the_folded_tree_in_the_click_frame() {
        let flat = TreeSort {
            flat: true,
            ..TreeSort::default()
        };
        let (_left, _right, mut state) = nested_folder_diff(flat);
        let rows = state.two_folder_diff_visible_rows.clone().unwrap();
        let views = [
            state.file_system_model_1_view.as_mut().unwrap(),
            state.file_system_model_2_view.as_mut().unwrap(),
        ];
        for row in rows
            .iter()
            .filter(|row| row.is_dir && !["", "dir_0"].contains(&row.rel_path.as_str()))
        {
            views[0]
                .collapsed
                .insert(row.diff_state.first().unwrap(), true);
            views[1]
                .collapsed
                .insert(row.diff_state.second().unwrap(), true);
        }
        let mut app = idle_app(state);

        let [before, click, next] = click_frame_and_next(&mut app, "F");

        assert!(!folder_state(&app).tree_sort.flat);
        assert_ne!(before.steady_texts(), next.steady_texts());
        assert_eq!(click.steady_texts(), next.steady_texts());
        assert_eq!(next.passes, 1);
    }

    #[test]
    fn the_diff_filter_paints_the_filtered_rows_in_the_click_frame() {
        let (_left, _right, state) = nested_folder_diff(TreeSort::default());
        let mut app = idle_app(state);

        let [before, click, next] = click_frame_and_next(&mut app, "D");

        assert_eq!(folder_state(&app).diff_filter.states, StateFilter::Diff);
        assert_ne!(before.steady_texts(), next.steady_texts());
        assert_eq!(click.steady_texts(), next.steady_texts());
        assert_eq!(next.passes, 1);
    }

    #[test]
    fn typing_a_path_filter_paints_the_filtered_rows_in_the_typing_frame() {
        let (_left, _right, state) = nested_folder_diff(TreeSort::default());
        let mut app = idle_app(state);
        let ctx = egui::Context::default();
        let field = text_pos(&settled_frame(&ctx, &mut app), r"*.obj target/ My\ Folder/");
        let before = click_frame(&ctx, &mut app, field);

        let typed = painted_frame(
            &ctx,
            &mut app,
            SCREEN,
            vec![egui::Event::Text("dir_0/".to_owned())],
        );
        let next = painted_frame(&ctx, &mut app, SCREEN, Vec::new());

        assert_eq!(folder_state(&app).path_filter, blacklist("dir_0/"));
        assert_ne!(before.steady_texts(), next.steady_texts());
        assert_eq!(typed.steady_texts(), next.steady_texts());
        assert_eq!(next.passes, 1);
    }

    #[test]
    fn toggling_flat_in_the_duplicates_tab_paints_the_flat_list_in_the_click_frame() {
        let (_left, _right, mut state) = nested_folder_diff(TreeSort::default());
        state.hash_service = HashService::new(0);
        let mut app = idle_app(state);
        app.folder.panes.view = ViewChoice::Duplicates;

        let [before, click, next] = click_frame_and_next(&mut app, "F");

        assert!(folder_state(&app).tree_sort.flat);
        assert_ne!(before.steady_texts(), next.steady_texts());
        assert_eq!(click.steady_texts(), next.steady_texts());
        assert_eq!(next.passes, 1);
    }

    #[test]
    fn the_rows_go_stale_with_each_of_their_inputs_until_rebuilt() {
        let (_left, _right, mut state) = nested_folder_diff(TreeSort::default());
        let changes: [fn(&mut FolderState); 6] = [
            |state| state.tree_sort.flat = true,
            |state| state.diff_filter.states = StateFilter::Diff,
            |state| state.diff_filter.show_parents = false,
            |state| state.tolerance.enabled = !state.tolerance.enabled,
            |state| state.path_filter = blacklist("dir_1/"),
            |state| {
                state.folder_find.query.text = "f_2".to_owned();
                state.apply_find();
            },
        ];

        assert!(!state.rows_are_stale());
        for change in changes {
            change(&mut state);
            assert!(state.rows_are_stale());
            state.build_two_folder_diff_rows_if_stale();
            assert!(!state.rows_are_stale());
        }
    }

    #[test]
    fn a_changed_sort_without_a_shown_folder_paints_in_one_pass_and_idles() {
        let mut state = FolderState::default();
        state.tree_sort.flat = true;
        state.diff_filter.states = StateFilter::Diff;
        let mut view = FolderView {
            state,
            panes: FolderPanes::default(),
        };
        let ctx = egui::Context::default();
        let mut frame = || {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
                ..Default::default()
            };
            ctx.run(input, |ctx| {
                view.poll(ctx, false);
                egui::CentralPanel::default().show(ctx, |ui| view.ui(ui, false, true));
            })
        };
        for _ in 0..5 {
            frame();
        }
        let settled = frame();

        assert_eq!(settled.platform_output.num_completed_passes, 1);
        assert_eq!(
            settled.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
            Duration::MAX
        );
        assert!(!view.state.rows_are_stale());
    }

    #[test]
    fn a_root_spelled_differently_from_the_side_is_not_rescanned() {
        let dir = tempdir().unwrap();
        let spelled = dir.path().join("missing").join("..");
        let state = FolderState {
            file_system_1_root_path: Some(spelled.clone()),
            file_system_model_1_view: Some(in_memory_view(&spelled, nested_files(1))),
            ..Default::default()
        };
        let model = |app: &ZApp| {
            let view = folder_state(app).file_system_model_1_view.as_ref();
            view.expect("the side is shown").file_system.clone()
        };
        let mut app = idle_app(state);
        let shown = model(&app);

        settled_frame(&egui::Context::default(), &mut app);

        assert!(Arc::ptr_eq(&model(&app), &shown));
        assert!(!folder_state(&app).file_system_1_scan.is_scanning());
        assert_eq!(app.folder.roots(), [Some(spelled), None]);
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
        let mut state = FolderState {
            hash_service,
            file_system_1_root_path: Some(left.path().to_path_buf()),
            file_system_2_root_path: Some(right.path().to_path_buf()),
            file_system_model_1_view: load_view(left.path()),
            file_system_model_2_view: load_view(right.path()),
            ..Default::default()
        };
        finish_compare(&mut state);
        let app = idle_app(state);
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
        app.folder.panes.view = ViewChoice::Duplicates;

        assert!(settled_repaint_delay(&mut app) <= Duration::from_millis(100));
    }

    #[test]
    fn choosing_a_view_sets_the_view_and_the_folder_pane_view() {
        let (_left, _right, mut app) = shown_app(HashService::new(0));
        let ctx = egui::Context::default();
        let choose = |app: &mut ZApp, choice: ViewChoice| {
            app.folder.panes.apply_view_choice(choice);
            repaint_delay(&ctx, app);
            (app.folder.panes.view, app.folder.panes.path_diff.view)
        };

        assert_eq!(
            choose(&mut app, ViewChoice::Map),
            (ViewChoice::Map, PaneView::Map)
        );
        assert_eq!(
            choose(&mut app, ViewChoice::Duplicates),
            (ViewChoice::Duplicates, PaneView::Map)
        );
        assert_eq!(
            choose(&mut app, ViewChoice::Tree),
            (ViewChoice::Tree, PaneView::Tree)
        );
        assert_eq!(
            choose(&mut app, ViewChoice::Duplicates),
            (ViewChoice::Duplicates, PaneView::Tree)
        );
        assert_eq!(
            choose(&mut app, ViewChoice::Map),
            (ViewChoice::Map, PaneView::Map)
        );
    }

    #[test]
    fn the_diff_mode_toggle_returns_from_the_duplicates_to_the_last_tree_or_map() {
        let mut panes = FolderPanes::default();
        let toggle = |panes: &mut FolderPanes, action| {
            panes.polled.actions = vec![action];
            panes.apply_view_actions();
            panes.view
        };

        assert_eq!(toggle(&mut panes, FolderAction::ToggleMap), ViewChoice::Map);
        assert_eq!(
            toggle(&mut panes, FolderAction::ToggleDiffMode),
            ViewChoice::Duplicates
        );
        assert_eq!(
            toggle(&mut panes, FolderAction::ToggleMap),
            ViewChoice::Duplicates
        );
        assert_eq!(
            toggle(&mut panes, FolderAction::ToggleDiffMode),
            ViewChoice::Map
        );
    }

    #[test]
    fn an_idle_duplicate_tab_stops_repainting() {
        let (_left, _right, mut app) = shown_app(HashService::new(1));
        app.folder.panes.view = ViewChoice::Duplicates;
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
