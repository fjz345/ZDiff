use super::{
    ZApp,
    launch::toggle_all_options,
    menu::MenuCtx,
    nav::{back_to_previous_revision, previous_revision},
    state::AppStateCtx,
};
use eframe::egui::{self, Layout};
use std::sync::atomic::Ordering;
use zcommon_ui::common::show_custom_popup;
use zcommon_ui::find_popup::{FindPopupEvent, find_popup};

use zdiff::universal_path::UniversalPath;

use egui_tiles::Tile;

use crate::{
    p4::P4Command,
    revert::{self, RevertRefusal, RevertWrite},
    ui_egui::{
        active_side::ActiveSide,
        diff_pane::FileDiffPaneCtx,
        merge_pane::{Closing, MergeView},
        panes::{Pane, TreeBehavior},
        side_viewers::SideViewers,
    },
    viewer::{
        ViewerKind, Viewers, hex::parse_hex_offset, hex_strings::HexMode, video_meta::VideoTab,
    },
};

impl ZApp {
    pub(super) fn ui(
        &mut self,
        ctx: &egui::Context,
        _frame: &mut eframe::Frame,
        app_ctx: &mut AppStateCtx,
    ) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let text_diff_shown = self.merge.is_none() && app_ctx.shows_text_diff();
            let AppStateCtx {
                scroll_left,
                scroll_right,
                h_scroll_linked,
                h_scroll_modifier,
                center_jumps,
                word_wrap,
                text_mode,
                text_likeness,
                sides_swapped,
                diff_options,
                file_1,
                file_2,
                goto_open,
                find_open,
                goto_input,
                find_query,
                find_both_panes,
                find_error,
                keybindings,
                myers_diff_algorithm,
                diff_processor,
                hex_processor,
                hex_settings,
                hex_strings,
                strings_active: _,
                image_processor,
                image_settings,
                audio_processor,
                audio_settings,
                audio_meta,
                video_frames,
                video_meta,
                viewers,
                viewer_override,
                viewer_fallback,
                kinds,
                ffmpeg,
                pdb_options,
                legacy_extension_map: _,
                revert_history,
                nav_history,
                toggle_all,
                code_language_override,
                syntax_filter,
                detected_languages,
                p4_profiles,
                regex_ignore_presets,
            } = app_ctx;
            let detected = [
                detected_languages[0].get(file_1.get_cached_file().as_deref()),
                detected_languages[1].get(file_2.get_cached_file().as_deref()),
            ];
            let pair_kind = viewers.and_then(Viewers::pair);
            let sides = match *viewers {
                Some(Viewers::Sides(sides)) => Some(sides),
                _ => None,
            };
            let side_shows = |kind| sides.is_some_and(|s| s.contains(&kind));
            let is_hex = pair_kind == Some(ViewerKind::Hex);
            let pdb_dump_shown = viewers.is_some_and(|v| v.shows(ViewerKind::Text))
                && [&mut *file_1, &mut *file_2]
                    .into_iter()
                    .any(|file| file.get_loaded_file().is_some_and(|f| f.is_dump()));
            let strings_view = is_hex && hex_settings.mode == HexMode::Strings;
            let hex_bytes = is_hex && !strings_view;
            let goto_hex =
                hex_bytes || (side_shows(ViewerKind::Hex) && !side_shows(ViewerKind::Text));
            let is_image = pair_kind == Some(ViewerKind::Image);
            let media_view =
                pair_kind.filter(|k| matches!(k, ViewerKind::Audio | ViewerKind::Video));
            let video_audio =
                media_view == Some(ViewerKind::Video) && video_meta.tab == VideoTab::Audio;
            self.show_menu(
                ui,
                MenuCtx {
                    file_1,
                    file_2,
                    diff_processor,
                    find_open,
                    goto_open,
                    sides_swapped,
                    keybindings,
                    p4_profiles,
                    kinds,
                    ffmpeg,
                    myers_diff_algorithm,
                    diff_phases: &mut diff_options.phases,
                    code_language_override,
                    syntax_filter,
                    detected_languages: detected,
                    h_scroll_modifier,
                    h_scroll_linked,
                    center_jumps,
                    text_diff_shown,
                },
            );
            self.frame_stats.mark("menu");
            let code_language = match code_language_override.as_deref() {
                Some(code) => [code; 2],
                None => detected.map(|language| language.unwrap_or("")),
            };

