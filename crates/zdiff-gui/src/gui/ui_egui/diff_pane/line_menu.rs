use std::path::Path;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MenuLine {
    pub(super) side: ActiveSide,
    pub(super) line: usize,
    pub(super) diff_row: usize,
}

impl MenuLine {
    pub(super) fn of(side: ActiveSide, content: &LineContent, diff_row: usize) -> Option<Self> {
        let LineContent::Code { line_num, .. } = content else {
            return None;
        };
        let line = usize::try_from(line_num - 1).ok()?;
        Some(Self {
            side,
            line,
            diff_row,
        })
    }
}

pub(super) struct LineMenuCtx<'a> {
    pub(super) diff_ctx: &'a MinimalDiffCtx,
    pub(super) pins: &'a [Pin],
    pub(super) pin_input: &'a mut PinInput,
    pub(super) pins_request: &'a mut Option<Vec<Pin>>,
    pub(super) revert_request: &'a mut Option<RevertRequest>,
    pub(super) temp_root: &'a Path,
    pub(super) shown_sides: [usize; 2],
}

fn other(side: ActiveSide) -> ActiveSide {
    match side {
        ActiveSide::Left => ActiveSide::Right,
        ActiveSide::Right => ActiveSide::Left,
    }
}

fn side_name(side: ActiveSide, shown_sides: [usize; 2]) -> &'static str {
    ["left", "right"][shown_sides[side as usize]]
}

fn pin_field(input: &mut PinInput, side: ActiveSide) -> &mut String {
    match side {
        ActiveSide::Left => &mut input.left,
        ActiveSide::Right => &mut input.right,
    }
}

fn pin_line(pin: &Pin, side: ActiveSide) -> usize {
    match side {
        ActiveSide::Left => pin.source,
        ActiveSide::Right => pin.target,
    }
}

pub(super) fn start_pin(input: &mut PinInput, side: ActiveSide, line: usize) {
    *pin_field(input, side) = (line + 1).to_string();
    pin_field(input, other(side)).clear();
    input.error = None;
}

pub(super) fn pending_partner(input: &mut PinInput, side: ActiveSide) -> Option<usize> {
    let line = pin_field(input, other(side)).trim().parse::<usize>().ok()?;
    line.checked_sub(1)
}

pub(super) fn without_line_pins(pins: &[Pin], side: ActiveSide, line: usize) -> Vec<Pin> {
    pins.iter()
        .filter(|pin| pin_line(pin, side) != line)
        .copied()
        .collect()
}

// `line_starts` holds 0, then the offset of each line break's last byte (not the byte after it).
pub(crate) fn line_text<'a>(
    contents: &'a str,
    line_starts: &[usize],
    line: usize,
) -> Option<&'a str> {
    let start = match line {
        0 => 0,
        _ => line_starts.get(line)? + 1,
    };
    let end = line_starts.get(line + 1).copied().unwrap_or(contents.len());
    let text = contents.get(start..end)?;
    Some(text.strip_suffix('\r').unwrap_or(text))
}

