use super::*;
use std::ops::RangeInclusive;

pub(super) const DEFAULT_CONTEXT_ROWS: usize = 2;
pub(super) const MAX_CONTEXT_ROWS: usize = 99;

#[derive(Debug, PartialEq)]
pub(super) enum Step {
    Dec,
    Inc,
    Reset,
}

pub(super) fn stepper(
    ui: &mut egui::Ui,
    value: usize,
    range: RangeInclusive<usize>,
    label: impl Fn(usize) -> String,
    dec_hint: &str,
    inc_hint: &str,
    reset_hint: &str,
) -> Option<Step> {
    let reset_click = |r: &egui::Response| r.secondary_clicked() || r.middle_clicked();
    let text_width = |v: usize| {
        egui::WidgetText::from(label(v))
            .into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Button,
            )
            .size()
            .x
    };
    // A value whose text widens must not move '>' under a pointer clicking it repeatedly.
    let widest_text = [*range.start(), *range.end(), value]
        .map(text_width)
        .into_iter()
        .fold(0.0, f32::max);
    let mid_width = widest_text + 2.0 * ui.spacing().button_padding.x;
    let dec = ui.button("<").on_hover_text(dec_hint);
    let mid = ui
        .add(egui::Button::new(label(value)).min_size(egui::vec2(mid_width, 0.0)))
        .on_hover_text(format!(
            "{reset_hint}\n(double-click, or right/middle-click any arrow)"
        ));
    let inc = ui.button(">").on_hover_text(inc_hint);
    if mid.double_clicked() || reset_click(&dec) || reset_click(&mid) || reset_click(&inc) {
        Some(Step::Reset)
    } else if dec.clicked() {
        Some(Step::Dec)
    } else if inc.clicked() {
        Some(Step::Inc)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, Pos2, Rect};

    struct Toolbar {
        ctx: egui::Context,
        time: f64,
        value: usize,
    }

    impl Toolbar {
        fn new(value: usize) -> Self {
            let mut toolbar = Self {
                ctx: egui::Context::default(),
                time: 0.0,
                value,
            };
            toolbar.frame(vec![]);
            toolbar
        }

        fn frame(&mut self, events: Vec<Event>) -> (Option<Step>, Vec<(String, Rect)>) {
            self.time += 0.1;
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 100.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let mut step = None;
            let output = self.ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        step = stepper(
                            ui,
                            self.value,
                            0..=MAX_CONTEXT_ROWS,
                            |v| v.to_string(),
                            "",
                            "",
                            "",
                        );
                    });
                });
            });
            let mut texts = Vec::new();
            output
                .shapes
                .iter()
                .for_each(|clipped| text_shapes(&clipped.shape, &mut texts));
            (step, texts)
        }

        fn click(&mut self, pos: Pos2) -> Option<Step> {
            let button = |pressed| Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            let (pressed, _) = self.frame(vec![Event::PointerMoved(pos), button(true)]);
            let (released, _) = self.frame(vec![button(false)]);
            pressed.or(released)
        }

        fn text_rect(&mut self, text: &str) -> Rect {
            let (_, texts) = self.frame(vec![]);
            texts
                .into_iter()
                .find_map(|(t, rect)| (t == text).then_some(rect))
                .unwrap_or_else(|| panic!("no {text:?} drawn"))
        }
    }

    fn text_shapes(shape: &egui::Shape, out: &mut Vec<(String, Rect)>) {
        match shape {
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| text_shapes(s, out)),
            egui::Shape::Text(text) => out.push((
                text.galley.text().to_owned(),
                text.galley.rect.translate(text.pos.to_vec2()),
            )),
            _ => {}
        }
    }

    #[test]
    fn clicking_inc_repeatedly_at_one_spot_never_resets() {
        let mut toolbar = Toolbar::new(7);
        let inc = toolbar.text_rect(">");
        for spot in [inc.left_center(), inc.center(), inc.right_center()] {
            toolbar.value = 7;
            for _ in 0..6 {
                assert_eq!(
                    toolbar.click(spot),
                    Some(Step::Inc),
                    "click at {spot:?} with value {}",
                    toolbar.value
                );
                toolbar.value += 1;
            }
        }
    }

    #[test]
    fn only_a_double_click_on_the_value_resets() {
        let mut toolbar = Toolbar::new(7);
        let value = toolbar.text_rect("7").center();
        assert_eq!(toolbar.click(value), None);
        assert_eq!(toolbar.click(value), Some(Step::Reset));
    }
}
