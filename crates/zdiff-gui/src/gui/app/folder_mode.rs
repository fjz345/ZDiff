use eframe::egui;
use zdiff::universal_path::UniversalPath;

use super::{ZApp, state::AppStateCtx, ui::send_title};
use crate::{
    file::FileProcessor,
    folder::view::{FolderShell, root_of_side},
    viewer::{Viewers, folder_pair, folder_side_errors},
};

pub(super) fn folder_key(path: &UniversalPath) -> String {
    match path {
        UniversalPath::Depot(folder, _) => folder.clone(),
        UniversalPath::Local(folder) | UniversalPath::Git(folder, _) => {
            folder.to_string_lossy().into_owned()
        }
    }
}

pub(super) fn load_errors(
    file_1: &mut FileProcessor,
    file_2: &mut FileProcessor,
) -> [Option<String>; 2] {
    let held = folder_side_errors([file_1.kind(), file_2.kind()], false);
    [(file_1, held[0]), (file_2, held[1])].map(|(file, held)| {
        if held {
            let path = file.get_path_as_string();
            Some(format!("{path} is a folder, the other side is a file"))
        } else {
            file.load_error().map(str::to_owned)
        }
    })
}

impl AppStateCtx {
    pub(super) fn resolve_folder_pair(&mut self, merge_open: bool) -> bool {
        let kinds = [self.file_1.kind(), self.file_2.kind()];
        let keys = [self.file_1.get_full_path(), self.file_2.get_full_path()]
            .map(|path| folder_key(&path));
        let known = keys.each_ref().map(|key| self.known_folders.contains(key));
        let folders = !merge_open && folder_pair(kinds, known);
        let held = folder_side_errors(kinds, folders);
        self.file_1.hold_load(folders || held[0]);
        self.file_2.hold_load(folders || held[1]);
        if folders {
            let shown = keys.into_iter().filter(|key| !key.is_empty());
            self.known_folders.extend(shown);
        }
        folders
    }
}

impl ZApp {
    pub(super) fn folder_frame(
        &mut self,
        ctx: &egui::Context,
        state: &mut AppStateCtx,
        map_claims_back: bool,
    ) {
        state.observe_history();
        state.viewers = Some(Viewers::Folders);
        let sides = [state.file_1.get_full_path(), state.file_2.get_full_path()];
        self.folder.set_roots(sides.each_ref().map(root_of_side));
        let shown = self.folder.roots();
        self.folder.frame(
            ctx,
            FolderShell {
                frame_stats: &mut self.frame_stats,
                show_frame_breakdown: &mut self.show_frame_breakdown,
                style_window: &mut self.style_window,
                map_claims_back,
            },
        );
        // Until U3 the folder still edits its own roots (bars, dialogs, drops, scan revert).
        let files = [&mut state.file_1, &mut state.file_2];
        let roots = shown.into_iter().zip(self.folder.roots());
        for (file, (shown, root)) in files.into_iter().zip(roots) {
            if shown != root {
                file.restore_path(root.map(UniversalPath::from).unwrap_or_default(), None);
            }
        }
        send_title(ctx, &mut self.window_title, self.folder.title());
    }

    #[cfg(test)]
    pub(crate) fn idle_showing_folder(folder: crate::folder::view::FolderView) -> ZApp {
        use super::state::AppState;

        assert!(
            folder.roots().iter().any(Option::is_some),
            "without a root the sides are no folder pair"
        );
        let mut state = AppStateCtx::default();
        for (file, root) in [&mut state.file_1, &mut state.file_2]
            .into_iter()
            .zip(folder.roots())
        {
            let side = root.map(UniversalPath::from).unwrap_or_default();
            state.known_folders.insert(folder_key(&side));
            file.restore_path(side, None);
        }
        let mut app = ZApp::new();
        app.folder = folder;
        app.state = Some(AppState::Idle(state));
        app
    }
}
