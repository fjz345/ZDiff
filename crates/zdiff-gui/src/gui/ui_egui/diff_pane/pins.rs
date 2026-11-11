use super::*;

#[derive(Default)]
pub(super) struct PinInput {
    pub(super) left: String,
    pub(super) right: String,
    pub(super) error: Option<String>,
}

pub(super) const PIN_MARKER: egui::Color32 = egui::Color32::from_rgb(90, 160, 255);

pub(super) fn shown_pins(diff_ctx: &MinimalDiffCtx) -> &[Pin] {
    if diff_ctx.ignored_pins.is_some() {
        &[]
    } else {
        &diff_ctx.input.pins
    }
}

pub(super) fn is_pinned(pins: &[Pin], side: ActiveSide, content: &LineContent) -> bool {
    let LineContent::Code { line_num, .. } = content else {
        return false;
    };
    let Ok(line) = usize::try_from(line_num - 1) else {
        return false;
    };
    pins.iter().any(|pin| match side {
        ActiveSide::Left => pin.source == line,
        ActiveSide::Right => pin.target == line,
    })
}

pub(super) fn paint_pin_marker(ui: &egui::Ui) {
    let rect = ui.max_rect();
    let bar = egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height()));
    ui.painter().rect_filled(bar, 0.0, PIN_MARKER);
}

pub(super) fn pins_ui(
    ui: &mut egui::Ui,
    input: &mut PinInput,
    pins: &[Pin],
    pins_request: &mut Option<Vec<Pin>>,
    diff_ctx: Option<&MinimalDiffCtx>,
) {
    ui.label("Pin");
    let left = ui.add(
        TextEdit::singleline(&mut input.left)
            .hint_text("L")
            .desired_width(35.0),
    );
    let right = ui.add(
        TextEdit::singleline(&mut input.right)
            .hint_text("R")
            .desired_width(35.0),
    );
    if left.changed() || right.changed() {
        input.error = None;
    }
    let entered =
        (left.lost_focus() || right.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if ui.button("Add pin").clicked() || entered {
        add_pin(input, pins, pins_request, diff_ctx);
    }
    if !pins.is_empty() {
        ui.menu_button(format!("{} pinned", pins.len()), |ui| {
            for (i, pin) in pins.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("{} : {}", pin.source + 1, pin.target + 1));
                    if zcommon_ui::common::icon_button(ui, "✖", "Remove pin").clicked() {
                        let mut kept = pins.to_vec();
                        kept.remove(i);
                        *pins_request = Some(kept);
                    }
                });
            }
            if ui.button("Clear all").clicked() {
                *pins_request = Some(Vec::new());
                ui.close();
            }
        });
    }
    let ignored = diff_ctx.and_then(|d| d.ignored_pins.as_ref());
    if let Some(reason) = &input.error {
        ui.colored_label(ui.visuals().warn_fg_color, reason);
    } else if let Some(err) = ignored {
        ui.colored_label(ui.visuals().warn_fg_color, format!("Pins ignored: {err}"));
    }
}

pub(super) fn add_pin(
    input: &mut PinInput,
    pins: &[Pin],
    pins_request: &mut Option<Vec<Pin>>,
    diff_ctx: Option<&MinimalDiffCtx>,
) {
    let added = parse_pin(&input.left, &input.right).and_then(|pin| {
        let counts = diff_ctx
            .and_then(|d| pin_line_counts(&d.input))
            .ok_or("pins need both files loaded")?;
        insert_pin(pins, pin, counts).map_err(|err| err.to_string())
    });
    match added {
        Ok(pins) => {
            *pins_request = Some(pins);
            *input = PinInput::default();
        }
        Err(reason) => input.error = Some(reason),
    }
}

/// The 1-based line fields as a 0-based pin.
pub(super) fn parse_pin(left: &str, right: &str) -> Result<Pin, String> {
    let line = |text: &str| match text.trim().parse::<usize>() {
        Ok(line) if line > 0 => Ok(line - 1),
        _ => Err(format!("\"{}\" is not a line number", text.trim())),
    };
    Ok(Pin {
        source: line(left)?,
        target: line(right)?,
    })
}

#[cfg(test)]
mod tests {
    
    

    
    
    use zdiff::diff_builder::{LineContent, RowBg};

    
    #[test]
    fn pin_fields_are_one_based_line_numbers() {
        use zdiff::myers::Pin;
        assert_eq!(
            super::parse_pin("1", " 12 "),
            Ok(Pin {
                source: 0,
                target: 11
            })
        );
        assert!(super::parse_pin("0", "3").is_err());
        assert!(super::parse_pin("2", "").is_err());
        assert!(super::parse_pin("x", "3").is_err());
    }

    #[test]
    fn pinned_lines_are_marked_on_their_own_side() {
        use crate::ui_egui::active_side::ActiveSide::{Left, Right};
        use zdiff::myers::Pin;
        let code = |line_num| LineContent::Code {
            tokens: Vec::new(),
            line_num,
            bg: RowBg::None,
            changed_chars: Vec::new(),
        };
        let pins = [Pin {
            source: 2,
            target: 6,
        }];
        assert!(super::is_pinned(&pins, Left, &code(3)));
        assert!(super::is_pinned(&pins, Right, &code(7)));
        assert!(!super::is_pinned(&pins, Right, &code(3)));
        assert!(!super::is_pinned(&pins, Left, &code(7)));
        assert!(!super::is_pinned(&pins, Left, &code(-1)));
        assert!(!super::is_pinned(&pins, Left, &LineContent::Void));
    }

}
