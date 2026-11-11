use std::ops::Range;

use crate::{
    cached_file::CachedFile,
    char_diff::ranges_in_span,
    diff_builder::{IsDimmed, IsGhost},
    diff_ir::{DiffOp, DiffResult},
    lexer::{RawTokenTrait, TokenKind},
};

/// Text that exists only on screen: it is not part of the row's file text and must never be copied.
#[derive(Debug, Clone, PartialEq)]
pub struct GhostInsertion {
    /// Byte offset into `RowText::text` where the ghost is displayed. Always `<= text.len()`.
    pub byte_offset: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RowText {
    /// The file's text for this row, without the line terminator. Trailing whitespace is kept.
    pub text: String,
    pub ghosts: Vec<GhostInsertion>,
    /// Real tokens an ignore pattern matched, as byte ranges into `text`.
    pub dimmed: Vec<Range<usize>>,
    /// Changed characters inside real edited tokens, as byte ranges into `text`.
    pub changed_chars: Vec<Range<usize>>,
    /// Real edited tokens that aren't hidden, as byte ranges into `text`.
    pub changed_tokens: Vec<Range<usize>>,
}

/// `changed_chars` is the row's `LineContent::Code::changed_chars`.
pub fn build_row_text<T: RawTokenTrait>(
    tokens: &[(DiffResult, IsDimmed, IsGhost)],
    changed_chars: &[Range<usize>],
    file_source: Option<&CachedFile<T>>,
    file_target: Option<&CachedFile<T>>,
) -> RowText {
    let mut row = RowText {
        text: String::new(),
        ghosts: Vec::new(),
        dimmed: Vec::new(),
        changed_chars: Vec::new(),
        changed_tokens: Vec::new(),
    };

    for (diff_result, is_dimmed, is_ghost) in tokens {
        let Some((file, token_idx)) = resolve_token(diff_result, file_source, file_target) else {
            continue;
        };
        let token = file.tokens[token_idx as usize].as_ref();
        // Terminators are LF, CRLF or a lone CR; the line break is implied by the row itself.
        if token.kind == TokenKind::Newline || token.span.is_empty() {
            continue;
        }
        let str = file.read_content_span(token.span.clone());

        if *is_ghost {
            row.ghosts.push(GhostInsertion {
                byte_offset: row.text.len(),
                text: str.to_string(),
            });
        } else {
            let start = row.text.len();
            row.text.push_str(str);
            if *is_dimmed {
                row.dimmed.push(start..row.text.len());
            }
            // Equal tokens may resolve to the other side's file, where these offsets mean nothing.
            if !matches!(diff_result.operation, DiffOp::Equal(_)) {
                if !diff_result.hide_in_diff {
                    row.changed_tokens.push(start..row.text.len());
                }
                let changed = ranges_in_span(changed_chars, &token.span);
                let to_row = |r: &Range<usize>| {
                    start + r.start - token.span.start..start + r.end - token.span.start
                };
                row.changed_chars.extend(changed.iter().map(to_row));
            }
        }
    }

    row
}

fn resolve_token<'a, T: RawTokenTrait>(
    diff_result: &DiffResult,
    file_source: Option<&'a CachedFile<T>>,
    file_target: Option<&'a CachedFile<T>>,
) -> Option<(&'a CachedFile<T>, u32)> {
    match diff_result.operation {
        DiffOp::Equal(is_source) => Some((
            if is_source {
                file_source?
            } else {
                file_target?
            },
            diff_result.token_source_idx?,
        )),
        DiffOp::Delete => Some((file_source?, diff_result.token_source_idx?)),
        DiffOp::Insert => Some((file_target?, diff_result.token_target_idx?)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicBool};

    use super::*;
    use crate::{
        cached_file::FileMetadata,
        diff_builder::{DiffBuilderOptions, LineContent, RowSides, build_diff_rows},
        diff_ir::DiffIR,
        ignore::IgnoreMask,
        lexer::{Lexer, RawToken},
        myers::{DiffInput, DiffPhases, MyersDiffAlgorithm, myers_diff_path},
        universal_path::UniversalPath,
    };

    fn cached_file(contents: &str) -> CachedFile<RawToken> {
        CachedFile {
            path: UniversalPath::from("test"),
            hash: zcommon::hash::hash_contents(b""),
            contents: contents.to_string(),
            tokens: Lexer::new(contents).parse(),
            metadata: FileMetadata::new(contents),
        }
    }

    fn equal(idx: u32) -> DiffResult {
        DiffResult {
            operation: DiffOp::Equal(true),
            token_source_idx: Some(idx),
            token_target_idx: Some(idx),
            hide_in_diff: false,
        }
    }

    fn ghost_delete(idx: u32) -> DiffResult {
        DiffResult {
            operation: DiffOp::Delete,
            token_source_idx: Some(idx),
            token_target_idx: None,
            hide_in_diff: false,
        }
    }

    /// Every token of `file` as a real (non-ghost) Equal token.
    fn all_real(file: &CachedFile<RawToken>) -> Vec<(DiffResult, IsDimmed, IsGhost)> {
        (0..file.tokens.len() as u32)
            .map(|i| (equal(i), false, false))
            .collect()
    }

    fn build(file: &CachedFile<RawToken>, tokens: &[(DiffResult, IsDimmed, IsGhost)]) -> RowText {
        build_row_text(tokens, &[], Some(file), Some(file))
    }

    #[test]
    fn changed_chars_of_real_edited_tokens_map_into_the_row() {
        // Tokens: "zz"(0) "\n"(1) "ab"(2) " "(3) "cd"(4)
        let file = cached_file("zz\nab cd");
        let tokens = vec![
            (equal(2), false, false),
            (equal(3), false, false),
            (ghost_delete(4), false, true),
            (ghost_delete(4), false, false),
        ];
        let changed = [0..1, 3..4, 7..8];
        let row = build_row_text(&tokens, &changed, Some(&file), Some(&file));
        assert_eq!(row.text, "ab cd");
        assert_eq!(row.changed_chars, [Range { start: 4, end: 5 }]);
    }

    #[test]
    fn changed_tokens_are_the_real_edited_tokens_not_hidden() {
        // Tokens: "zz"(0) "\n"(1) "ab"(2) " "(3) "cd"(4) " "(5) "ef"(6)
        let file = cached_file("zz\nab cd ef");
        let hidden = DiffResult {
            hide_in_diff: true,
            ..ghost_delete(6)
        };
        let tokens = vec![
            (equal(2), false, false),
            (equal(3), false, false),
            (ghost_delete(4), false, false),
            (ghost_delete(0), false, true),
            (equal(5), false, false),
            (hidden, false, false),
        ];
        let row = build_row_text(&tokens, &[], Some(&file), Some(&file));
        assert_eq!(row.text, "ab cd ef");
        assert_eq!(row.changed_tokens, [Range { start: 3, end: 5 }]);
    }

    #[test]
    fn plain_row() {
        let file = cached_file("let x = 1;");
        let row = build(&file, &all_real(&file));
        assert_eq!(row.text, "let x = 1;");
        assert!(row.ghosts.is_empty());
    }

    #[test]
    fn trailing_spaces_and_tabs_are_kept() {
        let file = cached_file("foo \t  ");
        let row = build(&file, &all_real(&file));
        assert_eq!(row.text, "foo \t  ");
    }

    #[test]
    fn lf_terminator_stripped_trailing_whitespace_kept() {
        let file = cached_file("foo  \n");
        let row = build(&file, &all_real(&file));
        assert_eq!(row.text, "foo  ");
    }

    #[test]
    fn crlf_terminator_stripped_trailing_whitespace_kept() {
        let file = cached_file("foo \t\r\n");
        let row = build(&file, &all_real(&file));
        assert_eq!(row.text, "foo \t");
    }

    #[test]
    fn blank_rows_are_empty() {
        for contents in ["", "\n", "\r\n"] {
            let file = cached_file(contents);
            let row = build(&file, &all_real(&file));
            assert_eq!(row.text, "", "contents: {contents:?}");
            assert!(row.ghosts.is_empty());
            assert!(row.dimmed.is_empty());
        }
    }

    #[test]
    fn whitespace_only_row_is_kept() {
        let file = cached_file("   \n");
        let row = build(&file, &all_real(&file));
        assert_eq!(row.text, "   ");
    }

    #[test]
    fn row_of_only_ghosts_has_empty_real_text() {
        let file = cached_file("foo bar\n");
        let tokens: Vec<_> = (0..file.tokens.len() as u32)
            .map(|i| (ghost_delete(i), false, true))
            .collect();
        let row = build(&file, &tokens);
        assert_eq!(row.text, "");
        assert!(row.dimmed.is_empty());
        let ghost_text: Vec<_> = row.ghosts.iter().map(|g| g.text.as_str()).collect();
        assert_eq!(ghost_text, ["foo", " ", "bar"]);
        assert!(row.ghosts.iter().all(|g| g.byte_offset == 0));
    }

    #[test]
    fn ghost_in_the_middle_has_correct_byte_offset() {
        // Tokens: "ab"(0) " "(1) "cd"(2)
        let file = cached_file("ab cd");
        let other = cached_file("XY");
        let tokens = vec![
            (equal(0), false, false),
            (equal(1), false, false),
            (ghost_delete(0), false, true),
            (equal(2), false, false),
        ];
        let row = build_row_text(&tokens, &[], Some(&file), Some(&other));
        assert_eq!(row.text, "ab cd");
        assert_eq!(
            row.ghosts,
            [GhostInsertion {
                byte_offset: 3,
                text: "ab".to_string(),
            }]
        );
    }

    #[test]
    fn several_ghosts_in_one_row() {
        let file = cached_file("a b c");
        let ghosts = cached_file("xx yy");
        let tokens = vec![
            (ghost_delete(0), false, true),
            (equal(0), false, false),
            (equal(1), false, false),
            (ghost_delete(2), false, true),
            (equal(2), false, false),
            (ghost_delete(1), false, true),
            (ghost_delete(2), false, true),
        ];
        let row = build_row_text(&tokens, &[], Some(&file), Some(&ghosts));
        assert_eq!(row.text, "a b");
        let offsets: Vec<_> = row.ghosts.iter().map(|g| g.byte_offset).collect();
        // Ghosts read from the source file here: "a"(0) " "(1) "b"(2).
        assert_eq!(offsets, [0, 2, 3, 3]);
        let texts: Vec<_> = row.ghosts.iter().map(|g| g.text.as_str()).collect();
        assert_eq!(texts, ["a", "b", " ", "b"]);
    }

    #[test]
    fn ghosts_after_all_real_text_stay_in_bounds() {
        // Real text ends in whitespace and ghosts follow it: offsets must not exceed text.len().
        let file = cached_file("foo  ");
        let mut tokens = all_real(&file);
        tokens.push((ghost_delete(0), false, true));
        tokens.push((ghost_delete(0), false, true));
        let row = build(&file, &tokens);
        assert_eq!(row.text, "foo  ");
        assert!(row.ghosts.iter().all(|g| g.byte_offset == row.text.len()));
    }

    #[test]
    fn dimmed_ranges_map_to_real_text_byte_offsets() {
        let file = cached_file("ab cd");
        let other = cached_file("ZZZZ");
        // A ghost before "cd" must shift neither the text nor the dimmed ranges.
        let tokens = vec![
            (equal(0), true, false),
            (equal(1), false, false),
            (ghost_delete(0), true, true),
            (equal(2), true, false),
        ];
        let row = build_row_text(&tokens, &[], Some(&file), Some(&other));
        assert_eq!(row.text, "ab cd");
        assert_eq!(row.dimmed, [0..2, 3..5]);
        for range in &row.dimmed {
            assert!(row.text.is_char_boundary(range.start) && row.text.is_char_boundary(range.end));
        }
    }

    #[test]
    fn multibyte_text_uses_byte_offsets() {
        let file = cached_file("é ü");
        let other = cached_file("g");
        let tokens = vec![
            (equal(0), false, false),
            (ghost_delete(0), false, true),
            (equal(1), false, false),
            (equal(2), false, false),
        ];
        let row = build_row_text(&tokens, &[], Some(&file), Some(&other));
        assert_eq!(row.text, "é ü");
        assert_eq!(row.ghosts[0].byte_offset, "é".len());
    }

    #[test]
    fn missing_file_contributes_nothing() {
        let file = cached_file("foo");
        let row = build_row_text(&all_real(&file), &[], None::<&CachedFile<RawToken>>, None);
        assert_eq!(row.text, "");
    }

    #[test]
    fn rows_from_the_diff_pipeline_split_real_text_from_ghosts() {
        let source = "keep\nold line\nend\n";
        let target = "keep\nend\n";
        let (file_source, file_target) = (cached_file(source), cached_file(target));
        let cmp = |a: &RawToken, b: &RawToken| {
            a.kind == b.kind && source[a.span.clone()] == target[b.span.clone()]
        };
        let path = myers_diff_path(
            MyersDiffAlgorithm::Linear,
            DiffInput {
                source: &file_source.tokens,
                target: &file_target.tokens,
                source_text: source,
                target_text: target,
                ignore: &IgnoreMask::default(),
            },
            cmp,
            DiffPhases::default(),
            &[],
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let diff_ir = DiffIR::new(&path, true, Arc::new(AtomicBool::new(false))).unwrap();
        let rows = build_diff_rows(
            diff_ir,
            RowSides {
                tokens_source: Some(&file_source.tokens),
                tokens_target: Some(&file_target.tokens),
                text_source: source,
                text_target: target,
            },
            &DiffBuilderOptions::default(),
            4,
        );

        let text_of = |content: &LineContent| match content {
            LineContent::Code { tokens, .. } => {
                build_row_text(tokens, &[], Some(&file_source), Some(&file_target))
            }
            other => panic!("expected code row, got {other:?}"),
        };
        let (left, right): (Vec<_>, Vec<_>) = rows
            .iter()
            .filter(|r| {
                matches!(r.left, LineContent::Code { .. })
                    && matches!(r.right, LineContent::Code { .. })
            })
            .map(|r| (text_of(&r.left), text_of(&r.right)))
            .unzip();

        let deleted_row = left
            .iter()
            .position(|l| l.text == "old line")
            .expect("deleted line is real text on the left");
        assert_eq!(right[deleted_row].text, "");
        let ghost_text: String = right[deleted_row]
            .ghosts
            .iter()
            .map(|g| g.text.as_str())
            .collect();
        assert_eq!(ghost_text, "old line");
        assert!(left.iter().all(|l| l.ghosts.is_empty()));
    }
}
