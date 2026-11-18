use std::{
    collections::HashMap,
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use memmap2::Mmap;
use zcommon::{change_map::ChangeMap, job::Job, kinds::KindTable, lru::LruMap};
use zdiff_folder::{
    comparison::{FileMeta, PairCompare, PairCompareCache, PairSide, PairState, compare_pair_with},
    fs::{FileSystemModel, FsNode},
    likeness::{Likeness, change_map, likeness},
    scan::ContentHash,
    vcs_source::ContentSource,
};

use crate::folder::fs_tree::{DiffState, FileSystemView, VisibleRowTwoFolderDiff};

// Disk bound: more readers than this mostly make the reads seek against each other.
const MAX_COMPARE_WORKERS: usize = 4;
const ROWS_PATCH_INTERVAL: Duration = Duration::from_millis(250);
// The gap after a patch is at least this many times its cost, so big trees don't patch every frame.
const PATCH_COST_FACTOR: u32 = 4;
// The image viewer's default.
const PIXEL_TOLERANCE: u8 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
struct LikenessSettings {
    kinds: KindTable,
    pixel_tolerance: u8,
}

impl Default for LikenessSettings {
    fn default() -> Self {
        Self {
            kinds: KindTable::default(),
            pixel_tolerance: PIXEL_TOLERANCE,
        }
    }
}

#[derive(Debug)]
struct CachedLikeness {
    metas: [FileMeta; 2],
    settings: Arc<LikenessSettings>,
    likeness: PairLikeness,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PairLikeness {
    Measured(Likeness),
    Failed(String),
}

impl PairLikeness {
    pub fn ratio(&self) -> Option<f64> {
        match self {
            PairLikeness::Measured(likeness) => Some(likeness.ratio),
            PairLikeness::Failed(_) => None,
        }
    }
}

fn map_pair(paths: &[PathBuf; 2], rel_path: &str) -> io::Result<([Mmap; 2], String)> {
    // SAFETY: a file changed while mapped only skews this one measure.
    let map = |path: &Path| unsafe { Mmap::map(&File::open(path)?) };
    let extension = Path::new(rel_path)
        .extension()
        .map_or(String::new(), |e| e.to_string_lossy().into_owned());
    Ok(([map(&paths[0])?, map(&paths[1])?], extension))
}

pub fn pair_change_map(
    paths: &[PathBuf; 2],
    rel_path: &str,
    cancel: &Arc<AtomicBool>,
) -> Option<io::Result<ChangeMap>> {
    let ([left, right], extension) = match map_pair(paths, rel_path) {
        Ok(mapped) => mapped,
        Err(e) => return Some(Err(e)),
    };
    change_map(&KindTable::default(), &extension, &left, &right, cancel).map(Ok)
}

#[derive(Debug, Clone)]
pub struct PendingPair {
    pub rel_path: String,
    pub paths: [PathBuf; 2],
    sizes: [u64; 2],
    modified: [Option<SystemTime>; 2],
    hashes: [Option<ContentHash>; 2],
    sources: Arc<[ContentSource; 2]>,
}

impl PendingPair {
    fn new(
        rel_path: &str,
        nodes: [&FsNode; 2],
        paths: [PathBuf; 2],
        sources: Arc<[ContentSource; 2]>,
    ) -> Self {
        Self {
            rel_path: rel_path.to_owned(),
            paths,
            sizes: nodes.map(|node| node.size),
            modified: nodes.map(|node| node.modified),
            hashes: nodes.map(|node| node.hash.clone()),
            sources,
        }
    }

    pub fn compare(&self) -> PairCompare {
        // compare_pair decides on the hashes, sizes and content; the time is only a cache key.
        let side = |i: usize| PairSide {
            rel_path: &self.rel_path,
            path: &self.paths[i],
            meta: FileMeta {
                size: self.sizes[i],
                modified: self.modified[i].unwrap_or(SystemTime::UNIX_EPOCH),
            },
            hash: self.hashes[i].as_ref(),
        };
        compare_pair_with(&self.sources[0], &self.sources[1], &side(0), &side(1))
    }

    /// `None` when cancelled.
    fn likeness(
        &self,
        settings: &LikenessSettings,
        cancel: &Arc<AtomicBool>,
    ) -> Option<io::Result<Likeness>> {
        let ([left, right], extension) = match map_pair(&self.paths, &self.rel_path) {
            Ok(mapped) => mapped,
            Err(e) => return Some(Err(e)),
        };
        let (kinds, tolerance) = (&settings.kinds, settings.pixel_tolerance);
        likeness(kinds, &extension, &left, &right, tolerance, cancel).map(Ok)
    }

    fn cache_metas(&self) -> Option<[FileMeta; 2]> {
        Some([
            file_meta(self.sizes[0], self.modified[0])?,
            file_meta(self.sizes[1], self.modified[1])?,
        ])
    }

    fn cache_sides(&self) -> Option<[PairSide<'_>; 2]> {
        Some([
            cache_side(
                &self.rel_path,
                &self.paths[0],
                self.sizes[0],
                self.modified[0],
                self.hashes[0].as_ref(),
            )?,
            cache_side(
                &self.rel_path,
                &self.paths[1],
                self.sizes[1],
                self.modified[1],
                self.hashes[1].as_ref(),
            )?,
        ])
    }
}

/// None without a modification time or hash: the cache could not tell that the file changed.
fn cache_side<'a>(
    rel_path: &'a str,
    path: &'a Path,
    size: u64,
    modified: Option<SystemTime>,
    hash: Option<&'a ContentHash>,
) -> Option<PairSide<'a>> {
    if modified.is_none() && hash.is_none() {
        return None;
    }
    Some(PairSide {
        rel_path,
        path,
        meta: FileMeta {
            size,
            modified: modified.unwrap_or(SystemTime::UNIX_EPOCH),
        },
        hash,
    })
}

