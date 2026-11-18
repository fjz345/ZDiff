use super::{ZApp, quickdiff::run_diff_against_shelf_base};
use eframe::egui::{self, containers::menu::MenuConfig};
use zcommon::vcs::revision::PathKind;
use zcommon_ui::common::{section_heading, show_custom_popup, show_settings_window};
use zcommon_ui::frame_stats;
use zcommon_ui::h_scroll::WheelModifier;
use zcommon_ui::keymap::{Action, menu_item};
use zcommon_ui::shared_keymap::load_shared;
use zdiff::myers::{DiffPhases, MyersDiffAlgorithm};

use crate::{
    cli::{self},
    diff_ctx::DiffProcessor,
    diff_phases::diff_phases_menu,
    file::FileProcessor,
    keybindings::{FileAction, Keybindings, ui_keybindings},
    language_icon::{emoji_renders, language_icon, show_language_icon},
    p4::{P4Profiles, ui_p4_profiles, update_p4_profiles},
    phase_trace_window::ui_phase_trace,
    folder::{folder_keymap::FolderAction, fs_tree::FolderRole},
    ui_egui::{export, file_role::FileRole, syntax},
    viewer::{FfmpegSetting, SharedKinds, ui_extension_map, ui_ffmpeg_setting},
};

const SYNTAX_LIST_ROWS: f32 = 16.0;

pub(super) struct MenuCtx<'a> {
    pub file_1: &'a mut FileProcessor,
    pub file_2: &'a mut FileProcessor,
    pub diff_processor: &'a mut DiffProcessor,
    pub find_open: &'a mut bool,
    pub goto_open: &'a mut bool,
    pub sides_swapped: &'a mut bool,
    pub keybindings: &'a mut Keybindings,
    pub p4_profiles: &'a mut P4Profiles,
    pub kinds: &'a mut SharedKinds,
    pub ffmpeg: &'a mut FfmpegSetting,
    pub myers_diff_algorithm: &'a mut MyersDiffAlgorithm,
    pub diff_phases: &'a mut DiffPhases,
    pub code_language_override: &'a mut Option<String>,
    pub syntax_filter: &'a mut String,
    pub search_syntax_variants: &'a mut bool,
    pub detected_languages: [Option<&'a str>; 2],
    pub h_scroll_modifier: &'a mut WheelModifier,
    pub h_scroll_linked: &'a mut bool,
    pub center_jumps: &'a mut bool,
    pub scope_breadcrumb: &'a mut bool,
    pub scope_block_headers: &'a mut bool,
    pub revert_buttons: &'a mut bool,
    pub text_diff_shown: bool,
    pub folders: bool,
}

