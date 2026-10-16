use super::*;

pub(super) const DEFAULT_CONTEXT_ROWS: usize = 2;

pub(super) enum Step {
    Dec,
    Inc,
    Reset,
}

pub(super) fn stepper(
    ui: &mut egui::Ui,
    value: String,
    dec_hint: &str,
    inc_hint: &str,
    reset_hint: &str,
) -> Option<Step> {
    let reset_click = |r: &egui::Response| r.secondary_clicked() || r.middle_clicked();
    let dec = ui.button("<").on_hover_text(dec_hint);
    let mid = ui.button(value).on_hover_text(format!(
        "{reset_hint}\n(click, or right/middle-click any arrow)"
    ));
    let inc = ui.button(">").on_hover_text(inc_hint);
    if mid.clicked() || reset_click(&dec) || reset_click(&mid) || reset_click(&inc) {
        Some(Step::Reset)
    } else if dec.clicked() {
        Some(Step::Dec)
    } else if inc.clicked() {
        Some(Step::Inc)
    } else {
        None
    }
}

