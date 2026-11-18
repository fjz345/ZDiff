use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use zcommon::find::{FindQuery, Matcher};

use crate::folder::fs_tree::VisibleRowTwoFolderDiff;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderFind {
    pub query: FindQuery,
    pub match_path: bool,
    pub show_contents: bool,
}

#[derive(Debug, PartialEq)]
pub struct FindMask {
    pub hidden: Vec<bool>,
    pub matches: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindChipEvent {
    Reopen,
    Clear,
}

pub struct TreeFindView<'a> {
    pub matcher: Option<&'a Matcher>,
    pub match_path: bool,
    /// The query, while the popup is closed.
    pub chip: Option<&'a str>,
    pub chip_event: Option<FindChipEvent>,
}

/// `rows` are the paired rows the path filter shows, so find can only narrow them. In `flat`
/// rows the ancestors of a match aren't kept.
pub fn find_mask(
    rows: &[VisibleRowTwoFolderDiff],
    matcher: &Matcher,
    find: &FolderFind,
    flat: bool,
) -> FindMask {
    let is_match = |rel_path: &str| {
        let text = if find.match_path {
            rel_path
        } else {
            rel_path.rsplit('/').next().unwrap_or(rel_path)
        };
        matcher.find_iter(text).next().is_some()
    };
    let matched: Vec<bool> = rows
        .iter()
        .map(|row| !row.rel_path.is_empty() && is_match(&row.rel_path))
        .collect();

    let mut ancestors: HashSet<&str> = HashSet::new();
    if !flat {
        for (row, _) in rows.iter().zip(&matched).filter(|(_, matched)| **matched) {
            let mut path = row.rel_path.as_str();
            // Stops at the first ancestor already kept: its own ancestors are kept too.
            while let Some((parent, _)) = path.rsplit_once('/') {
                if !ancestors.insert(parent) {
                    break;
                }
                path = parent;
            }
        }
    }

    // By path, not by row: a files-only list has no folder rows.
    let mut in_matched_folder: HashMap<&str, bool> = HashMap::new();
    let hidden = rows
        .iter()
        .zip(&matched)
        .map(|(row, &matched)| {
            let shown = row.rel_path.is_empty()
                || matched
                || ancestors.contains(row.rel_path.as_str())
                || (find.show_contents
                    && row.rel_path.rsplit_once('/').is_some_and(|(parent, _)| {
                        folder_or_ancestor_matches(parent, &mut in_matched_folder, &is_match)
                    }));
            !shown
        })
        .collect();

    FindMask {
        hidden,
        matches: matched.iter().filter(|&&matched| matched).count(),
    }
}

/// The matches `find_mask` sees inside the drawn text (path or name), relative to it, merged.
pub fn drawn_ranges(
    matcher: &Matcher,
    match_path: bool,
    rel_path: &str,
    draws_path: bool,
) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    if rel_path.is_empty() {
        return ranges;
    }
    let name_start = rel_path.rfind('/').map_or(0, |i| i + 1);
    let searched_from = if match_path { 0 } else { name_start };
    let drawn_from = if draws_path { 0 } else { name_start };
    for found in matcher.find_iter(&rel_path[searched_from..]) {
        let start = (searched_from + found.start).max(drawn_from);
        let end = searched_from + found.end;
        if start >= end {
            continue;
        }
        let range = start - drawn_from..end - drawn_from;
        match ranges.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => ranges.push(range),
        }
    }
    ranges
}

fn folder_or_ancestor_matches<'a>(
    folder: &'a str,
    memo: &mut HashMap<&'a str, bool>,
    is_match: &impl Fn(&str) -> bool,
) -> bool {
    if let Some(&matches) = memo.get(folder) {
        return matches;
    }
    let matches = is_match(folder)
        || folder
            .rsplit_once('/')
            .is_some_and(|(parent, _)| folder_or_ancestor_matches(parent, memo, is_match));
    memo.insert(folder, matches);
    matches
}

