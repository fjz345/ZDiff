use eframe::egui;
use zdiff::myers::DiffPhases;

const LINE_OR_TOKEN: &str = "Line or Token must stay on";
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
    let line_disabled = (*line && !*token).then_some(LINE_OR_TOKEN);
    let token_disabled = if *token && !*line {
        Some(LINE_OR_TOKEN)
    } else {
        (*token && *character).then_some(CHARACTER_NEEDS_TOKEN)
    };
    let character_disabled = (!*token).then_some(CHARACTER_NEEDS_TOKEN);
    [
        ("Scope", scope, None),
        ("Line", line, line_disabled),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn all_sets() -> impl Iterator<Item = DiffPhases> {
        (0..16u8).map(|bits| DiffPhases {
            scope: bits & 1 != 0,
            line: bits & 2 != 0,
            token: bits & 4 != 0,
            character: bits & 8 != 0,
        })
    }

    #[test]
    fn enabled_toggles_keep_a_valid_set_valid_and_disabled_ones_would_break_it() {
        for phases in all_sets().filter(|p| p.is_valid()) {
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

    #[test]
    fn the_defaults_are_line_and_token() {
        assert_eq!(
            DiffPhases::default(),
            DiffPhases {
                scope: false,
                line: true,
                token: true,
                character: false,
            }
        );
    }
}
