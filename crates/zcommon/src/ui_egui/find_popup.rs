use eframe::egui;

use crate::{find::FindQuery, ui_egui::common::show_custom_popup};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindPopupEvent {
    Changed,
    Submitted,
    Closed,
}

/// `extra` adds caller toggles below the shared ones and returns whether it changed any.
pub fn find_popup(
    ctx: &egui::Context,
    query: &mut FindQuery,
    error: Option<&str>,
    status: Option<&str>,
    extra: impl FnOnce(&mut egui::Ui) -> bool,
) -> Option<FindPopupEvent> {
    let mut open = true;
    let mut changed = false;
    let mut submitted = false;
    show_custom_popup(ctx, &mut open, "Find", true, |ui| {
        let response = ui.add(egui::TextEdit::singleline(&mut query.text).desired_width(160.0));
        response.request_focus();
        changed |= response.changed();
        if let Some(error) = error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        changed |= ui
            .checkbox(&mut query.case_sensitive, "Case sensitive")
            .changed();
        changed |= ui.checkbox(&mut query.whole_word, "Whole word").changed();
        changed |= ui.checkbox(&mut query.regex, "Regex").changed();
        changed |= extra(ui);
        if let Some(status) = status {
            ui.label(status);
        }
        submitted = ui.input(|i| i.key_pressed(egui::Key::Enter));
    });
    if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        Some(FindPopupEvent::Closed)
    } else if submitted {
        Some(FindPopupEvent::Submitted)
    } else if changed {
        Some(FindPopupEvent::Changed)
    } else {
        None
    }
}
