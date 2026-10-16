use super::*;

pub(in crate::ui_egui) struct RowCtx<'a, T: RawTokenTrait> {
    pub(in crate::ui_egui) side: ActiveSide,
    pub(in crate::ui_egui) row_index: usize,
    pub(in crate::ui_egui) occurrences: &'a mut OccurrenceState,
    pub(in crate::ui_egui) file_source: Option<Arc<CachedFile<T>>>,
    pub(in crate::ui_egui) file_target: Option<Arc<CachedFile<T>>>,
    pub(in crate::ui_egui) content: &'a LineContent,
    pub(in crate::ui_egui) width: f32,
    pub(in crate::ui_egui) wrap: bool,
    pub(in crate::ui_egui) is_highlighted: bool,
    pub(in crate::ui_egui) accent_line_num: bool,
    pub(in crate::ui_egui) find: RowFind<'a>,
    pub(in crate::ui_egui) selectable: bool,
    pub(in crate::ui_egui) code_language: &'a str,
    pub(in crate::ui_egui) trace: Option<&'a PhaseTrace>,
}

impl FileDiffPane {
    pub(in crate::ui_egui) fn render_side_row<T: RawTokenTrait>(
        ui: &mut egui::Ui,
        row: RowCtx<T>,
    ) {
        Self::render_row(
            ui,
            row,
            Gutter::Single,
            Marks {
                scope: false,
                line: true,
                tokens: false,
                chars: true,
            },
        );
    }

