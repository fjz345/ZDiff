use super::*;

pub(super) fn on_row_item_clicked(
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
            let (left_model, right_model) = (
                &file_system_1_view.unwrap().file_system,
                &file_system_2_view.unwrap().file_system,
            );
            let side =
                |model: &FileSystemModel, id: FsNodeId| (model.content().clone(), model.path(id));
            let sides = [side(left_model, path1), side(right_model, path2)];
            if sides.iter().any(|(content, _)| is_revision(content)) {
                message.status = Some(format!("Fetching {}...", entry.rel_path));
                revision_open::spawn(RevisionOpen {
                    tool: diff_tool.clone(),
                    exe,
                    sides,
                    rel_path: entry.rel_path.clone(),
                    warning: message.warning.clone(),
                });
                return Some(message);
            }
            let [left, right] = sides.map(|(_, path)| path.to_string_lossy().into_owned());
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
pub(super) struct SideLabel<'a> {
    pub(super) view: Option<&'a FileSystemView>,
    pub(super) node_id: Option<FsNodeId>,
    pub(super) depth: FsNodeDepth,
    pub(super) is_dir: bool,
    pub(super) is_collapsed: bool,
    pub(super) color: Option<egui::Color32>,
    pub(super) row_height: f32,
    pub(super) flat_rel_path: Option<&'a str>,
    pub(super) list_rel_path: Option<&'a str>,
    pub(super) highlight: &'a [Range<usize>],
}

pub(super) fn render_diff_side(
    ui: &mut egui::Ui,
    label: &SideLabel,
    mut on_select: impl FnMut(),
    mut on_open: impl FnMut(),
    mut on_toggle: impl FnMut(),
) -> Option<egui::Response> {
    let SideLabel {
        view,
        node_id,
        depth,
        is_dir,
        is_collapsed,
        color,
        row_height,
        flat_rel_path,
        list_rel_path,
        highlight,
    } = *label;
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
    let mut add_contents = |ui: &mut egui::Ui| {
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
                        .label(name_label(ui, "📁 ", &node.display_name()))
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
                        .label(name_label(ui, "", &node.display_name()))
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
    };
    add_contents(ui)
}

