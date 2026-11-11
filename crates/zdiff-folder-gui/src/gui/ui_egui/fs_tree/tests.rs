use crate::ui_egui::fs_tree::apply_path_bar;
use crate::ui_egui::fs_tree::{
    ContentWidths, CursorRowsCache, DiffState, EXPANDER_WIDTH, FileSystemView, RowLayout, RowsId,
    TREE_INDENT, TreeTable, VisibleRowTwoFolderDiff, aggregate_folder_state,
    apply_blueprint_results, apply_cursor_request, cursor_keys, side_width, single_side_rows,
    tree_hidden, two_folder_cursor_rows,
};
use crate::ui_egui::roots::{InvalidRoot, Side};
use crate::ui_egui::status_line::ToolMessage;
use crate::ui_egui::tree_compare::pending_pairs;
use crate::ui_egui::tree_cursor::{CursorRequest, TreeCursor};
use crate::ui_egui::tree_filter::{DiffFilter, StateFilter, arrange_two_folder_rows};
use crate::ui_egui::tree_guides::tree_guides;
use crate::ui_egui::tree_likeness::FolderLikeness;
use crate::ui_egui::tree_sort::{SortKey, TreeSort, sort_two_folder_rows};
use crate::ui_egui::tree_tolerance::{SizeDelta, Tolerance};
use eframe::egui;
use std::time::{Duration, SystemTime};
use zcommon::path::split_path;
use zcommon_ui::pathbar::PathBarAction;
use zdiff_folder::external_diff_tool::{DiffTool, DiffToolConfig};

use std::cell::Cell;
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;
use tempfile::{TempDir, tempdir};
use zcommon::kinds::{FileKind, KindTable};
use zdiff_folder::comparison::PairState;
use zdiff_folder::filter::{PathFilter, PatternList};
use zdiff_folder::fs::{FileSystemModel, FsIsDir, FsNodeDepth, FsNodeId};

#[test]
fn folder_role_names_are_unique_against_the_shared_roles() {
    use crate::ui_egui::fs_tree::FolderRole;
    use zcommon::style::{Role, SharedRole};
    let mut names: Vec<&str> = SharedRole::all().iter().map(|r| r.name()).collect();
    names.extend(FolderRole::all().iter().map(|r| r.name()));
    let count = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), count);
}

#[test]
fn every_file_kind_has_a_tag_role() {
    use crate::ui_egui::fs_tree::FolderRole;
    use zcommon::style::Role;
    for kind in FileKind::ALL {
        assert!(
            FolderRole::all().contains(&FolderRole::Tag(kind)),
            "{kind:?}"
        );
    }
}

struct CollapsedTestCase {
    name: &'static str,
    structure: Vec<(&'static str, FsIsDir)>,
    collapsed: Vec<&'static str>,
    expected: Vec<(&'static str, FsNodeDepth)>,
}

fn find_id_by_rel_path(fs: &FileSystemModel, root: &Path, rel: &str) -> FsNodeId {
    let target = root.join(rel);
    for (id, _, _) in fs.iter_tree() {
        if fs.path(id) == target {
            return id;
        }
    }
    panic!("Test setup error: path {:?} not found in model", target);
}

#[allow(dead_code)]
fn get_rel(file_system: &FileSystemModel, root: &Path, id: FsNodeId) -> String {
    if id == file_system.get_root_node_id() {
        return String::new();
    }

    let path = file_system.path(id);
    path.strip_prefix(root)
        .unwrap_or(&path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
        .trim_start_matches('/')
        .to_string()
}

#[allow(dead_code)]
fn get_rel_from_diff_state(
    file_system_1: &FileSystemModel,
    file_system_2: &FileSystemModel,
    r1: &Path,
    r2: &Path,
    state: &DiffState,
) -> String {
    match state {
        DiffState::OnlyInFirst(id) => get_rel(file_system_1, r1, *id),
        DiffState::OnlyInSecond(id) => get_rel(file_system_2, r2, *id),
        DiffState::Same(id, _)
        | DiffState::Different(id, _)
        | DiffState::Similar(id, _)
        | DiffState::Pending(id, _) => get_rel(file_system_1, r1, *id),
    }
}

fn format_tree(rows: &[(String, FsNodeDepth)]) -> String {
    rows.iter()
        .map(|(path, depth)| {
            let indent = "  ".repeat(*depth as usize);
            let name = if path.is_empty() { "/" } else { path };
            format!("{}└─ {}", indent, name)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[allow(dead_code)]
fn format_diff_tree_two(
    left: &[(String, FsNodeDepth)],
    right: &[(String, FsNodeDepth)],
    diff: &[(String, FsNodeDepth, String)],
) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "{:<30} | {:<30} | {:<30}\n",
        "LEFT TREE", "DIFF RESULT", "RIGHT TREE"
    ));
    output.push_str(&"-".repeat(96));
    output.push('\n');

    let l_map: HashMap<&str, FsNodeDepth> = left.iter().map(|(p, d)| (p.as_str(), *d)).collect();
    let r_map: HashMap<&str, FsNodeDepth> = right.iter().map(|(p, d)| (p.as_str(), *d)).collect();

    for (path, depth, marker) in diff {
        let l_row = l_map
            .get(path.as_str())
            .map(|d| {
                format!(
                    "{}└─ {}",
                    "  ".repeat(*d as usize),
                    if path.is_empty() { "/" } else { path }
                )
            })
            .unwrap_or_default();

        let r_row = r_map
            .get(path.as_str())
            .map(|d| {
                format!(
                    "{}└─ {}",
                    "  ".repeat(*d as usize),
                    if path.is_empty() { "/" } else { path }
                )
            })
            .unwrap_or_default();

        let d_row = format!(
            "{} {}└─ {}",
            marker,
            "  ".repeat(*depth as usize),
            if path.is_empty() { "/" } else { path }
        );

        output.push_str(&format!("{:<30} | {:<30} | {:<30}\n", l_row, d_row, r_row));
    }
    output
}

fn write_files(root: &Path, files: &[(&str, &str)]) {
    for (rel_path, content) in files {
        let path = root.join(rel_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

fn load_view(root: &Path) -> FileSystemView {
    FileSystemView::new(Arc::new(FileSystemModel::new(root).unwrap()))
}

/// b.txt differs, a.txt and everything under sub/ is equal, and each side has a file
/// of its own.
fn two_folder_trees() -> (TempDir, TempDir) {
    let left = tempdir().unwrap();
    let right = tempdir().unwrap();
    let shared = [
        ("a.txt", "same a"),
        ("sub/c.txt", "same c"),
        ("sub/deep/d.txt", "same d"),
    ];
    write_files(left.path(), &shared);
    write_files(right.path(), &shared);
    write_files(left.path(), &[("b.txt", "left b"), ("left_only.txt", "l")]);
    write_files(
        right.path(),
        &[("b.txt", "right b"), ("right_only.txt", "r")],
    );
    (left, right)
}

fn state_kind(state: &DiffState) -> &'static str {
    match state {
        DiffState::Different(..) => "Different",
        DiffState::Same(..) => "Same",
        DiffState::Similar(..) => "Similar",
        DiffState::OnlyInFirst(..) => "OnlyInFirst",
        DiffState::OnlyInSecond(..) => "OnlyInSecond",
        DiffState::Pending(..) => "Pending",
    }
}

#[test]
fn two_folder_rows_have_the_expected_paths_states_and_order() {
    let (left_dir, right_dir) = two_folder_trees();
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());

    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));

    let actual: Vec<(String, &str, FsNodeDepth, bool)> = rows
        .iter()
        .map(|row| {
            let rel = get_rel_from_diff_state(
                &left.file_system,
                &right.file_system,
                left_dir.path(),
                right_dir.path(),
                &row.diff_state,
            );
            (rel, state_kind(&row.diff_state), row.depth, row.is_dir)
        })
        .collect();
    let expected = [
        ("", "Different", 0, true),
        ("a.txt", "Same", 1, false),
        ("b.txt", "Different", 1, false),
        ("left_only.txt", "OnlyInFirst", 1, false),
        ("right_only.txt", "OnlyInSecond", 1, false),
        ("sub", "Same", 1, true),
        ("sub/c.txt", "Same", 2, false),
        ("sub/deep", "Same", 2, true),
        ("sub/deep/d.txt", "Same", 3, false),
    ]
    .map(|(rel, kind, depth, is_dir)| (rel.to_string(), kind, depth, is_dir));
    assert_eq!(actual, expected);
}

fn build_counting(
    left: &FileSystemView,
    right: &FileSystemView,
    states: &mut PairStates,
    comparisons: &Cell<usize>,
) -> Vec<VisibleRowTwoFolderDiff> {
    build_filtered(left, right, states, comparisons, &PathFilter::default())
}

type PairStates = HashMap<String, PairState>;

fn build_from_states(
    left: &FileSystemView,
    right: &FileSystemView,
    states: &PairStates,
    filter: &PathFilter,
) -> Vec<VisibleRowTwoFolderDiff> {
    build_tolerant_from_states(left, right, states, filter, &Tolerance::default())
}

fn build_tolerant_from_states(
    left: &FileSystemView,
    right: &FileSystemView,
    states: &PairStates,
    filter: &PathFilter,
    tolerance: &Tolerance,
) -> Vec<VisibleRowTwoFolderDiff> {
    FileSystemView::build_two_folder_diff_rows(
        Some(left),
        Some(right),
        filter,
        &KindTable::default(),
        tolerance,
        |rel_path, _| (states.get(rel_path).copied(), None),
        |_, _| None,
    )
    .unwrap()
}

fn build_filtered(
    left: &FileSystemView,
    right: &FileSystemView,
    states: &mut PairStates,
    comparisons: &Cell<usize>,
    filter: &PathFilter,
) -> Vec<VisibleRowTwoFolderDiff> {
    build_compared(
        left,
        right,
        states,
        comparisons,
        filter,
        &Tolerance::default(),
    )
}

fn build_compared(
    left: &FileSystemView,
    right: &FileSystemView,
    states: &mut PairStates,
    comparisons: &Cell<usize>,
    filter: &PathFilter,
    tolerance: &Tolerance,
) -> Vec<VisibleRowTwoFolderDiff> {
    let rows = build_tolerant_from_states(left, right, states, filter, tolerance);
    let pending = pending_pairs(&rows, [Some(left), Some(right)]);
    if pending.is_empty() {
        return rows;
    }
    for pair in pending {
        comparisons.set(comparisons.get() + 1);
        states.insert(pair.rel_path.clone(), pair.compare().state);
    }
    build_tolerant_from_states(left, right, states, filter, tolerance)
}

fn set_modified(root: &Path, rel_path: &str, modified: SystemTime) {
    File::options()
        .write(true)
        .open(root.join(rel_path))
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

#[test]
fn the_time_criterion_makes_a_pair_similar_without_comparing_it() {
    let (left_dir, right_dir) = (tempdir().unwrap(), tempdir().unwrap());
    write_files(left_dir.path(), &[("x.txt", "aaaa"), ("y.txt", "aaaa")]);
    write_files(right_dir.path(), &[("x.txt", "bbbb"), ("y.txt", "bbbb")]);
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    set_modified(left_dir.path(), "x.txt", time);
    set_modified(right_dir.path(), "x.txt", time);
    set_modified(left_dir.path(), "y.txt", time);
    set_modified(right_dir.path(), "y.txt", time + Duration::from_secs(1));
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let tolerance = Tolerance {
        enabled: true,
        size: false,
        same_size_and_time: true,
        ..Tolerance::default()
    };
    let comparisons = Cell::new(0);

    let rows = build_compared(
        &left,
        &right,
        &mut PairStates::new(),
        &comparisons,
        &PathFilter::default(),
        &tolerance,
    );

    assert_eq!(
        rows_with_kinds(&rows),
        [
            ("", "Different"),
            ("x.txt", "Similar"),
            ("y.txt", "Different")
        ]
    );
    assert_eq!(comparisons.get(), 1, "only y.txt is compared");
}

#[test]
fn turning_tolerance_off_restores_the_exact_states_without_comparing_again() {
    let (left_dir, right_dir) = two_folder_trees();
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let mut states = PairStates::new();
    let comparisons = Cell::new(0);
    let tolerance = Tolerance {
        enabled: true,
        max_size_delta: SizeDelta::Bytes(1),
        ..Tolerance::default()
    };
    let filter = PathFilter::default();

    let rows = build_compared(
        &left,
        &right,
        &mut states,
        &comparisons,
        &filter,
        &tolerance,
    );
    assert_eq!(kind_at(&rows, "b.txt"), "Similar");
    assert_eq!(comparisons.get(), 4);

    let off = Tolerance {
        enabled: false,
        ..tolerance
    };
    let rows = build_compared(&left, &right, &mut states, &comparisons, &filter, &off);
    assert_eq!(kind_at(&rows, "b.txt"), "Different");
    assert_eq!(comparisons.get(), 4);
}

#[test]
fn a_folder_of_same_and_similar_files_is_similar_and_hidden_by_diff_only() {
    let (left_dir, right_dir) = (tempdir().unwrap(), tempdir().unwrap());
    write_files(
        left_dir.path(),
        &[("sub/a.txt", "same"), ("sub/b.txt", "left")],
    );
    write_files(
        right_dir.path(),
        &[("sub/a.txt", "same"), ("sub/b.txt", "right")],
    );
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let tolerance = Tolerance {
        enabled: true,
        max_size_delta: SizeDelta::Bytes(1),
        ..Tolerance::default()
    };

    let rows = build_compared(
        &left,
        &right,
        &mut PairStates::new(),
        &Cell::new(0),
        &PathFilter::default(),
        &tolerance,
    );
    assert_eq!(
        rows_with_kinds(&rows),
        [
            ("", "Similar"),
            ("sub", "Similar"),
            ("sub/a.txt", "Same"),
            ("sub/b.txt", "Similar"),
        ]
    );
    let diff_only = DiffFilter {
        states: StateFilter::Diff,
        show_parents: true,
    };
    let shown = arrange_two_folder_rows(rows, diff_only, TreeSort::default());
    assert_eq!(rows_with_kinds(&shown), [("", "Similar")]);
}

fn build_with_likeness(
    left: &FileSystemView,
    right: &FileSystemView,
    pairs: &[(&str, PairState, Option<f64>)],
    tolerance: &Tolerance,
) -> Vec<VisibleRowTwoFolderDiff> {
    FileSystemView::build_two_folder_diff_rows(
        Some(left),
        Some(right),
        &PathFilter::default(),
        &KindTable::default(),
        tolerance,
        |rel_path, _| {
            pairs
                .iter()
                .find(|(path, ..)| *path == rel_path)
                .map_or((None, None), |(_, state, likeness)| {
                    (Some(*state), *likeness)
                })
        },
        |_, _| None,
    )
    .unwrap()
}

fn folder_likeness_at(rows: &[VisibleRowTwoFolderDiff], rel_path: &str) -> Option<FolderLikeness> {
    rows.iter()
        .find(|r| r.rel_path == rel_path)
        .unwrap()
        .folder_likeness
}

#[test]
fn a_folder_likeness_is_the_size_weighted_mean_of_the_files_below_it() {
    let (left_dir, right_dir) = (tempdir().unwrap(), tempdir().unwrap());
    let same = "s".repeat(10);
    let top = "t".repeat(50);
    write_files(
        left_dir.path(),
        &[
            ("sub/same.txt", &same),
            ("sub/diff.txt", &"l".repeat(30)),
            ("sub/gone.txt", &"g".repeat(10)),
            ("top.txt", &top),
            ("left_only/x.txt", "x"),
        ],
    );
    write_files(
        right_dir.path(),
        &[
            ("sub/same.txt", &same),
            ("sub/diff.txt", &"r".repeat(20)),
            ("top.txt", &top),
        ],
    );
    let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));

    let rows = build_with_likeness(
        &left,
        &right,
        &[
            ("sub/same.txt", PairState::Same, None),
            ("sub/diff.txt", PairState::Different, Some(0.5)),
            ("top.txt", PairState::Same, None),
        ],
        &Tolerance::default(),
    );

    assert_eq!(
        folder_likeness_at(&rows, "sub"),
        Some(FolderLikeness::Known(0.5))
    );
    assert_eq!(
        folder_likeness_at(&rows, ""),
        Some(FolderLikeness::Known(75.0 / 101.0))
    );
    assert_eq!(folder_likeness_at(&rows, "sub/diff.txt"), None);
    assert_eq!(folder_likeness_at(&rows, "left_only"), None);
}

#[test]
fn a_folder_with_an_unmeasured_pair_has_an_unknown_likeness() {
    let (left_dir, right_dir) = (tempdir().unwrap(), tempdir().unwrap());
    write_files(
        left_dir.path(),
        &[("sub/a.txt", "same"), ("sub/b.txt", "left")],
    );
    write_files(
        right_dir.path(),
        &[("sub/a.txt", "same"), ("sub/b.txt", "right")],
    );
    let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));

    for b_state in [None, Some(PairState::Different)] {
        let pairs: Vec<_> = [("sub/a.txt", Some(PairState::Same)), ("sub/b.txt", b_state)]
            .into_iter()
            .filter_map(|(path, state)| Some((path, state?, None)))
            .collect();
        let rows = build_with_likeness(&left, &right, &pairs, &Tolerance::default());
        assert_eq!(
            folder_likeness_at(&rows, "sub"),
            Some(FolderLikeness::Unknown)
        );
    }
}

