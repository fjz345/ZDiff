use eframe::egui::{self, Key, RichText};

use super::keymap::{Action, Keymap, Shortcut};
use super::shared_keymap::save_shared;

pub struct KeymapEditor<A> {
    capture: Option<(A, Option<usize>)>,
}

impl<A> Default for KeymapEditor<A> {
    fn default() -> Self {
        Self { capture: None }
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
    /// (an app's Esc to quit) don't act on it too. `extra_rows` draws into the same grid.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        keymap: &mut Keymap<A>,
        extra_rows: impl FnOnce(&mut egui::Ui),
    ) {
        let before = keymap.clone();
        self.keymap_ui(ui, keymap, extra_rows);
        if *keymap != before {
            save_shared(&before, keymap);
        }
    }

    fn keymap_ui(
        &mut self,
        ui: &mut egui::Ui,
        keymap: &mut Keymap<A>,
        extra_rows: impl FnOnce(&mut egui::Ui),
    ) {
        if let Some((action, slot)) = self.capture
            && let Some(captured) = take_key_press(ui)
        {
            self.capture = None;
            if let Some(shortcut) = captured {
                log::info!("Keymap: {} -> {}", action.name(), shortcut.format());
                set_shortcut(keymap.shortcuts_mut(action), slot, shortcut);
            }
        }

        if ui.button("Reset to defaults").clicked() {
            *keymap = Keymap::default();
            self.capture = None;
        }
        ui.separator();

        let clashes = keymap.clashes();
        let warn = ui.visuals().warn_fg_color;
        egui::Grid::new("keymap_editor")
            .num_columns(2)
            .spacing([40.0, 8.0])
            .show(ui, |ui| {
                for &action in A::all() {
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
                        ui.label(RichText::new(action.label()).color(warn))
                            .on_hover_text(clash_text(&row_others));
                    }

                    ui.horizontal(|ui| {
                        let shortcuts = keymap.shortcuts_mut(action);
                        let mut remove = None;
                        for (i, shortcut) in shortcuts.iter().enumerate() {
                            let text = if self.capture == Some((action, Some(i))) {
                                RichText::new("Press keys...")
                            } else {
                                RichText::new(shortcut.format())
                            };
                            let chip_others = others(Some(shortcut));
                            let chip = if chip_others.is_empty() {
                                ui.button(text)
                            } else {
                                ui.button(text.color(warn))
                                    .on_hover_text(clash_text(&chip_others))
                            };
                            if chip.clicked() {
                                self.capture = Some((action, Some(i)));
                            }
                            if ui.small_button("x").on_hover_text("Remove").clicked() {
                                remove = Some(i);
                            }
                        }
                        if let Some(i) = remove {
                            shortcuts.remove(i);
                            self.capture = None;
                        }
                        let add_text = if self.capture == Some((action, None)) {
                            "Press keys..."
                        } else {
                            "+"
                        };
                        if ui
                            .button(add_text)
                            .on_hover_text("Add a shortcut")
                            .clicked()
                        {
                            self.capture = Some((action, None));
                        }
                    });
                    ui.end_row();
                }
                extra_rows(ui);
            });
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

fn set_shortcut(shortcuts: &mut Vec<Shortcut>, slot: Option<usize>, shortcut: Shortcut) {
    if shortcuts.contains(&shortcut) {
        return;
    }
    match slot {
        Some(i) => shortcuts[i] = shortcut,
        None => shortcuts.push(shortcut),
    }
}
