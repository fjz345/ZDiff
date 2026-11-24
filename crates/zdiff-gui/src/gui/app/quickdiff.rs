use std::path::{Path, PathBuf};
use zdiff::universal_path::{DepotRev, UniversalPath};

use crate::{
    file::FileProcessor,
    keybindings::QuickDiffPaths,
    p4::{P4Command, P4Runner, list_shelf, local_path_of_depot_file},
};

/// Why a Quick Diff left both sides as they were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QuickDiffRefusal {
    BothTemp,
}

impl std::fmt::Display for QuickDiffRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            QuickDiffRefusal::BothTemp => {
                "both sides are temporary files, so neither has a workspace path to diff against"
            }
        })
    }
}

/// Points the sides at a Quick Diff slot's paths. A slot without a target path diffs the same
/// root-relative path under its target root. That path comes from the left side, or from the
/// right side when the left one is a temp copy, as when p4 launches an external diff. With both
/// sides temp, nothing changes.
pub(super) fn apply_quick_diff(
    slot: &QuickDiffPaths,
    file_1: &mut FileProcessor,
    file_2: &mut FileProcessor,
    temp_root: &Path,
) -> Result<(), QuickDiffRefusal> {
    let is_temp = |file: &mut FileProcessor| file.get_full_path().is_temp(temp_root);
    if is_temp(file_1) && is_temp(file_2) {
        return Err(QuickDiffRefusal::BothTemp);
    }
    if let Some((source_root, source_path)) = &slot.source {
        file_1.set_root(UniversalPath::from(source_root));
        file_1.set_path(UniversalPath::from(source_path));
    }

    // Don't set any target paths if root & path is ""
    if !(slot.target.0.is_empty() && slot.target.1.is_empty()) {
        let target_path = if slot.target.1.is_empty() {
            // The sides as they are now, so a slot source counts as the left side. Read before
            // file_2 gets the target root.
            let identity = if is_temp(file_1) {
                &mut *file_2
            } else {
                &mut *file_1
            };
            &identity.get_path().to_string()
        } else {
            &slot.target.1
        };

        file_2.set_root(UniversalPath::from(&slot.target.0));
        file_2.set_path(UniversalPath::from(target_path));
    }
    Ok(())
}

/// Why the local-file shortcut left the target as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LocalFileRefusal {
    LocalSource(PathBuf),
    /// p4 failed, or the depot file isn't mapped.
    Where(String),
    Missing(PathBuf),
}

impl std::fmt::Display for LocalFileRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocalFileRefusal::LocalSource(path) => write!(
                f,
                "the left file {} is a local file, not a depot path",
                path.display()
            ),
            LocalFileRefusal::Where(e) => write!(f, "p4 where: {e}"),
            LocalFileRefusal::Missing(path) => {
                write!(f, "the local file {} doesn't exist", path.display())
            }
        }
    }
}

/// Points the target at the workspace file that the source depot file maps to, and returns it.
/// The source stays. On a refusal nothing changes.
pub(super) fn diff_against_local_file(
    file_1: &mut FileProcessor,
    file_2: &mut FileProcessor,
    p4: &impl P4Runner,
) -> Result<PathBuf, LocalFileRefusal> {
    let source = file_1.get_full_path();
    if let UniversalPath::Local(path) = source {
        return Err(LocalFileRefusal::LocalSource(path));
    }
    let local = local_path_of_depot_file(&source, p4).map_err(LocalFileRefusal::Where)?;
    if !local.is_file() {
        return Err(LocalFileRefusal::Missing(local));
    }
    // The target's root belonged to its previous file; a depot root would be prefixed to this one.
    file_2.set_root("".into());
    file_2.set_path(UniversalPath::Local(local.clone()));
    Ok(local)
}

/// Why the shelf action left both sides as they were.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ShelfBaseRefusal {
    NoShelfSide,
    Listing(String),
    NotShelved(String),
    NoBase { depot_file: String, action: String },
}

impl std::fmt::Display for ShelfBaseRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShelfBaseRefusal::NoShelfSide => write!(f, "neither side is a shelved file (@=n)"),
            ShelfBaseRefusal::Listing(e) => f.write_str(e),
            ShelfBaseRefusal::NotShelved(file) => write!(f, "{file} is not in the shelf"),
            ShelfBaseRefusal::NoBase { depot_file, action } => {
                write!(
                    f,
                    "{depot_file} is shelved as {action}: it has no base revision"
                )
            }
        }
    }
}

