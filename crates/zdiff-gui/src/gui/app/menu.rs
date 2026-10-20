use super::{ZApp, launch::swap_sides, quickdiff::run_diff_against_shelf_base};
use eframe::egui::{self, containers::menu::MenuConfig};
use std::env;
use zcommon_ui::common::show_custom_popup;
use zcommon_ui::h_scroll::WheelModifier;
use zcommon_ui::keymap::menu_item;
use zdiff::{
    myers::{DiffPhases, MyersDiffAlgorithm},
    universal_path::UniversalPath,
};

use crate::{
    cli::{self},
    diff_ctx::DiffProcessor,
    diff_phases::diff_phases_menu,
    file::FileProcessor,
    keybindings::{FileAction, Keybindings, ui_keybindings},
    language_icon::{emoji_renders, language_icon, show_language_icon},
    p4::{P4Profiles, ui_p4_profiles, update_p4_profiles},
    phase_trace_window::ui_phase_trace,
    ui_egui::file_role::FileRole,
    viewer::{FfmpegSetting, SharedKinds, ui_extension_map, ui_ffmpeg_setting},
};

pub(super) struct MenuCtx<'a> {
    pub file_1: &'a mut FileProcessor,
    pub file_2: &'a mut FileProcessor,
    pub diff_processor: &'a mut DiffProcessor,
    pub find_open: &'a mut bool,
    pub goto_open: &'a mut bool,
    pub scroll_left: &'a mut f32,
    pub scroll_right: &'a mut f32,
    pub keybindings: &'a mut Keybindings,
    pub p4_profiles: &'a mut P4Profiles,
    pub kinds: &'a mut SharedKinds,
    pub ffmpeg: &'a mut FfmpegSetting,
    pub myers_diff_algorithm: &'a mut MyersDiffAlgorithm,
    pub diff_phases: &'a mut DiffPhases,
    pub code_language_override: &'a mut Option<String>,
    pub code_language_custom: &'a mut String,
    pub detected_languages: [Option<&'a str>; 2],
    pub h_scroll_modifier: &'a mut WheelModifier,
}

