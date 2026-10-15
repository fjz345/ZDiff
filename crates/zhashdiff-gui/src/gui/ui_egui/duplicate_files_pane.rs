use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Weak;
use std::time::{Duration, Instant};

use eframe::egui;
use serde::Deserialize;
use serde::Serialize;
use zcommon::hash::HashRepresentation;
use zcommon::hash::HashService;
use zcommon_ui::common::show_custom_popup;
use zcommon_ui::style_window::role_color;
use zcommon_ui::toggle::toggle_button;
use zhashdiff::conflict::DuplicateGroup;
use zhashdiff::conflict::ResolutionSummary;
use zhashdiff::conflict::SizedFile;
use zhashdiff::conflict::execute_resolution;
use zhashdiff::conflict::group_by_size_and_hash;
use zhashdiff::conflict::plan_resolution;
use zhashdiff::conflict::recycle;
use zhashdiff::conflict::size_collisions;
use zhashdiff::external_diff_tool::DiffToolConfig;
use zhashdiff::filter::PathFilter;
use zhashdiff::fs::FileSystemModel;
use zhashdiff::fs::FsNodeId;
use zhashdiff::vcs_source::ContentSource;

use crate::ui_egui::folder_keymap::FolderAction;
use crate::ui_egui::fs_tree::CursorRowsCache;
use crate::ui_egui::fs_tree::FileSystemView;
use crate::ui_egui::fs_tree::FolderRole;
use crate::ui_egui::fs_tree::NodeColumn;
use crate::ui_egui::fs_tree::RowsId;
use crate::ui_egui::fs_tree::TreeMode;
use crate::ui_egui::fs_tree::VisibleRowTwoFolderDiff;
use crate::ui_egui::fs_tree::draw_ui_folder_tree;
use crate::ui_egui::fs_tree::single_side_rows;
use crate::ui_egui::fs_tree::tree_hidden;
use crate::ui_egui::fs_tree::two_folder_cursor_rows;
use crate::ui_egui::panes::PathDiffView;
use crate::ui_egui::panes::ZAppPane;
use crate::ui_egui::roots::Side;
use crate::ui_egui::roots::set_root;
use crate::ui_egui::status_line::ToolMessage;
use crate::ui_egui::tree_cursor::CursorRequest;
use crate::ui_egui::tree_cursor::TreeCursor;
use crate::ui_egui::tree_filter::DuplicatesFilter;
use crate::ui_egui::tree_filter::filter_rows;
use crate::ui_egui::tree_sort::TreeSort;
use crate::ui_egui::tree_sort::sort_file_list;
use zcommon_ui::common::hash_to_color;

const MAX_CONCURRENT_HASHES: usize = 16;

const GROUP_COLUMN_WIDTH: f32 = 80.0;
const HASH_COLUMN_WIDTH: f32 = 140.0;
const KEEPER_COLOR: egui::Color32 = egui::Color32::LIGHT_GREEN;
const DUPLICATE_COUNT_COLOR: egui::Color32 = egui::Color32::from_gray(150);
const TOOLTIP_MAX_MEMBERS: usize = 30;

struct DuplicateGroups {
    model: Weak<FileSystemModel>,
    filter: PathFilter,
    groups: Vec<DuplicateGroup>,
    /// Group index and index into its paths.
    member_of: HashMap<FsNodeId, (usize, usize)>,
    duplicates_below: HashMap<FsNodeId, usize>,
    hash_of: HashMap<FsNodeId, HashRepresentation>,
}

fn duplicate_groups(
    view: &FileSystemView,
    filter: &PathFilter,
    hashes: &HashMap<PathBuf, Option<HashRepresentation>>,
) -> DuplicateGroups {
    let model = &view.file_system;
    let hidden = tree_hidden(view, filter);
    let mut node_of = HashMap::new();
    let mut hash_of = HashMap::new();
    let files: Vec<_> = model
        .iter_tree()
        .filter(|(node_id, node, _)| node.is_file() && !hidden[*node_id])
        .filter_map(|(node_id, node, _)| {
            let path = model.path(node_id);
            let Some(Some(hash)) = hashes.get(&path) else {
                return None;
            };
            node_of.insert(path.clone(), node_id);
            hash_of.insert(node_id, hash.clone());
            Some((path, node.size, hash.clone()))
        })
        .collect();
    let groups = group_by_size_and_hash(files);

    let mut member_of = HashMap::new();
    let mut duplicates_below = HashMap::new();
    for (group_index, group) in groups.iter().enumerate() {
        for (member_index, path) in group.paths.iter().enumerate() {
            let node_id = node_of[path];
            member_of.insert(node_id, (group_index, member_index));
            let mut folder = model.get_parent_id(node_id);
            while let Some(folder_id) = folder {
                *duplicates_below.entry(folder_id).or_default() += 1;
                folder = model.get_parent_id(folder_id);
            }
        }
    }

    DuplicateGroups {
        model: Arc::downgrade(model),
        filter: filter.clone(),
        groups,
        member_of,
        duplicates_below,
        hash_of,
    }
}

#[derive(Debug, PartialEq)]
enum GroupCell<'a> {
    File {
        /// 1-based, in tree order.
        id: usize,
        group: &'a DuplicateGroup,
        path: &'a Path,
        keeper: bool,
        pending: bool,
    },
    Folder {
        duplicates: usize,
    },
}

impl DuplicateGroups {
    fn is_for(&self, view: &FileSystemView, filter: &PathFilter) -> bool {
        self.model.ptr_eq(&Arc::downgrade(&view.file_system)) && self.filter == *filter
    }

    fn cell(
        &self,
        node_id: FsNodeId,
        keepers: &HashMap<String, PathBuf>,
        pending: &HashSet<PathBuf>,
    ) -> Option<GroupCell<'_>> {
        if let Some(&(group_index, member_index)) = self.member_of.get(&node_id) {
            let group = &self.groups[group_index];
            let path = &group.paths[member_index];
            return Some(GroupCell::File {
                id: group_index + 1,
                group,
                path,
                keeper: keepers
                    .get(&group.hash)
                    .is_some_and(|keeper| keeper == path),
                pending: pending.contains(path),
            });
        }
        let duplicates = *self.duplicates_below.get(&node_id)?;
        Some(GroupCell::Folder { duplicates })
    }

    fn member(&self, node_id: FsNodeId) -> Option<(&DuplicateGroup, &Path)> {
        let &(group_index, member_index) = self.member_of.get(&node_id)?;
        let group = &self.groups[group_index];
        Some((group, &group.paths[member_index]))
    }
}

#[derive(Debug, PartialEq)]
enum KeepItem {
    Keep,
    DontKeep,
}

fn keep_item(
    groups: &DuplicateGroups,
    keepers: &HashMap<String, PathBuf>,
    node_id: FsNodeId,
) -> Option<KeepItem> {
    let (group, path) = groups.member(node_id)?;
    Some(
        if keepers
            .get(&group.hash)
            .is_some_and(|keeper| keeper == path)
        {
            KeepItem::DontKeep
        } else {
            KeepItem::Keep
        },
    )
}

/// Keeps a group member, or clears its group's keeper when it is the keeper.
fn toggle_keep(
    groups: &DuplicateGroups,
    conflict_map: &HashMap<String, Vec<PathBuf>>,
    keepers: &mut HashMap<String, PathBuf>,
    node_id: FsNodeId,
) {
    let Some((group, path)) = groups.member(node_id) else {
        log::info!("Keep this: not a file with a duplicate");
        return;
    };
    let keep = (keep_item(groups, keepers, node_id) == Some(KeepItem::Keep)).then_some(path);
    set_keeper(conflict_map, keepers, &group.hash, keep);
}

/// Groups are ordered by their first member in display order, not by id.
fn group_jump(
    cursor: &TreeCursor,
    view: &FileSystemView,
    rows: &[VisibleRowTwoFolderDiff],
    groups: &DuplicateGroups,
    flat: bool,
    forward: bool,
) -> Option<(TreeCursor, Vec<CursorRequest>)> {
    let mut seen = HashSet::new();
    let mut cursor_rows = two_folder_cursor_rows(Some(view), None, rows, flat);
    // The diff jumps stop on differing files, so `differs` marks the stops.
    for (cursor_row, row) in cursor_rows.iter_mut().zip(rows) {
        let group = row
            .diff_state
            .first()
            .and_then(|node_id| groups.member_of.get(&node_id));
        cursor_row.differs = group.is_some_and(|&(group_index, _)| seen.insert(group_index));
    }
    if forward {
        cursor.next_diff(&cursor_rows)
    } else {
        cursor.prev_diff(&cursor_rows)
    }
}

#[derive(Debug, PartialEq)]
struct ResolvePreview {
    kept: Vec<PathBuf>,
    removed: Vec<PathBuf>,
    /// Groups without a keeper.
    skipped: usize,
}

fn resolve_preview(
    conflict_map: &HashMap<String, Vec<PathBuf>>,
    keepers: &HashMap<String, PathBuf>,
) -> ResolvePreview {
    let mut hashes: Vec<&String> = conflict_map.keys().collect();
    hashes.sort();
    let kept: Vec<PathBuf> = hashes
        .iter()
        .filter_map(|hash| keeper_of(&conflict_map[*hash], keepers, hash).cloned())
        .collect();
    ResolvePreview {
        skipped: conflict_map.len() - kept.len(),
        kept,
        removed: plan_resolution(conflict_map, keepers)
            .deletions
            .into_iter()
            .map(|deletion| deletion.path)
            .collect(),
    }
}

fn member_tooltip(
    group: &DuplicateGroup,
    path: &Path,
    keepers: &HashMap<String, PathBuf>,
    root: &Path,
) -> String {
    let others: Vec<&PathBuf> = group.paths.iter().filter(|p| *p != path).collect();
    let mut text = format!(
        "{} other {}:",
        others.len(),
        if others.len() == 1 { "copy" } else { "copies" }
    );
    for other in others.iter().take(TOOLTIP_MAX_MEMBERS) {
        let shown = other.strip_prefix(root).unwrap_or(other);
        text.push_str(&format!("\n{}", shown.display()));
        if keepers.get(&group.hash) == Some(*other) {
            text.push_str(" (keep)");
        }
    }
    if others.len() > TOOLTIP_MAX_MEMBERS {
        text.push_str(&format!("\n… {} more", others.len() - TOOLTIP_MAX_MEMBERS));
    }
    text
}

