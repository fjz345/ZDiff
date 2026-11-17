use super::*;

pub(super) fn block_toggle_buttons(
    ui: &mut egui::Ui,
    block: &RowBlock,
    [expand_slot, toggle_slot]: [egui::Rect; 2],
    request: &mut Option<(usize, BlockToggle)>,
) {
    let mut toggle_button = |slot: egui::Rect, label: &str, hover: &str, toggle: BlockToggle| {
        if ui
            .put(slot, egui::Button::new(label))
            .on_hover_text(hover)
            .clicked()
        {
            *request = Some((block.key, toggle));
        }
    };
    if !block.expanded {
        toggle_button(
            expand_slot,
            "+",
            "Expand the collapsed rows",
            BlockToggle::Expand,
        );
    }
    // Only on a fully collapsed block: once rows are revealed, the scope expansion has been done
    // or wouldn't add anything.
    if !block.expanded && block.rows.len() == 1 {
        toggle_button(
            toggle_slot,
            "{",
            "Expand up to the line opening the scope of the change below",
            BlockToggle::ExpandToScope,
        );
    } else {
        toggle_button(
            toggle_slot,
            "-",
            "Collapse these rows again",
            BlockToggle::Collapse,
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CenterLayout {
    pub(super) button: f32,
    pub(super) symbol: f32,
    pub(super) toggles: bool,
    pub(super) reverts: bool,
}

impl CenterLayout {
    pub(super) fn measure(mut glyph_width: impl FnMut(char) -> f32, button_padding: f32) -> Self {
        let mut widest = |chars: &[char]| chars.iter().map(|&c| glyph_width(c)).fold(0.0, f32::max);
        Self {
            button: widest(&['+', '{', '-', '<', '>']) + 2.0 * button_padding,
            symbol: widest(&['≠', '-', '+']),
            toggles: true,
            reverts: true,
        }
    }

    pub(super) fn width(self) -> f32 {
        let buttons = f32::from(2 * (u8::from(self.toggles) + u8::from(self.reverts)));
        buttons * self.button + self.symbol + buttons * CENTER_GAP
    }

    /// Expand, scope or collapse, revert-left, symbol, revert-right; at most `height` tall,
    /// centered in `cell`. Absent toggles or reverts are empty and take no room.
    pub(super) fn slots(self, cell: egui::Rect, height: f32) -> [egui::Rect; 5] {
        let half = height.min(cell.height()) / 2.0;
        let y = egui::Rangef::new(cell.center().y - half, cell.center().y + half);
        let mut left = cell.left();
        let toggle = if self.toggles { self.button } else { 0.0 };
        let revert = if self.reverts { self.button } else { 0.0 };
        [toggle, toggle, revert, self.symbol, revert].map(|width| {
            let slot = egui::Rect::from_x_y_ranges(left..=left + width, y);
            if width > 0.0 {
                left += width + CENTER_GAP;
            }
            slot
        })
    }
}

pub(super) struct CenterLayoutCache {
    pub(super) font_id: egui::FontId,
    pub(super) pixels_per_point: f32,
    pub(super) layout: CenterLayout,
}

pub(super) fn center_layout(
    cache: &mut Option<CenterLayoutCache>,
    font_id: &egui::FontId,
    pixels_per_point: f32,
    measure: impl FnOnce() -> CenterLayout,
) -> CenterLayout {
    match cache {
        Some(cached)
            if cached.font_id == *font_id && cached.pixels_per_point == pixels_per_point =>
        {
            cached.layout
        }
        _ => {
            let layout = measure();
            *cache = Some(CenterLayoutCache {
                font_id: font_id.clone(),
                pixels_per_point,
                layout,
            });
            layout
        }
    }
}

pub(super) fn row_symbol(
    row: &DiffRow,
    [delete, insert, modified]: [egui::Color32; 3],
) -> Option<(&'static str, egui::Color32)> {
    let has_op = |content: &LineContent, op: DiffOp| match content {
        LineContent::Code { tokens, .. } => tokens
            .iter()
            .any(|(result, _, _)| !result.hide_in_diff && result.operation == op),
        LineContent::Void | LineContent::Collapsed => false,
    };
    let has_delete = has_op(&row.left, DiffOp::Delete) || has_op(&row.right, DiffOp::Delete);
    let has_insert = has_op(&row.left, DiffOp::Insert) || has_op(&row.right, DiffOp::Insert);
    match (has_delete, has_insert) {
        (true, true) => Some(("≠", modified)),
        (true, false) => Some(("-", delete)),
        (false, true) => Some(("+", insert)),
        (false, false) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    

    use eframe::egui;
    
    use zdiff::{
        diff_builder::{DiffRow, LineContent, RowBg},
        diff_ir::{DiffOp, DiffResult},
    };

    
    #[test]
    fn the_center_layout_is_measured_once_per_font_and_zoom() {
        let mut cache = None;
        let mut measured = 0;
        for (size, pixels_per_point) in [
            (12.0, 1.0),
            (12.0, 1.0),
            (14.0, 1.0),
            (14.0, 1.0),
            (14.0, 1.5),
            (14.0, 1.5),
        ] {
            super::center_layout(
                &mut cache,
                &egui::FontId::monospace(size),
                pixels_per_point,
                || {
                    measured += 1;
                    CenterLayout {
                        button: size,
                        symbol: size,
                        toggles: true,
                        reverts: true,
                    }
                },
            );
        }
        assert_eq!(measured, 3);
    }

    #[test]
    fn the_center_column_fits_its_widest_buttons_and_symbol() {
        let layout = CenterLayout::measure(|c| if c == '≠' { 9.0 } else { 7.0 }, 4.0);
        assert_eq!(
            layout,
            CenterLayout {
                button: 15.0,
                symbol: 9.0,
                toggles: true,
                reverts: true
            }
        );
        assert_eq!(layout.width(), 4.0 * 15.0 + 9.0 + 4.0 * CENTER_GAP);
    }

    #[test]
    fn center_slots_are_toggles_revert_left_symbol_revert_right_edge_to_edge() {
        let layout = CenterLayout {
            button: 15.0,
            symbol: 9.0,
            toggles: true,
            reverts: true,
        };
        let cell =
            egui::Rect::from_min_size(egui::pos2(100.0, 40.0), egui::vec2(layout.width(), 14.0));
        let slots = layout.slots(cell, 18.0);

        assert_eq!(slots.map(|s| s.width()), [15.0, 15.0, 15.0, 9.0, 15.0]);
        assert_eq!(slots[0].left(), cell.left());
        assert_eq!(slots[4].right(), cell.right());
        for pair in slots.windows(2) {
            assert_eq!(pair[1].left() - pair[0].right(), CENTER_GAP);
        }
        for slot in slots {
            assert_eq!(slot.y_range(), cell.y_range());
        }
    }

    #[test]
    fn center_without_block_toggles_has_only_revert_buttons_and_symbol() {
        let layout = CenterLayout {
            button: 15.0,
            symbol: 9.0,
            toggles: false,
            reverts: true,
        };
        assert_eq!(layout.width(), 2.0 * 15.0 + 9.0 + 2.0 * CENTER_GAP);

        let cell =
            egui::Rect::from_min_size(egui::pos2(100.0, 40.0), egui::vec2(layout.width(), 14.0));
        let [expand, toggle, left, symbol, right] = layout.slots(cell, 18.0);

        assert_eq!(expand.width(), 0.0);
        assert_eq!(toggle.width(), 0.0);
        assert_eq!(left.left(), cell.left());
        assert_eq!(right.right(), cell.right());
        assert_eq!(symbol.left() - left.right(), CENTER_GAP);
        assert_eq!(right.left() - symbol.right(), CENTER_GAP);
    }

    #[test]
    fn center_without_revert_buttons_has_only_block_toggles_and_symbol() {
        let layout = CenterLayout {
            button: 15.0,
            symbol: 9.0,
            toggles: true,
            reverts: false,
        };
        assert_eq!(layout.width(), 2.0 * 15.0 + 9.0 + 2.0 * CENTER_GAP);

        let cell =
            egui::Rect::from_min_size(egui::pos2(100.0, 40.0), egui::vec2(layout.width(), 14.0));
        let [expand, toggle, left, symbol, right] = layout.slots(cell, 18.0);

        assert_eq!(left.width(), 0.0);
        assert_eq!(right.width(), 0.0);
        assert_eq!(expand.left(), cell.left());
        assert_eq!(toggle.left() - expand.right(), CENTER_GAP);
        assert_eq!(symbol.left() - toggle.right(), CENTER_GAP);
        assert_eq!(symbol.right(), cell.right());
    }

    #[test]
    fn center_without_any_buttons_is_just_the_symbol() {
        let layout = CenterLayout {
            button: 15.0,
            symbol: 9.0,
            toggles: false,
            reverts: false,
        };
        assert_eq!(layout.width(), 9.0);

        let cell =
            egui::Rect::from_min_size(egui::pos2(100.0, 40.0), egui::vec2(layout.width(), 14.0));
        let [expand, toggle, left, symbol, right] = layout.slots(cell, 18.0);

        for button in [expand, toggle, left, right] {
            assert_eq!(button.width(), 0.0);
        }
        assert_eq!(symbol.x_range(), cell.x_range());
    }

    #[test]
    fn center_slots_in_a_tall_row_are_one_button_high_and_centered() {
        let layout = CenterLayout {
            button: 15.0,
            symbol: 9.0,
            toggles: true,
            reverts: true,
        };
        let cell =
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(layout.width(), 50.0));
        for slot in layout.slots(cell, 18.0) {
            assert_eq!(slot.height(), 18.0);
            assert_eq!(slot.center().y, cell.center().y);
        }
    }

    fn code(ops: &[(DiffOp, bool)]) -> LineContent {
        LineContent::Code {
            tokens: ops
                .iter()
                .map(|(operation, hide_in_diff)| {
                    (
                        DiffResult {
                            operation: operation.clone(),
                            token_source_idx: None,
                            token_target_idx: None,
                            hide_in_diff: *hide_in_diff,
                        },
                        false,
                        false,
                    )
                })
                .collect(),
            line_num: 1,
            bg: RowBg::None,
            changed_chars: Vec::new(),
        }
    }

    #[test]
    fn only_changed_rows_get_a_colored_symbol() {
        use eframe::egui::Color32;
        let (delete, insert, modified) = (Color32::RED, Color32::GREEN, Color32::YELLOW);
        let symbol =
            |left, right| super::row_symbol(&DiffRow { left, right }, [delete, insert, modified]);
        const EQUAL: (DiffOp, bool) = (DiffOp::Equal(true), false);
        const DELETE: (DiffOp, bool) = (DiffOp::Delete, false);
        const INSERT: (DiffOp, bool) = (DiffOp::Insert, false);

        assert_eq!(
            symbol(code(&[EQUAL, DELETE]), LineContent::Void),
            Some(("-", delete))
        );
        assert_eq!(
            symbol(LineContent::Void, code(&[INSERT])),
            Some(("+", insert))
        );
        assert_eq!(
            symbol(code(&[EQUAL, DELETE]), code(&[EQUAL, INSERT])),
            Some(("≠", modified))
        );
        assert_eq!(symbol(code(&[EQUAL]), code(&[EQUAL])), None);
        assert_eq!(
            symbol(code(&[(DiffOp::Delete, true)]), LineContent::Void),
            None
        );
        assert_eq!(symbol(LineContent::Collapsed, LineContent::Collapsed), None);
        assert_eq!(symbol(LineContent::Void, LineContent::Void), None);
    }

}
