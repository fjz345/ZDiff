use eframe::egui;

pub const TOGGLE_BUTTON_SIZE: egui::Vec2 = egui::vec2(24.0, 24.0);

pub fn toggle_button(
    ui: &mut egui::Ui,
    value: &mut bool,
    label: impl Into<egui::WidgetText>,
    tooltip: &str,
) -> bool {
    let btn = egui::Button::new(label).selected(*value);
    if ui
        .add_sized(TOGGLE_BUTTON_SIZE, btn)
        .on_hover_text(tooltip)
        .clicked()
    {
        *value = !*value;
        return true;
    }
    false
}
