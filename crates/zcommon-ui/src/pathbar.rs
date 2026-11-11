use std::ops::Range;

use eframe::egui;

use zcommon::path::PathSegment;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathBarAction {
    Segment(usize),
    Commit(String),
}

/// Segments hidden behind "…" so `widths` fit in `available`, keeping the first and last two.
pub fn elide(
    widths: &[f32],
    separator: f32,
    ellipsis: f32,
    available: f32,
) -> Option<Range<usize>> {
    let n = widths.len();
    let full: f32 = widths.iter().sum::<f32>() + separator * n.saturating_sub(1) as f32;
    if n <= 3 || full <= available {
        return None;
    }
    let width_hiding = |hidden: &Range<usize>| {
        let shown = n - hidden.len();
        full - widths[hidden.clone()].iter().sum::<f32>() + ellipsis
            - separator * (n - 1 - shown) as f32
    };
    (2..n - 2)
        .map(|end| 1..end)
        .find(|hidden| width_hiding(hidden) <= available)
        .or(Some(1..n - 2))
}

/// Width limits that make `widths` fit in `available`: the widest items share one cap, narrower
/// ones keep theirs.
fn cap_widths(widths: &[f32], available: f32) -> Vec<f32> {
    let available = available.max(0.0);
    if widths.iter().sum::<f32>() <= available {
        return widths.to_vec();
    }
    let mut sorted = widths.to_vec();
    sorted.sort_by(f32::total_cmp);
    let mut remaining = available;
    let mut cap = 0.0;
    for (i, &width) in sorted.iter().enumerate() {
        let even = remaining / (sorted.len() - i) as f32;
        if width > even {
            cap = even;
            break;
        }
        remaining -= width;
    }
    widths.iter().map(|&width| width.min(cap)).collect()
}

#[derive(Clone)]
struct EditState {
    text: String,
    request_focus: bool,
}

/// `error` draws the bar red with the reason on hover; the caller decides what's invalid.
pub fn path_bar(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    segments: &[PathSegment],
    dim: usize,
    clickable: impl Fn(usize) -> bool,
    error: Option<&str>,
    commit_on_focus_loss: bool,
) -> Option<PathBarAction> {
    let id = ui.make_persistent_id(id_salt);
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    match ui.data(|d| d.get_temp::<EditState>(id)) {
        Some(state) => edit(ui, id, rect, state, commit_on_focus_loss),
        None => display(ui, id, rect, segments, dim, clickable, error),
    }
}

pub fn paint_busy(ui: &egui::Ui, bar: egui::Rect) {
    let size = bar.height() * 0.5;
    let center = egui::pos2(bar.right() - size, bar.top() + size);
    egui::Spinner::new().paint_at(
        ui,
        egui::Rect::from_center_size(center, egui::Vec2::splat(size)),
    );
}

fn edit(
    ui: &mut egui::Ui,
    id: egui::Id,
    rect: egui::Rect,
    mut state: EditState,
    commit_on_focus_loss: bool,
) -> Option<PathBarAction> {
    let text_id = id.with("text");
    if state.request_focus {
        let mut text_state = egui::TextEdit::load_state(ui.ctx(), text_id).unwrap_or_default();
        let all = egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(state.text.chars().count()),
        );
        text_state.cursor.set_char_range(Some(all));
        text_state.store(ui.ctx(), text_id);
    }
    let response = ui.put(
        rect,
        egui::TextEdit::singleline(&mut state.text)
            .id(text_id)
            .desired_width(rect.width()),
    );
    if std::mem::take(&mut state.request_focus) {
        response.request_focus();
    } else if !response.has_focus() {
        ui.data_mut(|d| d.remove::<EditState>(id));
        let (enter, escape) = ui.input_mut(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        let commit = (response.lost_focus() && enter) || commit_on_focus_loss;
        return (commit && !escape).then_some(PathBarAction::Commit(state.text));
    }
    ui.data_mut(|d| d.insert_temp(id, state));
    None
}