#[test]
fn a_pair_similar_before_its_compare_is_left_out_of_the_folder_likeness() {
    let (left_dir, right_dir) = (tempdir().unwrap(), tempdir().unwrap());
    write_files(
        left_dir.path(),
        &[("sub/a.txt", "same"), ("sub/b.txt", "left")],
    );
    write_files(
        right_dir.path(),
        &[("sub/a.txt", "same"), ("sub/b.txt", "rite")],
    );
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for root in [left_dir.path(), right_dir.path()] {
        set_modified(root, "sub/b.txt", modified);
    }
    let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
    let tolerance = Tolerance {
        enabled: true,
        size: false,
        same_size_and_time: true,
        ..Tolerance::default()
    };

    let rows = build_with_likeness(
        &left,
        &right,
        &[("sub/a.txt", PairState::Same, None)],
        &tolerance,
    );

    assert_eq!(kind_at(&rows, "sub/b.txt"), "Similar");
    assert_eq!(
        folder_likeness_at(&rows, "sub"),
        Some(FolderLikeness::Known(1.0))
    );
}

#[test]
fn a_folder_of_empty_files_averages_the_files() {
    let (left_dir, right_dir) = (tempdir().unwrap(), tempdir().unwrap());
    write_files(left_dir.path(), &[("sub/a.txt", ""), ("sub/gone.txt", "")]);
    write_files(right_dir.path(), &[("sub/a.txt", "")]);
    let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));

    let rows = build_with_likeness(
        &left,
        &right,
        &[("sub/a.txt", PairState::Same, None)],
        &Tolerance::default(),
    );

    assert_eq!(
        folder_likeness_at(&rows, "sub"),
        Some(FolderLikeness::Known(0.5))
    );
}

#[test]
fn rows_built_before_any_result_are_pending_and_folders_aggregate_them() {
    let (left_dir, right_dir) = two_folder_trees();
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());

    let rows = build_from_states(&left, &right, &PairStates::new(), &PathFilter::default());

    assert_eq!(
        rows_with_kinds(&rows),
        [
            ("", "Different"),
            ("a.txt", "Pending"),
            ("b.txt", "Pending"),
            ("left_only.txt", "OnlyInFirst"),
            ("right_only.txt", "OnlyInSecond"),
            ("sub", "Pending"),
            ("sub/c.txt", "Pending"),
            ("sub/deep", "Pending"),
            ("sub/deep/d.txt", "Pending"),
        ]
    );
    let pending: Vec<String> = pending_pairs(&rows, [Some(&left), Some(&right)])
        .into_iter()
        .map(|pair| pair.rel_path)
        .collect();
    assert_eq!(pending, ["a.txt", "b.txt", "sub/c.txt", "sub/deep/d.txt"]);
}

