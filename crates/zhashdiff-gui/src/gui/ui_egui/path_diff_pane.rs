use std::path::PathBuf;

use eframe::egui::{
    self, PopupCloseBehavior, RichText,
    containers::menu::{MenuButton, MenuConfig},
};
use serde::{Deserialize, Serialize};
use zcommon::{
    presets::{BuiltinPreset, UserPresets},
    ui_egui::{presets::presets_menu, toggle::toggle_button},
};
use zhashdiff::{
    external_diff_tool::DiffToolConfig,
    filter::{PathFilter, PatternList},
};

use crate::ui_egui::{
    folder_keymap::FolderAction,
    fs_tree::{
        CursorRowsCache, DiffState, FileSystemView, HScrollLink, TreeMode, TreeTable,
        TwoFolderTable, draw_ui_folder_tree,
    },
    panes::{PathDiffView, ZAppPane},
    roots::{Side, set_root},
    status_line::ToolMessage,
    tree_compare::TreeCompare,
    tree_cursor::TreeCursor,
    tree_filter::DiffFilter,
    tree_find::TreeFindView,
    tree_sort::TreeSort,
    tree_tolerance::{SizeDelta, Tolerance},
};

pub struct PathDiffPaneCtx<'a, 'b> {
    pub path_diff_view: &'a mut PathDiffView<'b>,

    // User Interaction State
    pub diff_tool_config: &'a DiffToolConfig,
    pub tool_message: &'a mut Option<ToolMessage>,
    pub path_filter: &'a mut PathFilter,
    pub filter_presets: &'a mut FilterPresets,
    pub tree_sort: &'a mut TreeSort,
    pub diff_filter: &'a mut DiffFilter,
    pub likeness_bar: &'a mut bool,
    pub tolerance: &'a mut Tolerance,
    pub tree_compare: &'a TreeCompare,
    pub h_scroll_link: &'a mut HScrollLink,
    pub find: &'a mut TreeFindView<'b>,
    /// A widget had keyboard focus when the frame started, so the tree's keys are not
    /// for the cursor.
    pub keyboard_taken: bool,
    pub actions: &'a [FolderAction],
}

#[derive(Serialize, Deserialize)]
pub struct PathDiffPane {
    pub title: Option<String>,

    #[serde(skip)]
    pub open_dir_window_1: bool,

    #[serde(skip)]
    pub open_dir_window_2: bool,

    #[serde(skip)]
    cursor: TreeCursor,
    /// Roots of the views the cursor belongs to.
    #[serde(skip)]
    cursor_roots: (Option<PathBuf>, Option<PathBuf>),
    #[serde(skip)]
    table: TreeTable,
    #[serde(skip)]
    rows_cache: CursorRowsCache,
}

impl ZAppPane for PathDiffPane {
    fn title(&self) -> String {
        self.title.clone().unwrap_or(format!("Pane"))
    }
}

impl PathDiffPane {
    pub fn new(title: Option<String>) -> Self {
        Self {
            title,
            open_dir_window_1: false,
            open_dir_window_2: false,
            cursor: TreeCursor::default(),
            cursor_roots: (None, None),
            table: TreeTable::default(),
            rows_cache: CursorRowsCache::default(),
        }
    }

