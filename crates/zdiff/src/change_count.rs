use std::ops::Range;

use crate::{
    char_diff::CharRanges,
    lexer::RawTokenTrait,
    myers::{
        DiffPhases, MyersNumAddDelete, Step, myers_count_add_deletes, path_steps, split_lines,
    },
    scope::scope_units,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeUnit {
    Scope,
    Line,
    Token,
    Character,
}

impl ChangeUnit {
    pub fn finest(phases: DiffPhases) -> Option<Self> {
        if phases.character {
            Some(Self::Character)
        } else if phases.token {
            Some(Self::Token)
        } else if phases.line {
            Some(Self::Line)
        } else if phases.scope {
            Some(Self::Scope)
        } else {
            None
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Scope => "scopes",
            Self::Line => "lines",
            Self::Token => "tokens",
            Self::Character => "chars",
        }
    }
}

/// (adds, deletes) of `path` in `unit`: inserted units of the target, deleted units of the source.
pub fn count_changes<T: RawTokenTrait>(
    unit: ChangeUnit,
    path: &[(i32, i32)],
    source: &[T],
    target: &[T],
    source_text: &str,
    target_text: &str,
    char_ranges: &CharRanges,
) -> MyersNumAddDelete {
    match unit {
        ChangeUnit::Token => myers_count_add_deletes(path),
        ChangeUnit::Character => {
            let chars = |text: &str, ranges: &[Range<usize>]| {
                ranges
                    .iter()
                    .map(|r| text[r.clone()].chars().count())
                    .sum::<usize>() as u32
            };
            (
                chars(target_text, &char_ranges.target),
                chars(source_text, &char_ranges.source),
            )
        }
        ChangeUnit::Line | ChangeUnit::Scope => {
            let units = |tokens: &[T], text: &str| match unit {
                ChangeUnit::Scope => scope_units(tokens, text),
                _ => split_lines(tokens),
            };
            let (deleted, inserted) = edited_tokens(path);
            (
                units_hit(&units(target, target_text), &inserted),
                units_hit(&units(source, source_text), &deleted),
            )
        }
    }
}

/// Ascending indices of the deleted source tokens and the inserted target tokens.
fn edited_tokens(path: &[(i32, i32)]) -> (Vec<usize>, Vec<usize>) {
    let (mut deleted, mut inserted) = (Vec::new(), Vec::new());
    let (mut x, mut y) = (0, 0);
    for step in path_steps(path) {
        match step {
            Step::Equal => (x, y) = (x + 1, y + 1),
            Step::Delete => {
                deleted.push(x);
                x += 1;
            }
            Step::Insert => {
                inserted.push(y);
                y += 1;
            }
        }
    }
    (deleted, inserted)
}

/// How many of the sorted, disjoint `units` hold one of the ascending `indices`.
fn units_hit(units: &[Range<usize>], indices: &[usize]) -> u32 {
    let mut hit = 0;
    let mut last = None;
    for &i in indices {
        let unit = units.partition_point(|u| u.end <= i);
        if unit < units.len() && last != Some(unit) {
            hit += 1;
            last = Some(unit);
        }
    }
    hit
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        char_diff::character_diff,
        ignore::IgnoreMask,
        lexer::{Lexer, RawToken},
        myers::{MyersDiffAlgorithm, myers_diff_path},
    };

    const SCOPE: DiffPhases = DiffPhases {
        scope: true,
        line: false,
        token: false,
        character: false,
    };
    const LINE: DiffPhases = DiffPhases {
        scope: false,
        line: true,
        token: false,
        character: false,
    };
    const LINE_TOKEN: DiffPhases = DiffPhases {
        scope: false,
        line: true,
        token: true,
        character: false,
    };
    const ALL: DiffPhases = DiffPhases {
        scope: true,
        line: true,
        token: true,
        character: true,
    };

    fn count(phases: DiffPhases, source: &str, target: &str) -> (ChangeUnit, MyersNumAddDelete) {
        let (ts, tt): (Vec<RawToken>, Vec<RawToken>) =
            (Lexer::new(source).collect(), Lexer::new(target).collect());
        let cmp = |a: &RawToken, b: &RawToken| {
            a.kind == b.kind && source[a.span.clone()] == target[b.span.clone()]
        };
        let algorithm = MyersDiffAlgorithm::Linear;
        let mask = IgnoreMask::default();
        let path = myers_diff_path(
            algorithm,
            &ts,
            &tt,
            source,
            target,
            cmp,
            &mask,
            phases,
            &[],
            Arc::default(),
        )
        .unwrap();
        let char_ranges = if phases.character {
            character_diff(
                algorithm,
                &path,
                &ts,
                &tt,
                source,
                target,
                &mask,
                Arc::default(),
            )
            .unwrap()
        } else {
            CharRanges::default()
        };
        let unit = ChangeUnit::finest(phases).unwrap();
        let counts = count_changes(unit, &path, &ts, &tt, source, target, &char_ranges);
        (unit, counts)
    }

    #[test]
    fn the_unit_is_the_finest_phase_on() {
        assert_eq!(ChangeUnit::finest(SCOPE), Some(ChangeUnit::Scope));
        assert_eq!(ChangeUnit::finest(LINE), Some(ChangeUnit::Line));
        assert_eq!(
            ChangeUnit::finest(DiffPhases {
                scope: true,
                ..LINE
            }),
            Some(ChangeUnit::Line)
        );
        assert_eq!(ChangeUnit::finest(LINE_TOKEN), Some(ChangeUnit::Token));
        assert_eq!(ChangeUnit::finest(ALL), Some(ChangeUnit::Character));
        let off = DiffPhases {
            scope: false,
            line: false,
            token: false,
            character: false,
        };
        assert_eq!(ChangeUnit::finest(off), None);
    }

    #[test]
    fn each_unit_has_a_label() {
        let labels = [
            ChangeUnit::Scope,
            ChangeUnit::Line,
            ChangeUnit::Token,
            ChangeUnit::Character,
        ]
        .map(ChangeUnit::label);
        assert_eq!(labels, ["scopes", "lines", "tokens", "chars"]);
    }

    #[test]
    fn line_counts_changed_lines_per_side() {
        let (unit, counts) = count(LINE, "a\nb b b\nc\n", "a\nB b b\nc\nd\n");
        assert_eq!(unit, ChangeUnit::Line);
        assert_eq!(counts, (2, 1));
    }

    #[test]
    fn token_counts_changed_tokens() {
        let (unit, counts) = count(LINE_TOKEN, "a b c\n", "a x c y\n");
        assert_eq!(unit, ChangeUnit::Token);
        assert_eq!(counts, (3, 1));
    }

    #[test]
    fn character_counts_changed_chars() {
        let (unit, counts) = count(ALL, "x = foo123;\n", "x = foo1245;\n");
        assert_eq!(unit, ChangeUnit::Character);
        assert_eq!(counts, (2, 1));
    }

    #[test]
    fn scope_counts_changed_scopes_and_the_stretches_between_them() {
        let source =
            "use x;\nfn a() {\n    1;\n    2;\n}\nfn b() {\n    3;\n}\nfn c() {\n    4;\n}\n";
        let target = "use y;\nfn a() {\n    1;\n    9;\n    8;\n}\nfn b() {\n    3;\n}\nfn c() {\n    5;\n}\n";
        let (unit, counts) = count(SCOPE, source, target);
        assert_eq!(unit, ChangeUnit::Scope);
        assert_eq!(counts, (3, 3));
    }

    #[test]
    fn scope_without_braces_counts_the_whole_file_as_one() {
        let (unit, counts) = count(SCOPE, "a\nb\nc\n", "a\nx\nc\ny\n");
        assert_eq!(unit, ChangeUnit::Scope);
        assert_eq!(counts, (1, 1));
    }

    #[test]
    fn equal_files_count_nothing_in_any_unit() {
        let text = "fn a() {\n    1;\n}\n";
        for phases in [SCOPE, LINE, LINE_TOKEN, ALL] {
            assert_eq!(count(phases, text, text).1, (0, 0), "{phases:?}");
        }
    }
}
