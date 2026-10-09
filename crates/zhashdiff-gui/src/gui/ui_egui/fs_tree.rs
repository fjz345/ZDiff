use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc,
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
use zhashdiff::{
    comparison::{FileMeta, PairState},
    external_diff_tool::{
        CantDiff, DiffPair, DiffToolConfig, cant_diff, folder_labels, open_diff_tool,
    },
    filter::PathFilter,
    fs::{FileSystemModel, FsNode, FsNodeDepth, FsNodeId, TreeIter},
};

use zcommon::path::{common_parent, split_path};
use zcommon::ui_egui::h_scroll::text_width;
use zcommon::ui_egui::pathbar::{PathBarAction, path_bar};
use zcommon::ui_egui::table::{Rows, Split, SplitColumn, SplitTable};

use crate::ui_egui::folder_keymap::FolderAction;
use crate::ui_egui::roots::{InvalidRoot, Side, set_root};
use crate::ui_egui::status_line::ToolMessage;
use crate::ui_egui::tools_window::resolve_tool_exe;
use crate::ui_egui::tree_compare::{PairLikeness, TreeCompare};
use crate::ui_egui::tree_cursor::{CursorRequest, CursorRow, TreeCursor};
use crate::ui_egui::tree_filter::DiffFilter;
use crate::ui_egui::tree_find::{FindChipEvent, TreeFindView, drawn_ranges};
use crate::ui_egui::tree_guides::{Guide, tree_guides};
use crate::ui_egui::tree_likeness::{likeness_cell, ui_likeness_cell};
use crate::ui_egui::tree_menu::{Entry, RowAction, apply_row_action, row_menu, show_row_menu};
use crate::ui_egui::tree_sort::{SortKey, TreeSort, sort_two_folder_rows};
use crate::ui_egui::tree_tolerance::{SideMeta, Tolerance, similar_tooltip};