#[test]
fn a_file_facing_a_folder_is_different_without_a_compare() {
    let (left_dir, right_dir) = (tempdir().unwrap(), tempdir().unwrap());
    write_files(left_dir.path(), &[("x", "")]);
    write_files(right_dir.path(), &[("x/inner.txt", "")]);
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());

    let rows = build_from_states(&left, &right, &PairStates::new(), &PathFilter::default());

    assert_eq!(kind_at(&rows, "x"), "Different");
    assert!(pending_pairs(&rows, [Some(&left), Some(&right)]).is_empty());
}

fn blacklist(text: &str) -> PathFilter {
    PathFilter {
        blacklist: PatternList::new(text),
        ..Default::default()
    }
}

fn rows_with_kinds(rows: &[VisibleRowTwoFolderDiff]) -> Vec<(&str, &'static str)> {
    rows.iter()
        .map(|r| (r.rel_path.as_str(), state_kind(&r.diff_state)))
        .collect()
}

#[test]
fn blacklisted_entries_are_absent_on_both_sides() {
    let (left_dir, right_dir) = two_folder_trees();
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());

    let rows = build_filtered(
        &left,
        &right,
        &mut PairStates::default(),
        &Cell::new(0),
        &blacklist("B.TXT, *_only.txt"),
    );

    // b.txt is on both sides, left_only.txt and right_only.txt on one. With every
    // difference hidden, the root reads as Same.
    assert_eq!(
        rows_with_kinds(&rows),
        [
            ("", "Same"),
            ("a.txt", "Same"),
            ("sub", "Same"),
            ("sub/c.txt", "Same"),
            ("sub/deep", "Same"),
            ("sub/deep/d.txt", "Same"),
        ]
    );
}

#[test]
fn a_blacklisted_folder_hides_its_whole_subtree() {
    let (left_dir, right_dir) = two_folder_trees();
    write_files(left_dir.path(), &[("sub/deep/more/e.txt", "l")]);
    // A file of the same name: the trailing slash spares it.
    write_files(right_dir.path(), &[("deep", "a file")]);
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());

    let rows = build_filtered(
        &left,
        &right,
        &mut PairStates::default(),
        &Cell::new(0),
        &blacklist("deep/"),
    );

    let paths: Vec<&str> = rows.iter().map(|r| r.rel_path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "",
            "a.txt",
            "b.txt",
            "deep",
            "left_only.txt",
            "right_only.txt",
            "sub",
            "sub/c.txt",
        ]
    );
}

#[test]
fn a_folder_whose_only_difference_is_blacklisted_reads_as_same() {
    let (left_dir, right_dir) = two_folder_trees();
    write_files(left_dir.path(), &[("sub/build.log", "left")]);
    write_files(right_dir.path(), &[("sub/build.log", "right")]);
    write_files(left_dir.path(), &[("sub/deep/left.obj", "only left")]);
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let mut states = PairStates::default();
    let comparisons = Cell::new(0);

    let rows = build_counting(&left, &right, &mut states, &comparisons);
    assert_eq!(kind_at(&rows, "sub"), "Different");
    assert_eq!(kind_at(&rows, "sub/deep"), "Different");

    let rows = build_filtered(
        &left,
        &right,
        &mut states,
        &comparisons,
        &blacklist("*.log\n*.obj"),
    );
    assert_eq!(kind_at(&rows, "sub"), "Same");
    assert_eq!(kind_at(&rows, "sub/deep"), "Same");
    assert_eq!(kind_at(&rows, ""), "Different", "b.txt still differs");
}

#[test]
fn the_root_is_never_filtered() {
    let (left_dir, right_dir) = two_folder_trees();
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());

    // Matches the roots' own names too (tempdirs are named .tmpXXXX).
    let rows = build_filtered(
        &left,
        &right,
        &mut PairStates::default(),
        &Cell::new(0),
        &blacklist("*, *.tmp*/"),
    );

    assert_eq!(rows_with_kinds(&rows), [("", "Same")]);
}

#[test]
fn changing_the_filter_rebuilds_without_comparing_files_again() {
    let (left_dir, right_dir) = two_folder_trees();
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let comparisons = Cell::new(0);
    let mut states = PairStates::default();

    let unfiltered = build_counting(&left, &right, &mut states, &comparisons);
    assert_eq!(comparisons.get(), 4);

    let filtered = build_filtered(&left, &right, &mut states, &comparisons, &blacklist("sub"));
    assert!(filtered.iter().all(|r| !r.rel_path.starts_with("sub")));
    let cleared = build_counting(&left, &right, &mut states, &comparisons);
    assert_eq!(comparisons.get(), 4);
    assert_eq!(cleared, unfiltered);
}

fn whitelist(white: &str, black: &str) -> PathFilter {
    PathFilter {
        blacklist: PatternList::new(black),
        whitelist: PatternList::new(white),
    }
}

#[test]
fn ancestors_of_a_whitelisted_file_are_visible_on_both_sides() {
    let (left_dir, right_dir) = two_folder_trees();
    for dir in [&left_dir, &right_dir] {
        write_files(dir.path(), &[("src/deep/main.rs", "same")]);
    }
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());

    let rows = build_filtered(
        &left,
        &right,
        &mut PairStates::default(),
        &Cell::new(0),
        &whitelist("*.rs", ""),
    );

    // Same means the entry is on both sides.
    assert_eq!(
        rows_with_kinds(&rows),
        [
            ("", "Same"),
            ("src", "Same"),
            ("src/deep", "Same"),
            ("src/deep/main.rs", "Same"),
        ]
    );
}

#[test]
fn a_folder_with_no_whitelisted_descendant_is_hidden() {
    let (left_dir, right_dir) = two_folder_trees();
    for dir in [&left_dir, &right_dir] {
        write_files(
            dir.path(),
            &[
                ("keep/a.rs", "same"),
                // Named like a match, but the whitelist applies to files only.
                ("x.rs/readme.md", "same"),
                // Blacklist beats whitelist, and the folder has nothing else.
                ("gen/b.rs", "same"),
            ],
        );
        fs::create_dir_all(dir.path().join("empty/nested")).unwrap();
    }
    // A whitelisted file on one side keeps its folder on both: the right one/ exists.
    write_files(left_dir.path(), &[("one/c.rs", "l")]);
    write_files(right_dir.path(), &[("one/c.md", "r")]);
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());

    let rows = build_filtered(
        &left,
        &right,
        &mut PairStates::default(),
        &Cell::new(0),
        &whitelist("*.rs", "gen/"),
    );

    assert_eq!(
        rows_with_kinds(&rows),
        [
            ("", "Different"),
            ("keep", "Same"),
            ("keep/a.rs", "Same"),
            ("one", "Different"),
            ("one/c.rs", "OnlyInFirst"),
        ]
    );
}

#[test]
fn with_a_whitelist_folder_states_reflect_only_the_visible_files() {
    let (left_dir, right_dir) = two_folder_trees();
    for dir in [&left_dir, &right_dir] {
        write_files(
            dir.path(),
            &[("src/main.rs", "same"), ("lib/deep/x.rs", "")],
        );
    }
    write_files(left_dir.path(), &[("src/notes.md", "left")]);
    write_files(right_dir.path(), &[("src/notes.md", "right")]);
    write_files(left_dir.path(), &[("lib/deep/y.rs", "left")]);
    write_files(right_dir.path(), &[("lib/deep/y.rs", "right")]);
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let mut states = PairStates::default();
    let comparisons = Cell::new(0);

    let rows = build_counting(&left, &right, &mut states, &comparisons);
    assert_eq!(kind_at(&rows, "src"), "Different");

    let rows = build_filtered(
        &left,
        &right,
        &mut states,
        &comparisons,
        &whitelist("*.rs", ""),
    );
    assert_eq!(kind_at(&rows, "src"), "Same", "notes.md is hidden");
    assert_eq!(kind_at(&rows, "lib"), "Different");
    assert_eq!(kind_at(&rows, "lib/deep"), "Different");
    assert_eq!(kind_at(&rows, ""), "Different", "y.rs still differs");
}

fn kind_at(rows: &[VisibleRowTwoFolderDiff], rel_path: &str) -> &'static str {
    let row = rows.iter().find(|r| r.rel_path == rel_path).unwrap();
    state_kind(&row.diff_state)
}

fn single_rows(view: &FileSystemView, filter: &PathFilter) -> Vec<VisibleRowTwoFolderDiff> {
    let rows = single_side_rows(view, filter, TreeSort::default()).unwrap();
    for row in &rows {
        assert!(
            matches!(row.diff_state, DiffState::OnlyInFirst(_)),
            "{}",
            row.rel_path
        );
    }
    rows
}

/// Relative paths of the duplicate tree's rows, everything expanded.
fn dup_tree_rows(root: &Path, filter: &PathFilter) -> Vec<String> {
    single_rows(&load_view(root), filter)
        .into_iter()
        .map(|row| row.rel_path)
        .collect()
}

/// Files and folders under a.txt, keep/, gen/ and src/, plus an empty folder.
fn dup_tree() -> TempDir {
    let dir = tempdir().unwrap();
    write_files(
        dir.path(),
        &[
            ("a.txt", "a"),
            ("b.obj", "b"),
            ("gen/c.txt", "c"),
            ("gen/deep/d.txt", "d"),
            ("keep/e.txt", "e"),
            ("keep/f.obj", "f"),
            ("src/deep/main.rs", "m"),
            // Named like a match, but the whitelist applies to files only.
            ("x.rs/readme.md", "r"),
        ],
    );
    fs::create_dir_all(dir.path().join("empty/nested")).unwrap();
    dir
}

#[test]
fn blacklisted_entries_are_absent_from_the_duplicate_tree() {
    let dir = dup_tree();

    assert_eq!(
        dup_tree_rows(dir.path(), &blacklist("*.OBJ, gen/")),
        [
            "",
            "empty",
            "empty/nested",
            "keep",
            "keep/e.txt",
            "src",
            "src/deep",
            "src/deep/main.rs",
            "x.rs",
            "x.rs/readme.md",
            "a.txt",
        ]
    );
}