            ui.separator();

            let mut goto_window_open = *goto_open;
            show_custom_popup(ctx, &mut goto_window_open, "Goto", true, |ui| {
                // Hex gotos a byte offset, in decimal or 0x hex.
                if goto_hex {
                    goto_input.retain(|c| c.is_ascii_hexdigit() || c == 'x' || c == 'X');
                } else {
                    goto_input.retain(|c| c.is_ascii_digit());
                }
                let response = ui.add(
                    egui::TextEdit::singleline(goto_input)
                        .desired_width(40.0)
                        .hint_text(if goto_hex { "offset" } else { "#" }),
                );
                response.request_focus();
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    if goto_hex {
                        match parse_hex_offset(goto_input, hex_processor.max_len()) {
                            Ok(offset) => {
                                hex_processor.goto(offset);
                                *goto_open = false;
                            }
                            Err(e) => log::error!("Goto offset {:?}: {}", goto_input, e),
                        }
                    } else if let Ok(line_number) = goto_input.parse::<usize>() {
                        diff_processor.update_goto(Some(line_number));
                        *goto_open = false;
                    }
                }
            });
            if !goto_window_open {
                goto_input.clear();
                *goto_open = goto_window_open;
            }
            if *find_open {
                let event = find_popup(ctx, find_query, find_error.as_deref(), None, |ui| {
                    ui.checkbox(find_both_panes, "Both panes").changed()
                });
                match event {
                    Some(FindPopupEvent::Changed | FindPopupEvent::Submitted) => {
                        diff_processor.find_both_sides = *find_both_panes;
                        *find_error = diff_processor
                            .update_find_query(find_query)
                            .err()
                            .map(|e| e.to_string());
                        if event == Some(FindPopupEvent::Submitted) && find_error.is_none() {
                            *find_open = false;
                        }
                    }
                    Some(FindPopupEvent::Closed) => *find_open = false,
                    None => {}
                }
            }

            // The text diff's state is stale while another viewer shows the pair.
            let text_scroll = side_shows(ViewerKind::Text)
                .then(|| diff_processor.get_scroll_to_row())
                .flatten();
            let hex_scroll = side_shows(ViewerKind::Hex)
                .then(|| hex_processor.scroll_to_row(0))
                .flatten();
            let scroll_to_rows = &if sides.is_some() {
                None
            } else if hex_bytes {
                hex_processor.scroll_to_row(diff_processor.conflict_cursor.get())
            } else if is_image || media_view.is_some() {
                None
            } else {
                diff_processor.get_scroll_to_row()
            };
            if let Some(scroll_to) = scroll_to_rows {
                log::info!("Navigating to line: {:?}", scroll_to);
            }

            let active_highlights = diff_processor.active_highlights.clone();
            let mut conflict_cursor = diff_processor.conflict_cursor.clone();
            let pins = diff_processor.pins().to_vec();
            let mut pins_request = None;
            let mut find_cursor = diff_processor.find_cursor.clone();
            let highlight_side = diff_processor.highlight_side;
            let find_matchers = [ActiveSide::Left, ActiveSide::Right]
                .map(|side| diff_processor.find_ctx.matcher_on(side).cloned());
            let find_hit = diff_processor.current_find_hit();
            let find_count = diff_processor.find_ctx.hits().len();
            let mut active_side = diff_processor.active_side;

            if file_1.get_full_path() != UniversalPath::default()
                || file_2.get_full_path() != UniversalPath::default()
            {
                self.launch_error = None;
            }
            // Rows of strings from before a changed minimum length would show without offsets.
            let strings_pending = strings_view && hex_strings.sides().is_none();
            let diff_ctx = if hex_bytes || strings_pending || is_image || media_view.is_some() {
                None
            } else {
                diff_processor.get_minimal_diff_ctx()
            };
            let (diff_ctx, side_text) = match sides {
                Some(_) => (None, diff_ctx.filter(|_| side_shows(ViewerKind::Text))),
                None => (diff_ctx, None),
            };
            let string_offsets = match hex_strings.sides().filter(|_| strings_view) {
                Some(sides) => sides
                    .each_ref()
                    .map(|s| s.as_ref().map_or(&[][..], |s| &s.offsets[..])),
                None => [&[][..]; 2],
            };
            let mut block_toggle_request = None;
            let mut toggle_all_request = false;
            let mut behavior = TreeBehavior {
                ctx_file_diff: FileDiffPaneCtx {
                    diff_ctx: diff_ctx.as_ref(),
                    scroll_left,
                    scroll_right,
                    h_scroll_linked,
                    word_wrap,
                    text_mode,
                    text_likeness,
                    diff_options,
                    regex_ignore_presets,
                    scroll_to_row_span: scroll_to_rows,
                    center_jumps: *center_jumps,
                    load_file_1_request: &mut None,
                    load_file_2_request: &mut None,
                    back_to_previous_request: &mut None,
                    set_file_1_root_request: &mut None,
                    set_file_2_root_request: &mut None,
                    open_file_request: &mut None,
                    file_source_path: file_1.get_full_path(),
                    file_target_path: file_2.get_full_path(),
                    file_source_label: file_1.label().map(str::to_owned),
                    file_target_label: file_2.label().map(str::to_owned),
                    file_source_previous: previous_revision(
                        nav_history,
                        0,
                        &file_1.get_full_path(),
                    ),
                    file_target_previous: previous_revision(
                        nav_history,
                        1,
                        &file_2.get_full_path(),
                    ),
                    launch_error: self.launch_error.as_deref(),
                    file_source_root: file_1.get_root(),
                    file_target_root: file_2.get_root(),
                    file_source_root_valid: file_1.root_valid(),
                    file_target_root_valid: file_2.root_valid(),
                    file_source_path_valid: file_1.path_valid(),
                    file_target_path_valid: file_2.path_valid(),
                    file_source_load_error: file_1.load_error().map(str::to_owned),
                    file_source_loading: file_1.get_loading_path().is_some(),
                    file_target_load_error: file_2.load_error().map(str::to_owned),
                    file_target_loading: file_2.get_loading_path().is_some(),
                    active_highlights: &active_highlights,
                    highlight_side,
                    conflict_cursor: &mut conflict_cursor,
                    pins: &pins,
                    pins_request: &mut pins_request,
                    find_cursor: &mut find_cursor,
                    find_matchers: find_matchers.each_ref().map(Option::as_ref),
                    find_hit,
                    find_count,
                    active_side: &mut active_side,
                    toggle_active_side: std::mem::take(&mut self.toggle_active_side),
                    diff_loading: diff_processor.is_in_progress() || strings_pending,
                    code_language,
                    revert_request: &mut None,
                    block_toggle_request: &mut block_toggle_request,
                    toggle_all_held: toggle_all.is_held(),
                    toggle_all_request: &mut toggle_all_request,
                    hex_view: hex_bytes.then(|| hex_processor.view_ctx()),
                    hex_mode: is_hex.then_some(&mut *hex_settings),
                    pdb_options: pdb_dump_shown.then_some(&mut *pdb_options),
                    string_offsets,
                    image_view: (is_image || side_shows(ViewerKind::Image))
                        .then_some(image_processor),
                    image_settings: Some(image_settings),
                    audio_view: (media_view == Some(ViewerKind::Audio)
                        || video_audio
                        || side_shows(ViewerKind::Audio))
                    .then_some((audio_processor, audio_settings)),
                    media_view,
                    audio_meta: (media_view == Some(ViewerKind::Audio)).then_some(audio_meta),
                    video_frames: (media_view == Some(ViewerKind::Video)
                        || side_shows(ViewerKind::Video))
                    .then_some(video_frames),
                    video_meta: (media_view == Some(ViewerKind::Video)).then_some(video_meta),
                    side_viewers: None,
                    viewer_override: &mut viewer_override.kind,
                    viewer_fallback: viewer_fallback.as_deref(),
                    phase_overlay: self.phase_overlay,
                    sides_swapped: *sides_swapped,
                    keymap: &keybindings.keymap,
                },
            };
            if let Some(kinds) = sides {
                let file_diff = &mut behavior.ctx_file_diff;
                file_diff.side_viewers = Some(SideViewers {
                    kinds,
                    text: side_text.as_ref(),
                    text_scroll,
                    hex: side_shows(ViewerKind::Hex).then(|| hex_processor.view_ctx()),
                    hex_scroll,
                    center_jumps: *center_jumps,
                    image: file_diff.image_view.take(),
                    image_settings: file_diff.image_settings.take(),
                    audio: file_diff.audio_view.take(),
                    video: file_diff.video_frames.take(),
                });
            }

            if let Some(request) = self.open_merge_form.ui(ctx) {
                self.merge = Some(MergeView::new(request, *myers_diff_algorithm));
            }
            let close_requested = ctx.input(|i| i.viewport().close_requested());
            let mut close_merge = None;
            if close_requested && let Some(merge) = &mut self.merge {
                close_merge = merge.request_close(Closing::App);
                if close_merge.is_none() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
            }
            self.frame_stats.mark("other");
            ui.with_layout(
                Layout::left_to_right(egui::Align::Min),
                |ui| match &mut self.merge {
                    Some(merge) => {
                        let closed = merge.ui(ui, code_language_override.as_deref(), *center_jumps);
                        close_merge = close_merge.or(closed);
                    }
                    None => self.tree.ui(&mut behavior, ui),
                },
            );
            self.frame_stats.mark("panes");
            if let Some(closing) = close_merge {
                let merge = self.merge.take().expect("only an open merge closes");
                self.exit_code.store(merge.exit_code(), Ordering::Relaxed);
                // The prompt's close request was cancelled frames ago, so it is made again.
                if closing == Closing::App && !close_requested {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }

            // TODO: Remove clones
            let clone_find = behavior.ctx_file_diff.find_cursor.clone();
            let clone_active_side = *behavior.ctx_file_diff.active_side;
            let clone_conflict = behavior.ctx_file_diff.conflict_cursor.clone();

            if let Some(file_path) = behavior.ctx_file_diff.set_file_1_root_request.take() {
                log::debug!("Set new root from root_request_1: {}", file_path);
                app_ctx.file_1.set_root(file_path);
            }
            if let Some(file_path) = behavior.ctx_file_diff.set_file_2_root_request.take() {
                log::debug!("Set new root from root_request_2: {}", file_path);
                app_ctx.file_2.set_root(file_path);
            }

            match behavior.ctx_file_diff.open_file_request.take() {
                Some(ActiveSide::Left) => {
                    Self::open_file_picker(app_ctx.file_1.get_tx(), ctx.clone())
                }
                Some(ActiveSide::Right) => {
                    Self::open_file_picker(app_ctx.file_2.get_tx(), ctx.clone())
                }
                None => {}
            }
            if let Some(file_path) = behavior.ctx_file_diff.load_file_1_request.take() {
                log::debug!("Set new path from file_1_request: {}", file_path);
                app_ctx.file_1.set_path(file_path);
            }
            if let Some(file_path) = behavior.ctx_file_diff.load_file_2_request.take() {
                log::debug!("Set new path from file_2_request: {}", file_path);
                app_ctx.file_2.set_path(file_path);
            }
            match behavior.ctx_file_diff.back_to_previous_request.take() {
                Some(ActiveSide::Left) => {
                    back_to_previous_revision(&mut app_ctx.file_1, nav_history, 0)
                }
                Some(ActiveSide::Right) => {
                    back_to_previous_revision(&mut app_ctx.file_2, nav_history, 1)
                }
                None => {}
            }
            if let Some(revert_request) = *behavior.ctx_file_diff.revert_request {
                log::debug!(
                    "new revert request: hunk {} into {:?}",
                    revert_request.hunk,
                    revert_request.target
                );
                // Plan against the files the shown rows were built from, not the latest loads.
                let planned = behavior
                    .ctx_file_diff
                    .diff_ctx
                    .ok_or(RevertRefusal::NoSuchHunk)
                    .and_then(|diff_ctx| {
                        revert::plan_hunk_revert(diff_ctx, revert_request, &std::env::temp_dir())
                    });
                match planned {
                    Ok(planned) => {
                        let path = planned.path.clone();
                        let written = revert::write_revert(
                            planned,
                            &std::env::temp_dir(),
                            &P4Command::new(false).for_file(UniversalPath::Local(path.clone())),
                        );
                        let result = match written {
                            Ok(RevertWrite::NeedsP4Edit(pending)) => {
                                log::info!(
                                    "{} is read-only and Perforce-managed, asking to p4 edit it",
                                    path.display()
                                );
                                self.pending_p4_edit = Some((revert_request.target, pending));
                                None
                            }
                            Ok(RevertWrite::Written(record)) => Some(Ok(record)),
                            Err(e) => Some(Err(e)),
                        };
                        if let Some(result) = result {
                            if result.is_ok() {
                                nav_history.clear_redo();
                            }
                            Self::finish_revert_write(
                                result,
                                &path,
                                revert_request.target,
                                revert_history,
                                diff_processor,
                                &mut app_ctx.file_1,
                                &mut app_ctx.file_2,
                            );
                        }
                    }
                    Err(refusal) => {
                        log::error!(
                            "Revert refused: {} (hunk {} into {:?})",
                            refusal,
                            revert_request.hunk,
                            revert_request.target
                        );
                    }
                }
            }

            if let Some((target, pending)) = self.pending_p4_edit.take() {
                let mut confirmed = None;
                let modal = egui::Modal::new(egui::Id::new("p4_edit_prompt")).show(ctx, |ui| {
                    ui.heading("Check out for edit?");
                    ui.label(format!(
                        "{} is read-only and managed by Perforce.",
                        pending.path().display()
                    ));
                    ui.label("Run p4 edit on it and apply the revert?");
                    ui.horizontal(|ui| {
                        if ui.button("p4 edit and revert").clicked() {
                            confirmed = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            confirmed = Some(false);
                        }
                    });
                });
                // Escape or a click outside the prompt declines.
                if modal.should_close() {
                    confirmed.get_or_insert(false);
                }
                match confirmed {
                    Some(true) => {
                        let path = pending.path().to_path_buf();
                        let p4 = P4Command::new(false).for_file(UniversalPath::Local(path.clone()));
                        let result = pending.confirm(&std::env::temp_dir(), &p4);
                        if result.is_ok() {
                            nav_history.clear_redo();
                        }
                        Self::finish_revert_write(
                            result,
                            &path,
                            target,
                            revert_history,
                            diff_processor,
                            &mut app_ctx.file_1,
                            &mut app_ctx.file_2,
                        );
                    }
                    Some(false) => {
                        log::info!(
                            "Revert cancelled: p4 edit declined for {}",
                            pending.path().display()
                        );
                    }
                    None => self.pending_p4_edit = Some((target, pending)),
                }
            }

            drop(behavior);
            if let Some(pins) = pins_request {
                diff_processor.set_pins(pins);
            }
            diff_processor.find_cursor = clone_find;
            diff_processor.set_active_side(clone_active_side);
            diff_processor.conflict_cursor = clone_conflict;
            if let Some((key, toggle)) = block_toggle_request {
                diff_processor.toggle_block(key, toggle);
                ctx.request_repaint();
            }
            if toggle_all_request {
                toggle_all_options(
                    toggle_all,
                    &mut app_ctx.diff_options,
                    &mut app_ctx.word_wrap,
                );
            }

            for (_tile_id, tile) in self.tree.tiles.iter() {
                if let Tile::Pane(Pane::FileDiff(..)) = tile {
                    let source = app_ctx.file_1.display_name();
                    let target = app_ctx.file_2.display_name();
                    let total_adds = app_ctx
                        .diff_processor
                        .get_minimal_diff_ctx()
                        .as_ref()
                        .map(|f| f.num_add_deletes)
                        .unwrap_or_default()
                        .0;
                    let total_deletes = app_ctx
                        .diff_processor
                        .get_minimal_diff_ctx()
                        .map(|f| f.num_add_deletes)
                        .unwrap_or_default()
                        .1;
                    let unit = app_ctx
                        .diff_processor
                        .get_minimal_diff_ctx()
                        .and_then(|f| f.change_unit)
                        .map_or(String::new(), |unit| format!(" {}", unit.label()));
                    let counts = if sides.is_some() {
                        "types differ".to_string()
                    } else if is_hex {
                        "hex".to_string()
                    } else if is_image {
                        "image".to_string()
                    } else if let Some(kind) = media_view {
                        kind.name().to_lowercase()
                    } else {
                        format!("+{}/-{}{}", total_adds, total_deletes, unit)
                    };
                    let title = if source.is_empty() && target.is_empty() {
                        "zdiff".to_owned()
                    } else {
                        format!("zdiff [{counts}] - {source}, {target}")
                    };
                    send_title(ctx, &mut self.window_title, title);
                    break;
                }
            }
            self.frame_stats.mark("other");
        });
    }
}