#[derive(Debug, Serialize, Deserialize)]
pub struct FileSystemView {
    pub file_system: Arc<FileSystemModel>,
    pub collapsed: HashMap<FsNodeId, bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VisibleRowTwoFolderDiff {
    /// Relative to the roots, with `/` separators. Empty for the root row.
    pub rel_path: String,
    pub is_dir: bool,
    pub depth: FsNodeDepth,
    pub diff_state: DiffState,
    pub file_kinds: [Option<FileKind>; 2],
    pub blueprints: [bool; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HScrollLink(pub bool);

impl Default for HScrollLink {
    fn default() -> Self {
        Self(true)
    }
}

#[derive(Default)]
pub struct TreeTable {
    split: Split,
    offsets: [f32; 2],
}

impl FileSystemView {
    pub fn new(model: Arc<FileSystemModel>) -> Self {
        Self {
            file_system: model,
            collapsed: HashMap::new(),
        }
    }

    pub fn iter_nodes(&self, start_id: FsNodeId) -> TreeIter<'_> {
        self.file_system.iter_subtree(start_id)
    }

    pub fn is_anything_collapsed(&self, node_id: FsNodeId) -> bool {
        if let Some(node) = self.file_system.get_node(node_id) {
            if !node.is_dir() {
                return false;
            }
            let is_collapsed = self.collapsed.get(&node_id).copied().unwrap_or(false);
            if is_collapsed {
                return true;
            }
            if let Some(children) = node.children() {
                return self.is_anything_collapsed_slice(children);
            }
        }

        false
    }

    pub fn is_anything_collapsed_slice(&self, nodes: &[FsNodeId]) -> bool {
        nodes.iter().any(|&id| self.is_anything_collapsed(id))
    }

    pub fn is_collapsed(&self, node_id: FsNodeId) -> bool {
        self.collapsed.get(&node_id).copied().unwrap_or(false)
    }

    pub fn is_parent_chain_collapsed(&self, node_id: FsNodeId) -> bool {
        if let Some(parent_id) = self.file_system.get_parent_id(node_id) {
            let is_parent_collapsed = self.collapsed.get(&parent_id).copied().unwrap_or(false);
            if is_parent_collapsed {
                return true;
            }
            return self.is_parent_chain_collapsed(parent_id);
        }

        false
    }

    pub fn recursive_collapse(&mut self, node_id: FsNodeId, collapse: bool) {
        let ids: Vec<FsNodeId> = self
            .iter_nodes(node_id)
            .filter(|(_, node, _)| node.is_dir())
            .map(|(id, _, _)| id)
            .collect();

        for id in ids {
            if collapse {
                self.collapsed.insert(id, true);
            } else {
                self.collapsed.remove(&id);
            }
        }
    }

    pub fn toggle_collapse(&mut self, id: FsNodeId) {
        let currently_collapsed = self.collapsed.get(&id).copied().unwrap_or(false);

        if currently_collapsed {
            self.collapsed.remove(&id);
        } else {
            self.collapsed.insert(id, true);
        }
    }

    pub fn recursive_collapse_slice(&mut self, node_ids: &[FsNodeId], collapse: bool) {
        for node_id in node_ids {
            self.recursive_collapse(*node_id, collapse);
        }
    }

    /// `pair_state` gives a file pair's exact state and likeness ratio, each when known.
    pub fn build_two_folder_diff_rows(
        file_system_1: Option<&FileSystemView>,
        file_system_2: Option<&FileSystemView>,
        filter: &PathFilter,
        kinds: &KindTable,
        tolerance: &Tolerance,
        mut pair_state: impl FnMut(&str, [&FsNode; 2]) -> (Option<PairState>, Option<f64>),
        mut blueprint: impl FnMut(&Path, FileMeta) -> Option<bool>,
    ) -> io::Result<Vec<VisibleRowTwoFolderDiff>> {
        let mut entries_map: BTreeMap<
            String,
            (
                Option<(FsNodeId, &FsNode, FsNodeDepth)>,
                Option<(FsNodeId, &FsNode, FsNodeDepth)>,
            ),
        > = BTreeMap::new();

        let get_rel_path = |view: &FileSystemView, id: FsNodeId, node: &FsNode| -> String {
            let root_id = view.file_system.get_root_node_id();
            if id == root_id {
                return String::new();
            }

            let root_node = view.file_system.get_node(root_id).unwrap();
            let root_path = root_node.as_path();

            // Use components to rebuild the path cleanly, avoiding slash/prefix issues
            node.as_path()
                .as_ref()
                .strip_prefix(&root_path)
                .unwrap_or(node.as_path().as_ref())
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        };

        // Filtered entries never enter the map, so folder states only see visible entries.
        if let Some(view) = file_system_1 {
            let hidden = filtered_out(view, filter);
            for (id, node, depth) in view.file_system.iter_tree() {
                if hidden[id] {
                    continue;
                }
                let rel = get_rel_path(view, id, node);
                entries_map.insert(rel, (Some((id, node, depth)), None));
            }
        }
        if let Some(view) = file_system_2 {
            let hidden = filtered_out(view, filter);
            for (id, node, depth) in view.file_system.iter_tree() {
                if hidden[id] {
                    continue;
                }
                let rel = get_rel_path(view, id, node);
                entries_map
                    .entry(rel)
                    .and_modify(|e| e.1 = Some((id, node, depth)))
                    .or_insert((None, Some((id, node, depth))));
            }
        }

        // With a whitelist, a folder is shown on every side it exists on when a visible file is
        // below it on either side, so a folder that exists on both sides never reads as one-sided.
        if !filter.whitelist.is_empty() {
            let mut kept_folders: HashSet<String> = HashSet::new();
            for (rel_path, (left, right)) in &entries_map {
                let has_file = [left, right]
                    .iter()
                    .any(|side| side.is_some_and(|(_, node, _)| !node.is_dir()));
                if !has_file {
                    continue;
                }
                let mut path = rel_path.as_str();
                // Stops at the first ancestor already kept: its own ancestors are kept too.
                while let Some((parent, _)) = path.rsplit_once('/') {
                    if !kept_folders.insert(parent.to_owned()) {
                        break;
                    }
                    path = parent;
                }
            }
            entries_map.retain(|rel_path, (left, right)| {
                if rel_path.is_empty() || kept_folders.contains(rel_path) {
                    return true;
                }
                // A path can be a folder on one side and a file on the other.
                for side in [&mut *left, &mut *right] {
                    if side.is_some_and(|(_, node, _)| node.is_dir()) {
                        *side = None;
                    }
                }
                left.is_some() || right.is_some()
            });
        }

        let num_files_and_folders_1 = file_system_1
            .and_then(|f| Some(f.file_system.total_files_and_folders()))
            .unwrap_or(0) as usize;
        let num_files_and_folders_2 = file_system_2
            .and_then(|f| Some(f.file_system.total_files_and_folders()))
            .unwrap_or(0) as usize;
        let initial_capacity = num_files_and_folders_1.max(num_files_and_folders_2);
        let mut out_rows = Vec::with_capacity(initial_capacity);

        let file_states: HashMap<&str, DiffState> = entries_map
            .iter()
            .filter_map(|(rel_path, (left, right))| {
                let ((l_id, l_node, _), (r_id, r_node, _)) = (left.as_ref()?, right.as_ref()?);
                if l_node.is_dir() {
                    return None;
                }
                let state = file_diff_state(
                    rel_path,
                    (*l_id, l_node),
                    (*r_id, r_node),
                    tolerance,
                    &mut pair_state,
                );
                Some((rel_path.as_str(), state))
            })
            .collect();

        for (rel_path, (left, right)) in &entries_map {
            let depth = left.map(|l| l.2).or(right.map(|r| r.2)).unwrap_or(0);

            let is_dir = left
                .map(|l| l.1.is_dir())
                .or(right.map(|r| r.1.is_dir()))
                .unwrap_or(false);

            let diff_state = match (left, right) {
                (Some((_, l_node, _)), Some(_)) => {
                    if l_node.is_dir() {
                        folder_diff_state(rel_path, &entries_map, &file_states)
                    } else {
                        file_states[rel_path.as_str()].clone()
                    }
                }
                (Some((l_id, _, _)), None) => DiffState::OnlyInFirst(*l_id),
                (None, Some((r_id, _, _))) => DiffState::OnlyInSecond(*r_id),
                (None, None) => panic!("unreachable"),
            };

            let extension = Path::new(rel_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            let file_kind = |side: &Option<(FsNodeId, &FsNode, FsNodeDepth)>| {
                side.filter(|(_, node, _)| !node.is_dir())
                    .map(|_| kinds.kind(extension))
            };

            let file_kinds = [file_kind(left), file_kind(right)];
            let mut blueprints = [false, false];
            for (i, side) in [left, right].into_iter().enumerate() {
                if file_kinds[i] == Some(FileKind::UnrealAsset)
                    && let Some((_, node, _)) = side
                    && let Some(modified) = node.modified
                {
                    let meta = FileMeta {
                        size: node.size,
                        modified,
                    };
                    blueprints[i] = blueprint(node.as_path().as_ref(), meta).unwrap_or(false);
                }
            }

            out_rows.push(VisibleRowTwoFolderDiff {
                rel_path: rel_path.clone(),
                is_dir,
                depth,
                diff_state,
                file_kinds,
                blueprints,
            });
        }

        Ok(out_rows)
    }
}

/// Per node id of the view's model, true when the filter hides the node: not kept by the filter,
/// or inside a hidden folder. The root is never hidden. Folders the whitelist leaves empty are
/// pruned later, across both sides.
fn filtered_out(view: &FileSystemView, filter: &PathFilter) -> Vec<bool> {
    let model = &view.file_system;
    let mut hidden = vec![false; model.total_files_and_folders()];
    if !filter.is_active() {
        return hidden;
    }
    // Pre-order, so a parent is decided before its children.
    for (id, node, _) in model.iter_tree() {
        let Some(parent) = node.parent else {
            continue;
        };
        hidden[id] = hidden[parent] || {
            let path = node.as_path();
            let name = path.as_ref().file_name().unwrap_or_default();
            !filter.keeps(&name.to_string_lossy(), node.is_dir())
        };
    }
    hidden
}

/// Per node id of the view's model, true when the filter hides the node in a single-tree view
/// (the duplicate finder): [`filtered_out`], and with a whitelist set, every non-root folder with
/// no visible file below it.
pub fn tree_hidden(view: &FileSystemView, filter: &PathFilter) -> Vec<bool> {
    let mut hidden = filtered_out(view, filter);
    if filter.whitelist.is_empty() {
        return hidden;
    }
    let model = &view.file_system;
    let mut has_visible_file = vec![false; hidden.len()];
    for id in model.iter_files() {
        if hidden[id] {
            continue;
        }
        let mut ancestor = model.get_parent_id(id);
        // Stops at the first ancestor already marked: its own ancestors are marked too.
        while let Some(folder) = ancestor {
            if std::mem::replace(&mut has_visible_file[folder], true) {
                break;
            }
            ancestor = model.get_parent_id(folder);
        }
    }
    let root_id = model.get_root_node_id();
    for (id, node, _) in model.iter_tree() {
        if node.is_dir() && id != root_id && !has_visible_file[id] {
            hidden[id] = true;
        }
    }
    hidden
}

/// Folder 1's rows for the single-side tree: every row is `OnlyInFirst`.
pub fn single_side_rows(
    view: &FileSystemView,
    filter: &PathFilter,
    sort: TreeSort,
) -> io::Result<Vec<VisibleRowTwoFolderDiff>> {
    let rows = FileSystemView::build_two_folder_diff_rows(
        Some(view),
        None,
        filter,
        &KindTable::default(),
        &Tolerance::default(),
        |_, _| (None, None),
        |_, _| None,
    )?;
    Ok(sort_two_folder_rows(rows, sort))
}

/// The two-folder rows as the cursor sees them. A row is hidden when it isn't drawn: the
/// root row, or an entry inside a folder collapsed on a side the entry exists on. A folder is
/// collapsed when it is collapsed on a side it exists on. In flat mode only the root row is hidden.
pub fn two_folder_cursor_rows(
    file_system_1_view: Option<&FileSystemView>,
    file_system_2_view: Option<&FileSystemView>,
    rows: &[VisibleRowTwoFolderDiff],
    flat: bool,
) -> Vec<CursorRow> {
    let views = [file_system_1_view, file_system_2_view];
    rows.iter()
        .map(|row| {
            let (hidden, collapsed) = fold_state(views, row, flat);
            CursorRow {
                rel_path: row.rel_path.clone(),
                hidden,
                is_dir: row.is_dir,
                collapsed,
                differs: !matches!(
                    row.diff_state,
                    DiffState::Same(..) | DiffState::Similar(..) | DiffState::Pending(..)
                ),
            }
        })
        .collect()
}

fn fold_state(
    views: [Option<&FileSystemView>; 2],
    row: &VisibleRowTwoFolderDiff,
    flat: bool,
) -> (bool, bool) {
    let on_either_side = |test: fn(&FileSystemView, FsNodeId) -> bool| {
        let test_side = |view: Option<&FileSystemView>, id: Option<FsNodeId>| match (view, id) {
            (Some(view), Some(id)) => test(view, id),
            _ => false,
        };
        test_side(views[0], row.diff_state.first()) || test_side(views[1], row.diff_state.second())
    };
    // Flat mode has no hierarchy to fold, so the views' collapse state doesn't apply.
    let hidden = row.rel_path.is_empty()
        || (!flat && on_either_side(FileSystemView::is_parent_chain_collapsed));
    let collapsed = !flat && row.is_dir && on_either_side(FileSystemView::is_collapsed);
    (hidden, collapsed)
}

/// One build of a rows `Vec`. A rebuilt `Vec` can reuse the old one's address and length.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowsId(u64);

impl RowsId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(PartialEq)]
struct CursorRowsKey {
    rows: RowsId,
    flat: bool,
    collapsed: [Option<HashMap<FsNodeId, bool>>; 2],
}

/// `two_folder_cursor_rows` and the indices of the drawn rows, kept until the rows, a view's
/// collapse state or flat mode change.
#[derive(Default)]
pub struct CursorRowsCache {
    key: Option<CursorRowsKey>,
    cursor_rows: Vec<CursorRow>,
    drawn: Vec<usize>,
    #[cfg(test)]
    builds: usize,
}

impl CursorRowsCache {
    pub fn update(
        &mut self,
        views: [Option<&FileSystemView>; 2],
        rows: &[VisibleRowTwoFolderDiff],
        rows_id: RowsId,
        flat: bool,
    ) -> &[CursorRow] {
        let collapsed = views.map(|view| view.map(|view| &view.collapsed));
        let current = self.key.as_ref().is_some_and(|key| {
            key.rows == rows_id
                && key.flat == flat
                && key.collapsed.iter().map(Option::as_ref).eq(collapsed)
        });
        if !current {
            if self.key.as_ref().is_some_and(|key| key.rows == rows_id) {
                assert_eq!(
                    self.cursor_rows.len(),
                    rows.len(),
                    "rows changed under one RowsId"
                );
                // The paths stay: a fold or flat mode changes only these two flags.
                for (cursor_row, row) in self.cursor_rows.iter_mut().zip(rows) {
                    (cursor_row.hidden, cursor_row.collapsed) = fold_state(views, row, flat);
                }
            } else {
                self.cursor_rows = two_folder_cursor_rows(views[0], views[1], rows, flat);
            }
            self.drawn.clear();
            self.drawn
                .extend((0..self.cursor_rows.len()).filter(|&i| !self.cursor_rows[i].hidden));
            self.key = Some(CursorRowsKey {
                rows: rows_id,
                flat,
                collapsed: collapsed.map(|collapsed| collapsed.cloned()),
            });
            #[cfg(test)]
            {
                self.builds += 1;
            }
        }
        &self.cursor_rows
    }

    pub fn cursor_rows(&self) -> &[CursorRow] {
        &self.cursor_rows
    }

    pub fn drawn(&self) -> &[usize] {
        &self.drawn
    }
}

pub fn apply_blueprint_results(
    rows: &mut [VisibleRowTwoFolderDiff],
    roots: [Option<&Path>; 2],
    results: &[(PathBuf, bool)],
) {
    let mut by_rel_path: HashMap<String, Vec<(usize, bool)>> = HashMap::new();
    for (path, blueprint) in results {
        for (side, root) in roots.iter().enumerate() {
            let Some(rel) = root.and_then(|root| path.strip_prefix(root).ok()) else {
                continue;
            };
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            by_rel_path.entry(rel).or_default().push((side, *blueprint));
        }
    }
    for row in rows {
        for &(side, blueprint) in by_rel_path.get(&row.rel_path).into_iter().flatten() {
            if row.file_kinds[side] == Some(FileKind::UnrealAsset) {
                row.blueprints[side] = blueprint;
            }
        }
    }
}

pub const PENDING_DELETION_COLOR: egui::Color32 = egui::Color32::LIGHT_RED;
const FOLDER_ROW_TINT: egui::Color32 =
    egui::Color32::from_rgba_unmultiplied_const(120, 150, 255, 14);
const DIMMED_TEXT_COLOR: egui::Color32 = egui::Color32::from_gray(105);
const FIND_MATCH_BG: egui::Color32 = egui::Color32::from_rgba_premultiplied(90, 66, 0, 140);
const FIND_CHIP_MAX_CHARS: usize = 24;
const GUIDE_LINE_COLOR: egui::Color32 = egui::Color32::from_gray(95);
const BLUEPRINT_TAG_COLOR: egui::Color32 = egui::Color32::from_rgb(70, 120, 255);
const TREE_INDENT: f32 = 16.0;
const EXPANDER_WIDTH: f32 = 12.0;

fn tag_color(kind: FileKind) -> egui::Color32 {
    match kind {
        FileKind::Text => egui::Color32::from_rgb(170, 190, 215),
        FileKind::Image => egui::Color32::from_rgb(215, 130, 220),
        FileKind::Video => egui::Color32::from_rgb(235, 150, 80),
        FileKind::Audio => egui::Color32::from_rgb(90, 200, 190),
        FileKind::Model => egui::Color32::from_rgb(140, 210, 110),
        FileKind::Archive => egui::Color32::from_rgb(210, 180, 110),
        FileKind::Executable => egui::Color32::from_rgb(230, 105, 105),
        FileKind::UnrealAsset => egui::Color32::from_rgb(100, 170, 255),
        FileKind::UnrealMap => egui::Color32::from_rgb(150, 140, 255),
        FileKind::Binary => egui::Color32::from_gray(130),
    }
}

pub struct TwoFolderTable<'a, 'b> {
    pub compare: &'a TreeCompare,
    pub likeness_bar: bool,
    pub tolerance: &'a Tolerance,
    pub table: &'a mut TreeTable,
    pub linked: bool,
    pub find: &'a mut TreeFindView<'b>,
}

pub enum TreeMode<'a, 'b> {
    TwoFolder(TwoFolderTable<'a, 'b>),
    /// Folder 1 alone, without the center column and Folder 2: the duplicates tab.
    SingleSide {
        pending_deletion: &'a HashSet<PathBuf>,
        column: NodeColumn<'a>,
        /// Items appended to a row's right-click menu.
        row_menu: &'a mut RowMenuItems<'a>,
        /// The caller moved the cursor.
        scroll_to_cursor: bool,
    },
}

pub type RowMenuItems<'a> = dyn FnMut(&mut egui::Ui, FsNodeId) + 'a;

pub struct NodeColumn<'a> {
    pub header: &'a str,
    pub width: f32,
    pub cell: &'a mut dyn FnMut(&mut egui::Ui, FsNodeId),
}

pub fn draw_ui_folder_tree(
    ui: &mut egui::Ui,
    file_system_1_view: &mut Option<FileSystemView>,
    file_system_2_view: &mut Option<FileSystemView>,
    visible_rows: Option<&[VisibleRowTwoFolderDiff]>,
    rows_id: RowsId,
    root_1: &mut Option<PathBuf>,
    root_2: &mut Option<PathBuf>,
    open_dir_window_1: &mut bool,
    open_dir_window_2: &mut bool,
    diff_tool_config: &DiffToolConfig,
    tool_message: &mut Option<ToolMessage>,
    cursor: &mut TreeCursor,
    rows_cache: &mut CursorRowsCache,
    sort: &mut TreeSort,
    keyboard_taken: bool,
    actions: &[FolderAction],
    diff_filter: DiffFilter,
    mode: TreeMode,
) -> egui::response::Response {
    if file_system_1_view.is_none() && file_system_2_view.is_none() {
        return ui
            .vertical_centered(|ui| {
                ui.add_space(100.0);
                ui.heading("Welcome to ZHashDiff");
                ui.label("Drag and drop folders here or use the buttons below to start comparing.");
                ui.horizontal(|ui| {
                    if ui.button("Open Folder 1").clicked() {
                        *open_dir_window_1 = true;
                    }
                    if ui.button("Open Folder 2").clicked() {
                        *open_dir_window_2 = true;
                    }
                });
                handle_drops(
                    ui,
                    root_1,
                    root_2,
                    egui::Rect::EVERYTHING,
                    egui::Rect::EVERYTHING,
                )
            })
            .response;
    }

    let visible_rows = visible_rows.expect("the app builds the rows of a shown view");
    let flat = diff_filter.is_flat(*sort);
    let tool_message_before = tool_message.clone();

    // Every frame rather than at each rebuild or collapse: filter, rescan, the collapse
    // buttons and the expander arrows all change the rows, and this sees all of them.
    *cursor = cursor.resolve(rows_cache.update(
        [file_system_1_view.as_ref(), file_system_2_view.as_ref()],
        visible_rows,
        rows_id,
        flat,
    ));

    // Scroll only when the keys moved the cursor, so manual scrolling isn't fought.
    let mut scroll_to_cursor = false;
    if !keyboard_taken {
        for key in ui.input(cursor_keys) {
            // Each key sees the rows the previous key's fold left.
            let cursor_rows = rows_cache.update(
                [file_system_1_view.as_ref(), file_system_2_view.as_ref()],
                visible_rows,
                rows_id,
                flat,
            );
            let (moved, request) = match key {
                egui::Key::ArrowDown => (cursor.down(&cursor_rows), None),
                egui::Key::ArrowUp => (cursor.up(&cursor_rows), None),
                egui::Key::ArrowLeft => cursor.left(&cursor_rows),
                egui::Key::ArrowRight => (cursor.clone(), cursor.right(&cursor_rows)),
                egui::Key::Enter => (cursor.clone(), cursor.enter(&cursor_rows)),
                _ => unreachable!("cursor_keys returned {key:?}"),
            };
            scroll_to_cursor |= moved != *cursor;
            *cursor = moved;
            // A fold in flat mode wouldn't show, only change the tree view behind it.
            let request = request.filter(|r| !flat || matches!(r, CursorRequest::Open(_)));
            if let Some(request) = request {
                apply_cursor_request(
                    file_system_1_view.as_mut(),
                    file_system_2_view.as_mut(),
                    visible_rows,
                    &request,
                    diff_tool_config,
                    tool_message,
                );
            }
        }
    }
    for action in actions {
        let cursor_rows = rows_cache.update(
            [file_system_1_view.as_ref(), file_system_2_view.as_ref()],
            visible_rows,
            rows_id,
            flat,
        );
        let target = match action {
            FolderAction::NextDiff => cursor.next_diff(&cursor_rows),
            FolderAction::PrevDiff => cursor.prev_diff(&cursor_rows),
            FolderAction::Refresh
            | FolderAction::OpenKeybindings
            | FolderAction::Find
            | FolderAction::KeepThis
            | FolderAction::NextGroup
            | FolderAction::PrevGroup => continue,
        };
        let Some((moved, expand)) = target else {
            log::info!("No more diffs");
            continue;
        };
        for request in &expand {
            apply_cursor_request(
                file_system_1_view.as_mut(),
                file_system_2_view.as_mut(),
                visible_rows,
                request,
                diff_tool_config,
                tool_message,
            );
        }
        scroll_to_cursor |= moved != *cursor;
        *cursor = moved;
    }

    // After the keys, so a fold they made is drawn this frame.
    rows_cache.update(
        [file_system_1_view.as_ref(), file_system_2_view.as_ref()],
        visible_rows,
        rows_id,
        flat,
    );
    let cursor_rows = rows_cache.cursor_rows();

    // Only drawn rows go to the table: it places row i at i * row height, both when
    // virtualizing and when scrolling to a row.
    let drawn_rows = rows_cache.drawn();
    let selected_row = drawn_rows
        .iter()
        .position(|&i| cursor.is_at(&cursor_rows[i].rel_path));
    scroll_to_cursor |= matches!(
        mode,
        TreeMode::SingleSide {
            scroll_to_cursor: true,
            ..
        }
    );
    let scroll_to_row = selected_row.filter(|_| scroll_to_cursor);

    let row_count = drawn_rows.len();
    let layout = if diff_filter.lists_files() {
        RowLayout::FileList
    } else if sort.flat {
        RowLayout::RelPaths
    } else {
        RowLayout::Tree
    };
    let guides = (layout == RowLayout::Tree).then(|| {
        let side = |id: fn(&DiffState) -> Option<FsNodeId>| {
            let rows: Vec<(FsNodeDepth, bool)> = drawn_rows
                .iter()
                .map(|&i| {
                    (
                        visible_rows[i].depth,
                        id(&visible_rows[i].diff_state).is_some(),
                    )
                })
                .collect();
            tree_guides(&rows)
        };
        [side(DiffState::first), side(DiffState::second)]
    });
    let root_state = match (file_system_1_view.as_ref(), file_system_2_view.as_ref()) {
        (Some(view), _) => {
            let root_id = view.file_system.get_root_node_id();
            visible_rows
                .iter()
                .find(|r| r.diff_state.first() == Some(root_id))
        }
        (None, Some(view)) => {
            let root_id = view.file_system.get_root_node_id();
            visible_rows
                .iter()
                .find(|r| r.diff_state.second() == Some(root_id))
        }
        (None, None) => None,
    }
    .map(|row| &row.diff_state);
    let available_height = ui.available_height();
    let available_width = ui.available_width();
    let table_top = ui.cursor().top();
    let mut side_rects = [egui::Rect::NOTHING; 2];
    let mut clicked_row = None;
    let mut toggle_row = None;
    let mut row_action = None;
    let mut chip_event = None;

    let (mut two_folder, pending_deletion, mut node_column, mut extra_menu) = match mode {
        TreeMode::TwoFolder(two_folder) => (Some(two_folder), None, None, None),
        TreeMode::SingleSide {
            pending_deletion,
            column,
            row_menu,
            ..
        } => (None, Some(pending_deletion), Some(column), Some(row_menu)),
    };
    let (find_matcher, match_path, find_chip_query) =
        two_folder
            .as_ref()
            .map_or((None, false, None), |two_folder| {
                let find = &two_folder.find;
                (find.matcher, find.match_path, find.chip)
            });

    let frame_output = egui::Frame::default()
        .fill(egui::Color32::from_gray(20))
        .inner_margin(0.0)
        .show(ui, |ui| {
            ui.set_min_height(available_height);
            ui.set_min_width(available_width);

            let row_height = ui.text_style_height(&egui::TextStyle::Body);
            let row_height_header = ui.text_style_height(&egui::TextStyle::Heading);
            let interact_height = ui.spacing().interact_size.y;
            let header_height = row_height_header.max(interact_height)
                + ui.spacing().item_spacing.y
                + interact_height;
            let views = [file_system_1_view.as_ref(), file_system_2_view.as_ref()];
            let row_cell = |i: usize| {
                let entry = &visible_rows[drawn_rows[i]];
                // Selected cells get their text color from egui_extras; an explicit color
                // would override it.
                let dimmed =
                    selected_row != Some(i) && matches!(entry.diff_state, DiffState::Same(..));
                let pending = pending_deletion.is_some_and(|pending| {
                    side_node(views[0], entry.diff_state.first())
                        .is_some_and(|node| pending.contains(node.as_path().as_ref()))
                });
                RowCell {
                    views,
                    entry,
                    dimmed,
                    pending,
                    row_height,
                    diff_tool_config,
                    find_matcher,
                    match_path,
                }
            };
            let mut side_cell = |i: usize, side: usize, ui: &mut egui::Ui| {
                let entry = &visible_rows[drawn_rows[i]];
                let row = row_cell(i);
                // A scrolled side's max rect is shifted; its clip is the cell.
                if entry.is_dir {
                    fill_cell(ui, ui.clip_rect().x_range(), FOLDER_ROW_TINT);
                }
                if let Some(guides) = &guides {
                    paint_guides(ui, guides[side].row(i));
                }
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                let cell = render_side_cell(
                    ui,
                    &row,
                    side,
                    tool_message,
                    layout,
                    extra_menu.as_deref_mut(),
                );
                if cell.clicked {
                    clicked_row = Some(entry);
                }
                if cell.toggle {
                    toggle_row = Some(entry);
                }
                if cell.action.is_some() {
                    row_action = cell.action;
                }
            };

            let Some(two_folder) = two_folder.as_mut() else {
                let column = node_column
                    .as_mut()
                    .expect("single-side mode has a node column");
                let mut table = TableBuilder::new(ui)
                    .id_salt("single_side_table")
                    .striped(true)
                    .resizable(false)
                    .auto_shrink([false, false])
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::remainder().clip(true))
                    .column(Column::exact(column.width).clip(true));
                if let Some(row) = scroll_to_row {
                    table = table.scroll_to_row(row, None);
                }
                let body = table
                    .header(header_height, |mut header| {
                        header.col(|ui| {
                            folder_1_header(
                                ui,
                                sort,
                                open_dir_window_1,
                                [&mut *root_1, &mut *root_2],
                                row_height_header,
                                None,
                            );
                        });
                        header.col(|ui| {
                            ui.label(column.header);
                        });
                    })
                    .body(|body| {
                        body.rows(row_height, row_count, |mut row| {
                            let i = row.index();
                            row.set_selected(selected_row == Some(i));
                            row.col(|ui| side_cell(i, 0, ui));
                            row.col(|ui| {
                                if let Some(node_id) =
                                    visible_rows[drawn_rows[i]].diff_state.first()
                                {
                                    (column.cell)(ui, node_id);
                                }
                            });
                        });
                    });
                side_rects[0] = body.inner_rect;
                return;
            };
            let center = CenterLayout::measure(ui, two_folder.likeness_bar);
            let content_widths = side_content_widths(
                ui,
                views,
                drawn_rows.iter().map(|&i| &visible_rows[i]),
                layout,
            );

            let mut split_table =
                SplitTable::new("two_folder_diff_table", &mut two_folder.table.split)
                    .center_width(center.width(ui.spacing().item_spacing.x))
                    .h_scroll(
                        &mut two_folder.table.offsets,
                        two_folder.linked,
                        content_widths,
                    )
                    .striped(true)
                    .selected_row(selected_row);
            if let Some(row) = scroll_to_row {
                split_table = split_table.scroll_to_row(row, None);
            }
            let (compare, tolerance) = (two_folder.compare, two_folder.tolerance);
            let output = split_table.show(
                ui,
                header_height,
                |column, ui| match column {
                    SplitColumn::Left => {
                        chip_event = folder_1_header(
                            ui,
                            sort,
                            open_dir_window_1,
                            [&mut *root_1, &mut *root_2],
                            row_height_header,
                            find_chip_query,
                        );
                    }
                    SplitColumn::Center => center.show(ui, |slot, ui| match slot {
                        CenterSlot::State => {
                            ui.vertical(|ui| {
                                sort_header(ui, "≠", SortKey::State, sort);
                                if let Some(state) = root_state {
                                    ui_custom_diff_state(ui, state);
                                }
                            });
                        }
                        CenterSlot::Likeness => {
                            ui.label("%").on_hover_text("Likeness of the two files");
                        }
                        CenterSlot::Tag(_) => {}
                    }),
                    SplitColumn::Right => {
                        let available = ui.available_width();
                        ui.vertical(|ui| {
                            if ui
                                .add_sized(
                                    [available, row_height_header],
                                    egui::Button::new("Open Folder 2"),
                                )
                                .clicked()
                            {
                                *open_dir_window_2 = true;
                            }
                            root_path_bar(ui, [&mut *root_1, &mut *root_2], Side::Folder2);
                        });
                    }
                },
                Rows::Uniform {
                    height: row_height,
                    count: row_count,
                },
                |i, column, ui| match column {
                    SplitColumn::Left => side_cell(i, 0, ui),
                    SplitColumn::Right => side_cell(i, 1, ui),
                    SplitColumn::Center => {
                        let row = row_cell(i);
                        if row.entry.is_dir {
                            fill_cell(ui, ui.max_rect().x_range(), FOLDER_ROW_TINT);
                        }
                        render_center_cell(ui, &row, &center, compare, tolerance);
                    }
                },
            );
            side_rects = output.side_rects;
        });
    if let Some(two_folder) = two_folder {
        two_folder.find.chip_event = chip_event;
    }

