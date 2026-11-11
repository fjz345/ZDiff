use std::{collections::HashMap, hash::Hash};

use eframe::egui::{self, Key, Modifiers, PointerButton};

use super::shared_keymap::SharedAction;

// Untagged so a key shortcut keeps the `{key, modifiers}` shape older keymap files hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Shortcut {
    Key {
        key: Key,
        modifiers: Modifiers,
    },
    Pointer {
        button: PointerButton,
        modifiers: Modifiers,
    },
}

impl Shortcut {
    pub fn modifiers(&self) -> Modifiers {
        match *self {
            Self::Key { modifiers, .. } | Self::Pointer { modifiers, .. } => modifiers,
        }
    }

    pub fn format(&self) -> String {
        let modifiers = self.modifiers();
        let mut s = String::new();
        if modifiers.ctrl {
            s.push_str("Ctrl+");
        }
        if modifiers.shift {
            s.push_str("Shift+");
        }
        if modifiers.alt {
            s.push_str("Alt+");
        }
        if modifiers.mac_cmd {
            s.push_str("Cmd+");
        }
        match self {
            Self::Key { key, .. } => s.push_str(&format!("{key:?}")),
            Self::Pointer { button, .. } => s.push_str(pointer_name(*button)),
        }
        s
    }

    pub fn matches(&self, r: &egui::InputState) -> bool {
        let pressed = match *self {
            Self::Key { key, .. } => r.key_pressed(key),
            Self::Pointer { button, .. } => r.pointer.button_pressed(button),
        };
        pressed && r.modifiers == self.modifiers()
    }

    fn pressed_by(event: &egui::Event) -> Option<Self> {
        match *event {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => Some(Self::Key { key, modifiers }),
            egui::Event::PointerButton {
                button,
                pressed: true,
                modifiers,
                ..
            } => Some(Self::Pointer { button, modifiers }),
            _ => None,
        }
    }
}

fn pointer_name(button: PointerButton) -> &'static str {
    match button {
        PointerButton::Primary => "Mouse Left",
        PointerButton::Secondary => "Mouse Right",
        PointerButton::Middle => "Mouse Middle",
        PointerButton::Extra1 => "Mouse Back",
        PointerButton::Extra2 => "Mouse Forward",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingPolicy {
    CtrlOrAlt,
    Never,
}

pub trait Action: Copy + Eq + Hash + 'static {
    fn all() -> &'static [Self];
    fn name(self) -> &'static str;
    fn label(self) -> &'static str;
    fn defaults(self) -> &'static [Shortcut];
    fn typing_policy(self) -> TypingPolicy {
        TypingPolicy::CtrlOrAlt
    }
    fn shared(self) -> Option<SharedAction> {
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Keymap<A: Action> {
    bindings: HashMap<A, Vec<Shortcut>>,
}

impl<A: Action> Default for Keymap<A> {
    fn default() -> Self {
        Self {
            bindings: A::all()
                .iter()
                .map(|&action| (action, action.defaults().to_vec()))
                .collect(),
        }
    }
}

impl<A: Action> Keymap<A> {
    pub fn shortcuts(&self, action: A) -> &[Shortcut] {
        &self.bindings[&action]
    }

    pub fn shortcuts_mut(&mut self, action: A) -> &mut Vec<Shortcut> {
        self.bindings
            .get_mut(&action)
            .expect("Keymap holds every action")
    }

    pub fn dispatch(&self, events: &[egui::Event], text_focus: bool) -> Vec<A> {
        let mut triggered = Vec::new();
        for event in events {
            let Some(pressed) = Shortcut::pressed_by(event) else {
                continue;
            };
            let modifiers = pressed.modifiers();
            for &action in A::all() {
                if !self.shortcuts(action).contains(&pressed) {
                    continue;
                }
                let allowed = !text_focus
                    || match action.typing_policy() {
                        TypingPolicy::CtrlOrAlt => {
                            modifiers.ctrl
                                || modifiers.command
                                || modifiers.mac_cmd
                                || modifiers.alt
                        }
                        TypingPolicy::Never => false,
                    };
                if allowed {
                    triggered.push(action);
                }
            }
        }
        triggered
    }

    pub fn clashes(&self) -> Vec<(Shortcut, Vec<A>)> {
        let mut by_shortcut: Vec<(Shortcut, Vec<A>)> = Vec::new();
        for &action in A::all() {
            for &shortcut in self.shortcuts(action) {
                match by_shortcut.iter_mut().find(|(s, _)| *s == shortcut) {
                    Some((_, actions)) if !actions.contains(&action) => actions.push(action),
                    Some(_) => {}
                    None => by_shortcut.push((shortcut, vec![action])),
                }
            }
        }
        by_shortcut.retain(|(_, actions)| actions.len() > 1);
        by_shortcut
    }

    pub fn first_shortcut_text(&self, action: A) -> Option<String> {
        self.shortcuts(action).first().map(Shortcut::format)
    }

    pub fn with_shortcut(&self, action: A, text: &str) -> String {
        match self.first_shortcut_text(action) {
            Some(shortcut) => format!("{text} ({shortcut})"),
            None => text.to_owned(),
        }
    }
}

pub fn menu_item<A: Action>(
    ui: &mut egui::Ui,
    label: &str,
    keymap: &Keymap<A>,
    action: A,
) -> egui::Response {
    let mut button = egui::Button::new(label);
    if let Some(text) = keymap.first_shortcut_text(action) {
        button = button.shortcut_text(text);
    }
    let response = ui.add(button);
    if response.clicked() {
        ui.close();
    }
    response
}

impl<A: Action> serde::Serialize for Keymap<A> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(
            A::all()
                .iter()
                .map(|&action| (action.name(), self.shortcuts(action))),
        )
    }
}