fn ui_group_cell(
    ui: &mut egui::Ui,
    cell: Option<GroupCell>,
    keepers: &HashMap<String, PathBuf>,
    root: &Path,
) {
    match cell {
        None => {}
        Some(GroupCell::Folder { duplicates }) => {
            ui.label(egui::RichText::new(format!("{duplicates} dup")).color(DUPLICATE_COUNT_COLOR))
                .on_hover_text(format!(
                    "{duplicates} files with a duplicate in this folder"
                ));
        }
        Some(GroupCell::File {
            id,
            group,
            path,
            keeper,
            pending,
        }) => {
            let chip = egui::Frame::new()
                .fill(hash_to_color(&group.hash))
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(4, 0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(id.to_string())
                            .color(egui::Color32::BLACK)
                            .strong(),
                    );
                })
                .response;
            let mark = if keeper {
                Some(ui.label(egui::RichText::new("✔ keep").color(KEEPER_COLOR)))
            } else if pending {
                Some(
                    ui.label(
                        egui::RichText::new("✖")
                            .color(role_color(ui.ctx(), FolderRole::PendingDeletion)),
                    ),
                )
            } else {
                None
            };
            let response = mark.map_or(chip.clone(), |mark| chip | mark);
            response.on_hover_ui(|ui| {
                ui.label(member_tooltip(group, path, keepers, root));
            });
        }
    }
}

fn files_to_hash(view: &FileSystemView, hidden: &[bool]) -> Vec<PathBuf> {
    let files: Vec<SizedFile> = view
        .file_system
        .iter_files()
        .map(|node_id| {
            let node = view
                .file_system
                .get_node(node_id)
                .expect("iter_files yields valid node ids");
            SizedFile {
                path: view.file_system.path(node_id),
                size: node.size,
                hidden: hidden[node_id],
            }
        })
        .collect();
    size_collisions(&files)
}

#[derive(Debug, PartialEq)]
struct HashProgress {
    hashed: usize,
    queued: usize,
}

fn view_root(view: Option<&FileSystemView>) -> Option<PathBuf> {
    view.map(|view| view.file_system.path(view.file_system.get_root_node_id()))
}

fn non_disk_root_message(sides: [Option<(&Path, &ContentSource)>; 2]) -> Option<String> {
    let described: Vec<_> = sides
        .into_iter()
        .enumerate()
        .filter_map(|(index, side)| {
            let (root, content) = side?;
            let kind = match content {
                ContentSource::Disk => return None,
                ContentSource::Git(_) => "a git revision",
                ContentSource::Depot(_) => "a depot path",
            };
            Some(format!(
                "Folder {} ({}) is {kind}",
                index + 1,
                root.display()
            ))
        })
        .collect();
    (!described.is_empty()).then(|| {
        format!(
            "No duplicates are searched: {}. Duplicates are found by hashing files on disk.",
            described.join("; ")
        )
    })
}

fn non_disk_root_message_of(path_diff_view: &PathDiffView) -> Option<String> {
    let views = [
        path_diff_view.file_system_1_view.as_ref(),
        path_diff_view.file_system_2_view.as_ref(),
    ];
    let roots = views.map(|view| view_root(view));
    non_disk_root_message([0, 1].map(|side| {
        let root = roots[side].as_deref()?;
        Some((root, views[side]?.file_system.content()))
    }))
}

/// The group's keeper, if it has one that is a member of the group.
fn keeper_of<'a>(
    paths: &[PathBuf],
    keepers: &'a HashMap<String, PathBuf>,
    hash: &str,
) -> Option<&'a PathBuf> {
    keepers.get(hash).filter(|keeper| paths.contains(keeper))
}

/// Files a resolve would delete. Taken from the resolve's own plan so the marks can't
/// drift from what is removed.
fn pending_deletions(
    conflict_map: &HashMap<String, Vec<PathBuf>>,
    keepers: &HashMap<String, PathBuf>,
) -> HashSet<PathBuf> {
    plan_resolution(conflict_map, keepers)
        .deletions
        .into_iter()
        .map(|deletion| deletion.path)
        .collect()
}

/// The only way keepers are stored, so a group is either without a keeper or kept by
/// one of its own files. `None` clears the group's keeper.
fn set_keeper(
    conflict_map: &HashMap<String, Vec<PathBuf>>,
    keepers: &mut HashMap<String, PathBuf>,
    hash: &str,
    keeper: Option<&Path>,
) {
    let Some(keeper) = keeper else {
        keepers.remove(hash);
        return;
    };
    let is_member = conflict_map
        .get(hash)
        .is_some_and(|paths| paths.iter().any(|path| path == keeper));
    if !is_member {
        log::error!(
            "Not keeping {}: it is not a file of conflict {hash}",
            keeper.display()
        );
        return;
    }
    keepers.insert(hash.to_string(), keeper.to_path_buf());
}

fn can_resolve(
    conflict_map: &HashMap<String, Vec<PathBuf>>,
    keepers: &HashMap<String, PathBuf>,
) -> bool {
    conflict_map
        .iter()
        .any(|(hash, paths)| keeper_of(paths, keepers, hash).is_some())
}

#[derive(Serialize, Deserialize)]
pub struct DuplicateFilesPane {
    pub title: Option<String>,

    #[serde(skip)]
    confirm_resolve: bool,
    #[serde(skip)]
    pub open_dir_window: bool,
    #[serde(skip)]
    groups: Option<DuplicateGroups>,
    #[serde(skip)]
    was_hashing: bool,
    #[serde(skip)]
    last_regroup: Option<Instant>,
    /// Outcome of the last Resolve, shown until closed.
    #[serde(skip)]
    resolution_summary: Option<ResolutionSummary>,
    #[serde(skip)]
    cursor: TreeCursor,
    /// Root of the view the cursor belongs to.
    #[serde(skip)]
    cursor_root: Option<PathBuf>,
    #[serde(skip)]
    tree_cache: Option<TreeRows>,
    #[serde(skip)]
    hashed_for: Option<(Weak<FileSystemModel>, PathFilter)>,
    /// Files requested since hashing was last idle.
    #[serde(skip)]
    hash_batch: usize,
    #[serde(skip)]
    cursor_rows: CursorRowsCache,
    #[serde(skip)]
    members_generation: u64,
}

struct TreeRows {
    // Weak, so a replaced model's address can't be reused by a new one while it is held here.
    model: Weak<FileSystemModel>,
    filter: PathFilter,
    sort: TreeSort,
    duplicates_filter: DuplicatesFilter,
    members: Option<u64>,
    rows: Vec<VisibleRowTwoFolderDiff>,
    id: RowsId,
}

impl ZAppPane for DuplicateFilesPane {
    fn title(&self) -> String {
        self.title.clone().unwrap_or("File Explorer".into())
    }
}

pub struct DuplicateFilesPaneCtx<'a, 'b> {
    pub hash_service: &'a mut HashService,
    pub path_diff_view: &'a mut PathDiffView<'b>,
    pub path_filter: &'a PathFilter,
    pub tree_sort: &'a mut TreeSort,
    pub duplicates_filter: &'a mut DuplicatesFilter,
    pub diff_tool_config: &'a DiffToolConfig,
    pub tool_message: &'a mut Option<ToolMessage>,
    pub keyboard_taken: bool,

    // Diff Action State
    pub conflict_map: &'a mut HashMap<String, Vec<PathBuf>>,
    pub conflict_map_resolved: &'a mut HashMap<String, PathBuf>,
    #[allow(dead_code)]
    pub diff_action_pressed: &'a mut bool,
    pub actions: &'a [FolderAction],
}

impl DuplicateFilesPane {
    pub fn new(title: Option<String>) -> Self {
        Self {
            title,
            confirm_resolve: false,
            open_dir_window: false,
            groups: None,
            was_hashing: false,
            last_regroup: None,
            resolution_summary: None,
            cursor: TreeCursor::default(),
            cursor_root: None,
            tree_cache: None,
            hashed_for: None,
            hash_batch: 0,
            cursor_rows: CursorRowsCache::default(),
            members_generation: 0,
        }
    }

    /// Requests the size collisions once per scanned model and filter, so a Cancel holds
    /// until Folder 1 is rescanned or the filter changes.
    fn hash_size_collisions(&mut self, ctx: &mut DuplicateFilesPaneCtx) {
        let Some(view) = ctx.path_diff_view.file_system_1_view.as_ref() else {
            return;
        };
        // Revision nodes have no file on disk; their paths would be looked up as network shares.
        if non_disk_root_message_of(ctx.path_diff_view).is_some() {
            return;
        }
        let model = Arc::downgrade(&view.file_system);
        let requested = self
            .hashed_for
            .as_ref()
            .is_some_and(|(hashed, filter)| hashed.ptr_eq(&model) && filter == ctx.path_filter);
        if requested {
            return;
        }
        self.hashed_for = Some((model, ctx.path_filter.clone()));

        if self.hash_progress(ctx.hash_service).is_none() {
            self.hash_batch = 0;
        }
        let files = files_to_hash(view, &tree_hidden(view, ctx.path_filter));
        self.hash_batch += files
            .iter()
            .filter(|path| ctx.hash_service.request(path))
            .count();
    }

    fn hash_progress(&self, hash_service: &HashService) -> Option<HashProgress> {
        let queued = hash_service.count_hash_queue() + hash_service.count_active_hashes();
        (queued > 0).then(|| HashProgress {
            hashed: self.hash_batch.saturating_sub(queued),
            queued,
        })
    }

    fn cancel_hashing(&mut self, ctx: &mut DuplicateFilesPaneCtx) {
        ctx.hash_service.cancel();
        self.hash_batch = ctx.hash_service.count_active_hashes();
    }

    fn clear_hashes(&mut self, ctx: &mut DuplicateFilesPaneCtx) {
        self.cancel_hashing(ctx);
        ctx.hash_service.clear();
        self.hashed_for = None;
        self.groups = None;
    }

