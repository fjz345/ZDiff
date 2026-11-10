use super::{
    ZApp,
    launch::{toggle_all_options, toggle_option},
    quickdiff::{
        LocalFileRefusal, apply_quick_diff, diff_against_local_file, run_diff_against_shelf_base,
    },
    state::AppState,
};
use eframe::egui::{self, PointerButton};
use zcommon_ui::keymap::{Action, Shortcut};
use zdiff::universal_path::UniversalPath;

use eframe::epaint::Pos2;

use crate::{
    keybindings::FileAction,
    p4::{P4Command, set_p4_slot_profile},
    revert::HistoryStep,
    toggle_all::OptionSet,
    ui_egui::export,
    viewer::video_meta::VideoTab,
};

impl ZApp {
    pub(super) fn request_shutdown(&mut self) {
        if let Some(state) = self.state.take() {
            let ctx = state.into_ctx();
            self.state = Some(AppState::Exit(ctx));
        }
    }

    pub(super) fn process_ctx_inputs(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // The editor takes the captured key press only later in this frame, when it is drawn.
        let capturing = self.keymap_editor.is_capturing();
        let app_state_ctx = self
            .state
            .as_mut()
            .expect("State was not valid while processing inputs")
            .ctx_mut();
        let user_quit: bool = false;
        // Read outside ctx.input: Context methods inside its closure can deadlock.
        let text_focused = ctx.wants_keyboard_input();
        let mut history_step = None;
        let mut diff_local_file = false;
        let mut diff_shelf_base = false;
        let mut tab_pressed = false;
        let mut esc_pressed = false;
        let mut save_merge = false;
        let mut export_diff = false;
        let mut open_style = false;
        {
            let _input_ctx = ctx.input(|r| {
                if capturing {
                    return;
                }
                tab_pressed = r.key_pressed(egui::Key::Tab);
                esc_pressed = r.key_pressed(egui::Key::Escape);

                // DoubleLeftClick
                if r.pointer.button_double_clicked(PointerButton::Primary) {
                    let mouse_pos = r.pointer.interact_pos().unwrap();
                    log::info!("double click @({},{})", mouse_pos.x, mouse_pos.y);
                }
                if r.pointer.button_clicked(PointerButton::Middle) {
                    let mouse_pos: Pos2 = r.pointer.interact_pos().unwrap();
                    log::info!("middle click @({},{})", mouse_pos.x, mouse_pos.y);
                }

                let actions = app_state_ctx
                    .keybindings
                    .keymap
                    .dispatch(&r.events, text_focused);
                for action in actions {
                    log::info!("Action triggered: {}", action.label());
                    match action {
                        FileAction::OpenFileSource => {
                            Self::open_file_picker(app_state_ctx.file_1.get_tx(), ctx.clone())
                        }
                        FileAction::OpenFileTarget => {
                            Self::open_file_picker(app_state_ctx.file_2.get_tx(), ctx.clone())
                        }
                        FileAction::RefreshDiff => {
                            app_state_ctx.diff_processor.reset_ctx();
                            Self::refresh_file_contents(
                                &mut app_state_ctx.file_1,
                                &mut app_state_ctx.file_2,
                            );
                        }
                        FileAction::RefreshDiffRowsOnly => {
                            app_state_ctx.diff_processor.reset_ctx();
                        }
                        FileAction::OpenKeybindings => self.open_shortcuts_window = true,
                        FileAction::OpenUniversalPath => self.open_universal_path_window = true,
                        FileAction::Find => app_state_ctx.find_open = true,
                        FileAction::Goto => app_state_ctx.goto_open = true,
                        FileAction::SaveMerge => save_merge = true,
                        FileAction::ExportDiff => export_diff = true,
                        FileAction::OpenMerge => self.open_merge_form.open = true,
                        FileAction::ClearFilePaths => Self::clear_file_paths(
                            &mut app_state_ctx.file_1,
                            &mut app_state_ctx.file_2,
                            &mut app_state_ctx.diff_processor,
                        ),
                        FileAction::OpenFileViewers => self.open_viewers_window = true,
                        FileAction::OpenStyle => open_style = true,
                        FileAction::NextFrame | FileAction::PrevFrame => {
                            if app_state_ctx.video_meta.tab == VideoTab::Frames {
                                app_state_ctx
                                    .video_frames
                                    .step(action == FileAction::NextFrame);
                            }
                        }
                        FileAction::NextConflict | FileAction::PrevConflict
                            if let Some(merge) = &mut self.merge =>
                        {
                            merge.navigate(action == FileAction::NextConflict);
                        }
                        FileAction::NextConflict => {
                            app_state_ctx.diff_processor.conflict_cursor.inc();
                            log::info!(
                                "ConflictCursor++ @{}",
                                app_state_ctx.diff_processor.conflict_cursor.get()
                            );
                        }
                        FileAction::PrevConflict => {
                            app_state_ctx.diff_processor.conflict_cursor.dec();
                            log::info!(
                                "ConflictCursor-- @{}",
                                app_state_ctx.diff_processor.conflict_cursor.get()
                            );
                        }
                        // The popup takes Enter; stepping here would move the kept hits under an error.
                        FileAction::NextFind if !app_state_ctx.find_open => {
                            app_state_ctx.diff_processor.find_cursor.inc();
                            log::info!(
                                "FindCursor++ @{}",
                                app_state_ctx.diff_processor.find_cursor.get()
                            );
                        }
                        FileAction::PrevFind if !app_state_ctx.find_open => {
                            app_state_ctx.diff_processor.find_cursor.dec();
                            log::info!(
                                "FindCursor-- @{}",
                                app_state_ctx.diff_processor.find_cursor.get()
                            );
                        }
                        FileAction::NextFind | FileAction::PrevFind => {}
                        FileAction::RevisionGraph => match &app_state_ctx.file_1.get_full_path() {
                            file @ UniversalPath::Depot(..) => {
                                match P4Command::open_revision_graph(file) {
                                    Ok(_) => {
                                        log::info!("Revision graph returned Ok");
                                    }
                                    Err(e) => log::error!("Failed to open revision graph: {e}"),
                                }
                            }
                            UniversalPath::Local(path_buf) | UniversalPath::Git(path_buf, _) => {
                                log::info!(
                                    "Can not open revision graph for local path {}",
                                    path_buf.display()
                                );
                            }
                        },
                        FileAction::TimelapseView => match &app_state_ctx.file_1.get_full_path() {
                            file @ UniversalPath::Depot(..) => {
                                match P4Command::open_timelapse_view(file) {
                                    Ok(_) => {
                                        log::info!("Timelapse view returned Ok");
                                    }
                                    Err(e) => log::error!("Failed to open timelapse view: {e}"),
                                }
                            }
                            UniversalPath::Local(path_buf) | UniversalPath::Git(path_buf, _) => {
                                log::info!(
                                    "Can not open timelapse view for local path {}",
                                    path_buf.display()
                                );
                            }
                        },
                        FileAction::UndoRevert => history_step = Some(HistoryStep::Undo),
                        FileAction::Redo => history_step = Some(HistoryStep::Redo),
                        FileAction::DiffLocalFile => diff_local_file = true,
                        FileAction::DiffShelfBase => diff_shelf_base = true,
                        FileAction::SwapSides => {
                            app_state_ctx.sides_swapped = !app_state_ctx.sides_swapped
                        }
                        FileAction::IgnoreWhitespace
                        | FileAction::IgnoreComments
                        | FileAction::IgnoreRegex
                        | FileAction::IgnoreLineEndings
                        | FileAction::IgnoreFileMarkers
                        | FileAction::HighlightRows
                        | FileAction::GhostRows
                        | FileAction::LinkHScroll
                        | FileAction::WordWrap
                        | FileAction::DiffOnly
                        | FileAction::FewerContextRows
                        | FileAction::MoreContextRows => toggle_option(app_state_ctx, action),
                        FileAction::ToggleAllOptions => toggle_all_options(
                            &mut app_state_ctx.toggle_all,
                            &mut app_state_ctx.diff_options,
                            &mut app_state_ctx.h_scroll_linked,
                            &mut app_state_ctx.word_wrap,
                        ),
                    }
                }
                let current = OptionSet::read(
                    &app_state_ctx.diff_options,
                    app_state_ctx.h_scroll_linked,
                    app_state_ctx.word_wrap,
                );
                app_state_ctx.toggle_all.observe(current);

                let handle_kb = |opt: &Option<Shortcut>, func: &mut dyn FnMut(Shortcut)| {
                    if let Some(kb) = opt {
                        if kb.matches(r) {
                            log::info!("Shortcut triggered: {}", kb.format());
                            func(*kb);
                        }
                    }
                };

                for (i, (kb, path)) in app_state_ctx
                    .keybindings
                    .user_quick_diffs
                    .iter()
                    .enumerate()
                {
                    handle_kb(kb, &mut |kb| {
                        log::info!(
                            "User Quick Diff Shortcut [{}] triggered: {}",
                            i + 1,
                            kb.format()
                        );

                        if let Err(refusal) = apply_quick_diff(
                            path,
                            &mut app_state_ctx.file_1,
                            &mut app_state_ctx.file_2,
                            &std::env::temp_dir(),
                        ) {
                            log::warn!(
                                "User Quick Diff Shortcut [{}] disabled: {}:\n{}\n{}",
                                i + 1,
                                refusal,
                                app_state_ctx.file_1.get_full_path(),
                                app_state_ctx.file_2.get_full_path()
                            );
                        } else {
                            // Before the sides load: their fetches read it on the loader thread.
                            set_p4_slot_profile(path.p4_profile);
                            if path.source.is_some() {
                                log::info!(
                                    "User Quick Diff Shortcut set paths:\nSource: {}\nTarget: {}",
                                    app_state_ctx.file_1.get_full_path(),
                                    app_state_ctx.file_2.get_full_path()
                                );
                            } else {
                                log::info!(
                                    "User Quick Diff Shortcut set paths:\nTarget: {}",
                                    app_state_ctx.file_2.get_full_path()
                                );
                            }
                        }
                    });
                }
            });
        }
        if tab_pressed && !text_focused {
            // begin_pass already made Tab a focus move; cancel it before any widget takes it.
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
            self.toggle_active_side = true;
        }
        if esc_pressed
            && !app_state_ctx.find_open
            && !app_state_ctx.goto_open
            && self.pending_p4_edit.is_none()
        {
            app_state_ctx.diff_processor.clear_results();
            app_state_ctx.hex_processor.clear_highlight();
        }
        if open_style {
            self.style_window.open(ctx);
        }
        // After the input closure: this writes to disk.
        if save_merge {
            match &mut self.merge {
                Some(merge) => merge.request_save(),
                None => log::info!("Save Merge: no merge is open"),
            }
        }
        if export_diff {
            let diff_ctx = (self.merge.is_none() && app_state_ctx.shows_text_diff())
                .then(|| app_state_ctx.diff_processor.get_minimal_diff_ctx())
                .flatten();
            match diff_ctx {
                Some(diff_ctx) => export::export_diff(&diff_ctx),
                None => log::info!("Export Diff: no text diff is shown"),
            }
        }
        if let Some(step) = history_step {
            Self::step_revert_history(app_state_ctx, step);
        }
        // After the input closure too: p4 can take a while.
        if diff_local_file {
            let p4 = P4Command::new(false).for_file(app_state_ctx.file_1.get_full_path());
            match diff_against_local_file(&mut app_state_ctx.file_1, &mut app_state_ctx.file_2, &p4)
            {
                Ok(local) => log::info!("Diffing against the local file {}", local.display()),
                Err(refusal @ LocalFileRefusal::LocalSource(_)) => {
                    log::info!("Can not diff against the local file: {refusal}")
                }
                Err(refusal) => log::error!("Can not diff against the local file: {refusal}"),
            }
        }
        if diff_shelf_base {
            run_diff_against_shelf_base(&mut app_state_ctx.file_1, &mut app_state_ctx.file_2);
        }

        if user_quit {
            self.request_shutdown();
        }
    }
}