    /// `marks.scope`: this row's line is in a changed scope and scopes are marked.
    pub(super) fn render_row<T: RawTokenTrait>(
        ui: &mut egui::Ui,
        row: RowCtx<T>,
        gutter: Gutter,
        marks: Marks,
    ) {
        let RowCtx {
            side,
            row_index,
            occurrences,
            file_source,
            file_target,
            content,
            width,
            wrap,
            is_highlighted,
            accent_line_num,
            find,
            selectable,
            code_language,
            trace,
        } = row;
        let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
        // Wrapped rows fill their cell, which is as tall as the row's taller side.
        let cell_h = if wrap {
            ui.max_rect().height().max(row_h)
        } else {
            row_h
        };

        let (rect, _) = ui.allocate_at_least(egui::vec2(width, cell_h), egui::Sense::hover());
        let mut extended_rect = rect.clone();
        extended_rect.extend_with_x(9999999.0);

        match content {
            LineContent::Code {
                tokens,
                line_num,
                bg,
                changed_chars,
            } => {
                let code_colors = syntax_colors(ui.ctx());
                let Rgba8([r, g, b, _]) = code_colors.get(SyntaxCategory::Background);
                ui.painter()
                    .rect_filled(extended_rect, 0.0, egui::Color32::from_rgb(r, g, b));
                ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                    let add_contents = |ui: &mut egui::Ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;

                        let line_number = |ui: &mut egui::Ui, text: String, width: f32| {
                            let text = egui::RichText::new(text).size(10.0);
                            let text = if accent_line_num {
                                text.color(role_color(ui.ctx(), FileRole::GotoLineNumber))
                                    .strong()
                            } else {
                                text.color(role_color(ui.ctx(), FileRole::LineNumber))
                            };
                            ui.add_sized([width, row_h], egui::Label::new(text).selectable(false));
                        };
                        match gutter {
                            Gutter::Single => {
                                let line = (*line_num > 0).then(|| line_num.to_string());
                                line_number(ui, line.unwrap_or_default(), GUTTER_WIDTH);
                            }
                            Gutter::Offset(offset) => {
                                let shown = offset.map(|o| format!("{o:08X}")).unwrap_or_default();
                                line_number(ui, shown, OFFSET_GUTTER_WIDTH);
                            }
                            Gutter::Unified { left, right, sign } => {
                                let shown = |line: Option<usize>| {
                                    line.map(|n| n.to_string()).unwrap_or_default()
                                };
                                line_number(ui, shown(left), GUTTER_WIDTH);
                                line_number(ui, shown(right), GUTTER_WIDTH);
                                let (sign, color) =
                                    sign.unwrap_or((' ', egui::Color32::PLACEHOLDER));
                                ui.add_sized(
                                    [UNIFIED_SIGN_WIDTH, row_h],
                                    egui::Label::new(egui::RichText::new(sign).color(color))
                                        .selectable(false),
                                );
                            }
                        }

                        ui.add_space(GUTTER_GAP);

                        let row_text = build_row_text(
                            tokens,
                            changed_chars,
                            file_source.as_deref(),
                            file_target.as_deref(),
                        );

                        let mut layout_job = syntax::highlight(
                            ui.ctx(),
                            ui.style(),
                            &code_colors,
                            &row_text.text,
                            code_language,
                        );
                        let style_ctx = ui.ctx().clone();
                        let color = |role| role_color(&style_ctx, role);
                        // Text matched by an ignore pattern is dimmed over the syntax colors.
                        recolor_ranges(
                            &mut layout_job,
                            &row_text.dimmed,
                            color(FileRole::DimmedText),
                        );
                        let (tokens_bg, chars_bg) = match side {
                            ActiveSide::Left => (
                                color(FileRole::DeleteChangedTokens),
                                color(FileRole::DeleteChangedChars),
                            ),
                            ActiveSide::Right => (
                                color(FileRole::InsertChangedTokens),
                                color(FileRole::InsertChangedChars),
                            ),
                        };
                        if marks.tokens {
                            highlight_ranges(&mut layout_job, &row_text.changed_tokens, tokens_bg);
                        }
                        if marks.chars {
                            highlight_ranges(&mut layout_job, &row_text.changed_chars, chars_bg);
                        }
                        let found = occurrences.ranges(side, row_index, &row_text.text);
                        highlight_ranges(&mut layout_job, &found, color(FileRole::Occurrence));
                        // After the occurrences, so a find match shows over one.
                        let (find_all, find_current) = find_ranges(&row_text.text, find);
                        highlight_ranges(&mut layout_job, &find_all, color(FileRole::Find));
                        if let Some(current) = find_current {
                            highlight_ranges(
                                &mut layout_job,
                                &[current],
                                color(FileRole::FindCurrent),
                            );
                        }

                        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
                        let wrap_at = if wrap {
                            gutter.text_width(width)
                        } else {
                            f32::INFINITY
                        };
                        let ghost_color = color(FileRole::GhostText);
                        let side_text = ui.fonts_mut(|fonts| {
                            layout_side_text(
                                fonts,
                                layout_job,
                                &row_text.ghosts,
                                ghost_color,
                                &font_id,
                                wrap_at,
                            )
                        });
                        let galley = side_text.galley.clone();
                        // The hit area spans the rest of the row, not just the text, so a press
                        // anywhere in the block starts a selection. Drag sense on both sides:
                        // egui hit-tests against the previous frame, so the side that becomes
                        // selectable on a press needs it already, or that press is lost.
                        let hit_size = egui::vec2(
                            galley.size().x.max(ui.available_width()),
                            side_text.height().max(cell_h),
                        );
                        let (hit_rect, response) =
                            ui.allocate_exact_size(hit_size, egui::Sense::click_and_drag());
                        let text_color = ui.style().visuals.text_color();
                        if selectable {
                            let marker = if tokens.iter().all(|(_, _, is_ghost)| *is_ghost) {
                                Some(COPY_MARKER_NO_LINE)
                            } else if row_text.text.is_empty() {
                                Some(COPY_MARKER_BLANK_LINE)
                            } else {
                                None
                            };
                            if marker.is_none() {
                                occurrences.track(
                                    ui,
                                    &response,
                                    hit_rect.left_top(),
                                    &galley,
                                    side,
                                    row_index,
                                );
                            }
                            let galley = match marker {
                                Some(marker) => copy_marker_galley(ui, marker),
                                None => galley,
                            };
                            let galley = if wrap {
                                reach_down_to(galley, hit_rect.height())
                            } else {
                                galley
                            };
                            LabelSelectionState::label_text_selection(
                                ui,
                                &response,
                                hit_rect.left_top(),
                                galley,
                                text_color,
                                egui::Stroke::NONE,
                            );
                        } else {
                            ui.painter().add(egui::epaint::TextShape::new(
                                hit_rect.left_top(),
                                galley,
                                text_color,
                            ));
                        }
                        for (pos, ghost_galley) in side_text.ghosts {
                            ui.painter().add(egui::epaint::TextShape::new(
                                hit_rect.left_top() + pos,
                                ghost_galley,
                                text_color,
                            ));
                        }

                        if marks.scope {
                            let scope_bg = match side {
                                ActiveSide::Left => color(FileRole::DeleteChangedScope),
                                ActiveSide::Right => color(FileRole::InsertChangedScope),
                            };
                            ui.painter().rect_filled(extended_rect, 0.0, scope_bg);
                        }
                        let bg = match bg {
                            RowBg::None => egui::Color32::TRANSPARENT,
                            RowBg::Delete | RowBg::Insert if !marks.line => {
                                egui::Color32::TRANSPARENT
                            }
                            RowBg::Delete => role_color(ui.ctx(), SharedRole::DeleteBackground),
                            RowBg::Insert => role_color(ui.ctx(), SharedRole::InsertBackground),
                            RowBg::Custom(zdiff::diff_builder::Color32([r, g, b, a])) => {
                                egui::Color32::from_rgba_unmultiplied(*r, *g, *b, *a)
                            }
                        };
                        ui.painter().rect_filled(extended_rect, 0.0, bg);
                        if is_highlighted {
                            ui.painter()
                                .rect_filled(extended_rect, 0.0, color(FileRole::GotoRow));
                        }
                        if let Some(trace) = trace {
                            let outlines = token_hunk_outlines(
                                trace,
                                tokens,
                                &row_text,
                                file_source.as_deref(),
                                file_target.as_deref(),
                            );
                            paint_trace_row(
                                ui.painter(),
                                &outlines,
                                &row_text,
                                &side_text.galley,
                                hit_rect.left_top(),
                            );
                        }
                    };
                    // A wrapped row's line number stays on its first line.
                    if wrap {
                        ui.horizontal_top(add_contents);
                    } else {
                        ui.horizontal_centered(add_contents);
                    }
                });
            }
            LineContent::Void => {
                let highlight = is_highlighted.then(|| role_color(ui.ctx(), FileRole::GotoRow));
                paint_row_look(
                    ui,
                    extended_rect,
                    void_look(ui.ctx()),
                    role_color(ui.ctx(), FileRole::VoidRow),
                    highlight,
                );

                if selectable {
                    // Full-row hit area like Code rows, so a press here starts a selection too.
                    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                        let (hit_rect, response) =
                            ui.allocate_exact_size(rect.size(), egui::Sense::click_and_drag());
                        let marker = copy_marker_galley(ui, COPY_MARKER_NO_LINE);
                        let marker = if wrap {
                            reach_down_to(marker, hit_rect.height())
                        } else {
                            marker
                        };
                        LabelSelectionState::label_text_selection(
                            ui,
                            &response,
                            hit_rect.left_top(),
                            marker,
                            egui::Color32::TRANSPARENT,
                            egui::Stroke::NONE,
                        );
                    });
                }
            }
            LineContent::Collapsed => {
                // Clear blue highlight
                let highlight =
                    is_highlighted.then(|| egui::Color32::from_rgba_unmultiplied(38, 79, 120, 100));
                paint_row_look(
                    ui,
                    extended_rect,
                    collapsed_look(ui.ctx()),
                    role_color(ui.ctx(), FileRole::CollapsedRow),
                    highlight,
                );

                ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;

                        ui.add_sized(
                            [gutter.width(), row_h],
                            egui::Label::new(
                            egui::RichText::new("|") // Mid-line ellipsis fits a gutter better than "-"
                            .color(egui::Color32::from_gray(100))
                            .size(12.0))
                            .selectable(false)
                        );

                        ui.add_space(GUTTER_GAP);

                        ui.add(
                            egui::Label::new(
                                egui::RichText::new("Collapsed context")
                                    .color(egui::Color32::from_gray(130))
                                    .size(11.0),
                            )
                            .selectable(false),
                        );

                        if selectable {
                            let galley = copy_marker_galley(ui, COPY_MARKER_NO_LINE);
                            let (hit_rect, response) = ui
                                .allocate_exact_size(galley.size(), egui::Sense::click_and_drag());
                            LabelSelectionState::label_text_selection(
                                ui,
                                &response,
                                hit_rect.left_top(),
                                galley,
                                egui::Color32::TRANSPARENT,
                                egui::Stroke::NONE,
                            );
                        }
                    });
                });
            }
        }
    }
}

