use eframe::egui;

pub const TOGGLE_BUTTON_SIZE: egui::Vec2 = egui::vec2(24.0, 24.0);

pub fn toggle_button(
    ui: &mut egui::Ui,
    value: &mut bool,
    label: impl Into<egui::WidgetText>,
    tooltip: &str,
) -> bool {
    let label = label.into().into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Button,
    );
    let width = label.size().x + 2.0 * ui.spacing().button_padding.x;
    let size = egui::vec2(width.max(TOGGLE_BUTTON_SIZE.x), TOGGLE_BUTTON_SIZE.y);
    let btn = egui::Button::new(egui::WidgetText::from(label)).selected(*value);
    if ui.add_sized(size, btn).on_hover_text(tooltip).clicked() {
        *value = !*value;
        return true;
    }
    false
}
