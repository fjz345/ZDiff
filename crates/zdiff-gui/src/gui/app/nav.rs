use zcommon::history::{HistoryStep, SnapshotHistory};
use zdiff::universal_path::UniversalPath;

use super::{ZApp, state::AppStateCtx};
use crate::file::FileProcessor;

#[derive(Debug, Clone, PartialEq)]
pub struct NavSnapshot {
    sides: [(UniversalPath, Option<String>); 2],
    sides_swapped: bool,
    browser_folders: [Option<String>; 2],
}

#[derive(Debug, Clone, Copy)]
enum Action {
    Navigation,
    Revert,
}

enum Stepped {
    Navigation(Box<NavSnapshot>),
    Revert,
}

#[derive(Debug, Default)]
pub struct NavHistory {
    snapshots: SnapshotHistory<NavSnapshot>,
    undo_order: Vec<Action>,
    redo_order: Vec<Action>,
}

impl NavHistory {
    fn observe(&mut self, now: &NavSnapshot) {
        let changed = self
            .snapshots
            .newest_first()
            .next()
            .is_some_and(|current| current != now);
        self.snapshots.observe(now);
        if changed {
            self.record(Action::Navigation);
        }
    }

    pub fn revert_recorded(&mut self) {
        self.snapshots.clear_redo();
        self.record(Action::Revert);
    }

    fn record(&mut self, action: Action) {
        self.undo_order.push(action);
        self.redo_order.clear();
    }

    fn order(&mut self, step: HistoryStep) -> (&mut Vec<Action>, &mut Vec<Action>) {
        match step {
            HistoryStep::Undo => (&mut self.undo_order, &mut self.redo_order),
            HistoryStep::Redo => (&mut self.redo_order, &mut self.undo_order),
        }
    }

    fn step(&mut self, step: HistoryStep, revert_can_step: bool) -> Option<Stepped> {
        loop {
            let latest = *self.order(step).0.last()?;
            let stepped = match latest {
                Action::Revert if revert_can_step => return Some(Stepped::Revert),
                // Its pair is no longer shown, or its file changed on disk.
                Action::Revert => None,
                Action::Navigation => self
                    .snapshots
                    .step(step)
                    .map(|snapshot| Stepped::Navigation(Box::new(snapshot))),
            };
            let (from, to) = self.order(step);
            let action = from.pop();
            if stepped.is_some() {
                to.extend(action);
                return stepped;
            }
        }
    }

    fn revert_stepped(&mut self, step: HistoryStep) {
        let (from, to) = self.order(step);
        to.extend(from.pop());
    }
}

fn read_side(file: &mut FileProcessor) -> (UniversalPath, Option<String>) {
    (file.get_full_path(), file.label().map(str::to_owned))
}

impl AppStateCtx {
    fn nav_snapshot(&mut self) -> NavSnapshot {
        NavSnapshot {
            sides: [read_side(&mut self.file_1), read_side(&mut self.file_2)],
            sides_swapped: self.sides_swapped,
            browser_folders: self.browser_folders.clone(),
        }
    }

    pub(super) fn observe_history(&mut self) {
        let snapshot = self.nav_snapshot();
        let [(path_1, _), (path_2, _)] = &snapshot.sides;
        self.revert_history.observe_pair(path_1, path_2);
        self.nav_history.observe(&snapshot);
    }

    fn apply_nav(&mut self, snapshot: NavSnapshot) {
        let [(path_1, label_1), (path_2, label_2)] = snapshot.sides;
        self.file_1.restore_path(path_1, label_1);
        self.file_2.restore_path(path_2, label_2);
        self.sides_swapped = snapshot.sides_swapped;
        self.browser_folders = snapshot.browser_folders;
    }
}

pub fn back_to_previous_revision(file: &mut FileProcessor, history: &NavHistory, side: usize) {
    let current = file.get_full_path();
    match previous_side(history, side, &current) {
        Some((path, label)) => file.restore_path(path.clone(), label.clone()),
        None => log::error!("No previous revision of {current} to go back to"),
    }
}