impl ZApp {
    pub(super) fn show_menu(&mut self, ui: &mut egui::Ui, menu_ctx: MenuCtx<'_>) {
        let MenuCtx {
            file_1,
            file_2,
            diff_processor,
            find_open,
            goto_open,
            scroll_left,
            scroll_right,
            keybindings,
            p4_profiles,
            kinds,
            ffmpeg,
            myers_diff_algorithm,
            diff_phases,
            code_language_override,
            code_language_custom,
            detected_languages,
            h_scroll_modifier,
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
                        if menu_item(ui, "Open Source", keymap, FileAction::OpenFileSource)
                            .clicked()
                        {
                            Self::open_file_picker(file_1.get_tx(), ui.ctx().clone());
                        }
                        if menu_item(ui, "Open Target", keymap, FileAction::OpenFileTarget)
                            .clicked()
                        {
                            Self::open_file_picker(file_2.get_tx(), ui.ctx().clone());
                        }
                        if ui.button("Open Merge...").clicked() {
                            self.open_merge_form.open = true;
                            ui.close();
                        }
                        if menu_item(ui, "Swap Source/Target", keymap, FileAction::SwapSides)
                            .clicked()
                        {
                            swap_sides(file_1, file_2, scroll_left, scroll_right, diff_processor);
                        }
                        if menu_item(
                            ui,
                            "Diff Shelved File Against Base",
                            keymap,
                            FileAction::DiffShelfBase,
                        )
                        .clicked()
                        {
                            run_diff_against_shelf_base(file_1, file_2);
                        }
                        if menu_item(ui, "Find", keymap, FileAction::Find).clicked() {
                            *find_open = true;
                        }
                        if menu_item(ui, "Goto", keymap, FileAction::Goto).clicked() {
                            *goto_open = true;
                        }
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
                        ui.menu_button("Code Language", |ui| {
                            let ctx = ui.ctx().clone();
                            let icon_for = |code: &str| {
                                language_icon(code, |emoji| emoji_renders(&ctx, emoji))
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
                            for (code, label) in [
                                ("rs", "Rust (.rs)"),
                                ("py", "Python (.py)"),
                                ("cpp", "C++ (.cpp)"),
                                ("js", "JavaScript (.js)"),
                                ("json", "JSON (.json)"),
                                ("xml", "XML (.xml)"),
                                ("md", "Markdown (.md)"),
                            ] {
                                ui.horizontal(|ui| {
                                    show_language_icon(ui, icon_for(code));
                                    if ui
                                        .radio(
                                            code_language_override.as_deref() == Some(code),
                                            label,
                                        )
                                        .clicked()
                                    {
                                        *code_language_override = Some(code.to_string());
                                    }
                                });
                            }
                            ui.separator();
                            ui.horizontal(|ui| {
                                show_language_icon(ui, icon_for(""));
                                if ui
                                    .radio(
                                        code_language_override.as_deref()
                                            == Some(code_language_custom.as_str())
                                            && !code_language_custom.is_empty(),
                                        "Use Custom",
                                    )
                                    .clicked()
                                {
                                    *code_language_override =
                                        Some(code_language_custom.trim().to_string());
                                }
                                let text_edit = egui::TextEdit::singleline(code_language_custom)
                                    .hint_text("Custom (e.g. go, html)");
                                let res = ui.add(text_edit);
                                if res.changed() && !code_language_custom.is_empty() {
                                    *code_language_override =
                                        Some(code_language_custom.trim().to_string());
                                }
                            });
                        });
                        if let Some(code) = code_language_override {
                            *code = code.trim_matches('.').to_string();
                        }
                        ui.menu_button("Horizontal Scroll Wheel", |ui| {
                            ui.radio_value(h_scroll_modifier, WheelModifier::Shift, "Shift+Wheel");
                            ui.radio_value(h_scroll_modifier, WheelModifier::Ctrl, "Ctrl+Wheel");
                        });
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
                        if ui.button("File Viewers").clicked() {
                            self.open_viewers_window = true;
                            ui.close();
                        }
                        if ui.button("Style").clicked() {
                            self.style_window.open(ui.ctx());
                            ui.close();
                        }
                    });

                    ui.menu_button("Debug", |ui| {
                        if ui.button("Clear File Paths").clicked() {
                            file_1.set_path(UniversalPath::default());
                            file_2.set_path(UniversalPath::default());
                            diff_processor.reset_ctx();
                            ui.close();
                        }
                        if menu_item(ui, "Clear Cached Files", keymap, FileAction::RefreshDiff)
                            .clicked()
                        {
                            diff_processor.reset_ctx();
                            Self::refresh_file_contents(file_1, file_2);
                        }
                        if menu_item(
                            ui,
                            "Clear Diff Rows",
                            keymap,
                            FileAction::RefreshDiffRowsOnly,
                        )
                        .clicked()
                        {
                            diff_processor.reset_ctx();
                        }
                        if ui.button("Phase Trace").clicked() {
                            self.open_phase_trace_window = true;
                            ui.close();
                        }
                        ui.checkbox(&mut self.phase_overlay, "Phase Overlay");
                        #[cfg(debug_assertions)]
                        {
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
                                        missing
                                            .map(|p| p.display().to_string())
                                            .unwrap_or_default()
                                    ));
                                if button.clicked() {
                                    ui.close();
                                    file_1.set_root("".into());
                                    file_2.set_root("".into());
                                    file_1.set_path(UniversalPath::from(p1));
                                    file_2.set_path(UniversalPath::from(p2));

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
                        }
                    });
                });
        });

        if self.open_shortcuts_window {
            show_custom_popup(
                ui.ctx(),
                &mut self.open_shortcuts_window,
                "Option - Shortcuts",
                true,
                |ui| {
                    ui_keybindings(
                        ui,
                        keybindings,
                        &mut self.keymap_editor,
                        &p4_profiles.profiles,
                    );
                },
            );
        }
        if !self.open_shortcuts_window {
            self.keymap_editor.cancel_capture();
        }
        if self.open_viewers_window {
            show_custom_popup(
                ui.ctx(),
                &mut self.open_viewers_window,
                "Option - File Viewers",
                true,
                |ui| {
                    ui_ffmpeg_setting(ui, ffmpeg);
                    ui.separator();
                    let edits = ui_extension_map(ui, &kinds.table);
                    kinds.apply(edits);
                },
            );
        }
        self.style_window.show::<FileRole>(ui.ctx(), "File diff");
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
            show_custom_popup(
                ui.ctx(),
                &mut self.open_universal_path_window,
                "Option - P4Config",
                true,
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
