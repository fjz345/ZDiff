use eframe::egui::{self, Key, PointerButton, RichText};

use super::common::{confirm_button, icon_button, reset_button};
use super::keymap::{Action, Keymap, Shortcut};
use super::shared_keymap::save_shared;

pub struct KeymapEditor<A> {
    capture: Option<(A, Option<usize>)>,
    filter: String,
}

impl<A> Default for KeymapEditor<A> {
    fn default() -> Self {
        Self {
            capture: None,
            filter: String::new(),
        }
    }
}

impl<A: Action> KeymapEditor<A> {
    pub fn is_capturing(&self) -> bool {
        self.capture.is_some()
    }

    pub fn cancel_capture(&mut self) {
        self.capture = None;
    }

    /// While capturing, removes the frame's first key press from the input so later readers
    /// (an app's Esc to quit) don't act on it too.
    pub fn ui(&mut self, ui: &mut egui::Ui, keymap: &mut Keymap<A>) {
        self.ui_where(ui, keymap, |_| true);
    }

    pub fn ui_where(
        &mut self,
        ui: &mut egui::Ui,
        keymap: &mut Keymap<A>,
        lists: impl Fn(A) -> bool,
    ) {
        let before = keymap.clone();
        self.keymap_ui(ui, keymap, &lists);
        if *keymap != before {
            save_shared(&before, keymap);
        }
    }

    fn keymap_ui(&mut self, ui: &mut egui::Ui, keymap: &mut Keymap<A>, lists: &dyn Fn(A) -> bool) {
        if let Some((action, slot)) = self.capture
            && let Some(captured) = take_press(ui)
        {
            self.capture = None;
            if let Some(shortcut) = captured {
                log::info!("Keymap: {} -> {}", action.name(), shortcut.format());
                set_shortcut(keymap.shortcuts_mut(action), slot, shortcut);
            }
        }

        ui.horizontal(|ui| {
            let filter = ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("🔎 Filter by action or key")
                    .desired_width(240.0),
            );
            // Else the next letter typed into the filter would bind as the captured shortcut.
            if filter.clicked() || filter.has_focus() {
                self.capture = None;
            }
            if !self.filter.is_empty() && icon_button(ui, "✖", "Clear filter").clicked() {
                self.filter.clear();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if confirm_button(ui, "Reset all...", "Reset every shortcut to its default") {
                    reset_listed(keymap, lists);
                    self.capture = None;
                }
            });
        });
        ui.weak("Click a shortcut to rebind it to keys or a mouse button, Esc cancels");
        ui.add_space(4.0);

        let clashes = keymap.clashes();
        let warn = ui.visuals().warn_fg_color;
        let mut shown = 0;
        egui::Grid::new("keymap_editor")
            .num_columns(3)
            .striped(true)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                for action in listed_actions(keymap, &self.filter, lists) {
                    shown += 1;
                    let others = |shortcut: Option<&Shortcut>| {
                        let mut labels: Vec<&str> = Vec::new();
                        for (clash, actions) in &clashes {
                            if !actions.contains(&action) || shortcut.is_some_and(|s| s != clash) {
                                continue;
                            }
                            for other in actions.iter().filter(|&&a| a != action) {
                                if !labels.contains(&other.label()) {
                                    labels.push(other.label());
                                }
                            }
                        }
                        labels
                    };

                    let row_others = others(None);
                    if row_others.is_empty() {
                        ui.label(action.label());
                    } else {
                        ui.label(RichText::new(format!("⚠ {}", action.label())).color(warn))
                            .on_hover_text(clash_text(&row_others));
                    }

                    let shortcuts = keymap.shortcuts_mut(action);
                    ui.horizontal(|ui| {
                        let mut remove = None;
                        for (i, shortcut) in shortcuts.iter().enumerate() {
                            let capturing = self.capture == Some((action, Some(i)));
                            let text = if capturing {
                                RichText::new("Press keys...")
                            } else {
                                RichText::new(shortcut.format())
                            };
                            let chip_others = others(Some(shortcut));
                            let chip = if chip_others.is_empty() {
                                ui.add(egui::Button::new(text).selected(capturing))
                            } else {
                                ui.add(egui::Button::new(text.color(warn)).selected(capturing))
                                    .on_hover_text(clash_text(&chip_others))
                            };
                            if chip.clicked() {
                                self.capture = Some((action, Some(i)));
                            }
                            if icon_button(ui, "✖", "Remove").clicked() {
                                remove = Some(i);
                            }
                            ui.add_space(4.0);
                        }
                        if let Some(i) = remove {
                            shortcuts.remove(i);
                            self.capture = None;
                        }
                        let add = if self.capture == Some((action, None)) {
                            ui.add(egui::Button::new("Press keys...").selected(true))
                        } else {
                            icon_button(ui, "➕", "Add a shortcut")
                        };
                        if add.clicked() {
                            self.capture = Some((action, None));
                        }
                    });

                    let is_default = shortcuts.as_slice() == action.defaults();
                    if reset_button(ui, !is_default) {
                        *shortcuts = action.defaults().to_vec();
                        self.capture = None;
                    }
                    ui.end_row();
                }
            });
        if shown == 0 {
            ui.weak("No action matches the filter");
        }
    }
}

fn clash_text(others: &[&str]) -> String {
    format!("Also bound to {}", others.join(", "))
}

