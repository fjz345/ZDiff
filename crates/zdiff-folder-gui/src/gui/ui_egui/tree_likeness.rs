use eframe::egui;
use zdiff_folder::{
    comparison::PairState,
    fs::FsNode,
    likeness::{Likeness, LikenessReason, Measure},
};

use crate::ui_egui::{
    fs_tree::{DiffState, VisibleRowTwoFolderDiff},
    tree_compare::{PairLikeness, TreeCompare},
};

const UNLIKE_COLOR: egui::Color32 = egui::Color32::from_rgb(0xB0, 0x3A, 0x2E);
const HALF_LIKE_COLOR: egui::Color32 = egui::Color32::from_rgb(0xA8, 0x8C, 0x1E);
const ALIKE_COLOR: egui::Color32 = egui::Color32::from_rgb(0x3A, 0x8C, 0x3A);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FolderLikeness {
    Known(f64),
    Unknown,
}

/// The likeness of the files below a folder, each weighted by its larger side's size.
#[derive(Debug, Default, Clone, Copy)]
pub struct LikenessSum {
    weighted: f64,
    weight: u64,
    ratios: f64,
    files: usize,
    unknown: bool,
}

impl LikenessSum {
    pub fn file(ratio: Option<f64>, weight: u64) -> Self {
        let ratio_or_0 = ratio.unwrap_or(0.0);
        Self {
            weighted: ratio_or_0 * weight as f64,
            weight,
            ratios: ratio_or_0,
            files: 1,
            unknown: ratio.is_none(),
        }
    }

    pub fn one_sided(size: u64) -> Self {
        Self::file(Some(0.0), size)
    }

    /// `None` leaves the pair out: Similar before its compare, its content is never read.
    pub fn pair(
        exact: Option<PairState>,
        likeness: Option<f64>,
        similar: bool,
        sizes: [u64; 2],
    ) -> Option<Self> {
        let ratio = match (exact, likeness) {
            (Some(PairState::Same), _) => Some(1.0),
            (_, Some(ratio)) => Some(ratio),
            (None, None) if similar => return None,
            _ => None,
        };
        Some(Self::file(ratio, sizes[0].max(sizes[1])))
    }

    pub fn add(&mut self, other: LikenessSum) {
        self.weighted += other.weighted;
        self.weight += other.weight;
        self.ratios += other.ratios;
        self.files += other.files;
        self.unknown |= other.unknown;
    }

    pub fn folder_likeness(self) -> Option<FolderLikeness> {
        if self.files == 0 {
            return None;
        }
        if self.unknown {
            return Some(FolderLikeness::Unknown);
        }
        if self.weight == 0 {
            return Some(FolderLikeness::Known(self.ratios / self.files as f64));
        }
        Some(FolderLikeness::Known(self.weighted / self.weight as f64))
    }
}

pub enum LikenessCell<'a> {
    Empty,
    Waiting,
    Same,
    Known(&'a PairLikeness),
    Folder(f64),
}

/// Only rows present on both sides get a likeness.
pub fn likeness_cell<'a>(
    entry: &VisibleRowTwoFolderDiff,
    nodes: [Option<&FsNode>; 2],
    compare: &'a TreeCompare,
) -> LikenessCell<'a> {
    match entry.folder_likeness {
        Some(FolderLikeness::Known(ratio)) => return LikenessCell::Folder(ratio),
        Some(FolderLikeness::Unknown) if compare.is_comparing() => return LikenessCell::Waiting,
        Some(FolderLikeness::Unknown) => return LikenessCell::Empty,
        None => {}
    }
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
        LikenessCell::Folder(ratio) => {
            bar(ui, ratio, row_height).on_hover_text(format!(
                "{} % of the content below alike: the files' likeness weighted by size, \
                 one-sided files 0 %",
                percent(ratio)
            ));
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
    painter.rect_filled(filled, 2.0, likeness_color(ratio));
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("{}%", percent(ratio)),
        egui::TextStyle::Monospace.resolve(ui.style()),
        ui.visuals().strong_text_color(),
    );
    response
}

fn likeness_color(ratio: f64) -> egui::Color32 {
    let t = ratio.clamp(0.0, 1.0) as f32 * 2.0;
    if t <= 1.0 {
        UNLIKE_COLOR.lerp_to_gamma(HALF_LIKE_COLOR, t)
    } else {
        HALF_LIKE_COLOR.lerp_to_gamma(ALIKE_COLOR, t - 1.0)
    }
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
