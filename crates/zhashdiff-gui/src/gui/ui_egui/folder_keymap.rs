use eframe::egui::{self, Key, Modifiers};
use zcommon_ui::{
    common::show_custom_popup,
    keymap::{Action, Keymap, Shortcut},
    keymap_editor::KeymapEditor,
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
    OpenFolder1,
    OpenFolder2,
    NextDiff,
    PrevDiff,
    Refresh,
    OpenKeybindings,
    Find,
    KeepThis,
    NextGroup,
    PrevGroup,
}

impl Action for FolderAction {
    fn all() -> &'static [Self] {
        &[
            Self::OpenFolder1,
            Self::OpenFolder2,
            Self::NextDiff,
            Self::PrevDiff,
            Self::Refresh,
            Self::OpenKeybindings,
            Self::Find,
            Self::KeepThis,
            Self::NextGroup,
            Self::PrevGroup,
        ]
    }

    fn name(self) -> &'static str {
        match self {
            Self::OpenFolder1 => "open_folder_1",
            Self::OpenFolder2 => "open_folder_2",
            Self::NextDiff => "next_diff",
            Self::PrevDiff => "prev_diff",
            Self::Refresh => "refresh",
            Self::OpenKeybindings => "open_keybindings",
            Self::Find => "find",
            Self::KeepThis => "keep_this",
            Self::NextGroup => "next_group",
            Self::PrevGroup => "prev_group",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::OpenFolder1 => "Open Folder 1",
            Self::OpenFolder2 => "Open Folder 2",
            Self::NextDiff => "Next differing file",
            Self::PrevDiff => "Previous differing file",
            Self::Refresh => "Refresh",
            Self::OpenKeybindings => "Open Keybindings",
            Self::Find => "Find",
            Self::KeepThis => "Keep this",
            Self::NextGroup => "Next duplicate group",
            Self::PrevGroup => "Previous duplicate group",
        }
    }

    fn defaults(self) -> &'static [Shortcut] {
        match self {
            Self::OpenFolder1 => SharedAction::Open1.defaults(),
            Self::OpenFolder2 => SharedAction::Open2.defaults(),
            Self::NextDiff => SharedAction::NextDiff.defaults(),
            Self::PrevDiff => SharedAction::PrevDiff.defaults(),
            Self::Refresh => SharedAction::Refresh.defaults(),
            Self::OpenKeybindings => SharedAction::OpenKeybindings.defaults(),
            Self::Find => SharedAction::Find.defaults(),
            Self::KeepThis => &[Shortcut {
                key: Key::K,
                modifiers: CTRL,
            }],
            Self::NextGroup => &[Shortcut {
                key: Key::Num4,
                modifiers: CTRL,
            }],
            Self::PrevGroup => &[Shortcut {
                key: Key::Num3,
                modifiers: CTRL,
            }],
        }
    }

    fn shared(self) -> Option<SharedAction> {
        match self {
            Self::OpenFolder1 => Some(SharedAction::Open1),
            Self::OpenFolder2 => Some(SharedAction::Open2),
            Self::NextDiff => Some(SharedAction::NextDiff),
            Self::PrevDiff => Some(SharedAction::PrevDiff),
            Self::Refresh => Some(SharedAction::Refresh),
            Self::OpenKeybindings => Some(SharedAction::OpenKeybindings),
            Self::Find => Some(SharedAction::Find),
            Self::KeepThis | Self::NextGroup | Self::PrevGroup => None,
        }
    }
}

// F5 was Refresh's default before Refresh was shared with zdiff.
pub fn deserialize_keymap<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Keymap<FolderAction>, D::Error> {
    let mut keymap = <Keymap<FolderAction> as serde::Deserialize>::deserialize(deserializer)?;
    let old_default = [Shortcut {
        key: Key::F5,
        modifiers: Modifiers::NONE,
    }];
    if keymap.shortcuts(FolderAction::Refresh) == old_default {
        *keymap.shortcuts_mut(FolderAction::Refresh) = FolderAction::Refresh.defaults().to_vec();
    }
    Ok(keymap)
}

#[derive(Default)]
pub struct KeybindingsWindow {
    pub open: bool,
    editor: KeymapEditor<FolderAction>,
}

impl KeybindingsWindow {
    pub fn is_capturing(&self) -> bool {
        self.editor.is_capturing()
    }

    pub fn show(&mut self, ctx: &egui::Context, keymap: &mut Keymap<FolderAction>) {
        show_custom_popup(ctx, &mut self.open, "Keybindings", true, |ui| {
            self.editor.ui(ui, keymap, |_| {});
        });
        if !self.open {
            self.editor.cancel_capture();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Event;
    use std::collections::HashSet;

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
    fn every_shared_action_is_mapped_once_with_the_shared_defaults() {
        for &shared in SharedAction::ALL {
            let mapped: Vec<_> = FolderAction::all()
                .iter()
                .filter(|a| a.shared() == Some(shared))
                .collect();
            assert_eq!(mapped.len(), 1, "{shared:?}");
            assert_eq!(mapped[0].defaults(), shared.defaults(), "{shared:?}");
        }
    }

    #[test]
    fn ctrl_f1_and_f2_open_the_folders_by_default() {
        let press = |key| Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: CTRL,
        };
        assert_eq!(
            Keymap::<FolderAction>::default().dispatch(&[press(Key::F1), press(Key::F2)], false),
            [FolderAction::OpenFolder1, FolderAction::OpenFolder2]
        );
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
