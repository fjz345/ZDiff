use std::{collections::HashMap, path::PathBuf};

use eframe::egui::{self};
use serde::{Deserialize, Serialize};
use zcommon::{
    hash::HashService,
    logger::{LogBuffer, ui_log_window},
};
use zhashdiff::{external_diff_tool::DiffToolConfig, filter::PathFilter};

use crate::ui_egui::{
    app::SideScan,
    duplicate_files_pane::{DuplicateFilesPane, DuplicateFilesPaneCtx},
    fs_tree::{FileSystemView, VisibleRowTwoFolderDiff},
    path_diff_pane::{FilterPresets, PathDiffPane, PathDiffPaneCtx},
    status_line::ToolMessage,
    tree_compare::TreeCompare,
    tree_filter::DiffFilter,
    tree_sort::TreeSort,
};

pub struct PathDiffView<'a> {
    pub file_system_1_root_path: &'a mut Option<PathBuf>,
    pub file_system_2_root_path: &'a mut Option<PathBuf>,
    pub file_system_1_view: &'a mut Option<FileSystemView>,
    pub file_system_2_view: &'a mut Option<FileSystemView>,
    pub file_system_1_scan: &'a mut SideScan,
    pub visible_rows: &'a mut Option<Vec<VisibleRowTwoFolderDiff>>,
}

pub struct TreeBehavior<'a, 'b> {
    pub log_buffer: LogBuffer,

    pub hash_service: &'a mut HashService,
    pub path_diff_view: &'a mut PathDiffView<'b>,

    // Diff Action State
    pub active_conflict_hash: &'a mut Option<String>,
    pub conflict_map: &'a mut HashMap<String, Vec<PathBuf>>,
    pub conflict_map_resolved: &'a mut HashMap<String, PathBuf>,
    pub diff_action_pressed: &'a mut bool,
    pub diff_tool_config: &'a DiffToolConfig,
    pub tool_message: &'a mut Option<ToolMessage>,
    pub path_filter: &'a mut PathFilter,
    pub filter_presets: &'a mut FilterPresets,
    pub tree_sort: &'a mut TreeSort,
    pub diff_filter: &'a mut DiffFilter,
    pub likeness_bar: &'a mut bool,
    pub tree_compare: &'a TreeCompare,
    pub keyboard_taken: bool,
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
            tree_compare: self.tree_compare,
            path_diff_view: self.path_diff_view,
            keyboard_taken: self.keyboard_taken,
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

            active_conflict_hash: self.active_conflict_hash,
            conflict_map: self.conflict_map,
            conflict_map_resolved: self.conflict_map_resolved,
            diff_action_pressed: self.diff_action_pressed,
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
        let response = match pane {
            Pane::Log(pane) => {
                let response = pane.ui(ui, &mut self.log_buffer);
                egui_tiles::UiResponse::from(response)
            }
            Pane::DuplicateFiles(pane) => {
                let mut ctx = self.create_duplicate_files_ctx();
                let response = pane.ui(ui, &mut ctx);
                egui_tiles::UiResponse::from(response)
            }
            Pane::PathDiff(path_diff_pane) => {
                let mut ctx = self.create_path_diff_ctx();
                let response = path_diff_pane.ui(ui, &mut ctx);
                egui_tiles::UiResponse::from(response)
            }
        };

        if ui
            .add(egui::Button::new("Drag me!").sense(egui::Sense::drag()))
            .drag_started()
        {
            egui_tiles::UiResponse::DragStarted
        } else {
            response
        }
    }
}

#[derive(Serialize, Deserialize)]
pub enum Pane {
    Log(LogPane),
    DuplicateFiles(DuplicateFilesPane),
    PathDiff(PathDiffPane),
}

impl Pane {
    pub fn title(&self) -> String {
        match self {
            Pane::Log(pane) => pane.title().into(),
            Pane::DuplicateFiles(p) => p.title(),
            Pane::PathDiff(p) => p.title(),
        }
    }
}

pub trait ZAppPane {
    fn title(&self) -> String {
        "Pane".to_string()
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
        self.title.clone().unwrap_or(format!("Pane"))
    }
}

impl LogPane {
    fn ui(&mut self, ui: &mut egui::Ui, log_buffer: &LogBuffer) -> egui_tiles::UiResponse {
        ui_log_window(ui, log_buffer.clone(), &mut self.scroll_to_bottom);

        return egui_tiles::UiResponse::None;
    }
}
