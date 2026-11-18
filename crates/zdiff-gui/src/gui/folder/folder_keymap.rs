use eframe::egui::{Key, Modifiers};
use zcommon_ui::{
    keymap::{Action, Keymap, Shortcut},
    shared_keymap::SharedAction,
};

const CTRL: Modifiers = Modifiers {
    ctrl: !cfg!(target_os = "macos"),
    mac_cmd: cfg!(target_os = "macos"),
    command: true,
    ..Modifiers::NONE
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FolderAction {
    NextDiff,
    PrevDiff,
    Refresh,
    Find,
    KeepThis,
    NextGroup,
    PrevGroup,
    OpenDiffTools,
    ToggleDiffMode,
    Export,
    ToggleMap,
}

impl Action for FolderAction {
    fn all() -> &'static [Self] {
        &[
            Self::NextDiff,
            Self::PrevDiff,
            Self::Refresh,
            Self::Find,
            Self::KeepThis,
            Self::NextGroup,
            Self::PrevGroup,
            Self::OpenDiffTools,
            Self::ToggleDiffMode,
            Self::Export,
            Self::ToggleMap,
        ]
    }

    fn name(self) -> &'static str {
        match self {
            Self::NextDiff => "next_diff",
            Self::PrevDiff => "prev_diff",
            Self::Refresh => "refresh",
            Self::Find => "find",
            Self::KeepThis => "keep_this",
            Self::NextGroup => "next_group",
            Self::PrevGroup => "prev_group",
            Self::OpenDiffTools => "open_diff_tools",
            Self::ToggleDiffMode => "toggle_diff_mode",
            Self::Export => "export",
            Self::ToggleMap => "toggle_map",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::NextDiff => "Next Differing File",
            Self::PrevDiff => "Previous Differing File",
            Self::Refresh => "Refresh",
            Self::Find => "Find",
            Self::KeepThis => "Toggle Keep on Duplicate",
            Self::NextGroup => "Next Duplicate Group",
            Self::PrevGroup => "Previous Duplicate Group",
            Self::OpenDiffTools => "Open External Diff Tools",
            Self::ToggleDiffMode => "Switch Path / Duplicate Diff",
            Self::Export => "Export Folder Diff / Duplicates",
            Self::ToggleMap => "Switch Tree / Map View",
        }
    }

    fn defaults(self) -> &'static [Shortcut] {
        match self {
            Self::NextDiff => SharedAction::NextDiff.defaults(),
            Self::PrevDiff => SharedAction::PrevDiff.defaults(),
            Self::Refresh => SharedAction::Refresh.defaults(),
            Self::Find => SharedAction::Find.defaults(),
            Self::KeepThis => &[Shortcut::Key {
                key: Key::K,
                modifiers: CTRL,
            }],
            Self::NextGroup => &[Shortcut::Key {
                key: Key::Num4,
                modifiers: CTRL,
            }],
            Self::PrevGroup => &[Shortcut::Key {
                key: Key::Num3,
                modifiers: CTRL,
            }],
            Self::OpenDiffTools => &[Shortcut::Key {
                key: Key::T,
                modifiers: CTRL,
            }],
            Self::ToggleDiffMode => &[Shortcut::Key {
                key: Key::D,
                modifiers: CTRL,
            }],
            Self::Export => SharedAction::Export.defaults(),
            Self::ToggleMap => &[Shortcut::Key {
                key: Key::M,
                modifiers: CTRL,
            }],
        }
    }

    fn shared(self) -> Option<SharedAction> {
        match self {
            Self::NextDiff => Some(SharedAction::NextDiff),
            Self::PrevDiff => Some(SharedAction::PrevDiff),
            Self::Refresh => Some(SharedAction::Refresh),
            Self::Find => Some(SharedAction::Find),
            Self::Export => Some(SharedAction::Export),
            Self::KeepThis
            | Self::NextGroup
            | Self::PrevGroup
            | Self::OpenDiffTools
            | Self::ToggleDiffMode
            | Self::ToggleMap => None,
        }
    }
}

