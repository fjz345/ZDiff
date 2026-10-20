use std::{
    cell::RefCell,
    collections::HashMap,
    hash::BuildHasherDefault,
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
    myers::{
        DiffPhases, FxHasher, LineHunk, MyersDiffAlgorithm, MyersPath, Side, Step, hunks_to_path,
        intern_lines, line_pairs_path, path_steps, phases_path, raw_diff_path, slice_mask,
        split_lines,
    },
    trace::{PhaseTrace, add_time, run_local, span},
};

/// Scope phase: Myers over units, each a top-level scope or the text between two scopes. Two
/// units are equal when all their lines are equal under the line key. The finer phases run only
/// inside the hunks of unequal units; with Scope the finest, a hunk's lines are changed whole.
pub(crate) fn scope_path<T, F>(
    algorithm: MyersDiffAlgorithm,
    source: &[T],
    target: &[T],
    source_text: &str,
    target_text: &str,
    cmp: &F,
    ignore: &IgnoreMask,
    phases: DiffPhases,
    cancel_flag: Arc<AtomicBool>,
    trace: Option<&RefCell<PhaseTrace>>,
) -> Option<MyersPath>
where
    T: RawTokenTrait,
    F: Fn(&T, &T) -> bool + Sync,
{
    if cancel_flag.load(Ordering::Relaxed) {
        return None;
    }
    let start = trace.map(|_| Instant::now());
    let (source_units, target_units) = (
        scope_units(source, source_text),
        scope_units(target, target_text),
    );
    let (source_lines, target_lines) = (split_lines(source), split_lines(target));
    let key = |side: Side, t: &T| {
        let t = t.as_ref();
        let text = match side {
            Side::Source => source_text,
            Side::Target => target_text,
        };
        (t.kind, &text.as_bytes()[t.span.clone()])
    };
    let (source_ids, target_ids) = intern_lines(
        source,
        target,
        &source_lines,
        &target_lines,
        ignore,
        key,
        &cancel_flag,
    )?;
    let mut unit_ids: HashMap<&[u32], u32, BuildHasherDefault<FxHasher>> = HashMap::default();
    let mut unit_id = |line_ids| {
        let next = unit_ids.len() as u32;
        *unit_ids.entry(line_ids).or_insert(next)
    };
    let source_unit_ids: Vec<u32> = source_units
        .iter()
        .map(|unit| unit_id(&source_ids[lines_in(&source_lines, unit)]))
        .collect();
    let target_unit_ids: Vec<u32> = target_units
        .iter()
        .map(|unit| unit_id(&target_ids[lines_in(&target_lines, unit)]))
        .collect();
    let unit_path = raw_diff_path(
        algorithm,
        &source_unit_ids,
        &target_unit_ids,
        |a: &u32, b: &u32| a == b,
        cancel_flag.clone(),
    )?;
    if cancel_flag.load(Ordering::Relaxed) {
        return None;
    }
    let hunks = unit_hunks(&unit_path, &source_units, &target_units, source, target);
    add_time(trace, start, |times| &mut times.scope);
    if let Some(trace) = trace {
        let spans = hunks
            .iter()
            .map(|h| span(h.source.clone(), h.target.clone()));
        trace.borrow_mut().scope_hunks.extend(spans);
    }

    let next = DiffPhases {
        scope: false,
        ..phases
    };
    hunks_to_path(source, target, &hunks, cmp, ignore, &cancel_flag, |hunk| {
        let (s, t) = (hunk.source.clone(), hunk.target.clone());
        if !next.line && !next.token {
            return Some(line_pairs_path(&source[s], &target[t], cmp));
        }
        run_local(
            trace,
            |x| s.start + x,
            |y| t.start + y,
            || {
                phases_path(
                    algorithm,
                    &source[s.clone()],
                    &target[t.clone()],
                    source_text,
                    target_text,
                    cmp,
                    &slice_mask(ignore, s.clone(), t.clone()),
                    next,
                    false,
                    cancel_flag.clone(),
                    trace,
                )
            },
        )
    })
}