fn node_cache_side<'a>(rel_path: &'a str, node: &'a FsNode) -> Option<PairSide<'a>> {
    let hash = node.hash.as_ref();
    cache_side(rel_path, Path::new(""), node.size, node.modified, hash)
}

fn file_meta(size: u64, modified: Option<SystemTime>) -> Option<FileMeta> {
    Some(FileMeta {
        size,
        modified: modified?,
    })
}

/// The file pairs of `rows` still Pending, in row order.
#[cfg(test)]
pub fn pending_pairs(
    rows: &[VisibleRowTwoFolderDiff],
    views: [Option<&FileSystemView>; 2],
) -> Vec<PendingPair> {
    let node = |side: usize, id| views[side]?.file_system.get_node(id);
    rows.iter()
        .filter(|row| !row.is_dir)
        .filter_map(|row| match row.diff_state {
            DiffState::Pending(l_id, r_id) => Some(PendingPair::new(
                &row.rel_path,
                [node(0, l_id)?, node(1, r_id)?],
                [
                    views[0]?.file_system.path(l_id),
                    views[1]?.file_system.path(r_id),
                ],
                Arc::new([
                    views[0]?.file_system.content().clone(),
                    views[1]?.file_system.content().clone(),
                ]),
            )),
            _ => None,
        })
        .collect()
}

/// Copies the states of `fresh` onto the rows with the same path, keeping the rows and their
/// order.
pub fn patch_row_states(rows: &mut [VisibleRowTwoFolderDiff], fresh: Vec<VisibleRowTwoFolderDiff>) {
    let mut fresh: HashMap<String, VisibleRowTwoFolderDiff> = fresh
        .into_iter()
        .map(|row| (row.rel_path.clone(), row))
        .collect();
    for row in rows {
        if let Some(fresh) = fresh.remove(&row.rel_path) {
            row.diff_state = fresh.diff_state;
            row.folder_likeness = fresh.folder_likeness;
        }
    }
}

#[derive(Debug, Clone)]
struct PairTask {
    pair: PendingPair,
    compare: bool,
    likeness: bool,
}

#[derive(Debug)]
pub struct PairResult {
    pub compare: Option<PairCompare>,
    pub likeness: Option<io::Result<Likeness>>,
}

/// One background compare of a list of pairs. Dropping it cancels the compare.
#[derive(Debug)]
struct CompareJob {
    tasks: Arc<[PairTask]>,
    job: Job<(usize, PairResult)>,
    likeness: Arc<AtomicBool>,
    settings: Arc<LikenessSettings>,
    received: usize,
    started: Instant,
    patched: Instant,
    patch_interval: Duration,
    unpatched: bool,
    #[cfg(test)]
    inbox: Option<std::sync::mpsc::Receiver<(usize, PairResult)>>,
}

impl CompareJob {
    /// The results so far, and whether no more will come.
    fn drain(&mut self) -> (Vec<(usize, PairResult)>, bool) {
        #[cfg(test)]
        if let Some(inbox) = &self.inbox {
            let mut results = Vec::new();
            loop {
                match inbox.try_recv() {
                    Ok(result) => results.push(result),
                    Err(std::sync::mpsc::TryRecvError::Empty) => return (results, false),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return (results, true),
                }
            }
        }
        // Read before polling, so everything sent before the worker finished is in the poll.
        let finished = !self.job.is_running();
        (self.job.poll(), finished)
    }

    fn new(
        tasks: Vec<PairTask>,
        likeness: bool,
        settings: Arc<LikenessSettings>,
        now: Instant,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            tasks: tasks.into(),
            job: Job::new(wake),
            likeness: Arc::new(AtomicBool::new(likeness)),
            settings,
            received: 0,
            started: now,
            patched: now,
            patch_interval: ROWS_PATCH_INTERVAL,
            unpatched: false,
            #[cfg(test)]
            inbox: None,
        }
    }

    fn start(
        tasks: Vec<PairTask>,
        likeness: bool,
        settings: Arc<LikenessSettings>,
        now: Instant,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let mut compare = Self::new(tasks, likeness, settings, now, wake);
        let (tasks, likeness) = (compare.tasks.clone(), compare.likeness.clone());
        let settings = compare.settings.clone();
        let workers = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(MAX_COMPARE_WORKERS)
            .min(tasks.len());
        compare.job.start(move |ctx| {
            let next = AtomicUsize::new(0);
            std::thread::scope(|scope| {
                for _ in 0..workers {
                    std::thread::Builder::new()
                        .name("folder compare".into())
                        .spawn_scoped(scope, || {
                            while !ctx.is_cancelled() {
                                let i = next.fetch_add(1, Ordering::Relaxed);
                                let Some(task) = tasks.get(i) else {
                                    break;
                                };
                                let compare = task.compare.then(|| task.pair.compare());
                                let measure = task.likeness
                                    && likeness.load(Ordering::Relaxed)
                                    && compare.as_ref().is_none_or(|c| {
                                        c.state == PairState::Different && c.error.is_none()
                                    });
                                let likeness = if measure {
                                    let Some(result) =
                                        task.pair.likeness(&settings, ctx.cancel_flag())
                                    else {
                                        break;
                                    };
                                    Some(result)
                                } else {
                                    None
                                };
                                if !ctx.send((i, PairResult { compare, likeness })) {
                                    break;
                                }
                            }
                        })
                        .expect("failed to spawn a folder compare thread");
                }
            });
        });
        compare
    }
}

#[derive(Debug, PartialEq)]
pub enum CompareUpdate {
    Idle,
    /// New results since the rows were last patched.
    Patch,
    /// Done or cancelled: the rows can be rebuilt and sorted.
    Finished,
}

