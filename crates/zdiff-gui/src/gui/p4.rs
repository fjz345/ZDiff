use eframe::egui;

pub use zcommon::vcs::p4::*;

pub fn ui_p4_profiles(ui: &mut egui::Ui, profiles: &mut P4Profiles) {
    ui.vertical(|ui| {
        ui.heading("Perforce Profiles");
        ui.label("A Quick Diff slot's profile goes over the default profile");
        ui.label("Empty fields come from the default profile, then P4CONFIG or the environment");
        ui.label("# to unset an env var for the command");

        ui.horizontal(|ui| {
            ui.label("Default profile");
            ui_p4_profile_combo(
                ui,
                "p4_default_profile",
                &profiles.profiles,
                "None",
                &mut profiles.default,
            );
        });
        if ui.button("Add Profile").clicked() {
            let name = format!("Profile {}", profiles.profiles.len() + 1);
            profiles.add(name);
        }

        let mut remove = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for profile in &mut profiles.profiles {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut profile.name)
                            .hint_text("name")
                            .desired_width(200.0),
                    );
                    if ui.button("Remove").clicked() {
                        remove = Some(profile.id);
                    }
                });

                let config = &mut profile.config;
                egui::Grid::new(("p4_profile_grid", profile.id))
                    .num_columns(2)
                    .spacing([40.0, 8.0])
                    .show(ui, |ui| {
                        let mut edit_row =
                            |label: &str, value: &mut String, hint: &str, password: bool| {
                                ui.label(label);
                                ui.add(
                                    egui::TextEdit::singleline(value)
                                        .hint_text(hint)
                                        .password(password)
                                        .desired_width(200.0),
                                );
                                ui.end_row();
                            };

                        edit_row("P4PORT", &mut config.port, "ssl:perforce:1666", false);
                        edit_row("P4USER", &mut config.user, "username", false);
                        edit_row("P4CLIENT", &mut config.client, "workspace_name", false);
                        edit_row("P4CHARSET", &mut config.charset, "utf8", false);
                        edit_row("P4PASSWD", &mut config.password, "password", true);
                    });
            }
        });
        if let Some(id) = remove {
            profiles.remove(id);
        }

        ui.add_space(10.0);
    });
}

/// Picks a profile, or `none_label` for none. A removed profile shows as none, which is how it
/// resolves.
pub fn ui_p4_profile_combo(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    profiles: &[P4Profile],
    none_label: &str,
    selected: &mut Option<u64>,
) {
    let selected_text = selected
        .and_then(|id| profiles.iter().find(|profile| profile.id == id))
        .map_or(none_label, |profile| profile.name.as_str());
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            ui.selectable_value(selected, None, none_label);
            for profile in profiles {
                ui.selectable_value(selected, Some(profile.id), &profile.name);
            }
        });
}
