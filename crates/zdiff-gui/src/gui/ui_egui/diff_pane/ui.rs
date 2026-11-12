use super::*;

impl FileDiffPane {
    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut FileDiffPaneCtx) -> egui_tiles::UiResponse {
        // No-op once registered.
        ui.ctx().add_plugin(CopyMarkerPlugin);
        self.occurrences.begin_frame(ui.ctx());
        let mut side_viewers = ctx.side_viewers.take();

        let text_viewer = ctx.hex_view.is_none()
            && ctx.image_view.is_none()
            && ctx.media_view.is_none()
            && side_viewers.is_none();
        let strings = ctx
            .hex_mode
            .as_ref()
            .is_some_and(|settings| settings.mode == HexMode::Strings);
        let files = ctx.diff_ctx.map(|diff_ctx| &diff_ctx.input);
        // A one-sided diff is the file against itself, so its rows carry it on both sides.
        let has_file = [
            files.is_some_and(|input| input.file_1.is_some()),
            files.is_some_and(|input| input.file_2.is_some()),
        ];
        let number_width = line_number_width_for(
            ui,
            [
                files.and_then(|input| input.file_1.as_deref()),
                files.and_then(|input| input.file_2.as_deref()),
            ],
        );
        let side_gutter = if strings {
            Gutter::Offset(None)
        } else {
            Gutter::Single { number_width }
        };
        let patch = text_viewer && !strings && *ctx.text_mode == TextMode::Patch;
        let unified = match ctx.diff_ctx {
            Some(diff_ctx)
                if text_viewer && !strings && !patch && *ctx.text_mode != TextMode::SideBySide =>
            {
                Some(unified_view(
                    &mut self.unified,
                    &diff_ctx.diff_rows,
                    *ctx.text_mode,
                ))
            }
            _ => None,
        };
        let single_side = unified.as_ref().and_then(|view| view.mode.single_side());
        let shown = shown_sides(ctx.sides_swapped);
        let data_column = |column: SplitColumn| match column {
            SplitColumn::Center => SplitColumn::Center,
            SplitColumn::Left => [SplitColumn::Left, SplitColumn::Right][shown[0]],
            SplitColumn::Right => [SplitColumn::Left, SplitColumn::Right][shown[1]],
        };
        let column_gutter = if single_side.is_some() {
            Gutter::Single { number_width }
        } else {
            Gutter::Unified {
                left: None,
                right: None,
                sign: None,
                number_width,
            }
        };
        let content_widths = match (ctx.diff_ctx, &unified) {
            (Some(diff_ctx), Some(view)) => {
                let [left, _] = self.content_widths(ui, &view.rows, diff_ctx);
                let extra = column_gutter.width() - GUTTER_WIDTH;
                [if left > 0.0 { left + extra } else { left }, 0.0]
            }
            (Some(diff_ctx), None) => {
                let extra = side_gutter.width() - GUTTER_WIDTH;
                self.content_widths(ui, &diff_ctx.diff_rows, diff_ctx)
                    .map(|width| if width > 0.0 { width + extra } else { width })
            }
            (None, _) => [0.0; 2],
        };

        log::trace!("=================================================");

        let available_width = ui.available_width();
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
        let wrap = *ctx.word_wrap;
        if !wrap {
            self.row_heights = None;
        }
        let patch_rows = ctx
            .diff_ctx
            .filter(|_| patch)
            .map(|diff_ctx| patch_view(&mut self.patch, diff_ctx));
        let row_heights = &mut self.row_heights;
        let ruler_cache = &mut self.ruler_spans;
        let ruler_bins = &mut self.ruler_bins;
        let occurrences = &mut self.occurrences;
        let pin_input = &mut self.pin_input;
        let split = &mut self.split;
        let header_height = &mut self.header_height;
        let center_layout_cache = &mut self.center_layout;
        let mono_font = egui::TextStyle::Monospace.resolve(ui.style());
        let egui_ctx = ui.ctx().clone();
        let keymap = ctx.keymap;
        let hint = |action: FileAction, text: &str| keymap.with_shortcut(action, text);
        let action_hint = |action: FileAction| hint(action, action.label());

        toolbar(ui, |ui| {
            toolbar_group(ui, |ui| {
                viewer_override_combo(ui, ctx.viewer_override, ctx.viewer_fallback);
                if let Some(settings) = ctx.hex_mode.as_deref_mut() {
                    hex_mode_ui(ui, settings);
                }
                if text_viewer && !strings {
                    text_mode_combo(ui, ctx.text_mode);
                }
                if let Some(options) = ctx.pdb_options.as_deref_mut() {
                    pdb_options_menu(ui, options);
                }
            });
            ui.separator();

            toolbar_group(ui, |ui| {
                let mut held = ctx.toggle_all_held;
                if toggle_button(
                    ui,
                    &mut held,
                    egui::RichText::new("All").strong(),
                    &hint(FileAction::ToggleAllOptions, "Toggle All Options"),
                ) {
                    *ctx.toggle_all_request = true;
                }
                toggle_button(
                    ui,
                    &mut ctx.diff_options.ignore.whitespace,
                    egui::RichText::new("WS").strong(),
                    &hint(FileAction::IgnoreWhitespace, "Ignore Whitespace"),
                );
                toggle_button(
                    ui,
                    &mut ctx.diff_options.ignore.comments,
                    egui::RichText::new("//").monospace().strong(),
                    &hint(FileAction::IgnoreComments, "Ignore Comments"),
                );
                toggle_button(
                    ui,
                    &mut ctx.diff_options.ignore.line_endings,
                    egui::RichText::new("EOL").strong(),
                    &hint(
                        FileAction::IgnoreLineEndings,
                        "Ignore Line Endings: CRLF, LF and CR are equal",
                    ),
                );
                toggle_button(
                    ui,
                    &mut ctx.diff_options.ignore.file_markers,
                    egui::RichText::new("BOM").strong(),
                    &hint(
                        FileAction::IgnoreFileMarkers,
                        "Ignore File Markers: a BOM at the start and the line break at the end of the file",
                    ),
                );
                ignore_patterns_btn(
                    ui,
                    TOGGLE_BUTTON_SIZE,
                    &mut ctx.diff_options.ignore.patterns,
                    ctx.diff_options.ignore.regex_enabled,
                    ctx.regex_ignore_presets,
                    &hint(
                        FileAction::IgnoreRegex,
                        "Ignore Regex: text matching these patterns is ignored and dimmed",
                    ),
                );
                toggle_button(
                    ui,
                    &mut ctx.diff_options.highlight_rows,
                    egui::RichText::new("HL").strong(),
                    &hint(FileAction::HighlightRows, "Highlight Rows"),
                );
                options_menu_button(ui, "Diff marks", |ui| {
                    diff_marks_checkboxes(ui, ctx.diff_options)
                });
                toggle_button(
                    ui,
                    &mut ctx.diff_options.ghost_rows,
                    egui::RichText::new("GR").strong(),
                    &hint(FileAction::GhostRows, "Ghost Rows"),
                );
                toggle_button(
                    ui,
                    ctx.word_wrap,
                    egui::RichText::new("Wrap").strong(),
                    &hint(FileAction::WordWrap, "Word Wrap"),
                );

                let mut active = ctx.diff_options.diff_only_with_extra_rows.is_some();
                if toggle_button(
                    ui,
                    &mut active,
                    egui::RichText::new("D").strong(),
                    &hint(FileAction::DiffOnly, "Diff Only"),
                ) {
                    ctx.diff_options.diff_only_with_extra_rows =
                        active.then_some(DEFAULT_CONTEXT_ROWS);
                }
                if let Some(diff_rows_num) = &mut ctx.diff_options.diff_only_with_extra_rows {
                    match stepper(
                        ui,
                        *diff_rows_num,
                        0..=MAX_CONTEXT_ROWS,
                        |rows| rows.to_string(),
                        &action_hint(FileAction::FewerContextRows),
                        &action_hint(FileAction::MoreContextRows),
                        &format!("Reset to {DEFAULT_CONTEXT_ROWS} context rows"),
                    ) {
                        Some(Step::Dec) => *diff_rows_num = diff_rows_num.saturating_sub(1),
                        Some(Step::Inc) => {
                            *diff_rows_num = (*diff_rows_num + 1).min(MAX_CONTEXT_ROWS)
                        }
                        Some(Step::Reset) => *diff_rows_num = DEFAULT_CONTEXT_ROWS,
                        Some(Step::Set(rows)) => *diff_rows_num = rows,
                        None => {}
                    }
                }
            });
            ui.separator();

            toolbar_group(ui, |ui| {
                let conflict_max = ctx.conflict_cursor.get_max();
                match stepper(
                    ui,
                    ctx.conflict_cursor.get(),
                    0..=conflict_max,
                    |cursor| format!("{cursor}/{conflict_max}"),
                    &action_hint(FileAction::PrevConflict),
                    &action_hint(FileAction::NextConflict),
                    "Clear conflict selection",
                ) {
                    Some(Step::Dec) => ctx.conflict_cursor.dec(),
                    Some(Step::Inc) => ctx.conflict_cursor.inc(),
                    Some(Step::Reset) => ctx.conflict_cursor.set(0),
                    Some(Step::Set(cursor)) => ctx.conflict_cursor.set(cursor),
                    None => {}
                }
            });
            toolbar_group(ui, |ui| {
                let find_count = ctx.find_count;
                match stepper(
                    ui,
                    ctx.find_cursor.get(),
                    0..=find_count.saturating_sub(1),
                    |cursor| {
                        let find_k = if find_count == 0 { 0 } else { cursor + 1 };
                        format!("{find_k}/{find_count}")
                    },
                    &action_hint(FileAction::PrevFind),
                    &action_hint(FileAction::NextFind),
                    "Go to first match",
                ) {
                    Some(Step::Dec) => ctx.find_cursor.dec(),
                    Some(Step::Inc) => ctx.find_cursor.inc(),
                    Some(Step::Reset) => ctx.find_cursor.set(0),
                    Some(Step::Set(cursor)) => ctx.find_cursor.set(cursor),
                    None => {}
                }
            });
            toolbar_group(ui, |ui| {
                pins_ui(ui, pin_input, ctx.pins, ctx.pins_request, ctx.diff_ctx);
            });
            toolbar_end(ui, |ui| {
                if side_viewers.is_none() {
                    likeness_ui(ui, ctx, text_viewer);
                }
                // No diff ctx while a diff is in flight, so a partial time is never shown.
                if let Some(diff_ctx) = ctx.diff_ctx {
                    let (label, tooltip) = diff_status_text(
                        diff_ctx.num_add_deletes,
                        diff_ctx.change_unit,
                        &diff_ctx.stage_times,
                    );
                    let label = diff_status_job(
                        &label,
                        diff_ctx.num_add_deletes,
                        &egui::TextStyle::Body.resolve(ui.style()),
                        [
                            role_color(ui.ctx(), SharedRole::InsertText),
                            role_color(ui.ctx(), SharedRole::DeleteText),
                        ],
                    );
                    // Not selectable, so a drag-copy over the rows can't pick it up.
                    ui.add(egui::Label::new(label).selectable(false))
                        .on_hover_text(tooltip);
                } else if let Some(sides) = &side_viewers {
                    let text = side_viewers::status_text(sides.kinds);
                    let text = egui::RichText::new(text).color(ui.visuals().warn_fg_color);
                    ui.add(egui::Label::new(text).selectable(false));
                } else if let Some(hex_view) = &ctx.hex_view {
                    ui.add(egui::Label::new(hex_view.status_text()).selectable(false));
                } else if let Some(image_view) = &ctx.image_view {
                    let (text, size_mismatch) = image_view.status_text();
                    let mut text = egui::RichText::new(text);
                    if size_mismatch {
                        text = text.color(ui.visuals().warn_fg_color);
                    }
                    ui.add(egui::Label::new(text).selectable(false));
                }
            });
        });

        if let Some(reason) = ctx.viewer_fallback {
            ui.colored_label(ui.visuals().warn_fg_color, format!("⚠ {reason}"));
        }

        let diff_rows = ctx.diff_ctx.as_ref().map(|f| &f.diff_rows);
        let source_path = ctx
            .diff_ctx
            .map(|f| f.input.file_1.as_ref().map(|f| &f.path))
            .unwrap_or_default();
        let target_path = ctx
            .diff_ctx
            .map(|f| f.input.file_2.as_ref().map(|f| &f.path))
            .unwrap_or_default();

        let press_pos = ui.input(|i| {
            i.pointer
                .primary_pressed()
                .then(|| i.pointer.interact_pos())
                .flatten()
        });
        if ctx.toggle_active_side {
            self.active_side.toggle();
        }
        // A mismatched pair's text side is diffed alone, as the left file.
        let side_text = side_viewers.as_ref().is_some_and(|s| s.text.is_some());
        let active_side = self.active_side.begin_frame(
            press_pos,
            source_path.is_some() || side_text,
            target_path.is_some(),
        );
        let active_side = single_side.unwrap_or(active_side);
        *ctx.active_side = active_side;

        let mut waiting_for_diff = false;
        let mut do_not_render_diff = match (&diff_rows, source_path, target_path) {
            (Some(_), None, None) | (None, None, None) => true,
            (None, Some(_), None) => true,
            (None, None, Some(_)) => true,
            (Some(_), Some(_), None) | (Some(_), None, Some(_)) => false,
            (None, Some(_), Some(_)) => {
                waiting_for_diff = true;
                true
            }
            (Some(_), Some(_), Some(_)) => false,
        };
        waiting_for_diff |= ctx.diff_loading;
        do_not_render_diff |= waiting_for_diff;
        // The hex and image viewers draw the loaded bytes and need no text diff.
        let other_viewer = ctx.hex_view.is_some()
            || ctx.image_view.is_some()
            || ctx.media_view.is_some()
            || side_viewers.is_some();
        if other_viewer {
            waiting_for_diff = false;
            do_not_render_diff = false;
        }
        let diff_rows_len = diff_rows.map(|f| f.len()).unwrap_or_else(|| 0);

        ui.add_space(4.0);
        ui.style_mut().override_text_style = Some(egui::TextStyle::Monospace);
        ui.spacing_mut().item_spacing.y = 0.0;

        if let Some(hex_view) = &ctx.hex_view {
            hex_view.inspector_ui(ui);
        }
        let hex_key_row = ctx.hex_view.as_ref().and_then(|hex_view| hex_view.cursor_keys_ui(ui));
        let table_height = ui.available_height();
        let hex_len = ctx.hex_view.as_ref().map_or(0, hex::byte_len);
        let show_ruler = if ctx.hex_view.is_some() {
            hex_len > 0
        } else {
            !other_viewer && !patch && !do_not_render_diff && diff_rows_len > 0
        };
        let ruler_id = egui::Id::new("file_diff_ruler");

        let mut side_rects = [egui::Rect::NOTHING; 2];
        let mut header_rects = [egui::Rect::NOTHING; 2];
        let mut browse: [Option<String>; 2] = [None, None];
        let mut back = [false; 2];
        let content_rect = ui.vertical(|ui| {
            ui.set_min_width(available_width);

            if table_height > 0.0 {
                // Without a diff only the path bars show; the pane state fills the rest.
                let height = if do_not_render_diff {
                    header_height.min(table_height)
                } else {
                    table_height
                };
                ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                    egui::Frame::default()
                        .fill(role_color(ui.ctx(), FileRole::TableBackground))
                        .show(ui, |ui| {
                            let full_area = ui.available_rect_before_wrap();
                            let mut area = full_area;
                            if show_ruler {
                                area.max.x -= RULER_WIDTH;
                            }
                            let button_padding = ui.spacing().button_padding.x;
                            let center = CenterLayout {
                                toggles: ctx.diff_ctx.is_some_and(|d| !d.row_blocks.is_empty()),
                                ..center_layout(
                                    center_layout_cache,
                                    &mono_font,
                                    ui.ctx().pixels_per_point(),
                                    || {
                                        ui.fonts_mut(|fonts| {
                                            CenterLayout::measure(
                                                |c| fonts.glyph_width(&mono_font, c),
                                                button_padding,
                                            )
                                        })
                                    },
                                )
                            };
                            let side_widths = split.widths(shared_width(
                                area.width(),
                                center.width(),
                                ui.spacing().item_spacing.x,
                            ));
                            let mut table = SplitTable::new("file_diff_table", &mut *split)
                                .center_width(center.width());
                            // Images are drawn below the header (the path editors), in the
                            // table's columns, outside its scroll area so the wheel zooms.
                            let mut image_left_width = None;
                            if unified.is_some() {
                                table = table.single_column();
                            }

                            let ruler_row = ui.data_mut(|d| d.remove_temp::<usize>(ruler_id.with("scroll_to")));
                            if let Some(row) = ruler_row {
                                table = table.scroll_to_row(row, Some(egui::Align::Center));
                            } else if let Some(span) = ctx.scroll_to_row_span {
                                log::trace!("scroll_to_row_span: {:?}", span);
                                let start = match &unified {
                                    Some(view) => {
                                        unified_scroll_index(&view.meta, span).unwrap_or(span.start)
                                    }
                                    None => span.start,
                                };
                                table = table.scroll_to_row(start, Some(egui::Align::Min));
                            } else if let Some(row) = hex_key_row {
                                table = table.scroll_to_row(row, None);
                            }

                            let root_path_bar = |ui: &mut egui::Ui,
                                                 id_salt: &str,
                                                 root: Option<&UniversalPath>,
                                                 is_valid: bool,
                                                 spinner_active: bool|
                             -> Option<UniversalPath> {
                                let root = root.cloned().unwrap_or_default();
                                let segments = bar_segments(&root, None, "No root folder");
                                let bar = ui.scope(|ui| {
                                    path_bar(
                                        ui,
                                        id_salt,
                                        &segments,
                                        usize::from(root.is_empty()),
                                        |i| i + 1 < segments.len(),
                                        (!is_valid).then_some("Invalid path"),
                                        true,
                                    )
                                });
                                if spinner_active {
                                    paint_busy(ui, bar.response.rect);
                                }
                                root_path_request(bar.inner?, &root)
                            };
                            let dim = file_path_dim(
                                [&ctx.file_source_path, &ctx.file_target_path],
                                ctx.file_source_label.is_some() || ctx.file_target_label.is_some(),
                            );
                            let file_path_bar = |ui: &mut egui::Ui,
                                                 id_salt: &str,
                                                 path: &UniversalPath,
                                                 label: Option<&str>,
                                                 previous: Option<&UniversalPath>,
                                                 root: Option<&UniversalPath>,
                                                 is_valid: bool,
                                                 load_error: Option<&str>,
                                                 spinner_active: bool|
                             -> (Option<UniversalPath>, Option<String>, bool) {
                                let segments = bar_segments(path, label, "No file");
                                let dim = if path.is_empty() { segments.len() } else { dim };
                                let full_path = path.to_string();
                                let reason = load_error.unwrap_or("Invalid path");
                                let error = match (label, is_valid) {
                                    (_, true) => None,
                                    (Some(_), false) => Some(format!("{full_path}\n{reason}")),
                                    (None, false) => Some(reason.to_owned()),
                                };
                                let (picked, bar) = ui
                                    .horizontal(|ui| {
                                        let picked = (label.is_none() && !path.is_empty())
                                            .then(|| revision_dropdown(ui, id_salt, path, previous))
                                            .flatten();
                                        let bar = ui.scope(|ui| {
                                            path_bar(
                                                ui,
                                                id_salt,
                                                &segments,
                                                dim,
                                                |i| i + 1 < segments.len(),
                                                error.as_deref(),
                                                true,
                                            )
                                        });
                                        (picked, bar)
                                    })
                                    .inner;
                                let rect = bar.response.rect;
                                if label.is_some() && is_valid && ui.rect_contains_pointer(rect) {
                                    bar.response.show_tooltip_text(&full_path);
                                }
                                if spinner_active {
                                    paint_busy(ui, rect);
                                }
                                match bar.inner {
                                    Some(PathBarAction::Segment(i)) => {
                                        (None, Some(segments[i].path.clone()), false)
                                    }
                                    Some(PathBarAction::Commit(text)) => {
                                        (file_path_request(&text, path, root), None, false)
                                    }
                                    None => match picked {
                                        Some(picked) if Some(&picked) == previous => {
                                            (None, None, true)
                                        }
                                        picked => (
                                            picked.and_then(|picked| {
                                                file_path_request(&picked.to_string(), path, root)
                                            }),
                                            None,
                                            false,
                                        ),
                                    },
                                }
                            };
                            let mut header = |column: SplitColumn, ui: &mut egui::Ui| {
                                // Clipped cells truncate text; the path bars lay out as before.
                                ui.style_mut().wrap_mode = None;
                                match column {
                                    SplitColumn::Left => {
                                        header_rects[0] = ui
                                            .vertical(|ui| {
                                                *ctx.set_file_1_root_request = root_path_bar(
                                                    ui,
                                                    "file_1_root_path_bar",
                                                    ctx.file_source_root.as_ref(),
                                                    ctx.file_source_root_valid,
                                                    ctx.file_source_loading,
                                                );
                                                ui.separator();
                                                (*ctx.load_file_1_request, browse[0], back[0]) = file_path_bar(
                                                    ui,
                                                    "file_1_path_bar",
                                                    &ctx.file_source_path,
                                                    ctx.file_source_label.as_deref(),
                                                    ctx.file_source_previous.as_ref(),
                                                    ctx.file_source_root.as_ref(),
                                                    ctx.file_source_path_valid,
                                                    ctx.file_source_load_error.as_deref(),
                                                    ctx.file_source_loading,
                                                );
                                                ui.separator();
                                            })
                                            .response
                                            .rect;
                                    }
                                    SplitColumn::Center => {}
                                    SplitColumn::Right => {
                                        header_rects[1] = ui
                                            .vertical(|ui| {
                                                *ctx.set_file_2_root_request = root_path_bar(
                                                    ui,
                                                    "file_2_root_path_bar",
                                                    ctx.file_target_root.as_ref(),
                                                    ctx.file_target_root_valid,
                                                    ctx.file_target_loading,
                                                );
                                                ui.separator();
                                                (*ctx.load_file_2_request, browse[1], back[1]) = file_path_bar(
                                                    ui,
                                                    "file_2_path_bar",
                                                    &ctx.file_target_path,
                                                    ctx.file_target_label.as_deref(),
                                                    ctx.file_target_previous.as_ref(),
                                                    ctx.file_target_root.as_ref(),
                                                    ctx.file_target_path_valid,
                                                    ctx.file_target_load_error.as_deref(),
                                                    ctx.file_target_loading,
                                                );
                                                ui.separator();
                                            })
                                            .response
                                            .rect;
                                    }
                                }
                            };
                            // Unified: one column holds both path bars, side by side. Single file:
                            // the chosen side's.
                            let header = |column: SplitColumn, ui: &mut egui::Ui| {
                                if let Some(side) = single_side {
                                    header(
                                        match side {
                                            ActiveSide::Left => SplitColumn::Left,
                                            ActiveSide::Right => SplitColumn::Right,
                                        },
                                        ui,
                                    );
                                } else if unified.is_some() {
                                    ui.columns(2, |columns| {
                                        header(data_column(SplitColumn::Left), &mut columns[0]);
                                        header(data_column(SplitColumn::Right), &mut columns[1]);
                                    });
                                } else {
                                    header(data_column(column), ui);
                                }
                            };
                            let header_only = ctx.image_view.is_some()
                                || ctx.media_view.is_some()
                                || side_viewers.is_some()
                                || patch
                                || do_not_render_diff;
                            let mut offsets = [
                                *ctx.scroll_left,
                                if unified.is_some() { 0.0 } else { *ctx.scroll_right },
                            ];
                            // Wrapped text fits its column, so nothing scrolls sideways. Hex rows
                            // have a fixed width, images pan.
                            if !(wrap || header_only || ctx.hex_view.is_some()) {
                                let column_widths = match unified {
                                    Some(_) => content_widths,
                                    None => shown.map(|side| content_widths[side]),
                                };
                                table = table.h_scroll(
                                    &mut offsets,
                                    *ctx.h_scroll_linked,
                                    column_widths,
                                );
                            }
                            let mut ruler_tops: &[f32] = &[];
                            let rows = if let Some(hex_view) = &ctx.hex_view {
                                Rows::Uniform {
                                    height: row_height,
                                    count: hex::row_count(hex_view),
                                }
                            } else if header_only {
                                Rows::Uniform {
                                    height: row_height,
                                    count: 0,
                                }
                            } else {
                                let (measured, text_widths, count) = match &unified {
                                    Some(view) => (
                                        Some(&view.rows),
                                        [column_gutter.text_width(area.width()), 0.0],
                                        view.meta.len(),
                                    ),
                                    None => (
                                        ctx.diff_ctx.map(|d| &d.diff_rows),
                                        shown.map(|column| side_gutter.text_width(side_widths[column])),
                                        diff_rows_len,
                                    ),
                                };
                                let heights = match (ctx.diff_ctx, measured) {
                                    (Some(diff_ctx), Some(rows)) if wrap => {
                                        let cached = wrapped_heights(
                                            row_heights,
                                            &egui_ctx,
                                            &mono_font,
                                            rows,
                                            diff_ctx.input.file_1.as_deref(),
                                            diff_ctx.input.file_2.as_deref(),
                                            text_widths,
                                        );
                                        ruler_tops = &cached.tops;
                                        Some(cached.heights.as_slice())
                                    }
                                    _ => None,
                                };
                                text_rows(heights, row_height, count)
                            };

                                    // Once per frame, not per row: a temp side gets no revert buttons.
                                    let temp_root = std::env::temp_dir();
                                    let text_diff = ctx.diff_ctx.filter(|_| !strings);
                                    let character_phase =
                                        text_diff.is_some_and(|d| d.input.options.phases.character);
                                    let marks = Marks::new(ctx.diff_options, character_phase);
                                    let changed_scopes = text_diff.map(|d| &*d.changed_scopes);
                                    let row_marks = |side: ActiveSide, content: &LineContent| {
                                        let in_changed_scope = match (content, changed_scopes) {
                                            (LineContent::Code { line_num, .. }, Some(scopes)) => {
                                                in_scopes(&scopes[side as usize], *line_num)
                                            }
                                            _ => false,
                                        };
                                        Marks {
                                            scope: marks.scope && in_changed_scope,
                                            ..marks
                                        }
                                    };
                                    let shown_pins = ctx.diff_ctx.map_or(&[][..], shown_pins);
                                    let symbol_colors = [
                                        role_color(&egui_ctx, SharedRole::DeleteText),
                                        role_color(&egui_ctx, SharedRole::InsertText),
                                        role_color(&egui_ctx, FileRole::Modified),
                                    ];
                                    let overlay = ctx
                                        .diff_ctx
                                        .filter(|_| ctx.phase_overlay && !strings)
                                        .and_then(|d| d.phase_trace.as_deref());
                                    let cell = |row_index: usize, column: SplitColumn, ui: &mut egui::Ui| {
                                            let column = if unified.is_some() { column } else { data_column(column) };
                                            if let Some(hex_view) = &ctx.hex_view {
                                                hex::cell(ui, hex_view, row_index, column);
                                                return;
                                            }
                                            if let (Some(view), Some(rows)) = (&unified, diff_rows) {
                                                if column == SplitColumn::Left {
                                                    let u = view.meta[row_index];
                                                    let content = &view.rows[row_index].left;
                                                    let context = u.kind == UnifiedKind::Context;
                                                    let marked = ctx.active_highlights.binary_search(&u.row).is_ok();
                                                    let lit = marked
                                                        && (context
                                                            || ctx.highlight_side.is_none_or(|s| s == u.side));
                                                    let accent = marked
                                                        && (context || ctx.highlight_side == Some(u.side));
                                                    let (matchers, hit) = (ctx.find_matchers, ctx.find_hit);
                                                    let mut find = row_find(matchers, hit, u.side, u.row, content);
                                                    if context && single_side.is_none() && find.current.is_none() {
                                                        let right = row_find(matchers, hit, ActiveSide::Right, u.row, &rows[u.row].right);
                                                        if right.current.is_some() || find.matcher.is_none() {
                                                            find = right;
                                                        }
                                                    }
                                                    let width = ui.max_rect().width();
                                                    let hit = Self::render_row(
                                                        ui,
                                                        RowCtx {
                                                            side: u.side,
                                                            row_index,
                                                            occurrences,
                                                            file_source: ctx.diff_ctx.map(|f| f.input.file_1.clone()).unwrap_or_default(),
                                                            file_target: ctx.diff_ctx.map(|f| f.input.file_2.clone()).unwrap_or_default(),
                                                            content,
                                                            width,
                                                            wrap,
                                                            is_highlighted: lit,
                                                            accent_line_num: accent,
                                                            find,
                                                            selectable: true,
                                                            code_language: ctx.code_language[u.side as usize],
                                                            trace: None,
                                                        },
                                                        if single_side.is_some() {
                                                            Gutter::Single { number_width }
                                                        } else {
                                                            Gutter::Unified {
                                                                left: u.left_line.filter(|_| has_file[0]),
                                                                right: u.right_line.filter(|_| has_file[1]),
                                                                sign: match u.kind {
                                                                    UnifiedKind::Delete => Some(('-', symbol_colors[0])),
                                                                    UnifiedKind::Insert => Some(('+', symbol_colors[1])),
                                                                    UnifiedKind::Context | UnifiedKind::Collapsed => None,
                                                                },
                                                                number_width,
                                                            }
                                                        },
                                                        row_marks(u.side, content),
                                                    );
                                                    if let (Some(hit), Some(diff_ctx), Some(at)) =
                                                        (hit, text_diff, MenuLine::of(u.side, content, u.row))
                                                    {
                                                        hit.context_menu(|ui| {
                                                            line_menu(ui, at, LineMenuCtx {
                                                                diff_ctx,
                                                                pins: ctx.pins,
                                                                pin_input: &mut *pin_input,
                                                                pins_request: &mut *ctx.pins_request,
                                                                revert_request: &mut *ctx.revert_request,
                                                                temp_root: &temp_root,
                                                            })
                                                        });
                                                    }
                                                    if is_pinned(shown_pins, u.side, content) {
                                                        let cell_ui = ui.new_child(
                                                            UiBuilder::new().max_rect(unscrolled_cell(ui)),
                                                        );
                                                        paint_pin_marker(&cell_ui);
                                                    }
                                                    let block = ctx.diff_ctx.and_then(|d| {
                                                        d.row_blocks
                                                            .binary_search_by_key(&u.row, |b| b.rows.start)
                                                            .ok()
                                                            .map(|i| &d.row_blocks[i])
                                                    });
                                                    if let Some(block) = block
                                                        && matches!(u.kind, UnifiedKind::Context | UnifiedKind::Collapsed)
                                                    {
                                                        let cell = unscrolled_cell(ui);
                                                        let strip = egui::Rect::from_min_size(
                                                            egui::pos2(cell.right() - center.width(), cell.top()),
                                                            egui::vec2(center.width(), cell.height()),
                                                        );
                                                        let [expand_slot, toggle_slot, ..] =
                                                            center.slots(strip, ui.spacing().interact_size.y);
                                                        let mut buttons = ui.new_child(UiBuilder::new().max_rect(cell));
                                                        block_toggle_buttons(
                                                            &mut buttons,
                                                            block,
                                                            [expand_slot, toggle_slot],
                                                            &mut *ctx.block_toggle_request,
                                                        );
                                                    }
                                                }
                                                return;
                                            }
                                            if let Some(rows) = diff_rows {
                                                let diff_row = &rows[row_index];
                                                let is_highlighted =
                                                    ctx.active_highlights.binary_search(&row_index).is_ok();
                                                let highlight_side = ctx.highlight_side;
                                                let lit = |side| {
                                                    is_highlighted
                                                        && highlight_side.is_none_or(|s| s == side)
                                                };
                                                let accent_line_num = |side| {
                                                    is_highlighted && highlight_side == Some(side)
                                                };
                                                let (find_matchers, find_hit) =
                                                    (ctx.find_matchers, ctx.find_hit);

                                                match column {
                                                SplitColumn::Left | SplitColumn::Right => {
                                                    let (side, content) = if column == SplitColumn::Left {
                                                        (ActiveSide::Left, &diff_row.left)
                                                    } else {
                                                        (ActiveSide::Right, &diff_row.right)
                                                    };
                                                    if !has_file[side as usize] {
                                                        return;
                                                    }
                                                    let width = ui.max_rect().width();
                                                    let gutter = match (strings, content) {
                                                        (true, LineContent::Code { line_num, .. }) => {
                                                            let offsets = ctx.string_offsets[side as usize];
                                                            Gutter::Offset(
                                                                usize::try_from(*line_num)
                                                                    .ok()
                                                                    .and_then(|n| line_offset(offsets, n)),
                                                            )
                                                        }
                                                        (true, _) => Gutter::Offset(None),
                                                        (false, _) => Gutter::Single { number_width },
                                                    };
                                                        let hit = Self::render_row(
                                                            ui,
                                                            RowCtx {
                                                                side,
                                                                row_index,
                                                                occurrences,
                                                                file_source: ctx.diff_ctx
                                                                    .map(|f| f.input.file_1.clone())
                                                                    .unwrap_or_default(),
                                                                file_target: ctx.diff_ctx
                                                                    .map(|f| f.input.file_2.clone())
                                                                    .unwrap_or_default(),
                                                                content,
                                                                width,
                                                                wrap,
                                                                is_highlighted: lit(side),
                                                                accent_line_num: accent_line_num(side),
                                                                find: row_find(
                                                                    find_matchers,
                                                                    find_hit,
                                                                    side,
                                                                    row_index,
                                                                    content,
                                                                ),
                                                                selectable: active_side == side,
                                                                code_language: ctx.code_language[side as usize],
                                                                trace: overlay,
                                                            },
                                                            gutter,
                                                            row_marks(side, content),
                                                        );
                                                    if let (Some(hit), Some(diff_ctx), Some(at)) =
                                                        (hit, text_diff, MenuLine::of(side, content, row_index))
                                                    {
                                                        hit.context_menu(|ui| {
                                                            line_menu(ui, at, LineMenuCtx {
                                                                diff_ctx,
                                                                pins: ctx.pins,
                                                                pin_input: &mut *pin_input,
                                                                pins_request: &mut *ctx.pins_request,
                                                                revert_request: &mut *ctx.revert_request,
                                                                temp_root: &temp_root,
                                                            })
                                                        });
                                                    }
                                                    let pinned = is_pinned(shown_pins, side, content);
                                                    if pinned || overlay.is_some() {
                                                        let cell_ui = ui.new_child(
                                                            UiBuilder::new().max_rect(unscrolled_cell(ui)),
                                                        );
                                                        if pinned {
                                                            paint_pin_marker(&cell_ui);
                                                        }
                                                        if let Some(trace) = overlay {
                                                            paint_trace_gutter(&cell_ui, trace, side, content, gutter.width());
                                                        }
                                                    }
                                                }

                                                SplitColumn::Center => {
                                                    ui.style_mut().wrap_mode = None;
                                                    let [expand_slot, toggle_slot, left_slot, symbol_slot, right_slot] =
                                                        center.slots(ui.max_rect(), ui.spacing().interact_size.y);
                                                    let diff_ctx = ctx.diff_ctx;
                                                    let hunk = diff_ctx.filter(|_| !strings).and_then(|d| {
                                                        revert::hunk_starting_at(&d.precomputed_diffs, row_index)
                                                            .map(|hunk| (d, hunk))
                                                    });
                                                    let mut revert_button = |ui: &mut egui::Ui, slot: egui::Rect, target: RevertTarget, label: &str| {
                                                        let Some((diff_ctx, hunk)) = hunk else {
                                                            return;
                                                        };
                                                        let hover = match target {
                                                            RevertTarget::Left => "Replace this hunk in the left file with the right side",
                                                            RevertTarget::Right => "Replace this hunk in the right file with the left side",
                                                        };
                                                        if revert::check_revert(diff_ctx, target, &temp_root).is_ok()
                                                            && ui.put(slot, egui::Button::new(label)).on_hover_text(hover).clicked()
                                                        {
                                                            *ctx.revert_request = Some(RevertRequest { hunk, target });
                                                        }
                                                    };
                                                    // A collapsed row, or the first row of an expanded block.
                                                    let block = diff_ctx.and_then(|d| {
                                                        d.row_blocks
                                                            .binary_search_by_key(&row_index, |b| b.rows.start)
                                                            .ok()
                                                            .map(|i| &d.row_blocks[i])
                                                    });
                                                    if let Some(block) = block {
                                                        block_toggle_buttons(ui, block, [expand_slot, toggle_slot], &mut *ctx.block_toggle_request);
                                                    }
                                                    let [left_target, right_target] =
                                                        shown.map(|side| [RevertTarget::Left, RevertTarget::Right][side]);
                                                    revert_button(ui, left_slot, left_target, "<");
                                                    if let Some((symbol, color)) =
                                                        row_symbol(diff_row, symbol_colors).filter(|_| marks.any())
                                                    {
                                                        ui.put(
                                                            symbol_slot,
                                                            egui::Label::new(egui::RichText::new(symbol).color(color))
                                                                .selectable(false),
                                                        );
                                                    }
                                                    revert_button(ui, right_slot, right_target, ">");
                                                }
                                                }
                                            }
                                        };
                            // The table fills what it is given, so header-only gets just the header.
                            // One child id in both cases keeps the path bars' state across a switch.
                            let rect = if header_only {
                                egui::Rect::from_min_size(
                                    area.min,
                                    egui::vec2(area.width(), *header_height),
                                )
                            } else {
                                area
                            };
                            let mut table_ui = ui.new_child(
                                UiBuilder::new()
                                    .max_rect(rect)
                                    .id_salt("file_diff_table_ui"),
                            );
                            let ruler_rows = rows;
                            let output =
                                table.show(&mut table_ui, *header_height, header, rows, cell);
                            ui.advance_cursor_after_rect(rect);
                            let ruler_spans = match (show_ruler, &ctx.hex_view, ctx.diff_ctx) {
                                (false, _, _) => None,
                                (true, Some(hex_view), _) => Some(hex_view.change_spans),
                                (true, None, Some(_)) if unified.is_some() => {
                                    unified.as_ref().map(|view| {
                                        view.ruler
                                            .get_or_init(|| unified_change_spans(&view.meta))
                                            .as_slice()
                                    })
                                }
                                (true, None, Some(diff_ctx)) => {
                                    Some(ruler_spans(ruler_cache, diff_ctx))
                                }
                                (true, None, None) => None,
                            };
                            if show_ruler && let Some(spans) = ruler_spans {
                                let body = output.body_rect;
                                let strip = egui::Rect::from_min_max(
                                    egui::pos2(area.max.x, body.top()),
                                    egui::pos2(full_area.max.x, body.bottom()),
                                );
                                // The hex strip bins bytes, the table scrolls by 16-byte rows.
                                let (len, row_unit) = if ctx.hex_view.is_some() {
                                    (hex_len, hex::HEX_ROW_BYTES)
                                } else if let Some(view) = &unified {
                                    (view.meta.len(), 1)
                                } else {
                                    (diff_rows_len, 1)
                                };
                                let viewport = (
                                    row_at(&ruler_rows, ruler_tops, output.scroll_offset)
                                        * row_unit as f32,
                                    row_at(
                                        &ruler_rows,
                                        ruler_tops,
                                        output.scroll_offset + body.height(),
                                    ) * row_unit as f32,
                                );
                                let colors = RulerColors {
                                    inserted: role_color(ui.ctx(), FileRole::RulerInserted),
                                    deleted: role_color(ui.ctx(), FileRole::RulerDeleted),
                                    changed: role_color(ui.ctx(), FileRole::RulerChanged),
                                };
                                if let Some(pos) = overview::show(
                                    ui,
                                    overview::OverviewCtx {
                                        id: ruler_id,
                                        rect: strip,
                                        spans,
                                        bin_cache: ruler_bins,
                                        len,
                                        viewport,
                                        colors: &colors,
                                    },
                                ) {
                                    ui.data_mut(|d| {
                                        d.insert_temp(ruler_id.with("scroll_to"), pos / row_unit)
                                    });
                                    ui.ctx().request_repaint();
                                }
                                ui.advance_cursor_after_rect(strip);
                            }
                            *ctx.scroll_left = offsets[0];
                            if unified.is_none() {
                                *ctx.scroll_right = offsets[1];
                            }
                            let measured = header_rects[0].height().max(header_rects[1].height());
                            if measured != *header_height {
                                *header_height = measured;
                                ui.ctx().request_repaint();
                            }
                            if ctx.image_view.is_some() || ctx.video_frames.is_some() {
                                image_left_width = Some(output.side_rects[0].width());
                            } else if !header_only {
                                side_rects = shown.map(|column| output.side_rects[column]);
                            }
                            if let (Some(image_view), Some(image_cfg), Some(left_width)) = (
                                ctx.image_view.as_deref_mut(),
                                ctx.image_settings.as_deref_mut(),
                                image_left_width,
                            ) {
                                image::show(ui, image_view, image_cfg, left_width, shown);
                            }
                            if let Some(view) = patch_rows.filter(|_| !do_not_render_diff) {
                                view.ui(ui);
                            }
                            if let (Some(meta), Some(frames), Some(image_cfg), Some(left_width)) = (
                                ctx.video_meta.as_deref_mut(),
                                ctx.video_frames.as_deref_mut(),
                                ctx.image_settings.as_deref_mut(),
                                image_left_width,
                            ) {
                                let audio = ctx
                                    .audio_view
                                    .as_mut()
                                    .map(|(processor, settings)| (&mut **processor, &mut **settings));
                                video_meta::show(ui, meta, frames, image_cfg, left_width, shown, audio);
                            } else if let Some((processor, settings)) = ctx.audio_view.as_mut() {
                                audio::show(ui, processor, settings);
                            } else if let Some(kind) = ctx.media_view {
                                media_placeholder(
                                    ui,
                                    kind,
                                    &ctx.file_source_path,
                                    &ctx.file_target_path,
                                );
                            }
                            if let Some(processor) = ctx.audio_meta.as_deref_mut() {
                                audio_meta::show(ui, processor);
                            }
                            if let Some(sides) = side_viewers.as_mut() {
                                let text = sides.text;
                                let (find_matchers, find_hit) = (ctx.find_matchers, ctx.find_hit);
                                let text_side = sides
                                    .kinds
                                    .iter()
                                    .position(|kind| *kind == ViewerKind::Text)
                                    .unwrap_or(0);
                                let (highlights, code_language) =
                                    (ctx.active_highlights, ctx.code_language[text_side]);
                                let side_columns = shown.map(|column| output.side_rects[column]);
                                side_viewers::show(ui, sides, side_columns, |ui, row| {
                                    let Some(text) = text else {
                                        return;
                                    };
                                    let content =
                                        side_viewers::plain_content(&text.diff_rows[row].left);
                                    let lit = highlights.binary_search(&row).is_ok();
                                    let find = row_find(
                                        find_matchers,
                                        find_hit,
                                        ActiveSide::Left,
                                        row,
                                        &content,
                                    );
                                    let width = ui.available_width();
                                    Self::render_side_row(
                                        ui,
                                        RowCtx {
                                            side: ActiveSide::Left,
                                            row_index: row,
                                            occurrences,
                                            file_source: text.input.file_1.clone(),
                                            file_target: None,
                                            content: &content,
                                            width,
                                            wrap: false,
                                            is_highlighted: lit,
                                            accent_line_num: lit,
                                            find,
                                            selectable: true,
                                            code_language,
                                            trace: None,
                                        },
                                    );
                                });
                            }
                        });
                });
            }

            if waiting_for_diff {
                loading_state(ui, "Comparing files...");
            } else if do_not_render_diff {
                let load_errors = [
                    ctx.file_source_load_error.as_deref(),
                    ctx.file_target_load_error.as_deref(),
                ];
                let error = ctx
                    .launch_error
                    .map(str::to_owned)
                    .or_else(|| load_error_text(load_errors));
                let loading = ctx.file_source_loading || ctx.file_target_loading;
                let any_path = !(ctx.file_source_path.is_empty() && ctx.file_target_path.is_empty());
                match no_diff_state(error, loading, any_path) {
                    NoDiffState::Error(error) => {
                        error_state(ui, &error);
                    }
                    NoDiffState::Loading(text) => {
                        loading_state(ui, text);
                    }
                    NoDiffState::Welcome => {
                        let (_, clicked) = empty_state(
                            ui,
                            "Welcome to ZDiff",
                            "Drag and drop files here or use the buttons below to start comparing.",
                            &["Open Left", "Open Right"],
                        );
                        *ctx.open_file_request = match clicked {
                            Some(0) => Some(ActiveSide::Left),
                            Some(_) => Some(ActiveSide::Right),
                            None => None,
                        };
                    }
                }
            }
        })
        .response
        .rect;

        for (i, side) in [ActiveSide::Left, ActiveSide::Right]
            .into_iter()
            .enumerate()
        {
            if back[i] {
                *ctx.back_to_previous_request = Some(side);
            }
            if let Some(folder) = browse[i].take() {
                // Opened anew even when one is open: the side may have switched local <-> depot.
                let source: Arc<dyn FolderSource> =
                    if UniversalPath::from(folder.as_str()).is_depot() {
                        Arc::new(DepotFolder)
                    } else {
                        Arc::new(LocalFolder)
                    };
                self.browsers[i] = Some(FolderBrowser::open(&egui_ctx, folder, source));
            }
            let Some(browser) = self.browsers[i]
                .as_mut()
                .filter(|_| header_rects[i].is_positive())
            else {
                continue;
            };
            let rect = egui::Rect::from_x_y_ranges(
                header_rects[i].x_range(),
                header_rects[i].bottom()..=content_rect.bottom(),
            );
            // An area of its own, above the rows, so they and the table's scroll get no input.
            let action = egui::Area::new(ui.id().with(("folder_browser", i)))
                .fixed_pos(rect.min)
                .constrain(false)
                .show(&egui_ctx, |ui| {
                    egui::Frame::new()
                        .fill(ui.visuals().panel_fill)
                        .stroke(ui.visuals().window_stroke())
                        .inner_margin(4.0)
                        .show(ui, |ui| {
                            let size = (rect.size() - Vec2::splat(8.0)).max(Vec2::ZERO);
                            ui.set_min_size(size);
                            ui.set_max_size(size);
                            browser.ui(ui)
                        })
                        .inner
                })
                .inner;
            match action {
                Some(BrowserAction::Close) => self.browsers[i] = None,
                Some(BrowserAction::Pick(picked)) => {
                    self.browsers[i] = None;
                    let paths = [&ctx.file_source_path, &ctx.file_target_path];
                    let roots = [ctx.file_source_root.as_ref(), ctx.file_target_root.as_ref()];
                    let [left, right] = browser_pick(side, &picked, paths, roots);
                    if left.is_some() {
                        *ctx.load_file_1_request = left;
                    }
                    if right.is_some() {
                        *ctx.load_file_2_request = right;
                    }
                }
                None => {}
            }
        }
        // Checked after every path bar ran: one that ends an edit consumes its Esc.
        if self.browsers.iter().any(Option::is_some)
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.browsers = [None, None];
        }

        let active_rect = side_rects[h_scroll::side_index(active_side)];
        if active_rect.is_positive() {
            ui.painter()
                .rect_stroke(active_rect, 0.0, outline_stroke(), egui::StrokeKind::Inside);
        }
        let [left_rect, right_rect] = side_rects;
        self.active_side.end_frame(left_rect, right_rect);
        self.occurrences.end_frame(ui.ctx());
        if !other_viewer {
            ui.ctx()
                .data_mut(|data| data.insert_temp(wheel_rects_id(), side_rects));
        }

        let [file_1_drop_rect, file_2_drop_rect] = drop_rects(side_rects, content_rect, shown);
        handle_drops(
            ui,
            ctx.load_file_1_request,
            ctx.load_file_2_request,
            file_1_drop_rect,
            file_2_drop_rect,
        );

        egui_tiles::UiResponse::None
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use zcommon_ui::table::{Split, SplitColumn, SplitTable};
    use zdiff::diff_builder::DiffBuilderOptions;

    use crate::ui_egui::copy_harness::{CopyHarness, Side};
    /// Top of the table body and the rect of row `target` after jumping to it, as the pane jumps
    /// to a conflict, find result or line.
    fn jump_to_row(
        wrapped_heights: Option<&[f32]>,
        num_rows: usize,
        target: usize,
    ) -> (f32, eframe::egui::Rect) {
        use eframe::egui;
        let ctx = egui::Context::default();
        let mut body_top = f32::NAN;
        let mut target_rect = egui::Rect::NOTHING;
        // The first frame lays out and asks for the scroll; the next ones apply it.
        for frame in 0..3 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 400.0),
                )),
                time: Some(frame as f64),
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    body_top = ui.cursor().top();
                    SplitTable::new("jump", &mut Split::default())
                        .scroll_to_row(target, Some(egui::Align::Min))
                        .show(
                            ui,
                            0.0,
                            |_, _| {},
                            super::text_rows(wrapped_heights, 15.0, num_rows),
                            |index, column, ui| {
                                if index == target && column == SplitColumn::Left {
                                    target_rect = ui.max_rect();
                                }
                            },
                        );
                });
            });
        }
        (body_top, target_rect)
    }

    #[test]
    fn jumping_to_a_row_lands_on_it_with_wrap_on() {
        // Every third row wraps onto three lines.
        let heights: Vec<f32> = (0..200)
            .map(|i| if i % 3 == 0 { 45.0 } else { 15.0 })
            .collect();

        for target in [1, 30, 100, 151] {
            let (top, rect) = jump_to_row(Some(&heights), heights.len(), target);
            assert!(
                (rect.top() - top).abs() < 1.0,
                "row {target}: {rect:?} vs top {top}"
            );
            assert_eq!(rect.height(), heights[target], "row {target}");
        }
        // Wrap off: one line per row.
        let (top, rect) = jump_to_row(None, 200, 100);
        assert!((rect.top() - top).abs() < 1.0, "{rect:?} vs top {top}");
        assert_eq!(rect.height(), 15.0);
    }

    // Identical on both sides except the last line, so the shared rows carry no ghosts.
    const OCCURRENCE_SOURCE: &str =
        "let value = 1;\nlet other = value + value;\nfoo(value);\nold\n";
    const OCCURRENCE_TARGET: &str =
        "let value = 1;\nlet other = value + value;\nfoo(value);\nnew\n";

    fn occurrence_harness() -> CopyHarness {
        CopyHarness::new(
            OCCURRENCE_SOURCE,
            OCCURRENCE_TARGET,
            &DiffBuilderOptions::default(),
        )
    }

    #[test]
    fn a_single_row_drag_searches_for_exactly_the_text_egui_copies() {
        let mut harness = occurrence_harness();
        for (side, from, to) in [
            (Side::Left, (0, 4), (0, 9)),
            (Side::Left, (1, 14), (1, 6)),
            (Side::Right, (2, 0), (2, 5)),
        ] {
            harness.drag(side, from, to);
            let copied = harness.copy();
            assert!(copied.is_some(), "{side:?} {from:?}..{to:?}");
            assert_eq!(
                harness.occurrence_needle(),
                copied,
                "{side:?} {from:?}..{to:?}"
            );
        }
    }

    #[test]
    fn a_double_clicked_word_is_searched_for() {
        let mut harness = occurrence_harness();

        harness.double_click(Side::Left, (1, 14));

        assert_eq!(harness.occurrence_needle().as_deref(), Some("value"));
        assert_eq!(harness.copy().as_deref(), Some("value"));
    }

    #[test]
    fn every_other_occurrence_is_highlighted_on_both_sides() {
        let mut harness = occurrence_harness();

        // The second "value" in row 1.
        harness.drag(Side::Left, (1, 20), (1, 25));
        assert_eq!(harness.occurrence_needle().as_deref(), Some("value"));

        assert_eq!(
            harness.occurrence_ranges(Side::Left, 0),
            [Range { start: 4, end: 9 }]
        );
        assert_eq!(
            harness.occurrence_ranges(Side::Left, 1),
            [Range { start: 12, end: 17 }]
        );
        assert_eq!(
            harness.occurrence_ranges(Side::Left, 2),
            [Range { start: 4, end: 9 }]
        );
        assert!(harness.occurrence_ranges(Side::Left, 3).is_empty());
        // The other side's identical row has no selection, so both of its matches show.
        assert_eq!(harness.occurrence_ranges(Side::Right, 1), [12..17, 20..25]);
        assert_eq!(
            harness.occurrence_ranges(Side::Right, 2),
            [Range { start: 4, end: 9 }]
        );
    }

    #[test]
    fn a_selection_across_rows_searches_for_nothing() {
        let mut harness = occurrence_harness();

        // Through the row first, so a selection in it exists before the drag leaves it.
        harness.drag_through(Side::Left, &[(0, 4), (0, 9), (1, 9)]);

        assert_eq!(harness.copy().as_deref(), Some("value = 1;\nlet other"));
        assert_eq!(harness.occurrence_needle(), None);
        assert!(harness.occurrence_ranges(Side::Left, 2).is_empty());
    }

    #[test]
    fn short_and_whitespace_only_selections_search_for_nothing() {
        let mut harness = CopyHarness::new(
            "let value = 1;\n    value;\n",
            "let value = 1;\n    value;\n",
            &DiffBuilderOptions::default(),
        );

        harness.drag(Side::Left, (0, 4), (0, 5));
        assert_eq!(harness.copy().as_deref(), Some("v"));
        assert_eq!(harness.occurrence_needle(), None);

        harness.drag(Side::Left, (1, 0), (1, 4));
        assert_eq!(harness.copy().as_deref(), Some("    "));
        assert_eq!(harness.occurrence_needle(), None);

        harness.drag(Side::Left, (1, 3), (1, 6));
        assert_eq!(harness.occurrence_needle().as_deref(), Some(" va"));
    }

    #[test]
    fn the_highlight_clears_with_the_selection() {
        let mut harness = occurrence_harness();

        harness.drag(Side::Left, (0, 4), (0, 9));
        assert_eq!(harness.occurrence_needle().as_deref(), Some("value"));
        harness.press_escape();
        assert_eq!(harness.occurrence_needle(), None);
        assert!(harness.occurrence_ranges(Side::Left, 1).is_empty());

        harness.drag(Side::Left, (0, 4), (0, 9));
        assert_eq!(harness.occurrence_needle().as_deref(), Some("value"));
        harness.click_outside_the_rows();
        assert_eq!(harness.occurrence_needle(), None);

        // A plain click in a row selects nothing.
        harness.drag(Side::Left, (0, 4), (0, 9));
        harness.drag(Side::Left, (1, 4), (1, 4));
        assert_eq!(harness.occurrence_needle(), None);
    }

    #[test]
    fn only_the_visible_rows_are_searched() {
        let source: String = (0..2000)
            .map(|i| format!("let value_{i} = {i};\n"))
            .collect();
        let harness = CopyHarness::new(&source, &source, &DiffBuilderOptions::default());

        let height = 400.0;
        let (searched, row_h) = harness.rows_searched_in_table("value", height);

        assert!(searched > 0);
        assert!(
            searched as f32 <= height / row_h + 2.0,
            "{searched} rows searched of {} at {row_h} per row",
            harness.rows().len()
        );
    }
}