#[test]
fn the_duplicate_tree_keeps_whitelisted_files_and_their_ancestors_only() {
    let dir = dup_tree();

    // keep/ has no whitelisted file left once f.obj is blacklisted.
    assert_eq!(
        dup_tree_rows(dir.path(), &whitelist("*.rs, *.obj", "f.obj")),
        ["", "src", "src/deep", "src/deep/main.rs", "b.obj"]
    );
}

#[test]
fn the_duplicate_trees_files_are_the_files_tree_hidden_leaves_visible() {
    let dir = dup_tree();
    let view = load_view(dir.path());
    let filters = [
        PathFilter::default(),
        blacklist("*.OBJ, gen/"),
        whitelist("*.rs, *.obj", "f.obj"),
    ];
    for filter in filters {
        let mut drawn: Vec<String> = single_rows(&view, &filter)
            .into_iter()
            .filter(|row| !row.is_dir)
            .map(|row| row.rel_path)
            .collect();
        let hidden = tree_hidden(&view, &filter);
        let mut visible: Vec<String> = view
            .file_system
            .iter_files()
            .filter(|&id| !hidden[id])
            .map(|id| get_rel(&view.file_system, dir.path(), id))
            .collect();
        drawn.sort();
        visible.sort();
        assert_eq!(drawn, visible, "{filter:?}");
    }
}

#[test]
fn without_a_filter_the_duplicate_tree_shows_everything() {
    let dir = dup_tree();

    assert_eq!(
        dup_tree_rows(dir.path(), &PathFilter::default()).len(),
        1 + 8 + 8,
        "the root, 8 files and 8 folders"
    );
}

fn row_paths(rows: &[VisibleRowTwoFolderDiff]) -> Vec<&str> {
    rows.iter().map(|r| r.rel_path.as_str()).collect()
}

#[test]
fn sorted_children_stay_under_their_parent() {
    let (left_dir, right_dir) = (tempdir().unwrap(), tempdir().unwrap());
    for root in [left_dir.path(), right_dir.path()] {
        // a.txt sorts between a and a/... by bytes ('.' < '/').
        write_files(
            root,
            &[
                ("a/b.txt", "b"),
                ("a/z/deep.txt", "deep"),
                ("a.txt", "a"),
                ("c/d.txt", "d"),
            ],
        );
    }
    let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));

    let rows = sort_two_folder_rows(rows, TreeSort::default());

    assert_eq!(
        row_paths(&rows),
        [
            "",
            "a",
            "a/z",
            "a/z/deep.txt",
            "a/b.txt",
            "c",
            "c/d.txt",
            "a.txt"
        ]
    );
}

#[test]
fn sorted_rows_keep_both_sides_on_the_same_relative_path() {
    let (left_dir, right_dir) = two_folder_trees();
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
    let by_state = TreeSort::default().clicked(SortKey::State);

    let rows = sort_two_folder_rows(rows, by_state);

    assert_eq!(
        row_paths(&rows),
        [
            "",
            "sub",
            "sub/deep",
            "sub/deep/d.txt",
            "sub/c.txt",
            "b.txt",
            "left_only.txt",
            "right_only.txt",
            "a.txt"
        ]
    );
    for row in &rows {
        if let Some(id) = row.diff_state.first() {
            assert_eq!(
                get_rel(&left.file_system, left_dir.path(), id),
                row.rel_path
            );
        }
        if let Some(id) = row.diff_state.second() {
            assert_eq!(
                get_rel(&right.file_system, right_dir.path(), id),
                row.rel_path
            );
        }
    }
}

#[test]
fn filtered_entries_are_absent_from_the_sorted_rows() {
    let (left_dir, right_dir) = two_folder_trees();
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let rows = build_filtered(
        &left,
        &right,
        &mut PairStates::default(),
        &Cell::new(0),
        &blacklist("b.txt, deep/"),
    );
    let by_state = TreeSort::default().clicked(SortKey::State);

    let rows = sort_two_folder_rows(rows, by_state);

    assert_eq!(
        row_paths(&rows),
        [
            "",
            "sub",
            "sub/c.txt",
            "left_only.txt",
            "right_only.txt",
            "a.txt"
        ]
    );
}

#[test]
fn flat_rows_are_ordered_by_depth_then_name_aligned_and_filtered() {
    let (left_dir, right_dir) = two_folder_trees();
    write_files(left_dir.path(), &[("sub/deep/a.txt", "l")]);
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let rows = build_filtered(
        &left,
        &right,
        &mut PairStates::default(),
        &Cell::new(0),
        &blacklist("b.txt, c.txt"),
    );
    let flat = TreeSort {
        flat: true,
        ..TreeSort::default()
    };

    let rows = sort_two_folder_rows(rows, flat);

    assert_eq!(
        row_paths(&rows),
        [
            "",
            "a.txt",
            "left_only.txt",
            "right_only.txt",
            "sub",
            "sub/deep",
            "sub/deep/a.txt",
            "sub/deep/d.txt"
        ]
    );
    for row in &rows {
        if let Some(id) = row.diff_state.first() {
            assert_eq!(
                get_rel(&left.file_system, left_dir.path(), id),
                row.rel_path
            );
        }
        if let Some(id) = row.diff_state.second() {
            assert_eq!(
                get_rel(&right.file_system, right_dir.path(), id),
                row.rel_path
            );
        }
    }
}

#[test]
fn single_side_flat_rows_are_ordered_by_depth_then_name_and_filtered() {
    let (left_dir, _right_dir) = two_folder_trees();
    write_files(left_dir.path(), &[("sub/deep/a.txt", "l")]);
    let left = load_view(left_dir.path());
    let flat = TreeSort {
        flat: true,
        ..TreeSort::default()
    };

    let rows = single_side_rows(&left, &blacklist("b.txt, c.txt"), flat).unwrap();

    assert_eq!(
        row_paths(&rows),
        [
            "",
            "a.txt",
            "left_only.txt",
            "sub",
            "sub/deep",
            "sub/deep/a.txt",
            "sub/deep/d.txt"
        ]
    );
    for row in &rows {
        assert_eq!(row.diff_state.second(), None);
        let id = row.diff_state.first().unwrap();
        assert_eq!(
            get_rel(&left.file_system, left_dir.path(), id),
            row.rel_path
        );
    }
}

#[test]
fn single_side_rows_follow_the_sort_and_keep_children_under_their_parent() {
    let dir = tempdir().unwrap();
    write_files(
        dir.path(),
        &[
            ("a/b.txt", "b"),
            ("a/z/deep.txt", "deep"),
            ("a.txt", "a"),
            ("c/d.txt", "d"),
        ],
    );
    let view = load_view(dir.path());
    let descending = TreeSort::default().clicked(SortKey::Name);

    let rows = single_side_rows(&view, &PathFilter::default(), descending).unwrap();

    assert_eq!(
        row_paths(&rows),
        [
            "",
            "c",
            "c/d.txt",
            "a",
            "a/z",
            "a/z/deep.txt",
            "a/b.txt",
            "a.txt"
        ]
    );
}

#[test]
fn a_side_is_as_wide_as_its_indentation_expander_and_name() {
    let spacing = 8.0;
    assert_eq!(
        side_width(RowLayout::Tree, 2, true, 50.0, spacing),
        2.0 * TREE_INDENT + EXPANDER_WIDTH + spacing + 50.0
    );
    assert_eq!(
        side_width(RowLayout::Tree, 2, false, 50.0, spacing),
        2.0 * TREE_INDENT + 50.0
    );
    assert_eq!(
        side_width(RowLayout::RelPaths, 2, true, 50.0, spacing),
        50.0
    );
    assert_eq!(
        side_width(RowLayout::FileList, 2, false, 50.0, spacing),
        50.0
    );
}

#[test]
fn two_folder_rows_carry_their_relative_path_with_slash_separators() {
    let (left_dir, right_dir) = two_folder_trees();
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());

    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));

    let paths: Vec<&str> = rows.iter().map(|r| r.rel_path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "",
            "a.txt",
            "b.txt",
            "left_only.txt",
            "right_only.txt",
            "sub",
            "sub/c.txt",
            "sub/deep",
            "sub/deep/d.txt",
        ]
    );
}

#[test]
fn rows_carry_each_sides_file_kind_from_the_table_and_none_for_folders() {
    let left_dir = tempdir().unwrap();
    let right_dir = tempdir().unwrap();
    let shared = [
        ("a.txt", "a"),
        ("p.PNG", "p"),
        ("x.uasset", "x"),
        ("noext", "n"),
        ("w.qqq", "w"),
        ("y.zzz", "y"),
        ("sub/c.txt", "c"),
    ];
    write_files(left_dir.path(), &shared);
    write_files(right_dir.path(), &shared);
    write_files(
        left_dir.path(),
        &[("left_only.rs", "l"), ("mixed/inner.txt", "i")],
    );
    write_files(right_dir.path(), &[("mixed", "file on the right")]);
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let mut kinds = KindTable::default();
    kinds.set_override("zzz", FileKind::Model);

    let rows = FileSystemView::build_two_folder_diff_rows(
        Some(&left),
        Some(&right),
        &PathFilter::default(),
        &kinds,
        &Tolerance::default(),
        |_, _| (None, None),
        |_, _| None,
    )
    .unwrap();

    let actual: Vec<(&str, [Option<FileKind>; 2])> = rows
        .iter()
        .map(|r| (r.rel_path.as_str(), r.file_kinds))
        .collect();
    let both = |kind| [Some(kind), Some(kind)];
    assert_eq!(
        actual,
        [
            ("", [None, None]),
            ("a.txt", both(FileKind::Text)),
            ("left_only.rs", [Some(FileKind::Text), None]),
            ("mixed", [None, Some(FileKind::Binary)]),
            ("mixed/inner.txt", [Some(FileKind::Text), None]),
            ("noext", both(FileKind::Binary)),
            ("p.PNG", both(FileKind::Image)),
            ("sub", [None, None]),
            ("sub/c.txt", both(FileKind::Text)),
            ("w.qqq", both(FileKind::Binary)),
            ("x.uasset", both(FileKind::UnrealAsset)),
            ("y.zzz", both(FileKind::Model)),
        ]
    );
}