pub(super) fn shelved_file(path: &UniversalPath) -> Option<(&str, u32)> {
    match path {
        UniversalPath::Depot(file, Some(DepotRev::Shelf(change))) => Some((file, *change)),
        _ => None,
    }
}

/// Points the side that is not at `@=n` at the base revision the shelf lists for the other side's
/// file, and returns it. A shelved source wins over a shelved target. On a refusal nothing changes.
pub(super) fn diff_against_shelf_base(
    file_1: &mut FileProcessor,
    file_2: &mut FileProcessor,
    p4: &impl P4Runner,
) -> Result<UniversalPath, ShelfBaseRefusal> {
    let (path_1, path_2) = (file_1.get_full_path(), file_2.get_full_path());
    let ((depot_file, change), other) = match (shelved_file(&path_1), shelved_file(&path_2)) {
        (Some(shelved), _) => (shelved, file_2),
        (None, Some(shelved)) => (shelved, file_1),
        (None, None) => return Err(ShelfBaseRefusal::NoShelfSide),
    };
    let shelf = list_shelf(p4, change).map_err(ShelfBaseRefusal::Listing)?;
    let entry = shelf
        .iter()
        .find(|entry| entry.depot_file == depot_file)
        .ok_or_else(|| ShelfBaseRefusal::NotShelved(depot_file.to_owned()))?;
    let base = entry.base_rev.ok_or_else(|| ShelfBaseRefusal::NoBase {
        depot_file: entry.depot_file.clone(),
        action: entry.action.clone(),
    })?;
    let base = UniversalPath::Depot(entry.depot_file.clone(), Some(DepotRev::Rev(base)));
    // The other side's root belonged to its previous file; it would be prefixed to this one.
    other.clear_root();
    other.set_path(base.clone());
    Ok(base)
}