/// The file pair states of the two shown trees, filled in by a background compare. Survives
/// rescans through the metadata cache.
#[derive(Debug, Default)]
pub struct TreeCompare {
    // Weak, so a replaced model is freed but its address can't be reused by a new one while
    // it is held here.
    trees: [Weak<FileSystemModel>; 2],
    states: HashMap<String, PairState>,
    cache: PairCompareCache,
    measure_likeness: bool,
    likeness_settings: Arc<LikenessSettings>,
    likeness: HashMap<String, PairLikeness>,
    likeness_cache: LruMap<String, CachedLikeness>,
    job: Option<CompareJob>,
    cancelled: bool,
    compare_time: Option<Duration>,
    repaint: Option<eframe::egui::Context>,
}

impl TreeCompare {
    pub fn set_repaint(&mut self, ctx: &eframe::egui::Context) {
        if self.repaint.is_none() {
            self.repaint = Some(ctx.clone());
        }
    }

    fn wake(&self) -> impl Fn() + Send + Sync + 'static + use<> {
        let repaint = self.repaint.clone();
        move || {
            if let Some(ctx) = &repaint {
                ctx.request_repaint();
            }
        }
    }

    /// States, the job and a cancel belong to the models they were made for.
    pub fn retain_for(&mut self, views: [Option<&FileSystemView>; 2]) {
        let trees =
            views.map(|view| view.map_or_else(Weak::new, |v| Arc::downgrade(&v.file_system)));
        if !Weak::ptr_eq(&trees[0], &self.trees[0]) || !Weak::ptr_eq(&trees[1], &self.trees[1]) {
            self.trees = trees;
            self.states.clear();
            self.likeness.clear();
            self.job = None;
            self.cancelled = false;
            self.compare_time = None;
        }
    }

    pub fn state_of(&self, rel_path: &str, nodes: [&FsNode; 2]) -> Option<PairState> {
        if let Some(state) = self.states.get(rel_path) {
            return Some(*state);
        }
        // The cache keys on rel_path, meta and hash only.
        let side = |node| node_cache_side(rel_path, node);
        self.cache.get(&side(nodes[0])?, &side(nodes[1])?)
    }

    pub fn likeness_of(&self, rel_path: &str, nodes: [&FsNode; 2]) -> Option<&PairLikeness> {
        if let Some(likeness) = self.likeness.get(rel_path) {
            return Some(likeness);
        }
        let metas = [
            file_meta(nodes[0].size, nodes[0].modified)?,
            file_meta(nodes[1].size, nodes[1].modified)?,
        ];
        let cached = self.likeness_cache.get(rel_path)?;
        (cached.metas == metas && cached.settings == self.likeness_settings)
            .then_some(&cached.likeness)
    }

    /// Returns whether it changed. On measures again after a cancel; off skips the likeness
    /// left in the running compare.
    pub fn set_likeness(&mut self, on: bool) -> bool {
        if self.measure_likeness == on {
            return false;
        }
        self.measure_likeness = on;
        self.cancelled &= !on;
        if let Some(job) = &self.job {
            job.likeness.store(on, Ordering::Relaxed);
        }
        true
    }

    /// Compares the pending pairs of `rows`, and measures the differing ones while likeness is
    /// on, replacing any running compare. Nothing starts after a cancel until the models
    /// change.
    pub fn start(
        &mut self,
        rows: &[VisibleRowTwoFolderDiff],
        views: [Option<&FileSystemView>; 2],
        now: Instant,
    ) {
        let _scope = zcommon_ui::frame_stats::scope("Start compare");
        let tasks = self.tasks(rows, views);
        let started = self.job.as_ref().map_or(now, |job| job.started);
        self.job = (!self.cancelled && !tasks.is_empty()).then(|| {
            let mut job = CompareJob::start(
                tasks,
                self.measure_likeness,
                self.likeness_settings.clone(),
                now,
                self.wake(),
            );
            job.started = started;
            job
        });
    }

    fn tasks(
        &self,
        rows: &[VisibleRowTwoFolderDiff],
        views: [Option<&FileSystemView>; 2],
    ) -> Vec<PairTask> {
        let [Some(left), Some(right)] = views else {
            return Vec::new();
        };
        let models = [&left.file_system, &right.file_system];
        let sources = Arc::new(models.map(|model| model.content().clone()));
        rows.iter()
            .filter(|row| !row.is_dir)
            .filter_map(|row| {
                let (DiffState::Pending(l_id, r_id)
                | DiffState::Different(l_id, r_id)
                | DiffState::Similar(l_id, r_id)) = row.diff_state
                else {
                    return None;
                };
                let nodes = [models[0].get_node(l_id)?, models[1].get_node(r_id)?];
                // Rows patched less often than results arrive can still be Pending.
                let state = match row.diff_state {
                    DiffState::Pending(..) => self.state_of(&row.rel_path, nodes),
                    // A pair Similar before its compare is never read.
                    DiffState::Similar(..) => Some(self.state_of(&row.rel_path, nodes)?),
                    _ => Some(PairState::Different),
                };
                let compare = state.is_none();
                // A file facing a folder is Different without a compare.
                let likeness = self.measure_likeness
                    && state != Some(PairState::Same)
                    && nodes.iter().all(|node| node.is_file())
                    // Likeness maps the files from disk.
                    && sources.iter().all(|source| *source == ContentSource::Disk)
                    && self.likeness_of(&row.rel_path, nodes).is_none();
                (compare || likeness).then(|| PairTask {
                    pair: PendingPair::new(
                        &row.rel_path,
                        nodes,
                        [models[0].path(l_id), models[1].path(r_id)],
                        sources.clone(),
                    ),
                    compare,
                    likeness,
                })
            })
            .collect()
    }

    /// The rows keep their remaining pairs Pending.
    pub fn cancel(&mut self) {
        if let Some(job) = &mut self.job {
            job.job.cancel();
            self.cancelled = true;
        }
    }

    pub fn is_comparing(&self) -> bool {
        self.job.is_some() && !self.cancelled
    }

    /// Done and total pairs of the running compare.
    pub fn progress(&self) -> Option<(usize, usize)> {
        let job = self.job.as_ref().filter(|_| !self.cancelled)?;
        Some((job.received, job.tasks.len()))
    }

    pub fn compare_time(&self) -> Option<Duration> {
        self.compare_time
    }

    pub fn poll(&mut self, now: Instant) -> CompareUpdate {
        let _scope = zcommon_ui::frame_stats::scope("Compare polling");
        let Some(job) = &mut self.job else {
            return CompareUpdate::Idle;
        };
        let (results, disconnected) = job.drain();
        for (i, result) in results {
            let pair = &job.tasks[i].pair;
            if let Some(compare) = result.compare {
                apply_result(&mut self.states, &mut self.cache, pair, compare);
                job.unpatched = true;
            }
            if let Some(likeness) = result.likeness {
                apply_likeness(
                    &mut self.likeness,
                    &mut self.likeness_cache,
                    &job.settings,
                    pair,
                    likeness,
                );
                // A likeness tolerance can turn the row Similar.
                job.unpatched = true;
            }
            job.received += 1;
        }
        let total = job.tasks.len();
        if self.cancelled || disconnected || job.received == total {
            let elapsed = now.saturating_duration_since(job.started);
            if job.tasks.iter().any(|task| task.compare) {
                self.compare_time = Some(elapsed);
            }
            let message = format!(
                "Compared {} of {total} file pairs in {} ms",
                job.received,
                elapsed.as_millis()
            );
            if job.received == total {
                log::info!("{message}");
            } else if self.cancelled {
                log::info!("{message}, cancelled");
            } else {
                log::error!("{message}, the compare workers stopped");
            }
            self.job = None;
            return CompareUpdate::Finished;
        }
        if job.unpatched && now >= job.patched + job.patch_interval {
            job.patched = now;
            job.unpatched = false;
            return CompareUpdate::Patch;
        }
        CompareUpdate::Idle
    }

    pub fn patched(&mut self, end: Instant, cost: Duration) {
        if let Some(job) = &mut self.job {
            job.patched = end;
            job.patch_interval = ROWS_PATCH_INTERVAL.max(cost * PATCH_COST_FACTOR);
        }
    }
}