    /// Opening another folder on either side clears the cursor. A rescan of the same roots
    /// keeps it.
    fn forget_cursor_of_other_roots(
        &mut self,
        file_system_1_view: Option<&FileSystemView>,
        file_system_2_view: Option<&FileSystemView>,
    ) {
        let root = |view: Option<&FileSystemView>| {
            view.map(|view| view.file_system.get_root().as_path().as_ref().to_path_buf())
        };
        let roots = (root(file_system_1_view), root(file_system_2_view));
        if roots != self.cursor_roots {
            self.cursor = TreeCursor::default();
            self.cursor_roots = roots;
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, ctx: &mut PathDiffPaneCtx) -> egui_tiles::UiResponse {
        self.forget_cursor_of_other_roots(
            ctx.path_diff_view.file_system_1_view.as_ref(),
            ctx.path_diff_view.file_system_2_view.as_ref(),
        );

        ui.horizontal(|ui| {
            let views = [
                ctx.path_diff_view.file_system_1_view.as_mut(),
                ctx.path_diff_view.file_system_2_view.as_mut(),
            ];

            let is_anything_collapsed = views.iter().flatten().any(|v| {
                v.file_system
                    .get_root()
                    .children()
                    .map_or(false, |children| v.is_anything_collapsed_slice(children))
            });

            let button_text = if is_anything_collapsed {
                "Expand All"
            } else {
                "Collapse All"
            };

            // Folding doesn't show in flat mode.
            let foldable = !ctx.diff_filter.is_flat(*ctx.tree_sort);
            if ui
                .add_enabled(foldable, egui::Button::new(button_text))
                .clicked()
            {
                for v in views.into_iter().flatten() {
                    if let Some(children) = v.file_system.get_root().children() {
                        v.recursive_collapse_slice(&children.clone(), !is_anything_collapsed);
                    }
                }
            }

            if ui
                .add_enabled(foldable, egui::Button::new("Expand Diffs Only"))
                .clicked()
            {
                let views = [
                    ctx.path_diff_view.file_system_1_view.as_mut(),
                    ctx.path_diff_view.file_system_2_view.as_mut(),
                ];
                for v in views.into_iter().flatten() {
                    if let Some(children) = v.file_system.get_root().children() {
                        v.recursive_collapse_slice(&children.clone(), false);
                    }
                }
                if let Some(rows) = ctx.path_diff_view.visible_rows.as_ref() {
                    for row in rows {
                        if let DiffState::Same(id1, id2) = row.diff_state {
                            if let Some(v1) = ctx.path_diff_view.file_system_1_view.as_mut() {
                                v1.collapsed.insert(id1, true);
                            }
                            if let Some(v2) = ctx.path_diff_view.file_system_2_view.as_mut() {
                                v2.collapsed.insert(id2, true);
                            }
                        }
                    }
                }
            }
        });

        ui.horizontal(|ui| {
            draw_filter_bar(ui, ctx.path_filter, ctx.filter_presets);
        });

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // Like the filter, the app reorders the rows on the next update.
            toggle_button(
                ui,
                &mut ctx.tree_sort.flat,
                RichText::new("F").strong(),
                "Flat: list all entries by depth, then by the sorted column",
            );
            toggle_button(
                ui,
                &mut ctx.diff_filter.diff_only,
                RichText::new("D").strong(),
                "Diff only: hide entries that are the same on both sides",
            );
            ui.add_enabled_ui(ctx.diff_filter.diff_only, |ui| {
                toggle_button(
                    ui,
                    &mut ctx.diff_filter.show_parents,
                    RichText::new("P").strong(),
                    "Show parent folders: off lists only the differing files (needs Diff only)",
                );
            });
            toggle_button(
                ui,
                ctx.likeness_bar,
                RichText::new("L").strong(),
                "Likeness: how alike the two files of each pair are, measured in the background",
            );
            toggle_button(
                ui,
                &mut ctx.tolerance.enabled,
                RichText::new("T").strong(),
                "Tolerance: differing files that pass every enabled criterion count as Similar (≈)",
            );
            MenuButton::new("⏷")
                .config(MenuConfig::new().close_behavior(PopupCloseBehavior::CloseOnClickOutside))
                .ui(ui, |ui| draw_tolerance_settings(ui, ctx.tolerance));
            toggle_button(
                ui,
                &mut ctx.h_scroll_link.0,
                "🔗",
                "Link horizontal scroll: scroll both folder sides sideways together",
            );
        });

        ui.separator();

        // Not inside a ScrollArea: the table scrolls itself, and at fractional DPI an outer
        // area overflows by a subpixel and lays its own bar over the table's.
        draw_ui_folder_tree(
            ui,
            &mut ctx.path_diff_view.file_system_1_view,
            &mut ctx.path_diff_view.file_system_2_view,
            ctx.path_diff_view.visible_rows.as_deref(),
            ctx.path_diff_view.rows_id,
            ctx.path_diff_view.file_system_1_root_path,
            ctx.path_diff_view.file_system_2_root_path,
            &mut self.open_dir_window_1,
            &mut self.open_dir_window_2,
            &ctx.diff_tool_config,
            ctx.tool_message,
            &mut self.cursor,
            &mut self.rows_cache,
            ctx.tree_sort,
            ctx.keyboard_taken,
            ctx.actions,
            *ctx.diff_filter,
            TreeMode::TwoFolder(TwoFolderTable {
                compare: ctx.tree_compare,
                likeness_bar: *ctx.likeness_bar,
                tolerance: ctx.tolerance,
                table: &mut self.table,
                linked: ctx.h_scroll_link.0,
                find: ctx.find,
            }),
        );

        // Handle folder dialogs
        if self.open_dir_window_1 {
            self.open_dir_window_1 = false;
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                let _ = set_root(
                    ctx.path_diff_view.file_system_1_root_path,
                    Side::Folder1,
                    &path,
                );
            }
        }
        if self.open_dir_window_2 {
            self.open_dir_window_2 = false;
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                let _ = set_root(
                    ctx.path_diff_view.file_system_2_root_path,
                    Side::Folder2,
                    &path,
                );
            }
        }

        egui_tiles::UiResponse::None
    }
}

