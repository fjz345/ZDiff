use std::{
    collections::HashMap,
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime},
};

use memmap2::Mmap;
use zcommon::kinds::KindTable;
use zhashdiff::{
    comparison::{FileMeta, PairCompare, PairCompareCache, PairSide, PairState, compare_pair_with},
    fs::{FileSystemModel, FsNode},
    likeness::{Likeness, likeness},
    scan::ContentHash,
    vcs_source::ContentSource,
};

use crate::ui_egui::fs_tree::{DiffState, FileSystemView, VisibleRowTwoFolderDiff};

// Disk bound: more readers than this mostly make the reads seek against each other.
const MAX_COMPARE_WORKERS: usize = 4;
const ROWS_PATCH_INTERVAL: Duration = Duration::from_millis(250);
// The image viewer's default. The likeness cache doesn't key on it.
const PIXEL_TOLERANCE: u8 = 0;

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

#[derive(Debug, Clone)]
pub struct PendingPair {
    pub rel_path: String,
    pub paths: [PathBuf; 2],
    sizes: [u64; 2],
    modified: [Option<SystemTime>; 2],
    hashes: [Option<ContentHash>; 2],
    sources: [ContentSource; 2],
}

impl PendingPair {
    fn new(rel_path: &str, nodes: [&FsNode; 2], sources: [&ContentSource; 2]) -> Self {
        Self {
            rel_path: rel_path.to_owned(),
            paths: nodes.map(|node| node.as_path().as_ref().to_path_buf()),
            sizes: nodes.map(|node| node.size),
            modified: nodes.map(|node| node.modified),
            hashes: nodes.map(|node| node.hash.clone()),
            sources: sources.map(Clone::clone),
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
        kinds: &KindTable,
        cancel: &Arc<AtomicBool>,
    ) -> Option<io::Result<Likeness>> {
        // SAFETY: a file changed while mapped only skews this one measure.
        let map = |path: &Path| unsafe { Mmap::map(&File::open(path)?) };
        let (left, right) = match (map(&self.paths[0]), map(&self.paths[1])) {
            (Ok(left), Ok(right)) => (left, right),
            (Err(e), _) | (_, Err(e)) => return Some(Err(e)),
        };
        let extension = Path::new(&self.rel_path)
            .extension()
            .map_or(String::new(), |e| e.to_string_lossy().into_owned());
        likeness(kinds, &extension, &left, &right, PIXEL_TOLERANCE, cancel).map(Ok)
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
            )?,
            cache_side(
                &self.rel_path,
                &self.paths[1],
                self.sizes[1],
                self.modified[1],
            )?,
        ])
    }
}

/// None without a modification time: the cache could not tell that the file changed.
fn cache_side<'a>(
    rel_path: &'a str,
    path: &'a Path,
    size: u64,
    modified: Option<SystemTime>,
) -> Option<PairSide<'a>> {
    Some(PairSide {
        rel_path,
        path,
        meta: FileMeta {
            size,
            modified: modified?,
        },
        hash: None,
    })
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
                    views[0]?.file_system.content(),
                    views[1]?.file_system.content(),
                ],
            )),
            _ => None,
        })
        .collect()
}

/// Copies the states of `fresh` onto the rows with the same path, keeping the rows and their
/// order.
pub fn patch_row_states(rows: &mut [VisibleRowTwoFolderDiff], fresh: Vec<VisibleRowTwoFolderDiff>) {
    let mut fresh: HashMap<String, DiffState> = fresh
        .into_iter()
        .map(|row| (row.rel_path, row.diff_state))
        .collect();
    for row in rows {
        if let Some(state) = fresh.remove(&row.rel_path) {
            row.diff_state = state;
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
    cancel: Arc<AtomicBool>,
    likeness: Arc<AtomicBool>,
    results: mpsc::Receiver<(usize, PairResult)>,
    received: usize,
    started: Instant,
    patched: Instant,
    unpatched: bool,
}

impl CompareJob {
    fn new(
        tasks: Vec<PairTask>,
        likeness: bool,
        now: Instant,
    ) -> (Self, mpsc::Sender<(usize, PairResult)>) {
        let (sender, results) = mpsc::channel();
        let job = Self {
            tasks: tasks.into(),
            cancel: Arc::new(AtomicBool::new(false)),
            likeness: Arc::new(AtomicBool::new(likeness)),
            results,
            received: 0,
            started: now,
            patched: now,
            unpatched: false,
        };
        (job, sender)
    }

    fn start(tasks: Vec<PairTask>, likeness: bool, now: Instant) -> Self {
        let (job, sender) = Self::new(tasks, likeness, now);
        let next = Arc::new(AtomicUsize::new(0));
        let workers = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(MAX_COMPARE_WORKERS)
            .min(job.tasks.len());
        for _ in 0..workers {
            let (tasks, cancel, likeness, next, sender) = (
                job.tasks.clone(),
                job.cancel.clone(),
                job.likeness.clone(),
                next.clone(),
                sender.clone(),
            );
            std::thread::Builder::new()
                .name("folder compare".into())
                .spawn(move || {
                    let kinds = KindTable::default();
                    while !cancel.load(Ordering::Relaxed) {
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
                            let Some(result) = task.pair.likeness(&kinds, &cancel) else {
                                break;
                            };
                            Some(result)
                        } else {
                            None
                        };
                        if sender.send((i, PairResult { compare, likeness })).is_err() {
                            break;
                        }
                    }
                })
                .expect("failed to spawn a folder compare thread");
        }
        job
    }
}

impl Drop for CompareJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
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
    likeness: HashMap<String, PairLikeness>,
    likeness_cache: HashMap<String, ([FileMeta; 2], PairLikeness)>,
    job: Option<CompareJob>,
    cancelled: bool,
    compare_time: Option<Duration>,
}

