use std::fmt::Write;
use std::sync::{Arc, atomic::AtomicBool};

use zdiff::{
    cached_file::CachedFile,
    diff_builder::{DiffBuilderOptions, DiffRow, LineContent, RowSides, build_diff_rows},
    lexer::RawToken,
};

use super::{
    diff_pane::line_text,
    unified::{UnifiedKind, UnifiedRow, unified_rows},
};
use crate::diff_ctx::{MinimalDiffCtx, finalize_diff_rows};

const CONTEXT_LINES: usize = 3;

pub fn export_diff(diff_ctx: &MinimalDiffCtx) {
    let Some(text) = patch_text(diff_ctx) else {
        log::info!("Export Diff: both sides need a file");
        return;
    };
    if text.is_empty() {
        log::info!("Export Diff: the files have no differences");
        return;
    }
    std::thread::spawn(move || {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("diff.patch")
            .add_filter("Patch", &["patch", "diff"])
            .add_filter("Text", &["txt"])
            .save_file()
        else {
            return;
        };
        match std::fs::write(&path, text) {
            Ok(()) => log::info!("Exported the diff to {}", path.display()),
            Err(e) => log::error!("Export Diff: can not write {}: {e}", path.display()),
        }
    });
}

pub fn patch_text(diff_ctx: &MinimalDiffCtx) -> Option<String> {
    let (Some(source), Some(target)) = (&diff_ctx.input.file_1, &diff_ctx.input.file_2) else {
        return None;
    };
    let options = DiffBuilderOptions {
        ghost_rows: false,
        ..diff_ctx.input.options.clone()
    };
    let rows = build_diff_rows(
        (*diff_ctx.diff_ir).clone(),
        RowSides {
            tokens_source: Some(&source.tokens),
            tokens_target: Some(&target.tokens),
            text_source: &source.contents,
            text_target: &target.contents,
        },
        &options,
        source.metadata.num_lines().max(target.metadata.num_lines()),
    );
    Some(unified_diff_text(rows, [source, target]))
}

#[derive(Clone, Copy, PartialEq)]
enum Mark {
    Context,
    Delete,
    Insert,
}

struct HunkLine {
    mark: Mark,
    lines: [Option<usize>; 2],
}

// A one-sided context row is a change the diff options hide; a patch still needs it.
fn hunk_line(row: &UnifiedRow) -> HunkLine {
    let lines = [row.left_line, row.right_line];
    let mark = match (row.kind, lines) {
        (UnifiedKind::Delete, _) | (UnifiedKind::Context, [Some(_), None]) => Mark::Delete,
        (UnifiedKind::Insert, _) | (UnifiedKind::Context, [None, Some(_)]) => Mark::Insert,
        _ => Mark::Context,
    };
    HunkLine { mark, lines }
}

pub fn unified_diff_text(full_rows: Vec<DiffRow>, files: [&CachedFile<RawToken>; 2]) -> String {
    assert!(
        !full_rows
            .iter()
            .any(|row| matches!(row.left, LineContent::Collapsed)),
        "the hunks are cut from uncollapsed rows"
    );
    let options = DiffBuilderOptions {
        diff_only_with_extra_rows: Some(CONTEXT_LINES),
        ..Default::default()
    };
    let (rows, _, _) = finalize_diff_rows(full_rows, &options, &Arc::new(AtomicBool::new(false)))
        .expect("an unset cancel flag never cancels");

    let mut out = String::new();
    let mut last_line = [0; 2];
    for hunk in unified_rows(&rows).split(|u| u.kind == UnifiedKind::Collapsed) {
        let hunk: Vec<HunkLine> = hunk.iter().map(hunk_line).collect();
        if hunk.iter().any(|l| l.mark != Mark::Context) {
            if out.is_empty() {
                writeln!(out, "--- {}\n+++ {}", files[0].path, files[1].path).unwrap();
            }
            write_hunk(&mut out, &hunk, files, last_line);
        }
        for line in &hunk {
            for (last, side_line) in last_line.iter_mut().zip(line.lines) {
                *last = side_line.unwrap_or(*last);
            }
        }
    }
    out
}