#[cfg(test)]
mod tests {
    use super::{FindMask, FolderFind, find_mask};
    use crate::folder::fs_tree::{DiffState, VisibleRowTwoFolderDiff};
    use zcommon::find::{FindQuery, Matcher};

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
            row("art", true, DiffState::Different(0, 0)),
            row("art/ui", true, DiffState::Different(0, 0)),
            row("art/ui/button.png", false, DiffState::Same(0, 0)),
            row("art/ui/panel.png", false, DiffState::Different(0, 0)),
            row("art/icons", true, DiffState::Same(0, 0)),
            row("art/icons/ui.png", false, DiffState::Same(0, 0)),
            row("src", true, DiffState::OnlyInFirst(0)),
            row("src/main.rs", false, DiffState::OnlyInFirst(0)),
            row("tools", true, DiffState::OnlyInSecond(0)),
            row("tools/make.rs", false, DiffState::OnlyInSecond(0)),
            row("readme.md", false, DiffState::Same(0, 0)),
        ]
    }

    fn find(text: &str) -> FolderFind {
        FolderFind {
            query: FindQuery {
                text: text.to_owned(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn mask(rows: &[VisibleRowTwoFolderDiff], find: &FolderFind, flat: bool) -> FindMask {
        find_mask(rows, &Matcher::new(&find.query).unwrap(), find, flat)
    }

    fn shown<'a>(
        rows: &'a [VisibleRowTwoFolderDiff],
        find: &FolderFind,
        flat: bool,
    ) -> Vec<&'a str> {
        let mask = mask(rows, find, flat);
        assert_eq!(mask.hidden.len(), rows.len());
        rows.iter()
            .zip(mask.hidden)
            .filter(|(_, hidden)| !hidden)
            .map(|(row, _)| row.rel_path.as_str())
            .collect()
    }

    #[test]
    fn a_file_match_keeps_its_ancestors() {
        let rows = rows();
        assert_eq!(
            shown(&rows, &find("panel"), false),
            ["", "art", "art/ui", "art/ui/panel.png"]
        );
        assert_eq!(mask(&rows, &find("panel"), false).matches, 1);
    }

    #[test]
    fn a_match_on_one_side_keeps_the_row() {
        let rows = rows();
        assert_eq!(
            shown(&rows, &find("main"), false),
            ["", "src", "src/main.rs"]
        );
        assert_eq!(
            shown(&rows, &find("make"), false),
            ["", "tools", "tools/make.rs"]
        );
    }

    #[test]
    fn a_matched_folder_shows_only_matching_contents_unless_contents_are_on() {
        let rows = rows();
        let off = find("ui");
        assert_eq!(
            shown(&rows, &off, false),
            ["", "art", "art/ui", "art/icons", "art/icons/ui.png"]
        );
        assert_eq!(mask(&rows, &off, false).matches, 2);

        let on = FolderFind {
            show_contents: true,
            ..off
        };
        assert_eq!(
            shown(&rows, &on, false),
            [
                "",
                "art",
                "art/ui",
                "art/ui/button.png",
                "art/ui/panel.png",
                "art/icons",
                "art/icons/ui.png"
            ]
        );
        assert_eq!(mask(&rows, &on, false).matches, 2);
    }

    #[test]
    fn a_path_query_matches_only_in_path_mode() {
        let rows = rows();
        let name = find("art/ui");
        assert_eq!(shown(&rows, &name, false), [""]);

        let path = FolderFind {
            match_path: true,
            ..name
        };
        assert_eq!(
            shown(&rows, &path, false),
            ["", "art", "art/ui", "art/ui/button.png", "art/ui/panel.png"]
        );
        assert_eq!(mask(&rows, &path, false).matches, 3);
    }

    #[test]
    fn no_match_hides_everything_below_the_root_and_counts_zero() {
        let rows = rows();
        let mask = mask(&rows, &find("nothing"), false);
        assert_eq!(mask.matches, 0);
        assert!(!mask.hidden[0]);
        assert!(mask.hidden[1..].iter().all(|&hidden| hidden));
    }

    #[test]
    fn the_root_never_matches() {
        let rows = vec![row("", true, DiffState::Same(0, 0))];
        let mut regex = find(".*");
        regex.query.regex = true;
        assert_eq!(mask(&rows, &regex, false).matches, 0);
    }

    #[test]
    fn flat_rows_keep_no_ancestors_but_keep_matched_folder_contents() {
        let rows = rows();
        assert_eq!(shown(&rows, &find("panel"), true), ["", "art/ui/panel.png"]);

        let files: Vec<_> = rows
            .into_iter()
            .filter(|row| row.rel_path.is_empty() || !row.is_dir)
            .collect();
        let contents = FolderFind {
            show_contents: true,
            ..find("ui")
        };
        assert_eq!(
            shown(&files, &contents, true),
            [
                "",
                "art/ui/button.png",
                "art/ui/panel.png",
                "art/icons/ui.png"
            ]
        );
    }
}
