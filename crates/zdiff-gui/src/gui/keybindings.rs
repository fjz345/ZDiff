use eframe::egui::{self, Key, Modifiers};
use zcommon::ui_egui::{
    keymap::{Action, Keymap, Shortcut, TypingPolicy},
    keymap_editor::KeymapEditor,
};

use crate::p4::{P4Profile, ui_p4_profile_combo};

type RootAndPath = (String, String);

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct QuickDiffPaths {
    pub target: RootAndPath,
    pub source: Option<RootAndPath>,
    /// A `P4Profile` id; `None` is Auto.
    #[cfg_attr(feature = "serde", serde(default))]
    pub p4_profile: Option<u64>,
}

const DEFAULT_COMMAND_ENABLE: bool = if cfg!(target_os = "macos")
{true}
else
{true};
const DEFAULT_CONTROL_ENABLE: bool = if cfg!(target_os = "macos")
{false}
else
{true};

const CTRL: Modifiers = Modifiers {
    ctrl: DEFAULT_CONTROL_ENABLE,
    command: DEFAULT_COMMAND_ENABLE,
    ..Modifiers::NONE
};
const CTRL_SHIFT: Modifiers = Modifiers {
    shift: true,
    ..CTRL
};
const CTRL_ALT: Modifiers = Modifiers { alt: true, ..CTRL };