/// The runs of unequal units in `path` (over the units), as token ranges.
fn unit_hunks<T>(
    path: &[(i32, i32)],
    source_units: &[Range<usize>],
    target_units: &[Range<usize>],
    source: &[T],
    target: &[T],
) -> Vec<LineHunk> {
    let token_start =
        |units: &[Range<usize>], i: usize, len: usize| units.get(i).map_or(len, |u| u.start);
    let mut hunks = Vec::new();
    let (mut x, mut y) = (0, 0);
    let mut open: Option<(usize, usize)> = None;
    for step in path_steps(path).into_iter().chain([Step::Equal]) {
        if step != Step::Equal {
            open.get_or_insert((x, y));
        } else if let Some((x0, y0)) = open.take() {
            hunks.push(LineHunk {
                source: token_start(source_units, x0, source.len())
                    ..token_start(source_units, x, source.len()),
                target: token_start(target_units, y0, target.len())
                    ..token_start(target_units, y, target.len()),
            });
        }
        match step {
            Step::Equal => (x, y) = (x + 1, y + 1),
            Step::Delete => x += 1,
            Step::Insert => y += 1,
        }
    }
    hunks
}

/// The lines (token ranges) inside `unit`, which starts and ends on line boundaries.
fn lines_in(lines: &[Range<usize>], unit: &Range<usize>) -> Range<usize> {
    let first = lines.partition_point(|l| l.start < unit.start);
    let end = lines.partition_point(|l| l.end <= unit.end);
    first..end
}

/// The top-level scopes and the stretches of text between them, covering every token in order.
/// Without scopes, or with unbalanced braces, the whole range is one unit.
pub(crate) fn scope_units<T: RawTokenTrait>(tokens: &[T], text: &str) -> Vec<Range<usize>> {
    let mut units = Vec::new();
    let mut start = 0;
    for scope in find_scopes(tokens, text).unwrap_or_default() {
        if start < scope.start {
            units.push(start..scope.start);
        }
        start = scope.end;
        units.push(scope);
    }
    if start < tokens.len() {
        units.push(start..tokens.len());
    }
    units
}

/// Top-level `{}` scopes as token ranges widened to whole lines, in order; scopes sharing a line
/// are merged. Empty without braces, None when the braces don't balance.
pub fn find_scopes<T: RawTokenTrait>(tokens: &[T], text: &str) -> Option<Vec<Range<usize>>> {
    let lines = split_lines(tokens);
    let line_of = |i: usize| lines.partition_point(|l| l.end <= i);
    let mut scopes: Vec<Range<usize>> = Vec::new();
    let (mut depth, mut open) = (0usize, 0);
    for (i, token) in tokens.iter().enumerate() {
        let token = token.as_ref();
        if token.kind != TokenKind::Symbol {
            continue;
        }
        match &text[token.span.clone()] {
            "{" => {
                if depth == 0 {
                    open = i;
                }
                depth += 1;
            }
            "}" => {
                depth = depth.checked_sub(1)?;
                if depth > 0 {
                    continue;
                }
                let mut first = line_of(open);
                let brace_alone = lines[first]
                    .clone()
                    .all(|j| j == open || tokens[j].as_ref().kind.is_whitespace());
                // A brace alone on its line: key the scope on the header line above it.
                if brace_alone
                    && first > 0
                    && lines[first - 1].start >= scopes.last().map_or(0, |s| s.end)
                {
                    first -= 1;
                }
                let scope = lines[first].start..lines[line_of(i)].end;
                match scopes.last_mut() {
                    Some(last) if last.end > scope.start => last.end = scope.end,
                    _ => scopes.push(scope),
                }
            }
            _ => {}
        }
    }
    (depth == 0).then_some(scopes)
}