pub(super) fn run_diff_against_shelf_base(file_1: &mut FileProcessor, file_2: &mut FileProcessor) {
    let source = file_1.get_full_path();
    let shelved = if shelved_file(&source).is_some() {
        source
    } else {
        file_2.get_full_path()
    };
    let p4 = P4Command::new(false).for_file(shelved);
    match diff_against_shelf_base(file_1, file_2, &p4) {
        Ok(base) => log::info!("Diffing against the shelf base {base}"),
        Err(refusal @ ShelfBaseRefusal::NoShelfSide) => {
            log::info!("Can not diff against the shelf base: {refusal}")
        }
        Err(refusal) => log::error!("Can not diff against the shelf base: {refusal}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod quick_diff {
        use super::*;
        use zcommon::test_util::os_path;

        fn temp() -> String {
            os_path(r"C:\Users\me\AppData\Local\Temp")
        }

        fn temp_copy() -> String {
            os_path(r"C:\Users\me\AppData\Local\Temp\p4v\main.rs#3")
        }

        fn workspace() -> String {
            os_path(r"C:\ws")
        }

        fn workspace_file() -> String {
            os_path(r"C:\ws\src\main.rs")
        }

        /// A slot that diffs the open file's root-relative path under a depot root.
        fn depot_slot() -> QuickDiffPaths {
            QuickDiffPaths {
                target: ("//depot/main".into(), String::new()),
                source: None,
                p4_profile: None,
            }
        }

        fn side(root: Option<&str>, path: &str) -> FileProcessor {
            let mut file = FileProcessor::new();
            if let Some(root) = root {
                file.set_root(UniversalPath::from(root));
            }
            file.set_path(UniversalPath::from(path));
            file
        }

        fn apply(
            slot: &QuickDiffPaths,
            file_1: &mut FileProcessor,
            file_2: &mut FileProcessor,
        ) -> Result<(), QuickDiffRefusal> {
            apply_quick_diff(slot, file_1, file_2, Path::new(&temp()))
        }

        #[test]
        fn neither_temp_takes_the_left_sides_path() {
            let mut left = side(Some(&workspace()), &workspace_file());
            let mut right = side(None, r"C:\other\lib.rs");
            assert_eq!(apply(&depot_slot(), &mut left, &mut right), Ok(()));
            assert_eq!(
                right.get_full_path(),
                UniversalPath::new("//depot/main/src/main.rs")
            );
            assert_eq!(left.get_full_path(), UniversalPath::new(workspace_file()));
        }

        #[test]
        fn a_temp_left_side_takes_the_right_sides_path() {
            let mut left = side(None, &temp_copy());
            let mut right = side(Some(&workspace()), &workspace_file());
            assert_eq!(apply(&depot_slot(), &mut left, &mut right), Ok(()));
            assert_eq!(
                right.get_full_path(),
                UniversalPath::new("//depot/main/src/main.rs")
            );
            assert_eq!(left.get_full_path(), UniversalPath::new(temp_copy()));
        }

        #[test]
        fn a_temp_right_side_takes_the_left_sides_path() {
            let mut left = side(Some(&workspace()), &workspace_file());
            let mut right = side(None, &temp_copy());
            assert_eq!(apply(&depot_slot(), &mut left, &mut right), Ok(()));
            assert_eq!(
                right.get_full_path(),
                UniversalPath::new("//depot/main/src/main.rs")
            );
            assert_eq!(left.get_full_path(), UniversalPath::new(workspace_file()));
        }

        #[test]
        fn both_temp_disables_every_slot_and_changes_nothing() {
            let other_copy = os_path(r"C:\Users\me\AppData\Local\Temp\p4v\main.rs#4");
            let slots = [
                depot_slot(),
                QuickDiffPaths {
                    target: ("//depot/main".into(), "src/main.rs".into()),
                    source: Some((workspace(), workspace_file())),
                    p4_profile: None,
                },
            ];
            for slot in slots {
                let mut left = side(None, &temp_copy());
                let mut right = side(None, &other_copy);
                assert_eq!(
                    apply(&slot, &mut left, &mut right),
                    Err(QuickDiffRefusal::BothTemp),
                    "{slot:?}"
                );
                assert_eq!(left.get_full_path(), UniversalPath::new(temp_copy()));
                assert_eq!(left.get_root(), None);
                assert_eq!(right.get_full_path(), UniversalPath::new(&other_copy));
                assert_eq!(right.get_root(), None);
            }
        }

        mod local_file {
            use super::*;
            use crate::p4::FakeP4;

            const DEPOT_FILE: &str = "//depot/main/src/main.rs#3";
            const TARGET: &str = r"C:\other\lib.rs";

            fn where_output(local: &Path) -> String {
                format!(
                    "... depotFile //depot/main/src/main.rs\n\
                     ... clientFile //ws/main/src/main.rs\n\
                     ... path {}\n\n",
                    local.display()
                )
            }

            fn assert_unchanged(left: &mut FileProcessor, right: &mut FileProcessor) {
                assert_eq!(left.get_full_path(), UniversalPath::new(DEPOT_FILE));
                assert_eq!(right.get_full_path(), UniversalPath::new(TARGET));
            }

            #[test]
            fn a_mapped_existing_file_becomes_the_target() {
                let dir = tempfile::tempdir().unwrap();
                let local = dir.path().join("my dir").join("main.rs");
                std::fs::create_dir_all(local.parent().unwrap()).unwrap();
                std::fs::write(&local, "fn main() {}").unwrap();
                let p4 = FakeP4::new(|_: &[&str]| Ok(where_output(&local)));

                let mut left = side(None, DEPOT_FILE);
                let mut right = side(None, TARGET);
                assert_eq!(
                    diff_against_local_file(&mut left, &mut right, &p4),
                    Ok(local.clone())
                );
                assert_eq!(left.get_full_path(), UniversalPath::new(DEPOT_FILE));
                assert_eq!(right.get_full_path(), UniversalPath::Local(local.clone()));
                assert_eq!(
                    p4.calls(),
                    vec![vec!["-ztag", "where", "//depot/main/src/main.rs"]]
                );
            }

            #[test]
            fn a_depot_root_on_the_target_is_dropped() {
                // A depot root (from a Quick Diff) would be prefixed to the local path.
                let dir = tempfile::tempdir().unwrap();
                let local = dir.path().join("main.rs");
                std::fs::write(&local, "fn main() {}").unwrap();
                let p4 = FakeP4::new(|_: &[&str]| Ok(where_output(&local)));

                let mut left = side(None, DEPOT_FILE);
                let mut right = side(Some("//depot/rel"), "//depot/rel/src/main.rs");
                assert!(diff_against_local_file(&mut left, &mut right, &p4).is_ok());
                assert_eq!(right.get_full_path(), UniversalPath::Local(local));
            }

            #[test]
            fn an_unmapped_file_changes_nothing() {
                // p4 exits 0 on "not in client view", with nothing on stdout.
                let p4 = FakeP4::new(|_: &[&str]| Ok(String::new()));
                let mut left = side(None, DEPOT_FILE);
                let mut right = side(None, TARGET);
                assert!(matches!(
                    diff_against_local_file(&mut left, &mut right, &p4),
                    Err(LocalFileRefusal::Where(_))
                ));
                assert_unchanged(&mut left, &mut right);

                let p4 = FakeP4::new(|_: &[&str]| Err("connect failed".to_string()));
                assert_eq!(
                    diff_against_local_file(&mut left, &mut right, &p4),
                    Err(LocalFileRefusal::Where("connect failed".into()))
                );
                assert_unchanged(&mut left, &mut right);
            }

            #[test]
            fn a_mapped_file_missing_on_disk_changes_nothing() {
                let dir = tempfile::tempdir().unwrap();
                let local = dir.path().join("main.rs");
                let p4 = FakeP4::new(|_: &[&str]| Ok(where_output(&local)));
                let mut left = side(None, DEPOT_FILE);
                let mut right = side(None, TARGET);
                assert_eq!(
                    diff_against_local_file(&mut left, &mut right, &p4),
                    Err(LocalFileRefusal::Missing(local.clone()))
                );
                assert_unchanged(&mut left, &mut right);

                // A directory isn't a file to diff.
                std::fs::create_dir(&local).unwrap();
                assert_eq!(
                    diff_against_local_file(&mut left, &mut right, &p4),
                    Err(LocalFileRefusal::Missing(local))
                );
                assert_unchanged(&mut left, &mut right);
            }

            #[test]
            fn a_local_source_changes_nothing_and_runs_no_p4() {
                let p4 = FakeP4::new(|_: &[&str]| panic!("p4 must not run"));
                let mut left = side(Some(&workspace()), &workspace_file());
                let mut right = side(None, TARGET);
                assert_eq!(
                    diff_against_local_file(&mut left, &mut right, &p4),
                    Err(LocalFileRefusal::LocalSource(workspace_file().into()))
                );
                assert_eq!(left.get_full_path(), UniversalPath::new(workspace_file()));
                assert_eq!(right.get_full_path(), UniversalPath::new(TARGET));
            }
        }

        mod shelf_base {
            use super::*;
            use crate::p4::FakeP4;

            const SHELVED: &str = "//depot/main/src/a b.rs@=1234";
            const BASE: &str = "//depot/main/src/a b.rs#4";
            const TARGET: &str = r"C:\other\lib.rs";

            fn describe(change: u32) -> String {
                format!(
                    "... change {change}\n\
                     ... desc Fix the thing.\n\
                     ... depotFile0 //depot/main/src/a b.rs\n\
                     ... action0 edit\n\
                     ... rev0 4\n\
                     ... depotFile1 //depot/main/src/new.rs\n\
                     ... action1 add\n\
                     ... rev1 1\n\n"
                )
            }

            #[test]
            fn the_target_becomes_the_base_revision_of_the_shelved_source() {
                let p4 = FakeP4::new(|_: &[&str]| Ok(describe(1234)));
                let mut left = side(None, SHELVED);
                let mut right = side(None, TARGET);
                assert_eq!(
                    diff_against_shelf_base(&mut left, &mut right, &p4),
                    Ok(UniversalPath::new(BASE))
                );
                assert_eq!(left.get_full_path(), UniversalPath::new(SHELVED));
                assert_eq!(right.get_full_path(), UniversalPath::new(BASE));
                assert_eq!(
                    p4.calls(),
                    vec![vec!["-ztag", "describe", "-S", "-s", "1234"]]
                );
            }

            #[test]
            fn a_shelved_target_sets_the_source() {
                let p4 = FakeP4::new(|_: &[&str]| Ok(describe(1234)));
                let mut left = side(None, TARGET);
                let mut right = side(None, SHELVED);
                assert!(diff_against_shelf_base(&mut left, &mut right, &p4).is_ok());
                assert_eq!(left.get_full_path(), UniversalPath::new(BASE));
                assert_eq!(right.get_full_path(), UniversalPath::new(SHELVED));
            }

            #[test]
            fn a_depot_root_on_the_other_side_is_dropped() {
                let p4 = FakeP4::new(|_: &[&str]| Ok(describe(1234)));
                let mut left = side(None, SHELVED);
                let mut right = side(Some("//depot/rel"), "//depot/rel/lib.rs");
                assert!(diff_against_shelf_base(&mut left, &mut right, &p4).is_ok());
                assert_eq!(right.get_full_path(), UniversalPath::new(BASE));
                assert_eq!(right.get_root(), None);
            }

            #[test]
            fn a_file_added_by_the_shelf_has_no_base() {
                let p4 = FakeP4::new(|_: &[&str]| Ok(describe(1234)));
                let mut left = side(None, "//depot/main/src/new.rs@=1234");
                let mut right = side(None, TARGET);
                assert_eq!(
                    diff_against_shelf_base(&mut left, &mut right, &p4),
                    Err(ShelfBaseRefusal::NoBase {
                        depot_file: "//depot/main/src/new.rs".into(),
                        action: "add".into()
                    })
                );
                assert_eq!(right.get_full_path(), UniversalPath::new(TARGET));
            }

            #[test]
            fn a_file_missing_from_the_shelf_changes_nothing() {
                let p4 = FakeP4::new(|_: &[&str]| Ok(describe(1234)));
                let mut left = side(None, "//depot/main/src/other.rs@=1234");
                let mut right = side(None, TARGET);
                assert_eq!(
                    diff_against_shelf_base(&mut left, &mut right, &p4),
                    Err(ShelfBaseRefusal::NotShelved(
                        "//depot/main/src/other.rs".into()
                    ))
                );
                assert_eq!(right.get_full_path(), UniversalPath::new(TARGET));
            }

            #[test]
            fn a_failing_p4_changes_nothing() {
                let p4 = FakeP4::new(|_: &[&str]| Err("connect failed".to_string()));
                let mut left = side(None, SHELVED);
                let mut right = side(None, TARGET);
                let refusal = diff_against_shelf_base(&mut left, &mut right, &p4).unwrap_err();
                assert!(
                    matches!(&refusal, ShelfBaseRefusal::Listing(e) if e.contains("connect failed")),
                    "{refusal:?}"
                );
                assert_eq!(left.get_full_path(), UniversalPath::new(SHELVED));
                assert_eq!(right.get_full_path(), UniversalPath::new(TARGET));
            }

            #[test]
            fn sides_without_a_shelf_change_nothing_and_run_no_p4() {
                let p4 = FakeP4::new(|_: &[&str]| panic!("p4 must not run"));
                for source in ["//depot/main/src/a b.rs#3", "//depot/main/src/a b.rs@1234"] {
                    let mut left = side(None, source);
                    let mut right = side(None, TARGET);
                    assert_eq!(
                        diff_against_shelf_base(&mut left, &mut right, &p4),
                        Err(ShelfBaseRefusal::NoShelfSide)
                    );
                    assert_eq!(left.get_full_path(), UniversalPath::new(source));
                    assert_eq!(right.get_full_path(), UniversalPath::new(TARGET));
                }
            }
        }

        #[test]
        fn a_slot_source_is_applied_before_the_path_is_taken() {
            // The source replaces the temp left side, so it supplies the path.
            let slot = QuickDiffPaths {
                source: Some((workspace(), os_path(r"C:\ws\src\lib.rs"))),
                ..depot_slot()
            };
            let mut left = side(None, &temp_copy());
            let mut right = side(None, r"C:\other\main.rs");
            assert_eq!(apply(&slot, &mut left, &mut right), Ok(()));
            assert_eq!(
                left.get_full_path(),
                UniversalPath::new(os_path(r"C:\ws\src\lib.rs"))
            );
            assert_eq!(
                right.get_full_path(),
                UniversalPath::new("//depot/main/src/lib.rs")
            );
        }
    }
}
