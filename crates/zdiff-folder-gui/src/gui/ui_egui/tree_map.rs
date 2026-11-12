use std::collections::HashMap;

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke, StrokeKind, Vec2, vec2,
};
use zcommon_ui::{likeness::percent, style_window::role_color};
use zdiff_folder::{external_diff_tool::DiffToolConfig, fs::FsNodeId};

use crate::ui_egui::{
    fs_tree::{
        DiffState, FileSystemView, FolderRole, RowsId, VisibleRowTwoFolderDiff,
        apply_cursor_request,
    },
    roots::Side,
    status_line::ToolMessage,
    tree_compare::TreeCompare,
    tree_cursor::{CursorRequest, TreeCursor},
    tree_likeness::FolderLikeness,
};

pub const ROOT: usize = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellState {
    Same,
    Similar,
    Different,
    OnlyInFirst,
    OnlyInSecond,
    Pending,
}

impl CellState {
    const ALL: [Self; 6] = [
        Self::Same,
        Self::Similar,
        Self::Different,
        Self::OnlyInFirst,
        Self::OnlyInSecond,
        Self::Pending,
    ];

    fn role(self) -> FolderRole {
        match self {
            Self::Same => FolderRole::Same,
            Self::Similar => FolderRole::Similar,
            Self::Different => FolderRole::Different,
            Self::OnlyInFirst => FolderRole::OnlyInFirst,
            Self::OnlyInSecond => FolderRole::OnlyInSecond,
            Self::Pending => FolderRole::DimmedText,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Same => "Same",
            Self::Similar => "Similar",
            Self::Different => "Different",
            Self::OnlyInFirst => "Only in Left",
            Self::OnlyInSecond => "Only in Right",
            Self::Pending => "Not compared yet",
        }
    }

    fn fill_strength(self) -> f32 {
        match self {
            Self::Same => 0.2,
            Self::Pending => 0.35,
            Self::Similar => 0.45,
            Self::Different | Self::OnlyInFirst | Self::OnlyInSecond => 0.6,
        }
    }

    fn name_prefix(self) -> &'static str {
        match self {
            Self::OnlyInFirst => "− ",
            Self::OnlyInSecond => "+ ",
            _ => "",
        }
    }

    fn of(state: &DiffState) -> Self {
        match state {
            DiffState::Same(..) => Self::Same,
            DiffState::Similar(..) => Self::Similar,
            DiffState::Different(..) => Self::Different,
            DiffState::OnlyInFirst(..) => Self::OnlyInFirst,
            DiffState::OnlyInSecond(..) => Self::OnlyInSecond,
            DiffState::Pending(..) => Self::Pending,
        }
    }

    fn merge(self, other: Self) -> Self {
        let rank = |state: Self| match state {
            Self::Same => 0,
            Self::Similar => 1,
            Self::Pending => 2,
            Self::Different | Self::OnlyInFirst | Self::OnlyInSecond => 3,
        };
        if self == other {
            self
        } else if rank(self).max(rank(other)) == 3 {
            Self::Different
        } else if rank(self) > rank(other) {
            self
        } else {
            other
        }
    }
}

#[derive(Debug, Clone)]
pub struct MapNode {
    path_row: usize,
    path_len: usize,
    pub row: Option<usize>,
    pub parent: usize,
    pub is_dir: bool,
    pub state: CellState,
    pub sizes: [u64; 2],
    pub weight: u64,
    pub files: u64,
    pub children: Vec<usize>,
}

impl MapNode {
    fn folder(path_row: usize, path_len: usize, parent: usize) -> Self {
        Self {
            path_row,
            path_len,
            row: None,
            parent,
            is_dir: true,
            state: CellState::Same,
            sizes: [0, 0],
            weight: 0,
            files: 0,
            children: Vec::new(),
        }
    }
}

/// Children sorted largest first; paths index into the rows it was built from.
#[derive(Debug, Clone)]
pub struct MapTree {
    pub nodes: Vec<MapNode>,
}

impl MapTree {
    pub fn build(
        rows: &[VisibleRowTwoFolderDiff],
        sizes: impl Fn(&VisibleRowTwoFolderDiff) -> [u64; 2],
    ) -> Self {
        let mut nodes = vec![MapNode::folder(0, 0, ROOT)];
        let mut index: HashMap<&str, usize> = HashMap::with_capacity(rows.len());
        index.insert("", ROOT);
        for (i, row) in rows.iter().enumerate() {
            let node = node_for(&mut nodes, &mut index, i, &row.rel_path);
            let node = &mut nodes[node];
            node.row = Some(i);
            node.is_dir = row.is_dir;
            node.state = CellState::of(&row.diff_state);
            if !row.is_dir {
                node.sizes = sizes(row);
            }
        }

        // A parent's index is below its children's, so a reverse walk finishes each child first.
        let mut merged: Vec<Option<CellState>> = vec![None; nodes.len()];
        for i in (0..nodes.len()).rev() {
            let node = &mut nodes[i];
            node.is_dir |= !node.children.is_empty();
            if !node.is_dir {
                node.weight = node.sizes[0].max(node.sizes[1]).max(1);
                node.files = 1;
            } else if node.children.is_empty() {
                node.weight = 1;
            }
            if node.row.is_none()
                && let Some(state) = merged[i]
            {
                node.state = state;
            }
            if i == ROOT {
                break;
            }
            let (parent, weight, sizes, files, state) =
                (node.parent, node.weight, node.sizes, node.files, node.state);
            let parent_node = &mut nodes[parent];
            parent_node.weight += weight;
            parent_node.sizes[0] += sizes[0];
            parent_node.sizes[1] += sizes[1];
            parent_node.files += files;
            merged[parent] = Some(merged[parent].map_or(state, |s| s.merge(state)));
        }

        let mut tree = Self { nodes };
        for i in 0..tree.nodes.len() {
            let mut children = std::mem::take(&mut tree.nodes[i].children);
            children.sort_by(|&a, &b| {
                let weight = |n: usize| tree.nodes[n].weight;
                weight(b)
                    .cmp(&weight(a))
                    .then_with(|| tree.name(rows, a).cmp(tree.name(rows, b)))
            });
            tree.nodes[i].children = children;
        }
        tree
    }

    pub fn path<'r>(&self, rows: &'r [VisibleRowTwoFolderDiff], node: usize) -> &'r str {
        let node = &self.nodes[node];
        if node.path_len == 0 {
            return "";
        }
        &rows[node.path_row].rel_path[..node.path_len]
    }

    pub fn name<'r>(&self, rows: &'r [VisibleRowTwoFolderDiff], node: usize) -> &'r str {
        let path = self.path(rows, node);
        path.rsplit_once('/').map_or(path, |(_, name)| name)
    }

    pub fn resolve(&self, rows: &[VisibleRowTwoFolderDiff], zoom: &str) -> usize {
        let mut node = ROOT;
        for part in zoom.split('/').filter(|part| !part.is_empty()) {
            let child = self.nodes[node]
                .children
                .iter()
                .copied()
                .find(|&child| self.nodes[child].is_dir && self.name(rows, child) == part);
            match child {
                Some(child) => node = child,
                None => break,
            }
        }
        node
    }

    pub fn ancestors(&self, node: usize) -> Vec<usize> {
        let mut chain = vec![node];
        let mut at = node;
        while at != ROOT {
            at = self.nodes[at].parent;
            chain.push(at);
        }
        chain.reverse();
        chain
    }
}