    /// Rebuilt when Folder 1 is rescanned or replaced, the filter or sort changed, or with
    /// Duplicates only, the group members changed.
    fn tree_rows(
        &mut self,
        view: &FileSystemView,
        filter: &PathFilter,
        sort: TreeSort,
        duplicates_filter: DuplicatesFilter,
    ) -> &[VisibleRowTwoFolderDiff] {
        let model = Arc::downgrade(&view.file_system);
        let members = duplicates_filter
            .duplicates_only
            .then_some(self.members_generation);
        let current = self.tree_cache.as_ref().is_some_and(|cache| {
            cache.model.ptr_eq(&model)
                && cache.filter == *filter
                && cache.sort == sort
                && cache.duplicates_filter == duplicates_filter
                && cache.members == members
        });
        if !current {
            let rows = single_side_rows(view, filter, sort).unwrap_or_else(|e| {
                log::error!("Duplicate tree rows not built: {e}");
                Vec::new()
            });
            let diff_filter = duplicates_filter.diff_filter();
            let member_of = self.groups.as_ref().map(|groups| &groups.member_of);
            let rows = filter_rows(rows, diff_filter, |row| {
                row.diff_state
                    .first()
                    .is_some_and(|node_id| member_of.is_some_and(|m| m.contains_key(&node_id)))
            });
            let rows = if diff_filter.lists_files() {
                sort_file_list(rows, sort)
            } else {
                rows
            };
            self.tree_cache = Some(TreeRows {
                model,
                filter: filter.clone(),
                sort,
                duplicates_filter,
                members,
                rows,
                id: RowsId::next(),
            });
        }
        &self.tree_cache.as_ref().expect("built above").rows
    }

    /// Opening another folder clears the cursor. A rescan of the same root keeps it.
    fn forget_cursor_of_other_root(&mut self, view: Option<&FileSystemView>) {
        let root = view_root(view);
        if root != self.cursor_root {
            self.cursor = TreeCursor::default();
            self.cursor_root = root;
        }
    }

    /// Keep this and the group jumps. Returns whether the cursor moved.
    fn apply_actions(&mut self, ctx: &mut DuplicateFilesPaneCtx) -> bool {
        let Some(view) = ctx.path_diff_view.file_system_1_view.as_mut() else {
            return false;
        };
        if ctx.actions.is_empty() {
            return false;
        }
        self.tree_rows(
            view,
            ctx.path_filter,
            *ctx.tree_sort,
            *ctx.duplicates_filter,
        );
        let rows = &self.tree_cache.as_ref().expect("built above").rows;
        let Some(groups) = self.groups.as_ref() else {
            return false;
        };
        let flat = ctx.duplicates_filter.diff_filter().is_flat(*ctx.tree_sort);
        let mut moved = false;
        for action in ctx.actions {
            let forward = match action {
                FolderAction::KeepThis => {
                    let node_id = self.cursor.rel_path().and_then(|rel_path| {
                        rows.iter()
                            .find(|row| row.rel_path == rel_path)
                            .and_then(|row| row.diff_state.first())
                    });
                    match node_id {
                        Some(node_id) => toggle_keep(
                            groups,
                            ctx.conflict_map,
                            ctx.conflict_map_resolved,
                            node_id,
                        ),
                        None => log::info!("Keep this: no file at the cursor"),
                    }
                    continue;
                }
                FolderAction::NextGroup => true,
                FolderAction::PrevGroup => false,
                FolderAction::OpenFolder1
                | FolderAction::OpenFolder2
                | FolderAction::NextDiff
                | FolderAction::PrevDiff
                | FolderAction::Refresh
                | FolderAction::OpenKeybindings
                | FolderAction::Find => continue,
            };
            let Some((cursor, expand)) =
                group_jump(&self.cursor, view, rows, groups, flat, forward)
            else {
                log::info!("No more duplicate groups");
                continue;
            };
            for request in &expand {
                let folder = rows
                    .iter()
                    .find(|row| row.rel_path == request.rel_path())
                    .and_then(|row| row.diff_state.first());
                if let Some(folder) = folder {
                    view.collapsed.remove(&folder);
                }
            }
            moved |= cursor != self.cursor;
            self.cursor = cursor;
        }
        moved
    }

    /// Groups again when Folder 1 or the filter changed, every `REGROUP_INTERVAL` while
    /// hashing, and once after it, which sees the last hashes.
    fn update_groups(&mut self, ctx: &mut DuplicateFilesPaneCtx) {
        const REGROUP_INTERVAL: Duration = Duration::from_millis(500);
        let hashing = self.hash_progress(ctx.hash_service).is_some();
        let was_hashing = std::mem::replace(&mut self.was_hashing, hashing);
        let current = match (ctx.path_diff_view.file_system_1_view.as_ref(), &self.groups) {
            (Some(view), Some(groups)) => groups.is_for(view, ctx.path_filter),
            (None, None) => true,
            _ => false,
        };
        let regrouped_recently = self
            .last_regroup
            .is_some_and(|at| at.elapsed() < REGROUP_INTERVAL);
        if current && (hashing && regrouped_recently || !hashing && !was_hashing) {
            return;
        }
        let snapshot = ctx.hash_service.snapshot();
        self.regroup(ctx, &snapshot.hashes);
        self.last_regroup = Some(Instant::now());
    }

    /// Keepers survive only as members of their group, so a resolve can't recycle a file
    /// the user no longer sees.
    fn regroup(
        &mut self,
        ctx: &mut DuplicateFilesPaneCtx,
        hashes: &HashMap<PathBuf, Option<HashRepresentation>>,
    ) {
        let groups = ctx
            .path_diff_view
            .file_system_1_view
            .as_ref()
            .map(|view| duplicate_groups(view, ctx.path_filter, hashes));
        *ctx.conflict_map = groups
            .iter()
            .flat_map(|groups| &groups.groups)
            .map(|group| {
                let mut paths = group.paths.clone();
                paths.sort();
                (group.hash.clone(), paths)
            })
            .collect();

        let conflict_map = &*ctx.conflict_map;
        ctx.conflict_map_resolved.retain(|hash, keeper| {
            conflict_map
                .get(hash)
                .is_some_and(|paths| paths.contains(keeper))
        });
        let same_members = match (&self.groups, &groups) {
            (Some(old), Some(new)) => {
                old.member_of.len() == new.member_of.len()
                    && new
                        .member_of
                        .keys()
                        .all(|id| old.member_of.contains_key(id))
            }
            (None, None) => true,
            _ => false,
        };
        if !same_members {
            self.members_generation += 1;
        }
        self.groups = groups;
    }

    /// Resolve: removes every non-keeper of the groups with a keeper through `delete`, then
    /// forgets the conflicts and requests a rescan of the folder.
    fn resolve_selected<E: Display>(
        &mut self,
        ctx: &mut DuplicateFilesPaneCtx,
        delete: impl FnMut(&Path) -> Result<(), E>,
    ) {
        let plan = plan_resolution(ctx.conflict_map, ctx.conflict_map_resolved);
        let summary = execute_resolution(&plan, delete);

        for path in &summary.removed {
            ctx.hash_service.remove(path);
        }
        ctx.conflict_map.clear();
        ctx.conflict_map_resolved.clear();

        if ctx.path_diff_view.file_system_1_view.is_some() {
            ctx.path_diff_view.file_system_1_scan.request_rescan();
        }

        self.resolution_summary = Some(summary);
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &mut DuplicateFilesPaneCtx,
    ) -> egui_tiles::UiResponse {
        self.forget_cursor_of_other_root(ctx.path_diff_view.file_system_1_view.as_ref());
        self.hash_size_collisions(ctx);
        self.update_groups(ctx);
        let scroll_to_cursor = self.apply_actions(ctx);

        ui.vertical(|ui| {
            self.ui_popups(ui, ctx);

            ui.horizontal_wrapped(|ui| {
                if ui.button("Open Folder").clicked() {
                    self.open_dir_window = true;
                }

                if let Some(fs_view) = &mut ctx.path_diff_view.file_system_1_view {
                    let nodes_considered_for_collapse =
                        &fs_view.file_system.get_root().children().unwrap().clone();
                    let is_anything_collapsed =
                        fs_view.is_anything_collapsed_slice(nodes_considered_for_collapse);
                    let button_text = if is_anything_collapsed {
                        "Collapse All"
                    } else {
                        "Expand All"
                    };

                    if ui.button(button_text).clicked() {
                        fs_view.recursive_collapse_slice(
                            nodes_considered_for_collapse,
                            !is_anything_collapsed,
                        );
                    }
                }

                if ui.button("Clear Hashes").clicked() {
                    self.clear_hashes(ctx);
                }

                ui.label("Concurrent Hashes");
                let mut slider_concurrent_hashes = ctx.hash_service.count_threads();
                let slider_response = ui.add(egui::Slider::new(
                    &mut slider_concurrent_hashes,
                    1..=MAX_CONCURRENT_HASHES,
                ));
                if slider_response.changed() {
                    ctx.hash_service.resize_workers(slider_concurrent_hashes);
                }

                if ctx.path_diff_view.file_system_1_view.is_some() {
                    // While the folder is rescanned the tree may still list files a resolve removed.
                    let rescanning = ctx.path_diff_view.file_system_1_scan.is_scanning();
                    let enabled =
                        !rescanning && can_resolve(ctx.conflict_map, ctx.conflict_map_resolved);
                    if ui
                        .add_enabled(enabled, egui::Button::new("Resolve"))
                        .on_disabled_hover_text("Keep a file of a group first")
                        .clicked()
                    {
                        self.confirm_resolve = true;
                    }
                }
            });

            if let Some(progress) = self.hash_progress(ctx.hash_service) {
                ui.horizontal(|ui| {
                    let total = progress.hashed + progress.queued;
                    ui.add(
                        egui::ProgressBar::new(progress.hashed as f32 / total as f32)
                            .desired_width(300.0)
                            .text(format!(
                                "{} hashed, {} queued",
                                progress.hashed, progress.queued
                            )),
                    );
                    if ui.button("Cancel").clicked() {
                        self.cancel_hashing(ctx);
                    }
                });
                // Workers don't wake the ui.
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let filter = &mut *ctx.duplicates_filter;
                toggle_button(
                    ui,
                    &mut filter.duplicates_only,
                    egui::RichText::new("D").strong(),
                    "Duplicates only: hide files without a duplicate and folders without one below",
                );
                toggle_button(
                    ui,
                    &mut ctx.tree_sort.flat,
                    egui::RichText::new("F").strong(),
                    "Flat: list all entries by depth, then by the sorted column (shared with the folder view)",
                );
                ui.add_enabled_ui(filter.duplicates_only, |ui| {
                    toggle_button(
                        ui,
                        &mut filter.show_parents,
                        egui::RichText::new("P").strong(),
                        "Show parent folders: off lists only the files with a duplicate (needs Duplicates only)",
                    );
                });
                toggle_button(
                    ui,
                    &mut filter.hash_column,
                    egui::RichText::new("H").strong(),
                    "Hash column: show the hash of each hashed file",
                );
            });
        });