fn blueprint_trees() -> (TempDir, TempDir) {
    let left = tempdir().unwrap();
    let right = tempdir().unwrap();
    write_files(
        left.path(),
        &[
            ("a.uasset", "a"),
            ("b.txt", "b"),
            ("sub/c.uasset", "c"),
            ("left_only.uasset", "l"),
        ],
    );
    write_files(
        right.path(),
        &[("a.uasset", "a2"), ("b.txt", "b"), ("sub/c.uasset", "c")],
    );
    (left, right)
}

fn blueprint_columns(rows: &[VisibleRowTwoFolderDiff]) -> Vec<(&str, [bool; 2])> {
    rows.iter()
        .map(|r| (r.rel_path.as_str(), r.blueprints))
        .collect()
}

#[test]
fn rows_check_each_sides_uasset_files_and_carry_known_blueprints() {
    let (left_dir, right_dir) = blueprint_trees();
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let known_blueprint = left_dir.path().join("a.uasset");
    let mut checked = Vec::new();

    let rows = FileSystemView::build_two_folder_diff_rows(
        Some(&left),
        Some(&right),
        &PathFilter::default(),
        &KindTable::default(),
        &Tolerance::default(),
        |_, _| (None, None),
        |path, meta| {
            let path = path();
            checked.push((path.clone(), meta.size));
            (path == known_blueprint).then_some(true)
        },
    )
    .unwrap();

    checked.sort();
    let mut expected = vec![
        (left_dir.path().join("a.uasset"), 1),
        (left_dir.path().join("left_only.uasset"), 1),
        (left_dir.path().join("sub").join("c.uasset"), 1),
        (right_dir.path().join("a.uasset"), 2),
        (right_dir.path().join("sub").join("c.uasset"), 1),
    ];
    expected.sort();
    assert_eq!(checked, expected);
    assert_eq!(
        blueprint_columns(&rows),
        [
            ("", [false, false]),
            ("a.uasset", [true, false]),
            ("b.txt", [false, false]),
            ("left_only.uasset", [false, false]),
            ("sub", [false, false]),
            ("sub/c.uasset", [false, false]),
        ]
    );
}

#[test]
fn blueprint_results_patch_the_matching_side_of_the_rows() {
    let (left_dir, right_dir) = blueprint_trees();
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let mut rows = FileSystemView::build_two_folder_diff_rows(
        Some(&left),
        Some(&right),
        &PathFilter::default(),
        &KindTable::default(),
        &Tolerance::default(),
        |_, _| (None, None),
        |_, _| None,
    )
    .unwrap();
    let roots = [Some(left_dir.path()), Some(right_dir.path())];
    let elsewhere = tempdir().unwrap();

    apply_blueprint_results(
        &mut rows,
        roots,
        &[
            (left_dir.path().join("a.uasset"), true),
            (right_dir.path().join("sub").join("c.uasset"), true),
            (left_dir.path().join("b.txt"), true),
            (left_dir.path().join("sub"), true),
            (elsewhere.path().join("a.uasset"), true),
        ],
    );
    assert_eq!(
        blueprint_columns(&rows),
        [
            ("", [false, false]),
            ("a.uasset", [true, false]),
            ("b.txt", [false, false]),
            ("left_only.uasset", [false, false]),
            ("sub", [false, false]),
            ("sub/c.uasset", [false, true]),
        ]
    );

    apply_blueprint_results(
        &mut rows,
        [None, Some(right_dir.path())],
        &[
            (left_dir.path().join("left_only.uasset"), true),
            (right_dir.path().join("sub").join("c.uasset"), false),
        ],
    );
    assert_eq!(rows[3].blueprints, [false, false]);
    assert_eq!(rows[5].blueprints, [false, false]);
}

#[test]
fn rebuilding_two_folder_rows_compares_no_files_and_builds_identical_rows() {
    let (left_dir, right_dir) = two_folder_trees();
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let comparisons = Cell::new(0);
    let mut states = PairStates::default();

    let first = build_counting(&left, &right, &mut states, &comparisons);
    // a.txt, b.txt, sub/c.txt and sub/deep/d.txt are on both sides.
    assert_eq!(comparisons.get(), 4);

    let second = build_counting(&left, &right, &mut states, &comparisons);
    assert_eq!(comparisons.get(), 4);
    assert_eq!(second, first);
}

#[test]
fn rows_inside_a_collapsed_folder_and_the_root_row_are_hidden_from_the_cursor() {
    let (left_dir, right_dir) = two_folder_trees();
    write_files(left_dir.path(), &[("sub/left_only_in_sub.txt", "l")]);
    let mut left = load_view(left_dir.path());
    let mut right = load_view(right_dir.path());
    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));

    // As a click on the row does: both sides.
    let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
    left.toggle_collapse(sub.diff_state.first().unwrap());
    right.toggle_collapse(sub.diff_state.second().unwrap());

    let hidden: Vec<String> = two_folder_cursor_rows(Some(&left), Some(&right), &rows, false)
        .into_iter()
        .filter(|r| r.hidden)
        .map(|r| r.rel_path)
        .collect();
    // sub/deep is expanded, but its parent isn't.
    assert_eq!(
        hidden,
        [
            "",
            "sub/c.txt",
            "sub/deep",
            "sub/deep/d.txt",
            "sub/left_only_in_sub.txt",
        ]
    );
}

#[test]
fn cursor_rows_are_rebuilt_when_the_rows_collapse_state_or_flat_mode_change_and_not_otherwise() {
    let (left_dir, right_dir) = two_folder_trees();
    let mut left = load_view(left_dir.path());
    let mut right = load_view(right_dir.path());
    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
    let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
    let (sub_left, sub_right) = (sub.diff_state.first(), sub.diff_state.second());
    let mut cache = CursorRowsCache::default();
    let mut builds = |left: &FileSystemView, right: &FileSystemView, id, flat| {
        cache.update([Some(left), Some(right)], &rows, id, flat);
        cache.builds
    };
    let id = RowsId::next();

    assert_eq!(builds(&left, &right, id, false), 1);
    assert_eq!(builds(&left, &right, id, false), 1);

    left.toggle_collapse(sub_left.unwrap());
    assert_eq!(builds(&left, &right, id, false), 2);
    assert_eq!(builds(&left, &right, id, false), 2);

    right.collapsed.insert(sub_right.unwrap(), true);
    assert_eq!(builds(&left, &right, id, false), 3);

    let id = RowsId::next();
    assert_eq!(builds(&left, &right, id, false), 4);
    assert_eq!(builds(&left, &right, id, false), 4);

    assert_eq!(builds(&left, &right, id, true), 5);
    assert_eq!(builds(&left, &right, id, true), 5);
}

#[test]
fn anything_collapsed_follows_each_fold_of_a_folder() {
    let (left_dir, _right_dir) = two_folder_trees();
    let mut view = load_view(left_dir.path());
    let sub = find_id_by_rel_path(&view.file_system, left_dir.path(), "sub");

    assert!(!view.is_anything_collapsed());
    view.toggle_collapse(sub);
    assert!(view.is_anything_collapsed());
    view.toggle_collapse(sub);
    assert!(!view.is_anything_collapsed());
}

#[test]
fn the_collapse_all_button_does_what_its_label_says() {
    let (left_dir, _right_dir) = two_folder_trees();
    let mut view = load_view(left_dir.path());
    let top_level = view.file_system.get_root().children().unwrap().clone();

    for expected_label in ["Collapse All", "Expand All", "Collapse All"] {
        let (label, collapse) = FileSystemView::collapse_all_toggle(view.is_anything_collapsed());
        assert_eq!(label, expected_label);
        view.recursive_collapse_slice(&top_level, collapse);
        assert_eq!(view.is_anything_collapsed(), label == "Collapse All");
    }
}

fn content_widths(
    widths: &mut ContentWidths,
    views: [&FileSystemView; 2],
    rows: &[VisibleRowTwoFolderDiff],
    cache: &CursorRowsCache,
) -> [f32; 2] {
    let ctx = egui::Context::default();
    let mut measured = [0.0; 2];
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            measured = widths.get(
                ui,
                views.map(Some),
                rows,
                cache.drawn(),
                cache.generation(),
                RowLayout::Tree,
            );
        });
    });
    measured
}

fn deep_and_shallow_trees() -> (TempDir, TempDir) {
    let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
    for root in [left.path(), right.path()] {
        write_files(
            root,
            &[
                ("x.txt", "x"),
                ("sub/deeper/a_much_longer_file_name_than_the_rest.txt", "y"),
            ],
        );
    }
    (left, right)
}

#[test]
fn content_widths_measure_each_name_once_and_follow_folds_and_rebuilt_rows() {
    let (left_dir, right_dir) = deep_and_shallow_trees();
    let mut left = load_view(left_dir.path());
    let mut right = load_view(right_dir.path());
    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
    let mut cache = CursorRowsCache::default();
    let mut widths = ContentWidths::default();
    let id = RowsId::next();

    cache.update([Some(&left), Some(&right)], &rows, id, false);
    let expanded = content_widths(&mut widths, [&left, &right], &rows, &cache);
    let measured = widths.measured;
    assert!(measured > 0);
    assert_eq!(
        content_widths(&mut widths, [&left, &right], &rows, &cache),
        expanded
    );

    cache.update([Some(&left), Some(&right)], &rows, RowsId::next(), false);
    assert_eq!(
        content_widths(&mut widths, [&left, &right], &rows, &cache),
        expanded
    );

    let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
    left.toggle_collapse(sub.diff_state.first().unwrap());
    right.toggle_collapse(sub.diff_state.second().unwrap());
    cache.update([Some(&left), Some(&right)], &rows, id, false);
    let folded = content_widths(&mut widths, [&left, &right], &rows, &cache);
    assert_eq!(
        folded,
        content_widths(
            &mut ContentWidths::default(),
            [&left, &right],
            &rows,
            &cache
        )
    );
    assert!(folded[0] < expanded[0] && folded[1] < expanded[1]);
    assert_eq!(widths.measured, measured);
}

