use zcommon::history::{HistoryStep, SnapshotHistory};
use zdiff::universal_path::UniversalPath;

use super::{ZApp, state::AppStateCtx};
use crate::file::FileProcessor;

#[derive(Debug, Clone, PartialEq)]
pub struct NavSnapshot {
    sides: [(UniversalPath, Option<String>); 2],
    sides_swapped: bool,
}

pub type NavHistory = SnapshotHistory<NavSnapshot>;

fn read_side(file: &mut FileProcessor) -> (UniversalPath, Option<String>) {
    (file.get_full_path(), file.label().map(str::to_owned))
}

impl AppStateCtx {
    fn nav_snapshot(&mut self) -> NavSnapshot {
        NavSnapshot {
            sides: [read_side(&mut self.file_1), read_side(&mut self.file_2)],
            sides_swapped: self.sides_swapped,
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
        // Reverts are the latest actions on the shown pair: its history is cleared on a change.
        if app_ctx.revert_history.can_step(step) {
            Self::step_revert_history(app_ctx, step);
            return;
        }
        match app_ctx.nav_history.step(step) {
            Some(snapshot) => {
                log::info!(
                    "{step:?}: {} | {}",
                    snapshot.sides[0].0,
                    snapshot.sides[1].0
                );
                app_ctx.apply_nav(snapshot);
            }
            None => log::info!("{step:?}: nothing to step"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
