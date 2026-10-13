use std::{
    cell::RefCell,
    ops::Range,
    time::{Duration, Instant},
};

use crate::{
    lexer::RawTokenTrait,
    myers::{Step, path_steps, split_lines},
};

/// A source and a target region as token ranges, and the 0-based lines they cover.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceSpan {
    pub source: Range<usize>,
    pub target: Range<usize>,
    pub source_lines: Range<usize>,
    pub target_lines: Range<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenHunk {
    pub span: TraceSpan,
    pub equal: usize,
    pub deleted: usize,
    pub inserted: usize,
}

/// A deleted token and the inserted token it was character-diffed with. Ranges are bytes of
/// each side's text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CharPair {
    pub source_token: usize,
    pub target_token: usize,
    pub source: Vec<Range<usize>>,
    pub target: Vec<Range<usize>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PhaseTimes {
    pub scope: Duration,
    pub line: Duration,
    pub token: Duration,
    pub character: Duration,
}

/// What each phase produced, in file coordinates. Times add up over all stretches and scopes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhaseTrace {
    pub scope_pairs: Vec<TraceSpan>,
    pub line_hunks: Vec<TraceSpan>,
    pub token_hunks: Vec<TokenHunk>,
    pub char_pairs: Vec<CharPair>,
    pub times: PhaseTimes,
}

pub(crate) struct TraceMark {
    scope_pairs: usize,
    line_hunks: usize,
    token_hunks: usize,
}

impl PhaseTrace {
    pub(crate) fn mark(&self) -> TraceMark {
        TraceMark {
            scope_pairs: self.scope_pairs.len(),
            line_hunks: self.line_hunks.len(),
            token_hunks: self.token_hunks.len(),
        }
    }

    /// Moves the spans recorded since `mark` out of a phase's local token range. `source` and
    /// `target` map a local token position, up to and including the local length, to a file one.
    pub(crate) fn map_since(
        &mut self,
        mark: &TraceMark,
        source: impl Fn(usize) -> usize,
        target: impl Fn(usize) -> usize,
    ) {
        let map = |r: &Range<usize>, f: &dyn Fn(usize) -> usize| {
            if r.is_empty() {
                f(r.start)..f(r.start)
            } else {
                f(r.start)..f(r.end - 1) + 1
            }
        };
        let spans = self.scope_pairs[mark.scope_pairs..]
            .iter_mut()
            .chain(&mut self.line_hunks[mark.line_hunks..])
            .chain(
                self.token_hunks[mark.token_hunks..]
                    .iter_mut()
                    .map(|h| &mut h.span),
            );
        for span in spans {
            span.source = map(&span.source, &source);
            span.target = map(&span.target, &target);
        }
    }

    pub(crate) fn fill_lines<T: RawTokenTrait>(&mut self, source: &[T], target: &[T]) {
        let (source_lines, target_lines) = (split_lines(source), split_lines(target));
        let lines = |lines: &[Range<usize>], r: &Range<usize>| {
            let line_of = |i: usize| lines.partition_point(|l| l.end <= i);
            if r.is_empty() {
                line_of(r.start)..line_of(r.start)
            } else {
                line_of(r.start)..line_of(r.end - 1) + 1
            }
        };
        let spans = self
            .scope_pairs
            .iter_mut()
            .chain(&mut self.line_hunks)
            .chain(self.token_hunks.iter_mut().map(|h| &mut h.span));
        for span in spans {
            span.source_lines = lines(&source_lines, &span.source);
            span.target_lines = lines(&target_lines, &span.target);
        }
    }
}

/// Runs a phase over a local token range and maps what it recorded with `source` and `target`
/// (see `PhaseTrace::map_since`).
pub(crate) fn run_local<R>(
    trace: Option<&RefCell<PhaseTrace>>,
    source: impl Fn(usize) -> usize,
    target: impl Fn(usize) -> usize,
    phase: impl FnOnce() -> Option<R>,
) -> Option<R> {
    let mark = trace.map(|trace| trace.borrow().mark());
    let result = phase()?;
    if let (Some(trace), Some(mark)) = (trace, mark) {
        trace.borrow_mut().map_since(&mark, source, target);
    }
    Some(result)
}

pub(crate) fn add_time(
    trace: Option<&RefCell<PhaseTrace>>,
    start: Option<Instant>,
    phase: impl FnOnce(&mut PhaseTimes) -> &mut Duration,
) {
    if let (Some(trace), Some(start)) = (trace, start) {
        *phase(&mut trace.borrow_mut().times) += start.elapsed();
    }
}