    if let Some(entry) = toggle_row {
        if let (Some(view), Some(id)) = (file_system_1_view.as_mut(), entry.diff_state.first()) {
            view.toggle_collapse(id);
        }
        if let (Some(view), Some(id)) = (file_system_2_view.as_mut(), entry.diff_state.second()) {
            view.toggle_collapse(id);
        }
    }
    if let Some(entry) = clicked_row {
        *cursor = TreeCursor::at(&entry.rel_path);
    }

    let table_y = table_top..=ui.min_rect().bottom();
    let [left_rect, right_rect] =
        side_rects.map(|rect| egui::Rect::from_x_y_ranges(rect.x_range(), table_y.clone()));
    handle_drops(ui, root_1, root_2, left_rect, right_rect);
    if let Some(action) = row_action {
        apply_row_action(ui.ctx(), action, root_1, root_2);
    }
    // The status line was drawn before this pane; show the new message next frame.
    if *tool_message != tool_message_before {
        ui.ctx().request_repaint();
    }

    frame_output.response
}

fn folder_1_header(
    ui: &mut egui::Ui,
    sort: &mut TreeSort,
    open_dir_window: &mut bool,
    roots: [&mut Option<PathBuf>; 2],
    button_height: f32,
    find_chip_query: Option<&str>,
) -> Option<FindChipEvent> {
    let mut chip_event = None;
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            sort_header(ui, "Name", SortKey::Name, sort);
            if let Some(query) = find_chip_query {
                chip_event = find_chip(ui, query);
            }
            let available = ui.available_width();
            if ui
                .add_sized(
                    [available, button_height],
                    egui::Button::new("Open Folder 1"),
                )
                .clicked()
            {
                *open_dir_window = true;
            }
        });
        root_path_bar(ui, roots, Side::Folder1);
    });
    chip_event
}

fn find_chip(ui: &mut egui::Ui, query: &str) -> Option<FindChipEvent> {
    let shown = if query.chars().count() > FIND_CHIP_MAX_CHARS {
        let head: String = query.chars().take(FIND_CHIP_MAX_CHARS - 1).collect();
        format!("{head}…")
    } else {
        query.to_owned()
    };
    let reopen = ui
        .button(format!("🔎 {shown}"))
        .on_hover_text(format!("Find: {query}\nClick to edit"));
    let clear = ui.small_button("x").on_hover_text("Clear find");
    if clear.clicked() {
        Some(FindChipEvent::Clear)
    } else if reopen.clicked() {
        Some(FindChipEvent::Reopen)
    } else {
        None
    }
}

/// A clickable column header for `key`. The active one shows the direction.
fn sort_header(ui: &mut egui::Ui, text: &str, key: SortKey, sort: &mut TreeSort) {
    let active = sort.key == key;
    let text = match (active, sort.descending) {
        (false, _) => text.to_owned(),
        (true, false) => format!("{text} ⏶"),
        (true, true) => format!("{text} ⏷"),
    };
    let hover = match key {
        SortKey::Name => "Sort by name",
        SortKey::State => "Sort by state",
    };
    if ui
        .selectable_label(active, text)
        .on_hover_text(hover)
        .clicked()
    {
        *sort = sort.clicked(key);
    }
}

fn handle_drops(
    ui: &egui::Ui,
    root_1: &mut Option<PathBuf>,
    root_2: &mut Option<PathBuf>,
    rect1: egui::Rect,
    rect2: egui::Rect,
) -> bool {
    return ui.input(|i| {
        if let Some(drop_pos) = i.pointer.hover_pos() {
            for dropped_file in &i.raw.dropped_files {
                if let Some(path) = &dropped_file.path {
                    if rect1.contains(drop_pos) {
                        let _ = set_root(root_1, Side::Folder1, path);
                        return true;
                    } else if rect2.contains(drop_pos) {
                        let _ = set_root(root_2, Side::Folder2, path);
                        return true;
                    }
                    break;
                }
            }
        }
        return false;
    });
}

#[derive(Clone)]
struct PathBarError {
    root: Option<PathBuf>,
    reason: String,
}

fn root_path_bar(ui: &mut egui::Ui, roots: [&mut Option<PathBuf>; 2], side: Side) {
    let paths = roots
        .each_ref()
        .map(|r| r.as_ref().map(|p| p.to_string_lossy().into_owned()));
    let dim = match &paths {
        [Some(a), Some(b)] => common_parent(a, false, b, false),
        _ => 0,
    };
    let index = match side {
        Side::Folder1 => 0,
        Side::Folder2 => 1,
    };
    let segments = paths[index].as_deref().map(split_path).unwrap_or_default();
    // Keyed to the root it was rejected against, so any later root change clears the red.
    let error_id = ui.make_persistent_id(("root_path_bar_error", index));
    let error = ui
        .data(|d| d.get_temp::<PathBarError>(error_id))
        .filter(|e| e.root == *roots[index]);
    let action = path_bar(
        ui,
        ("root_path_bar", index),
        &segments,
        dim,
        |i| i + 1 < segments.len(),
        error.as_ref().map(|e| e.reason.as_str()),
        false,
    );
    if let Some(action) = action {
        let root = roots[index].clone();
        match apply_path_bar(roots, side, action) {
            Ok(()) => ui.data_mut(|d| d.remove::<PathBarError>(error_id)),
            Err(e) => ui.data_mut(|d| {
                let reason = e.to_string();
                d.insert_temp(error_id, PathBarError { root, reason })
            }),
        }
    }
}

pub(crate) fn apply_path_bar(
    roots: [&mut Option<PathBuf>; 2],
    side: Side,
    action: PathBarAction,
) -> Result<(), InvalidRoot> {
    let [root_1, root_2] = roots;
    let root = match side {
        Side::Folder1 => root_1,
        Side::Folder2 => root_2,
    };
    let path = match action {
        PathBarAction::Segment(i) => {
            let current = root.as_ref().expect("segment clicked on an empty path bar");
            PathBuf::from(&split_path(&current.to_string_lossy())[i].path)
        }
        PathBarAction::Commit(text) => PathBuf::from(text.trim().trim_matches('"')),
    };
    set_root(root, side, &path)
}

#[derive(PartialEq, Debug, Clone)]
pub enum DiffState {
    Different(FsNodeId, FsNodeId),
    Same(FsNodeId, FsNodeId),
    Similar(FsNodeId, FsNodeId),
    OnlyInFirst(FsNodeId),
    OnlyInSecond(FsNodeId),
    Pending(FsNodeId, FsNodeId),
}

impl DiffState {
    pub fn first(&self) -> Option<FsNodeId> {
        match &self {
            DiffState::Same(path_buf, _)
            | DiffState::Different(path_buf, _)
            | DiffState::Similar(path_buf, _)
            | DiffState::Pending(path_buf, _) => Some(*path_buf),
            DiffState::OnlyInFirst(path_buf) => Some(*path_buf),
            DiffState::OnlyInSecond(..) => None,
        }
    }
    pub fn second(&self) -> Option<FsNodeId> {
        match &self {
            DiffState::Same(_, path_buf1)
            | DiffState::Different(_, path_buf1)
            | DiffState::Similar(_, path_buf1)
            | DiffState::Pending(_, path_buf1) => Some(*path_buf1),
            DiffState::OnlyInFirst(..) => None,
            DiffState::OnlyInSecond(path_buf) => Some(*path_buf),
        }
    }
}

pub fn ui_custom_diff_state(ui: &mut egui::Ui, state: &DiffState) -> egui::response::Response {
    let green = egui::Color32::from_rgb(0x7E, 0xD3, 0x21);
    let red = egui::Color32::from_rgb(0xD0, 0x02, 0x1B);
    let yellow = egui::Color32::from_rgb(0xF8, 0xE7, 0x1C);
    let blue = egui::Color32::from_rgb(0x4A, 0x90, 0xE2);
    // let teal = egui::Color32::from_rgb(0x50, 0xE3, 0xC2);

    match state {
        DiffState::Same(..) => ui.label(RichText::new("=").color(green)),
        DiffState::Different(..) => ui.label(RichText::new("≠").color(red)),
        DiffState::Similar(..) => ui.label(RichText::new("≈").color(yellow)),
        DiffState::OnlyInFirst(..) => ui.label(RichText::new("−").color(blue)),
        DiffState::OnlyInSecond(..) => ui.label(RichText::new("+").color(blue)),
        DiffState::Pending(..) => ui
            .add(egui::Spinner::new().size(ui.text_style_height(&egui::TextStyle::Body)))
            .on_hover_text("not compared"),
    }
}

fn file_diff_state(
    rel_path: &str,
    left: (FsNodeId, &FsNode),
    right: (FsNodeId, &FsNode),
    tolerance: &Tolerance,
    pair_state: &mut impl FnMut(&str, [&FsNode; 2]) -> (Option<PairState>, Option<f64>),
) -> DiffState {
    if right.1.is_dir() {
        return DiffState::Different(left.0, right.0);
    }
    let (exact, likeness) = pair_state(rel_path, [left.1, right.1]);
    let sides = [left.1, right.1].map(|node| Some(SideMeta::of(node)));
    if tolerance.is_similar(sides, exact, likeness) {
        return DiffState::Similar(left.0, right.0);
    }
    match exact {
        Some(PairState::Same) => DiffState::Same(left.0, right.0),
        Some(PairState::Different) => DiffState::Different(left.0, right.0),
        None => DiffState::Pending(left.0, right.0),
    }
}

pub fn aggregate_folder_state(
    l_id: FsNodeId,
    r_id: FsNodeId,
    children: impl IntoIterator<Item = DiffState>,
) -> DiffState {
    let mut state = DiffState::Same(l_id, r_id);
    for child in children {
        match child {
            DiffState::Same(..) => {}
            DiffState::Similar(..) => {
                if matches!(state, DiffState::Same(..)) {
                    state = DiffState::Similar(l_id, r_id);
                }
            }
            DiffState::Pending(..) => state = DiffState::Pending(l_id, r_id),
            _ => return DiffState::Different(l_id, r_id),
        }
    }
    state
}

fn folder_diff_state(
    parent_path: &str,
    entries_map: &BTreeMap<
        String,
        (
            Option<(FsNodeId, &FsNode, FsNodeDepth)>,
            Option<(FsNodeId, &FsNode, FsNodeDepth)>,
        ),
    >,
    file_states: &HashMap<&str, DiffState>,
) -> DiffState {
    let (current_left, current_right) = entries_map
        .get(parent_path)
        .cloned()
        .unwrap_or((None, None));
    let l_id = current_left.map(|l| l.0).unwrap_or(0);
    let r_id = current_right.map(|r| r.0).unwrap_or(0);

    let prefix = if parent_path.is_empty() {
        String::new()
    } else {
        format!("{}/", parent_path)
    };

    // Over all descendant entries, which gives the same result as the rule applied bottom-up.
    let descendants = entries_map
        .range(prefix.clone()..)
        .take_while(|(path, _)| path.starts_with(&prefix))
        .filter(|(path, _)| path.as_str() != parent_path)
        .filter_map(|(path, (left, right))| match (left, right) {
            (Some((li, _, _)), None) => Some(DiffState::OnlyInFirst(*li)),
            (None, Some((ri, _, _))) => Some(DiffState::OnlyInSecond(*ri)),
            (Some((_, ln, _)), Some(_)) if !ln.is_dir() => Some(file_states[path.as_str()].clone()),
            _ => None,
        });
    aggregate_folder_state(l_id, r_id, descendants)
}

fn on_row_item_clicked(
    file_system_1_view: Option<&FileSystemView>,
    file_system_2_view: Option<&FileSystemView>,
    entry: &VisibleRowTwoFolderDiff,
    config: &DiffToolConfig,
) -> Option<ToolMessage> {
    log::info!("on_row_iten_clicked");

    let path1 = entry.diff_state.first();
    let path2 = entry.diff_state.second();

    match (path1, path2) {
        (None, None) => panic!("should not have a row if both paths are none"),
        (None, Some(p)) | (Some(p), None) => {
            log::info!("nothing to diff, only in one tree {:?}", p);
            None
        }
        (Some(path1), Some(path2)) => {
            assert_eq!(file_system_1_view.is_some(), file_system_2_view.is_some()); // Model/View diff
            let mut message = ToolMessage::default();
            let Some(diff_tool) =
                config.choose_tool(&KindTable::default(), Path::new(&entry.rel_path))
            else {
                let error = format!(
                    "diffing failed: default diff tool {:?} is missing",
                    config.default_tool
                );
                log::error!("{error}");
                message.error = Some(error);
                return Some(message);
            };
            if config.mark_undiffable
                && let Some(reason) = cant_diff(entry.file_kinds, diff_tool)
            {
                let warning = format!("{}: opening anyway, {reason}", entry.rel_path);
                log::warn!("{warning}");
                message.warning = Some(warning);
            }
            let exe = match resolve_tool_exe(&diff_tool.exe_path) {
                Ok(exe) => exe,
                Err(err) => {
                    let error = format!("diffing failed: {err}");
                    log::error!("{error}");
                    message.error = Some(error);
                    return Some(message);
                }
            };
            let left = file_system_1_view
                .unwrap()
                .file_system
                .get_node(path1)
                .unwrap()
                .as_path()
                .as_ref()
                .to_string_lossy()
                .into_owned();
            let right = file_system_2_view
                .unwrap()
                .file_system
                .get_node(path2)
                .unwrap()
                .as_path()
                .as_ref()
                .to_string_lossy()
                .into_owned();
            let (left_name, right_name) = folder_labels(&entry.rel_path);
            let result = open_diff_tool(
                &exe,
                diff_tool,
                &DiffPair {
                    left: &left,
                    right: &right,
                    left_name: &left_name,
                    right_name: &right_name,
                },
            );
            if let Err(err) = result {
                // The spawn error doesn't name the program.
                let error = format!(
                    "diffing failed: {} ({}): {err}",
                    diff_tool.name,
                    exe.display()
                );
                log::error!("{error}");
                message.error = Some(error);
            };
            (message != ToolMessage::default()).then_some(message)
        }
    }
}

