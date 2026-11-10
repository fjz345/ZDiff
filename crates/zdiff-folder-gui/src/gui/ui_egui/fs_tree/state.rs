use super::*;

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
    pub folder_likeness: Option<FolderLikeness>,
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
    pub(super) split: Split,
    pub(super) offsets: [f32; 2],
    pub(super) widths: ContentWidths,
    pub(super) anchor: Option<ScrollAnchor>,
}

pub(super) struct ScrollAnchor {
    generation: u64,
    top: usize,
    rel_path: String,
    below_top: f32,
}

impl TreeTable {
    /// The offset that brings back the old top row, or its nearest drawn ancestor once it is gone.
    pub(super) fn anchored_offset(
        &self,
        generation: u64,
        rows: &[VisibleRowTwoFolderDiff],
        drawn: &[usize],
        pitch: f32,
    ) -> Option<f32> {
        let anchor = self
            .anchor
            .as_ref()
            .filter(|anchor| anchor.generation != generation)?;
        let is_self_or_ancestor = |rel_path: &str| {
            anchor
                .rel_path
                .strip_prefix(rel_path)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        };
        let (top, rel_path) = drawn
            .iter()
            .map(|&i| rows[i].rel_path.as_str())
            .enumerate()
            .filter(|&(_, rel_path)| is_self_or_ancestor(rel_path))
            .max_by_key(|&(_, rel_path)| rel_path.len())?;
        let below_top = if rel_path == anchor.rel_path {
            anchor.below_top
        } else {
            0.0
        };
        (top != anchor.top).then_some(top as f32 * pitch + below_top)
    }

    pub(super) fn set_anchor(
        &mut self,
        generation: u64,
        scroll_offset: f32,
        rows: &[VisibleRowTwoFolderDiff],
        drawn: &[usize],
        pitch: f32,
    ) {
        let Some(last) = drawn.len().checked_sub(1).filter(|_| scroll_offset > 0.0) else {
            self.anchor = None;
            return;
        };
        // A restored offset is a whole number of rows; float error must not pick the row above.
        let top = (((scroll_offset + 0.01) / pitch) as usize).min(last);
        let below_top = scroll_offset - top as f32 * pitch;
        match &mut self.anchor {
            Some(anchor) if (anchor.generation, anchor.top) == (generation, top) => {
                anchor.below_top = below_top;
            }
            _ => {
                self.anchor = Some(ScrollAnchor {
                    generation,
                    top,
                    rel_path: rows[drawn[top]].rel_path.clone(),
                    below_top,
                });
            }
        }
    }
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