/// `Some(None)` for Esc, which cancels.
fn take_press(ui: &egui::Ui) -> Option<Option<Shortcut>> {
    ui.input_mut(|input| {
        let index = input
            .events
            .iter()
            .position(|event| capture(event).is_some())?;
        capture(&input.events.remove(index))
    })
}

fn capture(event: &egui::Event) -> Option<Option<Shortcut>> {
    match *event {
        egui::Event::Key {
            key,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } => Some((key != Key::Escape).then_some(Shortcut::Key { key, modifiers })),
        // The primary button clicks widgets and the secondary opens context menus.
        egui::Event::PointerButton {
            button: button @ (PointerButton::Middle | PointerButton::Extra1 | PointerButton::Extra2),
            pressed: true,
            modifiers,
            ..
        } => Some(Some(Shortcut::Pointer { button, modifiers })),
        _ => None,
    }
}

fn listed_actions<A: Action>(
    keymap: &Keymap<A>,
    filter: &str,
    lists: &dyn Fn(A) -> bool,
) -> Vec<A> {
    let listed = |action: &A| {
        lists(*action) && matches_filter(action.label(), keymap.shortcuts(*action), filter)
    };
    A::all().iter().copied().filter(listed).collect()
}

fn reset_listed<A: Action>(keymap: &mut Keymap<A>, lists: &dyn Fn(A) -> bool) {
    for &action in A::all().iter().filter(|&&action| lists(action)) {
        *keymap.shortcuts_mut(action) = action.defaults().to_vec();
    }
}

fn matches_filter(label: &str, shortcuts: &[Shortcut], filter: &str) -> bool {
    let filter = filter.trim().to_lowercase();
    filter.is_empty()
        || label.to_lowercase().contains(&filter)
        || shortcuts
            .iter()
            .any(|shortcut| shortcut.format().to_lowercase().contains(&filter))
}

fn set_shortcut(shortcuts: &mut Vec<Shortcut>, slot: Option<usize>, shortcut: Shortcut) {
    if shortcuts.contains(&shortcut) {
        return;
    }
    match slot {
        Some(i) => shortcuts[i] = shortcut,
        None => shortcuts.push(shortcut),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Modifiers;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum TestAction {
        Listed,
        Hidden,
    }

    impl Action for TestAction {
        fn all() -> &'static [Self] {
            &[Self::Listed, Self::Hidden]
        }

        fn name(self) -> &'static str {
            match self {
                Self::Listed => "listed",
                Self::Hidden => "hidden",
            }
        }

        fn label(self) -> &'static str {
            self.name()
        }

        fn defaults(self) -> &'static [Shortcut] {
            &[]
        }
    }

    #[test]
    fn the_predicate_hides_actions_from_the_list_and_keeps_their_bindings() {
        let custom = Shortcut::Key {
            key: Key::F,
            modifiers: Modifiers::CTRL,
        };
        let mut keymap = Keymap::<TestAction>::default();
        for &action in TestAction::all() {
            keymap.shortcuts_mut(action).push(custom);
        }
        let lists = |action| action != TestAction::Hidden;

        assert_eq!(listed_actions(&keymap, "", &lists), [TestAction::Listed]);
        assert_eq!(listed_actions(&keymap, "hidden", &lists), []);
        assert_eq!(
            listed_actions(&keymap, "", &|_| true),
            [TestAction::Listed, TestAction::Hidden]
        );

        reset_listed(&mut keymap, &lists);

        assert_eq!(keymap.shortcuts(TestAction::Listed), []);
        assert_eq!(keymap.shortcuts(TestAction::Hidden), [custom]);
    }

    #[test]
    fn filter_matches_label_or_shortcut_text_ignoring_case() {
        let shortcuts = [Shortcut::Key {
            key: Key::F,
            modifiers: Modifiers::CTRL,
        }];
        assert!(matches_filter("Find", &shortcuts, ""));
        assert!(matches_filter("Find", &shortcuts, "  "));
        assert!(matches_filter("Find Next", &shortcuts, "next"));
        assert!(matches_filter("Find", &shortcuts, "ctrl+f"));
        assert!(!matches_filter("Find", &shortcuts, "alt"));
        assert!(!matches_filter("Find", &[], "ctrl"));
    }

    #[test]
    fn capture_takes_key_presses_and_the_middle_back_and_forward_buttons() {
        let key = |key, pressed, repeat| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers: Modifiers::CTRL,
        };
        let pointer = |button, pressed| egui::Event::PointerButton {
            pos: egui::Pos2::ZERO,
            button,
            pressed,
            modifiers: Modifiers::SHIFT,
        };
        assert_eq!(
            capture(&key(Key::Z, true, false)),
            Some(Some(Shortcut::Key {
                key: Key::Z,
                modifiers: Modifiers::CTRL
            }))
        );
        assert_eq!(capture(&key(Key::Escape, true, false)), Some(None));
        assert_eq!(capture(&key(Key::Z, true, true)), None);
        assert_eq!(capture(&key(Key::Z, false, false)), None);
        for button in [
            PointerButton::Middle,
            PointerButton::Extra1,
            PointerButton::Extra2,
        ] {
            assert_eq!(
                capture(&pointer(button, true)),
                Some(Some(Shortcut::Pointer {
                    button,
                    modifiers: Modifiers::SHIFT
                }))
            );
            assert_eq!(capture(&pointer(button, false)), None);
        }
        assert_eq!(capture(&pointer(PointerButton::Primary, true)), None);
        assert_eq!(capture(&pointer(PointerButton::Secondary, true)), None);
    }
}