fn write_hunk(
    out: &mut String,
    hunk: &[HunkLine],
    files: [&CachedFile<RawToken>; 2],
    last_line_before: [usize; 2],
) {
    let range = |side: usize| {
        let mut lines = hunk.iter().filter_map(|l| l.lines[side]);
        match lines.next() {
            Some(first) => format!("{first},{}", 1 + lines.count()),
            None => format!("{},0", last_line_before[side]),
        }
    };
    writeln!(out, "@@ -{} +{} @@", range(0), range(1)).unwrap();
    for line in hunk {
        let (prefix, side) = match line.mark {
            Mark::Context => (' ', 0),
            Mark::Delete => ('-', 0),
            Mark::Insert => ('+', 1),
        };
        let file = files[side];
        let number = line.lines[side].expect("a hunk line has a line on its side");
        let text = line_text(&file.contents, &file.metadata.line_starts, number - 1)
            .expect("row line numbers are inside their file");
        writeln!(out, "{prefix}{text}").unwrap();
        if number == file.metadata.num_lines() && !file.contents.ends_with('\n') {
            out.push_str("\\ No newline at end of file\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use zdiff::{
        cached_file::FileMetadata, diff_builder::DiffBuilderOptions, universal_path::UniversalPath,
    };

    use super::*;
    use crate::{
        diff_ctx::{DiffProcessor, UpdateDiffRowsInput},
        ui_egui::copy_harness::CopyHarness,
    };

    fn cached_file(path: &str, contents: &str) -> CachedFile<RawToken> {
        CachedFile {
            path: UniversalPath::from(path),
            hash: zcommon::hash::hash_contents(contents.as_bytes()),
            contents: contents.to_string(),
            tokens: Vec::new(),
            metadata: FileMetadata::new(contents),
        }
    }

    fn export_with(source: &str, target: &str, options: DiffBuilderOptions) -> String {
        let [with_ghosts, without_ghosts] = [true, false].map(|ghost_rows| {
            let options = DiffBuilderOptions {
                ghost_rows,
                ..options.clone()
            };
            let rows = CopyHarness::new(source, target, &options).rows().to_vec();
            unified_diff_text(
                rows,
                [&cached_file("a.txt", source), &cached_file("b.txt", target)],
            )
        });
        assert_eq!(with_ghosts, without_ghosts);
        without_ghosts
    }

    fn export(source: &str, target: &str) -> String {
        export_with(source, target, DiffBuilderOptions::default())
    }

    fn ignoring_whitespace() -> DiffBuilderOptions {
        let mut options = DiffBuilderOptions::default();
        options.ignore.whitespace = true;
        options
    }

    fn numbered(lines: std::ops::RangeInclusive<usize>) -> String {
        lines.map(|i| format!("{i}\n")).collect()
    }

    #[test]
    fn identical_files_export_nothing() {
        assert_eq!(export("a\nb\n", "a\nb\n"), "");
    }

    #[test]
    fn a_changed_line_is_one_hunk_with_three_lines_of_context() {
        let source = numbered(1..=9);
        let target = source.replace("5\n", "X\n");
        assert_eq!(
            export(&source, &target),
            "--- a.txt\n+++ b.txt\n@@ -2,7 +2,7 @@\n 2\n 3\n 4\n-5\n+X\n 6\n 7\n 8\n"
        );
    }

    #[test]
    fn distant_changes_are_separate_hunks() {
        let source = numbered(1..=20);
        let target = source.replace("\n2\n", "\nB\n").replace("\n19\n", "\nY\n");
        assert_eq!(
            export(&source, &target),
            "--- a.txt\n+++ b.txt\n\
             @@ -1,5 +1,5 @@\n 1\n-2\n+B\n 3\n 4\n 5\n\
             @@ -16,5 +16,5 @@\n 16\n 17\n 18\n-19\n+Y\n 20\n"
        );
    }

    #[test]
    fn a_run_of_changes_lists_deleted_lines_before_inserted_ones() {
        assert_eq!(
            export("a\nb\nc\nd\n", "a\nX\nY\nd\n"),
            "--- a.txt\n+++ b.txt\n@@ -1,4 +1,4 @@\n a\n-b\n-c\n+X\n+Y\n d\n"
        );
    }

    #[test]
    fn lines_added_to_an_empty_file_start_at_line_zero_on_the_empty_side() {
        assert_eq!(
            export("", "a\nb\n"),
            "--- a.txt\n+++ b.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n"
        );
        assert_eq!(
            export("a\nb\n", ""),
            "--- a.txt\n+++ b.txt\n@@ -1,2 +0,0 @@\n-a\n-b\n"
        );
    }

    #[test]
    fn a_last_line_without_a_newline_is_marked() {
        assert_eq!(
            export("a\nb", "a\nc"),
            "--- a.txt\n+++ b.txt\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+c\n\\ No newline at end of file\n"
        );
    }

    #[test]
    fn a_line_split_in_two_is_its_deleted_line_then_both_inserted_lines() {
        assert_eq!(
            export("a b\n", "a\nb\n"),
            "--- a.txt\n+++ b.txt\n@@ -1,1 +1,2 @@\n-a b\n+a\n+b\n"
        );
    }

    #[test]
    fn two_lines_joined_into_one_are_both_deleted_lines_then_the_inserted_line() {
        assert_eq!(
            export("a\nb\n", "a b\n"),
            "--- a.txt\n+++ b.txt\n@@ -1,2 +1,1 @@\n-a\n-b\n+a b\n"
        );
    }

    #[test]
    fn an_ignored_whitespace_change_alone_exports_nothing() {
        assert_eq!(
            export_with("a\nb \nc\n", "a\nb\nc\n", ignoring_whitespace()),
            ""
        );
    }

    #[test]
    fn an_ignored_whitespace_change_next_to_a_real_one_is_context() {
        assert_eq!(
            export_with("a\nb \nc\n", "a\nb\nX\n", ignoring_whitespace()),
            "--- a.txt\n+++ b.txt\n@@ -1,3 +1,3 @@\n a\n b \n-c\n+X\n"
        );
    }

    fn settled_diff(source: &str, target: &str, options: DiffBuilderOptions) -> MinimalDiffCtx {
        let file = |path: &str, contents: &str| {
            Some(Arc::new(CachedFile::from_contents(
                UniversalPath::from(path),
                contents.to_owned(),
            )))
        };
        let mut processor = DiffProcessor::default();
        processor.request_update(UpdateDiffRowsInput {
            file_1: file("a.txt", source),
            file_2: file("b.txt", target),
            options,
            ..Default::default()
        });
        let start = Instant::now();
        loop {
            processor.update();
            if !processor.is_in_progress() {
                return processor
                    .get_minimal_diff_ctx()
                    .expect("the diff completed");
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "the diff never completed"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn the_patch_ignores_how_the_pane_collapses_and_pads_its_rows() {
        let source = numbered(1..=9);
        let target = source.replace("5\n", "X\n");
        let pane_options = DiffBuilderOptions {
            diff_only_with_extra_rows: Some(0),
            ghost_rows: true,
            ..Default::default()
        };
        assert_eq!(
            patch_text(&settled_diff(&source, &target, pane_options)).as_deref(),
            Some("--- a.txt\n+++ b.txt\n@@ -2,7 +2,7 @@\n 2\n 3\n 4\n-5\n+X\n 6\n 7\n 8\n")
        );
    }

    #[test]
    fn crlf_line_ends_are_not_part_of_the_exported_text() {
        assert_eq!(
            export("a\r\nb\r\n", "a\r\nc\r\n"),
            "--- a.txt\n+++ b.txt\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n"
        );
    }
}