fn previous_side<'a>(
    history: &'a NavHistory,
    side: usize,
    current: &UniversalPath,
) -> Option<&'a (UniversalPath, Option<String>)> {
    history
        .snapshots
        .newest_first()
        .map(|snapshot| &snapshot.sides[side])
        .find(|(path, _)| path != current)
        .filter(|(path, _)| !path.is_empty() && file_name(path) == file_name(current))
}

pub fn previous_revision(
    history: &NavHistory,
    side: usize,
    current: &UniversalPath,
) -> Option<UniversalPath> {
    previous_side(history, side, current).map(|(path, _)| path.clone())
}

fn file_name(path: &UniversalPath) -> String {
    let full = match path {
        UniversalPath::Depot(file, _) => file.clone(),
        UniversalPath::Local(file) | UniversalPath::Git(file, _) => {
            file.to_string_lossy().into_owned()
        }
    };
    full.rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

impl ZApp {
    pub(super) fn step_history(app_ctx: &mut AppStateCtx, step: HistoryStep) {
        app_ctx.observe_history();
        let revert_can_step = app_ctx.revert_history.can_step(step);
        match app_ctx.nav_history.step(step, revert_can_step) {
            Some(Stepped::Revert) => {
                if Self::step_revert_history(app_ctx, step) {
                    app_ctx.nav_history.revert_stepped(step);
                }
            }
            Some(Stepped::Navigation(snapshot)) => {
                log::info!(
                    "{step:?}: {} | {}",
                    snapshot.sides[0].0,
                    snapshot.sides[1].0
                );
                app_ctx.apply_nav(*snapshot);
            }
            None => log::info!("{step:?}: nothing to step"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::revert::RevertRecord;
    use HistoryStep::{Redo, Undo};

    fn paths(ctx: &mut AppStateCtx) -> [UniversalPath; 2] {
        [ctx.file_1.get_full_path(), ctx.file_2.get_full_path()]
    }

    #[test]
    fn a_revision_pick_is_undone_and_redone_even_before_a_frame_observed_it() {
        let mut ctx = AppStateCtx::default();
        let local = UniversalPath::from(r"C:\ws\a.rs");
        let branch = UniversalPath::from(r"C:\ws\a.rs@git:main");
        ctx.file_1.set_path(local.clone());
        ctx.file_2.set_path(UniversalPath::from(r"C:\ws\b.rs"));
        ctx.observe_history();
        ctx.file_1.set_path(branch.clone());

        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(paths(&mut ctx)[0], local);
        ctx.observe_history();
        ZApp::step_history(&mut ctx, Redo);
        assert_eq!(paths(&mut ctx)[0], branch);
        ZApp::step_history(&mut ctx, Redo);
        assert_eq!(paths(&mut ctx)[0], branch);
    }

    #[test]
    fn a_revert_undo_refused_as_stale_no_longer_blocks_navigation_undo() {
        let mut ctx = AppStateCtx::default();
        let first = UniversalPath::from(r"C:\ws\a.rs");
        ctx.file_1.set_path(first.clone());
        ctx.observe_history();
        ctx.file_1.set_path(UniversalPath::from(r"C:\ws\b.rs"));
        ctx.observe_history();
        ctx.revert_history.record(RevertRecord {
            path: PathBuf::from("deleted since the revert").join("b.rs"),
            before: b"before".to_vec(),
            after: b"after".to_vec(),
        });
        ctx.nav_history.revert_recorded();

        ZApp::step_history(&mut ctx, Undo);
        assert!(!ctx.revert_history.can_step(Undo));
        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(paths(&mut ctx)[0], first);
    }

    #[test]
    fn undo_restores_a_quick_diff_label_and_the_swap() {
        let mut ctx = AppStateCtx::default();
        ctx.file_1
            .set_path(UniversalPath::from("//depot/main/a.rs#3"));
        ctx.file_1.set_label(Some("Shelf base".to_owned()));
        ctx.observe_history();
        ctx.file_1.set_path(UniversalPath::from(r"C:\ws\c.rs"));
        ctx.observe_history();
        ctx.sides_swapped = true;

        ZApp::step_history(&mut ctx, Undo);
        assert!(!ctx.sides_swapped);
        assert_eq!(ctx.file_1.label(), None);
        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(
            paths(&mut ctx)[0],
            UniversalPath::from("//depot/main/a.rs#3")
        );
        assert_eq!(ctx.file_1.label(), Some("Shelf base"));
    }

    fn browsers(ctx: &AppStateCtx) -> [Option<&str>; 2] {
        ctx.browser_folders.each_ref().map(Option::as_deref)
    }

    fn diff_of_two_files() -> AppStateCtx {
        let mut ctx = AppStateCtx::default();
        ctx.file_1.set_path(UniversalPath::from(r"C:\ws\src\a.rs"));
        ctx.file_2.set_path(UniversalPath::from(r"C:\ws\src\b.rs"));
        ctx.observe_history();
        ctx
    }

    #[test]
    fn back_closes_the_parent_folder_view_and_forward_opens_it_again() {
        let mut ctx = diff_of_two_files();
        let diff = paths(&mut ctx);
        ctx.browser_folders[0] = Some(r"C:\ws\src".to_owned());
        ctx.observe_history();

        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(browsers(&ctx), [None, None]);
        assert_eq!(paths(&mut ctx), diff);
        ZApp::step_history(&mut ctx, Redo);
        assert_eq!(browsers(&ctx), [Some(r"C:\ws\src"), None]);
        assert_eq!(paths(&mut ctx), diff);
    }

    #[test]
    fn back_from_the_parents_parent_shows_the_parent_folder_again() {
        let mut ctx = diff_of_two_files();
        ctx.browser_folders[1] = Some(r"C:\ws\src".to_owned());
        ctx.observe_history();
        ctx.browser_folders[1] = Some(r"C:\ws".to_owned());
        ctx.observe_history();

        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(browsers(&ctx), [None, Some(r"C:\ws\src")]);
        ZApp::step_history(&mut ctx, Redo);
        assert_eq!(browsers(&ctx), [None, Some(r"C:\ws")]);
    }

    #[test]
    fn back_from_a_picked_file_reopens_its_folder_then_the_diff_before() {
        let mut ctx = diff_of_two_files();
        let diff = paths(&mut ctx);
        ctx.browser_folders[0] = Some(r"C:\ws".to_owned());
        ctx.observe_history();
        let picked = UniversalPath::from(r"C:\ws\c.rs");
        ctx.file_1.set_path(picked.clone());
        ctx.browser_folders[0] = None;
        ctx.observe_history();

        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(browsers(&ctx), [Some(r"C:\ws"), None]);
        assert_eq!(paths(&mut ctx), diff);
        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(browsers(&ctx), [None, None]);
        assert_eq!(paths(&mut ctx), diff);

        ZApp::step_history(&mut ctx, Redo);
        assert_eq!(browsers(&ctx), [Some(r"C:\ws"), None]);
        ZApp::step_history(&mut ctx, Redo);
        assert_eq!(browsers(&ctx), [None, None]);
        assert_eq!(paths(&mut ctx)[0], picked);
    }

    fn diff_of_a_writable_file() -> (tempfile::TempDir, AppStateCtx, PathBuf) {
        let outside_the_os_temp_dir = std::env::current_exe().unwrap();
        let dir = tempfile::tempdir_in(outside_the_os_temp_dir.parent().unwrap()).unwrap();
        let file = dir.path().join("a.rs");
        std::fs::write(&file, "before").unwrap();
        let mut ctx = AppStateCtx::default();
        ctx.file_1.set_path(UniversalPath::from(file.clone()));
        ctx.file_2.set_path(UniversalPath::from(r"C:\ws\src\b.rs"));
        ctx.observe_history();
        (dir, ctx, file)
    }

    fn revert(ctx: &mut AppStateCtx, file: &Path) {
        std::fs::write(file, "after").unwrap();
        ctx.revert_history.record(RevertRecord {
            path: file.to_owned(),
            before: b"before".to_vec(),
            after: b"after".to_vec(),
        });
        ctx.nav_history.revert_recorded();
    }

    fn contents(file: &Path) -> String {
        std::fs::read_to_string(file).unwrap()
    }

    #[test]
    fn back_closes_a_folder_view_opened_after_a_revert_before_undoing_the_revert() {
        let (_dir, mut ctx, file) = diff_of_a_writable_file();
        revert(&mut ctx, &file);
        ctx.browser_folders[0] = Some(r"C:\ws".to_owned());
        ctx.observe_history();
        ctx.browser_folders[0] = Some(r"C:\".to_owned());
        ctx.observe_history();

        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(browsers(&ctx), [Some(r"C:\ws"), None]);
        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(browsers(&ctx), [None, None]);
        assert_eq!(contents(&file), "after");
        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(contents(&file), "before");

        ZApp::step_history(&mut ctx, Redo);
        assert_eq!(contents(&file), "after");
        assert_eq!(browsers(&ctx), [None, None]);
        ZApp::step_history(&mut ctx, Redo);
        assert_eq!(browsers(&ctx), [Some(r"C:\ws"), None]);
        ZApp::step_history(&mut ctx, Redo);
        assert_eq!(browsers(&ctx), [Some(r"C:\"), None]);
    }

    #[test]
    fn a_revert_made_after_a_folder_view_was_closed_is_undone_before_the_view_reopens() {
        let (_dir, mut ctx, file) = diff_of_a_writable_file();
        ctx.browser_folders[1] = Some(r"C:\ws".to_owned());
        ctx.observe_history();
        ctx.browser_folders[1] = None;
        ctx.observe_history();
        revert(&mut ctx, &file);
        ctx.sides_swapped = true;

        ZApp::step_history(&mut ctx, Undo);
        assert!(!ctx.sides_swapped);
        assert_eq!(contents(&file), "after");
        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(contents(&file), "before");
        assert_eq!(browsers(&ctx), [None, None]);
        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(browsers(&ctx), [None, Some(r"C:\ws")]);

        ZApp::step_history(&mut ctx, Redo);
        assert_eq!(browsers(&ctx), [None, None]);
        assert_eq!(contents(&file), "before");
        ZApp::step_history(&mut ctx, Redo);
        assert_eq!(contents(&file), "after");
        assert!(!ctx.sides_swapped);
        ZApp::step_history(&mut ctx, Redo);
        assert!(ctx.sides_swapped);
    }

    #[test]
    fn a_revert_of_a_pair_navigated_away_from_does_not_swallow_a_back() {
        let (_dir, mut ctx, file) = diff_of_a_writable_file();
        let pair = paths(&mut ctx);
        revert(&mut ctx, &file);
        ctx.file_2.set_path(UniversalPath::from(r"C:\ws\src\c.rs"));
        ctx.observe_history();
        ctx.browser_folders[0] = Some(r"C:\ws".to_owned());
        ctx.observe_history();

        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(browsers(&ctx), [None, None]);
        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(paths(&mut ctx), pair);
        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(paths(&mut ctx), pair);
        assert_eq!(contents(&file), "after");

        ZApp::step_history(&mut ctx, Redo);
        assert_eq!(paths(&mut ctx)[1], UniversalPath::from(r"C:\ws\src\c.rs"));
    }

    #[test]
    fn back_to_the_previous_revision_restores_its_label() {
        let mut ctx = AppStateCtx::default();
        let shelf_base = UniversalPath::from("//depot/main/a.rs#3");
        ctx.file_1.set_path(shelf_base.clone());
        ctx.file_1.set_label(Some("Shelf base".to_owned()));
        ctx.observe_history();
        ctx.file_1.set_path(UniversalPath::from(r"C:\ws\a.rs"));
        ctx.observe_history();

        back_to_previous_revision(&mut ctx.file_1, &ctx.nav_history, 0);
        assert_eq!(paths(&mut ctx)[0], shelf_base);
        assert_eq!(ctx.file_1.label(), Some("Shelf base"));
    }

    #[test]
    fn the_previous_revision_is_the_same_file_before_its_latest_change() {
        let mut ctx = AppStateCtx::default();
        let previous = |ctx: &mut AppStateCtx, side: usize| {
            let current = paths(ctx)[side].clone();
            previous_revision(&ctx.nav_history, side, &current)
        };
        let local = UniversalPath::from(r"C:\ws\a.rs");
        let branch = UniversalPath::from(r"C:\ws\a.rs@git:main");
        ctx.file_1.set_path(local.clone());
        ctx.observe_history();
        assert_eq!(previous(&mut ctx, 0), None);

        ctx.file_1.set_path(branch.clone());
        ctx.observe_history();
        ctx.file_2.set_path(UniversalPath::from(r"C:\ws\b.rs"));
        ctx.observe_history();
        assert_eq!(previous(&mut ctx, 0), Some(local.clone()));
        assert_eq!(previous(&mut ctx, 1), None, "was empty");

        ctx.file_1
            .set_path(UniversalPath::from("//stream/main/a.rs#3"));
        assert_eq!(
            previous(&mut ctx, 0),
            Some(branch.clone()),
            "not observed yet"
        );
        ctx.observe_history();
        assert_eq!(previous(&mut ctx, 0), Some(branch));

        ctx.file_1.set_path(UniversalPath::from(r"C:\ws\other.rs"));
        ctx.observe_history();
        assert_eq!(previous(&mut ctx, 0), None, "another file");

        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(
            previous(&mut ctx, 0),
            Some(UniversalPath::from(r"C:\ws\a.rs@git:main"))
        );
    }

    #[test]
    fn back_and_forward_step_through_folder_root_changes() {
        let mut ctx = AppStateCtx::default();
        let left = UniversalPath::from(r"C:\ws\src");
        let branch = UniversalPath::from(r"C:\ws\src@git:main");
        let right = UniversalPath::from("//depot/main/src");
        ctx.file_1.set_path(left.clone());
        ctx.file_2.set_path(right.clone());
        ctx.observe_history();
        ctx.file_1.set_path(branch.clone());
        ctx.observe_history();
        ctx.sides_swapped = true;
        ctx.observe_history();

        ZApp::step_history(&mut ctx, Undo);
        assert!(!ctx.sides_swapped);
        assert_eq!(paths(&mut ctx), [branch.clone(), right.clone()]);
        ZApp::step_history(&mut ctx, Undo);
        assert_eq!(paths(&mut ctx), [left, right.clone()]);

        ZApp::step_history(&mut ctx, Redo);
        ZApp::step_history(&mut ctx, Redo);
        assert!(ctx.sides_swapped);
        assert_eq!(paths(&mut ctx), [branch, right]);
    }

    #[test]
    fn the_previous_revision_of_a_folder_side_is_the_same_folder_before_its_latest_change() {
        let mut ctx = AppStateCtx::default();
        let previous = |ctx: &mut AppStateCtx, side: usize| {
            let current = paths(ctx)[side].clone();
            previous_revision(&ctx.nav_history, side, &current)
        };
        let local = UniversalPath::from(r"C:\ws\src");
        let branch = UniversalPath::from(r"C:\ws\src@git:main");
        ctx.file_1.set_path(local.clone());
        ctx.observe_history();
        assert_eq!(previous(&mut ctx, 0), None);

        ctx.file_1.set_path(branch.clone());
        ctx.observe_history();
        ctx.file_2.set_path(UniversalPath::from(r"C:\ws\docs"));
        ctx.observe_history();
        assert_eq!(previous(&mut ctx, 0), Some(local));
        assert_eq!(previous(&mut ctx, 1), None, "was empty");

        ctx.file_1.set_path(UniversalPath::from("//stream/main/src"));
        ctx.observe_history();
        assert_eq!(previous(&mut ctx, 0), Some(branch));

        ctx.file_1.set_path(UniversalPath::from(r"C:\ws\docs"));
        ctx.observe_history();
        assert_eq!(previous(&mut ctx, 0), None, "another folder");
    }
}
