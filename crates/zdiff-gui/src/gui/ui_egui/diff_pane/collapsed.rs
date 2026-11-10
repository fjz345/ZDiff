use super::*;
use std::ops::RangeInclusive;

pub(super) const DEFAULT_CONTEXT_ROWS: usize = 2;
pub(super) const MAX_CONTEXT_ROWS: usize = 99;
const DRAG_SWEEP_PX: f32 = 300.0;
const MAX_DRAG_PX_PER_STEP: f32 = 8.0;

#[derive(Debug, PartialEq)]
pub(super) enum Step {
    Dec,
    Inc,
    Reset,
    Set(usize),
}

fn slid_value(start: usize, dx: f32, range: &RangeInclusive<usize>) -> usize {
    let span = range.end().saturating_sub(*range.start());
    let px_per_step = (DRAG_SWEEP_PX / span as f32).min(MAX_DRAG_PX_PER_STEP);
    let steps = (dx / px_per_step).round() as isize;
    start
        .saturating_add_signed(steps)
        .clamp(*range.start(), *range.end())
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
        .add(
            egui::Button::new(label(value))
                .min_size(egui::vec2(mid_width, 0.0))
                .sense(egui::Sense::click_and_drag()),
        )
        .on_hover_and_drag_cursor(egui::CursorIcon::ResizeHorizontal)
        .on_hover_text(format!(
            "Drag to pick a value\n{reset_hint}: double-click, or right/middle-click any arrow"
        ));
    let inc = ui.button(">").on_hover_text(inc_hint);
    if mid.drag_started() {
        ui.data_mut(|d| d.insert_temp(mid.id, value));
    }
    let slid = mid
        .dragged()
        .then(|| {
            let start = ui.data(|d| d.get_temp(mid.id)).unwrap_or(value);
            let origin = ui.input(|i| i.pointer.press_origin());
            let dx = mid
                .interact_pointer_pos()
                .zip(origin)
                .map_or(0.0, |(pos, origin)| pos.x - origin.x);
            slid_value(start, dx, &range)
        })
        .filter(|&slid| slid != value);
    if mid.double_clicked() || reset_click(&dec) || reset_click(&mid) || reset_click(&inc) {
        Some(Step::Reset)
    } else if let Some(slid) = slid {
        Some(Step::Set(slid))
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

        fn drag(&mut self, from: Pos2, to: Pos2) -> Vec<Step> {
            let button = |pos, pressed| Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            let mut events = vec![vec![Event::PointerMoved(from), button(from, true)]];
            events.extend(
                (1..=10).map(|i| vec![Event::PointerMoved(from.lerp(to, i as f32 / 10.0))]),
            );
            events.push(vec![button(to, false)]);
            let mut steps = Vec::new();
            for frame_events in events {
                if let (Some(step), _) = self.frame(frame_events) {
                    if let Step::Set(value) = step {
                        self.value = value;
                    }
                    steps.push(step);
                }
            }
            steps
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

    #[test]
    fn dragging_the_value_sets_it_from_the_drag_distance() {
        for (dx, expected) in [
            (60.0, slid_value(7, 60.0, &(0..=MAX_CONTEXT_ROWS))),
            (-200.0, 0),
        ] {
            let mut toolbar = Toolbar::new(7);
            let origin = toolbar.text_rect("7").center();
            let steps = toolbar.drag(origin, origin + egui::vec2(dx, 0.0));
            assert!(
                steps.iter().all(|step| matches!(step, Step::Set(_))),
                "{steps:?}"
            );
            assert_eq!(toolbar.value, expected, "drag by {dx}");
        }
    }

    #[test]
    fn slid_value_moves_by_whole_steps_within_the_range() {
        let small = 0..=10;
        assert_eq!(slid_value(5, 0.0, &small), 5);
        assert_eq!(slid_value(5, 3.9, &small), 5);
        assert_eq!(slid_value(5, 4.1, &small), 6);
        assert_eq!(slid_value(5, 16.0, &small), 7);
        assert_eq!(slid_value(5, -8.0, &small), 4);
        assert_eq!(slid_value(9, 100.0, &small), 10);
        assert_eq!(slid_value(1, -100.0, &small), 0);
        assert_eq!(slid_value(0, 50.0, &(0..=0)), 0);
        assert_eq!(slid_value(0, 300.0, &(0..=3000)), 3000);
        assert_eq!(slid_value(3000, -150.0, &(0..=3000)), 1500);
        assert_eq!(slid_value(40, -10.0, &(0..=20)), 20);
    }
}
