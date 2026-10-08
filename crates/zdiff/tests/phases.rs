use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use zdiff::{
    char_diff::character_diff,
    diff_builder::{DiffBuilderOptions, DiffRow, LineContent, build_diff_rows},
    diff_ir::{DiffIR, DiffOp},
    ignore::{IgnoreMask, IgnoreOptions},
    lexer::{LexerDefault, RawToken},
    myers::{DiffPhases, MyersDiffAlgorithm, MyersPath, myers_diff_path},
};

const ALGORITHMS: [MyersDiffAlgorithm; 3] = [
    MyersDiffAlgorithm::Trace,
    MyersDiffAlgorithm::Linear,
    MyersDiffAlgorithm::LinearMT,
];

fn valid_phase_sets() -> Vec<DiffPhases> {
    (0..16u8)
        .map(|bits| DiffPhases {
            scope: bits & 1 != 0,
            line: bits & 2 != 0,
            token: bits & 4 != 0,
            character: bits & 8 != 0,
        })
        .filter(|phases| phases.is_valid())
        .collect()
}

fn lex(text: &str) -> Vec<RawToken> {
    LexerDefault::<RawToken>::new(text).collect()
}

fn path(
    algorithm: MyersDiffAlgorithm,
    s1: &str,
    s2: &str,
    t1: &[RawToken],
    t2: &[RawToken],
    mask: &IgnoreMask,
    phases: DiffPhases,
) -> MyersPath {
    let cmp =
        |a: &RawToken, b: &RawToken| a.kind == b.kind && s1[a.span.clone()] == s2[b.span.clone()];
    myers_diff_path(
        algorithm,
        t1,
        t2,
        s1,
        s2,
        cmp,
        mask,
        phases,
        &[],
        Arc::default(),
    )
    .expect("not cancelled")
}

fn rows(
    algorithm: MyersDiffAlgorithm,
    s1: &str,
    s2: &str,
    ignore: &IgnoreOptions,
    phases: DiffPhases,
) -> Vec<DiffRow> {
    let (t1, t2) = (lex(s1), lex(s2));
    let mask = ignore.mask(&t1, s1, &t2, s2);
    let path = path(algorithm, s1, s2, &t1, &t2, &mask, phases);
    let mut ir = DiffIR::new(&path, true, Arc::default()).expect("not cancelled");
    if phases.character {
        ir.char_ranges = character_diff(algorithm, &path, &t1, &t2, s1, s2, &mask, Arc::default())
            .expect("not cancelled");
    }
    let options = DiffBuilderOptions {
        ignore: ignore.clone(),
        phases,
        ..Default::default()
    };
    build_diff_rows(ir, Some(&t1), Some(&t2), s1, s2, &options, 16)
}

fn visible_ops(content: &LineContent) -> usize {
    match content {
        LineContent::Code { tokens, .. } => tokens
            .iter()
            .filter(|(res, _, _)| !res.hide_in_diff && !matches!(res.operation, DiffOp::Equal(_)))
            .count(),
        _ => 0,
    }
}

fn changed_chars(content: &LineContent) -> usize {
    match content {
        LineContent::Code { changed_chars, .. } => changed_chars.len(),
        _ => 0,
    }
}

#[test]
fn whitespace_only_changes_stay_hidden_with_every_valid_phase_set() {
    let s1 = "use a;\n\nfn alpha(a: i32) -> i32 {\n    let x = a + 1;\n    x * 2\n}\n\nfn beta() {\n    call(1, 2);\n}\nend();\n";
    let s2 = "use  a;\n\nfn  alpha(a: i32)  ->  i32 {\n        let x = a+1;\n    x * 2   \n}\n\n\nfn beta()  {\n\tcall(1,  2);\n}\n  end();\n";
    let on = IgnoreOptions {
        whitespace: true,
        ..Default::default()
    };
    let sets = valid_phase_sets();
    assert_eq!(sets.len(), 10);
    for algorithm in ALGORITHMS {
        for &phases in &sets {
            let plain = rows(algorithm, s1, s2, &IgnoreOptions::default(), phases);
            let plain_ops: usize = plain
                .iter()
                .map(|row| visible_ops(&row.left) + visible_ops(&row.right))
                .sum();
            assert!(plain_ops > 0, "{algorithm:?} {phases:?}");
            let plain_chars: usize = plain
                .iter()
                .map(|row| changed_chars(&row.left) + changed_chars(&row.right))
                .sum();
            assert_eq!(
                plain_chars > 0,
                phases.character,
                "{algorithm:?} {phases:?}"
            );

            for (i, row) in rows(algorithm, s1, s2, &on, phases).iter().enumerate() {
                assert_eq!(
                    (visible_ops(&row.left), visible_ops(&row.right)),
                    (0, 0),
                    "{algorithm:?} {phases:?}: row {i} {row:?}"
                );
                assert_eq!(
                    (changed_chars(&row.left), changed_chars(&row.right)),
                    (0, 0),
                    "{algorithm:?} {phases:?}: row {i} {row:?}"
                );
            }
        }
    }
}