pub(super) const STRIPE_SPACING: f32 = 8.0;
pub(super) const STRIPE_WIDTH: f32 = 1.5;

// The scroll area's content top moves with the rows; the row's left moves with the h-scroll.
pub(super) fn stripe_origin(ui: &egui::Ui, row_rect: egui::Rect) -> egui::Pos2 {
    let top = ui
        .stack()
        .iter()
        .find(|s| s.kind() == Some(egui::UiKind::ScrollArea))
        .map_or(0.0, |s| s.max_rect.top());
    egui::pos2(row_rect.left(), top)
}

pub(super) fn row_look_shapes(
    look: RowLook,
    rect: egui::Rect,
    origin: egui::Pos2,
    color: egui::Color32,
    highlight: Option<egui::Color32>,
) -> Vec<egui::Shape> {
    let fill = match look {
        RowLook::Solid => Some(highlight.unwrap_or(color)),
        RowLook::Stripes | RowLook::Empty => highlight,
    };
    let mut shapes: Vec<egui::Shape> = fill
        .map(|fill| egui::Shape::rect_filled(rect, 0.0, fill))
        .into_iter()
        .collect();
    if look == RowLook::Stripes {
        let stroke = egui::Stroke::new(STRIPE_WIDTH, color);
        shapes.extend(
            stripe_segments(
                [rect.min.x, rect.min.y],
                [rect.max.x, rect.max.y],
                [origin.x, origin.y],
                STRIPE_SPACING,
            )
            .map(|[a, b]| {
                egui::Shape::line_segment([egui::pos2(a[0], a[1]), egui::pos2(b[0], b[1])], stroke)
            }),
        );
    }
    shapes
}

