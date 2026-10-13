use eframe::egui;
use zhashdiff::{
    fs::FsNode,
    likeness::{Likeness, LikenessReason, Measure},
};

use crate::ui_egui::{
    fs_tree::{DiffState, VisibleRowTwoFolderDiff},
    tree_compare::{PairLikeness, TreeCompare},
};

const BAR_COLOR: egui::Color32 = egui::Color32::from_rgb(0x2E, 0x6B, 0x8F);

pub enum LikenessCell<'a> {
    Empty,
    Waiting,
    Same,
    Known(&'a PairLikeness),
}

/// Only file rows present on both sides get a likeness.
pub fn likeness_cell<'a>(
    entry: &VisibleRowTwoFolderDiff,
    nodes: [Option<&FsNode>; 2],
    compare: &'a TreeCompare,
) -> LikenessCell<'a> {
    let [Some(left), Some(right)] = nodes else {
        return LikenessCell::Empty;
    };
    if !left.is_file() || !right.is_file() {
        return LikenessCell::Empty;
    }
    let known = match entry.diff_state {
        DiffState::Same(..) => return LikenessCell::Same,
        DiffState::Different(..) => compare.likeness_of(&entry.rel_path, [left, right]),
        // Similar before its compare: never read, so never measured.
        DiffState::Similar(..) if compare.state_of(&entry.rel_path, [left, right]).is_none() => {
            return LikenessCell::Empty;
        }
        DiffState::Similar(..) => compare.likeness_of(&entry.rel_path, [left, right]),
        DiffState::Pending(..) => None,
        _ => return LikenessCell::Empty,
    };
    match known {
        Some(likeness) => LikenessCell::Known(likeness),
        // Nothing measures it after a cancel.
        None if compare.is_comparing() => LikenessCell::Waiting,
        None => LikenessCell::Empty,
    }
}

/// Floored, so a pair that differs reads 100 % only when measured so.
pub fn percent(ratio: f64) -> u32 {
    // The epsilon keeps 0.29 * 100 = 28.999... at 29.
    (ratio * 100.0 + 1e-9).floor().clamp(0.0, 100.0) as u32
}

pub fn ui_likeness_cell(ui: &mut egui::Ui, cell: LikenessCell, row_height: f32) {
    match cell {
        LikenessCell::Empty => {}
        LikenessCell::Waiting => {
            ui.add(egui::Spinner::new().size(row_height))
                .on_hover_text("likeness not measured yet");
        }
        LikenessCell::Same => {
            bar(ui, 1.0, row_height).on_hover_text("Same: 100 %");
        }
        LikenessCell::Known(PairLikeness::Measured(likeness)) => {
            bar(ui, likeness.ratio, row_height).on_hover_text(tooltip(likeness));
        }
        LikenessCell::Known(PairLikeness::Failed(error)) => {
            ui.add(egui::Label::new("?").selectable(false))
                .on_hover_text(format!("Likeness not measured: {error}"));
        }
    }
}

fn bar(ui: &mut egui::Ui, ratio: f64, row_height: f32) -> egui::Response {
    let size = egui::vec2(ui.available_width(), row_height);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    let mut filled = rect;
    filled.set_width(rect.width() * ratio.clamp(0.0, 1.0) as f32);
    painter.rect_filled(filled, 2.0, BAR_COLOR);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("{}%", percent(ratio)),
        egui::TextStyle::Monospace.resolve(ui.style()),
        ui.visuals().strong_text_color(),
    );
    response
}

fn tooltip(likeness: &Likeness) -> String {
    let measure = match likeness.measure {
        Measure::Lines => "matching lines",
        Measure::Pixels => "pixels within tolerance",
        Measure::Bytes => "equal bytes at the same offsets",
    };
    let text = format!("{} % {measure}", percent(likeness.ratio));
    match &likeness.reason {
        None => text,
        Some(LikenessReason::DimensionsDiffer { left, right }) => format!(
            "{text}: different size, {}x{} vs {}x{}",
            left.0, left.1, right.0, right.1
        ),
        Some(LikenessReason::ImageDecode(error)) => {
            format!("{text}: the image could not be decoded ({error})")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::percent;

    #[test]
    fn percent_is_floored_but_exact_fractions_stay_whole() {
        assert_eq!(percent(0.997), 99);
        assert_eq!(percent(1.0), 100);
        assert_eq!(percent(0.29), 29);
        assert_eq!(percent(2.0 * 7.0 / 50.0), 28);
        assert_eq!(percent(0.0), 0);
    }
}
