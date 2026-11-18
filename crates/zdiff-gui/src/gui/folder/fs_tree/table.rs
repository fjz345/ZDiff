use super::*;

pub struct TwoFolderTable<'a, 'b> {
    pub compare: &'a TreeCompare,
    pub likeness_bar: bool,
    pub tolerance: &'a Tolerance,
    pub table: &'a mut TreeTable,
    pub linked: bool,
    pub swapped: bool,
    pub find: &'a mut TreeFindView<'b>,
}

pub enum TreeMode<'a, 'b> {
    TwoFolder(TwoFolderTable<'a, 'b>),
    /// Folder 1 alone, without the center column and Folder 2: the duplicates tab.
    SingleSide {
        pending_deletion: &'a HashSet<FsNodeId>,
        columns: Vec<NodeColumn<'a>>,
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

pub struct FolderTreeCtx<'a> {
    pub file_system_1_view: &'a mut Option<FileSystemView>,
    pub file_system_2_view: &'a mut Option<FileSystemView>,
    pub visible_rows: Option<&'a [VisibleRowTwoFolderDiff]>,
    pub rows_id: RowsId,
    pub root_requests: &'a mut RootRequests,
    pub diff_tool_config: &'a DiffToolConfig,
    pub tool_message: &'a mut Option<ToolMessage>,
    pub cursor: &'a mut TreeCursor,
    pub rows_cache: &'a mut CursorRowsCache,
    pub sort: &'a mut TreeSort,
    pub keyboard_taken: bool,
    pub actions: &'a [FolderAction],
    pub diff_filter: DiffFilter,
    pub scanning: bool,
    pub scan_error: Option<&'a str>,
}

pub fn draw_ui_folder_tree(
    ui: &mut egui::Ui,
    ctx: FolderTreeCtx,
    mode: TreeMode,
) -> egui::response::Response {
    let _scope = zcommon_ui::frame_stats::scope("Folder tree table");
    let FolderTreeCtx {
        file_system_1_view,
        file_system_2_view,
        visible_rows,
        rows_id,
        root_requests,
        diff_tool_config,
        tool_message,
        cursor,
        rows_cache,
        sort,
        keyboard_taken,
        actions,
        diff_filter,
        scanning,
        scan_error,
    } = ctx;
    let no_view = file_system_1_view.is_none() && file_system_2_view.is_none();
    // The app keeps the id of rows it dropped; the cache must not pair that id with no rows.
    let (visible_rows, rows_id) = if no_view {
        (&[][..], RowsId::default())
    } else {
        let rows = visible_rows.expect("the app builds the rows of a shown view");
        (rows, rows_id)
    };
    let flat = diff_filter.is_flat(*sort);
    let tool_message_before = tool_message.clone();

    // Every frame rather than at each rebuild or collapse: filter, rescan, the collapse
    // buttons and the expander arrows all change the rows, and this sees all of them.
    rows_cache.update(
        [file_system_1_view.as_ref(), file_system_2_view.as_ref()],
        visible_rows,
        rows_id,
        flat,
    );
    rows_cache.resolve(cursor);

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
                egui::Key::ArrowDown => (cursor.down(cursor_rows), None),
                egui::Key::ArrowUp => (cursor.up(cursor_rows), None),
                egui::Key::ArrowLeft => cursor.left(cursor_rows),
                egui::Key::ArrowRight => (cursor.clone(), cursor.right(cursor_rows)),
                egui::Key::Enter => (cursor.clone(), cursor.enter(cursor_rows)),
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
            FolderAction::NextDiff => cursor.next_diff(cursor_rows),
            FolderAction::PrevDiff => cursor.prev_diff(cursor_rows),
            FolderAction::OpenFolder1
            | FolderAction::OpenFolder2
            | FolderAction::Refresh
            | FolderAction::SwapSides
            | FolderAction::OpenKeybindings
            | FolderAction::Find
            | FolderAction::KeepThis
            | FolderAction::NextGroup
            | FolderAction::PrevGroup
            | FolderAction::ClearPaths
            | FolderAction::OpenStyle
            | FolderAction::OpenDiffTools
            | FolderAction::ToggleDiffMode
            | FolderAction::Export
            | FolderAction::ToggleMap
            | FolderAction::Undo
            | FolderAction::Redo => continue,
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
    let selected_row = rows_cache.selected_row(cursor);

    // Only drawn rows go to the table: it places row i at i * row height, both when
    // virtualizing and when scrolling to a row.
    let drawn_rows = rows_cache.drawn();
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
    let guides = (layout == RowLayout::Tree).then(|| rows_cache.guides());
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
    let mut side_rects = [egui::Rect::NOTHING; 2];
    let mut clicked_row = None;
    let mut toggle_row = None;
    let mut center_clicked_row = None;
    let mut center_double_clicked_row = None;
    let mut row_action = None;
    let mut chip_event = None;

    let (mut two_folder, pending_deletion, mut node_columns, mut extra_menu) = match mode {
        TreeMode::TwoFolder(two_folder) => (Some(two_folder), None, None, None),
        TreeMode::SingleSide {
            pending_deletion,
            columns,
            row_menu,
            ..
        } => (None, Some(pending_deletion), Some(columns), Some(row_menu)),
    };
    let shown = shown_sides(two_folder.as_ref().is_some_and(|t| t.swapped));
    let (find_matcher, match_path, find_chip_query) =
        two_folder
            .as_ref()
            .map_or((None, false, None), |two_folder| {
                let find = &two_folder.find;
                (find.matcher, find.match_path, find.chip)
            });

    let frame_output = egui::Frame::default()
        .fill(role_color(ui.ctx(), FolderRole::TableBackground))
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
            let row_tint = role_color(ui.ctx(), FolderRole::RowTint);
            let views = [file_system_1_view.as_ref(), file_system_2_view.as_ref()];
            let row_cell = |i: usize| {
                let entry = &visible_rows[drawn_rows[i]];
                // Selected cells get their text color from egui_extras; an explicit color
                // would override it.
                let dimmed =
                    selected_row != Some(i) && matches!(entry.diff_state, DiffState::Same(..));
                let pending = pending_deletion.is_some_and(|pending| {
                    entry
                        .diff_state
                        .first()
                        .is_some_and(|id| pending.contains(&id))
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
                    fill_cell(ui, ui.clip_rect().x_range(), row_tint);
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
                let columns = node_columns
                    .as_mut()
                    .expect("single-side mode has node columns");
                // Not `Column::remainder()`: egui_extras floors a non-last remainder column at
                // its widest content, pushing the node columns off screen.
                let name_width = ui.available_rect_before_wrap().width()
                    - ui.spacing().scroll.allocated_width()
                    - columns
                        .iter()
                        .map(|column| column.width + ui.spacing().item_spacing.x)
                        .sum::<f32>();
                let mut table = TableBuilder::new(ui)
                    .id_salt("single_side_table")
                    .striped(true)
                    .resizable(false)
                    .auto_shrink([false, false])
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::exact(name_width.max(0.0)));
                for column in columns.iter() {
                    table = table.column(Column::exact(column.width).clip(true));
                }
                if let Some(row) = scroll_to_row {
                    table = table.scroll_to_row(row, None);
                }
                let body = table
                    .header(header_height, |mut header| {
                        header.col(|ui| {
                            folder_header(ui, sort, None);
                        });
                        for column in columns.iter() {
                            header.col(|ui| {
                                ui.label(column.header);
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(row_height, row_count, |mut row| {
                            let i = row.index();
                            row.set_selected(selected_row == Some(i));
                            row.col(|ui| side_cell(i, 0, ui));
                            for column in columns.iter_mut() {
                                row.col(|ui| {
                                    if let Some(node_id) =
                                        visible_rows[drawn_rows[i]].diff_state.first()
                                    {
                                        (column.cell)(ui, node_id);
                                    }
                                });
                            }
                        });
                    });
                side_rects[0] = body.inner_rect;
                return;
            };
            let center = CenterLayout::measure(ui, two_folder.likeness_bar);
            let side_widths = two_folder.table.widths.get(
                ui,
                views,
                visible_rows,
                drawn_rows,
                rows_cache.generation(),
                layout,
            );
            let content_widths = shown.map(|side| side_widths[side]);
            let pitch = row_height + ui.spacing().item_spacing.y;
            let generation = rows_cache.generation();
            let anchored_offset =
                two_folder
                    .table
                    .anchored_offset(generation, visible_rows, drawn_rows, pitch);

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
            } else if let Some(offset) = anchored_offset {
                split_table = split_table.vertical_scroll_offset(offset);
            }
            let (compare, tolerance) = (two_folder.compare, two_folder.tolerance);
            let output = split_table.show(
                ui,
                header_height,
                |column, ui| match column {
                    SplitColumn::Left => {
                        chip_event = folder_header(ui, sort, find_chip_query);
                    }
                    SplitColumn::Center => center.show(ui, |slot, ui| match slot {
                        CenterSlot::State => {
                            ui.vertical(|ui| {
                                center.state_header(ui, |ui| {
                                    sort_header(ui, "≠", SortKey::State, sort)
                                });
                                if let Some(state) = root_state {
                                    center.state_symbol(ui, |ui| ui_custom_diff_state(ui, state));
                                }
                            });
                        }
                        CenterSlot::Likeness => {
                            ui.label("%").on_hover_text(
                                "Likeness of the two files, or of the files below a folder",
                            );
                        }
                        CenterSlot::Tag(_) => {}
                    }),
                    SplitColumn::Right => {}
                },
                Rows::Uniform {
                    height: row_height,
                    count: row_count,
                },
                |i, column, ui| match column {
                    SplitColumn::Left => side_cell(i, shown[0], ui),
                    SplitColumn::Right => side_cell(i, shown[1], ui),
                    SplitColumn::Center => {
                        let row = row_cell(i);
                        if row.entry.is_dir {
                            fill_cell(ui, ui.max_rect().x_range(), row_tint);
                        }
                        let cell = render_center_cell(ui, &row, &center, compare, tolerance, shown);
                        if cell.clicked {
                            center_clicked_row = Some(&visible_rows[drawn_rows[i]]);
                        }
                        if cell.double_clicked {
                            center_double_clicked_row = Some(&visible_rows[drawn_rows[i]]);
                        }
                    }
                },
            );
            side_rects = output.side_rects;
            if no_view {
                let body = side_rects[0].union(side_rects[1]);
                let mut overlay = ui.new_child(egui::UiBuilder::new().max_rect(body));
                if scanning {
                    loading_state(&mut overlay, "Scanning folders...");
                } else if let Some(error) = scan_error {
                    error_state(&mut overlay, error);
                }
            }
            two_folder.table.set_anchor(
                generation,
                output.scroll_offset,
                visible_rows,
                drawn_rows,
                pitch,
            );
        });
    if let Some(entry) = center_clicked_row {
        clicked_row = Some(entry);
    }
    if let Some(entry) = center_double_clicked_row {
        if !entry.is_dir {
            *tool_message = on_row_item_clicked(
                file_system_1_view.as_ref(),
                file_system_2_view.as_ref(),
                entry,
                diff_tool_config,
            );
        } else if !flat {
            toggle_row = Some(entry);
        }
    }
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

    if let Some(action) = row_action {
        apply_row_action(ui.ctx(), action, root_requests);
    }
    // The status line was drawn before this pane; show the new message next frame.
    if *tool_message != tool_message_before {
        ui.ctx().request_repaint();
    }

    frame_output.response
}

pub(super) fn shown_sides(swapped: bool) -> [usize; 2] {
    if swapped { [1, 0] } else { [0, 1] }
}

pub(super) fn folder_header(
    ui: &mut egui::Ui,
    sort: &mut TreeSort,
    find_chip_query: Option<&str>,
) -> Option<FindChipEvent> {
    ui.horizontal(|ui| {
        sort_header(ui, "Name", SortKey::Name, sort);
        find_chip_query.and_then(|query| find_chip(ui, query))
    })
    .inner
}

pub(super) fn find_chip(ui: &mut egui::Ui, query: &str) -> Option<FindChipEvent> {
    let shown = if query.chars().count() > FIND_CHIP_MAX_CHARS {
        let head: String = query.chars().take(FIND_CHIP_MAX_CHARS - 1).collect();
        format!("{head}…")
    } else {
        query.to_owned()
    };
    let reopen = ui
        .button(format!("🔎 {shown}"))
        .on_hover_text(format!("Find: {query}\nClick to edit"));
    let clear = zcommon_ui::common::icon_button(ui, "✖", "Clear find");
    if clear.clicked() {
        Some(FindChipEvent::Clear)
    } else if reopen.clicked() {
        Some(FindChipEvent::Reopen)
    } else {
        None
    }
}

/// A clickable column header for `key`. The active one shows the direction.
pub(super) fn sort_header(ui: &mut egui::Ui, text: &str, key: SortKey, sort: &mut TreeSort) {
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