fn node_for<'r>(
    nodes: &mut Vec<MapNode>,
    index: &mut HashMap<&'r str, usize>,
    row: usize,
    path: &'r str,
) -> usize {
    if let Some(&node) = index.get(path) {
        return node;
    }
    let parent_path = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    let parent = node_for(nodes, index, row, parent_path);
    let node = nodes.len();
    nodes.push(MapNode::folder(row, path.len(), parent));
    nodes[parent].children.push(node);
    index.insert(path, node);
    node
}

/// Squarified treemap (Bruls, Huizing, van Wijk); squarest with the weights sorted largest first.
pub fn squarify(weights: &[f64], rect: Rect) -> Vec<Rect> {
    let mut rects = vec![Rect::from_min_max(rect.min, rect.min); weights.len()];
    let total: f64 = weights.iter().sum();
    let area = rect.width() as f64 * rect.height() as f64;
    if total <= 0.0 || area <= 0.0 {
        return rects;
    }
    let areas: Vec<f64> = weights.iter().map(|weight| weight * area / total).collect();
    let mut free = [rect.min.x, rect.min.y, rect.max.x, rect.max.y].map(f64::from);
    let mut start = 0;
    while start < areas.len() {
        let side = (free[2] - free[0]).min(free[3] - free[1]);
        let (mut sum, mut big, mut small) = (areas[start], areas[start], areas[start]);
        let mut worst = worst_aspect(sum, big, small, side);
        let mut end = start + 1;
        while end < areas.len() {
            let area = areas[end];
            let next = worst_aspect(sum + area, big.max(area), small.min(area), side);
            if next > worst {
                break;
            }
            (sum, big, small, worst) = (sum + area, big.max(area), small.min(area), next);
            end += 1;
        }
        let last = end == areas.len();
        place_row(
            &areas[start..end],
            sum,
            &mut free,
            last,
            &mut rects[start..end],
        );
        start = end;
    }
    rects
}

fn worst_aspect(sum: f64, big: f64, small: f64, side: f64) -> f64 {
    let (sum2, side2) = (sum * sum, side * side);
    (side2 * big / sum2).max(sum2 / (side2 * small))
}