#[test]
fn content_widths_measure_a_rescanned_model_again() {
    let (left_dir, right_dir) = deep_and_shallow_trees();
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
    let mut cache = CursorRowsCache::default();
    let mut widths = ContentWidths::default();
    cache.update([Some(&left), Some(&right)], &rows, RowsId::next(), false);
    let before = content_widths(&mut widths, [&left, &right], &rows, &cache);
    let measured = widths.measured;

    let left = load_view(left_dir.path());
    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
    cache.update([Some(&left), Some(&right)], &rows, RowsId::next(), false);

    assert_eq!(
        content_widths(&mut widths, [&left, &right], &rows, &cache),
        before
    );
    assert!(widths.measured > measured);
}

#[test]
fn cached_guides_are_the_guides_of_the_drawn_rows() {
    let (left_dir, right_dir) = two_folder_trees();
    let mut left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
    let mut cache = CursorRowsCache::default();
    let id = RowsId::next();
    let assert_guides = |cache: &CursorRowsCache| {
        for (side, side_id) in [
            (0, DiffState::first as fn(&DiffState) -> Option<FsNodeId>),
            (1, DiffState::second),
        ] {
            let input: Vec<(FsNodeDepth, bool)> = cache
                .drawn()
                .iter()
                .map(|&i| (rows[i].depth, side_id(&rows[i].diff_state).is_some()))
                .collect();
            let expected = tree_guides(&input);
            for i in 0..input.len() {
                assert_eq!(cache.guides()[side].row(i), expected.row(i));
            }
        }
    };

    cache.update([Some(&left), Some(&right)], &rows, id, false);
    assert_guides(&cache);

    let sub = rows.iter().find(|r| r.rel_path == "sub/deep").unwrap();
    left.toggle_collapse(sub.diff_state.first().unwrap());
    cache.update([Some(&left), Some(&right)], &rows, id, false);
    assert_guides(&cache);
}

#[test]
fn the_drawn_rows_are_the_rows_the_cursor_sees_and_reuse_one_buffer() {
    let (left_dir, right_dir) = two_folder_trees();
    let mut left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
    let id = RowsId::next();
    let mut cache = CursorRowsCache::default();
    let drawn_paths = |cache: &CursorRowsCache| -> Vec<String> {
        let cursor_rows = cache.cursor_rows();
        cache
            .drawn()
            .iter()
            .map(|&i| cursor_rows[i].rel_path.clone())
            .collect()
    };

    cache.update([Some(&left), Some(&right)], &rows, id, false);
    let buffer = cache.drawn().as_ptr();
    assert_eq!(drawn_paths(&cache).len(), rows.len() - 1);
    cache.update([Some(&left), Some(&right)], &rows, id, false);
    assert_eq!(cache.drawn().as_ptr(), buffer);

    let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
    left.toggle_collapse(sub.diff_state.first().unwrap());
    let cursor_rows = cache.update([Some(&left), Some(&right)], &rows, id, false);
    assert_eq!(
        cursor_rows,
        two_folder_cursor_rows(Some(&left), Some(&right), &rows, false)
    );
    assert_eq!(cache.drawn().as_ptr(), buffer);
    assert_eq!(
        drawn_paths(&cache),
        ["a.txt", "b.txt", "left_only.txt", "right_only.txt", "sub"]
    );

    let cursor_rows = cache.update([Some(&left), Some(&right)], &rows, id, true);
    assert_eq!(
        cursor_rows,
        two_folder_cursor_rows(Some(&left), Some(&right), &rows, true)
    );
}

#[test]
fn in_flat_mode_only_the_root_row_is_hidden_and_no_folder_is_collapsed() {
    let (left_dir, right_dir) = two_folder_trees();
    let mut left = load_view(left_dir.path());
    let mut right = load_view(right_dir.path());
    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
    let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
    left.toggle_collapse(sub.diff_state.first().unwrap());
    right.toggle_collapse(sub.diff_state.second().unwrap());

    let cursor_rows = two_folder_cursor_rows(Some(&left), Some(&right), &rows, true);

    let hidden: Vec<&str> = cursor_rows
        .iter()
        .filter(|r| r.hidden)
        .map(|r| r.rel_path.as_str())
        .collect();
    assert_eq!(hidden, [""]);
    assert!(cursor_rows.iter().all(|r| !r.collapsed));
}

#[test]
fn expand_and_collapse_requests_set_both_sides_even_when_they_disagree() {
    let (left_dir, right_dir) = two_folder_trees();
    let mut left = load_view(left_dir.path());
    let mut right = load_view(right_dir.path());
    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
    let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
    let (left_sub, right_sub) = (
        sub.diff_state.first().unwrap(),
        sub.diff_state.second().unwrap(),
    );
    let cursor_sees_collapsed = |left: &FileSystemView, right: &FileSystemView| {
        two_folder_cursor_rows(Some(left), Some(right), &rows, false)
            .iter()
            .find(|r| r.rel_path == "sub")
            .unwrap()
            .collapsed
    };
    let config = DiffToolConfig::default();

    left.toggle_collapse(left_sub);
    assert!(cursor_sees_collapsed(&left, &right), "left side only");

    let expand = CursorRequest::Expand("sub".to_string());
    apply_cursor_request(
        Some(&mut left),
        Some(&mut right),
        &rows,
        &expand,
        &config,
        &mut None,
    );
    assert!(!left.is_collapsed(left_sub) && !right.is_collapsed(right_sub));
    assert!(!cursor_sees_collapsed(&left, &right));

    right.toggle_collapse(right_sub);
    assert!(cursor_sees_collapsed(&left, &right), "right side only");

    let collapse = CursorRequest::Collapse("sub".to_string());
    apply_cursor_request(
        Some(&mut left),
        Some(&mut right),
        &rows,
        &collapse,
        &config,
        &mut None,
    );
    assert!(left.is_collapsed(left_sub) && right.is_collapsed(right_sub));
}

#[test]
fn in_single_side_mode_a_collapsed_folder_hides_its_rows_and_the_root_from_the_cursor() {
    let (left_dir, _right_dir) = two_folder_trees();
    let mut left = load_view(left_dir.path());
    let rows = single_rows(&left, &PathFilter::default());
    let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
    left.toggle_collapse(sub.diff_state.first().unwrap());

    let cursor_rows = two_folder_cursor_rows(Some(&left), None, &rows, false);

    let hidden: Vec<&str> = cursor_rows
        .iter()
        .filter(|r| r.hidden)
        .map(|r| r.rel_path.as_str())
        .collect();
    assert_eq!(hidden, ["", "sub/deep", "sub/deep/d.txt", "sub/c.txt"]);
    let collapsed: Vec<&str> = cursor_rows
        .iter()
        .filter(|r| r.collapsed)
        .map(|r| r.rel_path.as_str())
        .collect();
    assert_eq!(collapsed, ["sub"]);
}

#[test]
fn in_single_side_flat_mode_only_the_root_row_is_hidden_and_no_folder_is_collapsed() {
    let (left_dir, _right_dir) = two_folder_trees();
    let mut left = load_view(left_dir.path());
    let rows = single_rows(&left, &PathFilter::default());
    let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
    left.toggle_collapse(sub.diff_state.first().unwrap());

    let cursor_rows = two_folder_cursor_rows(Some(&left), None, &rows, true);

    let hidden: Vec<&str> = cursor_rows
        .iter()
        .filter(|r| r.hidden)
        .map(|r| r.rel_path.as_str())
        .collect();
    assert_eq!(hidden, [""]);
    assert!(cursor_rows.iter().all(|r| !r.collapsed));
}

#[test]
fn in_single_side_mode_the_cursor_walks_the_drawn_rows_and_folds_folder_1() {
    let (left_dir, _right_dir) = two_folder_trees();
    let mut left = load_view(left_dir.path());
    let rows = single_rows(&left, &PathFilter::default());
    let sub_id = rows
        .iter()
        .find(|r| r.rel_path == "sub")
        .unwrap()
        .diff_state
        .first()
        .unwrap();
    let config = DiffToolConfig::default();
    let cursor = TreeCursor::at("sub");

    let (_, collapse) = cursor.left(&two_folder_cursor_rows(Some(&left), None, &rows, false));
    apply_cursor_request(
        Some(&mut left),
        None,
        &rows,
        &collapse.unwrap(),
        &config,
        &mut None,
    );
    assert!(left.is_collapsed(sub_id));

    let cursor_rows = two_folder_cursor_rows(Some(&left), None, &rows, false);
    assert_eq!(cursor.down(&cursor_rows), TreeCursor::at("a.txt"));

    let expand = cursor.right(&cursor_rows).unwrap();
    apply_cursor_request(Some(&mut left), None, &rows, &expand, &config, &mut None);
    assert!(!left.is_collapsed(sub_id));
    let cursor_rows = two_folder_cursor_rows(Some(&left), None, &rows, false);
    assert_eq!(cursor.down(&cursor_rows), TreeCursor::at("sub/deep"));
}

#[test]
fn opening_a_file_in_single_side_mode_launches_nothing() {
    let (left_dir, _right_dir) = two_folder_trees();
    let mut left = load_view(left_dir.path());
    let rows = single_rows(&left, &PathFilter::default());
    let mut message = Some(ToolMessage {
        status: None,
        warning: Some("stale".to_string()),
        error: None,
    });

    apply_cursor_request(
        Some(&mut left),
        None,
        &rows,
        &CursorRequest::Open("b.txt".to_string()),
        &missing_tool_config(vec![FileKind::Text], true),
        &mut message,
    );

    assert_eq!(message, None);
}

fn missing_tool_config(kinds: Vec<FileKind>, mark_undiffable: bool) -> DiffToolConfig {
    DiffToolConfig {
        tools: vec![DiffTool {
            name: "Missing".to_string(),
            exe_path: "zdiff-folder-test-missing-tool.exe".into(),
            template: "{left} {right}".to_string(),
            kinds,
        }],
        rules: Vec::new(),
        default_tool: "Missing".to_string(),
        mark_undiffable,
    }
}

fn open_b_txt(config: &DiffToolConfig) -> Option<ToolMessage> {
    let (left_dir, right_dir) = two_folder_trees();
    let mut left = load_view(left_dir.path());
    let mut right = load_view(right_dir.path());
    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
    let mut message = Some(ToolMessage {
        status: None,
        warning: Some("stale".to_string()),
        error: None,
    });
    let open = CursorRequest::Open("b.txt".to_string());
    apply_cursor_request(
        Some(&mut left),
        Some(&mut right),
        &rows,
        &open,
        config,
        &mut message,
    );
    message
}