fn large_functions() -> (String, String) {
    let (mut s1, mut s2) = (String::new(), String::new());
    for f in 0..200 {
        let header = format!("fn f{f}(a: i32) -> i32 {{\n");
        if f % 50 == 7 {
            s2 += &header;
            s2 += "    a\n}\n";
        }
        s1 += &header;
        s2 += &header;
        for i in 0..4 {
            let line = format!("    let v{i} = a + {};\n", f * 4 + i);
            s1 += &line;
            if f % 10 == 3 && i == 1 {
                s2 += &format!("    let w{i} = a * {f};\n");
            } else {
                s2 += &line;
            }
        }
        s1 += "}\n";
        s2 += "}\n";
    }
    (s1, s2)
}

#[test]
fn scope_line_and_token_phases_return_none_promptly_when_cancelled() {
    let (s1, s2) = large_functions();
    let (t1, t2) = (lex(&s1), lex(&s2));
    let sets: Vec<DiffPhases> = valid_phase_sets()
        .into_iter()
        .filter(|phases| !phases.character)
        .collect();
    assert_eq!(sets.len(), 6);
    for algorithm in ALGORITHMS {
        for &phases in &sets {
            let run = |cancel_after: usize| {
                let calls = AtomicUsize::new(0);
                let cancel = Arc::new(AtomicBool::new(cancel_after == 0));
                let cmp = |a: &RawToken, b: &RawToken| {
                    if calls.fetch_add(1, Ordering::Relaxed) + 1 >= cancel_after {
                        cancel.store(true, Ordering::Relaxed);
                    }
                    a.kind == b.kind && s1[a.span.clone()] == s2[b.span.clone()]
                };
                let path = myers_diff_path(
                    algorithm,
                    &t1,
                    &t2,
                    &s1,
                    &s2,
                    cmp,
                    &IgnoreMask::default(),
                    phases,
                    &[],
                    Arc::clone(&cancel),
                );
                (path.is_some(), calls.into_inner())
            };
            let (finished, total) = run(usize::MAX);
            assert!(finished, "{algorithm:?} {phases:?}");
            let (finished, cancelled) = run(total / 10);
            assert!(!finished, "{algorithm:?} {phases:?}");
            // myers_diff_trace has no cancel check, so Trace only stops between Myers runs.
            if algorithm != MyersDiffAlgorithm::Trace {
                assert!(
                    cancelled < total / 2,
                    "{algorithm:?} {phases:?}: {cancelled} of {total} compares"
                );
            }
            assert_eq!(run(0), (false, 0), "{algorithm:?} {phases:?}");
        }
    }
}

struct Probe {
    calls: AtomicUsize,
    cancel_after: usize,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone)]
struct ProbedToken {
    token: RawToken,
    probe: Option<Arc<Probe>>,
}

impl From<RawToken> for ProbedToken {
    fn from(token: RawToken) -> Self {
        Self { token, probe: None }
    }
}

impl AsRef<RawToken> for ProbedToken {
    fn as_ref(&self) -> &RawToken {
        if let Some(probe) = &self.probe
            && probe.calls.fetch_add(1, Ordering::Relaxed) + 1 >= probe.cancel_after
        {
            probe.cancel.store(true, Ordering::Relaxed);
        }
        &self.token
    }
}

#[test]
fn character_phase_returns_none_promptly_when_cancelled() {
    let (mut s1, mut s2) = (String::new(), String::new());
    for i in 0..2000 {
        s1 += &format!("let v{i} = value_{i}a + {i}0;\n");
        if i % 2 == 0 {
            s2 += &format!("let v{i} = value_{i}a + {i}0;\n");
        } else {
            s2 += &format!("let v{i} = value_{i}b + {i}1;\n");
        }
    }
    let (t1, t2) = (lex(&s1), lex(&s2));
    let mask = IgnoreMask::default();
    let phases = DiffPhases {
        character: true,
        ..Default::default()
    };
    for algorithm in ALGORITHMS {
        let path = path(algorithm, &s1, &s2, &t1, &t2, &mask, phases);
        let run = |cancel_after: usize| {
            let probe = Arc::new(Probe {
                calls: AtomicUsize::new(0),
                cancel_after,
                cancel: Arc::new(AtomicBool::new(cancel_after == 0)),
            });
            let probed = |tokens: &[RawToken]| -> Vec<ProbedToken> {
                tokens
                    .iter()
                    .map(|token| ProbedToken {
                        token: token.clone(),
                        probe: Some(Arc::clone(&probe)),
                    })
                    .collect()
            };
            let ranges = character_diff(
                algorithm,
                &path,
                &probed(&t1),
                &probed(&t2),
                &s1,
                &s2,
                &mask,
                Arc::clone(&probe.cancel),
            );
            (ranges.is_some(), probe.calls.load(Ordering::Relaxed))
        };
        let (finished, total) = run(usize::MAX);
        assert!(finished, "{algorithm:?}");
        let (finished, cancelled) = run(total / 10);
        assert!(!finished, "{algorithm:?}");
        assert!(
            cancelled < total / 2,
            "{algorithm:?}: {cancelled} of {total} token reads"
        );
        assert_eq!(run(0), (false, 0), "{algorithm:?}");
    }
}
