use eframe::egui;
use std::time::Duration;
use zcommon::universal_path::UniversalPath;
use zdiff_folder::scan::SkippedEntry;

use crate::folder::{
    fs_tree::{DiffState, VisibleRowTwoFolderDiff},
    roots::Side,
};

#[derive(Debug)]
pub struct ScanSummary {
    pub elapsed: Duration,
    pub skipped: Vec<SkippedEntry>,
}

#[derive(Debug, Default, PartialEq)]
pub struct RowCounts {
    pub different: usize,
    pub only_in_1: usize,
    pub only_in_2: usize,
    pub same: usize,
    pub similar: usize,
    pub pending: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolMessage {
    pub status: Option<String>,
    pub warning: Option<String>,
    pub error: Option<String>,
    pub open_in_app: Option<[UniversalPath; 2]>,
}

/// Files, plus folders on one side only; a folder on both sides only aggregates its files.
pub fn count_rows(rows: &[VisibleRowTwoFolderDiff]) -> RowCounts {
    let mut counts = RowCounts::default();
    for row in rows.iter().filter(|row| !row.rel_path.is_empty()) {
        let count = match row.diff_state {
            DiffState::OnlyInFirst(..) => &mut counts.only_in_1,
            DiffState::OnlyInSecond(..) => &mut counts.only_in_2,
            _ if row.is_dir => continue,
            DiffState::Different(..) => &mut counts.different,
            DiffState::Same(..) => &mut counts.same,
            DiffState::Similar(..) => &mut counts.similar,
            DiffState::Pending(..) => &mut counts.pending,
        };
        *count += 1;
    }
    counts
}

pub fn counts_text(counts: &RowCounts, tolerance_on: bool) -> String {
    let similar = if tolerance_on {
        format!(", {} similar", counts.similar)
    } else {
        String::new()
    };
    format!(
        "{} different, {} only in 1, {} only in 2, {} same{similar}, {} pending",
        counts.different, counts.only_in_1, counts.only_in_2, counts.same, counts.pending
    )
}

pub fn ui_status_summary(
    ui: &mut egui::Ui,
    counts: Option<&RowCounts>,
    scans: [Option<&ScanSummary>; 2],
    compare_time: Option<Duration>,
    tolerance_on: bool,
) {
    let sides = [Side::Folder1, Side::Folder2];
    ui.horizontal(|ui| {
        if let Some(counts) = counts {
            ui.label(counts_text(counts, tolerance_on));
            ui.separator();
        }
        let mut times: Vec<String> = sides
            .iter()
            .zip(scans)
            .filter_map(|(side, scan)| Some(format!("Scan {side}: {:.1?}", scan?.elapsed)))
            .collect();
        times.extend(compare_time.map(|time| format!("Compare: {time:.1?}")));
        if !times.is_empty() {
            ui.label(times.join(", "));
        }
        let skipped: usize = scans.iter().flatten().map(|scan| scan.skipped.len()).sum();
        if skipped > 0 {
            ui.separator();
            let response = ui
                .link(format!("{skipped} skipped"))
                .on_hover_text("Log the skipped entries with their reasons");
            if response.clicked() {
                for (side, scan) in sides.iter().zip(scans) {
                    for entry in scan.iter().flat_map(|scan| &scan.skipped) {
                        log::warn!("{side}: skipped {}: {}", entry.path.display(), entry.reason);
                    }
                }
            }
        }
    });
}

pub fn ui_tool_message(ui: &mut egui::Ui, message: Option<&ToolMessage>) {
    let Some(message) = message else {
        return;
    };
    ui.horizontal(|ui| {
        if let Some(status) = &message.status {
            ui.label(status);
        }
        if let Some(warning) = &message.warning {
            ui.colored_label(ui.visuals().warn_fg_color, warning);
        }
        if let Some(error) = &message.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    });
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fs, path::Path, sync::Arc};

    use tempfile::tempdir;
    use zcommon::kinds::KindTable;
    use zdiff_folder::{comparison::PairState, filter::PathFilter, fs::FileSystemModel};