/// Must match what `render_diff_side` draws for a name `text_width` wide.
pub(super) fn side_width(
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
pub(super) enum RowLayout {
    Tree,
    /// Flat mode: relative paths in place of indented names.
    RelPaths,
    /// Diff only without parents: unindented names, the relative path in a tooltip.
    FileList,
}

/// Checks the kinds before choosing the tool, which allocates per rule, since this runs per drawn row.
pub(super) fn undiffable(
    entry: &VisibleRowTwoFolderDiff,
    config: &DiffToolConfig,
) -> Option<CantDiff> {
    if !config.mark_undiffable || entry.file_kinds.contains(&None) {
        return None;
    }
    let tool = config.choose_tool_quietly(&KindTable::default(), Path::new(&entry.rel_path))?;
    cant_diff(entry.file_kinds, tool)
}

#[derive(Clone, Copy)]
pub(super) enum CenterSlot {
    Tag(usize),
    State,
    Likeness,
}

/// Sized from the font, not the rows, so the center keeps its width as rows scroll by.
pub(super) struct CenterLayout {
    pub(super) tag: f32,
    pub(super) state: f32,
    pub(super) arrow_room: f32,
    pub(super) likeness: Option<f32>,
}

impl CenterLayout {
    pub(super) fn measure(ui: &egui::Ui, likeness_bar: bool) -> Self {
        let tag_font = egui::TextStyle::Monospace.resolve(ui.style());
        let header_font = egui::TextStyle::Button.resolve(ui.style());
        let (tag_glyph, header, state_symbol) = ui.fonts_mut(|f| {
            let mut width = |text: &str| {
                f.layout_no_wrap(
                    text.to_owned(),
                    header_font.clone(),
                    egui::Color32::PLACEHOLDER,
                )
                .size()
                .x
            };
            let header = ["≠ ⏶", "≠ ⏷"].map(&mut width);
            let state_symbol = width("≠");
            (
                f.glyph_width(&tag_font, 'M'),
                header[0].max(header[1]),
                state_symbol,
            )
        });
        let tag = 4.0 * tag_glyph;
        // The sort header is a selectable label, padded like a button. Its sort arrow's room is
        // kept on the left too, so the "≠" and the row symbols sit in the middle of the slot.
        let arrow_room = header - state_symbol;
        let state = (header + arrow_room + 2.0 * ui.spacing().button_padding.x)
            .max(ui.text_style_height(&egui::TextStyle::Body));
        Self {
            tag,
            state,
            arrow_room,
            likeness: likeness_bar.then_some(1.5 * tag),
        }
    }

    pub(super) fn state_symbol<R>(
        &self,
        ui: &mut egui::Ui,
        add: impl FnOnce(&mut egui::Ui) -> R,
    ) -> R {
        let height = ui.text_style_height(&egui::TextStyle::Body);
        ui.allocate_ui_with_layout(
            egui::vec2(self.state, height),
            egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
            add,
        )
        .inner
    }

    pub(super) fn state_header<R>(
        &self,
        ui: &mut egui::Ui,
        add: impl FnOnce(&mut egui::Ui) -> R,
    ) -> R {
        let cursor = ui.cursor().min;
        let rect = egui::Rect::from_min_max(
            egui::pos2(cursor.x + self.arrow_room, cursor.y),
            ui.max_rect().right_bottom(),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), add)
            .inner
    }

    pub(super) fn slots(&self) -> impl Iterator<Item = (CenterSlot, f32)> {
        [
            (CenterSlot::Tag(0), self.tag),
            (CenterSlot::State, self.state),
        ]
        .into_iter()
        .chain(self.likeness.map(|width| (CenterSlot::Likeness, width)))
        .chain([(CenterSlot::Tag(1), self.tag)])
    }

    pub(super) fn width(&self, spacing: f32) -> f32 {
        self.slots().map(|(_, width)| width + spacing).sum::<f32>() - spacing
    }

    pub(super) fn show(&self, ui: &mut egui::Ui, mut add: impl FnMut(CenterSlot, &mut egui::Ui)) {
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

/// Fills `cell_x` and half the gaps around it, like egui_extras' stripes, so a tinted row has
/// no seams. A cell's own clip stops short of the gaps.
pub(super) fn fill_cell(ui: &egui::Ui, cell_x: egui::Rangef, color: egui::Color32) {
    let rect = egui::Rect::from_x_y_ranges(
        cell_x.expand(0.5 * ui.spacing().item_spacing.x),
        ui.clip_rect().y_range(),
    );
    egui::Painter::new(ui.ctx().clone(), ui.layer_id(), rect).rect_filled(rect, 0.0, color);
}

#[derive(Clone, Copy)]
pub(super) struct RowCell<'a> {
    pub(super) views: [Option<&'a FileSystemView>; 2],
    pub(super) entry: &'a VisibleRowTwoFolderDiff,
    pub(super) dimmed: bool,
    pub(super) pending: bool,
    pub(super) row_height: f32,
    pub(super) diff_tool_config: &'a DiffToolConfig,
    pub(super) find_matcher: Option<&'a Matcher>,
    pub(super) match_path: bool,
}

pub(super) struct SideCell {
    pub(super) clicked: bool,
    pub(super) toggle: bool,
    pub(super) action: Option<RowAction>,
}

/// Draws one side of a row that `two_folder_cursor_rows` doesn't hide. `clicked` is a click
/// with either button, which moves the cursor to the row.
pub(super) fn render_side_cell(
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
        &SideLabel {
            view: views[side],
            node_id: ids[side],
            depth,
            is_dir: entry.is_dir,
            is_collapsed,
            color: if pending {
                Some(role_color(ui.ctx(), FolderRole::PendingDeletion))
            } else {
                dimmed.then(|| role_color(ui.ctx(), FolderRole::DimmedText))
            },
            row_height,
            flat_rel_path,
            list_rel_path,
            highlight: &highlight,
        },
        || select = true,
        || *tool_message = on_row_item_clicked(views[0], views[1], entry, diff_tool_config),
        || toggle = true,
    );

    if label.is_some() && cell.double_clicked() {
        if !entry.is_dir {
            *tool_message = on_row_item_clicked(views[0], views[1], entry, diff_tool_config);
        } else if flat_rel_path.is_none() {
            toggle = true;
        }
    }

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
            let paths = [0, 1].map(|i| {
                let (view, id) = (views[i]?, ids[i]?);
                Some(view.file_system.path(id))
            });
            let paths = [0, 1].map(|i| paths[i].as_deref());
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

pub(super) struct CenterCell {
    pub(super) clicked: bool,
    pub(super) double_clicked: bool,
}

pub(super) fn render_center_cell(
    ui: &mut egui::Ui,
    row: &RowCell,
    center: &CenterLayout,
    compare: &TreeCompare,
    tolerance: &Tolerance,
    shown_sides: [usize; 2],
) -> CenterCell {
    let RowCell {
        views,
        entry,
        dimmed,
        pending: _,
        row_height,
        diff_tool_config,
        ..
    } = *row;
    // Before the slots, so their buttons and tooltips stay on top. A click-only sense leaves
    // the drag to the split strip below.
    let cell = ui.interact(
        ui.max_rect(),
        ui.id().with("center_cell"),
        egui::Sense::click(),
    );
    let nodes = [
        side_node(views[0], entry.diff_state.first()),
        side_node(views[1], entry.diff_state.second()),
    ];
    let undiffable = undiffable(entry, diff_tool_config);
    let opacity = if dimmed { 0.5 } else { 1.0 };
    let body_height = ui.text_style_height(&egui::TextStyle::Body);
    let cell_rect = ui.max_rect();
    let (mut slot_left, mut slots_right) = (cell_rect.left(), cell_rect.left());
    for (i, (slot, width)) in center.slots().enumerate() {
        let rect = egui::Rect::from_x_y_ranges(slot_left..=slot_left + width, cell_rect.y_range());
        (slot_left, slots_right) = (rect.right() + ui.spacing().item_spacing.x, rect.right());
        if let (CenterSlot::Tag(_), Some(reason)) = (slot, &undiffable) {
            let stroke = egui::Stroke::new(1.0_f32, ui.visuals().warn_fg_color);
            ui.painter()
                .rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
            ui.interact(rect, ui.id().with(("tag", i)), egui::Sense::hover())
                .on_hover_text(reason.to_string());
        }
        match slot {
            CenterSlot::Tag(column) => {
                let side = shown_sides[column];
                if let Some(kind) = entry.file_kinds[side] {
                    let (text, color) = if entry.blueprints[side] {
                        ("BP", role_color(ui.ctx(), FolderRole::BlueprintTag))
                    } else {
                        (kind.tag(), role_color(ui.ctx(), FolderRole::Tag(kind)))
                    };
                    let font = egui::TextStyle::Monospace.resolve(ui.style());
                    paint_centered(ui, rect, text, font, color.gamma_multiply(opacity));
                }
            }
            CenterSlot::State => {
                let symbol_rect =
                    egui::Rect::from_center_size(rect.center(), egui::vec2(width, body_height));
                let Some((symbol, role)) = diff_state_symbol(&entry.diff_state) else {
                    ui.put(symbol_rect, egui::Spinner::new().size(body_height))
                        .on_hover_text("not compared");
                    continue;
                };
                let font = egui::TextStyle::Body.resolve(ui.style());
                let color = role_color(ui.ctx(), role).gamma_multiply(opacity);
                paint_centered(ui, symbol_rect, symbol, font, color);
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
                    ui.interact(
                        symbol_rect,
                        ui.id().with(("state", i)),
                        egui::Sense::hover(),
                    )
                    .on_hover_text(similar_tooltip(tolerance, sides, likeness));
                }
            }
            CenterSlot::Likeness => {
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt(i)
                        .max_rect(rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                child.style_mut().interaction.selectable_labels = false;
                child.multiply_opacity(opacity);
                ui_likeness_cell(
                    &mut child,
                    likeness_cell(entry, nodes, compare),
                    row_height,
                    PreviewPair::new(views, entry),
                );
            }
        }
    }
    ui.advance_cursor_after_rect(egui::Rect::from_x_y_ranges(
        cell_rect.left()..=slots_right,
        cell_rect.y_range(),
    ));
    CenterCell {
        clicked: cell.clicked() || cell.secondary_clicked(),
        double_clicked: cell.double_clicked(),
    }
}

fn paint_centered(
    ui: &egui::Ui,
    rect: egui::Rect,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
) {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
}

pub(super) fn side_node(view: Option<&FileSystemView>, id: Option<FsNodeId>) -> Option<&FsNode> {
    view?.file_system.get_node(id?)
}

/// Each line sits under its folder's expander, in the children's indentation.
pub(super) fn paint_guides(ui: &egui::Ui, guides: &[Guide]) {
    let cell = ui.max_rect();
    // Spans the item spacing between rows, like the folder tint, so the lines have no gaps.
    let half_spacing = 0.5 * ui.spacing().item_spacing.y;
    let stroke = egui::Stroke::new(1.0_f32, role_color(ui.ctx(), FolderRole::GuideLine));
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

pub(crate) fn apply_cursor_request(
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
pub(super) fn cursor_keys(input: &egui::InputState) -> Vec<egui::Key> {
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
