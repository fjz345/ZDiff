use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::folder::{
    fs_tree::{DiffState, VisibleRowTwoFolderDiff},
    tree_sort::{TreeSort, sort_file_list, sort_two_folder_rows},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateFilter {
    #[default]
    All,
    Diff,
    Equal,
}

impl StateFilter {
    pub fn next(self) -> Self {
        match self {
            StateFilter::All => StateFilter::Diff,
            StateFilter::Diff => StateFilter::Equal,
            StateFilter::Equal => StateFilter::All,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiffFilter {
    #[serde(alias = "diff_only", deserialize_with = "deserialize_states")]
    pub states: StateFilter,
    /// Only applies when `states` filters.
    pub show_parents: bool,
}

impl Default for DiffFilter {
    fn default() -> Self {
        Self {
            states: StateFilter::All,
            show_parents: true,
        }
    }
}

/// Also reads the `diff_only` bool saved before `states` existed.
fn deserialize_states<'de, D: Deserializer<'de>>(deserializer: D) -> Result<StateFilter, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Saved {
        DiffOnly(bool),
        States(StateFilter),
    }
    Ok(match Saved::deserialize(deserializer)? {
        Saved::DiffOnly(true) => StateFilter::Diff,
        Saved::DiffOnly(false) => StateFilter::All,
        Saved::States(states) => states,
    })
}

impl DiffFilter {
    pub fn filters(self) -> bool {
        self.states != StateFilter::All
    }

    pub fn lists_files(self) -> bool {
        self.filters() && !self.show_parents
    }

    /// The rows show no hierarchy, so folding doesn't apply.
    pub fn is_flat(self, sort: TreeSort) -> bool {
        sort.flat || self.lists_files()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DuplicatesFilter {
    pub duplicates_only: bool,
    /// Only applies with `duplicates_only`.
    pub show_parents: bool,
    pub hash_column: bool,
}

impl Default for DuplicatesFilter {
    fn default() -> Self {
        Self {
            duplicates_only: false,
            show_parents: true,
            hash_column: false,
        }
    }
}

impl DuplicatesFilter {
    pub fn diff_filter(self) -> DiffFilter {
        DiffFilter {
            states: if self.duplicates_only {
                StateFilter::Diff
            } else {
                StateFilter::All
            },
            show_parents: self.show_parents,
        }
    }
}

/// The root row is always kept: the tree sort hangs every row under it and the header shows its
/// state.
pub fn filter_diff_rows(
    rows: Vec<VisibleRowTwoFolderDiff>,
    filter: DiffFilter,
) -> Vec<VisibleRowTwoFolderDiff> {
    let equal = |row: &VisibleRowTwoFolderDiff| {
        matches!(row.diff_state, DiffState::Same(..) | DiffState::Similar(..))
    };
    match filter.states {
        StateFilter::All => rows,
        StateFilter::Diff => filter_rows(rows, filter, |row| !equal(row)),
        StateFilter::Equal => filter_rows(rows, filter, equal),
    }
}

/// `filter_diff_rows` with `keep` deciding which rows are kept.
pub fn filter_rows(
    mut rows: Vec<VisibleRowTwoFolderDiff>,
    filter: DiffFilter,
    keep: impl Fn(&VisibleRowTwoFolderDiff) -> bool,
) -> Vec<VisibleRowTwoFolderDiff> {
    if !filter.filters() {
        return rows;
    }
    if filter.lists_files() {
        rows.retain(|row| row.rel_path.is_empty() || (!row.is_dir && keep(row)));
        return rows;
    }
    let mut ancestors: HashSet<String> = HashSet::new();
    for row in rows.iter().filter(|row| keep(row)) {
        let mut path = row.rel_path.as_str();
        while let Some((parent, _)) = path.rsplit_once('/') {
            if !ancestors.insert(parent.to_owned()) {
                break;
            }
            path = parent;
        }
    }
    rows.retain(|row| row.rel_path.is_empty() || keep(row) || ancestors.contains(&row.rel_path));
    rows
}

pub fn arrange_two_folder_rows(
    rows: Vec<VisibleRowTwoFolderDiff>,
    filter: DiffFilter,
    sort: TreeSort,
) -> Vec<VisibleRowTwoFolderDiff> {
    let _scope = zcommon_ui::frame_stats::scope("Arrange rows (state filter, sort)");
    let rows = filter_diff_rows(rows, filter);
    if filter.lists_files() {
        sort_file_list(rows, sort)
    } else {
        sort_two_folder_rows(rows, sort)
    }
}

#[cfg(test)]
mod tests {
    use super::{DiffFilter, StateFilter, arrange_two_folder_rows, filter_diff_rows};
    use crate::folder::fs_tree::{DiffState, VisibleRowTwoFolderDiff};
    use crate::folder::tree_sort::{SortKey, TreeSort};

    fn row(rel_path: &str, is_dir: bool, diff_state: DiffState) -> VisibleRowTwoFolderDiff {
        VisibleRowTwoFolderDiff {
            rel_path: rel_path.to_owned(),
            is_dir,
            depth: rel_path.split('/').filter(|c| !c.is_empty()).count() as _,
            diff_state,
            file_kinds: [None, None],
            blueprints: [false, false],
            folder_likeness: None,
        }
    }

    fn rows() -> Vec<VisibleRowTwoFolderDiff> {
        vec![
            row("", true, DiffState::Different(0, 0)),
            row("a", true, DiffState::Different(0, 0)),
            row("a/same.txt", false, DiffState::Same(0, 0)),
            row("a/x", true, DiffState::Different(0, 0)),
            row("a/x/b.txt", false, DiffState::Different(0, 0)),
            row("added", true, DiffState::OnlyInSecond(0)),
            row("added/new.txt", false, DiffState::OnlyInSecond(0)),
            row("c.txt", false, DiffState::Different(0, 0)),
            row("empty_removed", true, DiffState::OnlyInFirst(0)),
            row("equal", true, DiffState::Same(0, 0)),
            row("equal/deep", true, DiffState::Same(0, 0)),
            row("equal/deep/e.txt", false, DiffState::Same(0, 0)),
            row("pending.txt", false, DiffState::Pending(0, 0)),
            row("removed.txt", false, DiffState::OnlyInFirst(0)),
            row("same.txt", false, DiffState::Same(0, 0)),
        ]
    }

    fn paths(rows: &[VisibleRowTwoFolderDiff]) -> Vec<&str> {
        rows.iter().map(|r| r.rel_path.as_str()).collect()
    }

    const DIFF_ONLY: DiffFilter = DiffFilter {
        states: StateFilter::Diff,
        show_parents: true,
    };
    const FILE_LIST: DiffFilter = DiffFilter {
        states: StateFilter::Diff,
        show_parents: false,
    };
    const EQUAL: DiffFilter = DiffFilter {
        states: StateFilter::Equal,
        show_parents: true,
    };
    const EQUAL_FILE_LIST: DiffFilter = DiffFilter {
        states: StateFilter::Equal,
        show_parents: false,
    };

    #[test]
    fn the_state_filter_cycles_all_diff_equal() {
        assert_eq!(StateFilter::All.next(), StateFilter::Diff);
        assert_eq!(StateFilter::Diff.next(), StateFilter::Equal);
        assert_eq!(StateFilter::Equal.next(), StateFilter::All);
    }

    #[test]
    fn equal_with_parents_keeps_same_rows_and_their_ancestors() {
        assert_eq!(
            paths(&filter_diff_rows(rows(), EQUAL)),
            [
                "",
                "a",
                "a/same.txt",
                "equal",
                "equal/deep",
                "equal/deep/e.txt",
                "same.txt",
            ]
        );
    }

    #[test]
    fn equal_without_parents_lists_only_equal_files_and_the_root() {
        assert_eq!(
            paths(&filter_diff_rows(rows(), EQUAL_FILE_LIST)),
            ["", "a/same.txt", "equal/deep/e.txt", "same.txt"]
        );
    }

    #[test]
    fn without_diff_only_every_row_is_kept_whatever_show_parents_says() {
        let all = paths(&rows()).len();
        for show_parents in [true, false] {
            let filter = DiffFilter {
                states: StateFilter::All,
                show_parents,
            };
            assert_eq!(filter_diff_rows(rows(), filter).len(), all);
        }
    }

    #[test]
    fn diff_only_with_parents_keeps_differing_rows_and_their_ancestors() {
        assert_eq!(
            paths(&filter_diff_rows(rows(), DIFF_ONLY)),
            [
                "",
                "a",
                "a/x",
                "a/x/b.txt",
                "added",
                "added/new.txt",
                "c.txt",
                "empty_removed",
                "pending.txt",
                "removed.txt",
            ]
        );
    }

    #[test]
    fn diff_only_hides_equal_subtrees() {
        let shown = filter_diff_rows(rows(), DIFF_ONLY);
        assert!(
            shown
                .iter()
                .all(|r| !matches!(r.diff_state, DiffState::Same(..)))
        );
        assert!(!paths(&shown).iter().any(|p| p.starts_with("equal")));
    }

    #[test]
    fn diff_only_keeps_one_sided_entries() {
        let shown = filter_diff_rows(rows(), DIFF_ONLY);
        for one_sided in ["added", "added/new.txt", "empty_removed", "removed.txt"] {
            assert!(paths(&shown).contains(&one_sided), "{one_sided}");
        }
        let listed = filter_diff_rows(rows(), FILE_LIST);
        for one_sided in ["added/new.txt", "removed.txt"] {
            assert!(paths(&listed).contains(&one_sided), "{one_sided}");
        }
    }

    #[test]
    fn diff_only_without_parents_lists_only_differing_files_and_the_root() {
        assert_eq!(
            paths(&filter_diff_rows(rows(), FILE_LIST)),
            [
                "",
                "a/x/b.txt",
                "added/new.txt",
                "c.txt",
                "pending.txt",
                "removed.txt",
            ]
        );
    }

    #[test]
    fn the_root_row_is_kept_when_everything_is_same() {
        let equal = || {
            vec![
                row("", true, DiffState::Same(0, 0)),
                row("a.txt", false, DiffState::Same(0, 0)),
            ]
        };
        assert_eq!(paths(&filter_diff_rows(equal(), DIFF_ONLY)), [""]);
        assert_eq!(paths(&filter_diff_rows(equal(), FILE_LIST)), [""]);
    }

    #[test]
    fn a_differing_row_under_a_same_folder_keeps_the_folder() {
        let rows = vec![
            row("", true, DiffState::Same(0, 0)),
            row("a", true, DiffState::Same(0, 0)),
            row("a/b.txt", false, DiffState::Different(0, 0)),
        ];
        assert_eq!(
            paths(&arrange_two_folder_rows(
                rows,
                DIFF_ONLY,
                TreeSort::default()
            )),
            ["", "a", "a/b.txt"]
        );
    }

    #[test]
    fn diff_only_hides_similar_rows_and_similar_folders() {
        let rows = || {
            vec![
                row("", true, DiffState::Different(0, 0)),
                row("near", true, DiffState::Similar(0, 0)),
                row("near/a.txt", false, DiffState::Similar(0, 0)),
                row("near/b.txt", false, DiffState::Same(0, 0)),
                row("near.txt", false, DiffState::Similar(0, 0)),
                row("far.txt", false, DiffState::Different(0, 0)),
            ]
        };
        assert_eq!(paths(&filter_diff_rows(rows(), DIFF_ONLY)), ["", "far.txt"]);
        assert_eq!(paths(&filter_diff_rows(rows(), FILE_LIST)), ["", "far.txt"]);
        assert_eq!(
            paths(&filter_diff_rows(rows(), EQUAL)),
            ["", "near", "near/a.txt", "near/b.txt", "near.txt"]
        );
    }

    #[test]
    fn the_file_list_is_sorted_by_the_active_key_with_the_root_first() {
        let name_asc = TreeSort::default();
        assert_eq!(
            paths(&arrange_two_folder_rows(rows(), FILE_LIST, name_asc)),
            [
                "",
                "a/x/b.txt",
                "c.txt",
                "added/new.txt",
                "pending.txt",
                "removed.txt",
            ]
        );
        let name_desc = name_asc.clicked(SortKey::Name);
        assert_eq!(
            paths(&arrange_two_folder_rows(rows(), FILE_LIST, name_desc)),
            [
                "",
                "removed.txt",
                "pending.txt",
                "added/new.txt",
                "c.txt",
                "a/x/b.txt",
            ]
        );
        let state_asc = name_asc.clicked(SortKey::State);
        assert_eq!(
            paths(&arrange_two_folder_rows(rows(), FILE_LIST, state_asc)),
            [
                "",
                "a/x/b.txt",
                "c.txt",
                "removed.txt",
                "added/new.txt",
                "pending.txt",
            ]
        );
    }

    #[test]
    fn the_file_list_ignores_flat() {
        let flat = TreeSort {
            flat: true,
            ..TreeSort::default()
        };
        assert_eq!(
            arrange_two_folder_rows(rows(), FILE_LIST, flat),
            arrange_two_folder_rows(rows(), FILE_LIST, TreeSort::default())
        );
    }

    #[test]
    fn diff_only_with_parents_sorts_as_a_tree() {
        assert_eq!(
            paths(&arrange_two_folder_rows(
                rows(),
                DIFF_ONLY,
                TreeSort::default()
            )),
            [
                "",
                "a",
                "a/x",
                "a/x/b.txt",
                "added",
                "added/new.txt",
                "empty_removed",
                "c.txt",
                "pending.txt",
                "removed.txt",
            ]
        );
    }

    #[test]
    fn only_the_file_list_and_flat_hide_the_hierarchy() {
        let tree = TreeSort::default();
        let flat = TreeSort { flat: true, ..tree };
        let no_diff_only_no_parents = DiffFilter {
            states: StateFilter::All,
            show_parents: false,
        };
        assert!(!DiffFilter::default().is_flat(tree));
        assert!(!DIFF_ONLY.is_flat(tree));
        assert!(!no_diff_only_no_parents.is_flat(tree));
        assert!(FILE_LIST.is_flat(tree));
        assert!(EQUAL_FILE_LIST.is_flat(tree));
        assert!(DiffFilter::default().is_flat(flat));
    }
}
