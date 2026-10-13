use eframe::egui::{self, Key, Modifiers};
use zcommon::ui_egui::{
    common::show_custom_popup,
    keymap::{Action, Keymap, Shortcut},
    keymap_editor::KeymapEditor,
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
    OpenKeybindings,
    Find,
    KeepThis,
    NextGroup,
    PrevGroup,
}

impl Action for FolderAction {
    fn all() -> &'static [Self] {
        &[
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
            Self::NextDiff => &[
                Shortcut {
                    key: Key::Num2,
                    modifiers: CTRL,
                },
                Shortcut {
                    key: Key::ArrowDown,
                    modifiers: Modifiers::ALT,
                },
            ],
            Self::PrevDiff => &[
                Shortcut {
                    key: Key::Num1,
                    modifiers: CTRL,
                },
                Shortcut {
                    key: Key::ArrowUp,
                    modifiers: Modifiers::ALT,
                },
            ],
            Self::Refresh => &[Shortcut {
                key: Key::F5,
                modifiers: Modifiers::NONE,
            }],
            Self::OpenKeybindings => &[Shortcut {
                key: Key::O,
                modifiers: CTRL,
            }],
            Self::Find => &[Shortcut {
                key: Key::F,
                modifiers: CTRL,
            }],
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
    fn f5_refreshes_by_default() {
        let f5 = Event::Key {
            key: Key::F5,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        assert_eq!(
            Keymap::<FolderAction>::default().dispatch(&[f5], false),
            [FolderAction::Refresh]
        );
        assert_eq!(FolderAction::Refresh.defaults()[0].format(), "F5");
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