/// `flat_rel_path`: in flat mode, the row's relative path, drawn instead of the indented name and
/// without the expander, since rows of different folders sit next to each other.
fn render_diff_side(
    ui: &mut egui::Ui,
    view: Option<&FileSystemView>,
    node_id: Option<FsNodeId>,
    depth: FsNodeDepth,
    is_dir: bool,
    is_collapsed: bool,
    color: Option<egui::Color32>,
    row_height: f32,
    flat_rel_path: Option<&str>,
    list_rel_path: Option<&str>,
    highlight: &[Range<usize>],
    mut on_select: impl FnMut(),
    mut on_open: impl FnMut(),
    mut on_toggle: impl FnMut(),
) -> Option<egui::Response> {
    let name_text = |text: &str| {
        let text = RichText::new(text);
        let text = if is_dir { text.strong() } else { text };
        match color {
            Some(color) => text.color(color),
            None => text,
        }
    };
    // `highlight` is in `name`. Matched parts only get a background, so widths stay as measured.
    let name_label = |ui: &egui::Ui, prefix: &str, name: &str| -> egui::WidgetText {
        if highlight.is_empty() {
            return name_text(&format!("{prefix}{name}")).into();
        }
        let mut job = LayoutJob::default();
        let mut append = |text: &str, matched: bool| {
            if text.is_empty() {
                return;
            }
            let text = name_text(text);
            let text = if matched {
                text.background_color(FIND_MATCH_BG)
            } else {
                text
            };
            text.append_to(
                &mut job,
                ui.style(),
                egui::FontSelection::Default,
                ui.text_valign(),
            );
        };
        append(prefix, false);
        let mut at = 0;
        for range in highlight {
            append(&name[at..range.start], false);
            append(&name[range.clone()], true);
            at = range.end;
        }
        append(&name[at..], false);
        job.into()
    };
    ui.horizontal(|ui| {
        if let Some(rel_path) = flat_rel_path {
            if let (Some(v), Some(id)) = (view, node_id) {
                if v.file_system.get_node(id).is_some() {
                    let prefix = if is_dir { "📁 " } else { "" };
                    let label_resp = ui
                        .label(name_label(ui, prefix, rel_path))
                        .interact(egui::Sense::click());
                    if label_resp.clicked() {
                        on_select();
                    }
                    if label_resp.double_clicked() && !is_dir {
                        on_open();
                    }
                    return Some(label_resp);
                }
            }
            return None;
        }

        ui.add_space((depth as f32) * TREE_INDENT);

        if let (Some(v), Some(id)) = (view, node_id) {
            if let Some(node) = v.file_system.get_node(id) {
                if is_dir {
                    // Openness: 0.0 is closed, 1.0 is open
                    let openness = if is_collapsed { 0.0 } else { 1.0 };
                    let (_rect, response) = ui.allocate_exact_size(
                        egui::vec2(EXPANDER_WIDTH, row_height),
                        egui::Sense::click(),
                    );
                    egui::collapsing_header::paint_default_icon(ui, openness, &response);

                    if response.clicked() {
                        on_toggle();
                    }

                    let label_resp = ui
                        .label(name_label(ui, "📁 ", node.display_name()))
                        .interact(egui::Sense::click());

                    if label_resp.clicked() {
                        on_select();
                    }
                    if label_resp.double_clicked() {
                        on_toggle();
                    }
                    return Some(label_resp);
                } else {
                    let mut label_resp = ui
                        .label(name_label(ui, "", node.display_name()))
                        .interact(egui::Sense::click());
                    if let Some(rel_path) = list_rel_path {
                        label_resp = label_resp.on_hover_text(rel_path);
                    }

                    if label_resp.clicked() {
                        on_select();
                    }
                    if label_resp.double_clicked() {
                        on_open();
                    }
                    return Some(label_resp);
                }
            }
        }
        None
    })
    .inner
}

/// Must match what `render_diff_side` draws for a name `text_width` wide.
fn side_width(
    layout: RowLayout,
    depth: FsNodeDepth,
    is_dir: bool,
    text_width: f32,
    spacing: f32,
) -> f32 {
    let indent = match layout {
        RowLayout::Tree => depth as f32 * TREE_INDENT,
        RowLayout::RelPaths => return text_width,
        RowLayout::FileList => 0.0,
    };
    let expander = if is_dir {
        EXPANDER_WIDTH + spacing
    } else {
        0.0
    };
    indent + expander + text_width
}

#[derive(Clone, Copy, PartialEq)]
enum RowLayout {
    Tree,
    /// Flat mode: relative paths in place of indented names.
    RelPaths,
    /// Diff only without parents: unindented names, the relative path in a tooltip.
    FileList,
}

/// Checks the kinds before choosing the tool, which allocates per rule, since this runs per drawn row.
fn undiffable(entry: &VisibleRowTwoFolderDiff, config: &DiffToolConfig) -> Option<CantDiff> {
    if !config.mark_undiffable || entry.file_kinds.contains(&None) {
        return None;
    }
    let tool = config.choose_tool_quietly(&KindTable::default(), Path::new(&entry.rel_path))?;
    cant_diff(entry.file_kinds, tool)
}

#[derive(Clone, Copy)]
enum CenterSlot {
    Tag(usize),
    State,
    Likeness,
}

/// Sized from the font, not the rows, so the center keeps its width as rows scroll by.
struct CenterLayout {
    tag: f32,
    state: f32,
    likeness: Option<f32>,
}

impl CenterLayout {
    fn measure(ui: &egui::Ui, likeness_bar: bool) -> Self {
        let tag_font = egui::TextStyle::Monospace.resolve(ui.style());
        let header_font = egui::TextStyle::Button.resolve(ui.style());
        let (tag_glyph, header) = ui.fonts_mut(|f| {
            let header = ["≠ ⏶", "≠ ⏷"].map(|text| {
                f.layout_no_wrap(
                    text.to_owned(),
                    header_font.clone(),
                    egui::Color32::PLACEHOLDER,
                )
                .size()
                .x
            });
            (f.glyph_width(&tag_font, 'M'), header[0].max(header[1]))
        });
        let tag = 4.0 * tag_glyph;
        // The sort header is a selectable label, padded like a button.
        let state = (header + 2.0 * ui.spacing().button_padding.x)
            .max(ui.text_style_height(&egui::TextStyle::Body));
        Self {
            tag,
            state,
            likeness: likeness_bar.then_some(1.5 * tag),
        }
    }

    fn slots(&self) -> impl Iterator<Item = (CenterSlot, f32)> {
        [
            (CenterSlot::Tag(0), self.tag),
            (CenterSlot::State, self.state),
        ]
        .into_iter()
        .chain(self.likeness.map(|width| (CenterSlot::Likeness, width)))
        .chain([(CenterSlot::Tag(1), self.tag)])
    }

    fn width(&self, spacing: f32) -> f32 {
        self.slots().map(|(_, width)| width + spacing).sum::<f32>() - spacing
    }

    fn show(&self, ui: &mut egui::Ui, mut add: impl FnMut(CenterSlot, &mut egui::Ui)) {
        // Selectable text would sit above the split strip and take its drag and double-click.
        ui.style_mut().interaction.selectable_labels = false;
        for (i, (slot, width)) in self.slots().enumerate() {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(width, ui.available_height()),
                egui::Sense::hover(),
            );
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(i)
                    .max_rect(rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            add(slot, &mut child);
        }
    }
}

fn side_content_widths<'a>(
    ui: &egui::Ui,
    views: [Option<&FileSystemView>; 2],
    rows: impl Iterator<Item = &'a VisibleRowTwoFolderDiff>,
    layout: RowLayout,
) -> [f32; 2] {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let spacing = ui.spacing().item_spacing.x;
    ui.fonts_mut(|fonts| {
        let mut glyph_width = |c: char| fonts.glyph_width(&font, c);
        let folder_width = text_width("📁 ", &mut glyph_width);
        let mut widths = [0.0f32; 2];
        for entry in rows {
            let ids = [entry.diff_state.first(), entry.diff_state.second()];
            for side in 0..2 {
                let Some(node) = side_node(views[side], ids[side]) else {
                    continue;
                };
                let name = if layout == RowLayout::RelPaths {
                    entry.rel_path.as_str()
                } else {
                    node.display_name()
                };
                let folder = if entry.is_dir { folder_width } else { 0.0 };
                let text = folder + text_width(name, &mut glyph_width);
                let width = side_width(layout, entry.depth, entry.is_dir, text, spacing);
                widths[side] = widths[side].max(width);
            }
        }
        widths
    })
}

/// Fills `cell_x` and half the gaps around it, like egui_extras' stripes, so a tinted row has
/// no seams. A cell's own clip stops short of the gaps.
fn fill_cell(ui: &egui::Ui, cell_x: egui::Rangef, color: egui::Color32) {
    let rect = egui::Rect::from_x_y_ranges(
        cell_x.expand(0.5 * ui.spacing().item_spacing.x),
        ui.clip_rect().y_range(),
    );
    egui::Painter::new(ui.ctx().clone(), ui.layer_id(), rect).rect_filled(rect, 0.0, color);
}

#[derive(Clone, Copy)]
struct RowCell<'a> {
    views: [Option<&'a FileSystemView>; 2],
    entry: &'a VisibleRowTwoFolderDiff,
    dimmed: bool,
    pending: bool,
    row_height: f32,
    diff_tool_config: &'a DiffToolConfig,
    find_matcher: Option<&'a Matcher>,
    match_path: bool,
}

struct SideCell {
    clicked: bool,
    toggle: bool,
    action: Option<RowAction>,
}

/// Draws one side of a row that `two_folder_cursor_rows` doesn't hide. `clicked` is a click
/// with either button, which moves the cursor to the row.
fn render_side_cell(
    ui: &mut egui::Ui,
    row: &RowCell,
    side: usize,
    tool_message: &mut Option<ToolMessage>,
    layout: RowLayout,
    extra_menu: Option<&mut RowMenuItems<'_>>,
) -> SideCell {
    let RowCell {
        views,
        entry,
        dimmed,
        pending,
        row_height,
        diff_tool_config,
        find_matcher,
        match_path,
    } = *row;
    let highlight = find_matcher.map_or_else(Vec::new, |matcher| {
        drawn_ranges(
            matcher,
            match_path,
            &entry.rel_path,
            layout == RowLayout::RelPaths,
        )
    });
    let rel_path = |shown: RowLayout| (layout == shown).then_some(entry.rel_path.as_str());
    let flat_rel_path = rel_path(RowLayout::RelPaths);
    let list_rel_path = rel_path(RowLayout::FileList);
    let depth = if list_rel_path.is_some() {
        0
    } else {
        entry.depth
    };
    let ids = [entry.diff_state.first(), entry.diff_state.second()];
    let is_collapsed = match (views[side], ids[side]) {
        (Some(view), Some(id)) => view.is_collapsed(id),
        _ => false,
    };
    // Before the labels, so they are on top and keep their own clicks.
    let cell_rect = egui::Rect::from_x_y_ranges(ui.clip_rect().x_range(), ui.max_rect().y_range());
    let cell = ui.interact(cell_rect, ui.id().with("side_cell"), egui::Sense::click());
    let mut select = false;
    let mut toggle = false;
    let label = render_diff_side(
        ui,
        views[side],
        ids[side],
        depth,
        entry.is_dir,
        is_collapsed,
        if pending {
            Some(PENDING_DELETION_COLOR)
        } else {
            dimmed.then_some(DIMMED_TEXT_COLOR)
        },
        row_height,
        flat_rel_path,
        list_rel_path,
        &highlight,
        || select = true,
        || *tool_message = on_row_item_clicked(views[0], views[1], entry, diff_tool_config),
        || toggle = true,
    );

    let nodes = [side_node(views[0], ids[0]), side_node(views[1], ids[1])];
    let entries = nodes.map(|node| {
        node.map(|n| {
            if n.is_dir() {
                Entry::Folder
            } else {
                Entry::File
            }
        })
    });
    let menu_side = [Side::Folder1, Side::Folder2][side];
    // Labels take their own clicks, so the cell alone misses a right-click on the name.
    let response = label.map_or(cell.clone(), |label| cell | label);
    let mut action = None;
    if let Some(menu) = row_menu(entries, menu_side) {
        response.context_menu(|ui| {
            let paths = nodes.map(|node| node.map(|n| n.as_path()));
            let paths = [0, 1].map(|i| paths[i].as_ref().map(|p| p.as_ref()));
            action = show_row_menu(ui, &menu, paths, menu_side);
            if let (Some(extra_menu), Some(id)) = (extra_menu, ids[side]) {
                extra_menu(ui, id);
            }
        });
    }
    SideCell {
        clicked: select || response.clicked() || response.secondary_clicked(),
        toggle,
        action,
    }
}

fn render_center_cell(
    ui: &mut egui::Ui,
    row: &RowCell,
    center: &CenterLayout,
    compare: &TreeCompare,
    tolerance: &Tolerance,
) {
    let RowCell {
        views,
        entry,
        dimmed,
        pending: _,
        row_height,
        diff_tool_config,
        ..
    } = *row;
    let nodes = [
        side_node(views[0], entry.diff_state.first()),
        side_node(views[1], entry.diff_state.second()),
    ];
    let undiffable = undiffable(entry, diff_tool_config);
    center.show(ui, |slot, ui| {
        if let (CenterSlot::Tag(_), Some(reason)) = (slot, &undiffable) {
            let stroke = egui::Stroke::new(1.0, ui.visuals().warn_fg_color);
            ui.painter()
                .rect_stroke(ui.max_rect(), 2.0, stroke, egui::StrokeKind::Inside);
            ui.interact(ui.max_rect(), ui.id().with("tag"), egui::Sense::hover())
                .on_hover_text(reason.to_string());
        }
        if dimmed {
            ui.multiply_opacity(0.5);
        }
        match slot {
            CenterSlot::Tag(side) => {
                if let Some(kind) = entry.file_kinds[side] {
                    let (text, color) = if entry.blueprints[side] {
                        ("BP", BLUEPRINT_TAG_COLOR)
                    } else {
                        (kind.tag(), tag_color(kind))
                    };
                    // Not selectable: a selectable label takes the hover, hiding the tag's tooltip.
                    let text = RichText::new(text).monospace().color(color);
                    ui.add(egui::Label::new(text).selectable(false));
                }
            }
            CenterSlot::State => {
                let response = ui_custom_diff_state(ui, &entry.diff_state);
                if matches!(entry.diff_state, DiffState::Similar(..)) {
                    let (sides, likeness) = match nodes {
                        [Some(left), Some(right)] if !entry.is_dir => (
                            Some([left, right].map(SideMeta::of)),
                            compare
                                .likeness_of(&entry.rel_path, [left, right])
                                .and_then(PairLikeness::ratio),
                        ),
                        _ => (None, None),
                    };
                    response.on_hover_text(similar_tooltip(tolerance, sides, likeness));
                }
            }
            CenterSlot::Likeness => {
                ui_likeness_cell(ui, likeness_cell(entry, nodes, compare), row_height);
            }
        }
    });
}

fn side_node(view: Option<&FileSystemView>, id: Option<FsNodeId>) -> Option<&FsNode> {
    view?.file_system.get_node(id?)
}

/// Each line sits under its folder's expander, in the children's indentation.
fn paint_guides(ui: &egui::Ui, guides: &[Guide]) {
    let cell = ui.max_rect();
    // Spans the item spacing between rows, like the folder tint, so the lines have no gaps.
    let half_spacing = 0.5 * ui.spacing().item_spacing.y;
    let stroke = egui::Stroke::new(1.0, GUIDE_LINE_COLOR);
    for (k, guide) in guides.iter().enumerate() {
        let bottom = match guide {
            Guide::Blank => continue,
            Guide::Through => cell.bottom() + half_spacing,
            Guide::End => cell.center().y,
        };
        let x = cell.left() + (k + 1) as f32 * TREE_INDENT + 0.5 * EXPANDER_WIDTH;
        let x = ui.painter().round_to_pixel_center(x);
        ui.painter()
            .vline(x, (cell.top() - half_spacing)..=bottom, stroke);
    }
}

fn apply_cursor_request(
    file_system_1_view: Option<&mut FileSystemView>,
    file_system_2_view: Option<&mut FileSystemView>,
    rows: &[VisibleRowTwoFolderDiff],
    request: &CursorRequest,
    diff_tool_config: &DiffToolConfig,
    tool_message: &mut Option<ToolMessage>,
) {
    let Some(entry) = rows.iter().find(|row| row.rel_path == request.rel_path()) else {
        log::error!("cursor request for a row that isn't there: {request:?}");
        return;
    };
    match request {
        CursorRequest::Open(_) => {
            *tool_message = on_row_item_clicked(
                file_system_1_view.as_deref(),
                file_system_2_view.as_deref(),
                entry,
                diff_tool_config,
            );
        }
        // Set rather than toggled per side, so the sides end up agreeing.
        CursorRequest::Expand(_) | CursorRequest::Collapse(_) => {
            let collapse = matches!(request, CursorRequest::Collapse(_));
            let sides = [
                (file_system_1_view, entry.diff_state.first()),
                (file_system_2_view, entry.diff_state.second()),
            ];
            for (view, id) in sides {
                if let (Some(view), Some(id)) = (view, id) {
                    if collapse {
                        view.collapsed.insert(id, true);
                    } else {
                        view.collapsed.remove(&id);
                    }
                }
            }
        }
    }
}

