use eframe::egui;
use zcommon::style::GradientPreset;

use crate::style_window::{diff_gradient, gradient_color};

/// Floored, so a pair that differs reads 100 % only when measured so.
pub fn percent(ratio: f64) -> u32 {
    // The epsilon keeps 0.29 * 100 = 28.999... at 29.
    (ratio * 100.0 + 1e-9).floor().clamp(0.0, 100.0) as u32
}

pub fn likeness_bar(ui: &mut egui::Ui, ratio: f64, size: egui::Vec2) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    let mut filled = rect;
    filled.set_width(rect.width() * ratio.clamp(0.0, 1.0) as f32);
    painter.rect_filled(filled, 2.0, ratio_color(ui.ctx(), ratio));
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("{}%", percent(ratio)),
        egui::TextStyle::Monospace.resolve(ui.style()),
        ui.visuals().strong_text_color(),
    );
    response
}

pub fn ratio_color(ctx: &egui::Context, ratio: f64) -> egui::Color32 {
    likeness_color(ratio, diff_gradient(ctx))
}

fn likeness_color(ratio: f64, gradient: GradientPreset) -> egui::Color32 {
    gradient_color(gradient, 1.0 - ratio.clamp(0.0, 1.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::percent;

    #[test]
    fn percent_is_floored_but_exact_fractions_stay_whole() {
        assert_eq!(percent(0.997), 99);
        assert_eq!(percent(1.0), 100);
        assert_eq!(percent(0.29), 29);
        assert_eq!(percent(2.0 * 7.0 / 50.0), 28);
        assert_eq!(percent(0.0), 0);
    }
}