impl TreeCompare {
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
        let paths = nodes.map(|node| node.as_path());
        let left = cache_side(
            rel_path,
            paths[0].as_ref(),
            nodes[0].size,
            nodes[0].modified,
        )?;
        let right = cache_side(
            rel_path,
            paths[1].as_ref(),
            nodes[1].size,
            nodes[1].modified,
        )?;
        self.cache.get(&left, &right)
    }

    pub fn likeness_of(&self, rel_path: &str, nodes: [&FsNode; 2]) -> Option<&PairLikeness> {
        if let Some(likeness) = self.likeness.get(rel_path) {
            return Some(likeness);
        }
        let metas = [
            file_meta(nodes[0].size, nodes[0].modified)?,
            file_meta(nodes[1].size, nodes[1].modified)?,
        ];
        let (cached_metas, likeness) = self.likeness_cache.get(rel_path)?;
        (*cached_metas == metas).then_some(likeness)
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
        let tasks = self.tasks(rows, views);
        let started = self.job.as_ref().map_or(now, |job| job.started);
        self.job = (!self.cancelled && !tasks.is_empty()).then(|| {
            let mut job = CompareJob::start(tasks, self.measure_likeness, now);
            job.started = started;
            job
        });
    }

    fn tasks(
        &self,
        rows: &[VisibleRowTwoFolderDiff],
        views: [Option<&FileSystemView>; 2],
    ) -> Vec<PairTask> {
        let node = |side: usize, id| views[side]?.file_system.get_node(id);
        rows.iter()
            .filter(|row| !row.is_dir)
            .filter_map(|row| {
                let (DiffState::Pending(l_id, r_id)
                | DiffState::Different(l_id, r_id)
                | DiffState::Similar(l_id, r_id)) = row.diff_state
                else {
                    return None;
                };
                let nodes = [node(0, l_id)?, node(1, r_id)?];
                let sources = [
                    views[0]?.file_system.content(),
                    views[1]?.file_system.content(),
                ];
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
                    && sources.iter().all(|source| **source == ContentSource::Disk)
                    && self.likeness_of(&row.rel_path, nodes).is_none();
                (compare || likeness).then(|| PairTask {
                    pair: PendingPair::new(&row.rel_path, nodes, sources),
                    compare,
                    likeness,
                })
            })
            .collect()
    }

    /// The rows keep their remaining pairs Pending.
    pub fn cancel(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
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
        let Some(job) = &mut self.job else {
            return CompareUpdate::Idle;
        };
        let mut disconnected = false;
        loop {
            match job.results.try_recv() {
                Ok((i, result)) => {
                    let pair = &job.tasks[i].pair;
                    if let Some(compare) = result.compare {
                        apply_result(&mut self.states, &mut self.cache, pair, compare);
                        job.unpatched = true;
                    }
                    if let Some(likeness) = result.likeness {
                        apply_likeness(
                            &mut self.likeness,
                            &mut self.likeness_cache,
                            pair,
                            likeness,
                        );
                        // A likeness tolerance can turn the row Similar.
                        job.unpatched = true;
                    }
                    job.received += 1;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
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
        if job.unpatched && now >= job.patched + ROWS_PATCH_INTERVAL {
            job.patched = now;
            job.unpatched = false;
            return CompareUpdate::Patch;
        }
        CompareUpdate::Idle
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
    cache: &mut HashMap<String, ([FileMeta; 2], PairLikeness)>,
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
        cache.insert(pair.rel_path.clone(), (metas, measured.clone()));
    }
    likeness.insert(pair.rel_path.clone(), measured);
}

#[cfg(test)]
impl TreeCompare {
    pub fn pause(&mut self, now: Instant) -> (mpsc::Sender<(usize, PairResult)>, Vec<String>) {
        let running = self.job.take().expect("a running compare");
        let tasks = running.tasks.to_vec();
        let rel_paths = tasks
            .iter()
            .map(|task| task.pair.rel_path.clone())
            .collect();
        let (mut job, sender) =
            CompareJob::new(tasks, running.likeness.load(Ordering::Relaxed), now);
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
        CompareUpdate, PairLikeness, PairResult, ROWS_PATCH_INTERVAL, TreeCompare, apply_result,
        pending_pairs,
    };
    use crate::ui_egui::fs_tree::{DiffState, FileSystemView, VisibleRowTwoFolderDiff};
    use crate::ui_egui::tree_likeness::{LikenessCell, likeness_cell};
    use crate::ui_egui::tree_tolerance::{SizeDelta, Tolerance};
    use std::{
        fs::{self, File},
        io,
        path::Path,
        sync::Arc,
        time::{Duration, Instant, SystemTime},
    };
    use tempfile::{TempDir, tempdir};
    use zcommon::kinds::KindTable;
    use zhashdiff::{
        comparison::{PairCompare, PairState},
        filter::PathFilter,
        fs::FileSystemModel,
        likeness::{Likeness, Measure},
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
            File::options()
                .write(true)
                .open(root.join("x.txt"))
                .unwrap()
                .set_modified(time)
                .unwrap();
        }
        (left_dir, right_dir)
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
