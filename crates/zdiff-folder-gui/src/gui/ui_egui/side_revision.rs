use std::path::{Path, PathBuf};

use eframe::egui;
use zcommon::{
    universal_path::{DepotRev, GitRev, UniversalPath},
    vcs::revision::PathKind,
};
use zcommon_ui::revision::SOURCE_CONTROL;

/// A side's revision; `Git(None)` is the working tree and `Depot(None)` is head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideRev {
    Git(Option<GitRev>),
    Depot(Option<DepotRev>),
}

/// The folder of a side's root and its revision.
pub fn split_side(root: &Path) -> (PathBuf, SideRev) {
    match UniversalPath::from(root) {
        UniversalPath::Git(folder, rev) => (folder, SideRev::Git(Some(rev))),
        UniversalPath::Depot(folder, rev) => (PathBuf::from(folder), SideRev::Depot(rev)),
        UniversalPath::Local(_) => (root.to_path_buf(), SideRev::Git(None)),
    }
}

/// Same folder at another revision of the same kind as `rev`.
pub fn with_revision(root: &Path, rev: SideRev) -> PathBuf {
    let (folder, _) = split_side(root);
    match rev {
        SideRev::Git(None) => folder,
        SideRev::Git(Some(rev)) => PathBuf::from(UniversalPath::Git(folder, rev).to_string()),
        SideRev::Depot(rev) => {
            let folder = folder.to_string_lossy().into_owned();
            PathBuf::from(UniversalPath::Depot(folder, rev).to_string())
        }
    }
}

/// A local root is kept as set, like `split_side` does: `UniversalPath` would normalize it.
fn universal_root(root: &Path) -> UniversalPath {
    match UniversalPath::from(root) {
        UniversalPath::Local(_) => UniversalPath::Local(root.to_path_buf()),
        root => root,
    }
}

fn root_of(path: UniversalPath) -> PathBuf {
    match path {
        UniversalPath::Local(folder) => folder,
        path => PathBuf::from(path.to_string()),
    }
}

pub fn revision_dropdown(
    ui: &mut egui::Ui,
    id_salt: usize,
    root: Option<&Path>,
    previous: Option<&Path>,
) -> Option<PathBuf> {
    let Some(root) = root else {
        ui.add_enabled(false, egui::Button::new(SOURCE_CONTROL));
        return None;
    };
    let id = ui.make_persistent_id(("side_revision_menu", id_salt));
    let previous = previous.map(universal_root);
    zcommon_ui::revision::revision_dropdown(
        ui,
        id,
        PathKind::Folder,
        &universal_root(root),
        previous.as_ref(),
    )
    .map(root_of)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rev(name: &str) -> SideRev {
        SideRev::Git(Some(GitRev::Rev(name.to_owned())))
    }

    #[test]
    fn working_tree_to_a_branch_keeps_the_sub_path() {
        assert_eq!(
            with_revision(Path::new(r"D:\repo\src\gui"), rev("main")),
            PathBuf::from(r"D:\repo\src\gui@git:main")
        );
    }

    #[test]
    fn branch_to_a_branch_with_a_slash_keeps_the_sub_path() {
        assert_eq!(
            with_revision(Path::new(r"D:\repo\src@git:main"), rev("origin/dev")),
            PathBuf::from(r"D:\repo\src@git:origin/dev")
        );
    }

    #[test]
    fn a_revision_to_the_index_keeps_the_sub_path() {
        assert_eq!(
            with_revision(
                Path::new(r"D:\repo\src@git:v1.2"),
                SideRev::Git(Some(GitRev::Index))
            ),
            PathBuf::from(r"D:\repo\src@git:index")
        );
    }

    #[test]
    fn a_revision_to_the_working_tree_keeps_the_sub_path() {
        assert_eq!(
            with_revision(
                Path::new(r"D:\repo\src@git:origin/main"),
                SideRev::Git(None)
            ),
            PathBuf::from(r"D:\repo\src")
        );
    }

    #[test]
    fn a_side_splits_into_its_folder_and_revision() {
        assert_eq!(
            split_side(Path::new(r"D:\repo\src@git:origin/main")),
            (PathBuf::from(r"D:\repo\src"), rev("origin/main"))
        );
        assert_eq!(
            split_side(Path::new(r"D:\repo\src@git:index")),
            (
                PathBuf::from(r"D:\repo\src"),
                SideRev::Git(Some(GitRev::Index))
            )
        );
        assert_eq!(
            split_side(Path::new(r"D:\repo\src")),
            (PathBuf::from(r"D:\repo\src"), SideRev::Git(None))
        );
    }

    #[test]
    fn a_depot_root_splits_into_a_depot_side() {
        assert_eq!(
            split_side(Path::new("//depot/main/src")),
            (PathBuf::from("//depot/main/src"), SideRev::Depot(None))
        );
        assert_eq!(
            split_side(Path::new("//depot/main/src@1234")),
            (
                PathBuf::from("//depot/main/src"),
                SideRev::Depot(Some(DepotRev::Change(1234)))
            )
        );
        assert_eq!(
            split_side(Path::new("//depot/main/src@=77")),
            (
                PathBuf::from("//depot/main/src"),
                SideRev::Depot(Some(DepotRev::Shelf(77)))
            )
        );
    }

    #[test]
    fn a_root_round_trips_through_a_universal_path() {
        for root in [
            r"D:\repo\src",
            "D:/repo/./src/",
            r"D:\repo\src@git:origin/main",
            r"D:\repo\src@git:index",
            "//depot/main/src",
            "//depot/main/src@1234",
            "//depot/main/src@=77",
        ] {
            assert_eq!(root_of(universal_root(Path::new(root))).as_os_str(), root);
        }
    }
}