/// The last row takes all that is left, so float error never leaves a gap.
fn place_row(areas: &[f64], sum: f64, free: &mut [f64; 4], last: bool, out: &mut [Rect]) {
    let [x0, y0, x1, y1] = *free;
    let column = x1 - x0 >= y1 - y0;
    let (length, depth) = if column {
        (y1 - y0, x1 - x0)
    } else {
        (x1 - x0, y1 - y0)
    };
    let thickness = if last || length <= 0.0 {
        depth
    } else {
        (sum / length).min(depth)
    };
    let (mut along, row_end) = if column { (y0, y1) } else { (x0, x1) };
    for (i, (&area, rect)) in areas.iter().zip(out.iter_mut()).enumerate() {
        let end = if i + 1 == areas.len() {
            row_end
        } else if sum > 0.0 {
            along + area / sum * length
        } else {
            along
        };
        let [ax, ay, bx, by] = if column {
            [x0, along, x0 + thickness, end]
        } else {
            [along, y0, end, y0 + thickness]
        };
        *rect = Rect::from_min_max(
            Pos2::new(ax as f32, ay as f32),
            Pos2::new(bx as f32, by as f32),
        );
        along = end;
    }
    if column {
        free[0] = x0 + thickness;
    } else {
        free[1] = y0 + thickness;
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutParams {
    pub header: f32,
    pub pad: f32,
    pub min_side: f32,
    pub visible: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    File,
    Folder {
        header: bool,
    },
    Closed,
    Rest {
        count: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapCell {
    pub node: usize,
    pub rect: Rect,
    pub kind: CellKind,
}

/// Cells in paint order: a folder before its entries.
pub fn layout(tree: &MapTree, root: usize, rect: Rect, params: LayoutParams) -> Vec<MapCell> {
    let mut cells = Vec::new();
    if tree.nodes[root].is_dir {
        layout_entries(tree, root, rect, params, &mut cells);
    } else {
        layout_node(tree, root, rect, params, &mut cells);
    }
    cells
}

fn shown_entries(tree: &MapTree, folder: usize, area: f32, params: LayoutParams) -> usize {
    let node = &tree.nodes[folder];
    let scale = area as f64 / node.weight as f64;
    let min_area = (params.min_side * params.min_side) as f64;
    node.children
        .iter()
        .take_while(|&&child| tree.nodes[child].weight as f64 * scale >= min_area)
        .count()
}

fn layout_entries(
    tree: &MapTree,
    folder: usize,
    rect: Rect,
    params: LayoutParams,
    cells: &mut Vec<MapCell>,
) {
    let children = &tree.nodes[folder].children;
    let shown = shown_entries(tree, folder, rect.area(), params);
    let mut weights: Vec<f64> = children[..shown]
        .iter()
        .map(|&child| tree.nodes[child].weight as f64)
        .collect();
    let rest = children.len() - shown;
    if rest > 0 {
        let rest_weight: u64 = children[shown..]
            .iter()
            .map(|&child| tree.nodes[child].weight)
            .sum();
        weights.push(rest_weight as f64);
    }
    let rects = squarify(&weights, rect);
    for (&child, &rect) in children[..shown].iter().zip(&rects) {
        layout_node(tree, child, rect, params, cells);
    }
    if rest > 0 && rects[shown].intersects(params.visible) {
        cells.push(MapCell {
            node: folder,
            rect: rects[shown],
            kind: CellKind::Rest { count: rest },
        });
    }
}

fn layout_node(
    tree: &MapTree,
    node: usize,
    rect: Rect,
    params: LayoutParams,
    cells: &mut Vec<MapCell>,
) {
    if !rect.intersects(params.visible) {
        return;
    }
    let cell = |kind| MapCell { node, rect, kind };
    if !tree.nodes[node].is_dir {
        cells.push(cell(CellKind::File));
        return;
    }
    let header = rect.height() >= params.header * 2.5 && rect.width() >= params.header * 2.5;
    let mut inner = rect.shrink(params.pad);
    if header {
        inner.min.y = rect.min.y + params.header;
    }
    let opens = inner.width() >= params.min_side
        && inner.height() >= params.min_side
        && shown_entries(tree, node, inner.area(), params) > 0;
    if !opens {
        cells.push(cell(CellKind::Closed));
        return;
    }
    cells.push(cell(CellKind::Folder { header }));
    layout_entries(tree, node, inner, params, cells);
}

pub fn cell_at(cells: &[MapCell], pos: Pos2) -> Option<usize> {
    cells.iter().rposition(|cell| cell.rect.contains(pos))
}

pub struct MapCtx<'a> {
    pub file_system_1_view: &'a mut Option<FileSystemView>,
    pub file_system_2_view: &'a mut Option<FileSystemView>,
    pub rows: &'a [VisibleRowTwoFolderDiff],
    pub rows_id: RowsId,
    pub compare: &'a TreeCompare,
    pub diff_tool_config: &'a DiffToolConfig,
    pub tool_message: &'a mut Option<ToolMessage>,
    pub cursor: &'a mut TreeCursor,
    pub keyboard_taken: bool,
}

#[derive(PartialEq)]
struct LayoutKey {
    rows: RowsId,
    root: usize,
    rect: Rect,
    params: LayoutParams,
}

fn mouse_back_claim_id() -> egui::Id {
    egui::Id::new("tree_map_mouse_back_claim")
}

// Last frame's claim: a zoomed-in map under the pointer zooms out on the back button instead.
pub fn take_mouse_back_claim(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.remove_temp::<bool>(mouse_back_claim_id()))
        .unwrap_or(false)
}

const MAX_SCALE: f32 = 256.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct MapView {
    scale: f32,
    pan: Vec2,
}

impl Default for MapView {
    fn default() -> Self {
        Self {
            scale: 1.0,
            pan: Vec2::ZERO,
        }
    }
}

impl MapView {
    fn is_fit(&self) -> bool {
        self.scale <= 1.0
    }

    fn virtual_rect(&self, viewport: Rect) -> Rect {
        Rect::from_min_size(viewport.min + self.pan, viewport.size() * self.scale)
    }

    fn clamped(self, viewport: Rect) -> Self {
        let pan = self
            .pan
            .clamp(viewport.size() * (1.0 - self.scale), Vec2::ZERO);
        Self { pan, ..self }
    }

    fn zoomed_about(self, pointer: Pos2, factor: f32, viewport: Rect) -> Self {
        let scale = (self.scale * factor).clamp(1.0, MAX_SCALE);
        let at = pointer - viewport.min;
        let pan = at - (at - self.pan) * (scale / self.scale);
        Self { scale, pan }.clamped(viewport)
    }
}

#[derive(Default)]
pub struct TreeMap {
    zoom: String,
    view: MapView,
    tree: Option<(RowsId, MapTree)>,
    cells: Option<(LayoutKey, Vec<MapCell>)>,
}

impl TreeMap {
    pub fn clear_zoom(&mut self) {
        self.zoom.clear();
        self.view = MapView::default();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: MapCtx) {
        let _scope = zcommon_ui::frame_stats::scope("Treemap");
        let MapCtx {
            file_system_1_view,
            file_system_2_view,
            rows,
            rows_id,
            compare,
            diff_tool_config,
            tool_message,
            cursor,
            keyboard_taken,
        } = ctx;
        if self
            .tree
            .as_ref()
            .is_none_or(|(built, _)| *built != rows_id)
        {
            let views = [file_system_1_view.as_ref(), file_system_2_view.as_ref()];
            let tree = MapTree::build(rows, |row| file_sizes(views, &row.diff_state));
            self.tree = Some((rows_id, tree));
        }
        let tree = &self.tree.as_ref().expect("built above").1;
        let mut root = tree.resolve(rows, &self.zoom);

        let mut zoom = None;
        let mut fit = false;
        ui.horizontal(|ui| {
            for (i, node) in tree.ancestors(root).into_iter().enumerate() {
                if i > 0 {
                    ui.weak("›");
                }
                let name = if node == ROOT {
                    "Root"
                } else {
                    tree.name(rows, node)
                };
                if ui.selectable_label(node == root, name).clicked() && node != root {
                    zoom = Some(node);
                }
            }
            let shown = &tree.nodes[root];
            let [left, right] = shown.sizes.map(size_text);
            ui.weak(format!("{} files, {left} | {right}", shown.files))
                .on_hover_text(
                    "Each box is a file sized by its larger side, colored by its diff state. \
                     Shows what the tree shows: filters and find apply.\n\
                     Click selects, double-click opens the diff, a folder click zooms in, \
                     Backspace or mouse back zooms out.\n\
                     Scroll zooms about the pointer and drag pans; middle-click or Fit \
                     shows the whole folder again.",
                );
            if !self.view.is_fit() {
                fit = ui
                    .small_button("Fit")
                    .on_hover_text("Show the whole folder (middle-click, Backspace)")
                    .clicked();
            }
        });
        if let Some(node) = zoom {
            root = node;
            fit = true;
        }

        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        if fit || response.middle_clicked() {
            self.view = MapView::default();
        }
        if response.dragged() {
            self.view.pan += response.drag_delta();
        }
        if let Some(pointer) = response.hover_pos() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = (scroll / 200.0).exp() * pinch;
            if factor != 1.0 {
                self.view = self.view.zoomed_about(pointer, factor, rect);
            }
        }
        self.view = self.view.clamped(rect);

        let line = ui.text_style_height(&egui::TextStyle::Small);
        let params = LayoutParams {
            header: line + 4.0,
            pad: 2.0,
            min_side: 4.0,
            visible: rect,
        };
        let virtual_rect = self.view.virtual_rect(rect);
        let key = LayoutKey {
            rows: rows_id,
            root,
            rect: virtual_rect,
            params,
        };
        if self.cells.as_ref().is_none_or(|(built, _)| *built != key) {
            self.cells = Some((key, layout(tree, root, virtual_rect, params)));
        }
        let cells = &self.cells.as_ref().expect("laid out above").1;

        let hovered = response.hover_pos().and_then(|pos| cell_at(cells, pos));
        let painter = ui.painter_at(rect);
        let look = Look::new(ui, params);
        painter.rect_filled(rect, 0.0, look.bg);
        if cells.is_empty() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Nothing to show",
                look.font.clone(),
                look.weak,
            );
        }
        for cell in cells {
            look.paint(&painter, tree, rows, cell);
        }
        let selected = cursor.rel_path().and_then(|at| {
            cells.iter().position(|cell| {
                !matches!(cell.kind, CellKind::Rest { .. }) && tree.path(rows, cell.node) == at
            })
        });
        if let Some(i) = selected {
            painter.rect_stroke(cells[i].rect, 0.0, look.selected, StrokeKind::Inside);
        }
        if let Some(i) = hovered {
            painter.rect_stroke(cells[i].rect, 0.0, look.hovered, StrokeKind::Inside);
        }

        let (mut select, mut open) = (None, None);
        let clicked = response
            .clicked()
            .then(|| response.interact_pointer_pos())
            .flatten()
            .and_then(|pos| cell_at(cells, pos));
        if let Some(i) = clicked {
            let cell = cells[i];
            // The first click of a double click already acted, maybe on a layout it zoomed away.
            if response.double_clicked() {
                let selected_file =
                    cell.kind == CellKind::File && cursor.is_at(tree.path(rows, cell.node));
                open = selected_file.then_some(cell.node);
            } else {
                match cell.kind {
                    CellKind::File => select = Some(cell.node),
                    CellKind::Folder { .. } | CellKind::Closed => {
                        select = Some(cell.node);
                        zoom = Some(cell.node);
                    }
                    CellKind::Rest { .. } => zoom = (cell.node != root).then_some(cell.node),
                }
            }
        }
        let back = (!keyboard_taken && ui.input(|i| i.key_pressed(egui::Key::Backspace)))
            || (response.hovered()
                && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Extra1)));
        let claims_back = response.hovered() && (root != ROOT || !self.view.is_fit());
        ui.data_mut(|d| d.insert_temp(mouse_back_claim_id(), claims_back));
        if back {
            if !self.view.is_fit() {
                self.view = MapView::default();
                ui.ctx().request_repaint();
            } else if root != ROOT {
                zoom = Some(tree.nodes[root].parent);
            }
        }
        if let Some(i) = hovered {
            let views = [file_system_1_view.as_ref(), file_system_2_view.as_ref()];
            response.on_hover_ui_at_pointer(|ui| tooltip(ui, tree, rows, cells[i], views, compare));
        }

        if let Some(node) = zoom {
            self.zoom = tree.path(rows, node).to_owned();
            self.view = MapView::default();
            ui.ctx().request_repaint();
        }
        let mut request = |request: CursorRequest| {
            apply_cursor_request(
                file_system_1_view.as_mut(),
                file_system_2_view.as_mut(),
                rows,
                &request,
                diff_tool_config,
                tool_message,
            )
        };
        if let Some(node) = select {
            *cursor = TreeCursor::at(tree.path(rows, node));
            // So the tree shows the selection when switched back.
            for ancestor in tree.ancestors(node) {
                if ancestor != node && tree.nodes[ancestor].row.is_some() {
                    request(CursorRequest::Expand(tree.path(rows, ancestor).to_owned()));
                }
            }
        }
        if let Some(node) = open {
            request(CursorRequest::Open(tree.path(rows, node).to_owned()));
            // The status line was drawn before this pane.
            ui.ctx().request_repaint();
        }
    }
}

