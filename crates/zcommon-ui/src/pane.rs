use eframe::egui::{
    self,
    containers::menu::{MenuButton, MenuConfig},
};

const TOOLBAR_SPACING: f32 = 4.0;
const STATE_TOP_FRACTION: f32 = 0.25;
const STATE_GAP: f32 = 8.0;
const LOADING_SPINNER_SIZE: f32 = 64.0;

pub trait ZAppPane {
    fn title(&self) -> String {
        "Pane".to_string()
    }
}

pub fn toolbar<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = TOOLBAR_SPACING;
        add_contents(ui)
    })
}

pub fn toolbar_group<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = TOOLBAR_SPACING;
        add_contents(ui)
    })
}

pub fn options_menu_button(
    ui: &mut egui::Ui,
    tooltip: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    MenuButton::new("⏷")
        .config(MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside))
        .ui(ui, add_contents)
        .0
        .on_hover_text(tooltip)
}

/// Fills the rest of `ui`, so the whole pane stays a drop target.
fn pane_state<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let size = ui.available_size();
    ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Center), |ui| {
        ui.set_min_size(size);
        ui.add_space(size.y * STATE_TOP_FRACTION);
        add_contents(ui)
    })
}

pub fn empty_state(
    ui: &mut egui::Ui,
    heading: &str,
    hint: &str,
    actions: &[&str],
) -> (egui::Response, Option<usize>) {
    let state = pane_state(ui, |ui| {
        ui.heading(heading);
        ui.label(hint);
        ui.add_space(STATE_GAP);
        let galleys: Vec<_> = actions
            .iter()
            .map(|text| {
                egui::WidgetText::from(*text).into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::TextStyle::Button,
                )
            })
            .collect();
        let spacing = ui.spacing().item_spacing.x;
        let padding = 2.0 * ui.spacing().button_padding.x;
        let width = galleys.iter().map(|g| g.size().x + padding).sum::<f32>()
            + spacing * galleys.len().saturating_sub(1) as f32;
        let row = egui::vec2(width, ui.spacing().interact_size.y);
        ui.allocate_ui_with_layout(
            row,
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                let mut clicked = None;
                for (i, galley) in galleys.into_iter().enumerate() {
                    if ui.button(egui::WidgetText::from(galley)).clicked() {
                        clicked = Some(i);
                    }
                }
                clicked
            },
        )
        .inner
    });
    (state.response, state.inner)
}

pub fn loading_state(ui: &mut egui::Ui, label: &str) -> egui::Response {
    pane_state(ui, |ui| {
        ui.label(label);
        ui.add_space(STATE_GAP);
        ui.add(egui::Spinner::new().size(LOADING_SPINNER_SIZE));
    })
    .response
}

pub fn error_state(ui: &mut egui::Ui, error: &str) -> egui::Response {
    pane_state(ui, |ui| {
        ui.colored_label(ui.visuals().error_fg_color, error);
    })
    .response
}

#[cfg(test)]
mod tests {
    use eframe::egui::{self, Pos2, Rect};

    use super::{empty_state, error_state, loading_state};

    #[test]
    fn every_pane_state_fills_the_rest_of_the_pane() {
        let ctx = egui::Context::default();
        let states: [fn(&mut egui::Ui) -> egui::Response; 3] = [
            |ui| empty_state(ui, "Heading", "Hint", &["Open Left", "Open Right"]).0,
            |ui| loading_state(ui, "Loading"),
            |ui| error_state(ui, "Error"),
        ];
        for state in states {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 300.0))),
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.label("Toolbar");
                    let rest = ui.available_rect_before_wrap();
                    assert_eq!(state(ui).rect, rest);
                });
            });
        }
    }
}
