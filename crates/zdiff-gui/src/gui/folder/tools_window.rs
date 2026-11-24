use std::{
    collections::HashMap,
    env, fs, io,
    path::{Path, PathBuf},
};

use eframe::egui::{self, RichText, TextBuffer};
use zcommon::kinds::FileKind;
use zcommon_ui::common::{icon_button, section_heading, show_settings_window};
use zdiff_folder::external_diff_tool::{
    DiffPair, DiffTool, DiffToolConfig, DiffToolMatch, DiffToolRule, ExeNotFound, find_on_path,
    folder_labels, open_diff_tool, presets, preview_command, resolve_exe, wait_in_background,
};

pub fn resolve_tool_exe(exe_path: &Path) -> Result<PathBuf, ExeNotFound> {
    let app_dir = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    resolve_exe(exe_path, app_dir.as_deref(), Path::is_file, find_on_path)
}

fn write_test_files(dir: &Path) -> io::Result<(PathBuf, PathBuf)> {
    fs::create_dir_all(dir)?;
    let (left, right) = (dir.join("left.txt"), dir.join("right.txt"));
    fs::write(
        &left,
        "zdiff-folder diff tool test\nthis line is on the left\nsame\n",
    )?;
    fs::write(
        &right,
        "zdiff-folder diff tool test\nthis line is on the right\nsame\n",
    )?;
    Ok((left, right))
}

/// Fixed files, not a self-deleting temp dir: the tool opens them after the spawn returns.
fn test_tool(tool: &DiffTool) -> Result<String, String> {
    let exe = resolve_tool_exe(tool.exe_to_launch()).map_err(|err| err.to_string())?;
    let (left, right) = write_test_files(&env::temp_dir().join("zdiff-folder-diff-tool-test"))
        .map_err(|err| format!("writing the test files failed: {err}"))?;
    let (left_name, right_name) = folder_labels("test.txt");
    let child = open_diff_tool(
        &exe,
        tool,
        &DiffPair {
            left: &left.to_string_lossy(),
            right: &right.to_string_lossy(),
            left_name: &left_name,
            right_name: &right_name,
        },
    )
    .map_err(|err| format!("{}: {err}", exe.display()))?;
    wait_in_background(child);
    Ok(format!("launched {}", exe.display()))
}

enum ToolAction {
    Test(usize),
    SetDefault(usize),
    Remove(usize),
    Rename,
}

enum RuleAction {
    Up(usize),
    Down(usize),
    Remove(usize),
}

#[derive(Debug, Default)]
pub struct ToolsWindow {
    pub open: bool,
    test_results: HashMap<String, Result<String, String>>,
    renaming: Option<(String, String)>,
    rename_error: Option<String>,
}

impl ToolsWindow {
    pub fn show(&mut self, ctx: &egui::Context, config: &mut DiffToolConfig) {
        let was_open = self.open;
        let mut open = self.open;
        show_settings_window(
            ctx,
            &mut open,
            "External Diff Tools",
            [600.0, 640.0],
            |ui| {
                ui.checkbox(
                    &mut config.mark_undiffable,
                    "Mark file rows the chosen tool can't diff",
                );
                section_heading(ui, "Tools");
                self.tools_ui(ui, config);
                section_heading(ui, "Rules");
                Self::rules_ui(ui, config);
            },
        );
        self.open = open;
        if was_open && !open {
            for tool in &config.tools {
                if let Err(err) = preview_command(tool) {
                    log::warn!("diff tool {:?} has a broken template: {err}", tool.name);
                }
            }
        }
    }