// F5 was Refresh's default before Refresh was shared with zdiff.
pub fn deserialize_keymap<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Keymap<FolderAction>, D::Error> {
    let mut keymap = <Keymap<FolderAction> as serde::Deserialize>::deserialize(deserializer)?;
    let old_default = [Shortcut::Key {
        key: Key::F5,
        modifiers: Modifiers::NONE,
    }];
    if keymap.shortcuts(FolderAction::Refresh) == old_default {
        *keymap.shortcuts_mut(FolderAction::Refresh) = FolderAction::Refresh.defaults().to_vec();
    }
    Ok(keymap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::FileAction;
    use eframe::egui::Event;
    use std::collections::HashSet;
    use zcommon_ui::shared_keymap::sync_from_file;

    #[test]
    fn a_shared_binding_customised_before_the_removed_actions_still_fires_through_the_file_keymap()
    {
        let custom = Shortcut::Key {
            key: Key::F9,
            modifiers: Modifiers::NONE,
        };
        let mut old = serde_json::to_value(Keymap::<FolderAction>::default()).unwrap();
        for name in ["swap_sides", "undo", "open_folder_1"] {
            let saved = old.as_object_mut().unwrap();
            saved.insert(name.to_owned(), serde_json::json!([custom]));
        }
        let folder: Keymap<FolderAction> = serde_json::from_value(old).unwrap();
        assert_eq!(folder, Keymap::default());

        let dir = tempfile::tempdir().unwrap();
        let shared_file = dir.path().join("keymap.json");
        let written_by_the_folder_editor = serde_json::json!({ "swap_sides": [custom] });
        std::fs::write(&shared_file, written_by_the_folder_editor.to_string()).unwrap();
        let mut file = Keymap::<FileAction>::default();
        sync_from_file(&mut file, &shared_file).unwrap();

        let f9 = Event::Key {
            key: Key::F9,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        assert_eq!(file.dispatch(&[f9], false), [FileAction::SwapSides]);
    }

    #[test]
    fn folder_action_names_are_unique() {
        let names: HashSet<_> = FolderAction::all().iter().map(|a| a.name()).collect();
        assert_eq!(names.len(), FolderAction::all().len());
    }

    #[test]
    fn folder_action_defaults_are_clash_free() {
        assert!(Keymap::<FolderAction>::default().clashes().is_empty());
    }

    #[test]
    fn every_folder_action_has_a_default() {
        for &action in FolderAction::all() {
            assert!(!action.defaults().is_empty(), "{action:?}");
        }
    }

    #[test]
    fn default_diff_jumps_read_ctrl_digits_and_alt_arrows_in_press_order() {
        assert_eq!(
            FolderAction::NextDiff.defaults()[0].format(),
            if cfg!(target_os = "macos") {
                "Cmd+Num2"
            } else {
                "Ctrl+Num2"
            }
        );
        let press = |key, modifiers, pressed, repeat| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers,
        };
        let events = [
            press(Key::Num2, CTRL, true, false),
            press(Key::ArrowUp, Modifiers::ALT, true, false),
            press(Key::ArrowDown, Modifiers::ALT, true, true),
            press(Key::Num1, CTRL, true, false),
            press(Key::ArrowDown, Modifiers::NONE, true, false),
            press(Key::Num2, Modifiers::NONE, true, false),
            press(
                Key::ArrowDown,
                Modifiers::ALT | Modifiers::SHIFT,
                true,
                false,
            ),
            press(Key::Num1, CTRL | Modifiers::SHIFT, true, false),
            press(Key::ArrowUp, CTRL, true, false),
            press(Key::Num9, CTRL, true, false),
            press(Key::Num2, CTRL, false, false),
        ];

        assert_eq!(
            Keymap::<FolderAction>::default().dispatch(&events, false),
            [
                FolderAction::NextDiff,
                FolderAction::PrevDiff,
                FolderAction::NextDiff,
                FolderAction::PrevDiff
            ]
        );
    }

    #[test]
    fn ctrl_r_refreshes_by_default() {
        let ctrl_r = Event::Key {
            key: Key::R,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: CTRL,
        };
        assert_eq!(
            Keymap::<FolderAction>::default().dispatch(&[ctrl_r], false),
            [FolderAction::Refresh]
        );
    }

    #[test]
    fn a_shared_action_is_mapped_at_most_once_with_the_shared_defaults() {
        for &shared in SharedAction::ALL {
            let mapped: Vec<_> = FolderAction::all()
                .iter()
                .filter(|a| a.shared() == Some(shared))
                .collect();
            assert!(mapped.len() <= 1, "{shared:?}");
            for action in mapped {
                assert_eq!(action.defaults(), shared.defaults(), "{shared:?}");
            }
        }
    }

    #[test]
    fn keep_and_group_jumps_read_ctrl_k_and_ctrl_digits_by_default() {
        let press = |key| Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: CTRL,
        };
        let events = [press(Key::K), press(Key::Num4), press(Key::Num3)];

        assert_eq!(
            Keymap::<FolderAction>::default().dispatch(&events, false),
            [
                FolderAction::KeepThis,
                FolderAction::NextGroup,
                FolderAction::PrevGroup
            ]
        );
    }

    #[test]
    fn ctrl_f_finds_by_default_and_in_a_keymap_saved_before_find_existed() {
        let ctrl_f = Event::Key {
            key: Key::F,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: CTRL,
        };
        let mut json = serde_json::to_value(Keymap::<FolderAction>::default()).unwrap();
        json.as_object_mut().unwrap().remove("find").unwrap();
        let restored: Keymap<FolderAction> = serde_json::from_value(json).unwrap();
        for keymap in [Keymap::default(), restored] {
            assert_eq!(
                keymap.dispatch(std::slice::from_ref(&ctrl_f), false),
                [FolderAction::Find]
            );
        }
    }
}
