use super::*;

/// Rows that are not file text get a one-character selectable label holding one of these
/// noncharacters. egui's cross-label copy adds a blank line for any vertical gap between copied
/// labels (and skips empty ones), so without them a Void or ghost-only row would copy as a blank
/// line and consecutive real blank lines would collapse into one. `CopyMarkerPlugin` removes them.
pub(super) const COPY_MARKER_NO_LINE: char = '\u{FDD0}';
pub(super) const COPY_MARKER_BLANK_LINE: char = '\u{FDD1}';

pub(super) fn strip_copy_markers(copied: &str) -> String {
    let (no_line, blank) = (
        COPY_MARKER_NO_LINE.to_string(),
        COPY_MARKER_BLANK_LINE.to_string(),
    );
    copied
        .split('\n')
        .filter(|line| *line != no_line)
        .map(|line| if line == blank { "" } else { line })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(in crate::ui_egui) struct CopyMarkerPlugin;

impl egui::Plugin for CopyMarkerPlugin {
    fn debug_name(&self) -> &'static str {
        "CopyMarkerPlugin"
    }

    fn output_hook(&mut self, output: &mut egui::FullOutput) {
        output
            .platform_output
            .commands
            .retain_mut(|command| match command {
                egui::OutputCommand::CopyText(text)
                    if text.contains([COPY_MARKER_NO_LINE, COPY_MARKER_BLANK_LINE]) =>
                {
                    *text = strip_copy_markers(text);
                    !text.is_empty()
                }
                _ => true,
            });
    }
}

/// Invisible selectable stand-in for a row that holds no file text; see `COPY_MARKER_NO_LINE`.
pub(super) fn copy_marker_galley(ui: &egui::Ui, marker: char) -> Arc<egui::Galley> {
    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
    ui.fonts_mut(|fonts| {
        fonts.layout_no_wrap(marker.to_string(), font_id, egui::Color32::TRANSPARENT)
    })
}

