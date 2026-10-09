use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::myers::{MyersDiffAlgorithm, Step, path_steps, raw_diff_path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    Unchanged,
    TheirsOnly,
    YoursOnly,
    BothSame,
    Conflict,
}

/// Line ranges into `lines()` of each text. Without a base every base range is `0..0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeChunk {
    pub kind: ChunkKind,
    pub base: Range<usize>,
    pub theirs: Range<usize>,
    pub yours: Range<usize>,
}

/// Lines with their line break, split like the lexer's Newline tokens (`\r\n`, `\n`, lone `\r`).
pub fn lines(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let (mut start, mut i) = (0, 0);
    while i < bytes.len() {
        let end = match bytes[i] {
            b'\n' => i + 1,
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => i + 2,
            b'\r' => i + 1,
            _ => {
                i += 1;
                continue;
            }
        };
        lines.push(&text[start..end]);
        (start, i) = (end, end);
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Line-level diff3. Lines compare exactly. Without a base, theirs is diffed against yours and
/// every difference is a conflict. None when cancelled.
pub fn merge_chunks(
    algorithm: MyersDiffAlgorithm,
    base: Option<&str>,
    theirs: &str,
    yours: &str,
    cancel_flag: Arc<AtomicBool>,
) -> Option<Vec<MergeChunk>> {
    let (theirs, yours) = (lines(theirs), lines(yours));
    let Some(base) = base else {
        // Theirs stands in for the base, so every difference comes out as a yours-only change.
        let identity: Vec<_> = (0..theirs.len()).map(Some).collect();
        let yours_match = base_matches(algorithm, &theirs, &yours, &cancel_flag)?;
        let chunks = walk(&theirs, &theirs, &yours, &identity, &yours_match);
        return Some(
            chunks
                .into_iter()
                .map(|c| MergeChunk {
                    kind: match c.kind {
                        ChunkKind::Unchanged => ChunkKind::Unchanged,
                        _ => ChunkKind::Conflict,
                    },
                    base: 0..0,
                    ..c
                })
                .collect(),
        );
    };
    let base = lines(base);
    let theirs_match = base_matches(algorithm, &base, &theirs, &cancel_flag)?;
    let yours_match = base_matches(algorithm, &base, &yours, &cancel_flag)?;
    Some(walk(&base, &theirs, &yours, &theirs_match, &yours_match))
}

/// Per base line, the side line it is paired with by the line diff.
fn base_matches(
    algorithm: MyersDiffAlgorithm,
    base: &[&str],
    side: &[&str],
    cancel_flag: &Arc<AtomicBool>,
) -> Option<Vec<Option<usize>>> {
    let path = raw_diff_path(algorithm, base, side, |a, b| a == b, cancel_flag.clone())?;
    // The inner algorithms don't check the flag on every path (see line_diff).
    if cancel_flag.load(Ordering::Relaxed) {
        return None;
    }
    let mut matches = vec![None; base.len()];
    let (mut x, mut y) = (0, 0);
    for step in path_steps(&path) {
        match step {
            Step::Equal => {
                matches[x] = Some(y);
                (x, y) = (x + 1, y + 1);
            }
            Step::Delete => x += 1,
            Step::Insert => y += 1,
        }
    }
    Some(matches)
}

/// Alternates maximal runs of base lines paired in order on both sides with the regions up to
/// the next base line paired on both sides.
fn walk(
    base: &[&str],
    theirs: &[&str],
    yours: &[&str],
    theirs_match: &[Option<usize>],
    yours_match: &[Option<usize>],
) -> Vec<MergeChunk> {
    let mut chunks = Vec::new();
    let (mut b, mut t, mut y) = (0, 0, 0);
    loop {
        let stable = (b..base.len())
            .take_while(|&i| {
                theirs_match[i] == Some(t + i - b) && yours_match[i] == Some(y + i - b)
            })
            .count();
        if stable > 0 {
            chunks.push(MergeChunk {
                kind: ChunkKind::Unchanged,
                base: b..b + stable,
                theirs: t..t + stable,
                yours: y..y + stable,
            });
            (b, t, y) = (b + stable, t + stable, y + stable);
        }

        let (b_end, t_end, y_end) =
            match (b..base.len()).find_map(|i| Some((i, theirs_match[i]?, yours_match[i]?))) {
                Some(sync) => sync,
                None => (base.len(), theirs.len(), yours.len()),
            };
        if (b, t, y) == (b_end, t_end, y_end) {
            break;
        }
        let (base_lines, theirs_lines, yours_lines) =
            (&base[b..b_end], &theirs[t..t_end], &yours[y..y_end]);
        let kind = match (theirs_lines != base_lines, yours_lines != base_lines) {
            (false, false) => ChunkKind::Unchanged,
            (true, false) => ChunkKind::TheirsOnly,
            (false, true) => ChunkKind::YoursOnly,
            (true, true) if theirs_lines == yours_lines => ChunkKind::BothSame,
            (true, true) => ChunkKind::Conflict,
        };
        chunks.push(MergeChunk {
            kind,
            base: b..b_end,
            theirs: t..t_end,
            yours: y..y_end,
        });
        (b, t, y) = (b_end, t_end, y_end);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{LexerDefault, RawToken};
    use crate::myers::split_lines;

    const ALGORITHMS: [MyersDiffAlgorithm; 3] = [
        MyersDiffAlgorithm::Trace,
        MyersDiffAlgorithm::Linear,
        MyersDiffAlgorithm::LinearMT,
    ];

    type Rendered = (ChunkKind, String, String, String);

    fn merge(base: Option<&str>, theirs: &str, yours: &str) -> Vec<Rendered> {
        let text = |text: &str, range: &Range<usize>| lines(text)[range.clone()].concat();
        let results = ALGORITHMS.map(|algorithm| {
            merge_chunks(
                algorithm,
                base,
                theirs,
                yours,
                Arc::new(AtomicBool::new(false)),
            )
            .expect("not cancelled")
            .iter()
            .map(|c| {
                (
                    c.kind,
                    text(base.unwrap_or(""), &c.base),
                    text(theirs, &c.theirs),
                    text(yours, &c.yours),
                )
            })
            .collect::<Vec<_>>()
        });
        for (algorithm, result) in ALGORITHMS.iter().zip(&results) {
            assert_eq!(result, &results[0], "{algorithm:?} differs from Trace");
        }
        let [first, ..] = results;
        first
    }

    fn chunk(kind: ChunkKind, base: &str, theirs: &str, yours: &str) -> Rendered {
        (kind, base.into(), theirs.into(), yours.into())
    }

    use ChunkKind::*;

    #[test]
    fn lines_split_like_the_lexer() {
        for text in [
            "",
            "a",
            "a\n",
            "a\nb",
            "a\r\nb\r\n",
            "a\rb\r",
            "\n\n",
            "\r\r\n\n",
            "a\r\n\rb",
        ] {
            let tokens = LexerDefault::<RawToken>::new(text).parse();
            let expected: Vec<&str> = split_lines(&tokens)
                .into_iter()
                .map(|l| &text[tokens[l.start].span.start..tokens[l.end - 1].span.end])
                .collect();
            assert_eq!(lines(text), expected, "{text:?}");
        }
    }

    #[test]
    fn no_changes_is_one_unchanged_chunk() {
        let text = "a\nb\nc\n";
        assert_eq!(
            merge(Some(text), text, text),
            vec![chunk(Unchanged, text, text, text)]
        );
    }

    #[test]
    fn all_empty_has_no_chunks() {
        assert_eq!(merge(Some(""), "", ""), vec![]);
        assert_eq!(merge(None, "", ""), vec![]);
    }

    #[test]
    fn theirs_only_change_is_its_own_chunk() {
        assert_eq!(
            merge(Some("a\nb\nc\n"), "a\nB\nc\n", "a\nb\nc\n"),
            vec![
                chunk(Unchanged, "a\n", "a\n", "a\n"),
                chunk(TheirsOnly, "b\n", "B\n", "b\n"),
                chunk(Unchanged, "c\n", "c\n", "c\n"),
            ]
        );
    }

    #[test]
    fn yours_only_change_is_its_own_chunk() {
        assert_eq!(
            merge(Some("a\nb\nc\n"), "a\nb\nc\n", "a\nc\n"),
            vec![
                chunk(Unchanged, "a\n", "a\n", "a\n"),
                chunk(YoursOnly, "b\n", "b\n", ""),
                chunk(Unchanged, "c\n", "c\n", "c\n"),
            ]
        );
    }

    #[test]
    fn identical_changes_on_both_sides_are_both_same() {
        assert_eq!(
            merge(Some("a\nb\nc\n"), "a\nX\nY\nc\n", "a\nX\nY\nc\n"),
            vec![
                chunk(Unchanged, "a\n", "a\n", "a\n"),
                chunk(BothSame, "b\n", "X\nY\n", "X\nY\n"),
                chunk(Unchanged, "c\n", "c\n", "c\n"),
            ]
        );
    }

    #[test]
    fn different_changes_on_both_sides_conflict() {
        assert_eq!(
            merge(Some("a\nb\nc\n"), "a\nT\nc\n", "a\nY\nY\nc\n"),
            vec![
                chunk(Unchanged, "a\n", "a\n", "a\n"),
                chunk(Conflict, "b\n", "T\n", "Y\nY\n"),
                chunk(Unchanged, "c\n", "c\n", "c\n"),
            ]
        );
    }

    #[test]
    fn a_change_on_one_side_and_a_delete_on_the_other_conflict() {
        assert_eq!(
            merge(Some("a\nb\nc\n"), "a\nT\nc\n", "a\nc\n"),
            vec![
                chunk(Unchanged, "a\n", "a\n", "a\n"),
                chunk(Conflict, "b\n", "T\n", ""),
                chunk(Unchanged, "c\n", "c\n", "c\n"),
            ]
        );
    }

    #[test]
    fn changes_on_directly_adjacent_lines_of_different_sides_conflict() {
        assert_eq!(
            merge(Some("a\nb\nc\nd\n"), "a\nB\nc\nd\n", "a\nb\nC\nd\n"),
            vec![
                chunk(Unchanged, "a\n", "a\n", "a\n"),
                chunk(Conflict, "b\nc\n", "B\nc\n", "b\nC\n"),
                chunk(Unchanged, "d\n", "d\n", "d\n"),
            ]
        );
    }

    #[test]
    fn changes_separated_by_an_unchanged_line_are_separate_chunks() {
        assert_eq!(
            merge(
                Some("a\nb\nc\nd\ne\n"),
                "a\nB\nc\nd\ne\n",
                "a\nb\nc\nD\ne\n"
            ),
            vec![
                chunk(Unchanged, "a\n", "a\n", "a\n"),
                chunk(TheirsOnly, "b\n", "B\n", "b\n"),
                chunk(Unchanged, "c\n", "c\n", "c\n"),
                chunk(YoursOnly, "d\n", "d\n", "D\n"),
                chunk(Unchanged, "e\n", "e\n", "e\n"),
            ]
        );
    }

    #[test]
    fn insertions_at_start_and_end() {
        assert_eq!(
            merge(Some("a\nb\n"), "T\na\nb\n", "a\nb\nY\n"),
            vec![
                chunk(TheirsOnly, "", "T\n", ""),
                chunk(Unchanged, "a\nb\n", "a\nb\n", "a\nb\n"),
                chunk(YoursOnly, "", "", "Y\n"),
            ]
        );
        assert_eq!(
            merge(Some("a\n"), "S\na\nE\n", "S\na\nX\n"),
            vec![
                chunk(BothSame, "", "S\n", "S\n"),
                chunk(Unchanged, "a\n", "a\n", "a\n"),
                chunk(Conflict, "", "E\n", "X\n"),
            ]
        );
    }

    #[test]
    fn insertions_on_both_sides_at_the_same_place_conflict() {
        assert_eq!(
            merge(Some("a\nb\n"), "a\nT\nb\n", "a\nY\nb\n"),
            vec![
                chunk(Unchanged, "a\n", "a\n", "a\n"),
                chunk(Conflict, "", "T\n", "Y\n"),
                chunk(Unchanged, "b\n", "b\n", "b\n"),
            ]
        );
    }

    #[test]
    fn empty_base() {
        assert_eq!(
            merge(Some(""), "a\nb\n", "a\nb\n"),
            vec![chunk(BothSame, "", "a\nb\n", "a\nb\n")]
        );
        assert_eq!(
            merge(Some(""), "a\nb\n", "a\nc\n"),
            vec![chunk(Conflict, "", "a\nb\n", "a\nc\n")]
        );
        assert_eq!(
            merge(Some(""), "a\n", ""),
            vec![chunk(TheirsOnly, "", "a\n", "")]
        );
        assert_eq!(
            merge(Some(""), "", "a\n"),
            vec![chunk(YoursOnly, "", "", "a\n")]
        );
    }

    #[test]
    fn without_a_base_every_difference_conflicts() {
        assert_eq!(
            merge(None, "a\nb\nc\nd\n", "a\nB\nc\nd\nY\n"),
            vec![
                chunk(Unchanged, "", "a\n", "a\n"),
                chunk(Conflict, "", "b\n", "B\n"),
                chunk(Unchanged, "", "c\nd\n", "c\nd\n"),
                chunk(Conflict, "", "", "Y\n"),
            ]
        );
        assert_eq!(
            merge(None, "T\na\n", "a\n"),
            vec![
                chunk(Conflict, "", "T\n", ""),
                chunk(Unchanged, "", "a\n", "a\n"),
            ]
        );
        assert_eq!(
            merge(None, "a\n", "a\n"),
            vec![chunk(Unchanged, "", "a\n", "a\n")]
        );
    }

    #[test]
    fn without_a_base_base_ranges_are_empty() {
        let chunks = merge_chunks(
            MyersDiffAlgorithm::Linear,
            None,
            "a\nb\n",
            "a\nc\n",
            Arc::new(AtomicBool::new(false)),
        )
        .expect("not cancelled");
        assert!(chunks.iter().all(|c| c.base == (0..0)), "{chunks:?}");
    }

    #[test]
    fn lines_compare_exactly() {
        assert_eq!(
            merge(Some("a\nb\n"), "a\nb \n", "a\n  b\n"),
            vec![
                chunk(Unchanged, "a\n", "a\n", "a\n"),
                chunk(Conflict, "b\n", "b \n", "  b\n"),
            ]
        );
        assert_eq!(
            merge(Some("a\nb\n"), "a\r\nb\n", "a\nb\n"),
            vec![
                chunk(TheirsOnly, "a\n", "a\r\n", "a\n"),
                chunk(Unchanged, "b\n", "b\n", "b\n"),
            ]
        );
        assert_eq!(
            merge(Some("a\n"), "a\r\n", "a"),
            vec![chunk(Conflict, "a\n", "a\r\n", "a")]
        );
    }

    #[test]
    fn a_cancelled_merge_returns_none() {
        for algorithm in ALGORITHMS {
            for base in [Some("a\nb\n"), None] {
                assert_eq!(
                    merge_chunks(
                        algorithm,
                        base,
                        "a\nT\n",
                        "Y\nb\n",
                        Arc::new(AtomicBool::new(true))
                    ),
                    None,
                    "{algorithm:?} {base:?}"
                );
            }
        }
    }
}
