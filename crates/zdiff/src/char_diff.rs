use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use crate::{
    ignore::IgnoreMask,
    lexer::{RawTokenTrait, TokenKind},
    myers::{MyersDiffAlgorithm, Step, path_steps, raw_diff_path},
    trace::{CharPair, PhaseTrace},
};

/// Changed bytes of each side, as byte ranges into that side's text. Sorted; each range lies
/// inside one token.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CharRanges {
    pub source: Vec<Range<usize>>,
    pub target: Vec<Range<usize>>,
}

/// The ranges of `ranges` (sorted, as in `CharRanges`) inside `span`.
pub fn ranges_in_span<'a>(ranges: &'a [Range<usize>], span: &Range<usize>) -> &'a [Range<usize>] {
    let start = ranges.partition_point(|r| r.start < span.start);
    let len = ranges[start..].partition_point(|r| r.end <= span.end);
    &ranges[start..start + len]
}

/// Character phase, run by the caller on the path from `myers_diff_path` as it needs the texts.
/// In each run of edits the n-th deleted token pairs with the n-th inserted one; same-kind pairs
/// get their characters diffed. Ignored tokens and line breaks are skipped. None when cancelled.
pub fn character_diff<T: RawTokenTrait>(
    algorithm: MyersDiffAlgorithm,
    path: &[(i32, i32)],
    source: &[T],
    target: &[T],
    source_text: &str,
    target_text: &str,
    ignore: &IgnoreMask,
    cancel_flag: Arc<AtomicBool>,
) -> Option<CharRanges> {
    character_diff_traced(
        algorithm,
        path,
        source,
        target,
        source_text,
        target_text,
        ignore,
        cancel_flag,
        None,
    )
}

pub fn character_diff_traced<T: RawTokenTrait>(
    algorithm: MyersDiffAlgorithm,
    path: &[(i32, i32)],
    source: &[T],
    target: &[T],
    source_text: &str,
    target_text: &str,
    ignore: &IgnoreMask,
    cancel_flag: Arc<AtomicBool>,
    mut trace: Option<&mut PhaseTrace>,
) -> Option<CharRanges> {
    let start = trace.is_some().then(Instant::now);
    if cancel_flag.load(Ordering::Relaxed) {
        return None;
    }
    let mut ranges = CharRanges::default();
    let (mut deleted, mut inserted) = (Vec::new(), Vec::new());
    let (mut x, mut y) = (0, 0);
    for step in path_steps(path).into_iter().chain([Step::Equal]) {
        match step {
            Step::Delete => {
                deleted.push(x);
                x += 1;
                continue;
            }
            Step::Insert => {
                inserted.push(y);
                y += 1;
                continue;
            }
            Step::Equal => (x, y) = (x + 1, y + 1),
        }
        for (&i, &j) in deleted.iter().zip(&inserted) {
            let (s, t) = (source[i].as_ref(), target[j].as_ref());
            let ignored = !ignore.is_empty() && (ignore.source[i] || ignore.target[j]);
            let empty = s.span.is_empty() || t.span.is_empty();
            if s.kind != t.kind || s.kind == TokenKind::Newline || ignored || empty {
                continue;
            }
            let (s_text, t_text) = (&source_text[s.span.clone()], &target_text[t.span.clone()]);
            let (s_changed, t_changed) =
                changed_chars(algorithm, s_text, t_text, cancel_flag.clone())?;
            let offset = |r: Range<usize>, start: usize| start + r.start..start + r.end;
            let s_start = s.span.start;
            let t_start = t.span.start;
            let s_changed: Vec<_> = s_changed.into_iter().map(|r| offset(r, s_start)).collect();
            let t_changed: Vec<_> = t_changed.into_iter().map(|r| offset(r, t_start)).collect();
            ranges.source.extend_from_slice(&s_changed);
            ranges.target.extend_from_slice(&t_changed);
            if let Some(trace) = trace.as_deref_mut() {
                trace.char_pairs.push(CharPair {
                    source_token: i,
                    target_token: j,
                    source: s_changed,
                    target: t_changed,
                });
            }
        }
        deleted.clear();
        inserted.clear();
    }
    if let (Some(trace), Some(start)) = (trace, start) {
        trace.times.character += start.elapsed();
    }
    Some(ranges)
}