    // Expand Diffs Only also marks files collapsed; only folders below the root count.
    pub fn is_anything_collapsed(&self) -> bool {
        let root = self.file_system.get_root_node_id();
        self.collapsed.iter().any(|(&id, &collapsed)| {
            collapsed
                && id != root
                && self
                    .file_system
                    .get_node(id)
                    .is_some_and(|node| node.is_dir())
        })
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
        mut blueprint: impl FnMut(&dyn Fn() -> PathBuf, FileMeta) -> Option<bool>,
    ) -> io::Result<Vec<VisibleRowTwoFolderDiff>> {
        let mut entries_map: BTreeMap<
            String,
            (
                Option<(FsNodeId, &FsNode, FsNodeDepth)>,
                Option<(FsNodeId, &FsNode, FsNodeDepth)>,
            ),
        > = BTreeMap::new();

        // Filtered entries never enter the map, so folder states only see visible entries.
        if let Some(view) = file_system_1 {
            let hidden = filtered_out(view, filter);
            let mut rels = rel_paths(view);
            for (id, node, depth) in view.file_system.iter_tree() {
                if hidden[id] {
                    continue;
                }
                let rel = std::mem::take(&mut rels[id]);
                entries_map.insert(rel, (Some((id, node, depth)), None));
            }
        }
        if let Some(view) = file_system_2 {
            let hidden = filtered_out(view, filter);
            let mut rels = rel_paths(view);
            for (id, node, depth) in view.file_system.iter_tree() {
                if hidden[id] {
                    continue;
                }
                let rel = std::mem::take(&mut rels[id]);
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

        let mut pair_likeness: HashMap<&str, LikenessSum> = HashMap::new();
        let file_states: HashMap<&str, DiffState> = entries_map
            .iter()
            .filter_map(|(rel_path, (left, right))| {
                let ((l_id, l_node, _), (r_id, r_node, _)) = (left.as_ref()?, right.as_ref()?);
                if l_node.is_dir() {
                    return None;
                }
                let (state, likeness) = file_diff_state(
                    rel_path,
                    (*l_id, l_node),
                    (*r_id, r_node),
                    tolerance,
                    &mut pair_state,
                );
                if let Some(likeness) = likeness {
                    pair_likeness.insert(rel_path.as_str(), likeness);
                }
                Some((rel_path.as_str(), state))
            })
            .collect();
        let descendant_states = descendant_states(&entries_map, &file_states);
        let descendant_likeness = descendant_likeness(&entries_map, &pair_likeness);

        for (rel_path, (left, right)) in &entries_map {
            let depth = left.map(|l| l.2).or(right.map(|r| r.2)).unwrap_or(0);

            let is_dir = left
                .map(|l| l.1.is_dir())
                .or(right.map(|r| r.1.is_dir()))
                .unwrap_or(false);

            let diff_state = match (left, right) {
                (Some((l_id, l_node, _)), Some((r_id, _, _))) => {
                    if l_node.is_dir() {
                        let below = descendant_states.get(rel_path.as_str()).cloned();
                        aggregate_folder_state(*l_id, *r_id, below)
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
            for (i, (side, view)) in [(left, file_system_1), (right, file_system_2)]
                .into_iter()
                .enumerate()
            {
                if file_kinds[i] == Some(FileKind::UnrealAsset)
                    && let Some((id, node, _)) = side
                    && let Some(view) = view
                    && let Some(modified) = node.modified
                {
                    let meta = FileMeta {
                        size: node.size,
                        modified,
                    };
                    let path = || view.file_system.path(*id);
                    blueprints[i] = blueprint(&path, meta).unwrap_or(false);
                }
            }

            let both_dirs = [left, right]
                .iter()
                .all(|side| side.is_some_and(|(_, node, _)| node.is_dir()));
            let folder_likeness = both_dirs
                .then(|| descendant_likeness.get(rel_path.as_str()))
                .flatten()
                .and_then(|sum| sum.folder_likeness());

            out_rows.push(VisibleRowTwoFolderDiff {
                rel_path: rel_path.clone(),
                is_dir,
                depth,
                diff_state,
                file_kinds,
                blueprints,
                folder_likeness,
            });
        }

        Ok(out_rows)
    }
}

/// Per node id of the view's model, true when the filter hides the node: not kept by the filter,
/// or inside a hidden folder. The root is never hidden. Folders the whitelist leaves empty are
/// pruned later, across both sides.
pub(super) fn filtered_out(view: &FileSystemView, filter: &PathFilter) -> Vec<bool> {
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
        hidden[id] = hidden[parent] || !filter.keeps(node.name(), node.is_dir());
    }
    hidden
}

/// Per node id of the view's model, the "/"-joined path relative to the root ("" for the root).
pub(super) fn rel_paths(view: &FileSystemView) -> Vec<String> {
    let model = &view.file_system;
    let mut rels = vec![String::new(); model.total_files_and_folders()];
    // Pre-order, so a parent is built before its children.
    for (id, node, _) in model.iter_tree() {
        let Some(parent) = node.parent else {
            continue;
        };
        let rel = &rels[parent];
        rels[id] = if rel.is_empty() {
            node.name().to_owned()
        } else {
            format!("{rel}/{}", node.name())
        };
    }
    rels
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

pub(super) fn fold_state(
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
pub(super) struct CursorRowsKey {
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
    guides: [TreeGuides; 2],
    generation: u64,
    resolved: Option<(u64, TreeCursor)>,
    selected: Option<(u64, TreeCursor, Option<usize>)>,
    #[cfg(test)]
    pub(super) builds: usize,
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
            let sides: [fn(&DiffState) -> Option<FsNodeId>; 2] =
                [DiffState::first, DiffState::second];
            self.guides = sides.map(|id| {
                let rows: Vec<(FsNodeDepth, bool)> = self
                    .drawn
                    .iter()
                    .map(|&i| (rows[i].depth, id(&rows[i].diff_state).is_some()))
                    .collect();
                tree_guides(&rows)
            });
            self.generation += 1;
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

    #[cfg(test)]
    pub fn cursor_rows(&self) -> &[CursorRow] {
        &self.cursor_rows
    }

    // A resolved cursor resolves to itself, so an unchanged one on unchanged rows is skipped.
    pub fn resolve(&mut self, cursor: &mut TreeCursor) {
        let unchanged = self
            .resolved
            .as_ref()
            .is_some_and(|(generation, resolved)| {
                *generation == self.generation && resolved == cursor
            });
        if !unchanged {
            *cursor = cursor.resolve(&self.cursor_rows);
            self.resolved = Some((self.generation, cursor.clone()));
        }
    }

    pub fn selected_row(&mut self, cursor: &TreeCursor) -> Option<usize> {
        if let Some((generation, at, selected)) = &self.selected
            && *generation == self.generation
            && at == cursor
        {
            return *selected;
        }
        let selected = self
            .drawn
            .iter()
            .position(|&i| cursor.is_at(&self.cursor_rows[i].rel_path));
        self.selected = Some((self.generation, cursor.clone(), selected));
        selected
    }

    pub fn drawn(&self) -> &[usize] {
        &self.drawn
    }

    pub fn guides(&self) -> &[TreeGuides; 2] {
        &self.guides
    }

    /// Changes whenever the drawn rows are rebuilt.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// Each side's widest drawn row. Names are measured once per model and font, so rebuilt rows
/// and folds only take a max over the cached widths.
#[derive(Default)]
pub struct ContentWidths {
    // Weak, like `TreeCompare`, so a new model can't reuse a dropped one's address.
    models: [Weak<FileSystemModel>; 2],
    font: Option<(egui::FontId, f32)>,
    folder_prefix: f32,
    // Per node id, NaN until measured.
    names: [Vec<f32>; 2],
    rel_paths: [Vec<f32>; 2],
    widest: Option<(u64, RowLayout, f32, [f32; 2])>,
    #[cfg(test)]
    pub(super) measured: usize,
}

impl ContentWidths {
    pub(super) fn get(
        &mut self,
        ui: &egui::Ui,
        views: [Option<&FileSystemView>; 2],
        rows: &[VisibleRowTwoFolderDiff],
        drawn: &[usize],
        generation: u64,
        layout: RowLayout,
    ) -> [f32; 2] {
        let font = (
            egui::TextStyle::Body.resolve(ui.style()),
            ui.ctx().pixels_per_point(),
        );
        let models =
            views.map(|view| view.map_or_else(Weak::new, |v| Arc::downgrade(&v.file_system)));
        let same_models = (0..2).all(|side| models[side].ptr_eq(&self.models[side]));
        if !same_models || self.font.as_ref() != Some(&font) {
            let unmeasured = |view: Option<&FileSystemView>| {
                vec![f32::NAN; view.map_or(0, |v| v.file_system.total_files_and_folders())]
            };
            self.names = views.map(unmeasured);
            self.rel_paths = views.map(unmeasured);
            self.folder_prefix =
                ui.fonts_mut(|fonts| text_width("📁 ", &mut |c| fonts.glyph_width(&font.0, c)));
            self.models = models;
            self.font = Some(font);
            self.widest = None;
        }
        let spacing = ui.spacing().item_spacing.x;
        if let Some((built, built_layout, built_spacing, widths)) = self.widest
            && (built, built_layout, built_spacing) == (generation, layout, spacing)
        {
            return widths;
        }

        let font = &self.font.as_ref().expect("set above").0;
        let mut widths = [0.0f32; 2];
        ui.fonts_mut(|fonts| {
            for &i in drawn {
                let entry = &rows[i];
                let ids = [entry.diff_state.first(), entry.diff_state.second()];
                for side in 0..2 {
                    let Some(node) = side_node(views[side], ids[side]) else {
                        continue;
                    };
                    let id = ids[side].expect("side_node found it");
                    let cached = if layout == RowLayout::RelPaths {
                        &mut self.rel_paths[side][id]
                    } else {
                        &mut self.names[side][id]
                    };
                    if cached.is_nan() {
                        let name = if layout == RowLayout::RelPaths {
                            entry.rel_path.as_str()
                        } else {
                            node.display_name()
                        };
                        *cached = text_width(name, &mut |c| fonts.glyph_width(font, c));
                        #[cfg(test)]
                        {
                            self.measured += 1;
                        }
                    }
                    let folder = if entry.is_dir {
                        self.folder_prefix
                    } else {
                        0.0
                    };
                    let width =
                        side_width(layout, entry.depth, entry.is_dir, folder + *cached, spacing);
                    widths[side] = widths[side].max(width);
                }
            }
        });
        self.widest = Some((generation, layout, spacing, widths));
        widths
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
    let (symbol, role) = match state {
        DiffState::Same(..) => ("=", FolderRole::Same),
        DiffState::Different(..) => ("≠", FolderRole::Different),
        DiffState::Similar(..) => ("≈", FolderRole::Similar),
        DiffState::OnlyInFirst(..) => ("−", FolderRole::OnlyInFirst),
        DiffState::OnlyInSecond(..) => ("+", FolderRole::OnlyInSecond),
        DiffState::Pending(..) => {
            return ui
                .add(egui::Spinner::new().size(ui.text_style_height(&egui::TextStyle::Body)))
                .on_hover_text("not compared");
        }
    };
    ui.label(RichText::new(symbol).color(role_color(ui.ctx(), role)))
}

pub(super) fn file_diff_state(
    rel_path: &str,
    left: (FsNodeId, &FsNode),
    right: (FsNodeId, &FsNode),
    tolerance: &Tolerance,
    pair_state: &mut impl FnMut(&str, [&FsNode; 2]) -> (Option<PairState>, Option<f64>),
) -> (DiffState, Option<LikenessSum>) {
    if right.1.is_dir() {
        let likeness = LikenessSum::one_sided(left.1.size);
        return (DiffState::Different(left.0, right.0), Some(likeness));
    }
    let (exact, likeness) = pair_state(rel_path, [left.1, right.1]);
    let sides = [left.1, right.1].map(|node| Some(SideMeta::of(node)));
    let similar = tolerance.is_similar(sides, exact, likeness);
    let sizes = [left.1.size, right.1.size];
    let likeness_sum = LikenessSum::pair(exact, likeness, similar, sizes);
    let state = match exact {
        _ if similar => DiffState::Similar(left.0, right.0),
        Some(PairState::Same) => DiffState::Same(left.0, right.0),
        Some(PairState::Different) => DiffState::Different(left.0, right.0),
        None => DiffState::Pending(left.0, right.0),
    };
    (state, likeness_sum)
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

pub(super) fn descendant_states<'a>(
    entries_map: &'a BTreeMap<
        String,
        (
            Option<(FsNodeId, &FsNode, FsNodeDepth)>,
            Option<(FsNodeId, &FsNode, FsNodeDepth)>,
        ),
    >,
    file_states: &HashMap<&str, DiffState>,
) -> HashMap<&'a str, DiffState> {
    let mut below: HashMap<&str, DiffState> = HashMap::new();
    // A path sorts before every path it prefixes, so in reverse its descendants come first.
    for (path, (left, right)) in entries_map.iter().rev() {
        let parent = match path.rsplit_once('/') {
            Some((parent, _)) => parent,
            None if path.is_empty() => continue,
            None => "",
        };
        let own = match (left, right) {
            (Some((li, _, _)), None) => Some(DiffState::OnlyInFirst(*li)),
            (None, Some((ri, _, _))) => Some(DiffState::OnlyInSecond(*ri)),
            (Some((_, ln, _)), Some(_)) if !ln.is_dir() => Some(file_states[path.as_str()].clone()),
            _ => None,
        };
        let states = [
            below.get(parent).cloned(),
            own,
            below.get(path.as_str()).cloned(),
        ];
        let joined = aggregate_folder_state(0, 0, states.into_iter().flatten());
        below.insert(parent, joined);
    }
    below
}

pub(super) fn descendant_likeness<'a>(
    entries_map: &'a BTreeMap<
        String,
        (
            Option<(FsNodeId, &FsNode, FsNodeDepth)>,
            Option<(FsNodeId, &FsNode, FsNodeDepth)>,
        ),
    >,
    pairs: &HashMap<&str, LikenessSum>,
) -> HashMap<&'a str, LikenessSum> {
    let file_size = |side: Option<(FsNodeId, &FsNode, FsNodeDepth)>| {
        side.filter(|(_, node, _)| !node.is_dir())
            .map(|(_, node, _)| node.size)
    };
    let mut below: HashMap<&str, LikenessSum> = HashMap::new();
    // As in descendant_states: in reverse, a folder's descendants come before it.
    for (path, (left, right)) in entries_map.iter().rev() {
        let parent = match path.rsplit_once('/') {
            Some((parent, _)) => parent,
            None if path.is_empty() => continue,
            None => "",
        };
        let own = match (left, right) {
            (Some((_, ln, _)), Some(_)) if !ln.is_dir() => pairs.get(path.as_str()).copied(),
            (Some(_), Some(_)) => file_size(*right).map(LikenessSum::one_sided),
            _ => file_size(*left)
                .or(file_size(*right))
                .map(LikenessSum::one_sided),
        };
        let mut sum = below.get(path.as_str()).copied().unwrap_or_default();
        if let Some(own) = own {
            sum.add(own);
        }
        below.entry(parent).or_default().add(sum);
    }
    below
}