// Clipped first: the row rect reaches far right, and a wide stroke would bleed into neighbours.
pub(super) fn paint_row_look(
    ui: &egui::Ui,
    row_rect: egui::Rect,
    look: RowLook,
    color: egui::Color32,
    highlight: Option<egui::Color32>,
) {
    let clip = row_rect.intersect(ui.clip_rect());
    if !clip.is_positive() {
        return;
    }
    let origin = stripe_origin(ui, row_rect);
    ui.painter()
        .with_clip_rect(clip)
        .extend(row_look_shapes(look, clip, origin, color, highlight));
}

/// A code row's text as drawn. `galley` holds only the real text, so only that can be selected
/// and copied; inline ghosts sit in gaps left in it. Ghost positions are relative to the galley.
pub(super) struct SideText {
    pub(super) galley: Arc<egui::Galley>,
    pub(super) ghosts: Vec<(Vec2, Arc<egui::Galley>)>,
}

impl SideText {
    pub(super) fn height(&self) -> f32 {
        self.ghosts
            .iter()
            .fold(self.galley.size().y, |height, (pos, ghost)| {
                height.max(pos.y + ghost.size().y)
            })
    }
}

/// Lays out a code row's text `job` and its ghosts, wrapped at `wrap_width` (infinite for one
/// line). Drawing and the wrapped row heights both go through here, so a row is exactly as tall
/// as what is drawn in it. Syntax colors don't move monospace glyphs, so `job` may be plain.
pub(super) fn layout_side_text(
    fonts: &mut egui::epaint::FontsView<'_>,
    mut job: egui::text::LayoutJob,
    ghosts: &[GhostInsertion],
    ghost_color: egui::Color32,
    font_id: &egui::FontId,
    wrap_width: f32,
) -> SideText {
    let text_len = job.text.len();
    let (inline, trailing): (Vec<_>, Vec<_>) = ghosts
        .iter()
        .partition(|ghost| ghost.byte_offset < text_len);

    let inline_galleys: Vec<Arc<egui::Galley>> = inline
        .iter()
        .map(|ghost| fonts.layout_no_wrap(ghost.text.clone(), font_id.clone(), ghost_color))
        .collect();
    let gaps: Vec<(usize, f32)> = inline
        .iter()
        .zip(&inline_galleys)
        .map(|(ghost, galley)| (ghost.byte_offset, galley.size().x))
        .collect();
    insert_ghost_gaps(&mut job, &gaps);
    let text = job.text.clone();
    job.wrap.max_width = wrap_width;
    let galley = fonts.layout_job(job);

    let mut placed: Vec<_> = ghost_positions(&galley, &text, &gaps)
        .into_iter()
        .zip(inline_galleys)
        .collect();
    if !trailing.is_empty() {
        let last_row = galley.rows.last().expect("a galley has at least one row");
        // One job continuing the last row, wrapping like the text. The minimum height keeps an
        // empty first row (nothing fits after the text) from collapsing onto the text.
        let mut tail = egui::text::LayoutJob::default();
        for (i, ghost) in trailing.iter().enumerate() {
            let leading_space = if i == 0 { last_row.size.x } else { 0.0 };
            tail.append(
                &ghost.text,
                leading_space,
                egui::TextFormat::simple(font_id.clone(), ghost_color),
            );
        }
        tail.first_row_min_height = last_row.height();
        tail.wrap.max_width = wrap_width;
        placed.push((egui::vec2(0.0, last_row.pos.y), fonts.layout_job(tail)));
    }
    SideText {
        galley,
        ghosts: placed,
    }
}

