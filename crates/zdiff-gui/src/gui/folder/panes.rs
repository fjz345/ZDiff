use std::{collections::HashMap, path::PathBuf};

use eframe::egui::{self};
use serde::{Deserialize, Serialize};
use zcommon::hash::HashService;
use zcommon_ui::pane::toolbar_group;
use zdiff_folder::{external_diff_tool::DiffToolConfig, filter::PathFilter};

use crate::folder::{
    duplicate_files_pane::DuplicateFilesPaneCtx,
    folder_keymap::FolderAction,
    fs_tree::{FileSystemView, RowsId, VisibleRowTwoFolderDiff},
    path_diff_pane::{FilterPresets, PaneView, PathDiffPaneCtx},
    status_line::ToolMessage,
    tree_compare::TreeCompare,
    tree_filter::{DiffFilter, DuplicatesFilter},
    tree_find::TreeFindView,
    tree_menu::RootRequests,
    tree_sort::TreeSort,
    tree_tolerance::Tolerance,
    view::SideScan,
};

pub struct PathDiffView<'a> {
    pub file_system_1_view: &'a mut Option<FileSystemView>,
    pub file_system_2_view: &'a mut Option<FileSystemView>,
    pub file_system_1_scan: &'a mut SideScan,
    pub visible_rows: &'a mut Option<Vec<VisibleRowTwoFolderDiff>>,
    pub rows_id: RowsId,
    pub scanning: bool,
    pub scan_error: Option<String>,
    pub root_requests: &'a mut RootRequests,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewChoice {
    #[default]
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
    pub hash_service: &'a mut HashService,
    pub path_diff_view: &'a mut PathDiffView<'b>,

    // Diff Action State
    pub conflict_map: &'a mut HashMap<String, Vec<PathBuf>>,
    pub conflict_map_resolved: &'a mut HashMap<String, PathBuf>,
    pub diff_tool_config: &'a DiffToolConfig,
    pub tool_message: &'a mut Option<ToolMessage>,
    pub path_filter: &'a mut PathFilter,
    pub filter_presets: &'a mut FilterPresets,
    pub tree_sort: &'a mut TreeSort,
    pub diff_filter: &'a mut DiffFilter,
    pub duplicates_filter: &'a mut DuplicatesFilter,
    pub likeness_bar: &'a mut bool,
    pub tolerance: &'a mut Tolerance,
    pub tree_compare: &'a TreeCompare,
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
            tolerance: self.tolerance,
            tree_compare: self.tree_compare,
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
            actions: self.actions,
            view_request: self.view_request,
        }
    }
}