#[test]
fn a_tool_that_is_not_found_reports_the_places_searched() {
    let message = open_b_txt(&missing_tool_config(vec![FileKind::Text], true)).unwrap();
    assert_eq!(message.warning, None);
    let error = message.error.unwrap();
    assert!(
        error.contains("zdiff-folder-test-missing-tool.exe not found, searched: "),
        "{error}"
    );
}

#[test]
fn a_missing_default_tool_is_reported() {
    let config = DiffToolConfig {
        default_tool: "Nothing".to_string(),
        ..missing_tool_config(vec![FileKind::Text], false)
    };
    let message = open_b_txt(&config).unwrap();
    assert_eq!(message.warning, None);
    assert!(message.error.unwrap().contains("\"Nothing\" is missing"));
}

#[test]
fn opening_a_marked_row_warns_with_the_tool_and_the_kind() {
    let message = open_b_txt(&missing_tool_config(vec![FileKind::Image], true)).unwrap();
    assert_eq!(
        message.warning.as_deref(),
        Some("b.txt: opening anyway, Missing can't diff Text files")
    );
    assert!(message.error.is_some());

    let unmarked = open_b_txt(&missing_tool_config(vec![FileKind::Image], false)).unwrap();
    assert_eq!(unmarked.warning, None);
}

#[test]
fn cursor_rows_mark_every_entry_that_is_not_same_as_differing() {
    let (left_dir, right_dir) = two_folder_trees();
    let left = load_view(left_dir.path());
    let right = load_view(right_dir.path());
    let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));

    let differing: Vec<String> = two_folder_cursor_rows(Some(&left), Some(&right), &rows, false)
        .into_iter()
        .filter(|r| r.differs)
        .map(|r| r.rel_path)
        .collect();
    assert_eq!(differing, ["", "b.txt", "left_only.txt", "right_only.txt"]);
}

#[test]
fn cursor_keys_keep_arrow_repeats_and_press_order_but_drop_enter_repeats() {
    use eframe::egui::{Event, InputState, Key, Modifiers};
    let press = |key, repeat| Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat,
        modifiers: Modifiers::NONE,
    };
    let mut input = InputState::default();
    input.events = vec![
        press(Key::ArrowDown, false),
        press(Key::ArrowDown, true),
        press(Key::Enter, false),
        // Held Enter would open the diff tool once per repeat.
        press(Key::Enter, true),
        press(Key::ArrowLeft, false),
        press(Key::ArrowUp, true),
    ];

    assert_eq!(
        cursor_keys(&input),
        [
            Key::ArrowDown,
            Key::ArrowDown,
            Key::Enter,
            Key::ArrowLeft,
            Key::ArrowUp
        ]
    );
}

#[test]
fn test_build_collapsed_rows_scenarios_single_view() {
    let cases = vec![
        CollapsedTestCase {
            name: "Simple collapsed directory",
            structure: vec![("a", true), ("a/file.txt", false)],
            collapsed: vec!["a"],
            expected: vec![("a", 1)],
        },
        CollapsedTestCase {
            name: "Fully expanded directory",
            structure: vec![
                ("dir_a", true),
                ("dir_a/file_1.txt", false),
                ("dir_b", true),
            ],
            collapsed: vec![],
            expected: vec![("dir_a", 1), ("dir_a/file_1.txt", 2), ("dir_b", 1)],
        },
        CollapsedTestCase {
            name: "Deep nesting with partial collapse",
            structure: vec![
                ("level1", true),
                ("level1/level2", true),
                ("level1/level2/derp.txt", false),
                ("level1/level2/level3", true),
                ("level1/level2/level3/file.txt", false),
            ],
            collapsed: vec!["level1/level2"],
            expected: vec![("level1", 1), ("level1/level2", 2)],
        },
    ];

    for case in cases {
        let temp = tempdir().unwrap();
        let root_path = temp.path();

        for (rel_path, is_dir) in &case.structure {
            let full_path = root_path.join(rel_path);
            if *is_dir {
                fs::create_dir_all(&full_path).unwrap();
            } else {
                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent).unwrap();
                }
                File::create(&full_path).unwrap();
            }
        }

        let mut view = load_view(root_path);

        for path_str in &case.collapsed {
            let id = find_id_by_rel_path(&view.file_system, root_path, path_str);
            view.collapsed.insert(id, true);
        }

        let rows = single_rows(&view, &PathFilter::default());
        let cursor_rows = two_folder_cursor_rows(Some(&view), None, &rows, false);

        let actual: Vec<(String, FsNodeDepth)> = rows
            .iter()
            .zip(&cursor_rows)
            .filter(|(_, cursor_row)| !cursor_row.hidden)
            .map(|(row, _)| (row.rel_path.clone(), row.depth))
            .collect();

        let expected_mapped: Vec<(String, FsNodeDepth)> = case
            .expected
            .iter()
            .map(|(p, d)| (p.to_string(), *d))
            .collect();

        if actual != expected_mapped {
            panic!(
                "\nTest Case Failed: {}\n\nEXPECTED TREE:\n{}\n\nACTUAL TREE:\n{}\n",
                case.name,
                format_tree(&expected_mapped),
                format_tree(&actual)
            );
        }
    }
}

#[test]
fn a_one_sided_child_makes_the_folder_different() {
    for child in [DiffState::OnlyInFirst(3), DiffState::OnlyInSecond(3)] {
        assert_eq!(
            aggregate_folder_state(1, 2, [DiffState::Same(3, 3), child]),
            DiffState::Different(1, 2)
        );
    }
}

#[test]
fn a_folder_whose_children_are_all_same_is_same() {
    assert_eq!(
        aggregate_folder_state(1, 2, [DiffState::Same(3, 3), DiffState::Same(4, 4)]),
        DiffState::Same(1, 2)
    );
    assert_eq!(aggregate_folder_state(1, 2, []), DiffState::Same(1, 2));
}

#[test]
fn a_pending_child_makes_an_otherwise_same_folder_pending() {
    assert_eq!(
        aggregate_folder_state(1, 2, [DiffState::Same(3, 3), DiffState::Pending(4, 4)]),
        DiffState::Pending(1, 2)
    );
    let sub = aggregate_folder_state(5, 6, [DiffState::Pending(7, 7)]);
    assert_eq!(
        aggregate_folder_state(1, 2, [DiffState::Same(3, 3), sub]),
        DiffState::Pending(1, 2)
    );
}

#[test]
fn a_folder_settles_different_while_siblings_are_still_pending() {
    assert_eq!(
        aggregate_folder_state(
            1,
            2,
            [
                DiffState::Pending(3, 3),
                DiffState::Different(4, 4),
                DiffState::Pending(5, 5),
            ]
        ),
        DiffState::Different(1, 2)
    );
}

#[test]
fn a_similar_child_counts_as_equal_but_marks_the_folder_similar() {
    assert_eq!(
        aggregate_folder_state(1, 2, [DiffState::Similar(3, 3), DiffState::Same(4, 4)]),
        DiffState::Similar(1, 2)
    );
    assert_eq!(
        aggregate_folder_state(1, 2, [DiffState::Similar(3, 3), DiffState::Pending(4, 4)]),
        DiffState::Pending(1, 2)
    );
    assert_eq!(
        aggregate_folder_state(1, 2, [DiffState::Similar(3, 3), DiffState::Different(4, 4)]),
        DiffState::Different(1, 2)
    );
}

#[test]
fn next_and_prev_diff_skip_similar_files() {
    let row = |rel_path: &str, diff_state| VisibleRowTwoFolderDiff {
        rel_path: rel_path.to_string(),
        is_dir: rel_path.is_empty(),
        depth: 0,
        diff_state,
        file_kinds: [None, None],
        blueprints: [false, false],
        folder_likeness: None,
    };
    let rows = [
        row("", DiffState::Different(0, 0)),
        row("a.txt", DiffState::Different(1, 1)),
        row("b.txt", DiffState::Similar(2, 2)),
        row("c.txt", DiffState::Different(3, 3)),
        row("d.txt", DiffState::Similar(4, 4)),
    ];
    let cursor_rows = two_folder_cursor_rows(None, None, &rows, false);

    let next = TreeCursor::at("a.txt").next_diff(&cursor_rows);
    assert_eq!(next.map(|(c, _)| c), Some(TreeCursor::at("c.txt")));
    let prev = TreeCursor::at("c.txt").prev_diff(&cursor_rows);
    assert_eq!(prev.map(|(c, _)| c), Some(TreeCursor::at("a.txt")));
    let wrapped = TreeCursor::at("c.txt").next_diff(&cursor_rows);
    assert_ne!(wrapped.map(|(c, _)| c), Some(TreeCursor::at("d.txt")));
}

#[test]
fn next_and_prev_diff_skip_pending_files() {
    let row = |rel_path: &str, is_dir, diff_state| VisibleRowTwoFolderDiff {
        rel_path: rel_path.to_string(),
        is_dir,
        depth: 0,
        diff_state,
        file_kinds: [None, None],
        blueprints: [false, false],
        folder_likeness: None,
    };
    let rows = [
        row("", true, DiffState::Different(0, 0)),
        row("a.txt", false, DiffState::Same(1, 1)),
        row("b.txt", false, DiffState::Pending(2, 2)),
        row("c.txt", false, DiffState::Different(3, 3)),
        row("d.txt", false, DiffState::Pending(4, 4)),
        row("e.txt", false, DiffState::Same(5, 5)),
    ];
    let cursor_rows = two_folder_cursor_rows(None, None, &rows, false);

    let next = TreeCursor::at("a.txt").next_diff(&cursor_rows);
    assert_eq!(next.map(|(c, _)| c), Some(TreeCursor::at("c.txt")));
    let prev = TreeCursor::at("e.txt").prev_diff(&cursor_rows);
    assert_eq!(prev.map(|(c, _)| c), Some(TreeCursor::at("c.txt")));

    let pending_only = [
        row("", true, DiffState::Pending(0, 0)),
        row("b.txt", false, DiffState::Pending(2, 2)),
    ];
    let cursor_rows = two_folder_cursor_rows(None, None, &pending_only, false);
    assert_eq!(TreeCursor::default().next_diff(&cursor_rows), None);
    assert_eq!(TreeCursor::default().prev_diff(&cursor_rows), None);
}