/// Byte ranges of the deleted chars of `source` and the inserted chars of `target`, adjacent ones
/// merged.
fn changed_chars(
    algorithm: MyersDiffAlgorithm,
    source: &str,
    target: &str,
    cancel_flag: Arc<AtomicBool>,
) -> Option<(Vec<Range<usize>>, Vec<Range<usize>>)> {
    let source_chars: Vec<(usize, char)> = source.char_indices().collect();
    let target_chars: Vec<(usize, char)> = target.char_indices().collect();
    let cmp = |a: &(usize, char), b: &(usize, char)| a.1 == b.1;
    let path = raw_diff_path(
        algorithm,
        &source_chars,
        &target_chars,
        cmp,
        cancel_flag.clone(),
    )?;
    if cancel_flag.load(Ordering::Relaxed) {
        return None;
    }
    let push = |ranges: &mut Vec<Range<usize>>, (start, c): (usize, char)| {
        let end = start + c.len_utf8();
        match ranges.last_mut() {
            Some(last) if last.end == start => last.end = end,
            _ => ranges.push(start..end),
        }
    };
    let (mut source_changed, mut target_changed) = (Vec::new(), Vec::new());
    let (mut x, mut y) = (0, 0);
    for step in path_steps(&path) {
        match step {
            Step::Delete => {
                push(&mut source_changed, source_chars[x]);
                x += 1;
            }
            Step::Insert => {
                push(&mut target_changed, target_chars[y]);
                y += 1;
            }
            Step::Equal => (x, y) = (x + 1, y + 1),
        }
    }
    Some((source_changed, target_changed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diff_builder::{DiffBuilderOptions, LineContent, build_diff_rows},
        diff_ir::DiffIR,
        ignore::IgnoreOptions,
        lexer::{LexerDefault, RawToken},
        myers::{DiffPhases, myers_diff_path},
    };

    const S1: &str = "a\nx = foo123;\n";
    const S2: &str = "a\nx = foo124;\n";

    type RowChanges = (Vec<Range<usize>>, Vec<Range<usize>>);

    fn lex(s: &str) -> Vec<RawToken> {
        LexerDefault::<RawToken>::new(s).collect()
    }

    fn path(
        s1: &str,
        s2: &str,
        t1: &[RawToken],
        t2: &[RawToken],
        mask: &IgnoreMask,
    ) -> Vec<(i32, i32)> {
        let cmp = |a: &RawToken, b: &RawToken| {
            a.kind == b.kind && s1[a.span.clone()] == s2[b.span.clone()]
        };
        let phases = DiffPhases {
            character: true,
            ..Default::default()
        };
        myers_diff_path(
            MyersDiffAlgorithm::Linear,
            t1,
            t2,
            s1,
            s2,
            cmp,
            mask,
            phases,
            &[],
            Arc::new(AtomicBool::new(false)),
        )
        .expect("not cancelled")
    }

    fn diff(s1: &str, s2: &str, ignore: &IgnoreOptions, cancel: bool) -> Option<CharRanges> {
        let (t1, t2) = (lex(s1), lex(s2));
        let mask = ignore.mask(&t1, s1, &t2, s2);
        let path = path(s1, s2, &t1, &t2, &mask);
        let cancel_flag = Arc::new(AtomicBool::new(cancel));
        let algorithm = MyersDiffAlgorithm::Linear;
        character_diff(algorithm, &path, &t1, &t2, s1, s2, &mask, cancel_flag)
    }

    fn changed_chars(content: &LineContent) -> Vec<Range<usize>> {
        match content {
            LineContent::Code { changed_chars, .. } => changed_chars.clone(),
            _ => Vec::new(),
        }
    }

    fn row_changes(char_ranges: Option<CharRanges>) -> Vec<RowChanges> {
        let (t1, t2) = (lex(S1), lex(S2));
        let path = path(S1, S2, &t1, &t2, &IgnoreMask::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let mut ir = DiffIR::new(&path, true, cancel).expect("not cancelled");
        if let Some(char_ranges) = char_ranges {
            ir.char_ranges = char_ranges;
        }
        let options = DiffBuilderOptions::default();
        build_diff_rows(ir, Some(&t1[..]), Some(&t2[..]), S1, S2, &options, 2)
            .iter()
            .map(|row| (changed_chars(&row.left), changed_chars(&row.right)))
            .collect()
    }

    #[test]
    fn foo123_vs_foo124_marks_only_the_differing_digit() {
        let ranges = diff(S1, S2, &IgnoreOptions::default(), false).unwrap();
        assert_eq!(
            ranges,
            CharRanges {
                source: vec![11..12],
                target: vec![11..12],
            }
        );
        assert_eq!(
            row_changes(Some(ranges)),
            [(vec![], vec![]), (vec![11..12], vec![11..12])]
        );
    }

    #[test]
    fn rows_carry_no_ranges_with_character_off() {
        assert_eq!(row_changes(None), [(vec![], vec![]), (vec![], vec![])]);
    }

    #[test]
    fn ranges_are_byte_ranges_of_changed_chars() {
        let (s1, s2) = ("s = \"\u{e4}1\"\n", "s = \"\u{e4}22\"\n");
        let ranges = diff(s1, s2, &IgnoreOptions::default(), false);
        assert_eq!(
            ranges.unwrap(),
            CharRanges {
                source: vec![7..8],
                target: vec![7..9],
            }
        );
    }

    #[test]
    fn tokens_of_different_kinds_are_not_diffed() {
        let ranges = diff("x = 12;\n", "x = a2;\n", &IgnoreOptions::default(), false);
        assert_eq!(ranges.unwrap(), CharRanges::default());
    }

    #[test]
    fn ignored_tokens_are_not_diffed() {
        let (s1, s2) = ("a // x1\n", "a // x2\n");
        let shown = diff(s1, s2, &IgnoreOptions::default(), false).unwrap();
        assert_eq!(shown.source, [6..7]);
        let ignore = IgnoreOptions {
            comments: true,
            ..Default::default()
        };
        assert_eq!(diff(s1, s2, &ignore, false).unwrap(), CharRanges::default());
    }

    #[test]
    fn cancelled_returns_none() {
        assert_eq!(diff(S1, S2, &IgnoreOptions::default(), true), None);
    }

    #[test]
    fn ranges_in_span_selects_the_ranges_inside() {
        let ranges = [1..2, 4..5, 6..8, 9..10];
        assert_eq!(ranges_in_span(&ranges, &(3..8)), [4..5, 6..8]);
        assert!(ranges_in_span(&ranges, &(2..4)).is_empty());
    }
}