    use super::{RowCounts, count_rows, counts_text};
    use crate::folder::fs_tree::{DiffState, FileSystemView, VisibleRowTwoFolderDiff};
    use crate::folder::tree_filter::{DiffFilter, StateFilter, arrange_two_folder_rows};
    use crate::folder::tree_sort::TreeSort;
    use crate::folder::tree_tolerance::{SizeDelta, Tolerance};

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
            row("pending", true, DiffState::Pending(0, 0)),
            row("pending/p.txt", false, DiffState::Pending(0, 0)),
            row("removed.txt", false, DiffState::OnlyInFirst(0)),
        ]
    }

    fn counts(filter: DiffFilter) -> RowCounts {
        count_rows(&arrange_two_folder_rows(
            rows(),
            filter,
            TreeSort::default(),
        ))
    }

    #[test]
    fn counts_files_and_one_sided_folders_of_the_filtered_rows() {
        assert_eq!(
            counts(DiffFilter::default()),
            RowCounts {
                different: 2,
                only_in_1: 2,
                only_in_2: 2,
                same: 2,
                similar: 0,
                pending: 1,
            }
        );
        let diff_only = DiffFilter {
            states: StateFilter::Diff,
            ..Default::default()
        };
        assert_eq!(
            counts(diff_only),
            RowCounts {
                different: 2,
                only_in_1: 2,
                only_in_2: 2,
                same: 0,
                similar: 0,
                pending: 1,
            }
        );
        let files_only = DiffFilter {
            states: StateFilter::Diff,
            show_parents: false,
        };
        assert_eq!(
            counts(files_only),
            RowCounts {
                different: 2,
                only_in_1: 1,
                only_in_2: 1,
                same: 0,
                similar: 0,
                pending: 1,
            }
        );
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

    #[test]
    fn counts_similar_files_with_tolerance_on_and_none_with_it_off() {
        let (left_dir, right_dir) = (tempdir().unwrap(), tempdir().unwrap());
        write_files(
            left_dir.path(),
            &[
                ("same.txt", "same"),
                ("far.txt", "a"),
                ("sub/near.txt", "aaaa"),
                ("sub/near2.txt", "bbbb"),
                ("left_only.txt", "l"),
            ],
        );
        write_files(
            right_dir.path(),
            &[
                ("same.txt", "same"),
                ("far.txt", "aaaaaaaa"),
                ("sub/near.txt", "aaab"),
                ("sub/near2.txt", "bbbbb"),
            ],
        );
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let states = HashMap::from([
            ("same.txt", PairState::Same),
            ("far.txt", PairState::Different),
            ("sub/near.txt", PairState::Different),
            ("sub/near2.txt", PairState::Different),
        ]);
        let on = Tolerance {
            enabled: true,
            max_size_delta: SizeDelta::Bytes(1),
            ..Tolerance::default()
        };
        let counts = |tolerance: &Tolerance| {
            count_rows(
                &FileSystemView::build_two_folder_diff_rows(
                    Some(&left),
                    Some(&right),
                    &PathFilter::default(),
                    &KindTable::default(),
                    tolerance,
                    |rel_path, _| (states.get(rel_path).copied(), None),
                    |_, _| None,
                )
                .unwrap(),
            )
        };

        assert_eq!(
            counts(&on),
            RowCounts {
                different: 1,
                only_in_1: 1,
                only_in_2: 0,
                same: 1,
                similar: 2,
                pending: 0,
            }
        );
        let off = Tolerance {
            enabled: false,
            ..on
        };
        assert_eq!(
            counts(&off),
            RowCounts {
                different: 3,
                only_in_1: 1,
                only_in_2: 0,
                same: 1,
                similar: 0,
                pending: 0,
            }
        );
    }

    #[test]
    fn the_similar_count_is_shown_only_with_tolerance_on() {
        let counts = RowCounts {
            different: 1,
            only_in_1: 2,
            only_in_2: 3,
            same: 4,
            similar: 5,
            pending: 6,
        };
        assert_eq!(
            counts_text(&counts, true),
            "1 different, 2 only in 1, 3 only in 2, 4 same, 5 similar, 6 pending"
        );
        let off = RowCounts {
            similar: 0,
            ..counts
        };
        assert_eq!(
            counts_text(&off, false),
            "1 different, 2 only in 1, 3 only in 2, 4 same, 6 pending"
        );
    }
}
