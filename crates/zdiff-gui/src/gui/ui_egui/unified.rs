//! Unified text mode: the side-by-side rows as one column, deleted lines above inserted ones.

use zdiff::{
    diff_builder::{DiffRow, LineContent},
    diff_ir::DiffOp,
};

use super::active_side::ActiveSide;
use crate::diff_ctx::ScrollSpan;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnifiedKind {
    Context,
    Delete,
    Insert,
    Collapsed,
}

/// One line of the unified column. Its content is `rows[row]` on `side`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnifiedRow {
    pub kind: UnifiedKind,
    pub row: usize,
    pub side: ActiveSide,
    pub left_line: Option<usize>,
    pub right_line: Option<usize>,
}

// Same rule as the conflict spans, so a changed run is one conflict block.
fn row_has_change(row: &DiffRow) -> bool {
    let has_change = |content: &LineContent| match content {
        LineContent::Code { tokens, .. } => tokens
            .iter()
            .any(|(res, _, _)| !res.hide_in_diff && !matches!(res.operation, DiffOp::Equal(_))),
        _ => false,
    };
    has_change(&row.left) || has_change(&row.right)
}

// Ghost-only and void sides have no line.
fn line_of(content: &LineContent) -> Option<usize> {
    match content {
        LineContent::Code { line_num, .. } => usize::try_from(*line_num).ok().filter(|n| *n > 0),
        LineContent::Void | LineContent::Collapsed => None,
    }
}

pub fn unified_rows(rows: &[DiffRow]) -> Vec<UnifiedRow> {
    let mut out = Vec::with_capacity(rows.len());
    let mut run_start = 0;
    let flush_run = |out: &mut Vec<UnifiedRow>, run: std::ops::Range<usize>| {
        for (kind, side) in [
            (UnifiedKind::Delete, ActiveSide::Left),
            (UnifiedKind::Insert, ActiveSide::Right),
        ] {
            for row in run.clone() {
                let content = match side {
                    ActiveSide::Left => &rows[row].left,
                    ActiveSide::Right => &rows[row].right,
                };
                let Some(line) = line_of(content) else {
                    continue;
                };
                out.push(UnifiedRow {
                    kind,
                    row,
                    side,
                    left_line: (side == ActiveSide::Left).then_some(line),
                    right_line: (side == ActiveSide::Right).then_some(line),
                });
            }
        }
    };
    for (i, row) in rows.iter().enumerate() {
        if row_has_change(row) {
            continue;
        }
        flush_run(&mut out, run_start..i);
        run_start = i + 1;
        if matches!(row.left, LineContent::Collapsed) {
            out.push(UnifiedRow {
                kind: UnifiedKind::Collapsed,
                row: i,
                side: ActiveSide::Left,
                left_line: None,
                right_line: None,
            });
            continue;
        }
        let (left_line, right_line) = (line_of(&row.left), line_of(&row.right));
        let side = if left_line.is_some() || right_line.is_none() {
            ActiveSide::Left
        } else {
            ActiveSide::Right
        };
        if left_line.is_none() && right_line.is_none() {
            continue;
        }
        out.push(UnifiedRow {
            kind: UnifiedKind::Context,
            row: i,
            side,
            left_line,
            right_line,
        });
    }
    flush_run(&mut out, run_start..rows.len());
    out
}

pub fn unified_index_of_row(unified: &[UnifiedRow], row: usize) -> Option<usize> {
    unified
        .iter()
        .position(|u| u.row == row)
        .or_else(|| unified.iter().position(|u| u.row > row))
}

