use super::{ZApp, state::AppStateCtx};
use eframe::egui;
use std::{path::Path, sync::mpsc};
use zcommon::vcs::revision::PathKind;
use zdiff::universal_path::UniversalPath;

use crate::{
    diff_ctx::DiffProcessor,
    file::FileProcessor,
    revert::{self, HistoryStep, RevertHistory, RevertRecord, RevertTarget, WriteRefusal},
};

impl ZApp {
    pub(super) fn open_file_picker(
        kind: PathKind,
        tx: mpsc::Sender<UniversalPath>,
        ctx: egui::Context,
    ) {
        std::thread::spawn(move || {
            let dialog = rfd::FileDialog::new();
            let picked = match kind {
                PathKind::File => dialog.pick_file(),
                PathKind::Folder => dialog.pick_folder(),
            };
            if let Some(path) = picked {
                if let Err(e) = tx.send(UniversalPath::from(path)) {
                    log::error!("Failed to send path: {e}");
                }
                ctx.request_repaint();
            }
        });
    }

    pub(super) fn refresh_file_contents(file_1: &mut FileProcessor, file_2: &mut FileProcessor) {
        file_1.invalidate_cache_file();
        file_2.invalidate_cache_file();
    }

    pub(super) fn clear_file_paths(
        file_1: &mut FileProcessor,
        file_2: &mut FileProcessor,
        diff_processor: &mut DiffProcessor,
    ) {
        file_1.set_path(UniversalPath::default());
        file_2.set_path(UniversalPath::default());
        diff_processor.reset_ctx();
    }

    /// Records a written revert for undo and reloads both sides; reloads the target when it was
    /// stale.
    pub(super) fn finish_revert_write(
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
    pub(super) fn step_revert_history(app_ctx: &mut AppStateCtx, step: HistoryStep) -> bool {
        let (name, since) = match step {
            HistoryStep::Undo => ("Undo", "the revert"),
            HistoryStep::Redo => ("Redo", "the undo"),
        };
        let stepped = app_ctx
            .revert_history
            .step(step, revert::write_history_step(&std::env::temp_dir()));
        let written = matches!(stepped, Some((_, Ok(()))));
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
                    "{name} refused: {} changed on disk since {since}. Reloading it; its \
                     reverts can no longer be undone or redone.",
                    path.display()
                );
                app_ctx.revert_history.forget_file(&path);
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
        written
    }
}
