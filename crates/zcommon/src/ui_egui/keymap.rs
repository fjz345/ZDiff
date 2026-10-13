use std::{collections::HashMap, hash::Hash};

use eframe::egui::{self, Key, Modifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Shortcut {
    pub key: Key,
    pub modifiers: Modifiers,
}

impl Shortcut {
    pub fn format(&self) -> String {
        let mut s = String::new();
        if self.modifiers.ctrl {
            s.push_str("Ctrl+");
        }
        if self.modifiers.shift {
            s.push_str("Shift+");
        }
        if self.modifiers.alt {
            s.push_str("Alt+");
        }
        if self.modifiers.mac_cmd {
            s.push_str("Cmd+");
        }
        s.push_str(&format!("{:?}", self.key));
        s
    }

    pub fn matches(&self, r: &egui::InputState) -> bool {
        r.key_pressed(self.key) && r.modifiers == self.modifiers
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
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            let pressed = Shortcut {
                key: *key,
                modifiers: *modifiers,
            };
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
}

#[cfg(feature = "serde")]
impl<A: Action> serde::Serialize for Keymap<A> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(
            A::all()
                .iter()
                .map(|&action| (action.name(), self.shortcuts(action))),
        )
    }
}

#[cfg(feature = "serde")]
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
                Self::Find => &[Shortcut {
                    key: Key::F,
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
                Self::PrevConflict => &[Shortcut {
                    key: Key::Num1,
                    modifiers: CTRL_SHIFT,
                }],
                Self::Refresh => &[Shortcut {
                    key: Key::F5,
                    modifiers: Modifiers::NONE,
                }],
                Self::Redo => &[Shortcut {
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

    #[cfg(feature = "serde")]
    #[test]
    fn keymap_serde_round_trip() {
        let mut keymap = Keymap::<TestAction>::default();
        keymap.shortcuts_mut(TestAction::Refresh).push(Shortcut {
            key: Key::R,
            modifiers: CTRL,
        });
        let json = serde_json::to_string(&keymap).unwrap();
        assert_eq!(
            serde_json::from_str::<Keymap<TestAction>>(&json).unwrap(),
            keymap
        );
    }

    #[cfg(feature = "serde")]
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

    #[cfg(feature = "serde")]
    #[test]
    fn keymap_deserialize_drops_unknown_names() {
        let mut json = serde_json::to_value(Keymap::<TestAction>::default()).unwrap();
        json["removed_action"] = json["find"].clone();
        let keymap = serde_json::from_value::<Keymap<TestAction>>(json).unwrap();
        assert_eq!(keymap, Keymap::default());
    }

    #[cfg(feature = "serde")]
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
}