fn walk_diffs(
    left: &mut FileSystemView,
    right: &mut FileSystemView,
    rows: &[VisibleRowTwoFolderDiff],
    flat: bool,
    next: bool,
) -> Vec<String> {
    let config = DiffToolConfig::default();
    let mut cursor = TreeCursor::default();
    let mut visited = Vec::new();
    loop {
        let cursor_rows = two_folder_cursor_rows(Some(left), Some(right), rows, flat);
        let target = if next {
            cursor.next_diff(&cursor_rows)
        } else {
            cursor.prev_diff(&cursor_rows)
        };
        let Some((moved, expand)) = target else {
            return visited;
        };
        for request in &expand {
            apply_cursor_request(
                Some(&mut *left),
                Some(&mut *right),
                rows,
                request,
                &config,
                &mut None,
            );
        }
        let rel_path = moved.rel_path().unwrap().to_owned();
        let drawn = two_folder_cursor_rows(Some(left), Some(right), rows, flat)
            .iter()
            .any(|r| r.rel_path == rel_path && !r.hidden);
        assert!(drawn, "{rel_path} is not drawn");
        visited.push(rel_path);
        cursor = moved;
    }
}

#[test]
fn next_and_prev_diff_reach_every_differing_file_in_every_option_combination() {
    let (left_dir, right_dir) = two_folder_trees();
    write_files(left_dir.path(), &[("sub/deep/e.txt", "left e")]);
    write_files(right_dir.path(), &[("sub/deep/e.txt", "right e")]);

    let tree_order = ["sub/deep/e.txt", "b.txt", "left_only.txt", "right_only.txt"];
    let flat_order = ["b.txt", "left_only.txt", "right_only.txt", "sub/deep/e.txt"];
    let list_order = ["b.txt", "sub/deep/e.txt", "left_only.txt", "right_only.txt"];

    for states in [StateFilter::All, StateFilter::Diff] {
        for show_parents in [true, false] {
            for flat in [false, true] {
                let filter = DiffFilter {
                    states,
                    show_parents,
                };
                let sort = TreeSort {
                    flat,
                    ..TreeSort::default()
                };
                let expected = if filter.lists_files() {
                    list_order
                } else if flat {
                    flat_order
                } else {
                    tree_order
                };
                let combination = format!("{filter:?} flat {flat}");

                for next in [true, false] {
                    let mut left = load_view(left_dir.path());
                    let mut right = load_view(right_dir.path());
                    let rows = arrange_two_folder_rows(
                        build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0)),
                        filter,
                        sort,
                    );
                    let left_sub = find_id_by_rel_path(&left.file_system, left_dir.path(), "sub");
                    let right_sub =
                        find_id_by_rel_path(&right.file_system, right_dir.path(), "sub");
                    left.toggle_collapse(left_sub);
                    right.toggle_collapse(right_sub);

                    let mut visited =
                        walk_diffs(&mut left, &mut right, &rows, filter.is_flat(sort), next);
                    if !next {
                        visited.reverse();
                    }
                    assert_eq!(visited, expected, "{combination} next {next}");
                }
            }
        }
    }
}
#[test]
fn path_bar_parent_segment_reroots_only_that_side() {
    let dir = tempdir().unwrap();
    let parent = dir.path().join("a");
    let root = parent.join("b");
    fs::create_dir_all(&root).unwrap();
    let mut root_1 = Some(root.clone());
    let mut root_2 = Some(root.clone());
    let parent_index = split_path(&root.to_string_lossy()).len() - 2;

    apply_path_bar(
        [&mut root_1, &mut root_2],
        Side::Folder1,
        PathBarAction::Segment(parent_index),
    )
    .unwrap();

    assert_eq!(root_1, Some(parent));
    assert_eq!(root_2, Some(root));
}

#[test]
fn path_bar_segment_on_a_git_root_keeps_the_revision() {
    let dir = tempdir().unwrap();
    let text = format!(
        "{}@git:origin/main",
        dir.path().join("a").join("b").display()
    );
    let mut root_1 = Some(text.into());
    let mut root_2 = None;
    let parent_index = split_path(&dir.path().join("a").to_string_lossy()).len() - 1;

    apply_path_bar(
        [&mut root_1, &mut root_2],
        Side::Folder1,
        PathBarAction::Segment(parent_index),
    )
    .unwrap();

    let expected = format!("{}@git:origin/main", dir.path().join("a").display());
    assert_eq!(root_1, Some(expected.into()));
}

#[test]
fn path_bar_segment_on_folder_2_reroots_folder_2() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("a");
    fs::create_dir_all(&root).unwrap();
    let mut root_1 = Some(root.clone());
    let mut root_2 = Some(root.clone());
    let parent_index = split_path(&root.to_string_lossy()).len() - 2;

    apply_path_bar(
        [&mut root_1, &mut root_2],
        Side::Folder2,
        PathBarAction::Segment(parent_index),
    )
    .unwrap();

    assert_eq!(root_1, Some(root));
    assert_eq!(root_2, Some(dir.path().to_path_buf()));
}

#[test]
fn path_bar_committed_folder_sets_root() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("target");
    fs::create_dir(&target).unwrap();
    let mut root_1 = None;
    let mut root_2 = Some(dir.path().to_path_buf());

    apply_path_bar(
        [&mut root_1, &mut root_2],
        Side::Folder2,
        PathBarAction::Commit(target.to_string_lossy().into_owned()),
    )
    .unwrap();

    assert_eq!(root_1, None);
    assert_eq!(root_2, Some(target));
}

#[test]
fn path_bar_committed_quoted_path_with_spaces_sets_root() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("with space");
    fs::create_dir(&target).unwrap();
    let mut root_1 = None;
    let mut root_2 = None;

    apply_path_bar(
        [&mut root_1, &mut root_2],
        Side::Folder1,
        PathBarAction::Commit(format!(" \"{}\" ", target.display())),
    )
    .unwrap();

    assert_eq!(root_1, Some(target));
}

#[test]
fn path_bar_committed_missing_path_keeps_root() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("missing");
    let mut root_1 = Some(dir.path().to_path_buf());
    let mut root_2 = None;

    let result = apply_path_bar(
        [&mut root_1, &mut root_2],
        Side::Folder1,
        PathBarAction::Commit(missing.to_string_lossy().into_owned()),
    );

    assert_eq!(result, Err(InvalidRoot::Missing(missing)));
    assert_eq!(root_1, Some(dir.path().to_path_buf()));
    assert_eq!(root_2, None);
}

#[test]
fn path_bar_committed_file_keeps_root() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("file.txt");
    File::create(&file).unwrap();
    let mut root_1 = None;
    let mut root_2 = Some(dir.path().to_path_buf());

    let result = apply_path_bar(
        [&mut root_1, &mut root_2],
        Side::Folder2,
        PathBarAction::Commit(file.to_string_lossy().into_owned()),
    );

    assert_eq!(result, Err(InvalidRoot::NotAFolder(file)));
    assert_eq!(root_2, Some(dir.path().to_path_buf()));
}

fn anchor_rows(rel_paths: &[&str]) -> (Vec<VisibleRowTwoFolderDiff>, Vec<usize>) {
    let rows = rel_paths
        .iter()
        .map(|rel_path| VisibleRowTwoFolderDiff {
            rel_path: rel_path.to_string(),
            is_dir: false,
            depth: 0,
            diff_state: DiffState::OnlyInFirst(0),
            file_kinds: [None, None],
            blueprints: [false, false],
            folder_likeness: None,
        })
        .collect();
    (rows, (1..rel_paths.len()).collect())
}

fn anchored_at(rel_paths: &[&str], scroll_offset: f32) -> TreeTable {
    let (rows, drawn) = anchor_rows(rel_paths);
    let mut table = TreeTable::default();
    table.set_anchor(1, scroll_offset, &rows, &drawn, 10.0);
    table
}

fn offset_after_rebuild(table: &TreeTable, rel_paths: &[&str]) -> Option<f32> {
    let (rows, drawn) = anchor_rows(rel_paths);
    table.anchored_offset(2, &rows, &drawn, 10.0)
}

#[test]
fn rebuilt_rows_scroll_the_top_row_back_to_the_top() {
    let table = anchored_at(&["", "a", "b", "c", "d"], 23.0);

    let offset = offset_after_rebuild(&table, &["", "d", "c", "a", "b"]);

    assert_eq!(offset, Some(13.0));
}

#[test]
fn a_top_row_that_is_gone_scrolls_its_nearest_drawn_ancestor_to_the_top() {
    let table = anchored_at(&["", "x", "x/y", "x/y/f", "z"], 25.0);

    let offset = offset_after_rebuild(&table, &["", "a", "b", "x", "x/y", "x/yz", "z"]);

    assert_eq!(offset, Some(30.0));
}

#[test]
fn rebuilt_rows_keep_the_scroll_when_the_top_row_did_not_move_or_nothing_is_left_of_it() {
    let table = anchored_at(&["", "a", "b", "c"], 15.0);
    let (rows, drawn) = anchor_rows(&["", "x", "b", "c"]);

    assert_eq!(table.anchored_offset(1, &rows, &drawn, 10.0), None);
    assert_eq!(offset_after_rebuild(&table, &["", "x", "b", "c"]), None);
    assert_eq!(offset_after_rebuild(&table, &["", "a", "c"]), None);
}

#[test]
fn a_view_at_the_very_top_stays_at_the_top_of_rebuilt_rows() {
    let table = anchored_at(&["", "a", "b", "c"], 0.0);

    let offset = offset_after_rebuild(&table, &["", "c", "b", "a"]);

    assert_eq!(offset, None);
}

#[test]
fn an_offset_a_hair_short_of_a_row_anchors_that_row() {
    let table = anchored_at(&["", "a", "b", "c", "d"], 20.0 - 0.001);

    let offset = offset_after_rebuild(&table, &["", "c", "d", "a", "b"]);

    assert_eq!(offset.map(f32::round), Some(0.0));
}