/// The arrows and Enter without modifiers, in press order. Arrow repeats are kept, Enter
/// repeats are dropped: a held Enter would open the diff tool or flip a folder per repeat.
/// Modified arrows are left to other bindings.
fn cursor_keys(input: &egui::InputState) -> Vec<egui::Key> {
    if !input.modifiers.is_none() {
        return Vec::new();
    }
    input
        .events
        .iter()
        .filter_map(|event| match event {
            egui::Event::Key {
                key:
                    key @ (egui::Key::ArrowDown
                    | egui::Key::ArrowUp
                    | egui::Key::ArrowLeft
                    | egui::Key::ArrowRight),
                pressed: true,
                ..
            } => Some(*key),
            egui::Event::Key {
                key: egui::Key::Enter,
                pressed: true,
                repeat: false,
                ..
            } => Some(egui::Key::Enter),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::ui_egui::fs_tree::apply_path_bar;
    use crate::ui_egui::fs_tree::{
        CursorRowsCache, DiffState, EXPANDER_WIDTH, FileSystemView, RowLayout, RowsId, TREE_INDENT,
        VisibleRowTwoFolderDiff, aggregate_folder_state, apply_blueprint_results,
        apply_cursor_request, cursor_keys, side_width, single_side_rows, tree_hidden,
        two_folder_cursor_rows,
    };
    use crate::ui_egui::roots::{InvalidRoot, Side};
    use crate::ui_egui::status_line::ToolMessage;
    use crate::ui_egui::tree_compare::pending_pairs;
    use crate::ui_egui::tree_cursor::{CursorRequest, TreeCursor};
    use crate::ui_egui::tree_filter::{DiffFilter, arrange_two_folder_rows};
    use crate::ui_egui::tree_sort::{SortKey, TreeSort, sort_two_folder_rows};
    use crate::ui_egui::tree_tolerance::{SizeDelta, Tolerance};
    use std::time::{Duration, SystemTime};
    use zcommon::path::split_path;
    use zcommon::ui_egui::pathbar::PathBarAction;
    use zhashdiff::external_diff_tool::{DiffTool, DiffToolConfig};

    use std::cell::Cell;
    use std::collections::HashMap;
    use std::fs::{self, File};
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::{TempDir, tempdir};
    use zcommon::kinds::{FileKind, KindTable};
    use zhashdiff::comparison::PairState;
    use zhashdiff::filter::{PathFilter, PatternList};
    use zhashdiff::fs::{FileSystemModel, FsIsDir, FsNodeDepth, FsNodeId};

    struct CollapsedTestCase {
        name: &'static str,
        structure: Vec<(&'static str, FsIsDir)>,
        collapsed: Vec<&'static str>,
        expected: Vec<(&'static str, FsNodeDepth)>,
    }

    fn find_id_by_rel_path(fs: &FileSystemModel, root: &Path, rel: &str) -> FsNodeId {
        let target = root.join(rel);
        for (id, node, _) in fs.iter_tree() {
            if node.as_path().as_ref() == target {
                return id;
            }
        }
        panic!("Test setup error: path {:?} not found in model", target);
    }

    #[allow(dead_code)]
    fn get_rel(file_system: &FileSystemModel, root: &Path, id: FsNodeId) -> String {
        let node = file_system.get_node(id).expect("Node ID not found");
        if id == file_system.get_root_node_id() {
            return String::new();
        }

        node.as_path()
            .as_ref()
            .strip_prefix(root)
            .unwrap_or(node.as_path().as_ref())
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
            .trim_start_matches('/')
            .to_string()
    }

    #[allow(dead_code)]
    fn get_rel_from_diff_state(
        file_system_1: &FileSystemModel,
        file_system_2: &FileSystemModel,
        r1: &Path,
        r2: &Path,
        state: &DiffState,
    ) -> String {
        match state {
            DiffState::OnlyInFirst(id) => get_rel(file_system_1, r1, *id),
            DiffState::OnlyInSecond(id) => get_rel(file_system_2, r2, *id),
            DiffState::Same(id, _)
            | DiffState::Different(id, _)
            | DiffState::Similar(id, _)
            | DiffState::Pending(id, _) => get_rel(file_system_1, r1, *id),
        }
    }

    fn format_tree(rows: &[(String, FsNodeDepth)]) -> String {
        rows.iter()
            .map(|(path, depth)| {
                let indent = "  ".repeat(*depth as usize);
                let name = if path.is_empty() { "/" } else { path };
                format!("{}└─ {}", indent, name)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[allow(dead_code)]
    fn format_diff_tree_two(
        left: &[(String, FsNodeDepth)],
        right: &[(String, FsNodeDepth)],
        diff: &[(String, FsNodeDepth, String)],
    ) -> String {
        let mut output = String::new();
        output.push_str(&format!(
            "{:<30} | {:<30} | {:<30}\n",
            "LEFT TREE", "DIFF RESULT", "RIGHT TREE"
        ));
        output.push_str(&"-".repeat(96));
        output.push('\n');

        let l_map: HashMap<&str, FsNodeDepth> =
            left.iter().map(|(p, d)| (p.as_str(), *d)).collect();
        let r_map: HashMap<&str, FsNodeDepth> =
            right.iter().map(|(p, d)| (p.as_str(), *d)).collect();

        for (path, depth, marker) in diff {
            let l_row = l_map
                .get(path.as_str())
                .map(|d| {
                    format!(
                        "{}└─ {}",
                        "  ".repeat(*d as usize),
                        if path.is_empty() { "/" } else { path }
                    )
                })
                .unwrap_or_default();

            let r_row = r_map
                .get(path.as_str())
                .map(|d| {
                    format!(
                        "{}└─ {}",
                        "  ".repeat(*d as usize),
                        if path.is_empty() { "/" } else { path }
                    )
                })
                .unwrap_or_default();

            let d_row = format!(
                "{} {}└─ {}",
                marker,
                "  ".repeat(*depth as usize),
                if path.is_empty() { "/" } else { path }
            );

            output.push_str(&format!("{:<30} | {:<30} | {:<30}\n", l_row, d_row, r_row));
        }
        output
    }

    fn write_files(root: &Path, files: &[(&str, &str)]) {
        for (rel_path, content) in files {
            let path = root.join(rel_path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
    }

    fn load_view(root: &Path) -> FileSystemView {
        FileSystemView::new(Arc::new(FileSystemModel::new(root).unwrap()))
    }

    /// b.txt differs, a.txt and everything under sub/ is equal, and each side has a file
    /// of its own.
    fn two_folder_trees() -> (TempDir, TempDir) {
        let left = tempdir().unwrap();
        let right = tempdir().unwrap();
        let shared = [
            ("a.txt", "same a"),
            ("sub/c.txt", "same c"),
            ("sub/deep/d.txt", "same d"),
        ];
        write_files(left.path(), &shared);
        write_files(right.path(), &shared);
        write_files(left.path(), &[("b.txt", "left b"), ("left_only.txt", "l")]);
        write_files(
            right.path(),
            &[("b.txt", "right b"), ("right_only.txt", "r")],
        );
        (left, right)
    }

    fn state_kind(state: &DiffState) -> &'static str {
        match state {
            DiffState::Different(..) => "Different",
            DiffState::Same(..) => "Same",
            DiffState::Similar(..) => "Similar",
            DiffState::OnlyInFirst(..) => "OnlyInFirst",
            DiffState::OnlyInSecond(..) => "OnlyInSecond",
            DiffState::Pending(..) => "Pending",
        }
    }

    #[test]
    fn two_folder_rows_have_the_expected_paths_states_and_order() {
        let (left_dir, right_dir) = two_folder_trees();
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());

        let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));

        let actual: Vec<(String, &str, FsNodeDepth, bool)> = rows
            .iter()
            .map(|row| {
                let rel = get_rel_from_diff_state(
                    &left.file_system,
                    &right.file_system,
                    left_dir.path(),
                    right_dir.path(),
                    &row.diff_state,
                );
                (rel, state_kind(&row.diff_state), row.depth, row.is_dir)
            })
            .collect();
        let expected = [
            ("", "Different", 0, true),
            ("a.txt", "Same", 1, false),
            ("b.txt", "Different", 1, false),
            ("left_only.txt", "OnlyInFirst", 1, false),
            ("right_only.txt", "OnlyInSecond", 1, false),
            ("sub", "Same", 1, true),
            ("sub/c.txt", "Same", 2, false),
            ("sub/deep", "Same", 2, true),
            ("sub/deep/d.txt", "Same", 3, false),
        ]
        .map(|(rel, kind, depth, is_dir)| (rel.to_string(), kind, depth, is_dir));
        assert_eq!(actual, expected);
    }

    fn build_counting(
        left: &FileSystemView,
        right: &FileSystemView,
        states: &mut PairStates,
        comparisons: &Cell<usize>,
    ) -> Vec<VisibleRowTwoFolderDiff> {
        build_filtered(left, right, states, comparisons, &PathFilter::default())
    }

    type PairStates = HashMap<String, PairState>;

    fn build_from_states(
        left: &FileSystemView,
        right: &FileSystemView,
        states: &PairStates,
        filter: &PathFilter,
    ) -> Vec<VisibleRowTwoFolderDiff> {
        build_tolerant_from_states(left, right, states, filter, &Tolerance::default())
    }

    fn build_tolerant_from_states(
        left: &FileSystemView,
        right: &FileSystemView,
        states: &PairStates,
        filter: &PathFilter,
        tolerance: &Tolerance,
    ) -> Vec<VisibleRowTwoFolderDiff> {
        FileSystemView::build_two_folder_diff_rows(
            Some(left),
            Some(right),
            filter,
            &KindTable::default(),
            tolerance,
            |rel_path, _| (states.get(rel_path).copied(), None),
            |_, _| None,
        )
        .unwrap()
    }

    fn build_filtered(
        left: &FileSystemView,
        right: &FileSystemView,
        states: &mut PairStates,
        comparisons: &Cell<usize>,
        filter: &PathFilter,
    ) -> Vec<VisibleRowTwoFolderDiff> {
        build_compared(
            left,
            right,
            states,
            comparisons,
            filter,
            &Tolerance::default(),
        )
    }

    fn build_compared(
        left: &FileSystemView,
        right: &FileSystemView,
        states: &mut PairStates,
        comparisons: &Cell<usize>,
        filter: &PathFilter,
        tolerance: &Tolerance,
    ) -> Vec<VisibleRowTwoFolderDiff> {
        let rows = build_tolerant_from_states(left, right, states, filter, tolerance);
        let pending = pending_pairs(&rows, [Some(left), Some(right)]);
        if pending.is_empty() {
            return rows;
        }
        for pair in pending {
            comparisons.set(comparisons.get() + 1);
            states.insert(pair.rel_path.clone(), pair.compare().state);
        }
        build_tolerant_from_states(left, right, states, filter, tolerance)
    }

    fn set_modified(root: &Path, rel_path: &str, modified: SystemTime) {
        File::options()
            .write(true)
            .open(root.join(rel_path))
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    #[test]
    fn the_time_criterion_makes_a_pair_similar_without_comparing_it() {
        let (left_dir, right_dir) = (tempdir().unwrap(), tempdir().unwrap());
        write_files(left_dir.path(), &[("x.txt", "aaaa"), ("y.txt", "aaaa")]);
        write_files(right_dir.path(), &[("x.txt", "bbbb"), ("y.txt", "bbbb")]);
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        set_modified(left_dir.path(), "x.txt", time);
        set_modified(right_dir.path(), "x.txt", time);
        set_modified(left_dir.path(), "y.txt", time);
        set_modified(right_dir.path(), "y.txt", time + Duration::from_secs(1));
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let tolerance = Tolerance {
            enabled: true,
            size: false,
            same_size_and_time: true,
            ..Tolerance::default()
        };
        let comparisons = Cell::new(0);

        let rows = build_compared(
            &left,
            &right,
            &mut PairStates::new(),
            &comparisons,
            &PathFilter::default(),
            &tolerance,
        );

        assert_eq!(
            rows_with_kinds(&rows),
            [
                ("", "Different"),
                ("x.txt", "Similar"),
                ("y.txt", "Different")
            ]
        );
        assert_eq!(comparisons.get(), 1, "only y.txt is compared");
    }

    #[test]
    fn turning_tolerance_off_restores_the_exact_states_without_comparing_again() {
        let (left_dir, right_dir) = two_folder_trees();
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let mut states = PairStates::new();
        let comparisons = Cell::new(0);
        let tolerance = Tolerance {
            enabled: true,
            max_size_delta: SizeDelta::Bytes(1),
            ..Tolerance::default()
        };
        let filter = PathFilter::default();

        let rows = build_compared(
            &left,
            &right,
            &mut states,
            &comparisons,
            &filter,
            &tolerance,
        );
        assert_eq!(kind_at(&rows, "b.txt"), "Similar");
        assert_eq!(comparisons.get(), 4);

        let off = Tolerance {
            enabled: false,
            ..tolerance
        };
        let rows = build_compared(&left, &right, &mut states, &comparisons, &filter, &off);
        assert_eq!(kind_at(&rows, "b.txt"), "Different");
        assert_eq!(comparisons.get(), 4);
    }

    #[test]
    fn a_folder_of_same_and_similar_files_is_similar_and_hidden_by_diff_only() {
        let (left_dir, right_dir) = (tempdir().unwrap(), tempdir().unwrap());
        write_files(
            left_dir.path(),
            &[("sub/a.txt", "same"), ("sub/b.txt", "left")],
        );
        write_files(
            right_dir.path(),
            &[("sub/a.txt", "same"), ("sub/b.txt", "right")],
        );
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let tolerance = Tolerance {
            enabled: true,
            max_size_delta: SizeDelta::Bytes(1),
            ..Tolerance::default()
        };

        let rows = build_compared(
            &left,
            &right,
            &mut PairStates::new(),
            &Cell::new(0),
            &PathFilter::default(),
            &tolerance,
        );
        assert_eq!(
            rows_with_kinds(&rows),
            [
                ("", "Similar"),
                ("sub", "Similar"),
                ("sub/a.txt", "Same"),
                ("sub/b.txt", "Similar"),
            ]
        );
        let diff_only = DiffFilter {
            diff_only: true,
            show_parents: true,
        };
        let shown = arrange_two_folder_rows(rows, diff_only, TreeSort::default());
        assert_eq!(rows_with_kinds(&shown), [("", "Similar")]);
    }

    #[test]
    fn rows_built_before_any_result_are_pending_and_folders_aggregate_them() {
        let (left_dir, right_dir) = two_folder_trees();
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());

        let rows = build_from_states(&left, &right, &PairStates::new(), &PathFilter::default());

        assert_eq!(
            rows_with_kinds(&rows),
            [
                ("", "Different"),
                ("a.txt", "Pending"),
                ("b.txt", "Pending"),
                ("left_only.txt", "OnlyInFirst"),
                ("right_only.txt", "OnlyInSecond"),
                ("sub", "Pending"),
                ("sub/c.txt", "Pending"),
                ("sub/deep", "Pending"),
                ("sub/deep/d.txt", "Pending"),
            ]
        );
        let pending: Vec<String> = pending_pairs(&rows, [Some(&left), Some(&right)])
            .into_iter()
            .map(|pair| pair.rel_path)
            .collect();
        assert_eq!(pending, ["a.txt", "b.txt", "sub/c.txt", "sub/deep/d.txt"]);
    }

    #[test]
    fn a_file_facing_a_folder_is_different_without_a_compare() {
        let (left_dir, right_dir) = (tempdir().unwrap(), tempdir().unwrap());
        write_files(left_dir.path(), &[("x", "")]);
        write_files(right_dir.path(), &[("x/inner.txt", "")]);
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());

        let rows = build_from_states(&left, &right, &PairStates::new(), &PathFilter::default());

        assert_eq!(kind_at(&rows, "x"), "Different");
        assert!(pending_pairs(&rows, [Some(&left), Some(&right)]).is_empty());
    }

    fn blacklist(text: &str) -> PathFilter {
        PathFilter {
            blacklist: PatternList::new(text),
            ..Default::default()
        }
    }

    fn rows_with_kinds(rows: &[VisibleRowTwoFolderDiff]) -> Vec<(&str, &'static str)> {
        rows.iter()
            .map(|r| (r.rel_path.as_str(), state_kind(&r.diff_state)))
            .collect()
    }

    #[test]
    fn blacklisted_entries_are_absent_on_both_sides() {
        let (left_dir, right_dir) = two_folder_trees();
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());

        let rows = build_filtered(
            &left,
            &right,
            &mut PairStates::default(),
            &Cell::new(0),
            &blacklist("B.TXT, *_only.txt"),
        );

        // b.txt is on both sides, left_only.txt and right_only.txt on one. With every
        // difference hidden, the root reads as Same.
        assert_eq!(
            rows_with_kinds(&rows),
            [
                ("", "Same"),
                ("a.txt", "Same"),
                ("sub", "Same"),
                ("sub/c.txt", "Same"),
                ("sub/deep", "Same"),
                ("sub/deep/d.txt", "Same"),
            ]
        );
    }

    #[test]
    fn a_blacklisted_folder_hides_its_whole_subtree() {
        let (left_dir, right_dir) = two_folder_trees();
        write_files(left_dir.path(), &[("sub/deep/more/e.txt", "l")]);
        // A file of the same name: the trailing slash spares it.
        write_files(right_dir.path(), &[("deep", "a file")]);
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());

        let rows = build_filtered(
            &left,
            &right,
            &mut PairStates::default(),
            &Cell::new(0),
            &blacklist("deep/"),
        );

        let paths: Vec<&str> = rows.iter().map(|r| r.rel_path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "",
                "a.txt",
                "b.txt",
                "deep",
                "left_only.txt",
                "right_only.txt",
                "sub",
                "sub/c.txt",
            ]
        );
    }

    #[test]
    fn a_folder_whose_only_difference_is_blacklisted_reads_as_same() {
        let (left_dir, right_dir) = two_folder_trees();
        write_files(left_dir.path(), &[("sub/build.log", "left")]);
        write_files(right_dir.path(), &[("sub/build.log", "right")]);
        write_files(left_dir.path(), &[("sub/deep/left.obj", "only left")]);
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let mut states = PairStates::default();
        let comparisons = Cell::new(0);

        let rows = build_counting(&left, &right, &mut states, &comparisons);
        assert_eq!(kind_at(&rows, "sub"), "Different");
        assert_eq!(kind_at(&rows, "sub/deep"), "Different");

        let rows = build_filtered(
            &left,
            &right,
            &mut states,
            &comparisons,
            &blacklist("*.log\n*.obj"),
        );
        assert_eq!(kind_at(&rows, "sub"), "Same");
        assert_eq!(kind_at(&rows, "sub/deep"), "Same");
        assert_eq!(kind_at(&rows, ""), "Different", "b.txt still differs");
    }

    #[test]
    fn the_root_is_never_filtered() {
        let (left_dir, right_dir) = two_folder_trees();
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());

        // Matches the roots' own names too (tempdirs are named .tmpXXXX).
        let rows = build_filtered(
            &left,
            &right,
            &mut PairStates::default(),
            &Cell::new(0),
            &blacklist("*, *.tmp*/"),
        );

        assert_eq!(rows_with_kinds(&rows), [("", "Same")]);
    }

    #[test]
    fn changing_the_filter_rebuilds_without_comparing_files_again() {
        let (left_dir, right_dir) = two_folder_trees();
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let comparisons = Cell::new(0);
        let mut states = PairStates::default();

        let unfiltered = build_counting(&left, &right, &mut states, &comparisons);
        assert_eq!(comparisons.get(), 4);

        let filtered = build_filtered(&left, &right, &mut states, &comparisons, &blacklist("sub"));
        assert!(filtered.iter().all(|r| !r.rel_path.starts_with("sub")));
        let cleared = build_counting(&left, &right, &mut states, &comparisons);
        assert_eq!(comparisons.get(), 4);
        assert_eq!(cleared, unfiltered);
    }

    fn whitelist(white: &str, black: &str) -> PathFilter {
        PathFilter {
            blacklist: PatternList::new(black),
            whitelist: PatternList::new(white),
        }
    }

    #[test]
    fn ancestors_of_a_whitelisted_file_are_visible_on_both_sides() {
        let (left_dir, right_dir) = two_folder_trees();
        for dir in [&left_dir, &right_dir] {
            write_files(dir.path(), &[("src/deep/main.rs", "same")]);
        }
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());

        let rows = build_filtered(
            &left,
            &right,
            &mut PairStates::default(),
            &Cell::new(0),
            &whitelist("*.rs", ""),
        );

        // Same means the entry is on both sides.
        assert_eq!(
            rows_with_kinds(&rows),
            [
                ("", "Same"),
                ("src", "Same"),
                ("src/deep", "Same"),
                ("src/deep/main.rs", "Same"),
            ]
        );
    }

    #[test]
    fn a_folder_with_no_whitelisted_descendant_is_hidden() {
        let (left_dir, right_dir) = two_folder_trees();
        for dir in [&left_dir, &right_dir] {
            write_files(
                dir.path(),
                &[
                    ("keep/a.rs", "same"),
                    // Named like a match, but the whitelist applies to files only.
                    ("x.rs/readme.md", "same"),
                    // Blacklist beats whitelist, and the folder has nothing else.
                    ("gen/b.rs", "same"),
                ],
            );
            fs::create_dir_all(dir.path().join("empty/nested")).unwrap();
        }
        // A whitelisted file on one side keeps its folder on both: the right one/ exists.
        write_files(left_dir.path(), &[("one/c.rs", "l")]);
        write_files(right_dir.path(), &[("one/c.md", "r")]);
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());

        let rows = build_filtered(
            &left,
            &right,
            &mut PairStates::default(),
            &Cell::new(0),
            &whitelist("*.rs", "gen/"),
        );

        assert_eq!(
            rows_with_kinds(&rows),
            [
                ("", "Different"),
                ("keep", "Same"),
                ("keep/a.rs", "Same"),
                ("one", "Different"),
                ("one/c.rs", "OnlyInFirst"),
            ]
        );
    }

    #[test]
    fn with_a_whitelist_folder_states_reflect_only_the_visible_files() {
        let (left_dir, right_dir) = two_folder_trees();
        for dir in [&left_dir, &right_dir] {
            write_files(
                dir.path(),
                &[("src/main.rs", "same"), ("lib/deep/x.rs", "")],
            );
        }
        write_files(left_dir.path(), &[("src/notes.md", "left")]);
        write_files(right_dir.path(), &[("src/notes.md", "right")]);
        write_files(left_dir.path(), &[("lib/deep/y.rs", "left")]);
        write_files(right_dir.path(), &[("lib/deep/y.rs", "right")]);
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let mut states = PairStates::default();
        let comparisons = Cell::new(0);

        let rows = build_counting(&left, &right, &mut states, &comparisons);
        assert_eq!(kind_at(&rows, "src"), "Different");

        let rows = build_filtered(
            &left,
            &right,
            &mut states,
            &comparisons,
            &whitelist("*.rs", ""),
        );
        assert_eq!(kind_at(&rows, "src"), "Same", "notes.md is hidden");
        assert_eq!(kind_at(&rows, "lib"), "Different");
        assert_eq!(kind_at(&rows, "lib/deep"), "Different");
        assert_eq!(kind_at(&rows, ""), "Different", "y.rs still differs");
    }

    fn kind_at(rows: &[VisibleRowTwoFolderDiff], rel_path: &str) -> &'static str {
        let row = rows.iter().find(|r| r.rel_path == rel_path).unwrap();
        state_kind(&row.diff_state)
    }

    fn single_rows(view: &FileSystemView, filter: &PathFilter) -> Vec<VisibleRowTwoFolderDiff> {
        let rows = single_side_rows(view, filter, TreeSort::default()).unwrap();
        for row in &rows {
            assert!(
                matches!(row.diff_state, DiffState::OnlyInFirst(_)),
                "{}",
                row.rel_path
            );
        }
        rows
    }

    /// Relative paths of the duplicate tree's rows, everything expanded.
    fn dup_tree_rows(root: &Path, filter: &PathFilter) -> Vec<String> {
        single_rows(&load_view(root), filter)
            .into_iter()
            .map(|row| row.rel_path)
            .collect()
    }

    /// Files and folders under a.txt, keep/, gen/ and src/, plus an empty folder.
    fn dup_tree() -> TempDir {
        let dir = tempdir().unwrap();
        write_files(
            dir.path(),
            &[
                ("a.txt", "a"),
                ("b.obj", "b"),
                ("gen/c.txt", "c"),
                ("gen/deep/d.txt", "d"),
                ("keep/e.txt", "e"),
                ("keep/f.obj", "f"),
                ("src/deep/main.rs", "m"),
                // Named like a match, but the whitelist applies to files only.
                ("x.rs/readme.md", "r"),
            ],
        );
        fs::create_dir_all(dir.path().join("empty/nested")).unwrap();
        dir
    }

    #[test]
    fn blacklisted_entries_are_absent_from_the_duplicate_tree() {
        let dir = dup_tree();

        assert_eq!(
            dup_tree_rows(dir.path(), &blacklist("*.OBJ, gen/")),
            [
                "",
                "empty",
                "empty/nested",
                "keep",
                "keep/e.txt",
                "src",
                "src/deep",
                "src/deep/main.rs",
                "x.rs",
                "x.rs/readme.md",
                "a.txt",
            ]
        );
    }

    #[test]
    fn the_duplicate_tree_keeps_whitelisted_files_and_their_ancestors_only() {
        let dir = dup_tree();

        // keep/ has no whitelisted file left once f.obj is blacklisted.
        assert_eq!(
            dup_tree_rows(dir.path(), &whitelist("*.rs, *.obj", "f.obj")),
            ["", "src", "src/deep", "src/deep/main.rs", "b.obj"]
        );
    }

    #[test]
    fn the_duplicate_trees_files_are_the_files_tree_hidden_leaves_visible() {
        let dir = dup_tree();
        let view = load_view(dir.path());
        let filters = [
            PathFilter::default(),
            blacklist("*.OBJ, gen/"),
            whitelist("*.rs, *.obj", "f.obj"),
        ];
        for filter in filters {
            let mut drawn: Vec<String> = single_rows(&view, &filter)
                .into_iter()
                .filter(|row| !row.is_dir)
                .map(|row| row.rel_path)
                .collect();
            let hidden = tree_hidden(&view, &filter);
            let mut visible: Vec<String> = view
                .file_system
                .iter_files()
                .filter(|&id| !hidden[id])
                .map(|id| get_rel(&view.file_system, dir.path(), id))
                .collect();
            drawn.sort();
            visible.sort();
            assert_eq!(drawn, visible, "{filter:?}");
        }
    }

    #[test]
    fn without_a_filter_the_duplicate_tree_shows_everything() {
        let dir = dup_tree();

        assert_eq!(
            dup_tree_rows(dir.path(), &PathFilter::default()).len(),
            1 + 8 + 8,
            "the root, 8 files and 8 folders"
        );
    }

    fn row_paths(rows: &[VisibleRowTwoFolderDiff]) -> Vec<&str> {
        rows.iter().map(|r| r.rel_path.as_str()).collect()
    }

    #[test]
    fn sorted_children_stay_under_their_parent() {
        let (left_dir, right_dir) = (tempdir().unwrap(), tempdir().unwrap());
        for root in [left_dir.path(), right_dir.path()] {
            // a.txt sorts between a and a/... by bytes ('.' < '/').
            write_files(
                root,
                &[
                    ("a/b.txt", "b"),
                    ("a/z/deep.txt", "deep"),
                    ("a.txt", "a"),
                    ("c/d.txt", "d"),
                ],
            );
        }
        let (left, right) = (load_view(left_dir.path()), load_view(right_dir.path()));
        let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));

        let rows = sort_two_folder_rows(rows, TreeSort::default());

        assert_eq!(
            row_paths(&rows),
            [
                "",
                "a",
                "a/z",
                "a/z/deep.txt",
                "a/b.txt",
                "c",
                "c/d.txt",
                "a.txt"
            ]
        );
    }

    #[test]
    fn sorted_rows_keep_both_sides_on_the_same_relative_path() {
        let (left_dir, right_dir) = two_folder_trees();
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
        let by_state = TreeSort::default().clicked(SortKey::State);

        let rows = sort_two_folder_rows(rows, by_state);

        assert_eq!(
            row_paths(&rows),
            [
                "",
                "sub",
                "sub/deep",
                "sub/deep/d.txt",
                "sub/c.txt",
                "b.txt",
                "left_only.txt",
                "right_only.txt",
                "a.txt"
            ]
        );
        for row in &rows {
            if let Some(id) = row.diff_state.first() {
                assert_eq!(
                    get_rel(&left.file_system, left_dir.path(), id),
                    row.rel_path
                );
            }
            if let Some(id) = row.diff_state.second() {
                assert_eq!(
                    get_rel(&right.file_system, right_dir.path(), id),
                    row.rel_path
                );
            }
        }
    }

    #[test]
    fn filtered_entries_are_absent_from_the_sorted_rows() {
        let (left_dir, right_dir) = two_folder_trees();
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let rows = build_filtered(
            &left,
            &right,
            &mut PairStates::default(),
            &Cell::new(0),
            &blacklist("b.txt, deep/"),
        );
        let by_state = TreeSort::default().clicked(SortKey::State);

        let rows = sort_two_folder_rows(rows, by_state);

        assert_eq!(
            row_paths(&rows),
            [
                "",
                "sub",
                "sub/c.txt",
                "left_only.txt",
                "right_only.txt",
                "a.txt"
            ]
        );
    }

    #[test]
    fn flat_rows_are_ordered_by_depth_then_name_aligned_and_filtered() {
        let (left_dir, right_dir) = two_folder_trees();
        write_files(left_dir.path(), &[("sub/deep/a.txt", "l")]);
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let rows = build_filtered(
            &left,
            &right,
            &mut PairStates::default(),
            &Cell::new(0),
            &blacklist("b.txt, c.txt"),
        );
        let flat = TreeSort {
            flat: true,
            ..TreeSort::default()
        };

        let rows = sort_two_folder_rows(rows, flat);

        assert_eq!(
            row_paths(&rows),
            [
                "",
                "a.txt",
                "left_only.txt",
                "right_only.txt",
                "sub",
                "sub/deep",
                "sub/deep/a.txt",
                "sub/deep/d.txt"
            ]
        );
        for row in &rows {
            if let Some(id) = row.diff_state.first() {
                assert_eq!(
                    get_rel(&left.file_system, left_dir.path(), id),
                    row.rel_path
                );
            }
            if let Some(id) = row.diff_state.second() {
                assert_eq!(
                    get_rel(&right.file_system, right_dir.path(), id),
                    row.rel_path
                );
            }
        }
    }

    #[test]
    fn single_side_flat_rows_are_ordered_by_depth_then_name_and_filtered() {
        let (left_dir, _right_dir) = two_folder_trees();
        write_files(left_dir.path(), &[("sub/deep/a.txt", "l")]);
        let left = load_view(left_dir.path());
        let flat = TreeSort {
            flat: true,
            ..TreeSort::default()
        };

        let rows = single_side_rows(&left, &blacklist("b.txt, c.txt"), flat).unwrap();

        assert_eq!(
            row_paths(&rows),
            [
                "",
                "a.txt",
                "left_only.txt",
                "sub",
                "sub/deep",
                "sub/deep/a.txt",
                "sub/deep/d.txt"
            ]
        );
        for row in &rows {
            assert_eq!(row.diff_state.second(), None);
            let id = row.diff_state.first().unwrap();
            assert_eq!(
                get_rel(&left.file_system, left_dir.path(), id),
                row.rel_path
            );
        }
    }

    #[test]
    fn single_side_rows_follow_the_sort_and_keep_children_under_their_parent() {
        let dir = tempdir().unwrap();
        write_files(
            dir.path(),
            &[
                ("a/b.txt", "b"),
                ("a/z/deep.txt", "deep"),
                ("a.txt", "a"),
                ("c/d.txt", "d"),
            ],
        );
        let view = load_view(dir.path());
        let descending = TreeSort::default().clicked(SortKey::Name);

        let rows = single_side_rows(&view, &PathFilter::default(), descending).unwrap();

        assert_eq!(
            row_paths(&rows),
            [
                "",
                "c",
                "c/d.txt",
                "a",
                "a/z",
                "a/z/deep.txt",
                "a/b.txt",
                "a.txt"
            ]
        );
    }

    #[test]
    fn a_side_is_as_wide_as_its_indentation_expander_and_name() {
        let spacing = 8.0;
        assert_eq!(
            side_width(RowLayout::Tree, 2, true, 50.0, spacing),
            2.0 * TREE_INDENT + EXPANDER_WIDTH + spacing + 50.0
        );
        assert_eq!(
            side_width(RowLayout::Tree, 2, false, 50.0, spacing),
            2.0 * TREE_INDENT + 50.0
        );
        assert_eq!(
            side_width(RowLayout::RelPaths, 2, true, 50.0, spacing),
            50.0
        );
        assert_eq!(
            side_width(RowLayout::FileList, 2, false, 50.0, spacing),
            50.0
        );
    }

    #[test]
    fn two_folder_rows_carry_their_relative_path_with_slash_separators() {
        let (left_dir, right_dir) = two_folder_trees();
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());

        let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));

        let paths: Vec<&str> = rows.iter().map(|r| r.rel_path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "",
                "a.txt",
                "b.txt",
                "left_only.txt",
                "right_only.txt",
                "sub",
                "sub/c.txt",
                "sub/deep",
                "sub/deep/d.txt",
            ]
        );
    }

    #[test]
    fn rows_carry_each_sides_file_kind_from_the_table_and_none_for_folders() {
        let left_dir = tempdir().unwrap();
        let right_dir = tempdir().unwrap();
        let shared = [
            ("a.txt", "a"),
            ("p.PNG", "p"),
            ("x.uasset", "x"),
            ("noext", "n"),
            ("w.qqq", "w"),
            ("y.zzz", "y"),
            ("sub/c.txt", "c"),
        ];
        write_files(left_dir.path(), &shared);
        write_files(right_dir.path(), &shared);
        write_files(
            left_dir.path(),
            &[("left_only.rs", "l"), ("mixed/inner.txt", "i")],
        );
        write_files(right_dir.path(), &[("mixed", "file on the right")]);
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let mut kinds = KindTable::default();
        kinds.set_override("zzz", FileKind::Model);

        let rows = FileSystemView::build_two_folder_diff_rows(
            Some(&left),
            Some(&right),
            &PathFilter::default(),
            &kinds,
            &Tolerance::default(),
            |_, _| (None, None),
            |_, _| None,
        )
        .unwrap();

        let actual: Vec<(&str, [Option<FileKind>; 2])> = rows
            .iter()
            .map(|r| (r.rel_path.as_str(), r.file_kinds))
            .collect();
        let both = |kind| [Some(kind), Some(kind)];
        assert_eq!(
            actual,
            [
                ("", [None, None]),
                ("a.txt", both(FileKind::Text)),
                ("left_only.rs", [Some(FileKind::Text), None]),
                ("mixed", [None, Some(FileKind::Binary)]),
                ("mixed/inner.txt", [Some(FileKind::Text), None]),
                ("noext", both(FileKind::Binary)),
                ("p.PNG", both(FileKind::Image)),
                ("sub", [None, None]),
                ("sub/c.txt", both(FileKind::Text)),
                ("w.qqq", both(FileKind::Binary)),
                ("x.uasset", both(FileKind::UnrealAsset)),
                ("y.zzz", both(FileKind::Model)),
            ]
        );
    }

    fn blueprint_trees() -> (TempDir, TempDir) {
        let left = tempdir().unwrap();
        let right = tempdir().unwrap();
        write_files(
            left.path(),
            &[
                ("a.uasset", "a"),
                ("b.txt", "b"),
                ("sub/c.uasset", "c"),
                ("left_only.uasset", "l"),
            ],
        );
        write_files(
            right.path(),
            &[("a.uasset", "a2"), ("b.txt", "b"), ("sub/c.uasset", "c")],
        );
        (left, right)
    }

    fn blueprint_columns(rows: &[VisibleRowTwoFolderDiff]) -> Vec<(&str, [bool; 2])> {
        rows.iter()
            .map(|r| (r.rel_path.as_str(), r.blueprints))
            .collect()
    }

    #[test]
    fn rows_check_each_sides_uasset_files_and_carry_known_blueprints() {
        let (left_dir, right_dir) = blueprint_trees();
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let known_blueprint = left_dir.path().join("a.uasset");
        let mut checked = Vec::new();

        let rows = FileSystemView::build_two_folder_diff_rows(
            Some(&left),
            Some(&right),
            &PathFilter::default(),
            &KindTable::default(),
            &Tolerance::default(),
            |_, _| (None, None),
            |path, meta| {
                checked.push((path.to_path_buf(), meta.size));
                (path == known_blueprint).then_some(true)
            },
        )
        .unwrap();

        checked.sort();
        let mut expected = vec![
            (left_dir.path().join("a.uasset"), 1),
            (left_dir.path().join("left_only.uasset"), 1),
            (left_dir.path().join("sub").join("c.uasset"), 1),
            (right_dir.path().join("a.uasset"), 2),
            (right_dir.path().join("sub").join("c.uasset"), 1),
        ];
        expected.sort();
        assert_eq!(checked, expected);
        assert_eq!(
            blueprint_columns(&rows),
            [
                ("", [false, false]),
                ("a.uasset", [true, false]),
                ("b.txt", [false, false]),
                ("left_only.uasset", [false, false]),
                ("sub", [false, false]),
                ("sub/c.uasset", [false, false]),
            ]
        );
    }

    #[test]
    fn blueprint_results_patch_the_matching_side_of_the_rows() {
        let (left_dir, right_dir) = blueprint_trees();
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let mut rows = FileSystemView::build_two_folder_diff_rows(
            Some(&left),
            Some(&right),
            &PathFilter::default(),
            &KindTable::default(),
            &Tolerance::default(),
            |_, _| (None, None),
            |_, _| None,
        )
        .unwrap();
        let roots = [Some(left_dir.path()), Some(right_dir.path())];
        let elsewhere = tempdir().unwrap();

        apply_blueprint_results(
            &mut rows,
            roots,
            &[
                (left_dir.path().join("a.uasset"), true),
                (right_dir.path().join("sub").join("c.uasset"), true),
                (left_dir.path().join("b.txt"), true),
                (left_dir.path().join("sub"), true),
                (elsewhere.path().join("a.uasset"), true),
            ],
        );
        assert_eq!(
            blueprint_columns(&rows),
            [
                ("", [false, false]),
                ("a.uasset", [true, false]),
                ("b.txt", [false, false]),
                ("left_only.uasset", [false, false]),
                ("sub", [false, false]),
                ("sub/c.uasset", [false, true]),
            ]
        );

        apply_blueprint_results(
            &mut rows,
            [None, Some(right_dir.path())],
            &[
                (left_dir.path().join("left_only.uasset"), true),
                (right_dir.path().join("sub").join("c.uasset"), false),
            ],
        );
        assert_eq!(rows[3].blueprints, [false, false]);
        assert_eq!(rows[5].blueprints, [false, false]);
    }

    #[test]
    fn rebuilding_two_folder_rows_compares_no_files_and_builds_identical_rows() {
        let (left_dir, right_dir) = two_folder_trees();
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let comparisons = Cell::new(0);
        let mut states = PairStates::default();

        let first = build_counting(&left, &right, &mut states, &comparisons);
        // a.txt, b.txt, sub/c.txt and sub/deep/d.txt are on both sides.
        assert_eq!(comparisons.get(), 4);

        let second = build_counting(&left, &right, &mut states, &comparisons);
        assert_eq!(comparisons.get(), 4);
        assert_eq!(second, first);
    }

    #[test]
    fn rows_inside_a_collapsed_folder_and_the_root_row_are_hidden_from_the_cursor() {
        let (left_dir, right_dir) = two_folder_trees();
        write_files(left_dir.path(), &[("sub/left_only_in_sub.txt", "l")]);
        let mut left = load_view(left_dir.path());
        let mut right = load_view(right_dir.path());
        let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));

        // As a click on the row does: both sides.
        let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
        left.toggle_collapse(sub.diff_state.first().unwrap());
        right.toggle_collapse(sub.diff_state.second().unwrap());

        let hidden: Vec<String> = two_folder_cursor_rows(Some(&left), Some(&right), &rows, false)
            .into_iter()
            .filter(|r| r.hidden)
            .map(|r| r.rel_path)
            .collect();
        // sub/deep is expanded, but its parent isn't.
        assert_eq!(
            hidden,
            [
                "",
                "sub/c.txt",
                "sub/deep",
                "sub/deep/d.txt",
                "sub/left_only_in_sub.txt",
            ]
        );
    }

    #[test]
    fn cursor_rows_are_rebuilt_when_the_rows_collapse_state_or_flat_mode_change_and_not_otherwise()
    {
        let (left_dir, right_dir) = two_folder_trees();
        let mut left = load_view(left_dir.path());
        let mut right = load_view(right_dir.path());
        let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
        let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
        let (sub_left, sub_right) = (sub.diff_state.first(), sub.diff_state.second());
        let mut cache = CursorRowsCache::default();
        let mut builds = |left: &FileSystemView, right: &FileSystemView, id, flat| {
            cache.update([Some(left), Some(right)], &rows, id, flat);
            cache.builds
        };
        let id = RowsId::next();

        assert_eq!(builds(&left, &right, id, false), 1);
        assert_eq!(builds(&left, &right, id, false), 1);

        left.toggle_collapse(sub_left.unwrap());
        assert_eq!(builds(&left, &right, id, false), 2);
        assert_eq!(builds(&left, &right, id, false), 2);

        right.collapsed.insert(sub_right.unwrap(), true);
        assert_eq!(builds(&left, &right, id, false), 3);

        let id = RowsId::next();
        assert_eq!(builds(&left, &right, id, false), 4);
        assert_eq!(builds(&left, &right, id, false), 4);

        assert_eq!(builds(&left, &right, id, true), 5);
        assert_eq!(builds(&left, &right, id, true), 5);
    }

    #[test]
    fn the_drawn_rows_are_the_rows_the_cursor_sees_and_reuse_one_buffer() {
        let (left_dir, right_dir) = two_folder_trees();
        let mut left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
        let id = RowsId::next();
        let mut cache = CursorRowsCache::default();
        let drawn_paths = |cache: &CursorRowsCache| -> Vec<String> {
            let cursor_rows = cache.cursor_rows();
            cache
                .drawn()
                .iter()
                .map(|&i| cursor_rows[i].rel_path.clone())
                .collect()
        };

        cache.update([Some(&left), Some(&right)], &rows, id, false);
        let buffer = cache.drawn().as_ptr();
        assert_eq!(drawn_paths(&cache).len(), rows.len() - 1);
        cache.update([Some(&left), Some(&right)], &rows, id, false);
        assert_eq!(cache.drawn().as_ptr(), buffer);

        let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
        left.toggle_collapse(sub.diff_state.first().unwrap());
        let cursor_rows = cache.update([Some(&left), Some(&right)], &rows, id, false);
        assert_eq!(
            cursor_rows,
            two_folder_cursor_rows(Some(&left), Some(&right), &rows, false)
        );
        assert_eq!(cache.drawn().as_ptr(), buffer);
        assert_eq!(
            drawn_paths(&cache),
            ["a.txt", "b.txt", "left_only.txt", "right_only.txt", "sub"]
        );

        let cursor_rows = cache.update([Some(&left), Some(&right)], &rows, id, true);
        assert_eq!(
            cursor_rows,
            two_folder_cursor_rows(Some(&left), Some(&right), &rows, true)
        );
    }

    #[test]
    fn in_flat_mode_only_the_root_row_is_hidden_and_no_folder_is_collapsed() {
        let (left_dir, right_dir) = two_folder_trees();
        let mut left = load_view(left_dir.path());
        let mut right = load_view(right_dir.path());
        let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
        let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
        left.toggle_collapse(sub.diff_state.first().unwrap());
        right.toggle_collapse(sub.diff_state.second().unwrap());

        let cursor_rows = two_folder_cursor_rows(Some(&left), Some(&right), &rows, true);

        let hidden: Vec<&str> = cursor_rows
            .iter()
            .filter(|r| r.hidden)
            .map(|r| r.rel_path.as_str())
            .collect();
        assert_eq!(hidden, [""]);
        assert!(cursor_rows.iter().all(|r| !r.collapsed));
    }

    #[test]
    fn expand_and_collapse_requests_set_both_sides_even_when_they_disagree() {
        let (left_dir, right_dir) = two_folder_trees();
        let mut left = load_view(left_dir.path());
        let mut right = load_view(right_dir.path());
        let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
        let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
        let (left_sub, right_sub) = (
            sub.diff_state.first().unwrap(),
            sub.diff_state.second().unwrap(),
        );
        let cursor_sees_collapsed = |left: &FileSystemView, right: &FileSystemView| {
            two_folder_cursor_rows(Some(left), Some(right), &rows, false)
                .iter()
                .find(|r| r.rel_path == "sub")
                .unwrap()
                .collapsed
        };
        let config = DiffToolConfig::default();

        left.toggle_collapse(left_sub);
        assert!(cursor_sees_collapsed(&left, &right), "left side only");

        let expand = CursorRequest::Expand("sub".to_string());
        apply_cursor_request(
            Some(&mut left),
            Some(&mut right),
            &rows,
            &expand,
            &config,
            &mut None,
        );
        assert!(!left.is_collapsed(left_sub) && !right.is_collapsed(right_sub));
        assert!(!cursor_sees_collapsed(&left, &right));

        right.toggle_collapse(right_sub);
        assert!(cursor_sees_collapsed(&left, &right), "right side only");

        let collapse = CursorRequest::Collapse("sub".to_string());
        apply_cursor_request(
            Some(&mut left),
            Some(&mut right),
            &rows,
            &collapse,
            &config,
            &mut None,
        );
        assert!(left.is_collapsed(left_sub) && right.is_collapsed(right_sub));
    }

    #[test]
    fn in_single_side_mode_a_collapsed_folder_hides_its_rows_and_the_root_from_the_cursor() {
        let (left_dir, _right_dir) = two_folder_trees();
        let mut left = load_view(left_dir.path());
        let rows = single_rows(&left, &PathFilter::default());
        let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
        left.toggle_collapse(sub.diff_state.first().unwrap());

        let cursor_rows = two_folder_cursor_rows(Some(&left), None, &rows, false);

        let hidden: Vec<&str> = cursor_rows
            .iter()
            .filter(|r| r.hidden)
            .map(|r| r.rel_path.as_str())
            .collect();
        assert_eq!(hidden, ["", "sub/deep", "sub/deep/d.txt", "sub/c.txt"]);
        let collapsed: Vec<&str> = cursor_rows
            .iter()
            .filter(|r| r.collapsed)
            .map(|r| r.rel_path.as_str())
            .collect();
        assert_eq!(collapsed, ["sub"]);
    }

    #[test]
    fn in_single_side_flat_mode_only_the_root_row_is_hidden_and_no_folder_is_collapsed() {
        let (left_dir, _right_dir) = two_folder_trees();
        let mut left = load_view(left_dir.path());
        let rows = single_rows(&left, &PathFilter::default());
        let sub = rows.iter().find(|r| r.rel_path == "sub").unwrap();
        left.toggle_collapse(sub.diff_state.first().unwrap());

        let cursor_rows = two_folder_cursor_rows(Some(&left), None, &rows, true);

        let hidden: Vec<&str> = cursor_rows
            .iter()
            .filter(|r| r.hidden)
            .map(|r| r.rel_path.as_str())
            .collect();
        assert_eq!(hidden, [""]);
        assert!(cursor_rows.iter().all(|r| !r.collapsed));
    }

    #[test]
    fn in_single_side_mode_the_cursor_walks_the_drawn_rows_and_folds_folder_1() {
        let (left_dir, _right_dir) = two_folder_trees();
        let mut left = load_view(left_dir.path());
        let rows = single_rows(&left, &PathFilter::default());
        let sub_id = rows
            .iter()
            .find(|r| r.rel_path == "sub")
            .unwrap()
            .diff_state
            .first()
            .unwrap();
        let config = DiffToolConfig::default();
        let cursor = TreeCursor::at("sub");

        let (_, collapse) = cursor.left(&two_folder_cursor_rows(Some(&left), None, &rows, false));
        apply_cursor_request(
            Some(&mut left),
            None,
            &rows,
            &collapse.unwrap(),
            &config,
            &mut None,
        );
        assert!(left.is_collapsed(sub_id));

        let cursor_rows = two_folder_cursor_rows(Some(&left), None, &rows, false);
        assert_eq!(cursor.down(&cursor_rows), TreeCursor::at("a.txt"));

        let expand = cursor.right(&cursor_rows).unwrap();
        apply_cursor_request(Some(&mut left), None, &rows, &expand, &config, &mut None);
        assert!(!left.is_collapsed(sub_id));
        let cursor_rows = two_folder_cursor_rows(Some(&left), None, &rows, false);
        assert_eq!(cursor.down(&cursor_rows), TreeCursor::at("sub/deep"));
    }

    #[test]
    fn opening_a_file_in_single_side_mode_launches_nothing() {
        let (left_dir, _right_dir) = two_folder_trees();
        let mut left = load_view(left_dir.path());
        let rows = single_rows(&left, &PathFilter::default());
        let mut message = Some(ToolMessage {
            warning: Some("stale".to_string()),
            error: None,
        });

        apply_cursor_request(
            Some(&mut left),
            None,
            &rows,
            &CursorRequest::Open("b.txt".to_string()),
            &missing_tool_config(vec![FileKind::Text], true),
            &mut message,
        );

        assert_eq!(message, None);
    }

    fn missing_tool_config(kinds: Vec<FileKind>, mark_undiffable: bool) -> DiffToolConfig {
        DiffToolConfig {
            tools: vec![DiffTool {
                name: "Missing".to_string(),
                exe_path: "zhashdiff-test-missing-tool.exe".into(),
                template: "{left} {right}".to_string(),
                kinds,
            }],
            rules: Vec::new(),
            default_tool: "Missing".to_string(),
            mark_undiffable,
        }
    }

    fn open_b_txt(config: &DiffToolConfig) -> Option<ToolMessage> {
        let (left_dir, right_dir) = two_folder_trees();
        let mut left = load_view(left_dir.path());
        let mut right = load_view(right_dir.path());
        let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));
        let mut message = Some(ToolMessage {
            warning: Some("stale".to_string()),
            error: None,
        });
        let open = CursorRequest::Open("b.txt".to_string());
        apply_cursor_request(
            Some(&mut left),
            Some(&mut right),
            &rows,
            &open,
            config,
            &mut message,
        );
        message
    }

    #[test]
    fn a_tool_that_is_not_found_reports_the_places_searched() {
        let message = open_b_txt(&missing_tool_config(vec![FileKind::Text], true)).unwrap();
        assert_eq!(message.warning, None);
        let error = message.error.unwrap();
        assert!(
            error.contains("zhashdiff-test-missing-tool.exe not found, searched: "),
            "{error}"
        );
    }

    #[test]
    fn a_missing_default_tool_is_reported() {
        let config = DiffToolConfig {
            default_tool: "Nothing".to_string(),
            ..missing_tool_config(vec![FileKind::Text], false)
        };
        let message = open_b_txt(&config).unwrap();
        assert_eq!(message.warning, None);
        assert!(message.error.unwrap().contains("\"Nothing\" is missing"));
    }

    #[test]
    fn opening_a_marked_row_warns_with_the_tool_and_the_kind() {
        let message = open_b_txt(&missing_tool_config(vec![FileKind::Image], true)).unwrap();
        assert_eq!(
            message.warning.as_deref(),
            Some("b.txt: opening anyway, Missing can't diff Text files")
        );
        assert!(message.error.is_some());

        let unmarked = open_b_txt(&missing_tool_config(vec![FileKind::Image], false)).unwrap();
        assert_eq!(unmarked.warning, None);
    }

    #[test]
    fn cursor_rows_mark_every_entry_that_is_not_same_as_differing() {
        let (left_dir, right_dir) = two_folder_trees();
        let left = load_view(left_dir.path());
        let right = load_view(right_dir.path());
        let rows = build_counting(&left, &right, &mut PairStates::default(), &Cell::new(0));

        let differing: Vec<String> =
            two_folder_cursor_rows(Some(&left), Some(&right), &rows, false)
                .into_iter()
                .filter(|r| r.differs)
                .map(|r| r.rel_path)
                .collect();
        assert_eq!(differing, ["", "b.txt", "left_only.txt", "right_only.txt"]);
    }

    #[test]
    fn cursor_keys_keep_arrow_repeats_and_press_order_but_drop_enter_repeats() {
        use eframe::egui::{Event, InputState, Key, Modifiers};
        let press = |key, repeat| Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers: Modifiers::NONE,
        };
        let mut input = InputState::default();
        input.events = vec![
            press(Key::ArrowDown, false),
            press(Key::ArrowDown, true),
            press(Key::Enter, false),
            // Held Enter would open the diff tool once per repeat.
            press(Key::Enter, true),
            press(Key::ArrowLeft, false),
            press(Key::ArrowUp, true),
        ];

        assert_eq!(
            cursor_keys(&input),
            [
                Key::ArrowDown,
                Key::ArrowDown,
                Key::Enter,
                Key::ArrowLeft,
                Key::ArrowUp
            ]
        );
    }

    #[test]
    fn test_build_collapsed_rows_scenarios_single_view() {
        let cases = vec![
            CollapsedTestCase {
                name: "Simple collapsed directory",
                structure: vec![("a", true), ("a/file.txt", false)],
                collapsed: vec!["a"],
                expected: vec![("a", 1)],
            },
            CollapsedTestCase {
                name: "Fully expanded directory",
                structure: vec![
                    ("dir_a", true),
                    ("dir_a/file_1.txt", false),
                    ("dir_b", true),
                ],
                collapsed: vec![],
                expected: vec![("dir_a", 1), ("dir_a/file_1.txt", 2), ("dir_b", 1)],
            },
            CollapsedTestCase {
                name: "Deep nesting with partial collapse",
                structure: vec![
                    ("level1", true),
                    ("level1/level2", true),
                    ("level1/level2/derp.txt", false),
                    ("level1/level2/level3", true),
                    ("level1/level2/level3/file.txt", false),
                ],
                collapsed: vec!["level1/level2"],
                expected: vec![("level1", 1), ("level1/level2", 2)],
            },
        ];

        for case in cases {
            let temp = tempdir().unwrap();
            let root_path = temp.path();

            for (rel_path, is_dir) in &case.structure {
                let full_path = root_path.join(rel_path);
                if *is_dir {
                    fs::create_dir_all(&full_path).unwrap();
                } else {
                    if let Some(parent) = full_path.parent() {
                        fs::create_dir_all(parent).unwrap();
                    }
                    File::create(&full_path).unwrap();
                }
            }

            let mut view = load_view(root_path);

            for path_str in &case.collapsed {
                let id = find_id_by_rel_path(&view.file_system, root_path, path_str);
                view.collapsed.insert(id, true);
            }

            let rows = single_rows(&view, &PathFilter::default());
            let cursor_rows = two_folder_cursor_rows(Some(&view), None, &rows, false);

            let actual: Vec<(String, FsNodeDepth)> = rows
                .iter()
                .zip(&cursor_rows)
                .filter(|(_, cursor_row)| !cursor_row.hidden)
                .map(|(row, _)| (row.rel_path.clone(), row.depth))
                .collect();

            let expected_mapped: Vec<(String, FsNodeDepth)> = case
                .expected
                .iter()
                .map(|(p, d)| (p.to_string(), *d))
                .collect();

            if actual != expected_mapped {
                panic!(
                    "\nTest Case Failed: {}\n\nEXPECTED TREE:\n{}\n\nACTUAL TREE:\n{}\n",
                    case.name,
                    format_tree(&expected_mapped),
                    format_tree(&actual)
                );
            }
        }
    }

    #[test]
    fn a_one_sided_child_makes_the_folder_different() {
        for child in [DiffState::OnlyInFirst(3), DiffState::OnlyInSecond(3)] {
            assert_eq!(
                aggregate_folder_state(1, 2, [DiffState::Same(3, 3), child]),
                DiffState::Different(1, 2)
            );
        }
    }

    #[test]
    fn a_folder_whose_children_are_all_same_is_same() {
        assert_eq!(
            aggregate_folder_state(1, 2, [DiffState::Same(3, 3), DiffState::Same(4, 4)]),
            DiffState::Same(1, 2)
        );
        assert_eq!(aggregate_folder_state(1, 2, []), DiffState::Same(1, 2));
    }

    #[test]
    fn a_pending_child_makes_an_otherwise_same_folder_pending() {
        assert_eq!(
            aggregate_folder_state(1, 2, [DiffState::Same(3, 3), DiffState::Pending(4, 4)]),
            DiffState::Pending(1, 2)
        );
        let sub = aggregate_folder_state(5, 6, [DiffState::Pending(7, 7)]);
        assert_eq!(
            aggregate_folder_state(1, 2, [DiffState::Same(3, 3), sub]),
            DiffState::Pending(1, 2)
        );
    }

    #[test]
    fn a_folder_settles_different_while_siblings_are_still_pending() {
        assert_eq!(
            aggregate_folder_state(
                1,
                2,
                [
                    DiffState::Pending(3, 3),
                    DiffState::Different(4, 4),
                    DiffState::Pending(5, 5),
                ]
            ),
            DiffState::Different(1, 2)
        );
    }

    #[test]
    fn a_similar_child_counts_as_equal_but_marks_the_folder_similar() {
        assert_eq!(
            aggregate_folder_state(1, 2, [DiffState::Similar(3, 3), DiffState::Same(4, 4)]),
            DiffState::Similar(1, 2)
        );
        assert_eq!(
            aggregate_folder_state(1, 2, [DiffState::Similar(3, 3), DiffState::Pending(4, 4)]),
            DiffState::Pending(1, 2)
        );
        assert_eq!(
            aggregate_folder_state(1, 2, [DiffState::Similar(3, 3), DiffState::Different(4, 4)]),
            DiffState::Different(1, 2)
        );
    }

    #[test]
    fn next_and_prev_diff_skip_similar_files() {
        let row = |rel_path: &str, diff_state| VisibleRowTwoFolderDiff {
            rel_path: rel_path.to_string(),
            is_dir: rel_path.is_empty(),
            depth: 0,
            diff_state,
            file_kinds: [None, None],
            blueprints: [false, false],
        };
        let rows = [
            row("", DiffState::Different(0, 0)),
            row("a.txt", DiffState::Different(1, 1)),
            row("b.txt", DiffState::Similar(2, 2)),
            row("c.txt", DiffState::Different(3, 3)),
            row("d.txt", DiffState::Similar(4, 4)),
        ];
        let cursor_rows = two_folder_cursor_rows(None, None, &rows, false);

        let next = TreeCursor::at("a.txt").next_diff(&cursor_rows);
        assert_eq!(next.map(|(c, _)| c), Some(TreeCursor::at("c.txt")));
        let prev = TreeCursor::at("c.txt").prev_diff(&cursor_rows);
        assert_eq!(prev.map(|(c, _)| c), Some(TreeCursor::at("a.txt")));
        let wrapped = TreeCursor::at("c.txt").next_diff(&cursor_rows);
        assert_ne!(wrapped.map(|(c, _)| c), Some(TreeCursor::at("d.txt")));
    }

    #[test]
    fn next_and_prev_diff_skip_pending_files() {
        let row = |rel_path: &str, is_dir, diff_state| VisibleRowTwoFolderDiff {
            rel_path: rel_path.to_string(),
            is_dir,
            depth: 0,
            diff_state,
            file_kinds: [None, None],
            blueprints: [false, false],
        };
        let rows = [
            row("", true, DiffState::Different(0, 0)),
            row("a.txt", false, DiffState::Same(1, 1)),
            row("b.txt", false, DiffState::Pending(2, 2)),
            row("c.txt", false, DiffState::Different(3, 3)),
            row("d.txt", false, DiffState::Pending(4, 4)),
            row("e.txt", false, DiffState::Same(5, 5)),
        ];
        let cursor_rows = two_folder_cursor_rows(None, None, &rows, false);

        let next = TreeCursor::at("a.txt").next_diff(&cursor_rows);
        assert_eq!(next.map(|(c, _)| c), Some(TreeCursor::at("c.txt")));
        let prev = TreeCursor::at("e.txt").prev_diff(&cursor_rows);
        assert_eq!(prev.map(|(c, _)| c), Some(TreeCursor::at("c.txt")));

        let pending_only = [
            row("", true, DiffState::Pending(0, 0)),
            row("b.txt", false, DiffState::Pending(2, 2)),
        ];
        let cursor_rows = two_folder_cursor_rows(None, None, &pending_only, false);
        assert_eq!(TreeCursor::default().next_diff(&cursor_rows), None);
        assert_eq!(TreeCursor::default().prev_diff(&cursor_rows), None);
    }

    fn walk_diffs(
        left: &mut FileSystemView,
        right: &mut FileSystemView,
        rows: &[VisibleRowTwoFolderDiff],
        flat: bool,
        next: bool,
    ) -> Vec<String> {
        let config = DiffToolConfig::default();
        let mut cursor = TreeCursor::default();
        let mut visited = Vec::new();
        loop {
            let cursor_rows = two_folder_cursor_rows(Some(left), Some(right), rows, flat);
            let target = if next {
                cursor.next_diff(&cursor_rows)
            } else {
                cursor.prev_diff(&cursor_rows)
            };
            let Some((moved, expand)) = target else {
                return visited;
            };
            for request in &expand {
                apply_cursor_request(
                    Some(&mut *left),
                    Some(&mut *right),
                    rows,
                    request,
                    &config,
                    &mut None,
                );
            }
            let rel_path = moved.rel_path().unwrap().to_owned();
            let drawn = two_folder_cursor_rows(Some(left), Some(right), rows, flat)
                .iter()
                .any(|r| r.rel_path == rel_path && !r.hidden);
            assert!(drawn, "{rel_path} is not drawn");
            visited.push(rel_path);
            cursor = moved;
        }
    }

    #[test]
    fn next_and_prev_diff_reach_every_differing_file_in_every_option_combination() {
        let (left_dir, right_dir) = two_folder_trees();
        write_files(left_dir.path(), &[("sub/deep/e.txt", "left e")]);
        write_files(right_dir.path(), &[("sub/deep/e.txt", "right e")]);

        let tree_order = ["sub/deep/e.txt", "b.txt", "left_only.txt", "right_only.txt"];
        let flat_order = ["b.txt", "left_only.txt", "right_only.txt", "sub/deep/e.txt"];
        let list_order = ["b.txt", "sub/deep/e.txt", "left_only.txt", "right_only.txt"];

        for diff_only in [false, true] {
            for show_parents in [true, false] {
                for flat in [false, true] {
                    let filter = DiffFilter {
                        diff_only,
                        show_parents,
                    };
                    let sort = TreeSort {
                        flat,
                        ..TreeSort::default()
                    };
                    let expected = if filter.lists_files() {
                        list_order
                    } else if flat {
                        flat_order
                    } else {
                        tree_order
                    };
                    let combination = format!("{filter:?} flat {flat}");

                    for next in [true, false] {
                        let mut left = load_view(left_dir.path());
                        let mut right = load_view(right_dir.path());
                        let rows = arrange_two_folder_rows(
                            build_counting(
                                &left,
                                &right,
                                &mut PairStates::default(),
                                &Cell::new(0),
                            ),
                            filter,
                            sort,
                        );
                        let left_sub =
                            find_id_by_rel_path(&left.file_system, left_dir.path(), "sub");
                        let right_sub =
                            find_id_by_rel_path(&right.file_system, right_dir.path(), "sub");
                        left.toggle_collapse(left_sub);
                        right.toggle_collapse(right_sub);

                        let mut visited =
                            walk_diffs(&mut left, &mut right, &rows, filter.is_flat(sort), next);
                        if !next {
                            visited.reverse();
                        }
                        assert_eq!(visited, expected, "{combination} next {next}");
                    }
                }
            }
        }
    }
    #[test]
    fn path_bar_parent_segment_reroots_only_that_side() {
        let dir = tempdir().unwrap();
        let parent = dir.path().join("a");
        let root = parent.join("b");
        fs::create_dir_all(&root).unwrap();
        let mut root_1 = Some(root.clone());
        let mut root_2 = Some(root.clone());
        let parent_index = split_path(&root.to_string_lossy()).len() - 2;

        apply_path_bar(
            [&mut root_1, &mut root_2],
            Side::Folder1,
            PathBarAction::Segment(parent_index),
        )
        .unwrap();

        assert_eq!(root_1, Some(parent));
        assert_eq!(root_2, Some(root));
    }

    #[test]
    fn path_bar_segment_on_folder_2_reroots_folder_2() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("a");
        fs::create_dir_all(&root).unwrap();
        let mut root_1 = Some(root.clone());
        let mut root_2 = Some(root.clone());
        let parent_index = split_path(&root.to_string_lossy()).len() - 2;

        apply_path_bar(
            [&mut root_1, &mut root_2],
            Side::Folder2,
            PathBarAction::Segment(parent_index),
        )
        .unwrap();

        assert_eq!(root_1, Some(root));
        assert_eq!(root_2, Some(dir.path().to_path_buf()));
    }

    #[test]
    fn path_bar_committed_folder_sets_root() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        let mut root_1 = None;
        let mut root_2 = Some(dir.path().to_path_buf());

        apply_path_bar(
            [&mut root_1, &mut root_2],
            Side::Folder2,
            PathBarAction::Commit(target.to_string_lossy().into_owned()),
        )
        .unwrap();

        assert_eq!(root_1, None);
        assert_eq!(root_2, Some(target));
    }

    #[test]
    fn path_bar_committed_quoted_path_with_spaces_sets_root() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("with space");
        fs::create_dir(&target).unwrap();
        let mut root_1 = None;
        let mut root_2 = None;

        apply_path_bar(
            [&mut root_1, &mut root_2],
            Side::Folder1,
            PathBarAction::Commit(format!(" \"{}\" ", target.display())),
        )
        .unwrap();

        assert_eq!(root_1, Some(target));
    }

    #[test]
    fn path_bar_committed_missing_path_keeps_root() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("missing");
        let mut root_1 = Some(dir.path().to_path_buf());
        let mut root_2 = None;

        let result = apply_path_bar(
            [&mut root_1, &mut root_2],
            Side::Folder1,
            PathBarAction::Commit(missing.to_string_lossy().into_owned()),
        );

        assert_eq!(result, Err(InvalidRoot::Missing(missing)));
        assert_eq!(root_1, Some(dir.path().to_path_buf()));
        assert_eq!(root_2, None);
    }

    #[test]
    fn path_bar_committed_file_keeps_root() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("file.txt");
        File::create(&file).unwrap();
        let mut root_1 = None;
        let mut root_2 = Some(dir.path().to_path_buf());

        let result = apply_path_bar(
            [&mut root_1, &mut root_2],
            Side::Folder2,
            PathBarAction::Commit(file.to_string_lossy().into_owned()),
        );

        assert_eq!(result, Err(InvalidRoot::NotAFolder(file)));
        assert_eq!(root_2, Some(dir.path().to_path_buf()));
    }
}