fn file_sizes(views: [Option<&FileSystemView>; 2], state: &DiffState) -> [u64; 2] {
    let size = |view: Option<&FileSystemView>, id: Option<FsNodeId>| {
        view.zip(id)
            .and_then(|(view, id)| view.file_system.get_node(id))
            .filter(|node| node.is_file())
            .map_or(0, |node| node.size)
    };
    [
        size(views[0], state.first()),
        size(views[1], state.second()),
    ]
}

fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

struct Look {
    bg: Color32,
    text: Color32,
    weak: Color32,
    roles: [Color32; CellState::ALL.len()],
    selected: Stroke,
    hovered: Stroke,
    font: FontId,
    line: f32,
    header: f32,
}

impl Look {
    fn new(ui: &egui::Ui, params: LayoutParams) -> Self {
        let visuals = ui.visuals();
        Self {
            bg: visuals.extreme_bg_color,
            text: visuals.strong_text_color(),
            weak: visuals.weak_text_color(),
            roles: CellState::ALL.map(|state| role_color(ui.ctx(), state.role())),
            selected: Stroke::new(2.0, visuals.selection.stroke.color),
            hovered: Stroke::new(1.0, visuals.strong_text_color()),
            font: egui::TextStyle::Small.resolve(ui.style()),
            line: ui.text_style_height(&egui::TextStyle::Small),
            header: params.header,
        }
    }

    fn tint(&self, state: CellState, strength: f32) -> Color32 {
        self.bg.lerp_to_gamma(self.roles[state as usize], strength)
    }

    fn text(&self, painter: &egui::Painter, rect: Rect, text: &str, color: Color32) -> f32 {
        let galley = painter.layout_no_wrap(text.to_owned(), self.font.clone(), color);
        let width = galley.size().x;
        painter
            .with_clip_rect(rect.intersect(painter.clip_rect()))
            .galley(rect.min + vec2(3.0, 1.0), galley, color);
        width
    }

    fn paint(
        &self,
        painter: &egui::Painter,
        tree: &MapTree,
        rows: &[VisibleRowTwoFolderDiff],
        cell: &MapCell,
    ) {
        let node = &tree.nodes[cell.node];
        let state = node.state;
        let name = || format!("{}{}", state.name_prefix(), tree.name(rows, cell.node));
        match cell.kind {
            CellKind::File | CellKind::Closed => {
                let rect = cell.rect.shrink(0.5);
                painter.rect_filled(rect, 0.0, self.tint(state, state.fill_strength()));
                let size = node.sizes[0].max(node.sizes[1]);
                if cell.kind == CellKind::File {
                    self.paint_size_delta(painter, rect, node);
                }
                if rect.width() >= 28.0 && rect.height() >= self.line + 2.0 {
                    let name = if cell.kind == CellKind::Closed {
                        format!("{}/", name())
                    } else {
                        name()
                    };
                    self.text(painter, rect, &name, self.text);
                    if rect.height() >= 2.0 * self.line + 4.0 {
                        let below = rect.with_min_y(rect.min.y + self.line);
                        self.text(painter, below, &size_text(size), self.weak);
                    }
                }
            }
            CellKind::Folder { header } => {
                let rect = cell.rect;
                painter.rect_filled(rect, 0.0, self.tint(state, 0.08));
                let border = Stroke::new(1.0, self.tint(state, 0.45));
                painter.rect_stroke(rect, 0.0, border, StrokeKind::Inside);
                if header {
                    let strip = rect.with_max_y(rect.min.y + self.header);
                    painter.rect_filled(strip, 0.0, self.tint(state, 0.3));
                    let width = self.text(painter, strip, &name(), self.text);
                    let after = strip.with_min_x(strip.min.x + width + 6.0);
                    let size = node.sizes[0].max(node.sizes[1]);
                    self.text(painter, after, &size_text(size), self.weak);
                }
            }
            CellKind::Rest { count } => {
                let rect = cell.rect.shrink(0.5);
                painter.rect_filled(rect, 0.0, self.tint(CellState::Pending, 0.2));
                if rect.width() >= 28.0 && rect.height() >= self.line + 2.0 {
                    self.text(painter, rect, &format!("+{count}"), self.weak);
                }
            }
        }
    }

    fn paint_size_delta(&self, painter: &egui::Painter, rect: Rect, node: &MapNode) {
        let [left, right] = node.sizes;
        let pair = matches!(
            node.state,
            CellState::Different | CellState::Similar | CellState::Pending
        );
        if !pair || left == right || rect.width() < 8.0 || rect.height() < 8.0 {
            return;
        }
        let track = Rect::from_min_max(
            rect.left_bottom() + vec2(1.0, -3.0),
            rect.right_bottom() + vec2(-1.0, -1.0),
        );
        let share = left.min(right) as f32 / left.max(right) as f32;
        painter.rect_filled(track, 0.0, self.text.gamma_multiply(0.15));
        let bar = track.with_max_x(track.min.x + track.width() * share);
        painter.rect_filled(bar, 0.0, self.text.gamma_multiply(0.6));
    }
}

fn tooltip(
    ui: &mut egui::Ui,
    tree: &MapTree,
    rows: &[VisibleRowTwoFolderDiff],
    cell: MapCell,
    views: [Option<&FileSystemView>; 2],
    compare: &TreeCompare,
) {
    // All cells share one tooltip Area, which otherwise keeps the last cell's width (egui#5167).
    ui.set_max_width(ui.spacing().tooltip_width);
    let node = &tree.nodes[cell.node];
    if let CellKind::Rest { count } = cell.kind {
        ui.label(format!("{count} more entries, too small to show"));
        return;
    }
    let path = tree.path(rows, cell.node);
    ui.strong(if path.is_empty() { "Root" } else { path });
    let color = role_color(ui.ctx(), node.state.role());
    ui.label(RichText::new(node.state.label()).color(color));
    let row = node.row.map(|row| &rows[row]);
    let ids = row.map(|row| [row.diff_state.first(), row.diff_state.second()]);
    for (i, side) in [Side::Folder1, Side::Folder2].into_iter().enumerate() {
        let present = node.is_dir || ids.is_some_and(|ids| ids[i].is_some());
        let size = if present {
            size_text(node.sizes[i])
        } else {
            "missing".to_owned()
        };
        ui.label(format!("{side}: {size}"));
    }
    if let Some(ratio) = row.and_then(|row| likeness(row, views, compare)) {
        ui.label(format!("Likeness: {} %", percent(ratio)));
    }
    if node.is_dir {
        ui.label(format!("{} files", node.files));
    }
}