/// `galley` with its rect stretched down to `height`, for selection. egui's cross-label copy adds
/// a blank line when the gap between two copied galleys is over half a row, so a side shorter
/// than its wrapped row has to reach the bottom of its cell. The text itself doesn't move.
pub(super) fn reach_down_to(galley: Arc<egui::Galley>, height: f32) -> Arc<egui::Galley> {
    if galley.rect.height() >= height {
        return galley;
    }
    let mut galley = Arc::unwrap_or_clone(galley);
    galley.rect.max.y = galley.rect.min.y + height;
    Arc::new(galley)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_support::{char_width, collapsed_row};
    

    
    
    use zdiff::{
        diff_builder::{DiffBuilderOptions, DiffRow, LineContent},
        ignore::IgnoreOptions,
    };

    use crate::ui_egui::copy_harness::{CopyHarness, Side};
    #[test]
    fn copy_markers_drop_non_file_rows_and_keep_blank_lines() {
        let (no_line, blank) = (
            COPY_MARKER_NO_LINE.to_string(),
            COPY_MARKER_BLANK_LINE.to_string(),
        );
        let copied = [
            no_line.as_str(),
            "a",
            &no_line,
            &blank,
            &blank,
            "b",
            &no_line,
        ]
        .join("\n");

        assert_eq!(strip_copy_markers(&copied), "a\n\n\nb");
    }
    const SOURCE: &str = "fn main() {\n    let x = 1;\n    let y = 2;\n}\n";
    const TARGET: &str = "fn main() {\n    let x = 1;\n    let z = 3 + 4;\n}\n";

    #[test]
    fn single_row_selection_copies_that_rows_text() {
        let mut harness = CopyHarness::new(SOURCE, TARGET, &DiffBuilderOptions::default());

        let copied = harness.drag_and_copy(Side::Left, (1, 4), (1, 9));
        assert_eq!(copied.as_deref(), Some("let x"));

        let copied = harness.drag_and_copy(Side::Right, (2, 4), (2, 9));
        assert_eq!(copied.as_deref(), Some("let z"));
    }

    // Same on both sides except the last line, so rows 0..=2 carry no ghosts and are identical
    // on both sides.
    const SHARED_SOURCE: &str = "fn main() {\n    foo(bar, baz);\n    let x = 1;\n}\nold\n";
    const SHARED_TARGET: &str = "fn main() {\n    foo(bar, baz);\n    let x = 1;\n}\nnew\n";

    fn shared_harness() -> CopyHarness {
        CopyHarness::new(SHARED_SOURCE, SHARED_TARGET, &DiffBuilderOptions::default())
    }

    #[test]
    fn multi_row_selection_copies_exact_file_text() {
        let mut harness = shared_harness();

        let expected = Some("fn main() {\n    foo(bar, baz);\n    let x = 1;\n}");
        let copied = harness.drag_and_copy(Side::Left, (0, 0), (3, 1));
        assert_eq!(copied.as_deref(), expected);
        let copied = harness.drag_and_copy(Side::Right, (0, 0), (3, 1));
        assert_eq!(copied.as_deref(), expected);
    }

    #[test]
    fn multi_row_selection_keeps_punctuation_spacing() {
        let mut harness = shared_harness();

        let copied = harness.drag_and_copy(Side::Left, (1, 4), (1, 18));
        assert_eq!(copied.as_deref(), Some("foo(bar, baz);"));
    }

    #[test]
    fn partial_first_and_last_row() {
        let mut harness = shared_harness();

        let copied = harness.drag_and_copy(Side::Left, (0, 3), (2, 9));
        assert_eq!(
            copied.as_deref(),
            Some("main() {\n    foo(bar, baz);\n    let x")
        );
    }

    #[test]
    fn upward_drag_copies_same_text_as_downward() {
        let mut harness = shared_harness();

        let down = harness.drag_and_copy(Side::Left, (0, 3), (2, 9));
        let up = harness.drag_and_copy(Side::Left, (2, 9), (0, 3));
        assert_eq!(up, down);
    }

    #[test]
    fn trailing_whitespace_is_kept() {
        let mut harness = CopyHarness::new("a  \nb\n", "a  \nb\n", &DiffBuilderOptions::default());

        let copied = harness.drag_and_copy(Side::Left, (0, 0), (1, 1));
        assert_eq!(copied.as_deref(), Some("a  \nb"));
    }

    #[test]
    fn crlf_rows_are_joined_with_lf() {
        let mut harness =
            CopyHarness::new("a\r\nb\r\n", "a\r\nb\r\n", &DiffBuilderOptions::default());

        let copied = harness.drag_and_copy(Side::Left, (0, 0), (1, 1));
        assert_eq!(copied.as_deref(), Some("a\nb"));
    }

    /// Phantom blank lines from rows without a label are a known gap, handled with the ghost
    /// fidelity work; this only pins that no placeholder text reaches the clipboard.
    fn assert_only_real_text(copied: Option<String>) {
        let copied = copied.expect("copy command emitted");
        let without_newlines: String = copied.chars().filter(|c| *c != '\n').collect();
        assert_eq!(without_newlines, "fn main() {    let x = 1;");
    }

    #[test]
    fn collapsed_row_contributes_nothing() {
        let mut harness = shared_harness();
        let collapsed = collapsed_row();
        harness.set_row(1, collapsed);

        assert_only_real_text(harness.drag_and_copy(Side::Left, (0, 0), (2, 14)));
    }

    #[test]
    fn copy_skips_a_collapsed_block_and_includes_it_once_expanded() {
        use std::{
            collections::BTreeMap,
            sync::{Arc, atomic::AtomicBool},
        };

        use crate::diff_ctx::{expand_rows, finalize_diff_rows};

        // Lines 1 and 12 differ, so one context row each leaves lines 3..=10 collapsed.
        let source: String = (1..=12).map(|n| format!("line {n}\n")).collect();
        let target = source
            .replace("line 1\n", "edit 1\n")
            .replace("line 12\n", "edit 12\n");
        let options = DiffBuilderOptions {
            diff_only_with_extra_rows: Some(1),
            ..Default::default()
        };
        let mut harness = CopyHarness::new(&source, &target, &options);
        let (collapsed, _, blocks) = finalize_diff_rows(
            harness.rows().to_vec(),
            &options,
            &Arc::new(AtomicBool::new(false)),
        )
        .expect("not cancelled");
        let [ref block] = blocks[..] else {
            panic!("expected one collapsed block: {collapsed:#?}");
        };

        harness.set_rows(collapsed.clone());
        let copied = harness
            .drag_and_copy(Side::Left, (1, 0), (3, 7))
            .expect("copy command emitted");
        let without_newlines: String = copied.chars().filter(|c| *c != '\n').collect();
        assert_eq!(without_newlines, "line 2line 11");

        let (expanded, _) = expand_rows(&collapsed, &blocks, &BTreeMap::from([(block.row, 0)]));
        harness.set_rows(expanded);
        let expected: Vec<String> = (2..=11).map(|n| format!("line {n}")).collect();
        let copied = harness.drag_and_copy(Side::Left, (1, 0), (10, 7));
        assert_eq!(copied, Some(expected.join("\n")));
    }

    #[test]
    fn void_row_contributes_nothing() {
        let mut harness = shared_harness();
        let void = DiffRow {
            left: LineContent::Void,
            right: LineContent::Void,
        };
        harness.set_row(1, void);

        assert_only_real_text(harness.drag_and_copy(Side::Left, (0, 0), (2, 14)));
    }

    fn options_variants() -> [(&'static str, DiffBuilderOptions); 2] {
        [
            ("default", DiffBuilderOptions::default()),
            (
                "ignore_whitespace",
                DiffBuilderOptions {
                    ignore: IgnoreOptions {
                        whitespace: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ),
        ]
    }

    #[test]
    fn inline_ghost_tokens_add_no_characters() {
        for (name, options) in options_variants() {
            let mut harness = CopyHarness::new(
                "let x = old(1);\nkeep\n",
                "let x = new(1);\nkeep\n",
                &options,
            );

            let copied = harness.drag_and_copy(Side::Left, (0, 0), (1, 4));
            assert_eq!(
                copied.as_deref(),
                Some("let x = old(1);\nkeep"),
                "left, {name}"
            );
            let copied = harness.drag_and_copy(Side::Right, (0, 0), (1, 4));
            assert_eq!(
                copied.as_deref(),
                Some("let x = new(1);\nkeep"),
                "right, {name}"
            );
        }
    }

    #[test]
    fn inline_ghost_row_copies_partial_real_text_on_either_side_of_the_ghost() {
        let mut harness = CopyHarness::new(
            "let x = old(1);\nkeep\n",
            "let x = new(1);\nkeep\n",
            &DiffBuilderOptions::default(),
        );

        let copied = harness.drag_and_copy(Side::Left, (0, 4), (0, 12));
        assert_eq!(copied.as_deref(), Some("x = old("));
        let copied = harness.drag_and_copy(Side::Left, (0, 9), (1, 2));
        assert_eq!(copied.as_deref(), Some("ld(1);\nke"));
    }

    #[test]
    fn selection_spanning_a_ghost_only_row_joins_the_surrounding_real_lines() {
        for (name, options) in options_variants() {
            let mut harness = CopyHarness::new("a\nb\n", "a\nx\nb\n", &options);

            let copied = harness.drag_and_copy(Side::Left, (0, 0), (2, 1));
            assert_eq!(copied.as_deref(), Some("a\nb"), "left, {name}");
            let copied = harness.drag_and_copy(Side::Right, (0, 0), (2, 1));
            assert_eq!(copied.as_deref(), Some("a\nx\nb"), "right, {name}");
        }
    }

    #[test]
    fn real_blank_line_inside_a_selection_is_preserved() {
        for (name, options) in options_variants() {
            let mut harness = CopyHarness::new("a\n\nb\n", "a\n\nb\n", &options);

            let copied = harness.drag_and_copy(Side::Left, (0, 0), (2, 1));
            assert_eq!(copied.as_deref(), Some("a\n\nb"), "{name}");
        }
    }

    #[test]
    fn hidden_whitespace_tokens_are_copied_in_ignore_whitespace_mode() {
        let options = DiffBuilderOptions {
            ignore: IgnoreOptions {
                whitespace: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut harness = CopyHarness::new("a  b\nc\n", "a b\nc\n", &options);

        let copied = harness.drag_and_copy(Side::Left, (0, 0), (1, 1));
        assert_eq!(copied.as_deref(), Some("a  b\nc"));
        let copied = harness.drag_and_copy(Side::Right, (0, 0), (1, 1));
        assert_eq!(copied.as_deref(), Some("a b\nc"));
    }

    const REAL_SOURCE: &str = "trait Processor {\n    fn run(&self);\n}\n\nstruct Item {\n    id: u64,\n    inner: u8,\n}\n\nfn go() {\n    println!(\"hello\");\n    old_body();\n}\n";
    const REAL_TARGET: &str = "trait NewProcessor {\n    fn run(&self);\n}\n\nstruct Item {\n    id: usize,\n}\n\nfn go() {\n    println!(\"hello world\");\n    new_body();\n    more();\n}\n";

    #[test]
    fn whole_diff_copies_each_files_exact_text() {
        for (name, options) in options_variants() {
            let mut harness = CopyHarness::new(REAL_SOURCE, REAL_TARGET, &options);
            let last = harness.rows().len() - 1;

            let copied = harness.drag_and_copy(Side::Left, (0, 0), (last, 1));
            assert_eq!(
                copied.as_deref(),
                Some(REAL_SOURCE.trim_end()),
                "left, {name}"
            );
            let copied = harness.drag_and_copy(Side::Right, (0, 0), (last, 1));
            assert_eq!(
                copied.as_deref(),
                Some(REAL_TARGET.trim_end()),
                "right, {name}"
            );
        }
    }

    // Each side has one line the other lacks. Ghost rows are off so the missing line is a Void
    // row, not ghost text, and a side's copy therefore holds only its own marker.
    const SIDES_SOURCE: &str = "top\nLEFTONLY\nm1\nm2\nm3\nbottom\n";
    const SIDES_TARGET: &str = "top\nm1\nm2\nm3\nRIGHTONLY\nbottom\n";

    fn sides_harness() -> CopyHarness {
        let options = DiffBuilderOptions {
            ghost_rows: false,
            ..Default::default()
        };
        CopyHarness::new(SIDES_SOURCE, SIDES_TARGET, &options)
    }

    fn code_rows(harness: &CopyHarness, side: Side) -> (usize, usize) {
        let is_code = |row: &DiffRow| {
            matches!(
                match side {
                    Side::Left => &row.left,
                    Side::Right => &row.right,
                },
                LineContent::Code { .. }
            )
        };
        let rows = harness.rows();
        let first = rows.iter().position(is_code).expect("side has code rows");
        let last = rows.iter().rposition(is_code).expect("side has code rows");
        (first, last)
    }

    fn whole_side(harness: &mut CopyHarness, side: Side) -> String {
        let (first, last) = code_rows(harness, side);
        harness
            .drag_and_copy(side, (first, 0), (last, usize::MAX))
            .expect("selection exists")
    }

    fn assert_only(copied: &str, own: &str, other: &str) {
        assert!(copied.contains(own), "missing {own}: {copied:?}");
        assert!(!copied.contains(other), "contains {other}: {copied:?}");
    }

    #[test]
    fn first_press_on_the_inactive_side_starts_a_selection_there() {
        let mut harness = sides_harness();

        // Left is active at startup, so this press is the first one on the inactive side.
        let copied = whole_side(&mut harness, Side::Right);
        assert_only(&copied, "RIGHTONLY", "LEFTONLY");
    }

    #[test]
    fn switching_sides_leaves_only_the_new_sides_selection() {
        let mut harness = sides_harness();

        assert_only(
            &whole_side(&mut harness, Side::Left),
            "LEFTONLY",
            "RIGHTONLY",
        );
        assert_only(
            &whole_side(&mut harness, Side::Right),
            "RIGHTONLY",
            "LEFTONLY",
        );
        assert_only(
            &whole_side(&mut harness, Side::Left),
            "LEFTONLY",
            "RIGHTONLY",
        );
    }

    #[test]
    fn drag_ending_on_the_opposite_side_copies_only_the_active_side() {
        let mut harness = sides_harness();
        let (left_first, left_last) = code_rows(&harness, Side::Left);
        let (right_first, right_last) = code_rows(&harness, Side::Right);

        let copied = harness
            .drag_across_and_copy(
                (Side::Left, (left_first, 0)),
                (Side::Right, (right_last, 3)),
            )
            .expect("selection exists");
        assert!(!copied.contains("RIGHTONLY"), "{copied:?}");

        let copied = harness
            .drag_across_and_copy(
                (Side::Right, (right_first, 0)),
                (Side::Left, (left_last, 3)),
            )
            .expect("selection exists");
        assert!(!copied.contains("LEFTONLY"), "{copied:?}");
    }

    #[test]
    fn press_in_the_blank_part_of_a_row_starts_a_selection() {
        let mut harness = shared_harness();

        let copied = harness.drag_from_blank_and_copy(Side::Left, 0, (2, 5));
        assert_eq!(
            copied.as_deref(),
            Some("fn main() {\n    foo(bar, baz);\n    l")
        );
    }

    #[test]
    fn escape_clears_the_selection() {
        let mut harness = sides_harness();

        whole_side(&mut harness, Side::Left);
        harness.press_escape();
        assert_eq!(harness.copy(), None);
    }

    #[test]
    fn recompute_leaves_a_valid_or_cleared_selection() {
        let mut harness = sides_harness();
        whole_side(&mut harness, Side::Left);

        harness.set_row(
            2,
            DiffRow {
                left: LineContent::Void,
                right: LineContent::Void,
            },
        );
        if let Some(copied) = harness.copy() {
            for line in copied.lines().filter(|l| !l.is_empty()) {
                assert!(
                    SIDES_SOURCE.lines().any(|s| s == line),
                    "garbled line {line:?}"
                );
            }
        }
    }

    #[test]
    fn scrolled_rows_select_by_their_visible_text() {
        let mut harness = CopyHarness::new(SOURCE, TARGET, &DiffBuilderOptions::default());
        // The gutter and the first two columns are out of view.
        harness.set_h_offset(GUTTER_WIDTH + GUTTER_GAP + 15.0);

        let copied = harness.drag_and_copy(Side::Left, (1, 4), (2, 9));
        assert_eq!(copied.as_deref(), Some("let x = 1;\n    let y"));
    }

    #[test]
    fn a_press_on_scrolled_out_text_goes_to_the_row_in_view_there() {
        let mut harness = CopyHarness::new(SOURCE, TARGET, &DiffBuilderOptions::default());
        harness.set_h_offset(GUTTER_WIDTH + GUTTER_GAP + 15.0);

        // The right row's first column is scrolled out over the left side, whose row takes the
        // press: the drag copies left text, never the right row's.
        let copied = harness.drag_across_and_copy((Side::Right, (2, 0)), (Side::Left, (2, 9)));
        assert_eq!(copied.as_deref(), Some(" = 2;"));
    }

    const LONG_LINE: &str = "let words = [alpha, beta, gamma, delta, epsilon, zeta, eta, theta];";

    /// A column that wraps `LONG_LINE` onto at least three rows.
    fn narrow_column() -> f32 {
        GUTTER_WIDTH + GUTTER_GAP + 20.0 * char_width()
    }

    #[test]
    fn a_wrapped_line_copies_as_the_one_file_line() {
        let text = format!("{LONG_LINE}\nnext\n");
        let mut harness = CopyHarness::new(&text, &text, &DiffBuilderOptions::default());
        harness.set_wrap([narrow_column(); 2]);

        let copied = harness.drag_and_copy(Side::Left, (0, 0), (1, 4));
        assert_eq!(copied, Some(format!("{LONG_LINE}\nnext")));
        // From the first visual row of the line into its last one.
        let copied = harness.drag_and_copy(Side::Right, (0, 4), (0, 60));
        assert_eq!(copied.as_deref(), Some(&LONG_LINE[4..60]));
    }

    #[test]
    fn a_short_side_in_a_tall_row_copies_without_a_blank_line() {
        let text = format!("{LONG_LINE}\nnext\n");
        let mut harness = CopyHarness::new(&text, &text, &DiffBuilderOptions::default());
        // The left wraps and makes row 0 tall; the right's copy of it is one line.
        harness.set_wrap([narrow_column(), 1000.0]);

        let copied = harness.drag_and_copy(Side::Right, (0, 0), (1, 4));
        assert_eq!(copied, Some(format!("{LONG_LINE}\nnext")));
    }

    #[test]
    fn a_void_side_in_a_tall_row_copies_nothing() {
        let options = DiffBuilderOptions {
            ghost_rows: false,
            ..Default::default()
        };
        let target = format!("top\n{LONG_LINE}\nbottom\n");
        let mut harness = CopyHarness::new("top\nbottom\n", &target, &options);
        assert!(matches!(harness.rows()[1].left, LineContent::Void));
        harness.set_wrap([narrow_column(); 2]);

        let copied = harness.drag_and_copy(Side::Left, (0, 0), (2, 6));
        assert_eq!(copied.as_deref(), Some("top\nbottom"));
        let copied = harness.drag_and_copy(Side::Right, (0, 0), (2, 6));
        assert_eq!(copied, Some(target.trim_end().to_owned()));
    }

    #[test]
    fn copy_with_wrap_on_gives_each_files_exact_text() {
        for (name, options) in options_variants() {
            let mut harness = CopyHarness::new(REAL_SOURCE, REAL_TARGET, &options);
            harness.set_wrap([GUTTER_WIDTH + GUTTER_GAP + 12.0 * char_width(); 2]);
            let last = harness.rows().len() - 1;

            let copied = harness.drag_and_copy(Side::Left, (0, 0), (last, 1));
            assert_eq!(
                copied.as_deref(),
                Some(REAL_SOURCE.trim_end()),
                "left, {name}"
            );
            let copied = harness.drag_and_copy(Side::Right, (0, 0), (last, 1));
            assert_eq!(
                copied.as_deref(),
                Some(REAL_TARGET.trim_end()),
                "right, {name}"
            );
        }
    }

    #[test]
    fn harness_builds_rows_with_ghost_tokens() {
        let harness = CopyHarness::new(SOURCE, TARGET, &DiffBuilderOptions::default());

        let has_ghost = harness.rows().iter().any(|row| {
            [&row.left, &row.right].into_iter().any(|side| match side {
                LineContent::Code { tokens, .. } => tokens.iter().any(|(_, _, is_ghost)| *is_ghost),
                _ => false,
            })
        });
        assert!(
            has_ghost,
            "expected at least one ghost token in {:?}",
            harness.rows()
        );
    }
}