fn display(
    ui: &mut egui::Ui,
    id: egui::Id,
    rect: egui::Rect,
    segments: &[PathSegment],
    dim: usize,
    clickable: impl Fn(usize) -> bool,
    error: Option<&str>,
) -> Option<PathBarAction> {
    let full_path = segments.last().map_or("", |s| s.path.as_str());
    let background = ui.interact(rect, id.with("background"), egui::Sense::click());
    let visuals = ui.visuals().clone();
    let stroke = match error {
        Some(_) => egui::Stroke::new(1.0_f32, visuals.error_fg_color),
        None => visuals.widgets.inactive.bg_stroke,
    };
    ui.painter().rect(
        rect,
        visuals.widgets.inactive.corner_radius,
        visuals.text_edit_bg_color(),
        stroke,
        egui::StrokeKind::Inside,
    );
    if let Some(reason) = error
        && ui.rect_contains_pointer(rect)
    {
        background.show_tooltip_text(reason);
    }

    let text_color = |i: usize| match error {
        Some(_) => visuals.error_fg_color,
        None if i < dim => visuals.weak_text_color(),
        None => visuals.text_color(),
    };
    let text_width = |text: &str, style: egui::TextStyle| {
        let style = ui.style().override_text_style.clone().unwrap_or(style);
        ui.painter()
            .layout_no_wrap(
                text.to_owned(),
                style.resolve(ui.style()),
                egui::Color32::PLACEHOLDER,
            )
            .size()
            .x
    };
    let spacing = ui.spacing().item_spacing.x;
    let padding = ui.spacing().button_padding.x;
    let widths: Vec<f32> = segments
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if clickable(i) {
                text_width(&s.name, egui::TextStyle::Button) + 2.0 * padding
            } else {
                text_width(&s.name, egui::TextStyle::Body)
            }
        })
        .collect();
    let edit_width = text_width("Edit", egui::TextStyle::Button) + 2.0 * padding;
    let inner = rect.shrink2(egui::vec2(padding, 0.0));
    let edit_rect = egui::Rect::from_min_max(
        egui::pos2(inner.right() - edit_width, inner.top()),
        inner.max,
    );
    let area = egui::Rect::from_min_max(
        inner.min,
        egui::pos2(edit_rect.left() - spacing, inner.bottom()),
    );
    let separator_width = text_width(SEPARATOR, egui::TextStyle::Body) + 2.0 * spacing;
    let ellipsis_width = text_width(ELLIPSIS, egui::TextStyle::Body);
    let hidden = elide(&widths, separator_width, ellipsis_width, area.width());

    let shown: Vec<usize> = (0..segments.len())
        .filter(|i| !hidden.as_ref().is_some_and(|h| h.contains(i)))
        .collect();
    let items = shown.len() + usize::from(hidden.is_some());
    let fixed = separator_width * items.saturating_sub(1) as f32
        + if hidden.is_some() {
            ellipsis_width
        } else {
            0.0
        };
    let shown_widths: Vec<f32> = shown.iter().map(|&i| widths[i]).collect();
    let caps = cap_widths(&shown_widths, area.width() - fixed);

    let mut action = None;
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(area)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.set_clip_rect(area.intersect(ui.clip_rect()));
    child.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
    if segments.is_empty() {
        let text = egui::RichText::new("No folder").color(visuals.weak_text_color());
        child.add(egui::Label::new(text).selectable(false));
    }
    for (i, segment) in segments.iter().enumerate() {
        if hidden.as_ref().is_some_and(|h| h.contains(&i)) {
            if hidden.as_ref().is_some_and(|h| h.start == i) {
                child.add(egui::Label::new(SEPARATOR).selectable(false));
                child
                    .add(egui::Label::new(ELLIPSIS).selectable(false))
                    .on_hover_text(full_path);
            }
            continue;
        }
        if i > 0 {
            child.add(egui::Label::new(SEPARATOR).selectable(false));
        }
        let text = egui::RichText::new(&segment.name).color(text_color(i));
        let slot = shown
            .iter()
            .position(|&s| s == i)
            .expect("segment is shown");
        let cut = caps[slot] < widths[i];
        let size = egui::vec2(caps[slot], rect.height());
        if clickable(i) {
            let button = egui::Button::new(text).frame_when_inactive(false);
            let response = if cut {
                child.add_sized(size, button.truncate())
            } else {
                child.add(button)
            };
            if response.on_hover_text(&segment.path).clicked() {
                action = Some(PathBarAction::Segment(i));
            }
        } else if cut {
            child
                .add_sized(size, egui::Label::new(text).selectable(false).truncate())
                .on_hover_text(&segment.path);
        } else {
            child.add(egui::Label::new(text).selectable(false));
        }
    }

    let mut edit_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(edit_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    if edit_ui
        .add(egui::Button::new("Edit").frame_when_inactive(false))
        .on_hover_text("Edit path")
        .clicked()
    {
        start_editing(ui, id, full_path);
    }

    if background.clicked() {
        start_editing(ui, id, full_path);
    }
    action
}

