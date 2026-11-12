use std::{collections::HashMap, path::PathBuf};

use eframe::egui::{self};
use serde::{Deserialize, Serialize};
use zcommon::{
    hash::HashService,
    logger::LogBuffer,
};
use zcommon_ui::pane::{ZAppPane, toolbar_group};
use zdiff_folder::{external_diff_tool::DiffToolConfig, filter::PathFilter};

use crate::ui_egui::{
    app::SideScan,
    duplicate_files_pane::{DuplicateFilesPane, DuplicateFilesPaneCtx},
    folder_keymap::FolderAction,
    fs_tree::{FileSystemView, HScrollLink, RowsId, VisibleRowTwoFolderDiff},
    path_diff_pane::{FilterPresets, PaneView, PathDiffPane, PathDiffPaneCtx},
    status_line::ToolMessage,
    tree_compare::TreeCompare,
    tree_filter::{DiffFilter, DuplicatesFilter},
    tree_find::TreeFindView,
    tree_sort::TreeSort,
    tree_tolerance::Tolerance,
};

pub struct PathDiffView<'a> {
    pub file_system_1_root_path: &'a mut Option<PathBuf>,
    pub file_system_2_root_path: &'a mut Option<PathBuf>,
    pub file_system_1_view: &'a mut Option<FileSystemView>,
    pub file_system_2_view: &'a mut Option<FileSystemView>,
    pub file_system_1_scan: &'a mut SideScan,
    pub visible_rows: &'a mut Option<Vec<VisibleRowTwoFolderDiff>>,
    pub rows_id: RowsId,
    pub previous_roots: [Option<PathBuf>; 2],
    pub scanning: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewChoice {
    Tree,
    Map,
    Duplicates,
}

impl ViewChoice {
    pub fn pane_view(self) -> Option<PaneView> {
        match self {
            Self::Tree => Some(PaneView::Tree),
            Self::Map => Some(PaneView::Map),
            Self::Duplicates => None,
        }
    }
}

impl From<PaneView> for ViewChoice {
    fn from(view: PaneView) -> Self {
        match view {
            PaneView::Tree => Self::Tree,
            PaneView::Map => Self::Map,
        }
    }
}

pub fn view_switch(ui: &mut egui::Ui, current: ViewChoice) -> Option<ViewChoice> {
    let mut choice = current;
    toolbar_group(ui, |ui| {
        ui.selectable_value(&mut choice, ViewChoice::Tree, "Tree");
        ui.selectable_value(&mut choice, ViewChoice::Map, "Map")
            .on_hover_text("Treemap: a box per file, sized by its larger side, colored by state");
        ui.selectable_value(&mut choice, ViewChoice::Duplicates, "Duplicates")
            .on_hover_text("Find duplicate files in the left folder");
    });
    (choice != current).then_some(choice)
}

pub struct TreeBehavior<'a, 'b> {
    pub log_buffer: LogBuffer,

    pub hash_service: &'a mut HashService,
    pub path_diff_view: &'a mut PathDiffView<'b>,

    // Diff Action State
    pub conflict_map: &'a mut HashMap<String, Vec<PathBuf>>,
    pub conflict_map_resolved: &'a mut HashMap<String, PathBuf>,
    pub diff_action_pressed: &'a mut bool,
    pub diff_tool_config: &'a DiffToolConfig,
    pub tool_message: &'a mut Option<ToolMessage>,
    pub path_filter: &'a mut PathFilter,
    pub filter_presets: &'a mut FilterPresets,
    pub tree_sort: &'a mut TreeSort,
    pub diff_filter: &'a mut DiffFilter,
    pub duplicates_filter: &'a mut DuplicatesFilter,
    pub likeness_bar: &'a mut bool,
    pub sides_swapped: bool,
    pub tolerance: &'a mut Tolerance,
    pub tree_compare: &'a TreeCompare,
    pub h_scroll_link: &'a mut HScrollLink,
    pub find: &'a mut TreeFindView<'b>,
    pub keyboard_taken: bool,
    pub actions: &'a [FolderAction],
    pub view_request: &'a mut Option<ViewChoice>,
}

impl<'a, 'b> TreeBehavior<'a, 'b> {
    pub fn create_path_diff_ctx<'c>(&'c mut self) -> PathDiffPaneCtx<'c, 'b> {
        PathDiffPaneCtx {
            diff_tool_config: self.diff_tool_config,
            tool_message: self.tool_message,
            path_filter: self.path_filter,
            filter_presets: self.filter_presets,
            tree_sort: self.tree_sort,
            diff_filter: self.diff_filter,
            likeness_bar: self.likeness_bar,
            sides_swapped: self.sides_swapped,
            tolerance: self.tolerance,
            tree_compare: self.tree_compare,
            h_scroll_link: self.h_scroll_link,
            find: self.find,
            path_diff_view: self.path_diff_view,
            keyboard_taken: self.keyboard_taken,
            actions: self.actions,
            view_request: self.view_request,
        }
    }

    pub fn create_duplicate_files_ctx<'c>(&'c mut self) -> DuplicateFilesPaneCtx<'c, 'b>
    where
        'a: 'c,
    {
        DuplicateFilesPaneCtx {
            hash_service: self.hash_service,
            path_diff_view: self.path_diff_view,
            path_filter: self.path_filter,
            tree_sort: self.tree_sort,
            duplicates_filter: self.duplicates_filter,
            diff_tool_config: self.diff_tool_config,
            tool_message: self.tool_message,
            keyboard_taken: self.keyboard_taken,

            conflict_map: self.conflict_map,
            conflict_map_resolved: self.conflict_map_resolved,
            diff_action_pressed: self.diff_action_pressed,
            actions: self.actions,
            view_request: self.view_request,
        }
    }
}

impl egui_tiles::Behavior<Pane> for TreeBehavior<'_, '_> {
    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        pane.title().into()
    }

    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        _tile_id: egui_tiles::TileId,
        pane: &mut Pane,
    ) -> egui_tiles::UiResponse {
        match pane {
            Pane::Log(pane) => pane.ui(ui, &self.log_buffer),
            Pane::DuplicateFiles(pane) => {
                let mut ctx = self.create_duplicate_files_ctx();
                pane.ui(ui, &mut ctx)
            }
            Pane::PathDiff(path_diff_pane) => {
                let mut ctx = self.create_path_diff_ctx();
                path_diff_pane.ui(ui, &mut ctx)
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
pub enum Pane {
    Log(LogPane),
    DuplicateFiles(Box<DuplicateFilesPane>),
    PathDiff(Box<PathDiffPane>),
}

impl Pane {
    pub fn title(&self) -> String {
        match self {
            Pane::Log(pane) => pane.title(),
            Pane::DuplicateFiles(p) => p.title(),
            Pane::PathDiff(p) => p.title(),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct LogPane {
    pub title: Option<String>,
    #[serde(default)]
    pub scroll_to_bottom: bool,
}
impl ZAppPane for LogPane {
    fn title(&self) -> String {
        self.title.clone().unwrap_or("Pane".to_string())
    }
}

impl LogPane {
    fn ui(&mut self, ui: &mut egui::Ui, log_buffer: &LogBuffer) -> egui_tiles::UiResponse {
        let _scope = zcommon_ui::frame_stats::scope("Log pane");
        zcommon_ui::log_window::log_window(ui, log_buffer.clone(), &mut self.scroll_to_bottom);

        egui_tiles::UiResponse::None
    }
}
