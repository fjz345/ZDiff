use std::ops::Range;

use eframe::egui;
use zdiff::{diff_builder::DiffBuilderOptions, myers::DiffPhases};

const CHARACTER_NEEDS_TOKEN: &str = "Character needs Token";

/// (label, flag, reason the toggle is disabled).
pub fn phase_toggles(
    phases: &mut DiffPhases,
) -> [(&'static str, &mut bool, Option<&'static str>); 4] {
    let DiffPhases {
        scope,
        line,
        token,
        character,
    } = phases;
    let token_disabled = (*token && *character).then_some(CHARACTER_NEEDS_TOKEN);
    let character_disabled = (!*token).then_some(CHARACTER_NEEDS_TOKEN);
    [
        ("Scope", scope, None),
        ("Line", line, None),
        ("Token", token, token_disabled),
        ("Character", character, character_disabled),
    ]
}

pub fn diff_phases_menu(ui: &mut egui::Ui, phases: &mut DiffPhases) {
    ui.menu_button("Diff phases", |ui| {
        for (label, on, disabled) in phase_toggles(phases) {
            let response = ui.add_enabled(disabled.is_none(), egui::Checkbox::new(on, label));
            if let Some(reason) = disabled {
                response.on_disabled_hover_text(reason);
            }
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marks {
    pub scope: bool,
    pub line: bool,
    pub tokens: bool,
    pub chars: bool,
}

impl Marks {
    /// Without the character phase the finest known change is a whole token.
    pub fn new(options: &DiffBuilderOptions, character_phase: bool) -> Self {
        let on = options.highlight_rows;
        Self {
            scope: on && options.mark_scopes,
            line: on && options.mark_lines,
            tokens: on && (options.mark_tokens || (options.mark_chars && !character_phase)),
            chars: on && options.mark_chars && character_phase,
        }
    }

    pub fn any(self) -> bool {
        self.scope || self.line || self.tokens || self.chars
    }
}

/// `scopes`: sorted 0-based line ranges. `line_num`: 1-based, or not positive for no line.
pub fn in_scopes(scopes: &[Range<usize>], line_num: i32) -> bool {
    let Some(line) = usize::try_from(line_num - 1).ok() else {
        return false;
    };
    let i = scopes.partition_point(|s| s.end <= line);
    scopes.get(i).is_some_and(|s| s.start <= line)
}

pub fn diff_marks_checkboxes(ui: &mut egui::Ui, options: &mut DiffBuilderOptions) {
    ui.checkbox(&mut options.mark_scopes, "Scope");
    ui.checkbox(&mut options.mark_lines, "Line");
    ui.checkbox(&mut options.mark_tokens, "Token");
    ui.checkbox(&mut options.mark_chars, "Character");
}

#[cfg(test)]
mod tests {
    use zdiff::diff_builder::DiffBuilderOptions;

    use super::*;

    #[test]
    fn the_character_mark_falls_back_to_tokens_without_the_character_phase() {
        let options = DiffBuilderOptions {
            mark_lines: false,
            mark_chars: true,
            ..Default::default()
        };
        let marks = |character_phase| Marks::new(&options, character_phase);
        assert!(marks(true).chars && !marks(true).tokens);
        assert!(!marks(false).chars && marks(false).tokens);
        assert!(!marks(false).line && !marks(false).scope);
    }

    #[test]
    fn the_default_marks_only_lines() {
        let marks = Marks::new(&DiffBuilderOptions::default(), true);
        assert_eq!(
            marks,
            Marks {
                scope: false,
                line: true,
                tokens: false,
                chars: false,
            }
        );
    }

    #[test]
    fn highlight_rows_off_turns_every_mark_off() {
        let options = DiffBuilderOptions {
            highlight_rows: false,
            mark_scopes: true,
            mark_lines: true,
            mark_tokens: true,
            mark_chars: true,
            ..Default::default()
        };
        assert!(!Marks::new(&options, true).any());
        assert!(!Marks::new(&options, false).any());
    }

    #[test]
    fn marks_are_any_when_one_is_on() {
        let none = Marks {
            scope: false,
            line: false,
            tokens: false,
            chars: false,
        };
        assert!(!none.any());
        for one in [
            Marks {
                scope: true,
                ..none
            },
            Marks { line: true, ..none },
            Marks {
                tokens: true,
                ..none
            },
            Marks {
                chars: true,
                ..none
            },
        ] {
            assert!(one.any());
        }
    }

    #[test]
    fn a_line_is_in_a_scope_when_its_number_falls_inside_one() {
        let scopes = [1..4, 6..7];
        let inside: Vec<i32> = (-1..9).filter(|&n| in_scopes(&scopes, n)).collect();
        assert_eq!(inside, [2, 3, 4, 7]);
    }

    #[test]
    fn the_diff_defaults_to_the_line_phase_only() {
        assert_eq!(
            DiffBuilderOptions::default().phases,
            DiffPhases {
                scope: false,
                line: true,
                token: false,
                character: false,
            }
        );
    }

    #[test]
    fn enabled_toggles_keep_a_valid_set_valid_and_disabled_ones_would_break_it() {
        let all_sets = (0..16u8).map(|bits| DiffPhases {
            scope: bits & 1 != 0,
            line: bits & 2 != 0,
            token: bits & 4 != 0,
            character: bits & 8 != 0,
        });
        for phases in all_sets.filter(|p| p.is_valid()) {
            for i in 0..4 {
                let mut flipped = phases;
                let (label, on, disabled) = phase_toggles(&mut flipped).into_iter().nth(i).unwrap();
                *on = !*on;
                assert_eq!(
                    flipped.is_valid(),
                    disabled.is_none(),
                    "{label} from {phases:?} (disabled: {disabled:?})"
                );
            }
        }
    }
}