fn start_editing(ui: &egui::Ui, id: egui::Id, path: &str) {
    let state = EditState {
        text: path.to_owned(),
        request_focus: true,
    };
    ui.data_mut(|d| d.insert_temp(id, state));
}

const SEPARATOR: &str = "›";
const ELLIPSIS: &str = "…";

#[cfg(test)]
mod tests {
    use super::{PathBarAction, cap_widths, elide, path_bar};
    use zcommon::path::split_path;
    use eframe::egui::{self, Event, Modifiers, PointerButton, Pos2, Rect};

    const PATH: &str = r"C:\proj\src\main.rs";

    struct Bar {
        ctx: egui::Context,
        commit_on_focus_loss: bool,
        time: f64,
    }

    impl Bar {
        fn editing(commit_on_focus_loss: bool) -> Bar {
            let mut bar = Bar {
                ctx: egui::Context::default(),
                commit_on_focus_loss,
                time: 0.0,
            };
            bar.frame(vec![]);
            bar.click(egui::pos2(200.0, 18.0));
            bar.frame(vec![]);
            bar.frame(vec![]);
            bar
        }

        fn frame(&mut self, events: Vec<Event>) -> Option<PathBarAction> {
            self.time += 0.1;
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 200.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let mut action = None;
            let segments = split_path(PATH);
            let _ = self.ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    action = path_bar(
                        ui,
                        "bar",
                        &segments,
                        0,
                        |_| true,
                        None,
                        self.commit_on_focus_loss,
                    );
                });
            });
            action
        }

        fn click(&mut self, pos: Pos2) -> Option<PathBarAction> {
            let button = |pressed| Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            let pressed = self.frame(vec![Event::PointerMoved(pos), button(true)]);
            pressed.or(self.frame(vec![button(false)]))
        }

        fn key(&mut self, key: egui::Key) -> Option<PathBarAction> {
            self.frame(vec![Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }])
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

    fn displayed_text(
        path: &str,
        width: f32,
        style: Option<egui::TextStyle>,
    ) -> (Vec<(String, Rect)>, Rect) {
        let ctx = egui::Context::default();
        let segments = split_path(path);
        let mut bar = Rect::NOTHING;
        let mut run = |ctx: &egui::Context, events: Vec<Event>| {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 200.0))),
                events,
                ..Default::default()
            };
            ctx.run(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.style_mut().override_text_style = style.clone();
                    bar = ui.available_rect_before_wrap();
                    path_bar(
                        ui,
                        "bar",
                        &segments,
                        0,
                        |i| i + 1 < segments.len(),
                        None,
                        false,
                    );
                });
            })
        };
        run(&ctx, vec![]);
        let output = run(&ctx, vec![]);
        let mut texts = Vec::new();
        output
            .shapes
            .iter()
            .for_each(|clipped| text_shapes(&clipped.shape, &mut texts));
        texts.retain(|(_, rect)| bar.y_range().intersects(rect.y_range()));
        (texts, bar)
    }

    fn assert_text_fits_left_of_edit(path: &str, width: f32) {
        for style in [None, Some(egui::TextStyle::Monospace)] {
            assert_text_fits_left_of_edit_in(path, width, style);
        }
    }

    fn assert_text_fits_left_of_edit_in(path: &str, width: f32, style: Option<egui::TextStyle>) {
        let (texts, bar) = displayed_text(path, width, style.clone());
        let padding = egui::Style::default().spacing.button_padding.x;
        let edit = texts
            .iter()
            .find(|(text, _)| text == "Edit")
            .expect("the Edit button is drawn")
            .1;
        for (text, rect) in texts.iter().filter(|(text, _)| text != "Edit") {
            assert!(
                rect.right() <= edit.left() - padding,
                "{style:?} {text:?} ends at {} under the Edit button starting at {}",
                rect.right(),
                edit.left() - padding
            );
        }
        assert!(
            edit.right() + padding <= bar.right(),
            "{style:?} Edit button ends at {} past the bar at {}",
            edit.right() + padding,
            bar.right()
        );
    }

    #[test]
    fn long_paths_never_reach_the_edit_button() {
        assert_text_fits_left_of_edit(
            r"C:\a_very_long_directory_name\another_extremely_long_directory\yet_another_long_one\some_really_long_file_name_here.rs",
            300.0,
        );
    }

    #[test]
    fn few_long_segments_never_reach_the_edit_button() {
        assert_text_fits_left_of_edit(
            r"C:\an_extremely_long_directory_name_that_goes_on\and_a_very_long_file_name_too.rs",
            250.0,
        );
    }

    #[test]
    fn short_paths_are_shown_whole() {
        let (texts, _) = displayed_text(PATH, 400.0, None);
        for segment in split_path(PATH) {
            let name = segment.name.as_str();
            assert!(texts.iter().any(|(text, _)| text == name), "{name}");
        }
    }

    #[test]
    fn click_elsewhere_commits_when_asked() {
        let mut bar = Bar::editing(true);
        assert_eq!(
            bar.click(egui::pos2(200.0, 150.0)),
            Some(PathBarAction::Commit(PATH.to_owned()))
        );
    }

    #[test]
    fn click_elsewhere_cancels_by_default() {
        let mut bar = Bar::editing(false);
        assert_eq!(bar.click(egui::pos2(200.0, 150.0)), None);
    }

    #[test]
    fn enter_commits_and_escape_cancels_either_way() {
        for commit_on_focus_loss in [false, true] {
            let mut bar = Bar::editing(commit_on_focus_loss);
            assert_eq!(
                bar.key(egui::Key::Enter),
                Some(PathBarAction::Commit(PATH.to_owned()))
            );
            let mut bar = Bar::editing(commit_on_focus_loss);
            assert_eq!(bar.key(egui::Key::Escape), None);
            assert_eq!(bar.click(egui::pos2(200.0, 150.0)), None);
        }
    }

    #[test]
    fn widths_that_fit_are_not_capped() {
        assert_eq!(cap_widths(&[10.0, 100.0, 30.0], 140.0), [10.0, 100.0, 30.0]);
    }

    #[test]
    fn only_the_widest_widths_are_capped() {
        assert_eq!(cap_widths(&[10.0, 100.0, 30.0], 100.0), [10.0, 60.0, 30.0]);
        assert_eq!(cap_widths(&[50.0, 50.0], 60.0), [30.0, 30.0]);
    }

    #[test]
    fn no_room_caps_everything_to_nothing() {
        assert_eq!(cap_widths(&[10.0, 20.0], -5.0), [0.0, 0.0]);
    }

    #[test]
    fn fitting_segments_are_not_elided() {
        assert_eq!(elide(&[10.0; 5], 2.0, 4.0, 58.0), None);
    }

    #[test]
    fn three_or_fewer_segments_are_never_elided() {
        assert_eq!(elide(&[100.0; 3], 2.0, 4.0, 10.0), None);
        assert_eq!(elide(&[100.0], 2.0, 4.0, 10.0), None);
        assert_eq!(elide(&[], 2.0, 4.0, 10.0), None);
    }

    #[test]
    fn hides_the_fewest_segments_after_the_first() {
        // Full: 50 + 4 * 2 = 58. Hiding 1..3 gives 10 + 4 + 10 + 10 + 3 * 2 = 40.
        assert_eq!(elide(&[10.0; 5], 2.0, 4.0, 57.0), Some(1..2));
        assert_eq!(elide(&[10.0; 5], 2.0, 4.0, 40.0), Some(1..3));
    }

    #[test]
    fn too_narrow_keeps_first_and_last_two() {
        assert_eq!(elide(&[10.0; 6], 2.0, 4.0, 1.0), Some(1..4));
    }
}
