use eframe::egui::{self, Key, RichText};

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
        let before = keymap.clone();
        self.keymap_ui(ui, keymap);
        if *keymap != before {
            save_shared(&before, keymap);
        }
    }

    fn keymap_ui(&mut self, ui: &mut egui::Ui, keymap: &mut Keymap<A>) {
        if let Some((action, slot)) = self.capture
            && let Some(captured) = take_key_press(ui)
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
                    *keymap = Keymap::default();
                    self.capture = None;
                }
            });
        });
        ui.weak("Click a shortcut to rebind it, Esc cancels");
        ui.add_space(4.0);

        let clashes = keymap.clashes();
        let warn = ui.visuals().warn_fg_color;
        let mut shown = 0;
        egui::Grid::new("keymap_editor")
            .num_columns(3)
            .striped(true)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                for &action in A::all() {
                    if !matches_filter(action.label(), keymap.shortcuts(action), &self.filter) {
                        continue;
                    }
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
fn take_key_press(ui: &egui::Ui) -> Option<Option<Shortcut>> {
    ui.input_mut(|input| {
        let index = input.events.iter().position(|event| {
            matches!(
                event,
                egui::Event::Key {
                    pressed: true,
                    repeat: false,
                    ..
                }
            )
        })?;
        let egui::Event::Key { key, modifiers, .. } = input.events.remove(index) else {
            unreachable!("position matched a key event");
        };
        Some((key != Key::Escape).then_some(Shortcut { key, modifiers }))
    })
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

    #[test]
    fn filter_matches_label_or_shortcut_text_ignoring_case() {
        let shortcuts = [Shortcut {
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
}
