use std::ops::Range;

use eframe::egui;

use crate::path::PathSegment;

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
) -> Option<PathBarAction> {
    let id = ui.make_persistent_id(id_salt);
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    match ui.data(|d| d.get_temp::<EditState>(id)) {
        Some(state) => edit(ui, id, rect, state),
        None => display(ui, id, rect, segments, dim, clickable, error),
    }
}

fn edit(
    ui: &mut egui::Ui,
    id: egui::Id,
    rect: egui::Rect,
    mut state: EditState,
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
        return (response.lost_focus() && enter && !escape)
            .then_some(PathBarAction::Commit(state.text));
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
        Some(_) => egui::Stroke::new(1.0, visuals.error_fg_color),
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
    let font = egui::TextStyle::Button.resolve(ui.style());
    let text_width = |text: &str| {
        ui.painter()
            .layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
            .size()
            .x
    };
    let spacing = ui.spacing().item_spacing.x;
    let padding = ui.spacing().button_padding.x;
    let widths: Vec<f32> = segments
        .iter()
        .map(|s| text_width(&s.name) + 2.0 * padding)
        .collect();
    let edit_width = text_width("Edit") + 2.0 * padding;
    let available = rect.width() - 2.0 * padding - edit_width - spacing;
    let hidden = elide(
        &widths,
        text_width(SEPARATOR) + 2.0 * spacing,
        text_width(ELLIPSIS) + 2.0 * padding,
        available,
    );

    let mut action = None;
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(padding, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui
            .add(egui::Button::new("Edit").frame_when_inactive(false))
            .on_hover_text("Edit path")
            .clicked()
        {
            start_editing(ui, id, full_path);
        }
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            if segments.is_empty() {
                let text = egui::RichText::new("No folder").color(visuals.weak_text_color());
                ui.add(egui::Label::new(text).selectable(false));
            }
            for (i, segment) in segments.iter().enumerate() {
                if hidden.as_ref().is_some_and(|h| h.contains(&i)) {
                    if hidden.as_ref().is_some_and(|h| h.start == i) {
                        ui.label(SEPARATOR);
                        ui.add(egui::Label::new(ELLIPSIS).selectable(false))
                            .on_hover_text(full_path);
                    }
                    continue;
                }
                if i > 0 {
                    ui.label(SEPARATOR);
                }
                let text = egui::RichText::new(&segment.name).color(text_color(i));
                if clickable(i) {
                    if ui
                        .add(egui::Button::new(text).frame_when_inactive(false))
                        .on_hover_text(&segment.path)
                        .clicked()
                    {
                        action = Some(PathBarAction::Segment(i));
                    }
                } else {
                    ui.add(egui::Label::new(text).selectable(false));
                }
            }
        });
    });

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
    use super::elide;

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
