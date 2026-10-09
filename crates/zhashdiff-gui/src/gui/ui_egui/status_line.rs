use eframe::egui;
use std::time::Duration;
use zhashdiff::scan::SkippedEntry;

use crate::ui_egui::{
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
    pub pending: usize,
}

/// Files, plus folders on one side only; a folder on both sides only aggregates its files.
pub fn count_rows(rows: &[VisibleRowTwoFolderDiff]) -> RowCounts {
    let mut counts = RowCounts::default();
    for row in rows.iter().filter(|row| !row.rel_path.is_empty()) {
        let count = match row.diff_state {
            DiffState::OnlyInFirst(..) => &mut counts.only_in_1,
            DiffState::OnlyInSecond(..) => &mut counts.only_in_2,
            _ if row.is_dir => continue,
            DiffState::Different(..) | DiffState::Partial(..) => &mut counts.different,
            DiffState::Same(..) => &mut counts.same,
            DiffState::Pending(..) => &mut counts.pending,
        };
        *count += 1;
    }
    counts
}

pub fn ui_status_summary(
    ui: &mut egui::Ui,
    rows: Option<&[VisibleRowTwoFolderDiff]>,
    scans: [Option<&ScanSummary>; 2],
    compare_time: Option<Duration>,
) {
    let sides = [Side::Folder1, Side::Folder2];
    ui.horizontal(|ui| {
        if let Some(rows) = rows {
            let counts = count_rows(rows);
            ui.label(format!(
                "{} different, {} only in 1, {} only in 2, {} same, {} pending",
                counts.different, counts.only_in_1, counts.only_in_2, counts.same, counts.pending
            ));
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

#[cfg(test)]
mod tests {
    use super::{RowCounts, count_rows};
    use crate::ui_egui::fs_tree::{DiffState, VisibleRowTwoFolderDiff};
    use crate::ui_egui::tree_filter::{DiffFilter, arrange_two_folder_rows};
    use crate::ui_egui::tree_sort::TreeSort;

    fn row(rel_path: &str, is_dir: bool, diff_state: DiffState) -> VisibleRowTwoFolderDiff {
        VisibleRowTwoFolderDiff {
            rel_path: rel_path.to_owned(),
            is_dir,
            depth: rel_path.split('/').filter(|c| !c.is_empty()).count() as _,
            diff_state,
            file_kinds: [None, None],
            blueprints: [false, false],
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
            row("c.txt", false, DiffState::Partial(0, 0)),
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
                pending: 1,
            }
        );
        let diff_only = DiffFilter {
            diff_only: true,
            ..Default::default()
        };
        assert_eq!(
            counts(diff_only),
            RowCounts {
                different: 2,
                only_in_1: 2,
                only_in_2: 2,
                same: 0,
                pending: 1,
            }
        );
        let files_only = DiffFilter {
            diff_only: true,
            show_parents: false,
        };
        assert_eq!(
            counts(files_only),
            RowCounts {
                different: 2,
                only_in_1: 1,
                only_in_2: 1,
                same: 0,
                pending: 1,
            }
        );
    }
}
