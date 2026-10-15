use eframe::egui;

use zcommon::presets::{BuiltinPreset, NameUse, UserPresets};

#[derive(Clone, Default)]
struct SaveDialog {
    name: String,
    confirm_overwrite: bool,
    error: Option<String>,
    focus_name: bool,
}

pub fn presets_menu(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    builtins: &[BuiltinPreset],
    user: &mut UserPresets,
    current_text: &str,
) -> Option<String> {
    let id = ui.make_persistent_id(id_salt);
    let mut picked = None;
    let mut delete = None;
    let mut open_save = false;

    ui.menu_button("Presets", |ui| {
        let mut any_user = false;
        for entry in user.list(builtins) {
            if entry.builtin {
                if ui.button(entry.name).on_hover_text(entry.text).clicked() {
                    picked = Some(entry.text.to_string());
                }
                continue;
            }
            if !any_user {
                ui.separator();
                any_user = true;
            }
            ui.horizontal(|ui| {
                if ui.button(entry.name).on_hover_text(entry.text).clicked() {
                    picked = Some(entry.text.to_string());
                }
                if ui
                    .small_button("x")
                    .on_hover_text("Delete preset")
                    .clicked()
                {
                    delete = Some(entry.name.to_string());
                }
            });
        }
        ui.separator();
        if ui.button("Save as preset...").clicked() {
            open_save = true;
        }
    });

    if let Some(name) = delete
        && let Err(err) = user.delete(builtins, &name)
    {
        log::error!("Deleting preset: {err}");
    }

    if open_save {
        let dialog = SaveDialog {
            focus_name: true,
            ..Default::default()
        };
        ui.data_mut(|d| d.insert_temp(id, dialog));
    }

    if let Some(mut dialog) = ui.data(|d| d.get_temp::<SaveDialog>(id)) {
        let mut close = false;
        let modal = egui::Modal::new(id.with("save_dialog")).show(ui.ctx(), |ui| {
            ui.heading("Save as preset");
            if dialog.confirm_overwrite {
                ui.label(format!("Overwrite preset \"{}\"?", dialog.name.trim()));
                ui.horizontal(|ui| {
                    if ui.button("Overwrite").clicked() {
                        match user.save(builtins, &dialog.name, current_text) {
                            Ok(_) => close = true,
                            Err(err) => {
                                dialog.error = Some(err.to_string());
                                dialog.confirm_overwrite = false;
                            }
                        }
                    }
                    if ui.button("Back").clicked() {
                        dialog.confirm_overwrite = false;
                        dialog.focus_name = true;
                    }
                });
                return;
            }

            let name_edit = ui.add(egui::TextEdit::singleline(&mut dialog.name).hint_text("Name"));
            if std::mem::take(&mut dialog.focus_name) {
                name_edit.request_focus();
            }
            if name_edit.changed() {
                dialog.error = None;
            }
            if let Some(err) = &dialog.error {
                ui.colored_label(ui.visuals().error_fg_color, err);
            }
            let enter = name_edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                let can_save = !dialog.name.trim().is_empty();
                let save_clicked = ui
                    .add_enabled(can_save, egui::Button::new("Save"))
                    .clicked();
                if save_clicked || (enter && can_save) {
                    if user.name_use(builtins, &dialog.name) == NameUse::User {
                        dialog.confirm_overwrite = true;
                    } else {
                        match user.save(builtins, &dialog.name, current_text) {
                            Ok(_) => close = true,
                            Err(err) => dialog.error = Some(err.to_string()),
                        }
                    }
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });

        if close || modal.should_close() {
            ui.data_mut(|d| d.remove::<SaveDialog>(id));
        } else {
            ui.data_mut(|d| d.insert_temp(id, dialog));
        }
    }

    picked
}