// A block scrolls to its first unified row; a row with a side (find, goto) to that side's line.
pub fn unified_scroll_index(unified: &[UnifiedRow], span: &ScrollSpan) -> Option<usize> {
    if let Some(end) = span.maybe_end {
        let rows = span.start..=end;
        return unified
            .iter()
            .position(|u| rows.contains(&u.row))
            .or_else(|| unified_index_of_row(unified, span.start));
    }
    span.side
        .and_then(|side| {
            unified
                .iter()
                .position(|u| u.row == span.start && u.side == side)
        })
        .or_else(|| unified_index_of_row(unified, span.start))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_ctx::{precompute_diff_spans, precompute_file_rows};
    use crate::ui_egui::copy_harness::CopyHarness;
    use zdiff::diff_builder::DiffBuilderOptions;

    fn rows_of(source: &str, target: &str, ghost_rows: bool) -> Vec<DiffRow> {
        let options = DiffBuilderOptions {
            ghost_rows,
            ..Default::default()
        };
        CopyHarness::new(source, target, &options).rows().to_vec()
    }

    fn summary(unified: &[UnifiedRow]) -> Vec<(UnifiedKind, Option<usize>, Option<usize>)> {
        unified
            .iter()
            .map(|u| (u.kind, u.left_line, u.right_line))
            .collect()
    }

    use UnifiedKind::{Context, Delete, Insert};

    #[test]
    fn a_modified_line_is_its_deleted_line_then_its_inserted_line() {
        for ghost_rows in [true, false] {
            let rows = rows_of("a\nb\nc\n", "a\nB\nc\n", ghost_rows);
            assert_eq!(
                summary(&unified_rows(&rows)),
                vec![
                    (Context, Some(1), Some(1)),
                    (Delete, Some(2), None),
                    (Insert, None, Some(2)),
                    (Context, Some(3), Some(3)),
                ],
                "ghost_rows: {ghost_rows}"
            );
        }
    }

    #[test]
    fn a_run_of_changed_rows_is_all_its_deleted_lines_then_all_its_inserted_lines() {
        let rows = rows_of("a\nb\nc\nd\n", "a\nX\nY\nd\n", true);
        assert_eq!(
            summary(&unified_rows(&rows)),
            vec![
                (Context, Some(1), Some(1)),
                (Delete, Some(2), None),
                (Delete, Some(3), None),
                (Insert, None, Some(2)),
                (Insert, None, Some(3)),
                (Context, Some(4), Some(4)),
            ]
        );
    }

    #[test]
    fn an_insert_only_run_has_no_deleted_lines_with_or_without_ghost_rows() {
        for ghost_rows in [true, false] {
            let rows = rows_of("a\nd\n", "a\nb\nc\nd\n", ghost_rows);
            assert_eq!(
                summary(&unified_rows(&rows)),
                vec![
                    (Context, Some(1), Some(1)),
                    (Insert, None, Some(2)),
                    (Insert, None, Some(3)),
                    (Context, Some(2), Some(4)),
                ],
                "ghost_rows: {ghost_rows}"
            );
        }
    }

    #[test]
    fn a_delete_only_run_has_no_inserted_lines_with_or_without_ghost_rows() {
        for ghost_rows in [true, false] {
            let rows = rows_of("a\nb\nc\nd\n", "a\nd\n", ghost_rows);
            assert_eq!(
                summary(&unified_rows(&rows)),
                vec![
                    (Context, Some(1), Some(1)),
                    (Delete, Some(2), None),
                    (Delete, Some(3), None),
                    (Context, Some(4), Some(2)),
                ],
                "ghost_rows: {ghost_rows}"
            );
        }
    }

    #[test]
    fn identical_files_are_all_context_with_both_line_numbers() {
        let rows = rows_of("a\nb\n", "a\nb\n", true);
        assert_eq!(
            summary(&unified_rows(&rows)),
            vec![(Context, Some(1), Some(1)), (Context, Some(2), Some(2))]
        );
    }

    #[test]
    fn a_collapsed_row_stays_one_row_and_ends_a_run() {
        let mut rows = rows_of("a\nb\nc\n", "a\nB\nc\n", true);
        let collapsed = DiffRow {
            left: LineContent::Collapsed,
            right: LineContent::Collapsed,
        };
        rows.insert(1, collapsed.clone());
        rows.push(collapsed);
        let unified = unified_rows(&rows);
        assert_eq!(
            unified.iter().map(|u| (u.kind, u.row)).collect::<Vec<_>>(),
            vec![
                (Context, 0),
                (UnifiedKind::Collapsed, 1),
                (Delete, 2),
                (Insert, 2),
                (Context, 3),
                (UnifiedKind::Collapsed, 4),
            ]
        );
        assert_eq!(unified[1].left_line, None);
        assert_eq!(unified[1].right_line, None);
    }

    #[test]
    fn rows_keep_their_side_by_side_row_and_content_side() {
        let rows = rows_of("a\nb\n", "a\nB\n", true);
        let unified = unified_rows(&rows);
        let placed: Vec<_> = unified.iter().map(|u| (u.row, u.side)).collect();
        assert_eq!(
            placed,
            vec![
                (0, ActiveSide::Left),
                (1, ActiveSide::Left),
                (1, ActiveSide::Right)
            ]
        );
    }

    #[test]
    fn a_side_by_side_row_maps_to_its_first_unified_row() {
        let rows = rows_of("a\nb\nc\nd\n", "a\nX\nY\nd\n", true);
        let unified = unified_rows(&rows);
        assert_eq!(unified_index_of_row(&unified, 0), Some(0));
        assert_eq!(unified_index_of_row(&unified, 1), Some(1));
        assert_eq!(unified_index_of_row(&unified, 2), Some(2));
        assert_eq!(unified_index_of_row(&unified, 3), Some(5));
        assert_eq!(unified_index_of_row(&unified, 4), None);
    }

    #[test]
    fn an_insert_only_row_maps_to_its_inserted_line() {
        let rows = rows_of("a\nd\n", "a\nb\nd\n", true);
        let unified = unified_rows(&rows);
        assert_eq!(unified_index_of_row(&unified, 1), Some(1));
    }

    fn conflict_targets(rows: &[DiffRow]) -> Vec<Option<usize>> {
        let unified = unified_rows(rows);
        precompute_diff_spans(rows)
            .iter()
            .map(|span| {
                let rows = span.rows();
                let scroll = ScrollSpan {
                    start: *rows.start(),
                    maybe_end: Some(*rows.end()),
                    side: None,
                };
                unified_scroll_index(&unified, &scroll)
            })
            .collect()
    }

    fn goto(rows: &[DiffRow], side: ActiveSide, line: usize) -> Option<usize> {
        let lines = |side| {
            rows.iter()
                .filter_map(|r| match side {
                    ActiveSide::Left => line_of(&r.left),
                    ActiveSide::Right => line_of(&r.right),
                })
                .max()
                .unwrap_or(0)
        };
        let (left, right) =
            precompute_file_rows(rows, lines(ActiveSide::Left), lines(ActiveSide::Right));
        let line_to_row = match side {
            ActiveSide::Left => left,
            ActiveSide::Right => right,
        };
        let scroll = ScrollSpan {
            start: line_to_row[line - 1],
            maybe_end: None,
            side: Some(side),
        };
        unified_scroll_index(&unified_rows(rows), &scroll)
    }

    #[test]
    fn each_conflict_block_scrolls_to_its_first_unified_row() {
        let rows = rows_of("a\nb\nc\nd\ne\nf\n", "a\nX\nY\nd\ne\nF\n", true);
        assert_eq!(conflict_targets(&rows), vec![Some(1), Some(7)]);
    }

    #[test]
    fn a_conflict_block_without_left_lines_scrolls_to_its_inserted_lines() {
        for ghost_rows in [true, false] {
            let rows = rows_of("a\nd\n", "a\nb\nc\nd\n", ghost_rows);
            assert_eq!(
                conflict_targets(&rows),
                vec![Some(1)],
                "ghost_rows: {ghost_rows}"
            );
        }
    }

    #[test]
    fn a_conflict_block_after_a_collapsed_row_counts_the_collapsed_row() {
        let mut rows = rows_of("a\nb\nc\n", "a\nB\nc\n", true);
        rows.insert(
            0,
            DiffRow {
                left: LineContent::Collapsed,
                right: LineContent::Collapsed,
            },
        );
        assert_eq!(conflict_targets(&rows), vec![Some(2)]);
    }

    #[test]
    fn goto_a_line_on_either_side_of_a_changed_row_lands_on_that_sides_unified_row() {
        let rows = rows_of("a\nb\nc\nd\n", "a\nX\nY\nd\n", true);
        assert_eq!(goto(&rows, ActiveSide::Left, 3), Some(2));
        assert_eq!(goto(&rows, ActiveSide::Right, 3), Some(4));
        assert_eq!(goto(&rows, ActiveSide::Left, 2), Some(1));
        assert_eq!(goto(&rows, ActiveSide::Right, 2), Some(3));
    }

    #[test]
    fn goto_a_line_of_an_unchanged_row_lands_on_its_one_unified_row() {
        let rows = rows_of("a\nb\nc\nd\n", "a\nX\nY\nd\n", true);
        assert_eq!(goto(&rows, ActiveSide::Left, 4), Some(5));
        assert_eq!(goto(&rows, ActiveSide::Right, 4), Some(5));
        assert_eq!(goto(&rows, ActiveSide::Right, 1), Some(0));
    }

    #[test]
    fn goto_a_line_in_a_one_sided_run_lands_on_that_line() {
        for ghost_rows in [true, false] {
            let inserted = rows_of("a\nd\n", "a\nb\nc\nd\n", ghost_rows);
            assert_eq!(goto(&inserted, ActiveSide::Right, 3), Some(2));
            let deleted = rows_of("a\nb\nc\nd\n", "a\nd\n", ghost_rows);
            assert_eq!(goto(&deleted, ActiveSide::Left, 3), Some(2));
            assert_eq!(goto(&deleted, ActiveSide::Right, 2), Some(3));
        }
    }

    #[test]
    fn a_find_hit_on_a_row_without_a_line_on_its_side_lands_on_the_rows_first_unified_row() {
        let rows = rows_of("a\nb\nc\n", "a\nc\n", true);
        let scroll = ScrollSpan {
            start: 1,
            maybe_end: None,
            side: Some(ActiveSide::Right),
        };
        assert_eq!(unified_scroll_index(&unified_rows(&rows), &scroll), Some(1));
    }
}