const fn shortcut(key: Key, modifiers: Modifiers) -> Shortcut {
    Shortcut { key, modifiers }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileAction {
    OpenFileSource,
    OpenFileTarget,
    RefreshDiff,
    RefreshDiffRowsOnly,
    OpenKeybindings,
    OpenUniversalPath,
    Find,
    Goto,
    NextConflict,
    PrevConflict,
    NextFind,
    PrevFind,
    RevisionGraph,
    TimelapseView,
    UndoRevert,
    Redo,
    /// Target becomes the local file of the source depot file.
    DiffLocalFile,
    DiffShelfBase,
    SwapSides,
    IgnoreWhitespace,
    IgnoreComments,
    IgnoreRegex,
    HighlightRows,
    GhostRows,
    LinkHScroll,
    WordWrap,
    DiffOnly,
    FewerContextRows,
    MoreContextRows,
    SaveMerge,
    NextFrame,
    PrevFrame,
}

impl Action for FileAction {
    fn all() -> &'static [Self] {
        &[
            Self::OpenFileSource,
            Self::OpenFileTarget,
            Self::RefreshDiff,
            Self::RefreshDiffRowsOnly,
            Self::OpenKeybindings,
            Self::OpenUniversalPath,
            Self::Find,
            Self::Goto,
            Self::NextConflict,
            Self::PrevConflict,
            Self::NextFind,
            Self::PrevFind,
            Self::RevisionGraph,
            Self::TimelapseView,
            Self::UndoRevert,
            Self::Redo,
            Self::DiffLocalFile,
            Self::DiffShelfBase,
            Self::SwapSides,
            Self::IgnoreWhitespace,
            Self::IgnoreComments,
            Self::IgnoreRegex,
            Self::HighlightRows,
            Self::GhostRows,
            Self::LinkHScroll,
            Self::WordWrap,
            Self::DiffOnly,
            Self::FewerContextRows,
            Self::MoreContextRows,
            Self::SaveMerge,
            Self::NextFrame,
            Self::PrevFrame,
        ]
    }

    fn name(self) -> &'static str {
        match self {
            Self::OpenFileSource => "open_file_source",
            Self::OpenFileTarget => "open_file_target",
            Self::RefreshDiff => "refresh_diff",
            Self::RefreshDiffRowsOnly => "refresh_diff_rows_only",
            Self::OpenKeybindings => "open_options_keybindings",
            Self::OpenUniversalPath => "open_universal_path",
            Self::Find => "find",
            Self::Goto => "goto",
            Self::NextConflict => "next_conflict",
            Self::PrevConflict => "prev_conflict",
            Self::NextFind => "next_find",
            Self::PrevFind => "prev_find",
            Self::RevisionGraph => "revision_graph",
            Self::TimelapseView => "timelapse_view",
            Self::UndoRevert => "undo_revert",
            Self::Redo => "redo_revert",
            Self::DiffLocalFile => "diff_local_file",
            Self::DiffShelfBase => "diff_shelf_base",
            Self::SwapSides => "swap_sides",
            Self::IgnoreWhitespace => "ignore_whitespace",
            Self::IgnoreComments => "ignore_comments",
            Self::IgnoreRegex => "ignore_regex",
            Self::HighlightRows => "highlight_rows",
            Self::GhostRows => "ghost_rows",
            Self::LinkHScroll => "link_h_scroll",
            Self::WordWrap => "word_wrap",
            Self::DiffOnly => "diff_only",
            Self::FewerContextRows => "diff_only_fewer_context_rows",
            Self::MoreContextRows => "diff_only_more_context_rows",
            Self::SaveMerge => "save_merge",
            Self::NextFrame => "next_frame",
            Self::PrevFrame => "prev_frame",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::OpenFileSource => "Open Source File",
            Self::OpenFileTarget => "Open Target File",
            Self::RefreshDiff => "Refresh Diff",
            Self::RefreshDiffRowsOnly => "Refresh Diff Rows Only",
            Self::OpenKeybindings => "Open Keybindings Options",
            Self::OpenUniversalPath => "Open P4Config",
            Self::Find => "Find",
            Self::Goto => "Goto",
            Self::NextConflict => "Next Conflict",
            Self::PrevConflict => "Previous Conflict",
            Self::NextFind => "Next Find Result",
            Self::PrevFind => "Previous Find Result",
            Self::RevisionGraph => "Revision Graph",
            Self::TimelapseView => "Timelapse View",
            Self::UndoRevert => "Undo Revert",
            Self::Redo => "Redo Revert",
            Self::DiffLocalFile => "Diff Source Against Local File",
            Self::DiffShelfBase => "Diff Shelved File Against Base",
            Self::SwapSides => "Swap Source/Target",
            Self::IgnoreWhitespace => "Ignore Whitespace",
            Self::IgnoreComments => "Ignore Comments",
            Self::IgnoreRegex => "Ignore Regex On/Off",
            Self::HighlightRows => "Highlight Rows",
            Self::GhostRows => "Ghost Rows",
            Self::LinkHScroll => "Link Horizontal Scrolling",
            Self::WordWrap => "Word Wrap",
            Self::DiffOnly => "Diff Only",
            Self::FewerContextRows => "Diff Only: Fewer Context Rows",
            Self::MoreContextRows => "Diff Only: More Context Rows",
            Self::SaveMerge => "Save Merge",
            Self::NextFrame => "Video: Next Frame",
            Self::PrevFrame => "Video: Previous Frame",
        }
    }

    fn defaults(self) -> &'static [Shortcut] {
        match self {
            Self::OpenFileSource => &[Shortcut {
                key: Key::F1,
                modifiers: CTRL,
            }],
            Self::OpenFileTarget => &[Shortcut {
                key: Key::F2,
                modifiers: CTRL,
            }],
            Self::RefreshDiff => &[Shortcut {
                key: Key::R,
                modifiers: CTRL,
            }],
            Self::RefreshDiffRowsOnly => &[Shortcut {
                key: Key::R,
                modifiers: CTRL_ALT,
            }],
            Self::OpenKeybindings => &[Shortcut {
                key: Key::O,
                modifiers: CTRL,
            }],
            Self::OpenUniversalPath => &[Shortcut {
                key: Key::U,
                modifiers: CTRL,
            }],
            Self::Find => &[Shortcut {
                key: Key::F,
                modifiers: CTRL,
            }],
            Self::Goto => &[Shortcut {
                key: Key::G,
                modifiers: CTRL,
            }],
            Self::NextConflict => &[
                Shortcut {
                    key: Key::Num2,
                    modifiers: CTRL,
                },
                Shortcut {
                    key: Key::ArrowDown,
                    modifiers: Modifiers::ALT,
                },
            ],
            Self::PrevConflict => &[
                Shortcut {
                    key: Key::Num1,
                    modifiers: CTRL,
                },
                Shortcut {
                    key: Key::ArrowUp,
                    modifiers: Modifiers::ALT,
                },
            ],
            Self::NextFind => &[Shortcut {
                key: Key::Enter,
                modifiers: Modifiers::NONE,
            }],
            Self::PrevFind => &[Shortcut {
                key: Key::Enter,
                modifiers: Modifiers::SHIFT,
            }],
            Self::RevisionGraph => &[Shortcut {
                key: Key::R,
                modifiers: CTRL_SHIFT,
            }],
            Self::TimelapseView => &[Shortcut {
                key: Key::T,
                modifiers: CTRL_SHIFT,
            }],
            Self::UndoRevert => &[Shortcut {
                key: Key::Z,
                modifiers: CTRL,
            }],
            Self::Redo => &[
                Shortcut {
                    key: Key::Y,
                    modifiers: CTRL,
                },
                Shortcut {
                    key: Key::Z,
                    modifiers: CTRL_SHIFT,
                },
            ],
            Self::DiffLocalFile => &[Shortcut {
                key: Key::L,
                modifiers: CTRL,
            }],
            Self::SwapSides => &[Shortcut {
                key: Key::S,
                modifiers: CTRL_SHIFT,
            }],
            Self::IgnoreWhitespace => &[Shortcut {
                key: Key::W,
                modifiers: Modifiers::ALT,
            }],
            Self::IgnoreComments => &[Shortcut {
                key: Key::C,
                modifiers: Modifiers::ALT,
            }],
            Self::IgnoreRegex => &[Shortcut {
                key: Key::X,
                modifiers: Modifiers::ALT,
            }],
            Self::HighlightRows => &[Shortcut {
                key: Key::H,
                modifiers: Modifiers::ALT,
            }],
            Self::GhostRows => &[Shortcut {
                key: Key::G,
                modifiers: Modifiers::ALT,
            }],
            Self::LinkHScroll => &[Shortcut {
                key: Key::L,
                modifiers: Modifiers::ALT,
            }],
            Self::WordWrap => &[Shortcut {
                key: Key::Z,
                modifiers: Modifiers::ALT,
            }],
            Self::DiffOnly => &[Shortcut {
                key: Key::D,
                modifiers: Modifiers::ALT,
            }],
            Self::FewerContextRows => &[Shortcut {
                key: Key::OpenBracket,
                modifiers: Modifiers::ALT,
            }],
            Self::MoreContextRows => &[Shortcut {
                key: Key::CloseBracket,
                modifiers: Modifiers::ALT,
            }],
            Self::DiffShelfBase => &[],
            Self::SaveMerge => &[Shortcut {
                key: Key::S,
                modifiers: CTRL,
            }],
            Self::NextFrame => &[Shortcut {
                key: Key::ArrowRight,
                modifiers: Modifiers::NONE,
            }],
            Self::PrevFrame => &[Shortcut {
                key: Key::ArrowLeft,
                modifiers: Modifiers::NONE,
            }],
        }
    }

    // A focused text field has its own Ctrl+Z/Ctrl+Y; they must not also rewrite a file on disk.
    fn typing_policy(self) -> TypingPolicy {
        match self {
            Self::UndoRevert | Self::Redo => TypingPolicy::Never,
            _ => TypingPolicy::CtrlOrAlt,
        }
    }
}