/// `find_scopes` as 0-based line ranges; empty when the braces don't balance.
pub fn scope_lines<T: RawTokenTrait>(tokens: &[T], text: &str) -> Vec<Range<usize>> {
    let lines = split_lines(tokens);
    let line_of = |i: usize| lines.partition_point(|l| l.end <= i);
    find_scopes(tokens, text)
        .unwrap_or_default()
        .iter()
        .map(|scope| line_of(scope.start)..line_of(scope.end - 1) + 1)
        .collect()
}

/// The `scopes` (sorted line ranges) holding any of the ascending `changed` lines.
pub fn changed_scopes(
    scopes: &[Range<usize>],
    changed: impl IntoIterator<Item = usize>,
) -> Vec<Range<usize>> {
    let mut scopes = scopes.iter().peekable();
    let mut held = Vec::new();
    for line in changed {
        while scopes.next_if(|s| s.end <= line).is_some() {}
        if let Some(scope) = scopes.next_if(|s| s.start <= line) {
            held.push(scope.clone());
        }
    }
    held
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{
        ignore::IgnoreOptions,
        lexer::{Lexer, RawToken},
        myers::{Pin, Step, myers_diff_path, myers_diff_path_traced, path_steps},
        trace::token_hunk,
    };

    const ALGORITHMS: [MyersDiffAlgorithm; 3] = [
        MyersDiffAlgorithm::Trace,
        MyersDiffAlgorithm::Linear,
        MyersDiffAlgorithm::LinearMT,
    ];

    const SCOPE_PHASES: [DiffPhases; 3] = [
        DiffPhases {
            scope: true,
            line: true,
            token: true,
            character: false,
        },
        DiffPhases {
            scope: true,
            line: true,
            token: false,
            character: false,
        },
        DiffPhases {
            scope: true,
            line: false,
            token: true,
            character: false,
        },
    ];

    const SCOPE_ONLY: DiffPhases = DiffPhases {
        scope: true,
        line: false,
        token: false,
        character: false,
    };

    const ALL_SCOPE_SETS: [DiffPhases; 4] = [
        SCOPE_PHASES[0],
        SCOPE_PHASES[1],
        SCOPE_PHASES[2],
        SCOPE_ONLY,
    ];

    const A: &str = "fn a() {\n    s1();\n    s2();\n    s3();\n    s4();\n    s5();\n    s6();\n    s7();\n    s8();\n    s9();\n}\n";
    const B: &str = "fn b() {\n    one();\n    two();\n}\n";
    const B2: &str = "fn b() {\n    one();\n    deux();\n}\n";
    const C: &str = "fn c() {\n    three();\n}\n";
    const C2: &str = "fn c() {\n    tres();\n}\n";

    fn lex(text: &str) -> Vec<RawToken> {
        Lexer::new(text).parse()
    }

    fn diff(
        algorithm: MyersDiffAlgorithm,
        source: &str,
        target: &str,
        ignore: &IgnoreOptions,
        phases: DiffPhases,
        pins: &[Pin],
        cancel: bool,
    ) -> Option<MyersPath> {
        let (ts, tt) = (lex(source), lex(target));
        let cmp = |a: &RawToken, b: &RawToken| {
            a.kind == b.kind && source[a.span.clone()] == target[b.span.clone()]
        };
        myers_diff_path(
            algorithm,
            &ts,
            &tt,
            source,
            target,
            cmp,
            &ignore.mask(&ts, source, &tt, target),
            phases,
            pins,
            Arc::new(AtomicBool::new(cancel)),
        )
    }

    #[derive(Debug, Default)]
    struct LineOps {
        equal: BTreeSet<(usize, usize)>,
        deleted: BTreeSet<usize>,
        inserted: BTreeSet<usize>,
    }

    fn line_ops(source: &str, target: &str, path: &MyersPath) -> LineOps {
        let (ts, tt) = (lex(source), lex(target));
        let line = |text: &str, token: &RawToken| text[..token.span.start].matches('\n').count();
        let mut ops = LineOps::default();
        let (mut x, mut y) = (0, 0);
        for step in path_steps(path) {
            match step {
                Step::Equal => {
                    assert_eq!(source[ts[x].span.clone()], target[tt[y].span.clone()]);
                    let pair = (line(source, &ts[x]), line(target, &tt[y]));
                    ops.equal.insert(pair);
                    (x, y) = (x + 1, y + 1);
                }
                Step::Delete => {
                    ops.deleted.insert(line(source, &ts[x]));
                    x += 1;
                }
                Step::Insert => {
                    ops.inserted.insert(line(target, &tt[y]));
                    y += 1;
                }
            }
        }
        assert_eq!((x, y), (ts.len(), tt.len()));
        ops
    }

    /// The lines of `part`, which occurs once in `text`.
    fn lines_of(text: &str, part: &str) -> Range<usize> {
        assert_eq!(text.matches(part).count(), 1, "{part:?}");
        let start = text[..text.find(part).unwrap()].matches('\n').count();
        start..start + part.matches('\n').count()
    }

    fn traced(
        algorithm: MyersDiffAlgorithm,
        source: &str,
        target: &str,
        ignore: &IgnoreOptions,
        phases: DiffPhases,
    ) -> (MyersPath, PhaseTrace) {
        let (ts, tt) = (lex(source), lex(target));
        let cmp = |a: &RawToken, b: &RawToken| {
            a.kind == b.kind && source[a.span.clone()] == target[b.span.clone()]
        };
        let mut trace = PhaseTrace::default();
        let path = myers_diff_path_traced(
            algorithm,
            &ts,
            &tt,
            source,
            target,
            cmp,
            &ignore.mask(&ts, source, &tt, target),
            phases,
            &[],
            Arc::default(),
            Some(&mut trace),
        )
        .expect("not cancelled");
        (path, trace)
    }

    fn lines_set(lines: Range<usize>) -> BTreeSet<usize> {
        lines.collect()
    }

    fn assert_same_as_without_scope(source: &str, target: &str, pins: &[Pin]) {
        for algorithm in ALGORITHMS {
            for phases in SCOPE_PHASES {
                let ignore = IgnoreOptions::default();
                let without = DiffPhases {
                    scope: false,
                    ..phases
                };
                assert_eq!(
                    diff(algorithm, source, target, &ignore, phases, pins, false),
                    diff(algorithm, source, target, &ignore, without, pins, false),
                    "{algorithm:?} {phases:?}: {source:?} -> {target:?}"
                );
            }
        }
    }

    #[test]
    fn an_unchanged_scope_stays_equal_and_a_changed_one_gets_the_finer_phase() {
        let source = format!("{A}{B}");
        let target = format!("{A}{B2}");
        let ignore = IgnoreOptions::default();
        for algorithm in ALGORITHMS {
            let path = diff(
                algorithm,
                &source,
                &target,
                &ignore,
                SCOPE_PHASES[0],
                &[],
                false,
            );
            let ops = line_ops(&source, &target, &path.unwrap());
            assert_eq!(
                ops.deleted,
                lines_set(lines_of(&source, "    two();\n")),
                "{algorithm:?}"
            );
            assert_eq!(
                ops.inserted,
                lines_set(lines_of(&target, "    deux();\n")),
                "{algorithm:?}"
            );
        }
    }

    #[test]
    fn scope_alone_changes_a_changed_scope_whole() {
        let source = format!("{A}top();\n{B}");
        let target = format!("{A}top();\n{B2}");
        let ignore = IgnoreOptions::default();
        for algorithm in ALGORITHMS {
            let path = diff(algorithm, &source, &target, &ignore, SCOPE_ONLY, &[], false);
            let ops = line_ops(&source, &target, &path.unwrap());
            assert_eq!(
                ops.deleted,
                lines_set(lines_of(&source, B)),
                "{algorithm:?}"
            );
            assert_eq!(
                ops.inserted,
                lines_set(lines_of(&target, B2)),
                "{algorithm:?}"
            );
        }
    }

    #[test]
    fn scope_alone_changes_only_the_text_between_scopes_that_changed() {
        let source = format!("{A}top();\n{B}");
        let target = format!("{A}bottom();\n{B}");
        let ignore = IgnoreOptions::default();
        for algorithm in ALGORITHMS {
            let path = diff(algorithm, &source, &target, &ignore, SCOPE_ONLY, &[], false);
            let ops = line_ops(&source, &target, &path.unwrap());
            assert_eq!(
                ops.deleted,
                lines_set(lines_of(&source, "top();\n")),
                "{algorithm:?}"
            );
            assert_eq!(
                ops.inserted,
                lines_set(lines_of(&target, "bottom();\n")),
                "{algorithm:?}"
            );
        }
    }

    #[test]
    fn an_added_function_next_to_an_unchanged_one_is_a_whole_insert() {
        let target = format!("{B}{A}");
        let ignore = IgnoreOptions::default();
        for algorithm in ALGORITHMS {
            for phases in ALL_SCOPE_SETS {
                let path = diff(algorithm, A, &target, &ignore, phases, &[], false);
                let ops = line_ops(A, &target, &path.unwrap());
                let context = format!("{algorithm:?} {phases:?}: {ops:?}");
                assert!(ops.deleted.is_empty(), "{context}");
                assert_eq!(ops.inserted, lines_set(lines_of(&target, B)), "{context}");
            }
        }
    }

    #[test]
    fn braces_in_strings_and_comments_do_not_split_a_scope() {
        let b = "fn b() {\n    s = \"}\";\n    // {\n    two();\n}\n";
        let b2 = "fn b() {\n    s = \"}\";\n    // {\n    deux();\n}\n";
        let source = format!("{A}{b}");
        let target = format!("{A}{b2}");
        let ignore = IgnoreOptions::default();
        for algorithm in ALGORITHMS {
            let path = diff(algorithm, &source, &target, &ignore, SCOPE_ONLY, &[], false);
            let ops = line_ops(&source, &target, &path.unwrap());
            assert_eq!(
                ops.deleted,
                lines_set(lines_of(&source, b)),
                "{algorithm:?}"
            );
            assert_eq!(
                ops.inserted,
                lines_set(lines_of(&target, b2)),
                "{algorithm:?}"
            );
        }
    }

    #[test]
    fn a_scope_that_differs_only_in_ignored_whitespace_is_unchanged() {
        let source = format!("{A}{B}");
        let target = format!("{A}fn  b()   {{\n    one();\n      two();\n}}\n");
        let whitespace = IgnoreOptions {
            whitespace: true,
            ..Default::default()
        };
        for algorithm in ALGORITHMS {
            for phases in ALL_SCOPE_SETS {
                let (_, trace) = traced(algorithm, &source, &target, &whitespace, phases);
                assert!(
                    trace.scope_hunks.is_empty(),
                    "{algorithm:?} {phases:?}: {:?}",
                    trace.scope_hunks
                );
                let (_, trace) = traced(
                    algorithm,
                    &source,
                    &target,
                    &IgnoreOptions::default(),
                    phases,
                );
                assert_eq!(trace.scope_hunks.len(), 1, "{algorithm:?} {phases:?}");
            }
        }
    }

    #[test]
    fn finer_phases_only_change_tokens_inside_the_scope_hunks() {
        let pairs = [
            (format!("{A}{B}"), format!("{A}{B2}")),
            (
                format!("{A}top();\n{B}{C}"),
                format!("bottom();\n{A}{B2}{C2}"),
            ),
            (format!("{A}{B}{C}"), format!("{C}{A}{B2}")),
            ("a\nb\n".to_string(), "a\nc\n".to_string()),
        ];
        for algorithm in ALGORITHMS {
            for phases in ALL_SCOPE_SETS {
                for (source, target) in &pairs {
                    let ignore = IgnoreOptions::default();
                    let (path, trace) = traced(algorithm, source, target, &ignore, phases);
                    let context = format!("{algorithm:?} {phases:?} {:?}", trace.scope_hunks);
                    for hunk in &trace.scope_hunks {
                        token_hunk(&path, hunk.source.clone(), hunk.target.clone());
                    }
                    let (mut x, mut y) = (0, 0);
                    for step in path_steps(&path) {
                        match step {
                            Step::Equal => (x, y) = (x + 1, y + 1),
                            Step::Delete => {
                                let inside =
                                    trace.scope_hunks.iter().any(|h| h.source.contains(&x));
                                assert!(inside, "deleted token {x} outside: {context}");
                                x += 1;
                            }
                            Step::Insert => {
                                let inside =
                                    trace.scope_hunks.iter().any(|h| h.target.contains(&y));
                                assert!(inside, "inserted token {y} outside: {context}");
                                y += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn scope_runs_inside_the_stretches_between_pins() {
        let source = format!("{A}{B}end\n");
        let target = format!("{A}{B2}end\n");
        let pins = [Pin {
            source: source.matches('\n').count() - 1,
            target: target.matches('\n').count() - 1,
        }];
        let ignore = IgnoreOptions::default();
        for algorithm in ALGORITHMS {
            let path = diff(
                algorithm, &source, &target, &ignore, SCOPE_ONLY, &pins, false,
            );
            let ops = line_ops(&source, &target, &path.unwrap());
            assert_eq!(
                ops.deleted,
                lines_set(lines_of(&source, B)),
                "{algorithm:?}"
            );
            assert_eq!(
                ops.inserted,
                lines_set(lines_of(&target, B2)),
                "{algorithm:?}"
            );
        }
    }

    #[test]
    fn unbalanced_braces_fall_back_to_the_next_phase() {
        let pairs = [
            ("fn a() {\n    x();\n", "fn a() {\n    y();\n"),
            ("fn a() {\n}\n}\n", "fn a() {\n}\n"),
            (
                "fn a() {\n    x();\n}\n\nfn b() {\n",
                "fn b() {\n    x();\n}\n\nfn a() {\n}\n",
            ),
        ];
        for (source, target) in pairs {
            assert_same_as_without_scope(source, target, &[]);
            assert_same_as_without_scope(target, source, &[]);
        }
    }

    #[test]
    fn a_pin_inside_a_function_falls_back_for_the_cut_stretches() {
        let source = format!("{A}\nfn b() {{\n    one();\n}}\n");
        let target = format!("fn b() {{\n    uno();\n}}\n\n{A}");
        let pin = Pin {
            source: 1,
            target: lines_of(&target, A).start + 1,
        };
        assert_same_as_without_scope(&source, &target, &[pin]);
    }

    #[test]
    fn without_braces_scope_changes_nothing() {
        let pairs = [
            ("a\nb\nc\n", "a\nx\nc\nd\n"),
            ("", "only target\n"),
            ("same\n", "same\n"),
        ];
        for (source, target) in pairs {
            assert_same_as_without_scope(source, target, &[]);
            assert_same_as_without_scope(target, source, &[]);
        }
    }

    #[test]
    fn a_cancelled_scope_diff_returns_none() {
        let source = format!("{A}\nfn b() {{\n}}\n");
        let target = format!("fn b() {{\n}}\n\n{A}");
        for algorithm in ALGORITHMS {
            for phases in SCOPE_PHASES {
                let ignore = IgnoreOptions::default();
                assert_eq!(
                    diff(algorithm, &source, &target, &ignore, phases, &[], true),
                    None
                );
            }
        }
    }

    /// Each scope's text.
    fn scopes(text: &str) -> Option<Vec<&str>> {
        let tokens: Vec<RawToken> = Lexer::new(text).parse();
        let scopes = find_scopes(&tokens, text)?;
        let span = |r: &Range<usize>| {
            let start = tokens[r.start].span.start;
            let end = tokens[r.end - 1].span.end;
            &text[start..end]
        };
        Some(scopes.iter().map(span).collect())
    }

    #[test]
    fn top_level_scopes_span_whole_lines() {
        let text = "use x;\n\nfn a() {\n    if b {\n        c();\n    }\n}\n\nfn d() { e(); } // end\nlast\n";
        assert_eq!(
            scopes(text),
            Some(vec![
                "fn a() {\n    if b {\n        c();\n    }\n}\n",
                "fn d() { e(); } // end\n",
            ])
        );
    }

    #[test]
    fn no_braces_means_no_scopes() {
        assert_eq!(scopes("a\nb\n"), Some(vec![]));
        assert_eq!(scopes(""), Some(vec![]));
    }

    #[test]
    fn scopes_sharing_a_line_merge() {
        let text = "if a {\n    b();\n} else {\n    c();\n}\nfn d() {} fn e() {\n}\n";
        assert_eq!(
            scopes(text),
            Some(vec![
                "if a {\n    b();\n} else {\n    c();\n}\n",
                "fn d() {} fn e() {\n}\n",
            ])
        );
    }

    #[test]
    fn a_last_line_without_a_line_break_ends_the_scope() {
        assert_eq!(scopes("x\nfn a() {\n}"), Some(vec!["fn a() {\n}"]));
    }

    #[test]
    fn a_brace_alone_on_its_line_starts_the_scope_at_the_line_above() {
        let text = "fn a()\n{\n    b();\n}\n}\n";
        assert_eq!(scopes(text), None);
        let text = "fn a()\n{\n    b();\n}\nfn c()\n  {\n}\n";
        assert_eq!(
            scopes(text),
            Some(vec!["fn a()\n{\n    b();\n}\n", "fn c()\n  {\n}\n"])
        );
    }

    #[test]
    fn a_brace_alone_after_a_scope_does_not_reach_into_it() {
        let text = "a {\n}\n{\n}\n";
        assert_eq!(scopes(text), Some(vec!["a {\n}\n", "{\n}\n"]));
    }

    #[test]
    fn braces_in_strings_and_comments_are_skipped() {
        let text = "fn a() {\n    s = \"}{\";\n    // }\n    /* { */\n}\n/*\n{\n*/\n";
        assert_eq!(
            scopes(text),
            Some(vec![
                "fn a() {\n    s = \"}{\";\n    // }\n    /* { */\n}\n"
            ])
        );
    }

    #[test]
    fn unbalanced_braces_mean_no_scopes() {
        assert_eq!(scopes("fn a() {\n"), None);
        assert_eq!(scopes("}\nfn a() {\n}\n"), None);
        assert_eq!(scopes("fn a() {\n}\n}\n"), None);
    }

    #[test]
    fn scope_lines_are_the_line_ranges_of_the_scopes() {
        let lines = |text: &str| scope_lines(&Lexer::new(text).parse(), text);
        let text = "use x;\nfn a() {\n    b();\n}\n\nfn c() { d(); }\nlast";
        assert_eq!(lines(text), [1..4, 5..6]);
        assert_eq!(lines("x\nfn a() {\n}"), [1..3]);
        assert!(lines("fn a() {\n").is_empty());
    }

    #[test]
    fn changed_scopes_are_the_scopes_holding_a_changed_line() {
        let scopes = [1..4, 5..6, 8..10];
        assert_eq!(changed_scopes(&scopes, [0, 2, 3, 7, 9]), [1..4, 8..10]);
        assert!(changed_scopes(&scopes, [4, 6]).is_empty());
        assert!(changed_scopes(&[], [1]).is_empty());
    }
}