    fn tools_ui(&mut self, ui: &mut egui::Ui, config: &mut DiffToolConfig) {
        let mut action = None;
        for index in 0..config.tools.len() {
            let is_default = config.tools[index].name == config.default_tool;
            let only_tool = config.tools.len() == 1;
            let tool = &mut config.tools[index];
            let preview = preview_command(tool);
            let mut title = RichText::new(if is_default {
                format!("{} (default)", tool.name)
            } else {
                tool.name.clone()
            });
            if preview.is_err() {
                title = title.color(ui.visuals().error_fg_color);
            }
            egui::CollapsingHeader::new(title.strong())
                .id_salt(("diff_tool", index))
                .default_open(is_default)
                .show(ui, |ui| {
                    egui::Grid::new(("diff_tool_fields", index))
                        .num_columns(2)
                        .spacing([16.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Name");
                            ui.horizontal(|ui| match &mut self.renaming {
                                Some((old, new)) if *old == tool.name => {
                                    ui.text_edit_singleline(new);
                                    if ui.button("OK").clicked() {
                                        action = Some(ToolAction::Rename);
                                    }
                                    if ui.button("Cancel").clicked() {
                                        self.renaming = None;
                                        self.rename_error = None;
                                    }
                                }
                                _ => {
                                    ui.label(&tool.name);
                                    if icon_button(ui, "✏", "Rename").clicked() {
                                        self.renaming =
                                            Some((tool.name.clone(), tool.name.clone()));
                                        self.rename_error = None;
                                    }
                                }
                            });
                            ui.end_row();
                            if let (Some((old, _)), Some(err)) =
                                (&self.renaming, &self.rename_error)
                                && *old == tool.name
                            {
                                ui.label("");
                                ui.colored_label(ui.visuals().error_fg_color, err);
                                ui.end_row();
                            }

                            ui.label("Executable");
                            let mut exe_path = tool.exe_path.to_string_lossy();
                            let exe_edit = egui::TextEdit::singleline(&mut exe_path)
                                .desired_width(f32::INFINITY);
                            if ui.add(exe_edit).changed() {
                                tool.exe_path = PathBuf::from(exe_path.as_str());
                            }
                            ui.end_row();

                            ui.label("Arguments");
                            ui.add(
                                egui::TextEdit::singleline(&mut tool.template)
                                    .desired_width(f32::INFINITY),
                            );
                            ui.end_row();

                            ui.label("");
                            ui.weak("{left} {right} {left_name} {right_name}");
                            ui.end_row();

                            ui.label("Preview");
                            match &preview {
                                Ok(command) => {
                                    ui.add(
                                        egui::Label::new(RichText::new(command).monospace()).wrap(),
                                    );
                                }
                                Err(err) => {
                                    ui.colored_label(
                                        ui.visuals().error_fg_color,
                                        format!("⚠ {err}"),
                                    );
                                }
                            }
                            ui.end_row();

                            ui.label("Kinds");
                            egui::Grid::new(("diff_tool_kinds", index)).show(ui, |ui| {
                                for (i, kind) in FileKind::ALL.into_iter().enumerate() {
                                    let mut listed = tool.kinds.contains(&kind);
                                    if ui.checkbox(&mut listed, format!("{kind:?}")).changed() {
                                        if listed {
                                            tool.kinds.push(kind);
                                            tool.kinds.sort();
                                        } else {
                                            tool.kinds.retain(|&k| k != kind);
                                        }
                                    }
                                    if i % 4 == 3 {
                                        ui.end_row();
                                    }
                                }
                            });
                            ui.end_row();
                        });

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("Test").clicked() {
                            action = Some(ToolAction::Test(index));
                        }
                        if ui
                            .add_enabled(!is_default, egui::Button::new("Set as default"))
                            .clicked()
                        {
                            action = Some(ToolAction::SetDefault(index));
                        }
                        if ui
                            .add_enabled(!only_tool, egui::Button::new("Remove"))
                            .clicked()
                        {
                            action = Some(ToolAction::Remove(index));
                        }
                    });
                    match self.test_results.get(&tool.name) {
                        Some(Ok(message)) => {
                            ui.colored_label(egui::Color32::LIGHT_GREEN, message);
                        }
                        Some(Err(err)) => {
                            ui.colored_label(ui.visuals().error_fg_color, err);
                        }
                        None => {}
                    }
                });
        }

        match action {
            Some(ToolAction::Test(index)) => {
                let tool = &config.tools[index];
                let result = test_tool(tool);
                match &result {
                    Ok(message) => log::info!("diff tool test {:?}: {message}", tool.name),
                    Err(err) => log::error!("diff tool test {:?} failed: {err}", tool.name),
                }
                self.test_results.insert(tool.name.clone(), result);
            }
            Some(ToolAction::SetDefault(index)) => {
                config.default_tool = config.tools[index].name.clone();
            }
            Some(ToolAction::Remove(index)) => {
                let name = config.tools[index].name.clone();
                config.remove_tool(&name);
                self.test_results.remove(&name);
            }
            Some(ToolAction::Rename) => {
                if let Some((old, new)) = &self.renaming {
                    match config.rename_tool(old, new) {
                        Ok(()) => {
                            self.test_results.remove(old);
                            self.renaming = None;
                            self.rename_error = None;
                        }
                        Err(err) => self.rename_error = Some(err.to_string()),
                    }
                }
            }
            None => {}
        }

        ui.add_space(4.0);
        ui.menu_button("➕ Add tool", |ui| {
            let local_app_data = env::var_os("LOCALAPPDATA").map(PathBuf::from);
            for preset in presets(local_app_data.as_deref()) {
                if ui.button(preset.name).clicked() {
                    config.add_tool(preset.to_tool(Path::is_file));
                }
            }
            ui.separator();
            if ui.button("Custom").clicked() {
                config.add_tool(DiffTool {
                    name: "Custom".to_string(),
                    exe_path: PathBuf::new(),
                    template: r#""{left}" "{right}""#.to_string(),
                    kinds: Vec::new(),
                });
            }
        });
    }

    fn rules_ui(ui: &mut egui::Ui, config: &mut DiffToolConfig) {
        ui.weak("First match wins, otherwise the default tool.");
        let names: Vec<String> = config.tools.iter().map(|tool| tool.name.clone()).collect();
        let rule_count = config.rules.len();
        let mut action = None;
        egui::Grid::new("diff_tool_rules")
            .num_columns(1)
            .striped(true)
            .show(ui, |ui| {
                for (index, rule) in config.rules.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        let is_kind = matches!(rule.matches, DiffToolMatch::Kind(_));
                        egui::ComboBox::from_id_salt(("diff_tool_rule_type", index))
                            .selected_text(if is_kind { "Kind" } else { "Extension" })
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(is_kind, "Kind").clicked() && !is_kind {
                                    rule.matches = DiffToolMatch::Kind(FileKind::Text);
                                }
                                if ui.selectable_label(!is_kind, "Extension").clicked() && is_kind {
                                    rule.matches = DiffToolMatch::Extension(String::new());
                                }
                            });
                        match &mut rule.matches {
                            DiffToolMatch::Kind(kind) => {
                                egui::ComboBox::from_id_salt(("diff_tool_rule_kind", index))
                                    .selected_text(format!("{kind:?}"))
                                    .show_ui(ui, |ui| {
                                        for option in FileKind::ALL {
                                            ui.selectable_value(
                                                kind,
                                                option,
                                                format!("{option:?}"),
                                            );
                                        }
                                    });
                            }
                            DiffToolMatch::Extension(extension) => {
                                ui.add(egui::TextEdit::singleline(extension).desired_width(80.0));
                            }
                        }
                        ui.label("➡");
                        let mut tool_text = RichText::new(&rule.tool);
                        if !names.contains(&rule.tool) {
                            tool_text = tool_text.color(ui.visuals().error_fg_color);
                        }
                        egui::ComboBox::from_id_salt(("diff_tool_rule_tool", index))
                            .selected_text(tool_text)
                            .show_ui(ui, |ui| {
                                for name in &names {
                                    ui.selectable_value(&mut rule.tool, name.clone(), name);
                                }
                            });
                        let up = ui.add_enabled_ui(index > 0, |ui| icon_button(ui, "⏶", "Move up"));
                        if up.inner.clicked() {
                            action = Some(RuleAction::Up(index));
                        }
                        let down = ui.add_enabled_ui(index + 1 < rule_count, |ui| {
                            icon_button(ui, "⏷", "Move down")
                        });
                        if down.inner.clicked() {
                            action = Some(RuleAction::Down(index));
                        }
                        if icon_button(ui, "✖", "Remove").clicked() {
                            action = Some(RuleAction::Remove(index));
                        }
                    });
                    ui.end_row();
                }
            });
        match action {
            Some(RuleAction::Up(index)) => config.rules.swap(index - 1, index),
            Some(RuleAction::Down(index)) => config.rules.swap(index, index + 1),
            Some(RuleAction::Remove(index)) => {
                config.rules.remove(index);
            }
            None => {}
        }
        if ui.button("➕ Add rule").clicked() {
            config.rules.push(DiffToolRule {
                matches: DiffToolMatch::Kind(FileKind::Text),
                tool: config.default_tool.clone(),
            });
        }
    }
}