pub type QuickDiffSlot = (Option<Shortcut>, QuickDiffPaths);

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Keybindings {
    pub keymap: Keymap<FileAction>,
    pub user_quick_diffs: Vec<QuickDiffSlot>,
}

impl Default for Keybindings {
    fn default() -> Self {
        Self {
            keymap: Keymap::default(),
            user_quick_diffs: [Key::F1, Key::F2, Key::F3, Key::F4]
                .into_iter()
                .map(|key| {
                    (
                        Some(shortcut(key, Modifiers::NONE)),
                        QuickDiffPaths::default(),
                    )
                })
                .collect(),
        }
    }
}

pub fn first_shortcut_text(keymap: &Keymap<FileAction>, action: FileAction) -> Option<String> {
    keymap.shortcuts(action).first().map(Shortcut::format)
}

pub fn with_shortcut(keymap: &Keymap<FileAction>, action: FileAction, text: &str) -> String {
    match first_shortcut_text(keymap, action) {
        Some(shortcut) => format!("{text} ({shortcut})"),
        None => text.to_owned(),
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Keybindings {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Saved {
            keymap: Keymap<FileAction>,
            user_quick_diffs: Vec<QuickDiffSlot>,
        }

        use serde::de::Error;
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("keymap").is_some() {
            let saved: Saved = serde_json::from_value(value).map_err(D::Error::custom)?;
            Ok(Self {
                keymap: saved.keymap,
                user_quick_diffs: saved.user_quick_diffs,
            })
        } else {
            let legacy: LegacyKeybindings =
                serde_json::from_value(value).map_err(D::Error::custom)?;
            Ok(legacy.migrate())
        }
    }
}

/// The named-field format saved before the keymap.
#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
struct LegacyKeybindings {
    open_file_source: Option<Shortcut>,
    open_file_target: Option<Shortcut>,
    refresh_diff_rows_only: Option<Shortcut>,
    refresh_diff: Option<Shortcut>,
    open_options_keybindings: Option<Shortcut>,
    open_universal_path: Option<Shortcut>,
    find: Option<Shortcut>,
    goto: Option<Shortcut>,
    next_conflict: Option<Shortcut>,
    prev_conflict: Option<Shortcut>,
    next_find: Option<Shortcut>,
    prev_find: Option<Shortcut>,
    user_quick_diffs: Vec<QuickDiffSlot>,
    revision_graph: Option<Shortcut>,
    timelapse_view: Option<Shortcut>,
    #[serde(default = "legacy_default_undo_revert")]
    undo_revert: Option<Shortcut>,
    #[serde(default = "legacy_default_redo_revert")]
    redo_revert: Option<Shortcut>,
    #[serde(default = "legacy_default_redo_revert_alt")]
    redo_revert_alt: Option<Shortcut>,
    #[serde(default = "legacy_default_diff_local_file")]
    diff_local_file: Option<Shortcut>,
}

#[cfg(feature = "serde")]
fn legacy_default_undo_revert() -> Option<Shortcut> {
    Some(shortcut(Key::Z, CTRL))
}

#[cfg(feature = "serde")]
fn legacy_default_redo_revert() -> Option<Shortcut> {
    Some(shortcut(Key::Y, CTRL))
}

#[cfg(feature = "serde")]
fn legacy_default_redo_revert_alt() -> Option<Shortcut> {
    Some(shortcut(Key::Z, CTRL_SHIFT))
}

#[cfg(feature = "serde")]
fn legacy_default_diff_local_file() -> Option<Shortcut> {
    Some(shortcut(Key::L, CTRL))
}

#[cfg(feature = "serde")]
impl LegacyKeybindings {
    fn migrate(self) -> Keybindings {
        let Self {
            open_file_source,
            open_file_target,
            refresh_diff_rows_only,
            refresh_diff,
            open_options_keybindings,
            open_universal_path,
            find,
            goto,
            next_conflict,
            prev_conflict,
            next_find,
            prev_find,
            user_quick_diffs,
            revision_graph,
            timelapse_view,
            undo_revert,
            redo_revert,
            redo_revert_alt,
            diff_local_file,
        } = self;

        let old_prev_conflict_default = shortcut(Key::Num1, CTRL_SHIFT);
        let prev_conflict = prev_conflict.map(|s| {
            if s == old_prev_conflict_default {
                shortcut(Key::Num1, CTRL)
            } else {
                s
            }
        });
        // The Alt arrows used to be hardcoded next to these bindings.
        let with_alt_arrow = |binding: Option<Shortcut>, arrow: Key| {
            let mut shortcuts: Vec<_> = binding.into_iter().collect();
            let alt_arrow = shortcut(arrow, Modifiers::ALT);
            if !shortcuts.contains(&alt_arrow) {
                shortcuts.push(alt_arrow);
            }
            shortcuts
        };
        let mut redo: Vec<_> = redo_revert.into_iter().chain(redo_revert_alt).collect();
        redo.dedup();

        let mut keymap = Keymap::default();
        for (action, shortcuts) in [
            (FileAction::OpenFileSource, Vec::from_iter(open_file_source)),
            (FileAction::OpenFileTarget, Vec::from_iter(open_file_target)),
            (FileAction::RefreshDiff, Vec::from_iter(refresh_diff)),
            (
                FileAction::RefreshDiffRowsOnly,
                Vec::from_iter(refresh_diff_rows_only),
            ),
            (
                FileAction::OpenKeybindings,
                Vec::from_iter(open_options_keybindings),
            ),
            (
                FileAction::OpenUniversalPath,
                Vec::from_iter(open_universal_path),
            ),
            (FileAction::Find, Vec::from_iter(find)),
            (FileAction::Goto, Vec::from_iter(goto)),
            (
                FileAction::NextConflict,
                with_alt_arrow(next_conflict, Key::ArrowDown),
            ),
            (
                FileAction::PrevConflict,
                with_alt_arrow(prev_conflict, Key::ArrowUp),
            ),
            (FileAction::NextFind, Vec::from_iter(next_find)),
            (FileAction::PrevFind, Vec::from_iter(prev_find)),
            (FileAction::RevisionGraph, Vec::from_iter(revision_graph)),
            (FileAction::TimelapseView, Vec::from_iter(timelapse_view)),
            (FileAction::UndoRevert, Vec::from_iter(undo_revert)),
            (FileAction::Redo, redo),
            (FileAction::DiffLocalFile, Vec::from_iter(diff_local_file)),
        ] {
            *keymap.shortcuts_mut(action) = shortcuts;
        }
        Keybindings {
            keymap,
            user_quick_diffs,
        }
    }
}

pub fn ui_keybindings(
    ui: &mut egui::Ui,
    keybindings: &mut Keybindings,
    editor: &mut KeymapEditor<FileAction>,
    p4_profiles: &[P4Profile],
) {
    if ui.button("Add Quick Diff").clicked() {
        keybindings
            .user_quick_diffs
            .push((None, QuickDiffPaths::default()));
    }
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        editor.ui(ui, &mut keybindings.keymap, |ui| {
            let ui_shortcut_row =
                |ui: &mut egui::Ui, label: &str, shortcut: &mut Option<Shortcut>| {
                    ui.label(label);

                    let id = ui.next_auto_id();
                    let is_listening = ui.memory(|mem| mem.has_focus(id));

                    let button_text = if is_listening {
                        "Press any key...".to_string()
                    } else {
                        shortcut
                            .as_ref()
                            .map_or_else(|| "None".to_string(), Shortcut::format)
                    };

                    if ui.add(egui::Button::new(button_text)).clicked() {
                        ui.memory_mut(|mem| mem.request_focus(id));
                    }

                    if is_listening {
                        let mut next_shortcut = None;
                        let mut should_surrender = false;

                        ui.input(|i| {
                            if i.key_pressed(egui::Key::Escape) {
                                should_surrender = true;
                            } else if i.pointer.secondary_clicked() {
                                next_shortcut = None;
                                should_surrender = true;
                            } else {
                                for event in &i.events {
                                    if let egui::Event::Key {
                                        key,
                                        pressed: true,
                                        modifiers,
                                        ..
                                    } = event
                                    {
                                        next_shortcut = Some(Shortcut {
                                            key: *key,
                                            modifiers: *modifiers,
                                        });
                                        should_surrender = true;
                                        break;
                                    }
                                }
                            }
                        });

                        if should_surrender {
                            log::info!("Setting shortcut for {}:\n{:?}\n -> \n{:?}", label, shortcut, next_shortcut);
                            *shortcut = next_shortcut;
                            ui.memory_mut(|mem| mem.surrender_focus(id));
                        }
                    }
                    ui.end_row();
                };

            let ui_quick_diff_path_row =
                |ui: &mut egui::Ui, label: &str, root: &mut String, path: &mut String| {
                    ui.label(label);

                    ui.horizontal(|ui| {
                        let widget = egui::TextEdit::singleline(root).desired_width(150.0);
                        ui.add(widget);
                        ui.label("/");
                        let widget = egui::TextEdit::singleline(path).desired_width(f32::INFINITY);
                        ui.add(widget);
                    });

                    ui.end_row();
                };

            ui.separator();
            ui.end_row();

            for (i, (user_qd, quick_diff_path)) in
                keybindings.user_quick_diffs.iter_mut().enumerate()
            {
                ui_shortcut_row(ui, &format!("User Quick Diff {}", i + 1), user_qd);
                ui_quick_diff_path_row(
                    ui,
                    "Target Path",
                    &mut quick_diff_path.target.0,
                    &mut quick_diff_path.target.1,
                );

                ui.label("P4 Profile");
                ui_p4_profile_combo(
                    ui,
                    ("quick_diff_p4_profile", i),
                    p4_profiles,
                    "Auto",
                    &mut quick_diff_path.p4_profile,
                );
                ui.end_row();

                let mut action_remove = false;
                let mut action_add = false;

                match &mut quick_diff_path.source {
                    Some((root, path)) => {
                        if ui.button("Remove").clicked() {
                            action_remove = true;
                        }
                        ui.horizontal(|ui| {
                            let widget = egui::TextEdit::singleline(root).desired_width(150.0);
                            ui.add(widget);
                            ui.label("/");
                            let widget =
                                egui::TextEdit::singleline(path).desired_width(f32::INFINITY);
                            ui.add(widget);
                        });
                    }
                    None => {
                        ui.label("Source Path");
                        if ui.button("➕ Add Source").clicked() {
                            action_add = true;
                        }
                    }
                }
                ui.end_row();

                if action_remove {
                    quick_diff_path.source = None;
                } else if action_add {
                    quick_diff_path.source = Some((String::new(), String::new()));
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn press(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn file_action_names_are_unique() {
        let names: HashSet<_> = FileAction::all().iter().map(|a| a.name()).collect();
        assert_eq!(names.len(), FileAction::all().len());
    }

    #[test]
    fn first_shortcut_text_follows_the_keymap() {
        let mut keymap = Keymap::<FileAction>::default();
        assert_eq!(
            first_shortcut_text(&keymap, FileAction::IgnoreWhitespace).as_deref(),
            Some("Alt+W")
        );
        keymap.shortcuts_mut(FileAction::IgnoreWhitespace).insert(
            0,
            Shortcut {
                key: Key::F5,
                modifiers: Modifiers::CTRL,
            },
        );
        assert_eq!(
            first_shortcut_text(&keymap, FileAction::IgnoreWhitespace).as_deref(),
            Some("Ctrl+F5")
        );
        keymap.shortcuts_mut(FileAction::IgnoreWhitespace).clear();
        assert_eq!(
            first_shortcut_text(&keymap, FileAction::IgnoreWhitespace),
            None
        );
    }

    #[test]
    fn with_shortcut_appends_the_first_shortcut_only_when_bound() {
        let mut keymap = Keymap::<FileAction>::default();
        assert_eq!(
            with_shortcut(&keymap, FileAction::WordWrap, "Word Wrap"),
            "Word Wrap (Alt+Z)"
        );
        keymap.shortcuts_mut(FileAction::WordWrap).clear();
        assert_eq!(
            with_shortcut(&keymap, FileAction::WordWrap, "Word Wrap"),
            "Word Wrap"
        );
    }

    #[test]
    fn file_action_defaults_are_clash_free() {
        assert_eq!(Keymap::<FileAction>::default().clashes(), []);
    }

    #[test]
    fn undo_and_redo_never_fire_while_typing() {
        let keymap = Keymap::<FileAction>::default();
        let events = [
            press(Key::Z, CTRL),
            press(Key::Y, CTRL),
            press(Key::Z, CTRL_SHIFT),
        ];
        assert_eq!(keymap.dispatch(&events, true), []);
        assert_eq!(
            keymap.dispatch(&events, false),
            [FileAction::UndoRevert, FileAction::Redo, FileAction::Redo]
        );
    }

    #[test]
    fn conflict_and_find_steps_dispatch_from_the_default_keys() {
        let keymap = Keymap::<FileAction>::default();
        let events = [
            press(Key::Num2, CTRL),
            press(Key::ArrowDown, Modifiers::ALT),
            press(Key::Num1, CTRL),
            press(Key::ArrowUp, Modifiers::ALT),
            press(Key::Enter, Modifiers::NONE),
            press(Key::Enter, Modifiers::SHIFT),
        ];
        assert_eq!(
            keymap.dispatch(&events, false),
            [
                FileAction::NextConflict,
                FileAction::NextConflict,
                FileAction::PrevConflict,
                FileAction::PrevConflict,
                FileAction::NextFind,
                FileAction::PrevFind,
            ]
        );
    }

    #[test]
    fn ctrl_s_saves_the_merge_and_ctrl_shift_s_still_swaps() {
        let keymap = Keymap::<FileAction>::default();
        assert_eq!(
            keymap.dispatch(&[press(Key::S, CTRL)], false),
            [FileAction::SaveMerge]
        );
        assert_eq!(
            keymap.dispatch(&[press(Key::S, CTRL)], true),
            [FileAction::SaveMerge]
        );
        assert_eq!(
            keymap.dispatch(&[press(Key::S, CTRL_SHIFT)], false),
            [FileAction::SwapSides]
        );
    }

    #[test]
    fn frame_steps_dispatch_from_the_arrow_keys_but_not_while_typing() {
        let keymap = Keymap::<FileAction>::default();
        let events = [
            press(Key::ArrowRight, Modifiers::NONE),
            press(Key::ArrowLeft, Modifiers::NONE),
        ];
        assert_eq!(
            keymap.dispatch(&events, false),
            [FileAction::NextFrame, FileAction::PrevFrame]
        );
        assert_eq!(keymap.dispatch(&events, true), []);
    }

    #[test]
    fn swap_and_toolbar_toggles_dispatch_from_the_default_keys() {
        let keymap = Keymap::<FileAction>::default();
        let expected = [
            (press(Key::S, CTRL_SHIFT), FileAction::SwapSides),
            (press(Key::W, Modifiers::ALT), FileAction::IgnoreWhitespace),
            (press(Key::C, Modifiers::ALT), FileAction::IgnoreComments),
            (press(Key::X, Modifiers::ALT), FileAction::IgnoreRegex),
            (press(Key::H, Modifiers::ALT), FileAction::HighlightRows),
            (press(Key::G, Modifiers::ALT), FileAction::GhostRows),
            (press(Key::L, Modifiers::ALT), FileAction::LinkHScroll),
            (press(Key::Z, Modifiers::ALT), FileAction::WordWrap),
            (press(Key::D, Modifiers::ALT), FileAction::DiffOnly),
            (
                press(Key::OpenBracket, Modifiers::ALT),
                FileAction::FewerContextRows,
            ),
            (
                press(Key::CloseBracket, Modifiers::ALT),
                FileAction::MoreContextRows,
            ),
        ];
        for (event, action) in expected {
            assert_eq!(
                keymap.dispatch(std::slice::from_ref(&event), false),
                [action]
            );
            assert_eq!(keymap.dispatch(&[event], true), [action], "while typing");
        }
    }

    #[cfg(feature = "serde")]
    mod migration {
        use super::*;
        use serde_json::{Value, json};

        const LEGACY_FIELDS: [&str; 18] = [
            "open_file_source",
            "open_file_target",
            "refresh_diff_rows_only",
            "refresh_diff",
            "open_options_keybindings",
            "open_universal_path",
            "find",
            "goto",
            "next_conflict",
            "prev_conflict",
            "next_find",
            "prev_find",
            "revision_graph",
            "timelapse_view",
            "undo_revert",
            "redo_revert",
            "redo_revert_alt",
            "diff_local_file",
        ];

        fn f_key(i: usize) -> Shortcut {
            let key = Key::from_name(&format!("F{}", i + 1)).unwrap();
            shortcut(key, Modifiers::SHIFT)
        }

        fn alt(key: Key) -> Shortcut {
            shortcut(key, Modifiers::ALT)
        }

        fn legacy_save(fields: &[(&str, Option<Shortcut>)]) -> Value {
            let mut save = json!({
                "user_quick_diffs": Keybindings::default().user_quick_diffs,
            });
            for (field, binding) in fields {
                save[*field] = serde_json::to_value(binding).unwrap();
            }
            save
        }

        fn load(save: Value) -> Keymap<FileAction> {
            serde_json::from_value::<Keybindings>(save).unwrap().keymap
        }

        #[test]
        fn an_old_save_converts_field_by_field() {
            let fields: Vec<_> = LEGACY_FIELDS
                .iter()
                .enumerate()
                .map(|(i, field)| (*field, Some(f_key(i))))
                .collect();
            let mut save = legacy_save(&fields);
            save["goto"] = Value::Null;
            let keymap = load(save);

            let expected = [
                (FileAction::OpenFileSource, vec![f_key(0)]),
                (FileAction::OpenFileTarget, vec![f_key(1)]),
                (FileAction::RefreshDiffRowsOnly, vec![f_key(2)]),
                (FileAction::RefreshDiff, vec![f_key(3)]),
                (FileAction::OpenKeybindings, vec![f_key(4)]),
                (FileAction::OpenUniversalPath, vec![f_key(5)]),
                (FileAction::Find, vec![f_key(6)]),
                (FileAction::Goto, vec![]),
                (
                    FileAction::NextConflict,
                    vec![f_key(8), alt(Key::ArrowDown)],
                ),
                (FileAction::PrevConflict, vec![f_key(9), alt(Key::ArrowUp)]),
                (FileAction::NextFind, vec![f_key(10)]),
                (FileAction::PrevFind, vec![f_key(11)]),
                (FileAction::RevisionGraph, vec![f_key(12)]),
                (FileAction::TimelapseView, vec![f_key(13)]),
                (FileAction::UndoRevert, vec![f_key(14)]),
                (FileAction::Redo, vec![f_key(15), f_key(16)]),
                (FileAction::DiffLocalFile, vec![f_key(17)]),
            ];
            let new_actions = [
                FileAction::DiffShelfBase,
                FileAction::SwapSides,
                FileAction::IgnoreWhitespace,
                FileAction::IgnoreComments,
                FileAction::IgnoreRegex,
                FileAction::HighlightRows,
                FileAction::GhostRows,
                FileAction::LinkHScroll,
                FileAction::WordWrap,
                FileAction::DiffOnly,
                FileAction::FewerContextRows,
                FileAction::MoreContextRows,
                FileAction::SaveMerge,
                FileAction::NextFrame,
                FileAction::PrevFrame,
            ];
            let expected: Vec<_> = expected
                .into_iter()
                .chain(new_actions.map(|a| (a, a.defaults().to_vec())))
                .collect();
            assert_eq!(expected.len(), FileAction::all().len());
            for (action, shortcuts) in expected {
                assert_eq!(keymap.shortcuts(action), shortcuts, "{action:?}");
            }
        }

        #[test]
        fn old_redo_fields_merge_into_redo() {
            let redo = shortcut(Key::Y, CTRL);
            let redo_alt = shortcut(Key::Z, CTRL_SHIFT);
            let both = legacy_save(&[
                ("redo_revert", Some(redo)),
                ("redo_revert_alt", Some(redo_alt)),
            ]);
            assert_eq!(load(both).shortcuts(FileAction::Redo), [redo, redo_alt]);

            let alt_only =
                legacy_save(&[("redo_revert", None), ("redo_revert_alt", Some(redo_alt))]);
            assert_eq!(load(alt_only).shortcuts(FileAction::Redo), [redo_alt]);

            let same = legacy_save(&[("redo_revert", Some(redo)), ("redo_revert_alt", Some(redo))]);
            assert_eq!(load(same).shortcuts(FileAction::Redo), [redo]);
        }

        #[test]
        fn old_default_previous_conflict_becomes_ctrl_1() {
            let old_default = Shortcut {
                key: Key::Num1,
                modifiers: Modifiers {
                    ctrl: DEFAULT_CONTROL_ENABLE,
                    command: true,
                    shift: true,
                    ..Default::default()
                },
            };
            let keymap = load(legacy_save(&[("prev_conflict", Some(old_default))]));
            assert_eq!(
                keymap.shortcuts(FileAction::PrevConflict),
                [shortcut(Key::Num1, CTRL), alt(Key::ArrowUp)]
            );
        }

        #[test]
        fn a_customized_previous_conflict_is_kept() {
            let custom = shortcut(Key::Num1, Modifiers::ALT);
            let keymap = load(legacy_save(&[("prev_conflict", Some(custom))]));
            assert_eq!(
                keymap.shortcuts(FileAction::PrevConflict),
                [custom, alt(Key::ArrowUp)]
            );
        }

        #[test]
        fn alt_arrows_are_added_to_the_conflict_actions_once() {
            let keymap = load(legacy_save(&[
                ("next_conflict", Some(alt(Key::ArrowDown))),
                ("prev_conflict", None),
            ]));
            assert_eq!(
                keymap.shortcuts(FileAction::NextConflict),
                [alt(Key::ArrowDown)]
            );
            assert_eq!(
                keymap.shortcuts(FileAction::PrevConflict),
                [alt(Key::ArrowUp)]
            );
        }

        #[test]
        fn an_old_save_keeps_its_quick_diffs() {
            let mut slots = Keybindings::default().user_quick_diffs;
            slots[2].0 = None;
            slots[2].1.p4_profile = Some(7);
            let mut save = legacy_save(&[]);
            save["user_quick_diffs"] = serde_json::to_value(&slots).unwrap();
            let restored: Keybindings = serde_json::from_value(save).unwrap();
            assert_eq!(restored.user_quick_diffs, slots);
        }
    }
}