impl ZApp {
    pub(super) fn show_menu(&mut self, ui: &mut egui::Ui, menu_ctx: MenuCtx<'_>) {
        let MenuCtx {
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
            diff_phases,
            code_language_override,
            syntax_filter,
            search_syntax_variants,
            detected_languages,
            h_scroll_modifier,
            h_scroll_linked,
            center_jumps,
            scope_breadcrumb,
            scope_block_headers,
            revert_buttons,
            text_diff_shown,
            folders,
        } = menu_ctx;
        let keymap = &keybindings.keymap;
        ui.horizontal(|ui| {
            egui::MenuBar::new()
                .config(
                    MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
                )
                .ui(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    ui.menu_button("File", |ui| {
                        if menu_item(ui, "Open Left", keymap, FileAction::OpenFileSource)
                            .clicked()
                        {
                            let ctx = ui.ctx().clone();
                            Self::open_file_picker(PathKind::File, file_1.get_tx(), ctx);
                        }
                        if menu_item(ui, "Open Right", keymap, FileAction::OpenFileTarget)
                            .clicked()
                        {
                            let ctx = ui.ctx().clone();
                            Self::open_file_picker(PathKind::File, file_2.get_tx(), ctx);
                        }
                        for (label, file) in [
                            ("Open Left Folder...", &mut *file_1),
                            ("Open Right Folder...", &mut *file_2),
                        ] {
                            if ui.button(label).clicked() {
                                let ctx = ui.ctx().clone();
                                Self::open_file_picker(PathKind::Folder, file.get_tx(), ctx);
                                ui.close();
                            }
                        }
                        ui.add_enabled_ui(!folders, |ui| {
                            if menu_item(ui, "Open Merge...", keymap, FileAction::OpenMerge)
                                .clicked()
                            {
                                self.open_merge_form.open = true;
                            }
                        });
                        if menu_item(ui, "Swap Sides", keymap, FileAction::SwapSides).clicked() {
                            *sides_swapped = !*sides_swapped;
                        }
                        ui.add_enabled_ui(!folders, |ui| {
                            if menu_item(
                                ui,
                                "Diff Shelved File Against Its Base Revision",
                                keymap,
                                FileAction::DiffShelfBase,
                            )
                            .clicked()
                            {
                                run_diff_against_shelf_base(file_1, file_2);
                            }
                        });
                        if folders {
                            let keymap = self.folder.keymap();
                            if menu_item(ui, "Refresh", keymap, FolderAction::Refresh).clicked() {
                                self.refresh_folders(file_1, file_2);
                            }
                        } else if menu_item(ui, "Refresh", keymap, FileAction::RefreshDiff)
                            .clicked()
                        {
                            diff_processor.reset_ctx();
                            Self::refresh_file_contents(file_1, file_2);
                        }
                        ui.add_enabled_ui(!folders, |ui| {
                            if menu_item(
                                ui,
                                "Recompute Diff (Keep Loaded Files)",
                                keymap,
                                FileAction::RefreshDiffRowsOnly,
                            )
                            .clicked()
                            {
                                diff_processor.reset_ctx();
                            }
                        });
                        if menu_item(ui, "Clear File Paths", keymap, FileAction::ClearFilePaths)
                            .clicked()
                        {
                            Self::clear_file_paths(file_1, file_2, diff_processor);
                        }
                        if folders {
                            let duplicates = self.folder.duplicates_shown();
                            let export_label = if duplicates {
                                "Export Duplicates..."
                            } else {
                                "Export Folder Diff..."
                            };
                            let keymap = self.folder.keymap();
                            if menu_item(ui, export_label, keymap, FolderAction::Export).clicked() {
                                self.folder.request_export();
                            }
                            ui.add_enabled_ui(!duplicates, |ui| {
                                let keymap = self.folder.keymap();
                                if menu_item(ui, "Find", keymap, FolderAction::Find).clicked() {
                                    self.folder.open_find();
                                }
                            });
                        } else {
                            ui.add_enabled_ui(text_diff_shown, |ui| {
                                if menu_item(ui, "Export Diff...", keymap, FileAction::ExportDiff)
                                    .clicked()
                                {
                                    match diff_processor.get_minimal_diff_ctx() {
                                        Some(diff_ctx) => export::export_diff(&diff_ctx),
                                        None => log::info!("Export Diff: the diff is not ready"),
                                    }
                                }
                            });
                            if menu_item(ui, "Find", keymap, FileAction::Find).clicked() {
                                *find_open = true;
                            }
                        }
                        ui.add_enabled_ui(!folders, |ui| {
                            if menu_item(ui, "Go to Line / Offset", keymap, FileAction::Goto)
                                .clicked()
                            {
                                *goto_open = true;
                            }
                        });
                    });

                    ui.menu_button("Options", |ui| {
                        ui.menu_button("Myers Algo", |ui| {
                            ui.radio_value(
                                myers_diff_algorithm,
                                MyersDiffAlgorithm::Trace,
                                "Trace",
                            );
                            ui.radio_value(
                                myers_diff_algorithm,
                                MyersDiffAlgorithm::Linear,
                                "Linear",
                            );
                            ui.radio_value(
                                myers_diff_algorithm,
                                MyersDiffAlgorithm::LinearMT,
                                "LinearMT",
                            );
                        });
                        diff_phases_menu(ui, diff_phases);
                        ui.menu_button("Syntax Highlighting", |ui| {
                            let ctx = ui.ctx().clone();
                            let icon_for = |name: &str| {
                                language_icon(name, |emoji| emoji_renders(&ctx, emoji))
                            };
                            let auto_label = match detected_languages {
                                [Some(a), Some(b)] if a != b => format!("Auto ({a} / {b})"),
                                [Some(a), _] | [_, Some(a)] => format!("Auto ({a})"),
                                [None, None] => "Auto (plain text)".to_string(),
                            };
                            if ui
                                .radio(code_language_override.is_none(), auto_label)
                                .clicked()
                            {
                                *code_language_override = None;
                            }
                            ui.separator();
                            ui.add(
                                egui::TextEdit::singleline(syntax_filter)
                                    .hint_text("Filter (name or extension)"),
                            );
                            ui.checkbox(search_syntax_variants, "Show variants when filtering")
                                .on_hover_text(
                                    "A typed filter also lists variants such as HTML (Rails) or \
                                     Git Config; the unfiltered list never shows them",
                                );
                            let selected = code_language_override
                                .as_deref()
                                .and_then(syntax::language_name);
                            let list_height = SYNTAX_LIST_ROWS
                                * (ui.spacing().interact_size.y + ui.spacing().item_spacing.y);
                            // Popups get last frame's height; without the min a filtered list never regrows.
                            egui::ScrollArea::vertical()
                                .max_height(list_height)
                                .min_scrolled_height(list_height)
                                // Fill so the bar hugs the menu edge; filling while sizing locks it at 600px.
                                .auto_shrink([ui.is_sizing_pass(), true])
                                .show(ui, |ui| {
                                    for language in syntax::languages().iter().filter(|l| {
                                        selected == Some(l.name)
                                            || l.listed(syntax_filter, *search_syntax_variants)
                                    }) {
                                        ui.horizontal(|ui| {
                                            show_language_icon(ui, icon_for(language.name));
                                            if ui
                                                .radio(
                                                    selected == Some(language.name),
                                                    language.name,
                                                )
                                                .clicked()
                                            {
                                                *code_language_override =
                                                    Some(language.name.to_string());
                                            }
                                        });
                                    }
                                });
                        });
                        ui.menu_button("Horizontal Scroll Wheel", |ui| {
                            ui.radio_value(h_scroll_modifier, WheelModifier::Shift, "Shift+Wheel");
                            ui.radio_value(h_scroll_modifier, WheelModifier::Ctrl, "Ctrl+Wheel");
                        });
                        ui.checkbox(h_scroll_linked, "Link Horizontal Scrolling")
                            .on_hover_text(keymap.with_shortcut(
                                FileAction::LinkHScroll,
                                "Scroll the left and right sides sideways together",
                            ));
                        ui.checkbox(center_jumps, "Center Jumps").on_hover_text(
                            "Go to line, next/previous difference and find put their target \
                             in the middle of the view; off puts it at the top",
                        );
                        ui.checkbox(scope_breadcrumb, "Scope Bar as Breadcrumb")
                            .on_hover_text(
                                "The scopes the top line is in show as one row of names; off shows \
                             each scope's first line as its own row",
                            );
                        ui.checkbox(scope_block_headers, "Scope Header per Collapsed Block")
                            .on_hover_text(
                                "In diff only, each collapsed block names the scopes its next change \
                             is in; clicking a name scrolls to that scope's first line",
                            );
                        ui.checkbox(revert_buttons, "Revert Buttons in Center Column")
                            .on_hover_text(
                                "The < and > buttons that replace a hunk on one side; off leaves \
                             the same replace in a line's right-click menu",
                            );
                        if menu_item(ui, "P4Config", keymap, FileAction::OpenUniversalPath)
                            .clicked()
                        {
                            self.open_universal_path_window = true;
                        }
                        if menu_item(
                            ui,
                            "Keyboard Shortcuts",
                            keymap,
                            FileAction::OpenKeybindings,
                        )
                        .clicked()
                        {
                            self.open_shortcuts_window = true;
                        }
                        if menu_item(ui, "File Viewers", keymap, FileAction::OpenFileViewers)
                            .clicked()
                        {
                            self.open_viewers_window = true;
                        }
                        if menu_item(ui, "Style", keymap, FileAction::OpenStyle).clicked() {
                            self.style_window.open(ui.ctx());
                        }
                        if folders
                            && menu_item(
                                ui,
                                "External Diff Tools",
                                self.folder.keymap(),
                                FolderAction::OpenDiffTools,
                            )
                            .clicked()
                        {
                            self.folder.open_tools_window();
                        }
                        ui.checkbox(&mut self.show_frame_breakdown, "Frame Time Breakdown");
                    });

                    #[cfg(debug_assertions)]
                    ui.menu_button("Debug", |ui| {
                        if folders && ui.button("Clear FileSystemModels").clicked() {
                            self.folder.clear_models();
                        }
                        if ui.button("Phase Trace").clicked() {
                            self.open_phase_trace_window = true;
                            ui.close();
                        }
                        ui.checkbox(&mut self.phase_overlay, "Phase Overlay");
                        let load_btn = |ui: &mut egui::Ui,
                                        label: &str,
                                        file_1: &mut FileProcessor,
                                        file_2: &mut FileProcessor,
                                        diff_processor: &mut DiffProcessor,
                                        p1: &str,
                                        p2: &str| {
                            let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
                            let (p1, p2) = (base.join(p1), base.join(p2));
                            let missing = [&p1, &p2].into_iter().find(|p| !p.exists());
                            let button = ui
                                .add_enabled(missing.is_none(), egui::Button::new(label))
                                .on_disabled_hover_text(format!(
                                    "Missing fixture {}",
                                    missing.map(|p| p.display().to_string()).unwrap_or_default()
                                ));
                            if button.clicked() {
                                ui.close();
                                file_1.set_root("".into());
                                file_2.set_root("".into());
                                file_1.set_path(p1.into());
                                file_2.set_path(p2.into());

                                diff_processor.reset_ctx();
                            }
                        };

                        load_btn(
                            ui,
                            "Load $A",
                            file_1,
                            file_2,
                            diff_processor,
                            "../../test/rust_files_diff_1/advanced_rust.rs",
                            "../../test/rust_files_diff_1/advanced_rust_2.rs",
                        );

                        load_btn(
                            ui,
                            "Load $B",
                            file_1,
                            file_2,
                            diff_processor,
                            "../../test/rust_files_diff_1/imgui.1.91.1.h",
                            "../../test/rust_files_diff_1/imgui.h",
                        );

                        load_btn(
                            ui,
                            "Load $C",
                            file_1,
                            file_2,
                            diff_processor,
                            "../../test/test_ignore_whitespace_simple/1.txt",
                            "../../test/test_ignore_whitespace_simple/2.txt",
                        );

                        load_btn(
                            ui,
                            "Load $D",
                            file_1,
                            file_2,
                            diff_processor,
                            "../../test/test_ignore_whitespace_extreme_simple/1.txt",
                            "../../test/test_ignore_whitespace_extreme_simple/2.txt",
                        );

                        for (label, p1, p2) in [
                            (
                                "Open Folders test_similar_1 vs test_similar_2",
                                "../../test/test_similar_1",
                                "../../test/test_similar_2",
                            ),
                            (
                                "Open Folders test_similar_2 vs test_similar_2_one_deeper",
                                "../../test/test_similar_2",
                                "../../test/test_similar_2_one_deeper",
                            ),
                            (
                                "Open Folders 1 vs test_similar_1/1",
                                "../../test/1",
                                "../../test/test_similar_1/1",
                            ),
                        ] {
                            load_btn(ui, label, file_1, file_2, diff_processor, p1, p2);
                        }

                        ui.separator();

                        load_btn(
                            ui,
                            "Open Text Logs",
                            file_1,
                            file_2,
                            diff_processor,
                            "../../test/test_log/1.txt",
                            "../../test/test_log/2.txt",
                        );

                        load_btn(
                            ui,
                            "Open Hex Binaries",
                            file_1,
                            file_2,
                            diff_processor,
                            "../../test/1/2/2_battery-crates_all_A1-Mini - Copy (4).3mf",
                            "../../test/test_similar_2/1/2/AAA.3mf",
                        );

                        load_btn(
                            ui,
                            "Open PNG Pair",
                            file_1,
                            file_2,
                            diff_processor,
                            "../../test/test_similar_1/different_hash.png",
                            "../../test/test_similar_2/different_hash.png",
                        );

                        load_btn(
                            ui,
                            "Open PNG vs Text",
                            file_1,
                            file_2,
                            diff_processor,
                            "../../test/1/admin_icon.png",
                            "../../test/test_log/1.txt",
                        );
                    });
                    frame_stats::fps_label(ui, &self.frame_stats, &mut self.show_frame_breakdown);
                });
        });
        if self.show_frame_breakdown {
            frame_stats::breakdown_window(
                ui.ctx(),
                &self.frame_stats,
                &mut self.show_frame_breakdown,
            );
        }