/// Read errors stay out of the cache, so the next scan retries them.
fn apply_result(
    states: &mut HashMap<String, PairState>,
    cache: &mut PairCompareCache,
    pair: &PendingPair,
    result: PairCompare,
) {
    match &result.error {
        Some(e) => log::warn!(
            "Comparing {} with {}: {e}",
            pair.paths[0].display(),
            pair.paths[1].display()
        ),
        None => {
            if let Some([left, right]) = pair.cache_sides() {
                cache.insert(&left, &right, result.state);
            }
        }
    }
    states.insert(pair.rel_path.clone(), result.state);
}

/// Like a compare read error, a failed measure stays out of the cache.
fn apply_likeness(
    likeness: &mut HashMap<String, PairLikeness>,
    cache: &mut LruMap<String, CachedLikeness>,
    settings: &Arc<LikenessSettings>,
    pair: &PendingPair,
    result: io::Result<Likeness>,
) {
    let measured = match result {
        Ok(measured) => PairLikeness::Measured(measured),
        Err(e) => {
            log::warn!(
                "Measuring the likeness of {} and {}: {e}",
                pair.paths[0].display(),
                pair.paths[1].display()
            );
            PairLikeness::Failed(e.to_string())
        }
    };
    if let (PairLikeness::Measured(_), Some(metas)) = (&measured, pair.cache_metas()) {
        let cached = CachedLikeness {
            metas,
            settings: settings.clone(),
            likeness: measured.clone(),
        };
        let bytes = size_of::<(String, CachedLikeness)>() + pair.rel_path.len();
        cache.insert(pair.rel_path.clone(), cached, bytes);
    }
    likeness.insert(pair.rel_path.clone(), measured);
}

#[cfg(test)]
impl TreeCompare {
    pub fn pause(
        &mut self,
        now: Instant,
    ) -> (std::sync::mpsc::Sender<(usize, PairResult)>, Vec<String>) {
        let running = self.job.take().expect("a running compare");
        let tasks = running.tasks.to_vec();
        let rel_paths = tasks
            .iter()
            .map(|task| task.pair.rel_path.clone())
            .collect();
        let (sender, results) = std::sync::mpsc::channel::<(usize, PairResult)>();
        let mut job = CompareJob::new(
            tasks,
            running.likeness.load(Ordering::Relaxed),
            running.settings.clone(),
            now,
            self.wake(),
        );
        job.inbox = Some(results);
        job.started = running.started;
        self.job = Some(job);
        (sender, rel_paths)
    }

