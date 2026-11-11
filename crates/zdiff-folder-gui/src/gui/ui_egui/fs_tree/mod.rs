use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use eframe::egui::{self, RichText, text::LayoutJob};
use egui_extras::{Column, TableBuilder};
use serde::{Deserialize, Serialize};
use zcommon::{
    find::Matcher,
    kinds::{FileKind, KindTable},
};
use zdiff_folder::{
    comparison::{FileMeta, PairState},
    external_diff_tool::{
        CantDiff, DiffPair, DiffToolConfig, cant_diff, folder_labels, open_diff_tool,
    },
    filter::PathFilter,
    fs::{FileSystemModel, FsNode, FsNodeDepth, FsNodeId, TreeIter},
};

use zcommon::path::{common_parent, split_path};
use zcommon::style::{Rgba8, Role};
use zcommon_ui::h_scroll::text_width;
use zcommon_ui::pathbar::{PathBarAction, path_bar};
use zcommon_ui::style_window::role_color;
use zcommon_ui::table::{Rows, Split, SplitColumn, SplitTable};

use crate::ui_egui::folder_keymap::FolderAction;
use crate::ui_egui::revision_open::{self, RevisionOpen, is_revision};
use crate::ui_egui::roots::{InvalidRoot, Side, set_root};
use crate::ui_egui::side_revision::{revision_dropdown, split_side, with_revision};
use crate::ui_egui::status_line::ToolMessage;
use crate::ui_egui::tools_window::resolve_tool_exe;
use crate::ui_egui::tree_compare::{PairLikeness, TreeCompare};
use crate::ui_egui::tree_cursor::{CursorRequest, CursorRow, TreeCursor};
use crate::ui_egui::tree_filter::DiffFilter;
use crate::ui_egui::tree_find::{FindChipEvent, TreeFindView, drawn_ranges};
use crate::ui_egui::tree_guides::{Guide, TreeGuides, tree_guides};
use crate::ui_egui::tree_likeness::{
    FolderLikeness, LikenessSum, PreviewPair, likeness_cell, ui_likeness_cell,
};
use crate::ui_egui::tree_menu::{Entry, RowAction, apply_row_action, row_menu, show_row_menu};
use crate::ui_egui::tree_sort::{SortKey, TreeSort, sort_two_folder_rows};
use crate::ui_egui::tree_tolerance::{SideMeta, Tolerance, similar_tooltip};

mod cells;
mod look;
mod state;
mod table;
#[cfg(test)]
mod tests;

pub(crate) use cells::apply_cursor_request;
use cells::*;
pub use look::*;
pub use state::*;
pub use table::*;