fn likeness(
    row: &VisibleRowTwoFolderDiff,
    views: [Option<&FileSystemView>; 2],
    compare: &TreeCompare,
) -> Option<f64> {
    if let Some(FolderLikeness::Known(ratio)) = row.folder_likeness {
        return Some(ratio);
    }
    let (DiffState::Different(left, right) | DiffState::Similar(left, right)) = row.diff_state
    else {
        return None;
    };
    let nodes = [
        views[0]?.file_system.get_node(left)?,
        views[1]?.file_system.get_node(right)?,
    ];
    if !nodes.iter().all(|node| node.is_file()) {
        return None;
    }
    compare.likeness_of(&row.rel_path, nodes)?.ratio()
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{pos2, vec2};

    fn row(rel_path: &str, is_dir: bool, state: DiffState) -> VisibleRowTwoFolderDiff {
        VisibleRowTwoFolderDiff {
            rel_path: rel_path.to_owned(),
            is_dir,
            depth: rel_path.split('/').count() as u16,
            diff_state: state,
            file_kinds: [None, None],
            blueprints: [false, false],
            folder_likeness: None,
        }
    }

    fn file(rel_path: &str, state: DiffState) -> VisibleRowTwoFolderDiff {
        row(rel_path, false, state)
    }

    fn dir(rel_path: &str, state: DiffState) -> VisibleRowTwoFolderDiff {
        row(rel_path, true, state)
    }

    fn sizes_from(table: &[(&str, [u64; 2])]) -> impl Fn(&VisibleRowTwoFolderDiff) -> [u64; 2] {
        let table: HashMap<String, [u64; 2]> =
            table.iter().map(|(p, s)| (p.to_string(), *s)).collect();
        move |row| table.get(&row.rel_path).copied().unwrap_or([0, 0])
    }

    fn shape(tree: &MapTree, rows: &[VisibleRowTwoFolderDiff]) -> Vec<(String, u64, Vec<String>)> {
        let mut out: Vec<_> = (0..tree.nodes.len())
            .map(|node| {
                let children = tree.nodes[node]
                    .children
                    .iter()
                    .map(|&c| tree.path(rows, c).to_owned())
                    .collect();
                (
                    tree.path(rows, node).to_owned(),
                    tree.nodes[node].weight,
                    children,
                )
            })
            .collect();
        out.sort();
        out
    }

    fn node_at(tree: &MapTree, rows: &[VisibleRowTwoFolderDiff], path: &str) -> usize {
        (0..tree.nodes.len())
            .find(|&node| tree.path(rows, node) == path)
            .unwrap_or_else(|| panic!("no node {path}"))
    }

    fn area(rect: Rect) -> f64 {
        rect.width().max(0.0) as f64 * rect.height().max(0.0) as f64
    }

    fn aspect(rect: Rect) -> f64 {
        let (w, h) = (rect.width() as f64, rect.height() as f64);
        w.max(h) / w.min(h)
    }

    const SIZES: &[(&str, [u64; 2])] = &[
        ("a/big.bin", [600, 500]),
        ("a/b/x.txt", [100, 100]),
        ("a/b/y.txt", [0, 50]),
        ("c.txt", [0, 30]),
        ("d.txt", [20, 0]),
    ];

    fn nested_rows() -> Vec<VisibleRowTwoFolderDiff> {
        vec![
            dir("", DiffState::Different(0, 0)),
            dir("a", DiffState::Different(1, 1)),
            dir("a/b", DiffState::Different(2, 2)),
            file("a/b/x.txt", DiffState::Same(3, 3)),
            file("a/b/y.txt", DiffState::Different(4, 4)),
            file("a/big.bin", DiffState::Different(5, 5)),
            file("c.txt", DiffState::OnlyInSecond(6)),
            file("d.txt", DiffState::OnlyInFirst(7)),
        ]
    }

    #[test]
    fn squarify_areas_are_proportional_and_tile_the_rect() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(600.0, 400.0));
        let cases: &[&[f64]] = &[
            &[1.0],
            &[6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0],
            &[5.0, 5.0, 5.0, 5.0],
            &[100.0, 1.0, 1.0, 1.0],
            &[1e9, 1.0, 1.0],
            &[3.0, 2.0, 0.0, 0.0],
            &[1.0, 4.0, 2.0, 8.0],
        ];
        for &weights in cases {
            let rects = squarify(weights, rect);
            assert_eq!(rects.len(), weights.len(), "{weights:?}");
            let total: f64 = weights.iter().sum();
            let covered: f64 = rects.iter().map(|r| area(*r)).sum();
            assert!((covered - area(rect)).abs() < 1.0, "{weights:?}: {covered}");
            for (r, w) in rects.iter().zip(weights) {
                let expected = w / total * area(rect);
                assert!(
                    (area(*r) - expected).abs() <= 0.5 + expected * 1e-4,
                    "{weights:?}: {r:?} for {w}"
                );
                assert!(rect.expand(1e-3).contains_rect(*r), "{weights:?}: {r:?}");
            }
            for i in 0..rects.len() {
                for j in i + 1..rects.len() {
                    let overlap = area(rects[i].intersect(rects[j]));
                    assert!(overlap < 1e-2, "{weights:?}: {i} and {j} overlap");
                }
            }
        }
    }

    #[test]
    fn squarify_lays_out_the_paper_example_squarer_than_slicing() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(6.0, 4.0));
        let weights = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
        let rects = squarify(&weights, rect);

        assert_eq!(rects[0], Rect::from_min_max(pos2(0.0, 0.0), pos2(3.0, 2.0)));
        assert_eq!(rects[1], Rect::from_min_max(pos2(0.0, 2.0), pos2(3.0, 4.0)));
        let worst = rects.iter().map(|r| aspect(*r)).fold(0.0, f64::max);
        let sliced_worst = weights
            .iter()
            .map(|w| aspect(Rect::from_min_size(Pos2::ZERO, vec2(*w as f32 / 4.0, 4.0))))
            .fold(0.0, f64::max);
        assert!(worst < 3.0, "{worst}");
        assert!(worst < sliced_worst);
    }

    #[test]
    fn map_tree_from_rows_nested_and_flat_agree() {
        let nested = nested_rows();
        let files_only: Vec<_> = nested.iter().filter(|r| !r.is_dir).cloned().rev().collect();

        let tree = MapTree::build(&nested, sizes_from(SIZES));
        let flat = MapTree::build(&files_only, sizes_from(SIZES));

        assert_eq!(shape(&tree, &nested), shape(&flat, &files_only));
        let a = node_at(&tree, &nested, "a");
        assert_eq!(tree.nodes[a].weight, 600 + 100 + 50);
        assert_eq!(tree.nodes[a].sizes, [700, 650]);
        assert_eq!(tree.nodes[a].files, 3);
        assert_eq!(tree.nodes[ROOT].weight, 600 + 100 + 50 + 30 + 20);
        let children: Vec<_> = tree.nodes[ROOT]
            .children
            .iter()
            .map(|&c| tree.path(&nested, c))
            .collect();
        assert_eq!(children, ["a", "c.txt", "d.txt"], "largest first");
    }

    #[test]
    fn empty_files_and_folders_keep_a_weight() {
        let rows = vec![
            dir("", DiffState::Same(0, 0)),
            dir("empty", DiffState::Same(1, 1)),
            file("zero.txt", DiffState::Same(2, 2)),
        ];
        let tree = MapTree::build(&rows, |_| [0, 0]);

        assert_eq!(tree.nodes[node_at(&tree, &rows, "empty")].weight, 1);
        assert_eq!(tree.nodes[node_at(&tree, &rows, "zero.txt")].weight, 1);
        assert_eq!(tree.nodes[ROOT].weight, 2);
    }

    #[test]
    fn folders_missing_from_the_rows_take_their_state_from_their_files() {
        let rows = vec![
            file("mixed/a", DiffState::Same(0, 0)),
            file("mixed/b", DiffState::Different(1, 1)),
            file("same/a", DiffState::Same(2, 2)),
            file("same/b", DiffState::Similar(3, 3)),
            file("left/a", DiffState::OnlyInFirst(4)),
            file("left/b", DiffState::OnlyInFirst(5)),
            file("half/a", DiffState::OnlyInFirst(6)),
            file("half/b", DiffState::Pending(7, 7)),
            file("wait/a", DiffState::Pending(8, 8)),
            file("wait/b", DiffState::Similar(9, 9)),
        ];
        let tree = MapTree::build(&rows, |_| [1, 1]);
        let state = |path| tree.nodes[node_at(&tree, &rows, path)].state;

        assert_eq!(state("mixed"), CellState::Different);
        assert_eq!(state("same"), CellState::Similar);
        assert_eq!(state("left"), CellState::OnlyInFirst);
        assert_eq!(state("half"), CellState::Different);
        assert_eq!(state("wait"), CellState::Pending);
        assert_eq!(state(""), CellState::Different);
    }

    #[test]
    fn zoom_path_resolves_to_subtree_and_falls_back_when_missing() {
        let rows = nested_rows();
        let tree = MapTree::build(&rows, sizes_from(SIZES));
        let resolved = |zoom| tree.path(&rows, tree.resolve(&rows, zoom));

        assert_eq!(resolved(""), "");
        assert_eq!(resolved("a"), "a");
        assert_eq!(resolved("a/b"), "a/b");
        assert_eq!(resolved("a/b/gone"), "a/b");
        assert_eq!(resolved("a/b/x.txt"), "a/b", "files are not zoom roots");
        assert_eq!(resolved("gone/a"), "");
        assert_eq!(
            tree.ancestors(tree.resolve(&rows, "a/b")),
            [
                ROOT,
                node_at(&tree, &rows, "a"),
                node_at(&tree, &rows, "a/b")
            ]
        );
    }

    const PARAMS: LayoutParams = LayoutParams {
        header: 16.0,
        pad: 2.0,
        min_side: 4.0,
        visible: Rect::EVERYTHING,
    };

    #[test]
    fn hit_test_returns_deepest_cell_and_none_outside() {
        let rows = nested_rows();
        let tree = MapTree::build(&rows, sizes_from(SIZES));
        let rect = Rect::from_min_size(pos2(5.0, 5.0), vec2(800.0, 600.0));
        let cells = layout(&tree, ROOT, rect, PARAMS);
        let cell_of = |path| {
            let node = node_at(&tree, &rows, path);
            *cells.iter().find(|c| c.node == node).unwrap()
        };

        let big = cell_of("a/big.bin");
        assert_eq!(big.kind, CellKind::File);
        assert_eq!(
            cell_at(&cells, big.rect.center()).map(|i| cells[i]),
            Some(big)
        );
        let a = cell_of("a");
        assert_eq!(a.kind, CellKind::Folder { header: true });
        let in_header = pos2(a.rect.center().x, a.rect.top() + PARAMS.header / 2.0);
        assert_eq!(cell_at(&cells, in_header).map(|i| cells[i]), Some(a));
        assert!(
            cells
                .iter()
                .all(|c| rect.expand(1e-3).contains_rect(c.rect))
        );
        assert_eq!(cell_at(&cells, pos2(1.0, 1.0)), None);
        assert_eq!(cell_at(&cells, pos2(900.0, 300.0)), None);
    }

    #[test]
    fn lod_merges_cells_below_min_size_into_rest() {
        let mut rows = vec![file("huge.bin", DiffState::Different(0, 0))];
        rows.extend((0..50).map(|i| file(&format!("tiny_{i:02}"), DiffState::Same(i, i))));
        let tree = MapTree::build(&rows, |row| {
            if row.rel_path == "huge.bin" {
                [1_000_000, 0]
            } else {
                [1, 1]
            }
        });
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(100.0, 100.0));
        let cells = layout(&tree, ROOT, rect, PARAMS);

        assert_eq!(cells.len(), 2, "{cells:?}");
        assert_eq!(cells[0].node, node_at(&tree, &rows, "huge.bin"));
        assert_eq!(cells[1].kind, CellKind::Rest { count: 50 });
        assert_eq!(cells[1].node, ROOT);
        assert!(area(cells[1].rect) > 0.0);
    }

    #[test]
    fn a_folder_too_small_for_its_entries_is_one_closed_cell() {
        let rows = nested_rows();
        let tree = MapTree::build(&rows, sizes_from(SIZES));
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(8.0, 8.0));
        let cells = layout(&tree, ROOT, rect, PARAMS);
        let a = node_at(&tree, &rows, "a");

        let a_cell = cells.iter().find(|c| c.node == a).unwrap();
        assert_eq!(a_cell.kind, CellKind::Closed);
        assert!(cells.iter().all(|c| tree.nodes[c.node].parent != a));
    }

    #[test]
    fn layout_culled_to_a_visible_rect_keeps_exactly_the_cells_touching_it() {
        let mut rows = nested_rows();
        let mut sizes: HashMap<String, [u64; 2]> =
            SIZES.iter().map(|(p, s)| (p.to_string(), *s)).collect();
        for i in 0..200 {
            let path = format!("a/b/n_{i:03}");
            sizes.insert(path.clone(), [1 << (i % 20), 3]);
            rows.push(file(&path, DiffState::Same(i, i)));
        }
        let tree = MapTree::build(&rows, |row| {
            sizes.get(&row.rel_path).copied().unwrap_or_default()
        });
        let rect = Rect::from_min_size(pos2(-900.0, -300.0), vec2(4000.0, 3000.0));
        let full = layout(&tree, ROOT, rect, PARAMS);
        assert!(full.iter().any(|c| matches!(c.kind, CellKind::Rest { .. })));
        for visible in [
            Rect::from_min_size(pos2(10.0, 20.0), vec2(800.0, 600.0)),
            Rect::from_min_size(pos2(2500.3, 1200.7), vec2(37.0, 900.0)),
            Rect::from_min_size(pos2(-5000.0, -5000.0), vec2(10.0, 10.0)),
        ] {
            let culled = layout(&tree, ROOT, rect, LayoutParams { visible, ..PARAMS });
            let touching: Vec<_> = full
                .iter()
                .copied()
                .filter(|cell| cell.rect.intersects(visible))
                .collect();
            assert_eq!(culled, touching, "{visible:?}");
        }
    }

    #[test]
    fn map_view_zooms_about_the_pointer_and_always_covers_the_viewport() {
        let viewport = Rect::from_min_size(pos2(10.0, 30.0), vec2(800.0, 500.0));
        let fraction = |view: MapView, at: Pos2| {
            let shown = view.virtual_rect(viewport);
            (at - shown.min) / shown.size()
        };
        let covers = |view: MapView, viewport: Rect| {
            view.virtual_rect(viewport)
                .expand(1e-3)
                .contains_rect(viewport)
        };
        let pointers = [
            viewport.center(),
            viewport.min,
            viewport.max,
            pos2(100.0, 400.0),
            pos2(790.0, 40.0),
        ];
        for at in pointers {
            let mut view = MapView::default();
            for factor in [1.5, 3.0, 10.0, 0.7] {
                let before = fraction(view, at);
                view = view.zoomed_about(at, factor, viewport);
                assert!(
                    (fraction(view, at) - before).length() < 1e-4,
                    "{at:?} x{factor}"
                );
                assert!(covers(view, viewport), "{at:?} x{factor}: {view:?}");
            }
            assert_eq!(view.zoomed_about(at, 1e9, viewport).scale, MAX_SCALE);
            let out = view.zoomed_about(at, 0.5, viewport);
            assert!(covers(out, viewport), "{at:?}: {out:?}");
            assert_eq!(view.zoomed_about(at, 1e-9, viewport), MapView::default());

            let smaller = Rect::from_min_size(viewport.min, viewport.size() * 0.3);
            let larger = Rect::from_min_size(viewport.min, viewport.size() * 2.0);
            for resized in [smaller, larger] {
                let kept = view.clamped(resized);
                assert_eq!(kept.scale, view.scale);
                assert!(covers(kept, resized), "{at:?}: {kept:?} in {resized:?}");
            }
        }
    }

    struct MapHarness {
        _dirs: [tempfile::TempDir; 2],
        views: [Option<FileSystemView>; 2],
        rows: Vec<VisibleRowTwoFolderDiff>,
        map: TreeMap,
        cursor: TreeCursor,
        ctx: egui::Context,
        compare: TreeCompare,
        config: DiffToolConfig,
        message: Option<ToolMessage>,
        time: f64,
    }

    impl MapHarness {
        fn new(files: &[(&str, usize)]) -> Self {
            use std::sync::Arc;
            use zcommon::kinds::KindTable;
            use zdiff_folder::{comparison::PairState, filter::PathFilter, fs::FileSystemModel};

            let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
            for dir in &dirs {
                for (path, size) in files {
                    let path = dir.path().join(path);
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(path, vec![b'x'; *size]).unwrap();
                }
            }
            let views = dirs.each_ref().map(|dir| {
                Some(FileSystemView::new(Arc::new(
                    FileSystemModel::new(dir.path()).unwrap(),
                )))
            });
            let rows = FileSystemView::build_two_folder_diff_rows(
                views[0].as_ref(),
                views[1].as_ref(),
                &PathFilter::default(),
                &KindTable::default(),
                &Default::default(),
                |_, _| (Some(PairState::Different), None),
                |_, _| None,
            )
            .unwrap();
            Self {
                _dirs: dirs,
                views,
                rows,
                map: TreeMap::default(),
                cursor: TreeCursor::default(),
                ctx: egui::Context::default(),
                compare: TreeCompare::default(),
                config: DiffToolConfig {
                    tools: vec![zdiff_folder::external_diff_tool::DiffTool {
                        name: "Missing".to_string(),
                        exe_path: "zdiff-folder-test-missing-tool.exe".into(),
                        template: "{left} {right}".to_string(),
                        kinds: Vec::new(),
                    }],
                    rules: Vec::new(),
                    default_tool: "Missing".to_string(),
                    mark_undiffable: false,
                },
                message: None,
                time: 0.0,
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            self.time += 0.1;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let [view_1, view_2] = &mut self.views;
            let _ = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    self.map.ui(
                        ui,
                        MapCtx {
                            file_system_1_view: view_1,
                            file_system_2_view: view_2,
                            rows: &self.rows,
                            rows_id: RowsId::default(),
                            compare: &self.compare,
                            diff_tool_config: &self.config,
                            tool_message: &mut self.message,
                            cursor: &mut self.cursor,
                            keyboard_taken: false,
                        },
                    );
                });
            });
        }

        fn cells(&self) -> &[MapCell] {
            &self.map.cells.as_ref().unwrap().1
        }

        fn viewport(&self) -> Rect {
            self.map.cells.as_ref().unwrap().0.params.visible
        }

        fn path_at(&self, pos: Pos2) -> &str {
            let tree = &self.map.tree.as_ref().unwrap().1;
            let cell = self.cells()[cell_at(self.cells(), pos).unwrap()];
            tree.path(&self.rows, cell.node)
        }

        fn point_on(&self, path: &str) -> Pos2 {
            let tree = &self.map.tree.as_ref().unwrap().1;
            let cell = self
                .cells()
                .iter()
                .find(|cell| {
                    !matches!(cell.kind, CellKind::Rest { .. })
                        && tree.path(&self.rows, cell.node) == path
                })
                .unwrap_or_else(|| panic!("no cell for {path}"));
            let shown = cell.rect.intersect(self.viewport());
            match cell.kind {
                CellKind::Folder { .. } => pos2(shown.center().x, cell.rect.top() + 1.0),
                _ => shown.center(),
            }
        }

        fn clicks_at(&mut self, pos: Pos2, button: egui::PointerButton, count: usize) {
            let event = |pressed| egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            self.frame(vec![egui::Event::PointerMoved(pos)]);
            for _ in 0..count {
                self.frame(vec![event(true)]);
                self.frame(vec![event(false)]);
            }
            self.frame(Vec::new());
        }

        fn clicks(&mut self, path: &str, count: usize) {
            let pos = self.point_on(path);
            self.clicks_at(pos, egui::PointerButton::Primary, count);
        }

        fn zoom_at(&mut self, pos: Pos2, factor: f32) {
            self.frame(vec![egui::Event::PointerMoved(pos)]);
            self.frame(vec![egui::Event::Zoom(factor)]);
        }

        fn wheel_at(&mut self, pos: Pos2, delta: f32) {
            self.frame(vec![egui::Event::PointerMoved(pos)]);
            self.frame(vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: vec2(0.0, delta),
                modifiers: egui::Modifiers::NONE,
            }]);
            for _ in 0..10 {
                self.frame(Vec::new());
            }
        }

        fn drag(&mut self, from: Pos2, by: Vec2) {
            let event = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            self.frame(vec![egui::Event::PointerMoved(from)]);
            self.frame(vec![event(from, true)]);
            for step in 1..=4 {
                self.frame(vec![egui::Event::PointerMoved(
                    from + by * step as f32 / 4.0,
                )]);
            }
            self.frame(vec![event(from + by, false)]);
            self.frame(Vec::new());
        }

        fn backspace(&mut self) {
            self.frame(vec![egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            self.frame(Vec::new());
        }

        fn click(&mut self, path: &str) {
            self.clicks(path, 1);
        }

        fn collapse(&mut self, path: &str) {
            let row = self.rows.iter().find(|row| row.rel_path == path).unwrap();
            let ids = [row.diff_state.first(), row.diff_state.second()];
            for (view, id) in self.views.iter_mut().zip(ids) {
                view.as_mut().unwrap().collapsed.insert(id.unwrap(), true);
            }
        }

        fn collapsed(&self, path: &str) -> [bool; 2] {
            let row = self.rows.iter().find(|row| row.rel_path == path).unwrap();
            let ids = [row.diff_state.first(), row.diff_state.second()];
            [0, 1].map(|side| {
                self.views[side]
                    .as_ref()
                    .unwrap()
                    .is_collapsed(ids[side].unwrap())
            })
        }
    }

    #[test]
    fn clicks_select_files_zoom_into_folders_and_backspace_zooms_out() {
        let mut map = MapHarness::new(&[
            ("a/x.txt", 3000),
            ("a/sub/y.txt", 2000),
            ("a/sub/z.txt", 1000),
            ("big.bin", 4000),
        ]);
        map.collapse("a");
        map.collapse("a/sub");
        map.frame(Vec::new());

        map.click("big.bin");
        assert_eq!(map.cursor, TreeCursor::at("big.bin"));
        assert_eq!(map.map.zoom, "");

        map.click("a");
        assert_eq!(map.map.zoom, "a");
        assert_eq!(map.cursor, TreeCursor::at("a"));
        assert_eq!(
            map.collapsed("a"),
            [true, true],
            "a selected folder stays folded"
        );

        map.click("a/sub/y.txt");
        assert_eq!(map.cursor, TreeCursor::at("a/sub/y.txt"));
        assert_eq!(map.map.zoom, "a");
        assert_eq!(map.collapsed("a"), [false, false]);
        assert_eq!(map.collapsed("a/sub"), [false, false]);

        let backspace = egui::Event::Key {
            key: egui::Key::Backspace,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        map.frame(vec![backspace]);
        assert_eq!(map.map.zoom, "");
        assert_eq!(map.cursor, TreeCursor::at("a/sub/y.txt"));
    }

    #[test]
    fn a_zoomed_in_map_under_the_pointer_claims_the_mouse_back_button() {
        let mut map = MapHarness::new(&[("a/x.txt", 3000), ("a/y.txt", 1000), ("b.bin", 4000)]);
        map.collapse("a");
        map.frame(Vec::new());
        let pos = map.point_on("b.bin");
        map.frame(vec![egui::Event::PointerMoved(pos)]);
        assert!(!take_mouse_back_claim(&map.ctx), "not zoomed in");

        map.click("a");
        assert_eq!(map.map.zoom, "a");
        assert!(take_mouse_back_claim(&map.ctx));
        assert!(!take_mouse_back_claim(&map.ctx), "taken once per frame");

        map.frame(vec![egui::Event::PointerGone]);
        map.frame(Vec::new());
        assert!(!take_mouse_back_claim(&map.ctx), "pointer left the map");

        let back = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Extra1,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        map.frame(vec![egui::Event::PointerMoved(pos)]);
        map.frame(vec![back(true)]);
        map.frame(vec![back(false)]);
        assert_eq!(map.map.zoom, "");
        map.frame(Vec::new());
        assert!(!take_mouse_back_claim(&map.ctx), "zoomed out");
    }

    #[test]
    fn double_clicking_a_file_opens_its_diff_and_a_folder_zooms_one_level() {
        let mut map = MapHarness::new(&[("a/sub/y.txt", 2000), ("big.bin", 1000)]);
        map.frame(Vec::new());

        map.clicks("a", 2);
        assert_eq!(map.map.zoom, "a");
        assert_eq!(map.cursor, TreeCursor::at("a"));
        assert_eq!(map.message, None);

        map.frame(vec![egui::Event::Key {
            key: egui::Key::Backspace,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        map.frame(Vec::new());
        map.clicks("big.bin", 2);
        assert_eq!(map.cursor, TreeCursor::at("big.bin"));
        let error = map.message.and_then(|message| message.error).unwrap();
        assert!(error.contains("diffing failed"), "{error}");
    }

    #[test]
    fn scrolling_zooms_in_until_merged_entries_become_boxes_and_back_out_to_fit() {
        let names: Vec<String> = (0..200).map(|i| format!("tiny_{i:03}")).collect();
        let mut files = vec![("huge.bin", 50_000)];
        files.extend(names.iter().map(|name| (name.as_str(), 1)));
        let mut map = MapHarness::new(&files);
        map.frame(Vec::new());
        let rest = map
            .cells()
            .iter()
            .find(|cell| matches!(cell.kind, CellKind::Rest { .. }))
            .expect("tiny files merged at fit")
            .rect
            .center();

        map.wheel_at(rest, 300.0);
        assert!(map.map.view.scale > 4.0, "{:?}", map.map.view);
        assert!(
            map.path_at(rest).starts_with("tiny_"),
            "{}",
            map.path_at(rest)
        );
        assert!(
            map.cells()
                .iter()
                .all(|cell| !matches!(cell.kind, CellKind::Rest { .. }))
        );

        map.wheel_at(rest, -3000.0);
        assert_eq!(map.map.view, MapView::default());
        assert!(
            map.cells()
                .iter()
                .any(|cell| matches!(cell.kind, CellKind::Rest { .. }))
        );
    }

    #[test]
    fn a_zoomed_map_pans_on_drag_and_fits_on_middle_click_backspace_and_folder_click() {
        let mut map = MapHarness::new(&[("a/x.txt", 3000), ("a/y.txt", 1000), ("b.bin", 5000)]);
        map.frame(Vec::new());
        let center = map.viewport().center();
        let at_center = map.path_at(center).to_owned();

        map.zoom_at(center, 2.0);
        let zoomed = map.map.view;
        assert_eq!(zoomed.scale, 2.0);
        assert_eq!(map.path_at(center), at_center);
        map.drag(center, vec2(-40.0, 0.0));
        assert!(map.map.view.pan.x < zoomed.pan.x, "{:?}", map.map.view);
        assert_eq!(map.map.view.pan.y, zoomed.pan.y);
        assert_eq!(map.cursor, TreeCursor::default());
        assert_eq!(map.map.zoom, "");
        assert_eq!(map.message, None);

        map.click("b.bin");
        assert_eq!(map.cursor, TreeCursor::at("b.bin"));
        assert_eq!(map.map.view.scale, 2.0);
        assert!(take_mouse_back_claim(&map.ctx));

        map.clicks_at(center, egui::PointerButton::Middle, 1);
        assert_eq!(map.map.view, MapView::default());

        map.zoom_at(center, 2.0);
        map.backspace();
        assert_eq!(map.map.view, MapView::default());
        assert_eq!(map.map.zoom, "");

        let header = map.point_on("a");
        map.zoom_at(header, 2.0);
        assert_eq!(map.map.view.scale, 2.0);
        map.clicks_at(header, egui::PointerButton::Primary, 1);
        assert_eq!(map.map.zoom, "a");
        assert_eq!(map.map.view, MapView::default());

        map.zoom_at(center, 3.0);
        map.backspace();
        assert_eq!(map.map.zoom, "a");
        assert_eq!(map.map.view, MapView::default());
        map.backspace();
        assert_eq!(map.map.zoom, "");
    }
}
