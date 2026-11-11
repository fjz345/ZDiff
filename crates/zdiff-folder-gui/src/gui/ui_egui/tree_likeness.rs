use eframe::egui;
use zcommon_ui::{
    change_map::change_preview_ui,
    likeness::{likeness_bar, percent},
};
use zdiff_folder::{
    comparison::PairState,
    fs::{FileSystemModel, FsNode, FsNodeId},
    likeness::{Likeness, LikenessReason, Measure},
    vcs_source::ContentSource,
};

use crate::ui_egui::{
    fs_tree::{DiffState, FileSystemView, VisibleRowTwoFolderDiff},
    tree_compare::{PairLikeness, TreeCompare, pair_change_map},
};

const PREVIEW_PAIRS: usize = 8;

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

#[derive(Clone, Copy)]
pub struct PreviewPair<'a> {
    rel_path: &'a str,
    models: [&'a FileSystemModel; 2],
    ids: [FsNodeId; 2],
}

impl<'a> PreviewPair<'a> {
    pub fn new(
        views: [Option<&'a FileSystemView>; 2],
        entry: &'a VisibleRowTwoFolderDiff,
    ) -> Option<Self> {
        let [Some(left), Some(right)] = views else {
            return None;
        };
        let models = [&*left.file_system, &*right.file_system];
        // Like the likeness, the preview maps the files from disk.
        if models
            .iter()
            .any(|model| *model.content() != ContentSource::Disk)
        {
            return None;
        }
        Some(Self {
            rel_path: &entry.rel_path,
            models,
            ids: [entry.diff_state.first()?, entry.diff_state.second()?],
        })
    }
}

fn preview_ui(ui: &mut egui::Ui, pair: PreviewPair) {
    let [Some(left), Some(right)] = [0, 1].map(|i| pair.models[i].get_node(pair.ids[i])) else {
        return;
    };
    let paths = [0, 1].map(|i| pair.models[i].path(pair.ids[i]));
    let key = (
        paths.clone(),
        [left, right].map(|node| (node.size, node.modified)),
    );
    let rel_path = pair.rel_path.to_owned();
    ui.separator();
    change_preview_ui(
        ui,
        egui::Id::new("tree_likeness_previews"),
        PREVIEW_PAIRS,
        key,
        move |cancel| {
            pair_change_map(&paths, &rel_path, cancel).map(|map| map.map_err(|e| e.to_string()))
        },
    );
}

pub fn ui_likeness_cell(
    ui: &mut egui::Ui,
    cell: LikenessCell,
    row_height: f32,
    preview: Option<PreviewPair>,
) {
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
            bar(ui, likeness.ratio, row_height).on_hover_ui(|ui| {
                ui.label(tooltip(likeness));
                if let Some(pair) = preview {
                    preview_ui(ui, pair);
                }
            });
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
    likeness_bar(ui, ratio, egui::vec2(ui.available_width(), row_height))
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
