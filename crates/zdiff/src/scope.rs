use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{
    ignore::IgnoreMask,
    lexer::{RawTokenTrait, TokenKind},
    myers::{
        DiffPhases, MyersDiffAlgorithm, MyersPath, Step, path_steps, phases_path, raw_diff_path,
        slice_mask, split_lines,
    },
};

/// Scope phase: pairs the top-level scopes of both sides by Myers over their header lines (under
/// the ignore mask). Paired scopes and the text between scopes go to the next phases; unpaired
/// scopes are deleted or inserted whole. No scopes on either side, or unbalanced braces on
/// either, leave the whole stretch to the next phases.
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
) -> Option<MyersPath>
where
    T: RawTokenTrait,
    F: Fn(&T, &T) -> bool + Sync,
{
    if cancel_flag.load(Ordering::Relaxed) {
        return None;
    }
    let next = DiffPhases {
        scope: false,
        ..phases
    };
    let next_path = |s: &[T], t: &[T], ignore: &IgnoreMask| {
        phases_path(
            algorithm,
            s,
            t,
            source_text,
            target_text,
            cmp,
            ignore,
            next,
            false,
            cancel_flag.clone(),
        )
    };
    let (Some(source_scopes), Some(target_scopes)) = (
        find_scopes(source, source_text),
        find_scopes(target, target_text),
    ) else {
        return next_path(source, target, ignore);
    };
    if source_scopes.is_empty() && target_scopes.is_empty() {
        return next_path(source, target, ignore);
    }

    let key_eq = |a: &Range<usize>, b: &Range<usize>| {
        let mut a_key = header(source, a).filter(|&i| !ignore.source.get(i).is_some_and(|&f| f));
        let mut b_key = header(target, b).filter(|&i| !ignore.target.get(i).is_some_and(|&f| f));
        loop {
            match (a_key.next(), b_key.next()) {
                (None, None) => return true,
                (Some(i), Some(j)) if cmp(&source[i], &target[j]) => {}
                _ => return false,
            }
        }
    };
    let scope_pairs = raw_diff_path(
        algorithm,
        &source_scopes,
        &target_scopes,
        key_eq,
        cancel_flag.clone(),
    )?;
    if cancel_flag.load(Ordering::Relaxed) {
        return None;
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    for step in path_steps(&scope_pairs) {
        match step {
            Step::Equal => {
                pairs.push((i, j));
                (i, j) = (i + 1, j + 1);
            }
            Step::Delete => i += 1,
            Step::Insert => j += 1,
        }
    }

    let mut path = Vec::with_capacity(source.len() + target.len() + 1);
    path.push((0, 0));
    let (mut from, mut next_scope) = ((0, 0), (0, 0));
    let ends = pairs.into_iter().map(Some).chain([None]);
    for pair in ends {
        let (to, scope_end) = match pair {
            Some((i, j)) => ((source_scopes[i].start, target_scopes[j].start), (i, j)),
            None => (
                (source.len(), target.len()),
                (source_scopes.len(), target_scopes.len()),
            ),
        };
        let unpaired = (
            &source_scopes[next_scope.0..scope_end.0],
            &target_scopes[next_scope.1..scope_end.1],
        );
        between_scopes(
            &mut path,
            source,
            target,
            from..to,
            unpaired,
            ignore,
            &next_path,
        )?;
        if let Some((i, j)) = pair {
            let (s, t) = (source_scopes[i].clone(), target_scopes[j].clone());
            let mask = slice_mask(ignore, s.clone(), t.clone());
            let local = next_path(&source[s.clone()], &target[t.clone()], &mask)?;
            extend_offset(&mut path, &local, (s.start, t.start));
            (from, next_scope) = ((s.end, t.end), (i + 1, j + 1));
        }
    }
    Some(path)
}

/// The tokens between two paired scopes (or a file end). Unpaired scopes are deleted or inserted
/// whole; the rest of both sides goes to the next phase as one range.
fn between_scopes<T, N>(
    path: &mut MyersPath,
    source: &[T],
    target: &[T],
    range: Range<(usize, usize)>,
    (source_unpaired, target_unpaired): (&[Range<usize>], &[Range<usize>]),
    ignore: &IgnoreMask,
    next_path: &N,
) -> Option<()>
where
    T: RawTokenTrait,
    N: Fn(&[T], &[T], &IgnoreMask) -> Option<MyersPath>,
{
    let (s, t) = (range.start.0..range.end.0, range.start.1..range.end.1);
    if source_unpaired.is_empty() && target_unpaired.is_empty() {
        let mask = slice_mask(ignore, s.clone(), t.clone());
        let local = next_path(&source[s.clone()], &target[t.clone()], &mask)?;
        extend_offset(path, &local, (s.start, t.start));
        return Some(());
    }
    let (source_rest, target_rest) = (
        outside(s.clone(), source_unpaired),
        outside(t.clone(), target_unpaired),
    );
    let gather = |tokens: &[T], indices: &[usize]| -> Vec<T> {
        indices.iter().map(|&i| tokens[i].clone()).collect()
    };
    let mask = if ignore.is_empty() {
        IgnoreMask::default()
    } else {
        IgnoreMask {
            source: source_rest.iter().map(|&i| ignore.source[i]).collect(),
            target: target_rest.iter().map(|&i| ignore.target[i]).collect(),
            ..Default::default()
        }
    };
    let local = next_path(
        &gather(source, &source_rest),
        &gather(target, &target_rest),
        &mask,
    )?;

    let (mut x, mut y) = (s.start, t.start);
    let delete_to = |path: &mut MyersPath, x: &mut usize, y: usize, to: usize| {
        while *x < to {
            *x += 1;
            path.push((*x as i32, y as i32));
        }
    };
    let insert_to = |path: &mut MyersPath, x: usize, y: &mut usize, to: usize| {
        while *y < to {
            *y += 1;
            path.push((x as i32, *y as i32));
        }
    };
    let (mut gx, mut gy) = (0, 0);
    for step in path_steps(&local) {
        if step != Step::Insert {
            delete_to(path, &mut x, y, source_rest[gx]);
        }
        if step != Step::Delete {
            insert_to(path, x, &mut y, target_rest[gy]);
        }
        if step != Step::Insert {
            (x, gx) = (x + 1, gx + 1);
        }
        if step != Step::Delete {
            (y, gy) = (y + 1, gy + 1);
        }
        path.push((x as i32, y as i32));
    }
    delete_to(path, &mut x, y, s.end);
    insert_to(path, x, &mut y, t.end);
    Some(())
}

/// A scope's first line, which is its key.
fn header<T: RawTokenTrait>(tokens: &[T], scope: &Range<usize>) -> Range<usize> {
    let line_break = tokens[scope.clone()]
        .iter()
        .position(|t| t.as_ref().kind == TokenKind::Newline);
    scope.start..line_break.map_or(scope.end, |i| scope.start + i + 1)
}

/// The indices of `range` outside the sorted, disjoint `scopes`.
fn outside(range: Range<usize>, scopes: &[Range<usize>]) -> Vec<usize> {
    let mut indices = Vec::with_capacity(range.len());
    let mut start = range.start;
    for scope in scopes {
        indices.extend(start..scope.start);
        start = scope.end;
    }
    indices.extend(start..range.end);
    indices
}

fn extend_offset(path: &mut MyersPath, local: &[(i32, i32)], (x, y): (usize, usize)) {
    path.extend(
        local[1..]
            .iter()
            .map(|&(lx, ly)| (x as i32 + lx, y as i32 + ly)),
    );
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{
        ignore::IgnoreOptions,
        lexer::{LexerDefault, RawToken},
        myers::{Pin, Step, myers_diff_path, path_steps},
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

    const A: &str = "fn a() {\n    s1();\n    s2();\n    s3();\n    s4();\n    s5();\n    s6();\n    s7();\n    s8();\n    s9();\n}\n";
    const C: &str = "fn c() {\n    three();\n}\n";
    const C2: &str = "fn c() {\n    tres();\n}\n";

    fn lex(text: &str) -> Vec<RawToken> {
        LexerDefault::<RawToken>::new(text).parse()
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

    /// `moved` is wholly deleted and wholly inserted, and each of `paired` pairs its header lines.
    fn assert_scopes_pair(source: &str, target: &str, moved: &str, paired: &[(&str, &str)]) {
        let (moved_source, moved_target) = (lines_of(source, moved), lines_of(target, moved));
        for algorithm in ALGORITHMS {
            for phases in SCOPE_PHASES {
                let ignore = IgnoreOptions::default();
                let path = diff(algorithm, source, target, &ignore, phases, &[], false).unwrap();
                let ops = line_ops(source, target, &path);
                let context = format!("{algorithm:?} {phases:?}: {ops:?}");
                for (s, t) in paired {
                    let header = (lines_of(source, s).start, lines_of(target, t).start);
                    assert!(ops.equal.contains(&header), "{context}");
                }
                assert!(
                    ops.equal
                        .iter()
                        .all(|(s, t)| !moved_source.contains(s) && !moved_target.contains(t)),
                    "{context}"
                );
                assert!(
                    moved_source.clone().all(|l| ops.deleted.contains(&l)),
                    "{context}"
                );
                assert!(
                    moved_target.clone().all(|l| ops.inserted.contains(&l)),
                    "{context}"
                );
            }
        }
    }

    fn assert_plain_diff_does_not_pair(source: &str, target: &str, s: &str, t: &str) {
        let header = (lines_of(source, s).start, lines_of(target, t).start);
        for algorithm in ALGORITHMS {
            let ignore = IgnoreOptions::default();
            let phases = DiffPhases::default();
            let path = diff(algorithm, source, target, &ignore, phases, &[], false).unwrap();
            let ops = line_ops(source, target, &path);
            assert!(!ops.equal.contains(&header), "{algorithm:?}: {ops:?}");
        }
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
    fn reordered_functions_pair_by_header() {
        let (b, b2) = (
            "fn b() {\n    one();\n    two();\n}\n",
            "fn b() {\n    uno();\n    dos();\n}\n",
        );
        let source = format!("{A}\n{b}\n{C}");
        let target = format!("{b2}\n{C2}\n{A}");
        assert_plain_diff_does_not_pair(&source, &target, b, b2);
        assert_scopes_pair(&source, &target, A, &[(b, b2), (C, C2)]);
    }

    #[test]
    fn braces_in_strings_and_comments_do_not_break_pairing() {
        let (b, b2) = (
            "fn b() {\n    s = \"}\";\n    two();\n}\n",
            "fn b() {\n    s = \"}\";\n    dos();\n}\n",
        );
        let (c, c2) = (
            "fn c() {\n    // {\n    three();\n}\n",
            "fn c() {\n    // {\n    tres();\n}\n",
        );
        let source = format!("{A}\n{b}\n{c}");
        let target = format!("{b2}\n{c2}\n{A}");
        assert_plain_diff_does_not_pair(&source, &target, b, b2);
        assert_scopes_pair(&source, &target, A, &[(b, b2), (c, c2)]);
    }

    #[test]
    fn an_added_function_is_a_whole_insert() {
        let added = "fn b() {\n    x();\n    y();\n}\n";
        let (a, a2) = (
            "fn a() {\n    x();\n    y();\n}\n",
            "fn a() {\n    z();\n}\n",
        );
        let target = format!("{added}\n{a2}");
        let added_lines = lines_of(&target, added);
        for algorithm in ALGORITHMS {
            let ignore = IgnoreOptions::default();
            let path = diff(
                algorithm,
                a,
                &target,
                &ignore,
                DiffPhases::default(),
                &[],
                false,
            );
            let ops = line_ops(a, &target, &path.unwrap());
            assert!(
                ops.equal.iter().any(|(_, t)| added_lines.contains(t)),
                "{algorithm:?}: {ops:?}"
            );
        }
        for algorithm in ALGORITHMS {
            for phases in SCOPE_PHASES {
                let ignore = IgnoreOptions::default();
                let path = diff(algorithm, a, &target, &ignore, phases, &[], false).unwrap();
                let ops = line_ops(a, &target, &path);
                let context = format!("{algorithm:?} {phases:?}: {ops:?}");
                assert!(
                    ops.equal.contains(&(0, lines_of(&target, a2).start)),
                    "{context}"
                );
                assert!(
                    ops.equal.iter().all(|(_, t)| !added_lines.contains(t)),
                    "{context}"
                );
                assert!(
                    added_lines.clone().all(|l| ops.inserted.contains(&l)),
                    "{context}"
                );
            }
        }
    }

    #[test]
    fn text_between_scopes_pairs_across_an_unpaired_scope() {
        let source = format!("{A}top();\nfn b() {{\n}}\n{C}end();\n");
        let target = format!("top();\nfn b() {{\n}}\n{C2}\n{A}end();\n");
        let pair = |part| (lines_of(&source, part).start, lines_of(&target, part).start);
        let (top, end) = (pair("top"), pair("end"));
        for algorithm in ALGORITHMS {
            for phases in SCOPE_PHASES {
                let ignore = IgnoreOptions::default();
                let path = diff(algorithm, &source, &target, &ignore, phases, &[], false);
                let ops = line_ops(&source, &target, &path.unwrap());
                let context = format!("{algorithm:?} {phases:?}: {ops:?}");
                assert!(ops.equal.contains(&top), "{context}");
                assert!(ops.equal.contains(&end), "{context}");
                assert!(
                    lines_of(&source, A).all(|l| ops.deleted.contains(&l)),
                    "{context}"
                );
                assert!(
                    lines_of(&target, A).all(|l| ops.inserted.contains(&l)),
                    "{context}"
                );
            }
        }
    }

    #[test]
    fn scope_keys_honor_the_ignore_options() {
        let (b, b2) = ("fn b() {\n    one();\n}\n", "fn  b()   {\n    uno();\n}\n");
        let source = format!("{A}\n{b}\n{C}");
        let target = format!("{b2}\n{C2}\n{A}");
        let header = (lines_of(&source, b).start, lines_of(&target, b2).start);
        let whitespace = IgnoreOptions {
            whitespace: true,
            ..Default::default()
        };
        for algorithm in ALGORITHMS {
            for phases in SCOPE_PHASES {
                for (ignore, pairs) in [(&IgnoreOptions::default(), false), (&whitespace, true)] {
                    let path = diff(algorithm, &source, &target, ignore, phases, &[], false);
                    let ops = line_ops(&source, &target, &path.unwrap());
                    assert_eq!(
                        ops.equal.contains(&header),
                        pairs,
                        "{algorithm:?} {phases:?} {ignore:?}: {ops:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn scopes_pair_inside_the_stretches_between_pins() {
        let (b, b2) = (
            "fn b() {\n    one();\n    two();\n}\n",
            "fn b() {\n    uno();\n    dos();\n}\n",
        );
        let source = format!("{A}\n{b}\n{C}end\n");
        let target = format!("{b2}\n{C2}\n{A}end\n");
        let pins = [Pin {
            source: source.matches('\n').count() - 1,
            target: target.matches('\n').count() - 1,
        }];
        let header = (lines_of(&source, b).start, lines_of(&target, b2).start);
        for algorithm in ALGORITHMS {
            for phases in SCOPE_PHASES {
                let ignore = IgnoreOptions::default();
                let path = diff(algorithm, &source, &target, &ignore, phases, &pins, false);
                let ops = line_ops(&source, &target, &path.unwrap());
                assert!(
                    ops.equal.contains(&header),
                    "{algorithm:?} {phases:?}: {ops:?}"
                );
            }
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
        let tokens: Vec<RawToken> = LexerDefault::<RawToken>::new(text).parse();
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
}