pub const BLACKLIST_PRESETS: &[BuiltinPreset] = &[
    BuiltinPreset {
        name: "No build dirs",
        text: "bin/ obj/ target/ Intermediate/ Binaries/ DerivedDataCache/ Saved/ .vs/",
    },
    BuiltinPreset {
        name: "No VCS",
        text: ".git/ .svn/ .hg/",
    },
    BuiltinPreset {
        name: "No binaries",
        text: "*.exe *.dll *.pdb *.lib *.obj *.o",
    },
];

pub const WHITELIST_PRESETS: &[BuiltinPreset] = &[
    BuiltinPreset {
        name: "Text only",
        text: "*.txt *.md *.log *.json *.xml *.ini *.cfg *.yaml *.yml *.toml *.csv",
    },
    BuiltinPreset {
        name: "Source",
        text: "*.rs *.c *.h *.cpp *.hpp *.cs *.py *.js *.ts *.hlsl *.usf *.ush",
    },
    BuiltinPreset {
        name: "Images",
        text: "*.png *.tga *.jpg *.jpeg *.bmp *.dds *.exr *.hdr *.psd",
    },
    BuiltinPreset {
        name: "Unreal assets",
        text: "*.uasset *.umap",
    },
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterPresets {
    #[serde(default)]
    pub blacklist: UserPresets,
    #[serde(default)]
    pub whitelist: UserPresets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterField {
    Blacklist,
    Whitelist,
}

impl FilterField {
    fn label(self) -> &'static str {
        match self {
            FilterField::Blacklist => "Blacklist:",
            FilterField::Whitelist => "Whitelist:",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            FilterField::Blacklist => r"*.obj target/ My\ Folder/",
            FilterField::Whitelist => "*.rs *.toml",
        }
    }

    fn patterns(self, filter: &PathFilter) -> &PatternList {
        match self {
            FilterField::Blacklist => &filter.blacklist,
            FilterField::Whitelist => &filter.whitelist,
        }
    }

    pub fn presets(
        self,
        presets: &mut FilterPresets,
    ) -> (&'static [BuiltinPreset], &mut UserPresets) {
        match self {
            FilterField::Blacklist => (BLACKLIST_PRESETS, &mut presets.blacklist),
            FilterField::Whitelist => (WHITELIST_PRESETS, &mut presets.whitelist),
        }
    }

    /// Typed text and a picked preset both land here, so a pick is an ordinary filter change.
    pub fn set_text(self, filter: &mut PathFilter, text: impl Into<String>) {
        let list = match self {
            FilterField::Blacklist => &mut filter.blacklist,
            FilterField::Whitelist => &mut filter.whitelist,
        };
        *list = PatternList::new(text);
    }
}

/// Only edits the filter: the app rebuilds the rows on the next update when it changed, so the
/// table drawn below still has this frame's rows.
fn draw_tolerance_settings(ui: &mut egui::Ui, tolerance: &mut Tolerance) {
    ui.checkbox(&mut tolerance.enabled, "Tolerance");
    ui.add_enabled_ui(tolerance.enabled, |ui| {
        ui.horizontal(|ui| {
            ui.checkbox(&mut tolerance.size, "Sizes differ by at most");
            ui.add_enabled_ui(tolerance.size, |ui| {
                draw_size_delta(ui, &mut tolerance.max_size_delta)
            });
        });
        ui.checkbox(
            &mut tolerance.same_size_and_time,
            "Same size and modification time",
        )
        .on_hover_text("Decides without reading the files");
        ui.horizontal(|ui| {
            ui.checkbox(&mut tolerance.likeness, "Likeness at least")
                .on_hover_text(
                    "Measured in the background; a pair stays different until it is measured",
                );
            ui.add_enabled(
                tolerance.likeness,
                egui::DragValue::new(&mut tolerance.min_likeness)
                    .range(0.0..=100.0)
                    .speed(0.1)
                    .suffix(" %"),
            );
        });
    });
}

fn draw_size_delta(ui: &mut egui::Ui, delta: &mut SizeDelta) {
    match delta {
        SizeDelta::Bytes(max) => ui.add(egui::DragValue::new(max).suffix(" bytes")),
        SizeDelta::Percent(max) => ui.add(
            egui::DragValue::new(max)
                .range(0.0..=100.0)
                .speed(0.1)
                .suffix(" %"),
        ),
    };
    let percent = matches!(delta, SizeDelta::Percent(_));
    if ui.selectable_label(!percent, "bytes").clicked() && percent {
        *delta = SizeDelta::Bytes(1024);
    }
    if ui.selectable_label(percent, "%").clicked() && !percent {
        *delta = SizeDelta::Percent(1.0);
    }
}

fn draw_filter_bar(ui: &mut egui::Ui, filter: &mut PathFilter, presets: &mut FilterPresets) {
    for field in [FilterField::Blacklist, FilterField::Whitelist] {
        ui.label(field.label());
        draw_pattern_field(ui, filter, presets, field);
    }
    let has_text = *filter != PathFilter::default();
    if ui
        .add_enabled(has_text, egui::Button::new("Clear"))
        .clicked()
    {
        *filter = PathFilter::default();
        ui.ctx().request_repaint();
    }
    if filter.is_active() {
        ui.label(RichText::new("Filter active").color(ui.visuals().warn_fg_color));
    }
}

fn draw_pattern_field(
    ui: &mut egui::Ui,
    filter: &mut PathFilter,
    presets: &mut FilterPresets,
    field: FilterField,
) {
    let mut text = field.patterns(filter).text().to_owned();
    let edit = egui::TextEdit::singleline(&mut text)
        .hint_text(field.hint())
        .desired_width(300.0);
    let mut new_text = ui.add(edit).changed().then(|| text.clone());
    let (builtins, user) = field.presets(presets);
    if let Some(picked) = presets_menu(ui, ("filter_presets", field), builtins, user, &text) {
        new_text = Some(picked);
    }
    if let Some(text) = new_text {
        field.set_text(filter, text);
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::{BLACKLIST_PRESETS, FilterField, FilterPresets, PathDiffPane, WHITELIST_PRESETS};
    use crate::ui_egui::{fs_tree::FileSystemView, tree_cursor::TreeCursor};
    use std::{path::Path, sync::Arc};
    use tempfile::tempdir;
    use zhashdiff::{filter::PathFilter, fs::FileSystemModel};

    const DIR: bool = true;
    const FILE: bool = false;

    fn load_view(root: &Path) -> FileSystemView {
        FileSystemView::new(Arc::new(FileSystemModel::new(root).unwrap()))
    }

    fn preset_names(field: FilterField, presets: &mut FilterPresets) -> Vec<String> {
        let (builtins, user) = field.presets(presets);
        user.list(builtins).map(|e| e.name.to_string()).collect()
    }

    #[test]
    fn every_filter_preset_matches_one_name_per_pattern_and_nothing_else() {
        use FilterField::{Blacklist, Whitelist};
        type Names = &'static [(&'static str, bool)];
        let samples: &[(FilterField, &str, Names, Names)] = &[
            (
                Blacklist,
                "No build dirs",
                &[
                    ("bin", DIR),
                    ("obj", DIR),
                    ("target", DIR),
                    ("Intermediate", DIR),
                    ("Binaries", DIR),
                    ("DerivedDataCache", DIR),
                    ("Saved", DIR),
                    (".vs", DIR),
                ],
                &[("target", FILE), ("src", DIR), ("Content", DIR)],
            ),
            (
                Blacklist,
                "No VCS",
                &[(".git", DIR), (".svn", DIR), (".hg", DIR)],
                &[(".git", FILE), (".gitignore", FILE), ("git", DIR)],
            ),
            (
                Blacklist,
                "No binaries",
                &[
                    ("a.exe", FILE),
                    ("a.dll", FILE),
                    ("a.pdb", FILE),
                    ("a.lib", FILE),
                    ("a.obj", FILE),
                    ("a.o", FILE),
                ],
                &[("a.rs", FILE), ("a.object", FILE), ("a.o.txt", FILE)],
            ),
            (
                Whitelist,
                "Text only",
                &[
                    ("a.txt", FILE),
                    ("a.md", FILE),
                    ("a.log", FILE),
                    ("a.json", FILE),
                    ("a.xml", FILE),
                    ("a.ini", FILE),
                    ("a.cfg", FILE),
                    ("a.yaml", FILE),
                    ("a.yml", FILE),
                    ("a.toml", FILE),
                    ("a.csv", FILE),
                ],
                &[("a.rs", FILE), ("a.png", FILE), ("txt", FILE)],
            ),
            (
                Whitelist,
                "Source",
                &[
                    ("a.rs", FILE),
                    ("a.c", FILE),
                    ("a.h", FILE),
                    ("a.cpp", FILE),
                    ("a.hpp", FILE),
                    ("a.cs", FILE),
                    ("a.py", FILE),
                    ("a.js", FILE),
                    ("a.ts", FILE),
                    ("a.hlsl", FILE),
                    ("a.usf", FILE),
                    ("a.ush", FILE),
                ],
                &[("a.txt", FILE), ("a.rs.bak", FILE)],
            ),
            (
                Whitelist,
                "Images",
                &[
                    ("a.png", FILE),
                    ("a.tga", FILE),
                    ("a.jpg", FILE),
                    ("a.jpeg", FILE),
                    ("a.bmp", FILE),
                    ("a.dds", FILE),
                    ("a.exr", FILE),
                    ("a.hdr", FILE),
                    ("a.psd", FILE),
                ],
                &[("a.txt", FILE), ("a.gif", FILE)],
            ),
            (
                Whitelist,
                "Unreal assets",
                &[("a.uasset", FILE), ("a.umap", FILE)],
                &[("a.uexp", FILE), ("a.ubulk", FILE)],
            ),
        ];

        let all = [
            (Blacklist, BLACKLIST_PRESETS),
            (Whitelist, WHITELIST_PRESETS),
        ];
        assert_eq!(
            samples.len(),
            BLACKLIST_PRESETS.len() + WHITELIST_PRESETS.len()
        );
        for (field, builtins) in all {
            for preset in builtins {
                let (_, _, matched, unmatched) = samples
                    .iter()
                    .find(|(f, name, ..)| *f == field && *name == preset.name)
                    .unwrap_or_else(|| panic!("no sample for {field:?} {}", preset.name));
                assert_eq!(
                    preset.text.split_whitespace().count(),
                    matched.len(),
                    "{}: one name per pattern",
                    preset.name
                );
                let mut filter = PathFilter::default();
                field.set_text(&mut filter, preset.text);
                let hides = |name: &str, is_dir: bool| !filter.keeps(name, is_dir);
                let blacklist = field == Blacklist;
                for &(name, is_dir) in *matched {
                    assert_eq!(hides(name, is_dir), blacklist, "{}: {name}", preset.name);
                }
                for &(name, is_dir) in *unmatched {
                    assert_eq!(hides(name, is_dir), !blacklist, "{}: {name}", preset.name);
                }
                if !blacklist {
                    assert!(!hides("src", DIR), "{}: folders stay", preset.name);
                }
            }
        }
    }

    #[test]
    fn blacklist_and_whitelist_presets_are_separate_stores() {
        let mut presets = FilterPresets::default();
        let (builtins, user) = FilterField::Blacklist.presets(&mut presets);
        user.save(builtins, "Mine", "*.tmp").unwrap();
        let (builtins, user) = FilterField::Whitelist.presets(&mut presets);
        user.save(builtins, "Shaders", "*.hlsl").unwrap();

        assert_eq!(
            preset_names(FilterField::Blacklist, &mut presets),
            ["No build dirs", "No VCS", "No binaries", "Mine"]
        );
        assert_eq!(
            preset_names(FilterField::Whitelist, &mut presets),
            ["Text only", "Source", "Images", "Unreal assets", "Shaders"]
        );
    }

    #[test]
    fn opening_another_folder_on_either_side_clears_the_cursor() {
        let (a, b, c) = (tempdir().unwrap(), tempdir().unwrap(), tempdir().unwrap());
        let mut pane = PathDiffPane::new(None);
        pane.forget_cursor_of_other_roots(Some(&load_view(a.path())), Some(&load_view(b.path())));

        pane.cursor = TreeCursor::at("x");
        pane.forget_cursor_of_other_roots(Some(&load_view(a.path())), Some(&load_view(c.path())));
        assert_eq!(pane.cursor, TreeCursor::default(), "right side changed");

        pane.cursor = TreeCursor::at("x");
        pane.forget_cursor_of_other_roots(Some(&load_view(b.path())), Some(&load_view(c.path())));
        assert_eq!(pane.cursor, TreeCursor::default(), "left side changed");

        pane.cursor = TreeCursor::at("x");
        pane.forget_cursor_of_other_roots(Some(&load_view(b.path())), None);
        assert_eq!(pane.cursor, TreeCursor::default(), "right side closed");
    }

    #[test]
    fn rescanning_the_same_folders_keeps_the_cursor() {
        let (a, b) = (tempdir().unwrap(), tempdir().unwrap());
        let mut pane = PathDiffPane::new(None);
        pane.forget_cursor_of_other_roots(Some(&load_view(a.path())), Some(&load_view(b.path())));
        pane.cursor = TreeCursor::at("x");

        // New models of the same roots, as a rescan makes.
        pane.forget_cursor_of_other_roots(Some(&load_view(a.path())), Some(&load_view(b.path())));
        assert_eq!(pane.cursor, TreeCursor::at("x"));
    }
}