pub(crate) fn span(source: Range<usize>, target: Range<usize>) -> TraceSpan {
    TraceSpan {
        source,
        target,
        ..Default::default()
    }
}

/// The operations of `path`, which passes through both corners of the hunk, inside the hunk.
pub(crate) fn token_hunk(
    path: &[(i32, i32)],
    source: Range<usize>,
    target: Range<usize>,
) -> TokenHunk {
    let corner = |x: usize, y: usize| {
        let i = path.partition_point(|&(px, py)| ((px + py) as usize) < x + y);
        assert_eq!(
            path.get(i),
            Some(&(x as i32, y as i32)),
            "path misses a hunk corner"
        );
        i
    };
    let (from, to) = (
        corner(source.start, target.start),
        corner(source.end, target.end),
    );
    let mut hunk = TokenHunk {
        span: span(source, target),
        ..Default::default()
    };
    for step in path_steps(&path[from..=to]) {
        match step {
            Step::Equal => hunk.equal += 1,
            Step::Delete => hunk.deleted += 1,
            Step::Insert => hunk.inserted += 1,
        }
    }
    hunk
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicBool};

    use super::*;
    use crate::{
        char_diff::character_diff_traced,
        ignore::IgnoreOptions,
        lexer::{Lexer, RawToken},
        myers::{
            DiffPhases, MyersDiffAlgorithm, MyersPath, Pin, myers_diff_path, myers_diff_path_traced,
        },
    };

    const ALGORITHMS: [MyersDiffAlgorithm; 3] = [
        MyersDiffAlgorithm::Trace,
        MyersDiffAlgorithm::Linear,
        MyersDiffAlgorithm::LinearMT,
    ];
    const ALL: DiffPhases = DiffPhases {
        scope: true,
        line: true,
        token: true,
        character: true,
    };
    const SOURCE: &str = "fn a() {\n    x = foo123;\n}\ntop();\nfn gone() {\n    g();\n}\nmid();\nfn c() {\n    one();\n}\n";
    const TARGET: &str = "fn a() {\n    x = foo124;\n}\ntop();\nmid2();\nfn c() {\n    uno();\n}\n";

    fn lex(text: &str) -> Vec<RawToken> {
        Lexer::new(text).parse()
    }

    fn run(
        algorithm: MyersDiffAlgorithm,
        source: &str,
        target: &str,
        phases: DiffPhases,
        pins: &[Pin],
        trace: Option<&mut PhaseTrace>,
    ) -> MyersPath {
        let (ts, tt) = (lex(source), lex(target));
        let cmp = |a: &RawToken, b: &RawToken| {
            a.kind == b.kind && source[a.span.clone()] == target[b.span.clone()]
        };
        let ignore = IgnoreOptions::default().mask(&ts, source, &tt, target);
        let cancel = Arc::new(AtomicBool::new(false));
        match trace {
            Some(trace) => {
                let path = myers_diff_path_traced(
                    algorithm,
                    &ts,
                    &tt,
                    source,
                    target,
                    cmp,
                    &ignore,
                    phases,
                    pins,
                    cancel.clone(),
                    Some(&mut *trace),
                )
                .expect("not cancelled");
                if phases.character {
                    character_diff_traced(
                        algorithm,
                        &path,
                        &ts,
                        &tt,
                        source,
                        target,
                        &ignore,
                        cancel,
                        Some(trace),
                    )
                    .expect("not cancelled");
                }
                path
            }
            None => myers_diff_path(
                algorithm, &ts, &tt, source, target, cmp, &ignore, phases, pins, cancel,
            )
            .expect("not cancelled"),
        }
    }

    fn traced(source: &str, target: &str, phases: DiffPhases, pins: &[Pin]) -> PhaseTrace {
        let mut trace = PhaseTrace::default();
        run(
            MyersDiffAlgorithm::Linear,
            source,
            target,
            phases,
            pins,
            Some(&mut trace),
        );
        trace
    }

    fn lines(spans: &[TraceSpan]) -> Vec<(Range<usize>, Range<usize>)> {
        spans
            .iter()
            .map(|s| (s.source_lines.clone(), s.target_lines.clone()))
            .collect()
    }

    fn token_at(text: &str, needle: &str) -> usize {
        let start = text.find(needle).unwrap();
        lex(text)
            .iter()
            .position(|t| t.span.start == start)
            .unwrap()
    }

    #[test]
    fn every_enabled_phase_records_its_results_in_file_coordinates() {
        for algorithm in ALGORITHMS {
            let mut trace = PhaseTrace::default();
            run(algorithm, SOURCE, TARGET, ALL, &[], Some(&mut trace));

            assert_eq!(
                lines(&trace.scope_pairs),
                vec![(0..3, 0..3), (8..11, 5..8)],
                "{algorithm:?}"
            );
            let hunks = vec![(1..2, 1..2), (7..8, 4..5), (9..10, 6..7)];
            assert_eq!(lines(&trace.line_hunks), hunks, "{algorithm:?}");
            let token_spans: Vec<_> = trace.token_hunks.iter().map(|h| h.span.clone()).collect();
            assert_eq!(token_spans, trace.line_hunks, "{algorithm:?}");
            assert!(
                trace
                    .token_hunks
                    .iter()
                    .all(|h| h.deleted > 0 && h.inserted > 0 && h.equal > 0),
                "{algorithm:?}: {:?}",
                trace.token_hunks
            );
            let foo = (
                SOURCE.find("foo123").unwrap() + 5,
                TARGET.find("foo124").unwrap() + 5,
            );
            let foo_pair = CharPair {
                source_token: token_at(SOURCE, "foo123"),
                target_token: token_at(TARGET, "foo124"),
                source: vec![foo.0..foo.0 + 1],
                target: vec![foo.1..foo.1 + 1],
            };
            assert!(
                trace.char_pairs.contains(&foo_pair),
                "{algorithm:?}: {:?}",
                trace.char_pairs
            );
            let times = trace.times;
            assert!(times.scope > Duration::ZERO, "{algorithm:?}: {times:?}");
            assert!(times.line > Duration::ZERO, "{algorithm:?}: {times:?}");
            assert!(times.token > Duration::ZERO, "{algorithm:?}: {times:?}");
            assert!(times.character > Duration::ZERO, "{algorithm:?}: {times:?}");
        }
    }

    #[test]
    fn disabled_phases_record_nothing() {
        let line_only = traced(
            SOURCE,
            TARGET,
            DiffPhases {
                token: false,
                ..DiffPhases::default()
            },
            &[],
        );
        assert!(!line_only.line_hunks.is_empty());
        assert!(line_only.scope_pairs.is_empty());
        assert!(line_only.token_hunks.is_empty());
        assert!(line_only.char_pairs.is_empty());
        assert_eq!(
            line_only.times.scope + line_only.times.token,
            Duration::ZERO
        );

        let token_only = traced(
            SOURCE,
            TARGET,
            DiffPhases {
                line: false,
                ..DiffPhases::default()
            },
            &[],
        );
        assert!(token_only.line_hunks.is_empty());
        assert_eq!(token_only.times.line, Duration::ZERO);
        let whole = TraceSpan {
            source: 0..lex(SOURCE).len(),
            target: 0..lex(TARGET).len(),
            source_lines: 0..11,
            target_lines: 0..8,
        };
        let spans: Vec<_> = token_only
            .token_hunks
            .iter()
            .map(|h| h.span.clone())
            .collect();
        assert_eq!(spans, vec![whole]);
    }

    #[test]
    fn hunks_after_a_pin_are_in_file_lines() {
        let pins = [Pin {
            source: 2,
            target: 2,
        }];
        let trace = traced("a\nb\nc\nd\n", "a\nB\nc\nD\n", DiffPhases::default(), &pins);
        assert_eq!(lines(&trace.line_hunks), vec![(1..2, 1..2), (3..4, 3..4)]);
        assert_eq!(
            lines(
                &trace
                    .token_hunks
                    .iter()
                    .map(|h| h.span.clone())
                    .collect::<Vec<_>>()
            ),
            lines(&trace.line_hunks)
        );
    }

    #[test]
    fn tracing_does_not_change_the_path() {
        let phase_sets = [
            DiffPhases::default(),
            ALL,
            DiffPhases { line: false, ..ALL },
            DiffPhases {
                token: false,
                character: false,
                ..ALL
            },
        ];
        let pins = [Pin {
            source: 3,
            target: 3,
        }];
        for algorithm in ALGORITHMS {
            for phases in phase_sets {
                for pins in [&[][..], &pins] {
                    let mut trace = PhaseTrace::default();
                    assert_eq!(
                        run(algorithm, SOURCE, TARGET, phases, pins, Some(&mut trace)),
                        run(algorithm, SOURCE, TARGET, phases, pins, None),
                        "{algorithm:?} {phases:?} {pins:?}"
                    );
                }
            }
        }
    }
}