pub(super) fn line_menu(ui: &mut egui::Ui, at: MenuLine, ctx: LineMenuCtx) {
    let name_of = |side| side_name(side, ctx.shown_sides);
    let (name, other_name) = (name_of(at.side), name_of(other(at.side)));
    let shown = at.line + 1;

    if let Some(partner) = pending_partner(ctx.pin_input, at.side)
        && ui
            .button(format!("Pin to {other_name} line {}", partner + 1))
            .clicked()
    {
        *pin_field(ctx.pin_input, at.side) = shown.to_string();
        add_pin(
            ctx.pin_input,
            ctx.pins,
            ctx.pins_request,
            Some(ctx.diff_ctx),
        );
        ui.close();
    }
    if ui
        .button(format!("Pin {name} line {shown} to..."))
        .on_hover_text(format!(
            "Then right-click a {other_name} line, or type it in the Pin field"
        ))
        .clicked()
    {
        start_pin(ctx.pin_input, at.side, at.line);
        ui.close();
    }
    if ctx.pins.iter().any(|pin| pin_line(pin, at.side) == at.line)
        && ui.button(format!("Unpin {name} line {shown}")).clicked()
    {
        *ctx.pins_request = Some(without_line_pins(ctx.pins, at.side, at.line));
        ui.close();
    }

    let file = match at.side {
        ActiveSide::Left => &ctx.diff_ctx.input.file_1,
        ActiveSide::Right => &ctx.diff_ctx.input.file_2,
    };
    if let Some(file) = file {
        ui.separator();
        if ui.button("Copy line").clicked() {
            let text = line_text(&file.contents, &file.metadata.line_starts, at.line);
            ui.ctx().copy_text(text.unwrap_or_default().to_owned());
            ui.close();
        }
        if ui.button("Copy path:line").clicked() {
            ui.ctx().copy_text(format!("{}:{shown}", file.path));
            ui.close();
        }
    }

    let Some(hunk) = revert::hunk_containing(&ctx.diff_ctx.precomputed_diffs, at.diff_row) else {
        return;
    };
    let targets = [
        (RevertTarget::Left, ActiveSide::Left),
        (RevertTarget::Right, ActiveSide::Right),
    ];
    let mut separated = false;
    for (target, side) in targets {
        let label = format!(
            "Replace this hunk in the {} file with the {} side",
            name_of(side),
            name_of(other(side))
        );
        if revert::check_revert(ctx.diff_ctx, target, ctx.temp_root).is_err() {
            continue;
        }
        if !separated {
            ui.separator();
            separated = true;
        }
        if ui.button(label).clicked() {
            *ctx.revert_request = Some(RevertRequest { hunk, target });
            ui.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zdiff::cached_file::FileMetadata;

    #[test]
    fn line_text_is_each_line_without_its_line_ending() {
        let contents = "first\nsecond\r\n\nlast";
        let starts = FileMetadata::new(contents).line_starts;
        let lines: Vec<_> = (0..5).map(|i| line_text(contents, &starts, i)).collect();
        assert_eq!(
            lines,
            [Some("first"), Some("second"), Some(""), Some("last"), None]
        );
    }

    #[test]
    fn menu_lines_are_zero_based_code_lines() {
        let code = |line_num| LineContent::Code {
            tokens: Vec::new(),
            line_num,
            bg: RowBg::None,
            changed_chars: Vec::new(),
        };
        assert_eq!(
            MenuLine::of(ActiveSide::Right, &code(4), 9),
            Some(MenuLine {
                side: ActiveSide::Right,
                line: 3,
                diff_row: 9
            })
        );
        assert_eq!(MenuLine::of(ActiveSide::Left, &code(0), 0), None);
        assert_eq!(MenuLine::of(ActiveSide::Left, &LineContent::Void, 0), None);
    }

    #[test]
    fn a_started_pin_waits_for_a_line_on_the_other_side() {
        let mut input = PinInput {
            right: "7".to_owned(),
            error: Some("old".to_owned()),
            ..PinInput::default()
        };
        start_pin(&mut input, ActiveSide::Left, 11);
        assert_eq!((input.left.as_str(), input.right.as_str()), ("12", ""));
        assert_eq!(input.error, None);
        assert_eq!(pending_partner(&mut input, ActiveSide::Right), Some(11));
        assert_eq!(pending_partner(&mut input, ActiveSide::Left), None);
    }

    #[test]
    fn unpinning_a_line_drops_only_the_pins_on_that_side_and_line() {
        let pins = [
            Pin {
                source: 2,
                target: 5,
            },
            Pin {
                source: 5,
                target: 9,
            },
        ];
        assert_eq!(
            without_line_pins(&pins, ActiveSide::Left, 5),
            [Pin {
                source: 2,
                target: 5
            }]
        );
        assert_eq!(
            without_line_pins(&pins, ActiveSide::Right, 5),
            [Pin {
                source: 5,
                target: 9
            }]
        );
    }
}