        ui.separator();

        if let Some(message) = non_disk_root_message_of(ctx.path_diff_view) {
            ui.colored_label(ui.visuals().warn_fg_color, message);
        }

        if let Some(view) = ctx.path_diff_view.file_system_1_view.as_ref() {
            self.tree_rows(
                view,
                ctx.path_filter,
                *ctx.tree_sort,
                *ctx.duplicates_filter,
            );
            let cache = self.tree_cache.as_ref().expect("built above");
            let pending_deletion = pending_deletions(ctx.conflict_map, ctx.conflict_map_resolved);
            let root = view_root(Some(view)).expect("a view has a root");
            let groups = self.groups.as_ref();
            let keepers = &*ctx.conflict_map_resolved;
            let mut group_cell = |ui: &mut egui::Ui, node_id: FsNodeId| {
                let cell =
                    groups.and_then(|groups| groups.cell(node_id, keepers, &pending_deletion));
                ui_group_cell(ui, cell, keepers, &root);
            };
            let mut hash_cell = |ui: &mut egui::Ui, node_id: FsNodeId| {
                if let Some(hash) = groups.and_then(|groups| groups.hash_of.get(&node_id)) {
                    ui.label(egui::RichText::new(hash).monospace())
                        .on_hover_text(hash);
                }
            };
            let mut columns = vec![NodeColumn {
                header: "Group",
                width: GROUP_COLUMN_WIDTH,
                cell: &mut group_cell,
            }];
            if ctx.duplicates_filter.hash_column {
                columns.push(NodeColumn {
                    header: "Hash",
                    width: HASH_COLUMN_WIDTH,
                    cell: &mut hash_cell,
                });
            }
            let mut keep_clicked = None;
            let mut row_menu = |ui: &mut egui::Ui, node_id: FsNodeId| {
                let Some(item) = groups.and_then(|groups| keep_item(groups, keepers, node_id))
                else {
                    return;
                };
                ui.separator();
                let text = match item {
                    KeepItem::Keep => "Keep this",
                    KeepItem::DontKeep => "Don't keep",
                };
                if ui.button(text).clicked() {
                    keep_clicked = Some(node_id);
                }
            };
            draw_ui_folder_tree(
                ui,
                ctx.path_diff_view.file_system_1_view,
                &mut None,
                Some(&cache.rows),
                cache.id,
                ctx.path_diff_view.file_system_1_root_path,
                &mut None,
                &mut self.open_dir_window,
                &mut false,
                ctx.diff_tool_config,
                ctx.tool_message,
                &mut self.cursor,
                &mut self.cursor_rows,
                ctx.tree_sort,
                ctx.keyboard_taken,
                &[],
                ctx.duplicates_filter.diff_filter(),
                TreeMode::SingleSide {
                    pending_deletion: &pending_deletion,
                    columns,
                    row_menu: &mut row_menu,
                    scroll_to_cursor,
                },
            );
            if let (Some(node_id), Some(groups)) = (keep_clicked, self.groups.as_ref()) {
                toggle_keep(groups, ctx.conflict_map, ctx.conflict_map_resolved, node_id);
            }
        } else {
            ui.label("No root dir set...");
            if ui.button("Open Folder").clicked() {
                self.open_dir_window = true;
            }
        }