        if self.open_shortcuts_window {
            show_settings_window(
                ui.ctx(),
                &mut self.open_shortcuts_window,
                "Keyboard Shortcuts",
                [600.0, 680.0],
                |ui| {
                    let file_before = keybindings.keymap.clone();
                    let folder_before = self.folder.keymap().clone();
                    ui_keybindings(
                        ui,
                        keybindings,
                        &mut self.keymap_editor,
                        &p4_profiles.profiles,
                    );
                    section_heading(ui, "Folder diff");
                    ui.push_id("folder_keymap", |ui| {
                        self.folder_keymap_editor.ui_where(
                            ui,
                            self.folder.keymap_mut(),
                            |action| action.shared().is_none(),
                        );
                    });
                    let file_changed = keybindings.keymap != file_before;
                    let folder_changed = *self.folder.keymap() != folder_before;
                    if file_changed {
                        load_shared(self.folder.keymap_mut());
                    }
                    if folder_changed {
                        load_shared(&mut keybindings.keymap);
                    }
                },
            );
        }
        if !self.open_shortcuts_window {
            self.keymap_editor.cancel_capture();
            self.folder_keymap_editor.cancel_capture();
        }
        if self.open_viewers_window {
            show_settings_window(
                ui.ctx(),
                &mut self.open_viewers_window,
                "File Viewers",
                [480.0, 520.0],
                |ui| {
                    ui_ffmpeg_setting(ui, ffmpeg);
                    let edits = ui_extension_map(ui, &kinds.table);
                    kinds.apply(edits);
                },
            );
        }
        if folders {
            self.style_window.show::<FolderRole>(ui.ctx(), "Folder diff");
            self.folder.show_windows(ui.ctx());
        } else {
            self.style_window.show::<FileRole>(ui.ctx(), "File diff");
        }
        if self.open_help_window {
            show_custom_popup(
                ui.ctx(),
                &mut self.open_help_window,
                "Command line",
                true,
                |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(cli::HELP).monospace()));
                },
            );
        }
        if self.open_phase_trace_window {
            let trace = diff_processor
                .get_minimal_diff_ctx()
                .and_then(|ctx| ctx.phase_trace);
            show_custom_popup(
                ui.ctx(),
                &mut self.open_phase_trace_window,
                "Debug - Phase Trace",
                false,
                |ui| ui_phase_trace(ui, trace.as_deref()),
            );
        }
        if self.open_universal_path_window {
            show_settings_window(
                ui.ctx(),
                &mut self.open_universal_path_window,
                "P4Config",
                [460.0, 460.0],
                |ui| {
                    let before = p4_profiles.clone();
                    ui_p4_profiles(ui, p4_profiles);
                    if *p4_profiles != before {
                        // Not the profiles themselves: they hold passwords.
                        log::info!("P4 profiles changed");
                        update_p4_profiles(p4_profiles.clone());
                    }
                },
            );
        }
    }
}