/// Lays out the text rows: one line each, or each its wrapped height. A `scroll_to_row` on the
/// table goes by the same heights.
pub(in crate::ui_egui) fn text_rows(
    wrapped_heights: Option<&[f32]>,
    row_height: f32,
    num_rows: usize,
) -> Rows<'_> {
    match wrapped_heights {
        Some(heights) => Rows::Heights(heights),
        None => Rows::Uniform {
            height: row_height,
            count: num_rows,
        },
    }
}


#[cfg(test)]
mod tests {
    mod row_looks {
        use eframe::egui::{self, Color32, Shape};
        use zcommon::style::{RowLook, stripe_segments};
        use zcommon_ui::table::{Rows, Split, SplitColumn, SplitTable};

        use super::super::{STRIPE_SPACING, row_look_shapes, stripe_origin};

        const ROW: usize = 40;
        const ROW_HEIGHT: f32 = 15.0;

        fn row_top_and_origin(scroll_to: usize) -> (f32, egui::Pos2) {
            let ctx = egui::Context::default();
            let mut found = None;
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
                        SplitTable::new("stripes", &mut Split::default())
                            .scroll_to_row(scroll_to, Some(egui::Align::Min))
                            .show(
                                ui,
                                0.0,
                                |_, _| {},
                                Rows::Uniform {
                                    height: ROW_HEIGHT,
                                    count: 200,
                                },
                                |index, column, ui| {
                                    if index == ROW && column == SplitColumn::Left {
                                        let rect = ui.max_rect();
                                        found = Some((rect.top(), stripe_origin(ui, rect)));
                                    }
                                },
                            );
                    });
                });
            }
            found.expect("row not shown")
        }

        #[test]
        fn the_stripe_origin_moves_with_the_scrolled_rows() {
            let (top_a, origin_a) = row_top_and_origin(30);
            let (top_b, origin_b) = row_top_and_origin(33);
            assert_ne!(top_a, top_b);
            let content_y = ROW as f32 * ROW_HEIGHT;
            assert!(
                (top_a - origin_a.y - content_y).abs() < 1.0,
                "{top_a} {origin_a:?}"
            );
            assert!(
                (top_b - origin_b.y - content_y).abs() < 1.0,
                "{top_b} {origin_b:?}"
            );
        }

        const COLOR: Color32 = Color32::from_rgb(10, 20, 30);
        const HIGHLIGHT: Color32 = Color32::from_rgb(200, 200, 0);

        fn rect() -> egui::Rect {
            egui::Rect::from_min_max(egui::pos2(100.0, 40.0), egui::pos2(160.0, 55.0))
        }

        fn fills(shapes: &[Shape]) -> Vec<(egui::Rect, Color32)> {
            shapes
                .iter()
                .filter_map(|s| match s {
                    Shape::Rect(r) => Some((r.rect, r.fill)),
                    _ => None,
                })
                .collect()
        }

        fn lines(shapes: &[Shape]) -> Vec<([egui::Pos2; 2], Color32)> {
            shapes
                .iter()
                .filter_map(|s| match s {
                    Shape::LineSegment { points, stroke } => Some((*points, stroke.color)),
                    _ => None,
                })
                .collect()
        }

        #[test]
        fn solid_fills_the_row_with_the_role_or_the_highlight() {
            let origin = egui::pos2(100.0, 3.0);
            let shapes = row_look_shapes(RowLook::Solid, rect(), origin, COLOR, None);
            assert_eq!(fills(&shapes), [(rect(), COLOR)]);
            assert!(lines(&shapes).is_empty());
            let shapes = row_look_shapes(RowLook::Solid, rect(), origin, COLOR, Some(HIGHLIGHT));
            assert_eq!(fills(&shapes), [(rect(), HIGHLIGHT)]);
        }

        #[test]
        fn empty_draws_only_the_highlight() {
            let origin = egui::pos2(100.0, 3.0);
            assert!(row_look_shapes(RowLook::Empty, rect(), origin, COLOR, None).is_empty());
            let shapes = row_look_shapes(RowLook::Empty, rect(), origin, COLOR, Some(HIGHLIGHT));
            assert_eq!(fills(&shapes), [(rect(), HIGHLIGHT)]);
            assert!(lines(&shapes).is_empty());
        }

        #[test]
        fn stripes_are_role_colored_lines_anchored_to_the_origin() {
            let origin = egui::pos2(100.0, 3.0);
            let shapes = row_look_shapes(RowLook::Stripes, rect(), origin, COLOR, None);
            assert!(fills(&shapes).is_empty());
            let expected: Vec<_> = stripe_segments(
                [rect().min.x, rect().min.y],
                [rect().max.x, rect().max.y],
                [origin.x, origin.y],
                STRIPE_SPACING,
            )
            .map(|[a, b]| ([egui::pos2(a[0], a[1]), egui::pos2(b[0], b[1])], COLOR))
            .collect();
            assert!(!expected.is_empty());
            assert_eq!(lines(&shapes), expected);

            let shapes = row_look_shapes(RowLook::Stripes, rect(), origin, COLOR, Some(HIGHLIGHT));
            assert_eq!(fills(&shapes), [(rect(), HIGHLIGHT)]);
            assert!(matches!(shapes[0], Shape::Rect(_)));
            assert_eq!(lines(&shapes), expected);
        }
    }
}