// Every viewport command requests a repaint, so sending one each frame never lets the app idle.
fn send_title(ctx: &egui::Context, sent: &mut Option<String>, title: String) {
    if sent.as_ref() != Some(&title) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
        *sent = Some(title);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_egui::text_mode::TextMode;

    #[test]
    fn an_unchanged_window_title_lets_the_app_idle() {
        let ctx = egui::Context::default();
        let mut sent = None;
        let mut pass = |title: &str| {
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                send_title(ctx, &mut sent, title.to_owned());
            });
            output.viewport_output[&egui::ViewportId::ROOT].repaint_delay
        };
        for _ in 0..4 {
            pass("zdiff [+1/-1] - a, b");
        }
        assert_eq!(pass("zdiff [+1/-1] - a, b"), std::time::Duration::MAX);
        assert_eq!(pass("zdiff [+2/-1] - a, b"), std::time::Duration::ZERO);
        assert_eq!(sent.as_deref(), Some("zdiff [+2/-1] - a, b"));
    }

    fn shape_texts(shape: &egui::Shape, texts: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(text) => texts.push(text.galley.text().to_owned()),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| shape_texts(s, texts)),
            _ => {}
        }
    }

    #[test]
    fn a_single_loaded_file_shows_on_its_own_side_only() {
        let texts = single_file_texts(TextMode::SideBySide);
        let count = |wanted: &str| texts.iter().filter(|text| *text == wanted).count();
        assert_eq!(count("only_side_line"), 1);
        assert_eq!(count("No file"), 1);
    }

    #[test]
    fn a_single_loaded_file_is_numbered_on_its_own_side_only_in_the_unified_view() {
        let texts = single_file_texts(TextMode::Unified);
        let count = |wanted: &str| texts.iter().filter(|text| *text == wanted).count();
        assert_eq!(count("only_side_line"), 1);
        assert_eq!(count("1"), 1);
    }

    fn single_file_texts(text_mode: TextMode) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("only.txt");
        std::fs::write(&path, "only_side_line\n").unwrap();
        let mut app = super::super::ZApp::new();
        let state = app.state.as_mut().unwrap().ctx_mut();
        state.file_1.set_path(UniversalPath::from(path));
        state.text_mode = text_mode;
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut diffed = false;
        loop {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 1000.0),
                )),
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
            if diffed {
                let mut texts = Vec::new();
                output
                    .shapes
                    .iter()
                    .for_each(|clipped| shape_texts(&clipped.shape, &mut texts));
                return texts;
            }
            let state = app.state.as_mut().unwrap().ctx_mut();
            diffed = state.diff_processor.get_minimal_diff_ctx().is_some();
            assert!(std::time::Instant::now() < deadline, "the diff never came");
        }
    }

    #[test]
    fn a_pane_loading_its_files_shows_no_welcome_screen() {
        let dir = tempfile::tempdir().unwrap();
        let text: String = (0..400_000).map(|i| format!("line {i}\n")).collect();
        let mut app = super::super::ZApp::new();
        let state = app.state.as_mut().unwrap().ctx_mut();
        for (file, name) in [(&mut state.file_1, "a.txt"), (&mut state.file_2, "b.txt")] {
            let path = dir.path().join(name);
            std::fs::write(&path, &text).unwrap();
            file.set_path(UniversalPath::from(path));
        }
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut loading_frames = 0;
        loop {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 1000.0),
                )),
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| eframe::App::update(&mut app, ctx, &mut frame));
            let state = app.state.as_mut().unwrap().ctx_mut();
            let loading = state.file_1.get_loading_path().is_some()
                || state.file_2.get_loading_path().is_some();
            if loading {
                let mut texts = Vec::new();
                output
                    .shapes
                    .iter()
                    .for_each(|clipped| shape_texts(&clipped.shape, &mut texts));
                assert!(!texts.iter().any(|text| text == "Welcome to ZDiff"));
                assert!(texts.iter().any(|text| text == "Loading files..."));
                loading_frames += 1;
            } else if loading_frames > 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the files never loaded"
            );
        }
    }
}
