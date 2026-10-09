use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime},
};

use zhashdiff::{
    comparison::{
        DiskReader, FileMeta, PairCompare, PairCompareCache, PairSide, PairState, compare_pair,
    },
    fs::{FileSystemModel, FsNode},
};

use crate::ui_egui::fs_tree::{DiffState, FileSystemView, VisibleRowTwoFolderDiff};

// Disk bound: more readers than this mostly make the reads seek against each other.
const MAX_COMPARE_WORKERS: usize = 4;
const ROWS_PATCH_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub struct PendingPair {
    pub rel_path: String,
    pub paths: [PathBuf; 2],
    sizes: [u64; 2],
    modified: [Option<SystemTime>; 2],
}

impl PendingPair {
    fn new(rel_path: &str, nodes: [&FsNode; 2]) -> Self {
        Self {
            rel_path: rel_path.to_owned(),
            paths: nodes.map(|node| node.as_path().as_ref().to_path_buf()),
            sizes: nodes.map(|node| node.size),
            modified: nodes.map(|node| node.modified),
        }
    }

    pub fn compare(&self) -> PairCompare {
        // compare_pair decides on the sizes and the content; the time is only a cache key.
        let side = |i: usize| PairSide {
            rel_path: &self.rel_path,
            path: &self.paths[i],
            meta: FileMeta {
                size: self.sizes[i],
                modified: self.modified[i].unwrap_or(SystemTime::UNIX_EPOCH),
            },
        };
        compare_pair(&DiskReader, &side(0), &side(1))
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
    })
}

/// The file pairs of `rows` still Pending, in row order.
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

/// One background compare of a list of pairs. Dropping it cancels the compare.
#[derive(Debug)]
struct CompareJob {
    pairs: Arc<[PendingPair]>,
    cancel: Arc<AtomicBool>,
    results: mpsc::Receiver<(usize, PairCompare)>,
    received: usize,
    started: Instant,
    patched: Instant,
    unpatched: bool,
}

impl CompareJob {
    fn new(pairs: Vec<PendingPair>, now: Instant) -> (Self, mpsc::Sender<(usize, PairCompare)>) {
        let (sender, results) = mpsc::channel();
        let job = Self {
            pairs: pairs.into(),
            cancel: Arc::new(AtomicBool::new(false)),
            results,
            received: 0,
            started: now,
            patched: now,
            unpatched: false,
        };
        (job, sender)
    }

    fn start(pairs: Vec<PendingPair>, now: Instant) -> Self {
        let (job, sender) = Self::new(pairs, now);
        let next = Arc::new(AtomicUsize::new(0));
        let workers = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(MAX_COMPARE_WORKERS)
            .min(job.pairs.len());
        for _ in 0..workers {
            let (pairs, cancel, next, sender) = (
                job.pairs.clone(),
                job.cancel.clone(),
                next.clone(),
                sender.clone(),
            );
            std::thread::Builder::new()
                .name("folder compare".into())
                .spawn(move || {
                    while !cancel.load(Ordering::Relaxed) {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(pair) = pairs.get(i) else {
                            break;
                        };
                        if sender.send((i, pair.compare())).is_err() {
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
    job: Option<CompareJob>,
    cancelled: bool,
}

impl TreeCompare {
    /// States, the job and a cancel belong to the models they were made for.
    pub fn retain_for(&mut self, views: [Option<&FileSystemView>; 2]) {
        let trees =
            views.map(|view| view.map_or_else(Weak::new, |v| Arc::downgrade(&v.file_system)));
        if !Weak::ptr_eq(&trees[0], &self.trees[0]) || !Weak::ptr_eq(&trees[1], &self.trees[1]) {
            self.trees = trees;
            self.states.clear();
            self.job = None;
            self.cancelled = false;
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

    /// Compares the pending pairs of `rows`, replacing any running compare. Nothing starts
    /// after a cancel until the models change.
    pub fn start(
        &mut self,
        rows: &[VisibleRowTwoFolderDiff],
        views: [Option<&FileSystemView>; 2],
        now: Instant,
    ) {
        let pairs = pending_pairs(rows, views);
        self.job = (!self.cancelled && !pairs.is_empty()).then(|| CompareJob::start(pairs, now));
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

    /// Compared and total pairs of the running compare.
    pub fn progress(&self) -> Option<(usize, usize)> {
        let job = self.job.as_ref().filter(|_| !self.cancelled)?;
        Some((job.received, job.pairs.len()))
    }

    pub fn poll(&mut self, now: Instant) -> CompareUpdate {
        let Some(job) = &mut self.job else {
            return CompareUpdate::Idle;
        };
        let mut disconnected = false;
        loop {
            match job.results.try_recv() {
                Ok((i, result)) => {
                    apply_result(&mut self.states, &mut self.cache, &job.pairs[i], result);
                    job.received += 1;
                    job.unpatched = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        let total = job.pairs.len();
        if self.cancelled || disconnected || job.received == total {
            let message = format!(
                "Compared {} of {total} file pairs in {} ms",
                job.received,
                now.saturating_duration_since(job.started).as_millis()
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

#[cfg(test)]
impl TreeCompare {
    pub fn pause(&mut self, now: Instant) -> (mpsc::Sender<(usize, PairCompare)>, Vec<String>) {
        let pairs = self.job.take().expect("a running compare").pairs.to_vec();
        let rel_paths = pairs.iter().map(|pair| pair.rel_path.clone()).collect();
        let (job, sender) = CompareJob::new(pairs, now);
        self.job = Some(job);
        (sender, rel_paths)
    }
}

#[cfg(test)]
mod tests {
    use super::{CompareUpdate, ROWS_PATCH_INTERVAL, TreeCompare, apply_result, pending_pairs};
    use crate::ui_egui::fs_tree::{DiffState, FileSystemView, VisibleRowTwoFolderDiff};
    use std::{
        fs, io,
        path::Path,
        sync::Arc,
        time::{Duration, Instant},
    };
    use tempfile::{TempDir, tempdir};
    use zcommon::kinds::KindTable;
    use zhashdiff::{
        comparison::{PairCompare, PairState},
        filter::PathFilter,
        fs::FileSystemModel,
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
        compare.retain_for([Some(left), Some(right)]);
        FileSystemView::build_two_folder_diff_rows(
            Some(left),
            Some(right),
            &PathFilter::default(),
            &KindTable::default(),
            |rel_path, nodes| compare.state_of(rel_path, nodes),
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
                    DiffState::Partial(..) => "Partial",
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
            let result = PairCompare {
                state: PairState::Same,
                error: None,
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
}
