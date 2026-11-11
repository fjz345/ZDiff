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
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = TOOLBAR_SPACING;
        add_contents(ui)
    })
}

pub fn toolbar_group<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    wrapping_group(
        ui,
        egui::Layout::left_to_right(egui::Align::Center),
        add_contents,
    )
}

pub fn toolbar_end<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    wrapping_group(
        ui,
        egui::Layout::right_to_left(egui::Align::Center),
        add_contents,
    )
}

// A group is laid out after it is placed, so last frame's size decides whether it fits.
fn wrapping_group<R>(
    ui: &mut egui::Ui,
    layout: egui::Layout,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let id = ui.next_auto_id();
    let last_size = ui.data(|d| d.get_temp::<egui::Vec2>(id));
    // A wrapping layout reports a whole row as available, not what is left of the current one.
    let rest_of_row = |ui: &egui::Ui| (ui.max_rect().right() - ui.cursor().min.x).max(0.0);
    let row_started = ui.cursor().min.x > ui.max_rect().min.x;
    if row_started && last_size.is_some_and(|size| size.x > rest_of_row(ui)) {
        ui.end_row();
    }
    let height = last_size.map_or(0.0, |size| size.y);
    let size = egui::vec2(rest_of_row(ui), height.max(ui.spacing().interact_size.y));
    let group = ui.allocate_ui_with_layout(size, layout, |ui| {
        ui.spacing_mut().item_spacing.x = TOOLBAR_SPACING;
        add_contents(ui)
    });
    let measured = group.response.rect.size();
    if last_size != Some(measured) {
        ui.data_mut(|d| d.insert_temp(id, measured));
        ui.ctx().request_repaint();
    }
    group
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
        ui.set_style(ui.ctx().style());
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
