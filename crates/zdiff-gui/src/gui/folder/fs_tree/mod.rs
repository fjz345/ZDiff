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
    universal_path::UniversalPath,
};
use zdiff_folder::{
    comparison::{FileMeta, PairState},
    external_diff_tool::{
        CantDiff, DiffPair, DiffToolConfig, cant_diff, folder_labels, open_diff_tool,
    },
    filter::PathFilter,
    fs::{FileSystemModel, FsNode, FsNodeDepth, FsNodeId, TreeIter},
    vcs_source::{ContentSource, depot_node, git_node},
};

use zcommon::style::{Rgba8, Role};
use zcommon_ui::h_scroll::text_width;
use zcommon_ui::pane::{error_state, loading_state};
use zcommon_ui::style_window::role_color;
use zcommon_ui::table::{Rows, Split, SplitColumn, SplitTable};

use crate::folder::folder_keymap::FolderAction;
use crate::folder::revision_open::{self, RevisionOpen, is_revision};
use crate::folder::roots::Side;
use crate::folder::status_line::ToolMessage;
use crate::folder::tools_window::resolve_tool_exe;
use crate::folder::tree_compare::{PairLikeness, TreeCompare};
use crate::folder::tree_cursor::{CursorRequest, CursorRow, TreeCursor};
use crate::folder::tree_filter::DiffFilter;
use crate::folder::tree_find::{FindChipEvent, TreeFindView, drawn_ranges};
use crate::folder::tree_guides::{Guide, TreeGuides, tree_guides};
use crate::folder::tree_likeness::{
    FolderLikeness, LikenessSum, PreviewPair, likeness_cell, ui_likeness_cell,
};
use crate::folder::tree_menu::{
    Entry, RootRequests, RowAction, apply_row_action, row_menu, show_row_menu,
};
use crate::folder::tree_sort::{SortKey, TreeSort, sort_two_folder_rows};
use crate::folder::tree_tolerance::{SideMeta, Tolerance, similar_tooltip};

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
