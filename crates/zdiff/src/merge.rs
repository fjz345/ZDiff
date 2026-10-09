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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Base,
    Theirs,
    Yours,
    TheirsThenYours,
    YoursThenTheirs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Unresolved,
    Auto(Pick),
    User(Pick),
    Edited(String),
}

#[derive(Debug, Clone, Copy)]
pub struct MergeTexts<'a> {
    pub base: Option<&'a str>,
    pub theirs: &'a str,
    pub yours: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub struct MergeLabels<'a> {
    pub base: &'a str,
    pub theirs: &'a str,
    pub yours: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    chunk: usize,
    before: Resolution,
    after: Resolution,
}

#[derive(Debug, Clone)]
pub struct Merge {
    chunks: Vec<MergeChunk>,
    resolutions: Vec<Resolution>,
    undo: Vec<Change>,
    redo: Vec<Change>,
}

impl Merge {
    pub fn new(chunks: Vec<MergeChunk>) -> Self {
        // Unchanged chunks never take base: without a base its ranges are empty.
        let resolutions = chunks
            .iter()
            .map(|c| match c.kind {
                ChunkKind::TheirsOnly => Resolution::Auto(Pick::Theirs),
                ChunkKind::Conflict => Resolution::Unresolved,
                ChunkKind::Unchanged | ChunkKind::YoursOnly | ChunkKind::BothSame => {
                    Resolution::Auto(Pick::Yours)
                }
            })
            .collect();
        Self {
            chunks,
            resolutions,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn chunks(&self) -> &[MergeChunk] {
        &self.chunks
    }

    pub fn resolutions(&self) -> &[Resolution] {
        &self.resolutions
    }

    pub fn pick(&mut self, chunk: usize, pick: Pick) {
        self.resolve(chunk, Resolution::User(pick));
    }

    pub fn edit(&mut self, chunk: usize, text: String) {
        self.resolve(chunk, Resolution::Edited(text));
    }

    fn resolve(&mut self, chunk: usize, resolution: Resolution) {
        if self.resolutions[chunk] == resolution {
            return;
        }
        let before = std::mem::replace(&mut self.resolutions[chunk], resolution.clone());
        self.redo.clear();
        self.undo.push(Change {
            chunk,
            before,
            after: resolution,
        });
    }

    /// The chunk whose resolution changed, None when there is nothing to undo.
    pub fn undo(&mut self) -> Option<usize> {
        let change = self.undo.pop()?;
        self.resolutions[change.chunk] = change.before.clone();
        let chunk = change.chunk;
        self.redo.push(change);
        Some(chunk)
    }

    pub fn redo(&mut self) -> Option<usize> {
        let change = self.redo.pop()?;
        self.resolutions[change.chunk] = change.after.clone();
        let chunk = change.chunk;
        self.undo.push(change);
        Some(chunk)
    }

    /// Resolved text with markers (yours, base, theirs, like `git merge-file`) for unresolved
    /// conflicts. Every line break takes yours' first one, else theirs', else base's, else `\n`.
    pub fn output(&self, texts: MergeTexts, labels: MergeLabels) -> String {
        let base_text = texts.base.unwrap_or("");
        let (base, theirs, yours) = (lines(base_text), lines(texts.theirs), lines(texts.yours));
        let eol = [texts.yours, texts.theirs, base_text]
            .into_iter()
            .find_map(|text| lines(text).first().and_then(|l| line_break(l)))
            .unwrap_or("\n");

        let mut out = String::new();
        let mut push = |piece: &str| {
            if piece.is_empty() {
                return;
            }
            if !out.is_empty() && !out.ends_with(eol) {
                out.push_str(eol);
            }
            // Per piece, so a lone \r at the end of one and a \n starting the next stay two breaks.
            for line in lines(piece) {
                match line_break(line) {
                    Some(brk) => {
                        out.push_str(&line[..line.len() - brk.len()]);
                        out.push_str(eol);
                    }
                    None => out.push_str(line),
                }
            }
        };
        let marker = |marker: &str, label: &str| match label {
            "" => format!("{marker}\n"),
            label => format!("{marker} {label}\n"),
        };

        for (chunk, resolution) in self.chunks.iter().zip(&self.resolutions) {
            let base = base[chunk.base.clone()].concat();
            let theirs = theirs[chunk.theirs.clone()].concat();
            let yours = yours[chunk.yours.clone()].concat();
            let pieces: Vec<String> = match resolution {
                Resolution::Unresolved => {
                    let mut pieces = vec![marker("<<<<<<<", labels.yours), yours];
                    if texts.base.is_some() {
                        pieces.extend([marker("|||||||", labels.base), base]);
                    }
                    pieces.extend([
                        marker("=======", ""),
                        theirs,
                        marker(">>>>>>>", labels.theirs),
                    ]);
                    pieces
                }
                Resolution::Auto(pick) | Resolution::User(pick) => match pick {
                    Pick::Base => vec![base],
                    Pick::Theirs => vec![theirs],
                    Pick::Yours => vec![yours],
                    Pick::TheirsThenYours => vec![theirs, yours],
                    Pick::YoursThenTheirs => vec![yours, theirs],
                },
                Resolution::Edited(text) => vec![text.clone()],
            };
            for piece in &pieces {
                push(piece);
            }
        }
        out
    }
}

fn line_break(line: &str) -> Option<&'static str> {
    if line.ends_with("\r\n") {
        Some("\r\n")
    } else if line.ends_with('\n') {
        Some("\n")
    } else if line.ends_with('\r') {
        Some("\r")
    } else {
        None
    }
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

    mod resolution {
        use super::*;
        use Pick::*;
        use Resolution::*;

        const LABELS: MergeLabels = MergeLabels {
            base: "base.txt",
            theirs: "theirs.txt",
            yours: "yours.txt",
        };

        const CONFLICT: MergeTexts = MergeTexts {
            base: Some("a\nB\nz\n"),
            theirs: "a\nT\nz\n",
            yours: "a\nY\nz\n",
        };

        fn new_merge(texts: MergeTexts) -> Merge {
            Merge::new(
                merge_chunks(
                    MyersDiffAlgorithm::Trace,
                    texts.base,
                    texts.theirs,
                    texts.yours,
                    Arc::new(AtomicBool::new(false)),
                )
                .expect("not cancelled"),
            )
        }

        fn picked(texts: MergeTexts, chunk: usize, pick: Pick) -> String {
            let mut merge = new_merge(texts);
            merge.pick(chunk, pick);
            merge.output(texts, LABELS)
        }

        fn edited(texts: MergeTexts, chunk: usize, text: &str) -> String {
            let mut merge = new_merge(texts);
            merge.edit(chunk, text.into());
            merge.output(texts, LABELS)
        }

        #[test]
        fn chunks_start_auto_resolved_except_conflicts() {
            let texts = MergeTexts {
                base: Some("a\nb\nc\nd\ne\nf\ng\nh\ni\n"),
                theirs: "a\nT\nc\nD\ne\nF1\ng\nh\ni\n",
                yours: "a\nb\nc\nD\ne\nF2\ng\nY\ni\n",
            };
            let merge = new_merge(texts);
            assert_eq!(
                merge.chunks().iter().map(|c| c.kind).collect::<Vec<_>>(),
                [
                    Unchanged, TheirsOnly, Unchanged, BothSame, Unchanged, Conflict, Unchanged,
                    YoursOnly, Unchanged
                ]
            );
            assert_eq!(
                merge.resolutions(),
                [
                    Auto(Yours),
                    Auto(Theirs),
                    Auto(Yours),
                    Auto(Yours),
                    Auto(Yours),
                    Unresolved,
                    Auto(Yours),
                    Auto(Yours),
                    Auto(Yours),
                ]
            );
            assert_eq!(
                merge.output(texts, LABELS),
                "a\nT\nc\nD\ne\n<<<<<<< yours.txt\nF2\n||||||| base.txt\nf\n=======\nF1\n\
                 >>>>>>> theirs.txt\ng\nY\ni\n"
            );
        }

        #[test]
        fn every_pick_produces_its_text() {
            for (pick, expected) in [
                (Base, "a\nB\nz\n"),
                (Theirs, "a\nT\nz\n"),
                (Yours, "a\nY\nz\n"),
                (TheirsThenYours, "a\nT\nY\nz\n"),
                (YoursThenTheirs, "a\nY\nT\nz\n"),
            ] {
                assert_eq!(picked(CONFLICT, 1, pick), expected, "{pick:?}");
            }
        }

        #[test]
        fn every_pick_without_a_base() {
            let texts = MergeTexts {
                base: None,
                ..CONFLICT
            };
            for (pick, expected) in [
                (Base, "a\nz\n"),
                (Theirs, "a\nT\nz\n"),
                (Yours, "a\nY\nz\n"),
                (TheirsThenYours, "a\nT\nY\nz\n"),
                (YoursThenTheirs, "a\nY\nT\nz\n"),
            ] {
                assert_eq!(picked(texts, 1, pick), expected, "{pick:?}");
            }
        }

        #[test]
        fn picks_record_user_resolved() {
            let mut merge = new_merge(CONFLICT);
            merge.pick(0, Yours);
            merge.pick(1, TheirsThenYours);
            assert_eq!(
                merge.resolutions(),
                [User(Yours), User(TheirsThenYours), Auto(Yours)]
            );
        }

        #[test]
        fn an_edited_chunk_is_used_verbatim() {
            let text = "  x \t\n\n// weird <<<<<<< \n";
            let mut merge = new_merge(CONFLICT);
            merge.edit(1, text.into());
            assert_eq!(merge.resolutions()[1], Edited(text.into()));
            assert_eq!(merge.output(CONFLICT, LABELS), format!("a\n{text}z\n"));
            assert_eq!(
                edited(CONFLICT, 0, "A\n"),
                "A\n<<<<<<< yours.txt\nY\n||||||| base.txt\nB\n=======\nT\n>>>>>>> theirs.txt\nz\n"
            );
            assert_eq!(edited(CONFLICT, 1, ""), "a\nz\n");
        }

        #[test]
        fn unresolved_conflicts_produce_markers_with_labels() {
            assert_eq!(
                new_merge(CONFLICT).output(CONFLICT, LABELS),
                "a\n<<<<<<< yours.txt\nY\n||||||| base.txt\nB\n=======\nT\n>>>>>>> theirs.txt\nz\n"
            );
            let inserted = MergeTexts {
                base: Some("a\nz\n"),
                ..CONFLICT
            };
            assert_eq!(
                new_merge(inserted).output(inserted, LABELS),
                "a\n<<<<<<< yours.txt\nY\n||||||| base.txt\n=======\nT\n>>>>>>> theirs.txt\nz\n"
            );
        }

        #[test]
        fn without_a_base_markers_have_no_base_section() {
            let texts = MergeTexts {
                base: None,
                ..CONFLICT
            };
            assert_eq!(
                new_merge(texts).output(texts, LABELS),
                "a\n<<<<<<< yours.txt\nY\n=======\nT\n>>>>>>> theirs.txt\nz\n"
            );
        }

        #[test]
        fn empty_labels_leave_bare_markers() {
            let labels = MergeLabels {
                base: "",
                theirs: "",
                yours: "",
            };
            assert_eq!(
                new_merge(CONFLICT).output(CONFLICT, labels),
                "a\n<<<<<<<\nY\n|||||||\nB\n=======\nT\n>>>>>>>\nz\n"
            );
        }

        #[test]
        fn line_endings_follow_yours() {
            let crlf_theirs = MergeTexts {
                base: Some("a\nB\nz\n"),
                theirs: "a\r\nT\r\nz\r\n",
                yours: "a\nY\nz\n",
            };
            assert_eq!(picked(crlf_theirs, 0, Theirs), "a\nT\nz\n");

            let crlf = MergeTexts {
                base: Some("a\r\nB\r\nz\r\n"),
                theirs: "a\r\nT\nz\r\n",
                yours: "a\r\nY\r\nz\r\n",
            };
            assert_eq!(picked(crlf, 1, Theirs), "a\r\nT\r\nz\r\n");
            assert_eq!(edited(crlf, 1, "x\ny\n"), "a\r\nx\r\ny\r\nz\r\n");
            assert_eq!(
                new_merge(crlf).output(crlf, LABELS),
                "a\r\n<<<<<<< yours.txt\r\nY\r\n||||||| base.txt\r\nB\r\n=======\r\nT\r\n\
                 >>>>>>> theirs.txt\r\nz\r\n"
            );

            let cr = MergeTexts {
                base: Some("a\nB\nz\n"),
                theirs: "a\nT\nz\n",
                yours: "a\rY\rz\r",
            };
            assert_eq!(edited(cr, 0, "x\r\ny\n"), "x\ry\r");
        }

        #[test]
        fn without_breaks_in_yours_line_endings_follow_theirs_then_base_then_lf() {
            let theirs_crlf = MergeTexts {
                base: Some("B\n"),
                theirs: "T\r\n",
                yours: "Y",
            };
            assert_eq!(
                new_merge(theirs_crlf).output(theirs_crlf, LABELS),
                "<<<<<<< yours.txt\r\nY\r\n||||||| base.txt\r\nB\r\n=======\r\nT\r\n\
                 >>>>>>> theirs.txt\r\n"
            );
            let base_crlf = MergeTexts {
                base: Some("B\r\n"),
                theirs: "T",
                yours: "",
            };
            assert_eq!(picked(base_crlf, 0, TheirsThenYours), "T");
            assert_eq!(picked(base_crlf, 0, Base), "B\r\n");
            assert_eq!(edited(base_crlf, 0, "x\ny"), "x\r\ny");
            let none = MergeTexts {
                base: None,
                theirs: "T",
                yours: "Y",
            };
            assert_eq!(
                new_merge(none).output(none, LABELS),
                "<<<<<<< yours.txt\nY\n=======\nT\n>>>>>>> theirs.txt\n"
            );
        }

        #[test]
        fn a_piece_without_a_final_break_gets_one_only_when_more_follows() {
            let no_final_break = MergeTexts {
                base: Some("a\nB"),
                theirs: "a\nT",
                yours: "a\nY",
            };
            assert_eq!(picked(no_final_break, 1, Theirs), "a\nT");
            assert_eq!(picked(no_final_break, 1, TheirsThenYours), "a\nT\nY");
            assert_eq!(picked(no_final_break, 1, YoursThenTheirs), "a\nY\nT");
            assert_eq!(edited(CONFLICT, 1, "x"), "a\nx\nz\n");
            assert_eq!(
                edited(CONFLICT, 2, "z"),
                "a\n<<<<<<< yours.txt\nY\n||||||| base.txt\nB\n=======\nT\n>>>>>>> theirs.txt\nz"
            );
        }

        #[test]
        fn a_lone_cr_before_a_lf_stays_two_breaks() {
            let texts = MergeTexts {
                base: Some("a\nB\nz\n"),
                theirs: "a\nT\nz\n",
                yours: "a\nY\nz\n",
            };
            let mut merge = new_merge(texts);
            merge.edit(0, "a\r".into());
            merge.edit(1, "\nb\n".into());
            assert_eq!(merge.output(texts, LABELS), "a\n\nb\nz\n");
        }

        fn assert_state(merge: &Merge, resolutions: &[Resolution], output: &str) {
            assert_eq!(merge.resolutions(), resolutions);
            assert_eq!(merge.output(CONFLICT, LABELS), output);
        }

        #[test]
        fn undo_and_redo_restore_exact_states() {
            let s0 = (
                vec![Auto(Yours), Unresolved, Auto(Yours)],
                "a\n<<<<<<< yours.txt\nY\n||||||| base.txt\nB\n=======\nT\n>>>>>>> theirs.txt\nz\n",
            );
            let s1 = (vec![Auto(Yours), User(Theirs), Auto(Yours)], "a\nT\nz\n");
            let s2 = (
                vec![Auto(Yours), Edited("x\n".into()), Auto(Yours)],
                "a\nx\nz\n",
            );
            let s3 = (
                vec![Edited("A\n".into()), Edited("x\n".into()), Auto(Yours)],
                "A\nx\nz\n",
            );

            let mut merge = new_merge(CONFLICT);
            assert_state(&merge, &s0.0, s0.1);
            merge.pick(1, Theirs);
            assert_state(&merge, &s1.0, s1.1);
            merge.edit(1, "x\n".into());
            assert_state(&merge, &s2.0, s2.1);
            merge.edit(0, "A\n".into());
            assert_state(&merge, &s3.0, s3.1);

            assert_eq!(merge.undo(), Some(0));
            assert_state(&merge, &s2.0, s2.1);
            assert_eq!(merge.undo(), Some(1));
            assert_state(&merge, &s1.0, s1.1);
            assert_eq!(merge.undo(), Some(1));
            assert_state(&merge, &s0.0, s0.1);
            assert_eq!(merge.undo(), None);
            assert_state(&merge, &s0.0, s0.1);

            assert_eq!(merge.redo(), Some(1));
            assert_state(&merge, &s1.0, s1.1);
            assert_eq!(merge.redo(), Some(1));
            assert_state(&merge, &s2.0, s2.1);
            assert_eq!(merge.redo(), Some(0));
            assert_state(&merge, &s3.0, s3.1);
            assert_eq!(merge.redo(), None);
            assert_state(&merge, &s3.0, s3.1);
        }

        #[test]
        fn a_new_change_clears_redo() {
            let mut merge = new_merge(CONFLICT);
            merge.pick(1, Theirs);
            merge.pick(1, Yours);
            assert_eq!(merge.undo(), Some(1));
            merge.edit(1, "x\n".into());
            assert_eq!(merge.redo(), None);
            assert_eq!(merge.resolutions()[1], Edited("x\n".into()));
            assert_eq!(merge.undo(), Some(1));
            assert_eq!(merge.resolutions()[1], User(Theirs));
        }

        #[test]
        fn repeating_a_resolution_is_not_recorded() {
            let mut merge = new_merge(CONFLICT);
            merge.pick(1, Theirs);
            merge.pick(1, Theirs);
            merge.edit(2, "z\n".into());
            merge.edit(2, "z\n".into());
            assert_eq!(merge.undo(), Some(2));
            assert_eq!(merge.undo(), Some(1));
            assert_eq!(merge.undo(), None);
        }

        #[test]
        fn picking_an_auto_resolved_side_is_recorded() {
            let mut merge = new_merge(CONFLICT);
            merge.pick(0, Yours);
            assert_eq!(merge.resolutions()[0], User(Yours));
            assert_eq!(merge.undo(), Some(0));
            assert_eq!(merge.resolutions()[0], Auto(Yours));
        }
    }
}