impl<'de, A: Action> serde::Deserialize<'de> for Keymap<A> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut saved =
            std::collections::BTreeMap::<String, Vec<Shortcut>>::deserialize(deserializer)?;
        let bindings = A::all()
            .iter()
            .map(|&action| {
                let shortcuts = saved
                    .remove(action.name())
                    .unwrap_or_else(|| action.defaults().to_vec());
                (action, shortcuts)
            })
            .collect();
        for name in saved.keys() {
            log::warn!("Keymap: dropping bindings of unknown action {name:?}");
        }
        Ok(Self { bindings })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: Modifiers = Modifiers {
        ctrl: true,
        command: true,
        ..Modifiers::NONE
    };
    const CTRL_SHIFT: Modifiers = Modifiers {
        shift: true,
        ..CTRL
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum TestAction {
        Find,
        NextConflict,
        PrevConflict,
        Refresh,
        Redo,
    }

    impl Action for TestAction {
        fn all() -> &'static [Self] {
            &[
                Self::Find,
                Self::NextConflict,
                Self::PrevConflict,
                Self::Refresh,
                Self::Redo,
            ]
        }

        fn name(self) -> &'static str {
            match self {
                Self::Find => "find",
                Self::NextConflict => "next_conflict",
                Self::PrevConflict => "prev_conflict",
                Self::Refresh => "refresh",
                Self::Redo => "redo",
            }
        }

        fn label(self) -> &'static str {
            self.name()
        }

        fn defaults(self) -> &'static [Shortcut] {
            match self {
                Self::Find => &[Shortcut::Key {
                    key: Key::F,
                    modifiers: CTRL,
                }],
                Self::NextConflict => &[
                    Shortcut::Key {
                        key: Key::Num2,
                        modifiers: CTRL,
                    },
                    Shortcut::Key {
                        key: Key::ArrowDown,
                        modifiers: Modifiers::ALT,
                    },
                ],
                Self::PrevConflict => &[Shortcut::Key {
                    key: Key::Num1,
                    modifiers: CTRL_SHIFT,
                }],
                Self::Refresh => &[Shortcut::Key {
                    key: Key::F5,
                    modifiers: Modifiers::NONE,
                }],
                Self::Redo => &[Shortcut::Key {
                    key: Key::Y,
                    modifiers: CTRL,
                }],
            }
        }

        fn typing_policy(self) -> TypingPolicy {
            match self {
                Self::Redo => TypingPolicy::Never,
                _ => TypingPolicy::CtrlOrAlt,
            }
        }
    }

    fn press(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn release(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers,
        }
    }

    fn pointer(button: PointerButton, pressed: bool, modifiers: Modifiers) -> egui::Event {
        egui::Event::PointerButton {
            pos: egui::Pos2::ZERO,
            button,
            pressed,
            modifiers,
        }
    }

    fn with_back_on_refresh() -> Keymap<TestAction> {
        let mut keymap = Keymap::<TestAction>::default();
        keymap
            .shortcuts_mut(TestAction::Refresh)
            .push(Shortcut::Pointer {
                button: PointerButton::Extra1,
                modifiers: Modifiers::NONE,
            });
        keymap
    }

    #[test]
    fn dispatch_fires_a_pointer_binding_on_its_press_only() {
        let keymap = with_back_on_refresh();
        let events = [
            pointer(PointerButton::Extra1, true, Modifiers::NONE),
            pointer(PointerButton::Extra1, false, Modifiers::NONE),
            pointer(PointerButton::Extra2, true, Modifiers::NONE),
            pointer(PointerButton::Primary, true, Modifiers::NONE),
            pointer(PointerButton::Extra1, true, CTRL),
        ];
        assert_eq!(keymap.dispatch(&events, false), [TestAction::Refresh]);
    }

    #[test]
    fn dispatch_applies_the_typing_policy_to_pointer_bindings() {
        let mut keymap = with_back_on_refresh();
        let back = [pointer(PointerButton::Extra1, true, Modifiers::NONE)];
        assert!(keymap.dispatch(&back, true).is_empty());

        keymap
            .shortcuts_mut(TestAction::Redo)
            .push(Shortcut::Pointer {
                button: PointerButton::Extra2,
                modifiers: CTRL,
            });
        let ctrl_forward = [pointer(PointerButton::Extra2, true, CTRL)];
        assert!(keymap.dispatch(&ctrl_forward, true).is_empty());
        assert_eq!(keymap.dispatch(&ctrl_forward, false), [TestAction::Redo]);
    }

    #[test]
    fn a_key_shortcut_keeps_the_json_shape_of_older_keymaps() {
        let ctrl_f = Shortcut::Key {
            key: Key::F,
            modifiers: CTRL,
        };
        let json = serde_json::to_value(ctrl_f).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "key": "F", "modifiers": serde_json::to_value(CTRL).unwrap() })
        );
        assert_eq!(serde_json::from_value::<Shortcut>(json).unwrap(), ctrl_f);
    }

    #[test]
    fn a_pointer_shortcut_round_trips_through_json() {
        let back = Shortcut::Pointer {
            button: PointerButton::Extra1,
            modifiers: Modifiers::SHIFT,
        };
        let json = serde_json::to_string(&back).unwrap();
        assert_eq!(serde_json::from_str::<Shortcut>(&json).unwrap(), back);
    }

    #[test]
    fn dispatch_fires_a_single_binding() {
        let keymap = Keymap::<TestAction>::default();
        let events = [press(Key::F, CTRL), release(Key::F, CTRL)];
        assert_eq!(keymap.dispatch(&events, false), [TestAction::Find]);
    }

    #[test]
    fn dispatch_fires_the_second_binding_of_an_action() {
        let keymap = Keymap::<TestAction>::default();
        let events = [press(Key::ArrowDown, Modifiers::ALT)];
        assert_eq!(keymap.dispatch(&events, false), [TestAction::NextConflict]);
    }

    #[test]
    fn dispatch_keeps_press_order_across_actions() {
        let keymap = Keymap::<TestAction>::default();
        let events = [
            press(Key::F5, Modifiers::NONE),
            press(Key::F, CTRL),
            press(Key::F5, Modifiers::NONE),
            press(Key::Num2, CTRL),
        ];
        assert_eq!(
            keymap.dispatch(&events, false),
            [
                TestAction::Refresh,
                TestAction::Find,
                TestAction::Refresh,
                TestAction::NextConflict,
            ]
        );
    }

    #[test]
    fn dispatch_matches_modifiers_exactly() {
        let keymap = Keymap::<TestAction>::default();
        assert!(keymap.dispatch(&[press(Key::Num1, CTRL)], false).is_empty());
        assert!(
            keymap
                .dispatch(&[press(Key::Num2, CTRL_SHIFT)], false)
                .is_empty()
        );
        assert!(
            keymap
                .dispatch(&[press(Key::F, Modifiers::NONE)], false)
                .is_empty()
        );
        assert_eq!(
            keymap.dispatch(&[press(Key::Num1, CTRL_SHIFT)], false),
            [TestAction::PrevConflict]
        );
    }

    #[test]
    fn dispatch_suppresses_modifier_less_shortcuts_while_typing() {
        let keymap = Keymap::<TestAction>::default();
        let events = [press(Key::F5, Modifiers::NONE)];
        assert!(keymap.dispatch(&events, true).is_empty());
        assert_eq!(keymap.dispatch(&events, false), [TestAction::Refresh]);
    }

    #[test]
    fn dispatch_allows_ctrl_and_alt_shortcuts_while_typing() {
        let keymap = Keymap::<TestAction>::default();
        let events = [press(Key::F, CTRL), press(Key::ArrowDown, Modifiers::ALT)];
        assert_eq!(
            keymap.dispatch(&events, true),
            [TestAction::Find, TestAction::NextConflict]
        );
    }

    #[test]
    fn dispatch_suppresses_never_while_typing_actions_even_with_ctrl() {
        let keymap = Keymap::<TestAction>::default();
        let events = [press(Key::Y, CTRL)];
        assert!(keymap.dispatch(&events, true).is_empty());
        assert_eq!(keymap.dispatch(&events, false), [TestAction::Redo]);
    }

    #[test]
    fn dispatch_skips_unbound_actions() {
        let mut keymap = Keymap::<TestAction>::default();
        keymap.shortcuts_mut(TestAction::Find).clear();
        assert!(keymap.dispatch(&[press(Key::F, CTRL)], false).is_empty());
    }

    #[test]
    fn clashes_report_a_shortcut_on_two_actions() {
        let mut keymap = Keymap::<TestAction>::default();
        assert!(keymap.clashes().is_empty());

        let ctrl_f = keymap.shortcuts(TestAction::Find)[0];
        keymap.shortcuts_mut(TestAction::Refresh).push(ctrl_f);
        assert_eq!(
            keymap.clashes(),
            [(ctrl_f, vec![TestAction::Find, TestAction::Refresh])]
        );
        assert_eq!(
            keymap.dispatch(&[press(Key::F, CTRL)], false),
            [TestAction::Find, TestAction::Refresh]
        );
    }

    #[test]
    fn keymap_serde_round_trip() {
        let mut keymap = Keymap::<TestAction>::default();
        keymap
            .shortcuts_mut(TestAction::Refresh)
            .push(Shortcut::Key {
                key: Key::R,
                modifiers: CTRL,
            });
        let json = serde_json::to_string(&keymap).unwrap();
        assert_eq!(
            serde_json::from_str::<Keymap<TestAction>>(&json).unwrap(),
            keymap
        );
    }

    #[test]
    fn keymap_deserialize_gives_missing_actions_their_defaults() {
        let mut json = serde_json::to_value(Keymap::<TestAction>::default()).unwrap();
        json.as_object_mut().unwrap().remove("find");
        json["refresh"] = serde_json::json!([]);
        let keymap = serde_json::from_value::<Keymap<TestAction>>(json).unwrap();
        assert_eq!(
            keymap.shortcuts(TestAction::Find),
            TestAction::Find.defaults()
        );
        assert!(keymap.shortcuts(TestAction::Refresh).is_empty());
    }

    #[test]
    fn keymap_deserialize_drops_unknown_names() {
        let mut json = serde_json::to_value(Keymap::<TestAction>::default()).unwrap();
        json["removed_action"] = json["find"].clone();
        let keymap = serde_json::from_value::<Keymap<TestAction>>(json).unwrap();
        assert_eq!(keymap, Keymap::default());
    }

    #[test]
    fn keymap_deserialize_keeps_an_explicit_empty_list() {
        let keymap =
            serde_json::from_str::<Keymap<TestAction>>(r#"{"find": [], "redo": []}"#).unwrap();
        assert!(keymap.shortcuts(TestAction::Find).is_empty());
        assert!(keymap.shortcuts(TestAction::Redo).is_empty());
        assert_eq!(
            keymap.shortcuts(TestAction::NextConflict),
            TestAction::NextConflict.defaults()
        );
    }

    #[test]
    fn first_shortcut_text_follows_the_keymap() {
        let mut keymap = Keymap::<TestAction>::default();
        assert_eq!(
            keymap.first_shortcut_text(TestAction::Find).as_deref(),
            Some("Ctrl+F")
        );
        keymap.shortcuts_mut(TestAction::Find).insert(
            0,
            Shortcut::Key {
                key: Key::F5,
                modifiers: Modifiers::CTRL,
            },
        );
        assert_eq!(
            keymap.first_shortcut_text(TestAction::Find).as_deref(),
            Some("Ctrl+F5")
        );
        keymap.shortcuts_mut(TestAction::Find).clear();
        assert_eq!(keymap.first_shortcut_text(TestAction::Find), None);
    }

    #[test]
    fn with_shortcut_appends_the_first_shortcut_only_when_bound() {
        let mut keymap = Keymap::<TestAction>::default();
        assert_eq!(
            keymap.with_shortcut(TestAction::Refresh, "Refresh"),
            "Refresh (F5)"
        );
        keymap.shortcuts_mut(TestAction::Refresh).clear();
        assert_eq!(
            keymap.with_shortcut(TestAction::Refresh, "Refresh"),
            "Refresh"
        );
    }
}