    /// Path, compare and likeness of each pair of the running compare.
    pub fn queued(&self) -> Vec<(&str, bool, bool)> {
        self.job
            .iter()
            .flat_map(|job| job.tasks.iter())
            .map(|task| (task.pair.rel_path.as_str(), task.compare, task.likeness))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompareUpdate, LikenessSettings, PairLikeness, PairResult, ROWS_PATCH_INTERVAL,
        TreeCompare, apply_result, pending_pairs,
    };
    use crate::folder::fs_tree::{DiffState, FileSystemView, VisibleRowTwoFolderDiff};
    use crate::folder::tree_likeness::{LikenessCell, likeness_cell};
    use crate::folder::tree_tolerance::{SizeDelta, Tolerance};
    use std::{
        fs::{self, File},
        io,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize},
        },
        time::{Duration, Instant, SystemTime},
    };
    use tempfile::{TempDir, tempdir};
    use zcommon::kinds::{FileKind, KindTable};
    use zdiff_folder::{
        comparison::{PairCompare, PairState},
        filter::PathFilter,
        fs::FileSystemModel,
        likeness::{Likeness, Measure},
        scan::{ContentHash, FlatEntry, FlatListing, HashKind, P4Recipe, scan},
    };

    fn write_files(root: &Path, files: &[(&str, &str)]) {
        for (rel_path, content) in files {
            let path = root.join(rel_path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
    }

    fn trees() -> (TempDir, TempDir) {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left.path(), right.path()] {
            write_files(root, &[("a.txt", "same a"), ("sub/c.txt", "same c")]);
        }
        write_files(left.path(), &[("b.txt", "left b"), ("left_only.txt", "l")]);
        write_files(right.path(), &[("b.txt", "right b")]);
        (left, right)
    }

    fn load_view(root: &Path) -> FileSystemView {
        FileSystemView::new(Arc::new(FileSystemModel::new(root).unwrap()))
    }

    fn build(
        compare: &mut TreeCompare,
        left: &FileSystemView,
        right: &FileSystemView,
    ) -> Vec<VisibleRowTwoFolderDiff> {
        build_tolerant(compare, left, right, &Tolerance::default())
    }

    fn build_tolerant(
        compare: &mut TreeCompare,
        left: &FileSystemView,
        right: &FileSystemView,
        tolerance: &Tolerance,
    ) -> Vec<VisibleRowTwoFolderDiff> {
        compare.retain_for([Some(left), Some(right)]);
        FileSystemView::build_two_folder_diff_rows(
            Some(left),
            Some(right),
            &PathFilter::default(),
            &KindTable::default(),
            tolerance,
            |rel_path, nodes| {
                (
                    compare.state_of(rel_path, nodes),
                    compare
                        .likeness_of(rel_path, nodes)
                        .and_then(PairLikeness::ratio),
                )
            },
            |_, _| None,
        )
        .unwrap()
    }

    fn compare_pending(
        compare: &mut TreeCompare,
        rows: &[VisibleRowTwoFolderDiff],
        left: &FileSystemView,
        right: &FileSystemView,
    ) -> Vec<String> {
        let pairs = pending_pairs(rows, [Some(left), Some(right)]);
        for pair in &pairs {
            apply_result(
                &mut compare.states,
                &mut compare.cache,
                pair,
                pair.compare(),
            );
        }
        pairs.into_iter().map(|pair| pair.rel_path).collect()
    }

    fn states(rows: &[VisibleRowTwoFolderDiff]) -> Vec<(&str, &'static str)> {
        rows.iter()
            .map(|row| {
                let kind = match row.diff_state {
                    DiffState::Different(..) => "Different",
                    DiffState::Same(..) => "Same",
                    DiffState::Similar(..) => "Similar",
                    DiffState::OnlyInFirst(..) => "OnlyInFirst",
                    DiffState::OnlyInSecond(..) => "OnlyInSecond",
                    DiffState::Pending(..) => "Pending",
                };
                (row.rel_path.as_str(), kind)
            })
            .collect()
    }

    fn state_at(rows: &[VisibleRowTwoFolderDiff], rel_path: &str) -> &'static str {
        states(rows)
            .into_iter()
            .find(|(path, _)| *path == rel_path)
            .unwrap()
            .1
    }

    #[test]
    fn a_rescan_compares_only_the_pairs_whose_files_changed() {
        let (left_dir, right_dir) = trees();
        let mut left = load_view(left_dir.path());
        let mut right = load_view(right_dir.path());
        let mut compare = TreeCompare::default();

        let rows = build(&mut compare, &left, &right);
        assert_eq!(
            compare_pending(&mut compare, &rows, &left, &right),
            ["a.txt", "b.txt", "sub/c.txt"]
        );
        let rows = build(&mut compare, &left, &right);
        assert_eq!(state_at(&rows, "b.txt"), "Different");

        fs::write(right_dir.path().join("b.txt"), "left b").unwrap();
        let rows = build(&mut compare, &left, &right);
        assert!(
            compare_pending(&mut compare, &rows, &left, &right).is_empty(),
            "the same trees are not compared again"
        );
        assert_eq!(state_at(&rows, "b.txt"), "Different");

        right = load_view(right_dir.path());
        let rows = build(&mut compare, &left, &right);
        assert_eq!(
            compare_pending(&mut compare, &rows, &left, &right),
            ["b.txt"]
        );
        let rows = build(&mut compare, &left, &right);
        assert_eq!(state_at(&rows, "b.txt"), "Same");

        fs::write(left_dir.path().join("b.txt"), "changed b").unwrap();
        left = load_view(left_dir.path());
        let rows = build(&mut compare, &left, &right);
        assert_eq!(
            compare_pending(&mut compare, &rows, &left, &right),
            ["b.txt"]
        );
        let rows = build(&mut compare, &left, &right);
        assert_eq!(state_at(&rows, "b.txt"), "Different");
    }

    fn depot_view(root: &Path, digest: &str) -> FileSystemView {
        let files = vec![FlatEntry {
            rel_path: "a.txt".into(),
            size: 6,
            hash: Some(ContentHash {
                kind: HashKind::P4Digest(P4Recipe::Raw),
                value: digest.into(),
            }),
        }];
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

    #[test]
    fn a_rescan_compares_a_depot_pair_again_only_when_its_digest_changed() {
        let local_dir = tempdir().unwrap();
        write_files(local_dir.path(), &[("a.txt", "same a")]);
        let local = load_view(local_dir.path());
        let depot_root = Path::new("//depot/main/...");
        let mut compare = TreeCompare::default();

        let depot = depot_view(depot_root, "AAAA");
        let rows = build(&mut compare, &local, &depot);
        assert_eq!(
            compare_pending(&mut compare, &rows, &local, &depot),
            ["a.txt"]
        );

        let depot = depot_view(depot_root, "AAAA");
        let rows = build(&mut compare, &local, &depot);
        assert!(compare_pending(&mut compare, &rows, &local, &depot).is_empty());

        let depot = depot_view(depot_root, "BBBB");
        let rows = build(&mut compare, &local, &depot);
        assert_eq!(
            compare_pending(&mut compare, &rows, &local, &depot),
            ["a.txt"]
        );
    }

    #[test]
    fn a_read_error_is_different_until_a_rescan_retries_it() {
        let (left_dir, right_dir) = trees();
        let left = load_view(left_dir.path());
        let mut right = load_view(right_dir.path());
        let mut compare = TreeCompare::default();
        let rows = build(&mut compare, &left, &right);
        let pairs = pending_pairs(&rows, [Some(&left), Some(&right)]);
        let a = pairs.iter().find(|pair| pair.rel_path == "a.txt").unwrap();

        let failed = PairCompare {
            state: PairState::Different,
            error: Some(io::Error::other("locked")),
        };
        apply_result(&mut compare.states, &mut compare.cache, a, failed);
        let rows = build(&mut compare, &left, &right);
        assert_eq!(state_at(&rows, "a.txt"), "Different");

        right = load_view(right_dir.path());
        let rows = build(&mut compare, &left, &right);
        assert_eq!(state_at(&rows, "a.txt"), "Pending");
    }

    fn poll_until_finished(compare: &mut TreeCompare) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while compare.poll(Instant::now()) != CompareUpdate::Finished {
            assert!(Instant::now() < deadline, "the compare did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_background_compare_fills_in_every_pending_pair() {
        let (left_dir, right_dir) = trees();
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let mut compare = TreeCompare::default();
        let rows = build(&mut compare, &left, &right);

        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        assert_eq!(compare.progress().map(|(_, total)| total), Some(3));
        poll_until_finished(&mut compare);

        assert!(!compare.is_comparing());
        let rows = build(&mut compare, &left, &right);
        assert_eq!(
            states(&rows),
            [
                ("", "Different"),
                ("a.txt", "Same"),
                ("b.txt", "Different"),
                ("left_only.txt", "OnlyInFirst"),
                ("sub", "Same"),
                ("sub/c.txt", "Same"),
            ]
        );
    }

    #[test]
    fn a_cancelled_compare_does_not_restart_until_the_trees_change() {
        let (left_dir, right_dir) = trees();
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let mut compare = TreeCompare::default();
        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        let _sender = compare.pause(Instant::now());

        compare.cancel();
        assert_eq!(compare.progress(), None);
        assert_eq!(compare.poll(Instant::now()), CompareUpdate::Finished);

        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        assert!(!compare.is_comparing());
        assert_eq!(compare.poll(Instant::now()), CompareUpdate::Idle);

        let right = load_view(right_dir.path());
        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        assert!(compare.is_comparing());
    }

    #[test]
    fn results_reach_the_rows_at_most_once_per_interval_and_at_the_end() {
        let (left_dir, right_dir) = trees();
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let mut compare = TreeCompare::default();
        let rows = build(&mut compare, &left, &right);
        let start = Instant::now();
        compare.start(&rows, [Some(&left), Some(&right)], start);
        let (sender, paths) = compare.pause(start);
        let same = |i| {
            let compare = PairCompare {
                state: PairState::Same,
                error: None,
            };
            let result = PairResult {
                compare: Some(compare),
                likeness: None,
            };
            sender.send((i, result)).unwrap();
        };

        assert_eq!(
            compare.poll(start + ROWS_PATCH_INTERVAL),
            CompareUpdate::Idle
        );
        same(0);
        assert_eq!(compare.poll(start), CompareUpdate::Idle);
        assert_eq!(compare.progress(), Some((1, paths.len())));
        let later = start + ROWS_PATCH_INTERVAL;
        assert_eq!(compare.poll(later), CompareUpdate::Patch);
        assert_eq!(
            compare.poll(later + ROWS_PATCH_INTERVAL),
            CompareUpdate::Idle
        );
        for i in 1..paths.len() {
            same(i);
        }
        assert_eq!(compare.poll(later), CompareUpdate::Finished);
        assert_eq!(compare.poll(later), CompareUpdate::Idle);
    }

    #[test]
    fn a_slow_patch_delays_the_next_one_from_its_end_in_proportion_to_its_cost() {
        let (left_dir, right_dir) = trees();
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let mut compare = TreeCompare::default();
        let rows = build(&mut compare, &left, &right);
        let start = Instant::now();
        compare.start(&rows, [Some(&left), Some(&right)], start);
        let (sender, paths) = compare.pause(start);
        assert!(paths.len() > 2);
        let same = |i| {
            let compare = PairCompare {
                state: PairState::Same,
                error: None,
            };
            let result = PairResult {
                compare: Some(compare),
                likeness: None,
            };
            sender.send((i, result)).unwrap();
        };

        same(0);
        let later = start + ROWS_PATCH_INTERVAL;
        assert_eq!(compare.poll(later), CompareUpdate::Patch);
        let cost = Duration::from_millis(300);
        let end = later + cost;
        compare.patched(end, cost);
        same(1);
        assert_eq!(compare.poll(end + ROWS_PATCH_INTERVAL), CompareUpdate::Idle);
        assert_eq!(compare.poll(end + 3 * cost), CompareUpdate::Idle);
        assert_eq!(compare.poll(end + 4 * cost), CompareUpdate::Patch);
    }

    #[test]
    fn a_likeness_result_reaches_the_rows_at_the_next_interval() {
        let (left_dir, right_dir) = trees();
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let mut compare = TreeCompare::default();
        compare.set_likeness(true);
        let rows = build(&mut compare, &left, &right);
        let start = Instant::now();
        compare.start(&rows, [Some(&left), Some(&right)], start);
        let (sender, paths) = compare.pause(start);
        assert!(paths.len() > 1);

        let result = PairResult {
            compare: None,
            likeness: Some(Ok(Likeness {
                ratio: 0.5,
                measure: Measure::Bytes,
                reason: None,
            })),
        };
        sender.send((0, result)).unwrap();

        assert_eq!(
            compare.poll(start + ROWS_PATCH_INTERVAL),
            CompareUpdate::Patch
        );
    }

    #[test]
    fn the_compare_time_spans_restarts_and_lasts_until_the_trees_change() {
        let (left_dir, right_dir) = trees();
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let mut compare = TreeCompare::default();
        let rows = build(&mut compare, &left, &right);
        let start = Instant::now();
        compare.start(&rows, [Some(&left), Some(&right)], start);
        compare.pause(start);
        assert_eq!(compare.compare_time(), None);

        let restart = start + Duration::from_secs(1);
        compare.start(&rows, [Some(&left), Some(&right)], restart);
        let (sender, paths) = compare.pause(restart);
        for i in 0..paths.len() {
            let result = PairResult {
                compare: Some(PairCompare {
                    state: PairState::Same,
                    error: None,
                }),
                likeness: None,
            };
            sender.send((i, result)).unwrap();
        }
        let end = start + Duration::from_secs(3);
        assert_eq!(compare.poll(end), CompareUpdate::Finished);
        assert_eq!(compare.compare_time(), Some(Duration::from_secs(3)));

        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], end);
        assert_eq!(compare.compare_time(), Some(Duration::from_secs(3)));

        let right = load_view(right_dir.path());
        compare.retain_for([Some(&left), Some(&right)]);
        assert_eq!(compare.compare_time(), None);
    }

    #[test]
    fn a_measure_only_job_keeps_the_compare_time() {
        let (left_dir, right_dir) = trees();
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let mut compare = TreeCompare::default();
        let rows = build(&mut compare, &left, &right);
        let start = Instant::now();
        compare.start(&rows, [Some(&left), Some(&right)], start);
        let (sender, paths) = compare.pause(start);
        for i in 0..paths.len() {
            let result = PairResult {
                compare: Some(PairCompare {
                    state: PairState::Different,
                    error: None,
                }),
                likeness: None,
            };
            sender.send((i, result)).unwrap();
        }
        let end = start + Duration::from_secs(3);
        assert_eq!(compare.poll(end), CompareUpdate::Finished);

        assert!(compare.set_likeness(true));
        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], end);
        let (sender, paths) = compare.pause(end);
        for i in 0..paths.len() {
            let result = PairResult {
                compare: None,
                likeness: Some(Err(io::Error::other("locked"))),
            };
            sender.send((i, result)).unwrap();
        }
        assert_eq!(
            compare.poll(end + Duration::from_secs(1)),
            CompareUpdate::Finished
        );
        assert_eq!(compare.compare_time(), Some(Duration::from_secs(3)));
    }

    fn likeness_at(
        compare: &mut TreeCompare,
        left: &FileSystemView,
        right: &FileSystemView,
        rel_path: &str,
    ) -> Option<PairLikeness> {
        let rows = build(compare, left, right);
        let row = rows.iter().find(|row| row.rel_path == rel_path).unwrap();
        let nodes = [
            left.file_system.get_node(row.diff_state.first()?)?,
            right.file_system.get_node(row.diff_state.second()?)?,
        ];
        compare.likeness_of(rel_path, nodes).cloned()
    }

    #[test]
    fn with_likeness_off_no_likeness_is_queued() {
        let (left_dir, right_dir) = trees();
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let mut compare = TreeCompare::default();
        let rows = build(&mut compare, &left, &right);

        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        assert_eq!(
            compare.queued(),
            [
                ("a.txt", true, false),
                ("b.txt", true, false),
                ("sub/c.txt", true, false)
            ]
        );
        poll_until_finished(&mut compare);

        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        assert!(!compare.is_comparing());
        assert_eq!(likeness_at(&mut compare, &left, &right, "b.txt"), None);
    }

    #[test]
    fn with_likeness_on_only_differing_file_pairs_are_measured() {
        let (left_dir, right_dir) = trees();
        write_files(left_dir.path(), &[("x", "file")]);
        write_files(right_dir.path(), &[("x/inner.txt", "folder")]);
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let mut compare = TreeCompare::default();
        let rows = build(&mut compare, &left, &right);
        compare_pending(&mut compare, &rows, &left, &right);

        assert!(compare.set_likeness(true));
        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());

        assert_eq!(compare.queued(), [("b.txt", false, true)]);
    }

    fn time_similar_trees() -> (TempDir, TempDir) {
        let (left_dir, right_dir) = trees();
        write_files(left_dir.path(), &[("x.txt", "aaaa")]);
        write_files(right_dir.path(), &[("x.txt", "bbbb")]);
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        for root in [left_dir.path(), right_dir.path()] {
            set_modified(&root.join("x.txt"), time);
        }
        // Back-to-back writes on both sides can share an NTFS timestamp tick.
        let other_time = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000);
        for rel_path in ["a.txt", "sub/c.txt"] {
            set_modified(&right_dir.path().join(rel_path), other_time);
        }
        (left_dir, right_dir)
    }

    fn set_modified(path: &Path, time: SystemTime) {
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    const TIME_ONLY: Tolerance = Tolerance {
        enabled: true,
        size: false,
        max_size_delta: SizeDelta::Bytes(0),
        same_size_and_time: true,
        likeness: false,
        min_likeness: 99.0,
    };

    #[test]
    fn a_pair_similar_by_modification_time_shows_no_likeness_spinner() {
        let (left_dir, right_dir) = time_similar_trees();
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let mut compare = TreeCompare::default();
        compare.set_likeness(true);
        let rows = build_tolerant(&mut compare, &left, &right, &TIME_ONLY);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        let _sender = compare.pause(Instant::now());
        assert!(compare.is_comparing());

        let row = rows.iter().find(|row| row.rel_path == "x.txt").unwrap();
        let nodes = [
            left.file_system.get_node(row.diff_state.first().unwrap()),
            right.file_system.get_node(row.diff_state.second().unwrap()),
        ];
        assert!(matches!(
            likeness_cell(row, nodes, &compare),
            LikenessCell::Empty
        ));
    }

    #[test]
    fn a_pair_similar_by_modification_time_is_neither_compared_nor_measured() {
        let (left_dir, right_dir) = time_similar_trees();
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let mut compare = TreeCompare::default();
        compare.set_likeness(true);
        let tolerance = TIME_ONLY;

        let rows = build_tolerant(&mut compare, &left, &right, &tolerance);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());

        assert_eq!(state_at(&rows, "x.txt"), "Similar");
        assert_eq!(
            compare.queued(),
            [
                ("a.txt", true, true),
                ("b.txt", true, true),
                ("sub/c.txt", true, true)
            ]
        );
    }

    #[test]
    fn a_compared_different_pair_that_is_similar_by_size_is_still_measured() {
        let (left_dir, right_dir) = trees();
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let mut compare = TreeCompare::default();
        let rows = build(&mut compare, &left, &right);
        compare_pending(&mut compare, &rows, &left, &right);
        compare.set_likeness(true);
        let tolerance = Tolerance {
            enabled: true,
            max_size_delta: SizeDelta::Bytes(1),
            ..Tolerance::default()
        };

        let rows = build_tolerant(&mut compare, &left, &right, &tolerance);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());

        assert_eq!(state_at(&rows, "b.txt"), "Similar");
        assert_eq!(compare.queued(), [("b.txt", false, true)]);
    }

    #[test]
    fn a_pending_pair_is_measured_after_its_compare_only_when_it_differs() {
        let (left_dir, right_dir) = trees();
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let mut compare = TreeCompare::default();
        compare.set_likeness(true);
        let rows = build(&mut compare, &left, &right);

        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        assert_eq!(
            compare.queued(),
            [
                ("a.txt", true, true),
                ("b.txt", true, true),
                ("sub/c.txt", true, true)
            ]
        );
        poll_until_finished(&mut compare);

        let Some(PairLikeness::Measured(b)) = likeness_at(&mut compare, &left, &right, "b.txt")
        else {
            panic!("b.txt was not measured");
        };
        assert_eq!(b.ratio, 0.0);
        assert_eq!(likeness_at(&mut compare, &left, &right, "a.txt"), None);
    }

    #[test]
    fn a_rescan_measures_only_the_pairs_whose_files_changed() {
        let (left_dir, right_dir) = trees();
        let left = load_view(left_dir.path());
        let mut right = load_view(right_dir.path());
        let mut compare = TreeCompare::default();
        compare.set_likeness(true);
        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        poll_until_finished(&mut compare);

        right = load_view(right_dir.path());
        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        assert!(!compare.is_comparing(), "{:?}", compare.queued());
        assert!(matches!(
            likeness_at(&mut compare, &left, &right, "b.txt"),
            Some(PairLikeness::Measured(_))
        ));

        fs::write(right_dir.path().join("b.txt"), "right b, changed").unwrap();
        right = load_view(right_dir.path());
        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        assert_eq!(compare.queued(), [("b.txt", true, true)]);
    }

    #[test]
    fn a_rescan_measures_again_under_other_likeness_settings() {
        let (left_dir, right_dir) = trees();
        let left = load_view(left_dir.path());
        let mut right = load_view(right_dir.path());
        let mut compare = TreeCompare::default();
        compare.set_likeness(true);
        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        poll_until_finished(&mut compare);

        let mut kinds = KindTable::default();
        kinds.set_override("txt", FileKind::Binary);
        for settings in [
            LikenessSettings {
                pixel_tolerance: 8,
                ..LikenessSettings::default()
            },
            LikenessSettings {
                kinds,
                ..LikenessSettings::default()
            },
        ] {
            compare.likeness_settings = Arc::new(settings);
            right = load_view(right_dir.path());
            let rows = build(&mut compare, &left, &right);
            assert_eq!(likeness_at(&mut compare, &left, &right, "b.txt"), None);
            compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
            assert_eq!(compare.queued(), [("b.txt", false, true)]);
            poll_until_finished(&mut compare);
        }
    }

    #[test]
    fn a_failed_likeness_is_kept_until_a_rescan_retries_it() {
        let (left_dir, right_dir) = trees();
        let left = load_view(left_dir.path());
        let mut right = load_view(right_dir.path());
        let mut compare = TreeCompare::default();
        compare.set_likeness(true);
        let rows = build(&mut compare, &left, &right);
        compare_pending(&mut compare, &rows, &left, &right);
        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        let (sender, _) = compare.pause(Instant::now());

        let failed = PairResult {
            compare: None,
            likeness: Some(Err(io::Error::other("locked"))),
        };
        sender.send((0, failed)).unwrap();
        assert_eq!(compare.poll(Instant::now()), CompareUpdate::Finished);
        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        assert!(!compare.is_comparing());
        assert_eq!(
            likeness_at(&mut compare, &left, &right, "b.txt"),
            Some(PairLikeness::Failed("locked".into()))
        );

        right = load_view(right_dir.path());
        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        assert_eq!(compare.queued(), [("b.txt", false, true)]);
    }

    #[test]
    fn turning_likeness_on_measures_even_after_a_cancel() {
        let (left_dir, right_dir) = trees();
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let mut compare = TreeCompare::default();
        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        let _sender = compare.pause(Instant::now());
        compare.cancel();
        assert_eq!(compare.poll(Instant::now()), CompareUpdate::Finished);

        assert!(compare.set_likeness(true));
        assert!(!compare.set_likeness(true));
        let rows = build(&mut compare, &left, &right);
        compare.start(&rows, [Some(&left), Some(&right)], Instant::now());
        assert!(compare.is_comparing());
    }
}