        if self.open_dir_window {
            self.open_dir_window = false;
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                // The app loads the view from the root path; a view set here directly
                // would be replaced on the next frame.
                let _ = set_root(
                    ctx.path_diff_view.file_system_1_root_path,
                    Side::Folder1,
                    &path,
                );
            }
        }

        egui_tiles::UiResponse::None
    }

    fn ui_popups(&mut self, ui: &mut egui::Ui, ctx: &mut DuplicateFilesPaneCtx) {
        self.ui_resolve_confirm(ui, ctx);
        self.ui_resolution_summary(ui);
    }

    /// Rebuilt every frame, so what is recycled is the list shown.
    fn ui_resolve_confirm(&mut self, ui: &mut egui::Ui, ctx: &mut DuplicateFilesPaneCtx) {
        if !self.confirm_resolve {
            return;
        }
        let preview = resolve_preview(ctx.conflict_map, ctx.conflict_map_resolved);
        if preview.kept.is_empty() {
            self.confirm_resolve = false;
            return;
        }
        let root = view_root(ctx.path_diff_view.file_system_1_view.as_ref()).unwrap_or_default();
        let shown = |path: &PathBuf| {
            path.strip_prefix(&root)
                .unwrap_or(path)
                .display()
                .to_string()
        };
        let rescanning = ctx.path_diff_view.file_system_1_scan.is_scanning();
        let mut is_open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        show_custom_popup(ui.ctx(), &mut is_open, "Resolve Duplicates", true, |ui| {
            ui.label(format!("Keep {} files:", preview.kept.len()));
            egui::ScrollArea::vertical()
                .id_salt("resolve_kept")
                .max_height(150.0)
                .show(ui, |ui| {
                    for path in &preview.kept {
                        ui.label(egui::RichText::new(shown(path)).color(KEEPER_COLOR));
                    }
                });
            ui.separator();
            ui.label(format!(
                "Move {} files to the Recycle Bin:",
                preview.removed.len()
            ));
            egui::ScrollArea::vertical()
                .id_salt("resolve_removed")
                .max_height(200.0)
                .show(ui, |ui| {
                    for path in &preview.removed {
                        ui.label(
                            egui::RichText::new(shown(path))
                                .color(role_color(ui.ctx(), FolderRole::PendingDeletion)),
                        );
                    }
                });
            if preview.skipped > 0 {
                ui.separator();
                ui.label(format!(
                    "{} groups without a kept file are skipped",
                    preview.skipped
                ));
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !rescanning,
                        egui::Button::new(format!("Recycle {} files", preview.removed.len())),
                    )
                    .clicked()
                {
                    confirmed = true;
                }
                if ui.button("Cancel").clicked() {
                    cancelled = true;
                }
            });
        });
        if confirmed {
            self.confirm_resolve = false;
            self.resolve_selected(ctx, recycle);
        } else if !is_open || cancelled {
            self.confirm_resolve = false;
        }
    }

    fn ui_resolution_summary(&mut self, ui: &mut egui::Ui) {
        let Some(summary) = &self.resolution_summary else {
            return;
        };
        let mut is_open = true;
        let mut close_clicked = false;
        show_custom_popup(ui.ctx(), &mut is_open, "Resolve Summary", true, |ui| {
            ui.label(format!(
                "Recycled {} files, {} failed",
                summary.removed.len(),
                summary.failed.len()
            ));
            if !summary.failed.is_empty() {
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for (path, error) in &summary.failed {
                            ui.label(
                                egui::RichText::new(format!("{}: {error}", path.display()))
                                    .color(ui.visuals().error_fg_color),
                            );
                        }
                    });
            }
            ui.separator();
            if ui.button("Close").clicked() {
                close_clicked = true;
            }
        });
        if !is_open || close_clicked {
            self.resolution_summary = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_egui::app::{SideScan, finish_side_scan};
    use crate::ui_egui::tree_sort::SortKey;
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::time::Instant;
    use tempfile::{TempDir, tempdir};
    use zcommon::hash::hash_file_mmap;
    use zhashdiff::filter::PatternList;
    use zhashdiff::vcs_source::{DepotContent, DepotRevisions, GitContent};

    /// Owns everything a `DuplicateFilesPaneCtx` borrows.
    struct PaneState {
        hash_service: HashService,
        root_1: Option<PathBuf>,
        root_2: Option<PathBuf>,
        view_1: Option<FileSystemView>,
        scan_1: SideScan,
        view_2: Option<FileSystemView>,
        visible_rows: Option<Vec<VisibleRowTwoFolderDiff>>,
        path_filter: PathFilter,
        tree_sort: TreeSort,
        duplicates_filter: DuplicatesFilter,
        diff_tool_config: DiffToolConfig,
        tool_message: Option<ToolMessage>,
        conflict_map: HashMap<String, Vec<PathBuf>>,
        conflict_map_resolved: HashMap<String, PathBuf>,
        diff_action_pressed: bool,
        actions: Vec<FolderAction>,
    }

    impl PaneState {
        fn new(view: FileSystemView) -> Self {
            Self {
                hash_service: HashService::new(0),
                root_1: view_root(Some(&view)),
                root_2: None,
                view_1: Some(view),
                scan_1: SideScan::default(),
                view_2: None,
                visible_rows: None,
                path_filter: PathFilter::default(),
                tree_sort: TreeSort::default(),
                duplicates_filter: DuplicatesFilter::default(),
                diff_tool_config: DiffToolConfig::default(),
                tool_message: None,
                conflict_map: HashMap::new(),
                conflict_map_resolved: HashMap::new(),
                diff_action_pressed: false,
                actions: Vec::new(),
            }
        }

        fn regroup(
            &mut self,
            pane: &mut DuplicateFilesPane,
            hashes: &HashMap<PathBuf, Option<HashRepresentation>>,
        ) {
            self.with_ctx(|ctx| pane.regroup(ctx, hashes))
        }

        fn frame(&mut self, pane: &mut DuplicateFilesPane) {
            self.with_ctx(|ctx| {
                pane.hash_size_collisions(ctx);
                pane.update_groups(ctx);
            });
        }

        fn hash_frames(&mut self, pane: &mut DuplicateFilesPane) {
            self.frame(pane);
            self.hash_service.resize_workers(1);
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                assert!(Instant::now() < deadline, "hashing did not finish");
                let idle = pane.hash_progress(&self.hash_service).is_none();
                self.frame(pane);
                if idle {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            self.hash_service.resize_workers(0);
        }

        fn finish_rescan(&mut self) {
            finish_side_scan(
                Side::Folder1,
                &mut self.root_1,
                &mut self.view_1,
                &mut self.scan_1,
                &mut self.visible_rows,
            );
        }

        fn with_ctx<R>(&mut self, f: impl FnOnce(&mut DuplicateFilesPaneCtx) -> R) -> R {
            let mut path_diff_view = PathDiffView {
                file_system_1_root_path: &mut self.root_1,
                file_system_2_root_path: &mut self.root_2,
                file_system_1_view: &mut self.view_1,
                file_system_2_view: &mut self.view_2,
                file_system_1_scan: &mut self.scan_1,
                visible_rows: &mut self.visible_rows,
                rows_id: RowsId::default(),
            };
            let mut ctx = DuplicateFilesPaneCtx {
                hash_service: &mut self.hash_service,
                path_diff_view: &mut path_diff_view,
                path_filter: &self.path_filter,
                tree_sort: &mut self.tree_sort,
                duplicates_filter: &mut self.duplicates_filter,
                diff_tool_config: &self.diff_tool_config,
                tool_message: &mut self.tool_message,
                keyboard_taken: false,
                conflict_map: &mut self.conflict_map,
                conflict_map_resolved: &mut self.conflict_map_resolved,
                diff_action_pressed: &mut self.diff_action_pressed,
                actions: &self.actions,
            };
            f(&mut ctx)
        }
    }

    /// a.txt, b.txt, sub/c.txt and e.txt share content, d.txt is unique.
    fn duplicate_tree() -> (TempDir, FileSystemView) {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        for name in ["a.txt", "b.txt", "sub/c.txt", "e.txt"] {
            fs::write(dir.path().join(name), "same").unwrap();
        }
        fs::write(dir.path().join("d.txt"), "other").unwrap();

        let model = FileSystemModel::new(dir.path()).unwrap();
        (dir, FileSystemView::new(Arc::new(model)))
    }

    fn hashes_of(paths: &[&Path]) -> HashMap<PathBuf, Option<HashRepresentation>> {
        paths
            .iter()
            .map(|p| (p.to_path_buf(), Some(hash_file_mmap(p).unwrap())))
            .collect()
    }

    fn same_hash(dir: &Path) -> String {
        hash_file_mmap(dir.join("a.txt")).unwrap()
    }

    #[test]
    fn regroup_groups_all_visible_hashed_files() {
        let (dir, view) = duplicate_tree();
        let root = dir.path();
        let hashes = hashes_of(&[
            &root.join("a.txt"),
            &root.join("b.txt"),
            &root.join("sub").join("c.txt"),
            &root.join("d.txt"),
            &root.join("e.txt"),
        ]);
        let mut state = PaneState::new(view);
        let mut pane = DuplicateFilesPane::new(None);

        state.regroup(&mut pane, &hashes);

        assert_eq!(
            state.conflict_map,
            HashMap::from([(
                same_hash(root),
                vec![
                    root.join("a.txt"),
                    root.join("b.txt"),
                    root.join("e.txt"),
                    root.join("sub").join("c.txt")
                ],
            )])
        );
    }

    #[test]
    fn files_not_hashed_yet_are_left_out_of_the_groups() {
        let (dir, view) = duplicate_tree();
        let root = dir.path();
        // sub/c.txt is queued but not hashed, e.txt was never requested, d.txt has a unique size.
        let mut hashes = hashes_of(&[&root.join("a.txt"), &root.join("b.txt")]);
        hashes.insert(root.join("sub").join("c.txt"), None);
        let mut state = PaneState::new(view);
        let mut pane = DuplicateFilesPane::new(None);

        state.regroup(&mut pane, &hashes);
        assert_eq!(
            state.conflict_map,
            HashMap::from([(
                same_hash(root),
                vec![root.join("a.txt"), root.join("b.txt")]
            )])
        );
    }

    #[test]
    fn regrouping_keeps_valid_keepers_only() {
        let (dir, view) = duplicate_tree();
        let root = dir.path();
        let hashes = hashes_of(&[&root.join("a.txt"), &root.join("b.txt")]);
        let mut state = PaneState::new(view);
        let mut pane = DuplicateFilesPane::new(None);
        state.regroup(&mut pane, &hashes);

        let hash = same_hash(root);
        state
            .conflict_map_resolved
            .insert(hash.clone(), root.join("b.txt"));
        state
            .conflict_map_resolved
            .insert("gone".to_string(), PathBuf::new());

        state.regroup(&mut pane, &hashes);

        assert_eq!(
            state.conflict_map_resolved,
            HashMap::from([(hash, root.join("b.txt"))])
        );
    }

    fn groups(groups: &[(&str, &[&str])]) -> HashMap<String, Vec<PathBuf>> {
        groups
            .iter()
            .map(|(hash, paths)| (hash.to_string(), paths.iter().map(PathBuf::from).collect()))
            .collect()
    }

    fn keepers(keepers: &[(&str, &str)]) -> HashMap<String, PathBuf> {
        keepers
            .iter()
            .map(|(hash, path)| (hash.to_string(), PathBuf::from(path)))
            .collect()
    }

    fn paths(paths: &[&str]) -> HashSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn pending_deletions_without_keepers_is_empty() {
        let conflict_map = groups(&[("h1", &["a", "b"]), ("h2", &["c", "d", "e"])]);

        assert!(pending_deletions(&conflict_map, &HashMap::new()).is_empty());
    }

    #[test]
    fn pending_deletions_marks_every_member_but_the_keeper() {
        let conflict_map = groups(&[("h1", &["a", "b", "c"]), ("h2", &["d", "e"])]);

        assert_eq!(
            pending_deletions(&conflict_map, &keepers(&[("h1", "b")])),
            paths(&["a", "c"])
        );
        assert_eq!(
            pending_deletions(&conflict_map, &keepers(&[("h1", "b"), ("h2", "e")])),
            paths(&["a", "c", "d"])
        );
    }

    #[test]
    fn pending_deletions_follow_a_changed_keeper() {
        let conflict_map = groups(&[("h", &["a", "b", "c"])]);
        let mut keepers = HashMap::new();

        set_keeper(&conflict_map, &mut keepers, "h", Some(Path::new("a")));
        assert_eq!(
            pending_deletions(&conflict_map, &keepers),
            paths(&["b", "c"])
        );

        set_keeper(&conflict_map, &mut keepers, "h", Some(Path::new("c")));
        assert_eq!(
            pending_deletions(&conflict_map, &keepers),
            paths(&["a", "b"])
        );

        set_keeper(&conflict_map, &mut keepers, "h", None);
        assert!(pending_deletions(&conflict_map, &keepers).is_empty());
    }

    #[test]
    fn pending_deletions_ignore_a_keeper_outside_its_group() {
        let conflict_map = groups(&[("h", &["a", "b"])]);

        assert!(pending_deletions(&conflict_map, &keepers(&[("h", "")])).is_empty());
        assert!(pending_deletions(&conflict_map, &keepers(&[("other", "a")])).is_empty());
    }

    #[test]
    fn set_keeper_only_stores_members_of_the_group() {
        let conflict_map = groups(&[("h", &["a", "b"])]);
        let mut keepers = keepers(&[("h", "a")]);

        set_keeper(&conflict_map, &mut keepers, "h", Some(Path::new("")));
        set_keeper(
            &conflict_map,
            &mut keepers,
            "h",
            Some(Path::new("elsewhere")),
        );
        set_keeper(&conflict_map, &mut keepers, "unknown", Some(Path::new("a")));
        assert_eq!(keepers, self::keepers(&[("h", "a")]));

        set_keeper(&conflict_map, &mut keepers, "h", Some(Path::new("b")));
        assert_eq!(keepers, self::keepers(&[("h", "b")]));
    }

    #[test]
    fn resolve_needs_a_group_with_a_keeper() {
        let conflict_map = groups(&[("h1", &["a", "b"]), ("h2", &["c", "d"])]);

        assert!(!can_resolve(&conflict_map, &HashMap::new()));
        assert!(!can_resolve(&conflict_map, &keepers(&[("gone", "a")])));
        assert!(!can_resolve(&conflict_map, &keepers(&[("h1", "")])));
        assert!(can_resolve(&conflict_map, &keepers(&[("h2", "d")])));
    }

    #[test]
    fn opening_a_different_folder_clears_conflicts_keepers_and_marks() {
        let (dir, view) = duplicate_tree();
        let root = dir.path();
        let mut state = PaneState::new(view);
        let mut pane = DuplicateFilesPane::new(None);
        state.hash_frames(&mut pane);
        let hash = same_hash(root);
        state
            .conflict_map_resolved
            .insert(hash.clone(), root.join("a.txt"));

        state.frame(&mut pane);
        assert_eq!(
            state.conflict_map_resolved,
            HashMap::from([(hash.clone(), root.join("a.txt"))])
        );
        assert!(!pending_deletions(&state.conflict_map, &state.conflict_map_resolved).is_empty());

        let (other_dir, other_view) = duplicate_tree();
        state.view_1 = Some(other_view);
        state.frame(&mut pane);

        assert!(state.conflict_map.is_empty());
        assert!(state.conflict_map_resolved.is_empty());
        assert!(pending_deletions(&state.conflict_map, &state.conflict_map_resolved).is_empty());

        state.hash_frames(&mut pane);
        assert_eq!(
            state.conflict_map[&hash],
            [
                other_dir.path().join("a.txt"),
                other_dir.path().join("b.txt"),
                other_dir.path().join("e.txt"),
                other_dir.path().join("sub").join("c.txt"),
            ]
        );
        assert!(state.conflict_map_resolved.is_empty());

        state.view_1 = None;
        state.frame(&mut pane);
        assert!(state.conflict_map.is_empty());
    }

    fn blacklist(text: &str) -> PathFilter {
        PathFilter {
            blacklist: PatternList::new(text),
            ..Default::default()
        }
    }

    fn tree_paths(
        pane: &mut DuplicateFilesPane,
        view: &FileSystemView,
        filter: &PathFilter,
        sort: TreeSort,
    ) -> Vec<String> {
        pane.tree_rows(view, filter, sort, DuplicatesFilter::default())
            .iter()
            .map(|row| row.rel_path.clone())
            .collect()
    }

    #[test]
    fn the_tree_rows_are_kept_until_a_rescan_or_a_filter_or_sort_change() {
        let (dir, view) = duplicate_tree();
        let root = dir.path();
        let mut pane = DuplicateFilesPane::new(None);
        let sort = TreeSort::default();
        let all = ["", "sub", "sub/c.txt", "a.txt", "b.txt", "d.txt", "e.txt"];

        assert_eq!(
            tree_paths(&mut pane, &view, &PathFilter::default(), sort),
            all
        );
        let built = pane
            .tree_rows(
                &view,
                &PathFilter::default(),
                sort,
                DuplicatesFilter::default(),
            )
            .as_ptr();
        assert_eq!(
            pane.tree_rows(
                &view,
                &PathFilter::default(),
                sort,
                DuplicatesFilter::default()
            )
            .as_ptr(),
            built
        );

        fs::write(root.join("f.txt"), "new").unwrap();
        let rescanned = FileSystemView::new(Arc::new(FileSystemModel::new(root).unwrap()));
        assert!(
            tree_paths(&mut pane, &rescanned, &PathFilter::default(), sort)
                .contains(&"f.txt".to_string())
        );

        assert_eq!(
            tree_paths(&mut pane, &rescanned, &blacklist("*.txt"), sort),
            ["", "sub"]
        );

        let flat = TreeSort { flat: true, ..sort };
        assert_eq!(
            tree_paths(&mut pane, &rescanned, &blacklist("a.txt, f.txt"), sort),
            ["", "sub", "sub/c.txt", "b.txt", "d.txt", "e.txt"]
        );
        assert_eq!(
            tree_paths(&mut pane, &rescanned, &blacklist("a.txt, f.txt"), flat),
            ["", "b.txt", "d.txt", "e.txt", "sub", "sub/c.txt"]
        );
    }

    const DUPLICATES_ONLY: DuplicatesFilter = DuplicatesFilter {
        duplicates_only: true,
        show_parents: true,
        hash_column: false,
    };

    fn duplicate_rows(
        pane: &mut DuplicateFilesPane,
        state: &PaneState,
        duplicates_filter: DuplicatesFilter,
    ) -> Vec<String> {
        let view = state.view_1.as_ref().unwrap();
        pane.tree_rows(view, &state.path_filter, state.tree_sort, duplicates_filter)
            .iter()
            .map(|row| row.rel_path.clone())
            .collect()
    }

    #[test]
    fn duplicates_only_hides_files_without_a_group_and_folders_without_a_member_below() {
        let (dir, _) = duplicate_tree();
        let root = dir.path();
        fs::create_dir(root.join("solo")).unwrap();
        fs::write(root.join("solo").join("u.txt"), "unique").unwrap();
        let view = FileSystemView::new(Arc::new(FileSystemModel::new(root).unwrap()));
        let hashes = hashes_of(&[
            &root.join("a.txt"),
            &root.join("b.txt"),
            &root.join("sub").join("c.txt"),
            &root.join("d.txt"),
            &root.join("e.txt"),
            &root.join("solo").join("u.txt"),
        ]);
        let mut state = PaneState::new(view);
        let mut pane = DuplicateFilesPane::new(None);
        state.regroup(&mut pane, &hashes);

        assert_eq!(
            duplicate_rows(&mut pane, &state, DuplicatesFilter::default()),
            [
                "",
                "solo",
                "solo/u.txt",
                "sub",
                "sub/c.txt",
                "a.txt",
                "b.txt",
                "d.txt",
                "e.txt"
            ]
        );
        assert_eq!(
            duplicate_rows(&mut pane, &state, DUPLICATES_ONLY),
            ["", "sub", "sub/c.txt", "a.txt", "b.txt", "e.txt"]
        );
        let without_parents = DuplicatesFilter {
            show_parents: false,
            ..DUPLICATES_ONLY
        };
        assert_eq!(
            duplicate_rows(&mut pane, &state, without_parents),
            ["", "a.txt", "b.txt", "sub/c.txt", "e.txt"]
        );
        state.tree_sort.flat = true;
        assert_eq!(
            duplicate_rows(&mut pane, &state, DUPLICATES_ONLY),
            ["", "a.txt", "b.txt", "e.txt", "sub", "sub/c.txt"]
        );
    }

    #[test]
    fn duplicates_only_rows_follow_the_group_members_as_hashes_arrive() {
        let (dir, view) = duplicate_tree();
        let root = dir.path();
        let mut state = PaneState::new(view);
        let mut pane = DuplicateFilesPane::new(None);
        state.regroup(&mut pane, &HashMap::new());
        assert_eq!(duplicate_rows(&mut pane, &state, DUPLICATES_ONLY), [""]);

        let hashes = hashes_of(&[&root.join("a.txt"), &root.join("b.txt")]);
        state.regroup(&mut pane, &hashes);
        assert_eq!(
            duplicate_rows(&mut pane, &state, DUPLICATES_ONLY),
            ["", "a.txt", "b.txt"]
        );

        let built = pane.tree_cache.as_ref().unwrap().id;
        state.regroup(&mut pane, &hashes);
        duplicate_rows(&mut pane, &state, DUPLICATES_ONLY);
        assert_eq!(pane.tree_cache.as_ref().unwrap().id, built);
    }

    #[test]
    fn without_parent_folders_next_group_does_not_expand_the_folder_of_a_listed_file() {
        let (dir, mut state, mut pane) = two_group_state();
        let sub = node(state.view_1.as_ref().unwrap(), dir.path().join("sub"));
        state.tree_sort = TreeSort::default().clicked(SortKey::State);
        state.duplicates_filter = DuplicatesFilter {
            show_parents: false,
            ..DUPLICATES_ONLY
        };

        state.actions = vec![FolderAction::NextGroup];
        assert!(state.with_ctx(|ctx| pane.apply_actions(ctx)));
        assert_eq!(pane.cursor, TreeCursor::at("sub/c.txt"));
        assert!(state.view_1.as_ref().unwrap().is_collapsed(sub));
    }

    #[test]
    fn opening_another_folder_clears_the_cursor_and_a_rescan_keeps_it() {
        let (dir, view) = duplicate_tree();
        let (_other_dir, other_view) = duplicate_tree();
        let mut pane = DuplicateFilesPane::new(None);
        pane.forget_cursor_of_other_root(Some(&view));
        pane.cursor = TreeCursor::at("a.txt");

        let rescanned = FileSystemView::new(Arc::new(FileSystemModel::new(dir.path()).unwrap()));
        pane.forget_cursor_of_other_root(Some(&rescanned));
        assert_eq!(pane.cursor, TreeCursor::at("a.txt"));

        pane.forget_cursor_of_other_root(Some(&other_view));
        assert_eq!(pane.cursor, TreeCursor::default());

        pane.cursor = TreeCursor::at("a.txt");
        pane.forget_cursor_of_other_root(None);
        assert_eq!(pane.cursor, TreeCursor::default());
    }

    fn git_content(model_root: &str) -> ContentSource {
        ContentSource::Git(GitContent {
            model_root: PathBuf::from(model_root),
            repo_root: PathBuf::from("repo"),
            dir: String::new(),
            commit: Some("abc".into()),
        })
    }

    fn depot_content(model_root: &str) -> ContentSource {
        ContentSource::Depot(DepotContent {
            model_root: PathBuf::from(model_root),
            depot_folder: "//depot/main".into(),
            revisions: DepotRevisions::Shelf(7),
        })
    }

    #[test]
    fn a_non_disk_root_on_either_side_yields_a_message_naming_it() {
        let disk = ContentSource::Disk;
        let git = git_content("repo@git:abc");
        let depot = depot_content("//depot/main");
        let disk_root = Path::new("C:/disk");

        assert_eq!(
            non_disk_root_message([Some((disk_root, &disk)), Some((disk_root, &disk))]),
            None
        );
        assert_eq!(
            non_disk_root_message([Some((disk_root, &disk)), None]),
            None
        );
        assert_eq!(non_disk_root_message([None, None]), None);

        let message =
            non_disk_root_message([Some((Path::new("repo@git:abc"), &git)), None]).unwrap();
        assert!(message.contains("Folder 1"), "{message}");
        assert!(message.contains("repo@git:abc"), "{message}");

        let message = non_disk_root_message([
            Some((disk_root, &disk)),
            Some((Path::new("//depot/main"), &depot)),
        ])
        .unwrap();
        assert!(message.contains("Folder 2"), "{message}");
        assert!(message.contains("//depot/main"), "{message}");
        assert!(!message.contains("Folder 1"), "{message}");

        let message = non_disk_root_message([
            Some((Path::new("repo@git:abc"), &git)),
            Some((Path::new("//depot/main"), &depot)),
        ])
        .unwrap();
        assert!(
            message.contains("Folder 1") && message.contains("Folder 2"),
            "{message}"
        );
    }

    fn requested(state: &PaneState) -> Vec<PathBuf> {
        let mut paths: Vec<_> = state.hash_service.snapshot().hashes.into_keys().collect();
        paths.sort();
        paths
    }

    #[test]
    fn only_visible_files_sharing_a_size_are_requested_from_the_hash_service() {
        let (dir, _) = duplicate_tree();
        let root = dir.path();
        fs::write(root.join("f.txt"), "diff").unwrap();
        fs::write(root.join("notes.md"), "same").unwrap();
        let view = FileSystemView::new(Arc::new(FileSystemModel::new(root).unwrap()));
        let mut state = PaneState::new(view);
        state.path_filter = PathFilter {
            blacklist: PatternList::new("b.txt"),
            whitelist: PatternList::new("*.txt"),
        };
        let mut pane = DuplicateFilesPane::new(None);

        state.with_ctx(|ctx| pane.hash_size_collisions(ctx));

        assert_eq!(
            requested(&state),
            [
                root.join("a.txt"),
                root.join("e.txt"),
                root.join("f.txt"),
                root.join("sub").join("c.txt"),
            ]
        );
    }

    #[test]
    fn cancel_drops_the_queued_hashes_and_does_not_restart_them() {
        let (_dir, view) = duplicate_tree();
        let mut state = PaneState::new(view);
        let mut pane = DuplicateFilesPane::new(None);
        state.with_ctx(|ctx| pane.hash_size_collisions(ctx));
        assert_eq!(state.hash_service.count_hash_queue(), 4);

        state.with_ctx(|ctx| pane.cancel_hashing(ctx));
        state.with_ctx(|ctx| pane.hash_size_collisions(ctx));

        assert_eq!(state.hash_service.count_hash_queue(), 0);
        assert!(requested(&state).is_empty());
        assert_eq!(pane.hash_progress(&state.hash_service), None);
    }

    #[test]
    fn clear_hashes_requests_the_collisions_again() {
        let (_dir, view) = duplicate_tree();
        let mut state = PaneState::new(view);
        let mut pane = DuplicateFilesPane::new(None);
        state.with_ctx(|ctx| pane.hash_size_collisions(ctx));

        state.with_ctx(|ctx| pane.clear_hashes(ctx));
        assert_eq!(state.hash_service.count_hash_queue(), 0);
        state.with_ctx(|ctx| pane.hash_size_collisions(ctx));

        assert_eq!(state.hash_service.count_hash_queue(), 4);
        assert_eq!(requested(&state).len(), 4);
    }

    #[test]
    fn a_rescan_or_a_filter_change_requests_the_new_collisions() {
        let (dir, view) = duplicate_tree();
        let root = dir.path();
        let mut state = PaneState::new(view);
        state.path_filter = blacklist("b.txt");
        let mut pane = DuplicateFilesPane::new(None);
        state.with_ctx(|ctx| pane.hash_size_collisions(ctx));
        assert_eq!(requested(&state).len(), 3);

        fs::write(root.join("g.txt"), "xx").unwrap();
        fs::write(root.join("h.txt"), "yy").unwrap();
        state.scan_1.request_rescan();
        state.finish_rescan();
        state.with_ctx(|ctx| pane.hash_size_collisions(ctx));
        assert_eq!(requested(&state).len(), 5);
        assert!(requested(&state).contains(&root.join("g.txt")));

        state.path_filter = PathFilter::default();
        state.with_ctx(|ctx| pane.hash_size_collisions(ctx));
        assert_eq!(requested(&state).len(), 6);
        assert!(requested(&state).contains(&root.join("b.txt")));
    }

    #[test]
    fn the_progress_counts_the_files_hashed_and_queued_since_hashing_was_idle() {
        let (dir, view) = duplicate_tree();
        let root = dir.path();
        let mut state = PaneState::new(view);
        let mut pane = DuplicateFilesPane::new(None);
        assert_eq!(pane.hash_progress(&state.hash_service), None);

        state.with_ctx(|ctx| pane.hash_size_collisions(ctx));
        assert_eq!(
            pane.hash_progress(&state.hash_service),
            Some(HashProgress {
                hashed: 0,
                queued: 4
            })
        );

        state.hash_service.resize_workers(1);
        let deadline = Instant::now() + Duration::from_secs(10);
        while pane.hash_progress(&state.hash_service).is_some() {
            assert!(Instant::now() < deadline, "hashing did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        state.hash_service.resize_workers(0);

        fs::write(root.join("g.txt"), "xx").unwrap();
        fs::write(root.join("h.txt"), "yy").unwrap();
        state.scan_1.request_rescan();
        state.finish_rescan();
        state.with_ctx(|ctx| pane.hash_size_collisions(ctx));
        assert_eq!(
            pane.hash_progress(&state.hash_service),
            Some(HashProgress {
                hashed: 0,
                queued: 2
            })
        );
    }

    #[test]
    fn a_hidden_file_is_left_out_of_the_groups_even_when_hashed() {
        let dir = tempdir().unwrap();
        let (kept, hidden) = (dir.path().join("a.txt"), dir.path().join("a.obj"));
        fs::write(&kept, "same").unwrap();
        fs::write(&hidden, "same").unwrap();
        let view = FileSystemView::new(Arc::new(FileSystemModel::new(dir.path()).unwrap()));
        let hashes = hashes_of(&[&kept, &hidden]);
        let mut state = PaneState::new(view);
        let mut pane = DuplicateFilesPane::new(None);

        state.regroup(&mut pane, &hashes);
        assert_eq!(
            state.conflict_map.len(),
            1,
            "without a filter they are duplicates"
        );

        state.path_filter = blacklist("*.obj");
        state.regroup(&mut pane, &hashes);

        assert!(state.conflict_map.is_empty());
    }

    #[test]
    fn changing_the_filter_drops_the_keepers_and_marks_that_no_longer_apply() {
        let (dir, view) = duplicate_tree();
        let root = dir.path();
        let (a, b, c, e) = (
            root.join("a.txt"),
            root.join("b.txt"),
            root.join("sub").join("c.txt"),
            root.join("e.txt"),
        );
        let mut state = PaneState::new(view);
        let mut pane = DuplicateFilesPane::new(None);
        state.path_filter = blacklist("*.obj");
        state.hash_frames(&mut pane);
        let hash = same_hash(root);
        set_keeper(
            &state.conflict_map,
            &mut state.conflict_map_resolved,
            &hash,
            Some(&a),
        );
        let pending = |state: &PaneState| {
            pending_deletions(&state.conflict_map, &state.conflict_map_resolved)
        };

        state.frame(&mut pane);
        assert_eq!(
            pending(&state),
            HashSet::from([b.clone(), c.clone(), e.clone()])
        );

        // b.txt, which a resolve would recycle, is hidden now.
        state.path_filter = blacklist("*.obj, b.txt");
        state.frame(&mut pane);
        assert_eq!(
            state.conflict_map_resolved,
            HashMap::from([(hash.clone(), a.clone())])
        );
        assert_eq!(pending(&state), HashSet::from([c.clone(), e.clone()]));

        state.path_filter = blacklist("*.obj, a.txt, b.txt");
        state.frame(&mut pane);
        assert_eq!(
            state.conflict_map,
            HashMap::from([(hash.clone(), vec![e, c])])
        );
        assert!(state.conflict_map_resolved.is_empty());
        assert!(pending(&state).is_empty());

        state.path_filter = blacklist("*.txt");
        state.frame(&mut pane);
        assert!(state.conflict_map.is_empty());
    }

    #[test]
    fn groups_appear_once_the_collisions_are_hashed_without_pressing_diff() {
        let (dir, view) = duplicate_tree();
        let root = dir.path();
        let mut state = PaneState::new(view);
        let mut pane = DuplicateFilesPane::new(None);

        state.frame(&mut pane);
        assert!(state.conflict_map.is_empty());
        state.hash_frames(&mut pane);

        assert_eq!(
            state.conflict_map,
            HashMap::from([(
                same_hash(root),
                vec![
                    root.join("a.txt"),
                    root.join("b.txt"),
                    root.join("e.txt"),
                    root.join("sub").join("c.txt")
                ],
            )])
        );
    }

    fn node(view: &FileSystemView, path: PathBuf) -> FsNodeId {
        view.file_system.find_path(path).unwrap()
    }

    #[test]
    fn group_cells_show_the_group_keeper_and_marks_and_folders_count_their_duplicates() {
        let (dir, _) = duplicate_tree();
        let root = dir.path();
        fs::write(root.join("sub").join("x.txt"), "xx").unwrap();
        fs::write(root.join("y.txt"), "xx").unwrap();
        fs::write(root.join("z.txt"), "zzzz").unwrap();
        let view = FileSystemView::new(Arc::new(FileSystemModel::new(root).unwrap()));
        let all: Vec<PathBuf> = view
            .file_system
            .iter_files()
            .map(|id| view.file_system.path(id))
            .collect();
        let hashes = hashes_of(&all.iter().map(PathBuf::as_path).collect::<Vec<_>>());
        let groups = duplicate_groups(&view, &PathFilter::default(), &hashes);
        let same = same_hash(root);
        let keepers = HashMap::from([(same.clone(), root.join("a.txt"))]);
        let pending = HashSet::from([
            root.join("b.txt"),
            root.join("e.txt"),
            root.join("sub").join("c.txt"),
        ]);
        let cell = |path: PathBuf| groups.cell(node(&view, path), &keepers, &pending);
        let file = |path: PathBuf| match cell(path) {
            Some(GroupCell::File {
                id,
                group,
                path,
                keeper,
                pending,
            }) => (id, group.hash.clone(), path.to_path_buf(), keeper, pending),
            other => panic!("not a file cell: {other:?}"),
        };

        let (same_id, hash, path, keeper, pending_a) = file(root.join("a.txt"));
        assert_eq!(
            (hash, path, keeper, pending_a),
            (same.clone(), root.join("a.txt"), true, false)
        );
        let (id, _, _, keeper, pending_b) = file(root.join("b.txt"));
        assert_eq!((id, keeper, pending_b), (same_id, false, true));
        let (xx_id, hash, _, keeper, pending_x) = file(root.join("sub").join("x.txt"));
        assert_ne!(hash, same);
        assert_eq!((keeper, pending_x), (false, false));
        assert_eq!(file(root.join("y.txt")).0, xx_id);

        let same_members = [
            root.join("a.txt"),
            root.join("b.txt"),
            root.join("e.txt"),
            root.join("sub").join("c.txt"),
        ];
        let xx_members = [root.join("y.txt"), root.join("sub").join("x.txt")];
        let first_in_tree = view
            .file_system
            .iter_tree()
            .map(|(id, _, _)| view.file_system.path(id))
            .find(|path| same_members.contains(path) || xx_members.contains(path))
            .unwrap();
        let expected = if same_members.contains(&first_in_tree) {
            (1, 2)
        } else {
            (2, 1)
        };
        assert_eq!((same_id, xx_id), expected);

        assert_eq!(cell(root.join("d.txt")), None);
        assert_eq!(cell(root.join("z.txt")), None);
        assert_eq!(
            cell(root.join("sub")),
            Some(GroupCell::Folder { duplicates: 2 })
        );
        assert_eq!(
            cell(root.to_path_buf()),
            Some(GroupCell::Folder { duplicates: 6 })
        );

        let hidden_a = duplicate_groups(&view, &blacklist("a.txt"), &hashes);
        assert_eq!(
            hidden_a.cell(node(&view, root.join("a.txt")), &keepers, &pending),
            None
        );
        assert_eq!(
            hidden_a.cell(node(&view, root.to_path_buf()), &keepers, &pending),
            Some(GroupCell::Folder { duplicates: 5 })
        );
    }

    #[test]
    fn the_member_tooltip_lists_the_other_members_and_marks_the_keeper() {
        let root = Path::new("root");
        let group = DuplicateGroup {
            size: 4,
            hash: "h".to_string(),
            paths: vec![
                root.join("a.txt"),
                root.join("b.txt"),
                root.join("sub").join("c.txt"),
            ],
        };
        let keepers = HashMap::from([("h".to_string(), root.join("a.txt"))]);

        assert_eq!(
            member_tooltip(&group, &root.join("b.txt"), &keepers, root),
            format!(
                "2 other copies:\na.txt (keep)\n{}",
                Path::new("sub").join("c.txt").display()
            )
        );
        assert_eq!(
            member_tooltip(
                &group,
                &root.join("a.txt"),
                &HashMap::new(),
                &root.join("sub")
            ),
            format!("2 other copies:\n{}\nc.txt", root.join("b.txt").display())
        );

        let many = DuplicateGroup {
            paths: (0..35).map(|i| root.join(format!("{i}.txt"))).collect(),
            ..group
        };
        let text = member_tooltip(&many, &root.join("0.txt"), &HashMap::new(), root);
        assert!(text.starts_with("34 other copies:\n1.txt\n"), "{text}");
        assert!(text.ends_with("\n30.txt\n… 4 more"), "{text}");
    }

    /// duplicate_tree plus x.txt and y.txt, a second group, with sub collapsed.
    fn two_group_state() -> (TempDir, PaneState, DuplicateFilesPane) {
        let (dir, _) = duplicate_tree();
        let root = dir.path();
        fs::write(root.join("x.txt"), "xx").unwrap();
        fs::write(root.join("y.txt"), "xx").unwrap();
        let mut view = FileSystemView::new(Arc::new(FileSystemModel::new(root).unwrap()));
        let sub = node(&view, root.join("sub"));
        view.collapsed.insert(sub, true);
        let all: Vec<PathBuf> = [
            "a.txt",
            "b.txt",
            "sub/c.txt",
            "d.txt",
            "e.txt",
            "x.txt",
            "y.txt",
        ]
        .iter()
        .map(|name| root.join(name))
        .collect();
        let hashes = hashes_of(&all.iter().map(PathBuf::as_path).collect::<Vec<_>>());
        let mut state = PaneState::new(view);
        let mut pane = DuplicateFilesPane::new(None);
        state.with_ctx(|ctx| pane.regroup(ctx, &hashes));
        (dir, state, pane)
    }

    fn jump(
        state: &PaneState,
        pane: &mut DuplicateFilesPane,
        from: Option<&str>,
        forward: bool,
    ) -> Option<(String, Vec<CursorRequest>)> {
        let view = state.view_1.as_ref().unwrap();
        let cursor = from.map_or_else(TreeCursor::default, TreeCursor::at);
        pane.tree_rows(
            view,
            &state.path_filter,
            state.tree_sort,
            state.duplicates_filter,
        );
        let rows = &pane.tree_cache.as_ref().unwrap().rows;
        let groups = pane.groups.as_ref().unwrap();
        group_jump(&cursor, view, rows, groups, false, forward)
            .map(|(cursor, expand)| (cursor.rel_path().unwrap().to_string(), expand))
    }

    #[test]
    fn next_and_previous_group_land_on_the_first_member_and_expand_its_folders() {
        let (_dir, state, mut pane) = two_group_state();
        let expand_sub = vec![CursorRequest::Expand("sub".to_string())];
        let mut to = |from, forward| jump(&state, &mut pane, from, forward);

        assert_eq!(
            to(None, true),
            Some(("sub/c.txt".into(), expand_sub.clone()))
        );
        assert_eq!(to(Some("sub/c.txt"), true), Some(("x.txt".into(), vec![])));
        assert_eq!(to(Some("a.txt"), true), Some(("x.txt".into(), vec![])));
        assert_eq!(to(Some("x.txt"), true), None);
        assert_eq!(to(Some("y.txt"), true), None);

        assert_eq!(to(None, false), Some(("x.txt".into(), vec![])));
        assert_eq!(to(Some("y.txt"), false), Some(("x.txt".into(), vec![])));
        assert_eq!(
            to(Some("x.txt"), false),
            Some(("sub/c.txt".into(), expand_sub.clone()))
        );
        assert_eq!(
            to(Some("e.txt"), false),
            Some(("sub/c.txt".into(), expand_sub))
        );
        assert_eq!(to(Some("sub/c.txt"), false), None);
    }

    #[test]
    fn the_group_keys_move_the_cursor_and_expand_and_ctrl_k_keeps_the_cursors_file() {
        let (dir, mut state, mut pane) = two_group_state();
        let root = dir.path();
        let sub = node(state.view_1.as_ref().unwrap(), root.join("sub"));

        state.actions = vec![FolderAction::NextGroup];
        assert!(state.with_ctx(|ctx| pane.apply_actions(ctx)));
        assert_eq!(pane.cursor, TreeCursor::at("sub/c.txt"));
        assert!(!state.view_1.as_ref().unwrap().is_collapsed(sub));

        state.actions = vec![FolderAction::KeepThis, FolderAction::NextGroup];
        assert!(state.with_ctx(|ctx| pane.apply_actions(ctx)));
        assert_eq!(pane.cursor, TreeCursor::at("x.txt"));
        assert_eq!(
            state.conflict_map_resolved,
            HashMap::from([(same_hash(root), root.join("sub").join("c.txt"))])
        );

        state.actions = vec![FolderAction::NextGroup];
        assert!(!state.with_ctx(|ctx| pane.apply_actions(ctx)));
        assert_eq!(pane.cursor, TreeCursor::at("x.txt"));
    }

    #[test]
    fn keep_this_sets_the_keeper_of_a_member_and_again_clears_it() {
        let (dir, mut state, pane) = two_group_state();
        let root = dir.path();
        let view = state.view_1.as_ref().unwrap();
        let [a, b, d, sub] =
            ["a.txt", "b.txt", "d.txt", "sub"].map(|name| node(view, root.join(name)));
        let groups = pane.groups.as_ref().unwrap();
        let item = |state: &PaneState, id| keep_item(groups, &state.conflict_map_resolved, id);
        let toggle = |state: &mut PaneState, id| {
            toggle_keep(
                groups,
                &state.conflict_map,
                &mut state.conflict_map_resolved,
                id,
            )
        };
        let hash = same_hash(root);

        assert_eq!(item(&state, a), Some(KeepItem::Keep));
        assert_eq!(item(&state, d), None);
        assert_eq!(item(&state, sub), None);

        toggle(&mut state, b);
        assert_eq!(
            state.conflict_map_resolved,
            HashMap::from([(hash.clone(), root.join("b.txt"))])
        );
        assert_eq!(item(&state, b), Some(KeepItem::DontKeep));
        assert_eq!(item(&state, a), Some(KeepItem::Keep));

        toggle(&mut state, a);
        assert_eq!(
            state.conflict_map_resolved,
            HashMap::from([(hash.clone(), root.join("a.txt"))])
        );
        toggle(&mut state, d);
        toggle(&mut state, sub);
        assert_eq!(
            state.conflict_map_resolved,
            HashMap::from([(hash, root.join("a.txt"))])
        );

        toggle(&mut state, a);
        assert!(state.conflict_map_resolved.is_empty());
    }

    #[test]
    fn the_resolve_preview_lists_what_is_kept_and_removed_and_counts_skipped_groups() {
        let conflict_map = groups(&[
            ("h1", &["a", "b", "c"]),
            ("h2", &["d", "e"]),
            ("h3", &["f", "g"]),
        ]);

        assert_eq!(
            resolve_preview(&conflict_map, &keepers(&[("h1", "b"), ("h3", "elsewhere")])),
            ResolvePreview {
                kept: vec![PathBuf::from("b")],
                removed: vec![PathBuf::from("a"), PathBuf::from("c")],
                skipped: 2,
            }
        );
    }

    /// Removes files for real, except `failing`, whose removal fails and leaves it in place.
    fn remove_except(failing: &Path) -> impl FnMut(&Path) -> io::Result<()> + '_ {
        move |path| {
            if path == failing {
                Err(io::Error::other("file is in use"))
            } else {
                fs::remove_file(path)
            }
        }
    }

    #[test]
    fn resolve_removes_non_keepers_and_a_later_diff_no_longer_sees_them() {
        let (dir, view) = duplicate_tree();
        let root = dir.path();
        let a = root.join("a.txt");
        let b = root.join("b.txt");
        let c = root.join("sub").join("c.txt");
        let hashes = hashes_of(&[&a, &b, &c, &root.join("d.txt")]);
        let mut state = PaneState::new(view);
        for path in [&a, &b, &c] {
            state.hash_service.request(path);
        }
        let mut pane = DuplicateFilesPane::new(None);
        state.regroup(&mut pane, &hashes);
        let hash = same_hash(root);
        set_keeper(
            &state.conflict_map,
            &mut state.conflict_map_resolved,
            &hash,
            Some(&a),
        );

        state.with_ctx(|ctx| pane.resolve_selected(ctx, remove_except(&c)));
        state.finish_rescan();

        let summary = pane
            .resolution_summary
            .as_ref()
            .expect("resolve leaves a summary to show");
        assert_eq!(summary.removed, vec![b.clone()]);
        assert_eq!(
            summary.failed,
            vec![(c.clone(), "file is in use".to_string())]
        );
        assert!(a.exists());
        assert!(!b.exists());
        assert!(c.exists());

        assert!(state.conflict_map.is_empty());
        assert!(state.conflict_map_resolved.is_empty());

        let known_hashes = state.hash_service.snapshot().hashes;
        assert!(!known_hashes.contains_key(&b));
        assert!(known_hashes.contains_key(&a));
        assert!(
            known_hashes.contains_key(&c),
            "a failed file keeps its hash"
        );

        let view = state.view_1.as_ref().unwrap();
        assert_eq!(view.file_system.find_path(&b), None);
        assert!(view.file_system.find_path(&c).is_some());

        // b's stale hash is still passed in: only the tree decides what is diffed.
        state.regroup(&mut pane, &hashes);
        assert_eq!(state.conflict_map, HashMap::from([(hash, vec![a, c])]));
    }

    #[test]
    fn resolve_keeps_the_collapsed_state_of_the_rescanned_tree() {
        let (dir, view) = duplicate_tree();
        let root = dir.path();
        let a = root.join("a.txt");
        let hashes = hashes_of(&[&a, &root.join("b.txt")]);
        let mut state = PaneState::new(view);
        let view = state.view_1.as_mut().unwrap();
        let sub_id = view.file_system.find_path(root.join("sub")).unwrap();
        view.collapsed.insert(sub_id, true);
        // Rows of the two-folder diff hold node ids of the old tree.
        state.visible_rows = Some(Vec::new());
        let mut pane = DuplicateFilesPane::new(None);
        state.regroup(&mut pane, &hashes);
        set_keeper(
            &state.conflict_map,
            &mut state.conflict_map_resolved,
            &same_hash(root),
            Some(&a),
        );

        state.with_ctx(|ctx| pane.resolve_selected(ctx, remove_except(Path::new(""))));
        state.finish_rescan();

        assert!(state.visible_rows.is_none());
        let view = state.view_1.as_ref().unwrap();
        let id_of = |path: PathBuf| view.file_system.find_path(path).unwrap();
        assert_eq!(view.file_system.find_path(root.join("b.txt")), None);
        assert!(view.is_collapsed(id_of(root.join("sub"))));
        assert!(!view.is_collapsed(id_of(root.to_path_buf())));
    }
}
