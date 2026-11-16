use super::*;
use zcommon::hash::Blake3Hash;
use zdiff::{
    lexer::RawToken,
    nesting::{self, NestedScope},
};

pub(super) const MAX_STICKY_ROWS: usize = 5;
const MAX_CRUMB_CHARS: usize = 48;
const CRUMB_SEPARATOR: &str = "  >  ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BarLayout {
    Sticky,
    Breadcrumb,
}

impl BarLayout {
    pub(super) fn of(breadcrumb: bool) -> Self {
        if breadcrumb {
            Self::Breadcrumb
        } else {
            Self::Sticky
        }
    }

    fn rows(self, scopes: usize) -> usize {
        match self {
            Self::Sticky => scopes.min(MAX_STICKY_ROWS),
            Self::Breadcrumb => scopes.min(1),
        }
    }
}

#[derive(Default)]
pub(super) struct ScopeCache([Option<(Blake3Hash, Arc<[NestedScope]>)>; 2]);

impl ScopeCache {
    pub(super) fn scopes(
        &mut self,
        side: usize,
        file: &CachedFile<RawToken>,
    ) -> Arc<[NestedScope]> {
        let slot = &mut self.0[side];
        match slot {
            Some((hash, scopes)) if *hash == file.hash => scopes.clone(),
            _ => {
                let scopes: Arc<[NestedScope]> =
                    nesting::nested_scopes(&file.tokens, &file.contents).into();
                *slot = Some((file.hash, scopes.clone()));
                scopes
            }
        }
    }
}

fn side_line(row: &DiffRow, side: usize) -> Option<usize> {
    let content = if side == 0 { &row.left } else { &row.right };
    match content {
        LineContent::Code { line_num, .. } if *line_num > 0 => Some(*line_num as usize - 1),
        _ => None,
    }
}

pub(super) fn line_near(rows: &[DiffRow], side: usize, row: usize) -> Option<usize> {
    let row = row.min(rows.len());
    let below = rows[row..].iter().find_map(|row| side_line(row, side));
    below.or_else(|| {
        rows[..row]
            .iter()
            .rev()
            .find_map(|row| side_line(row, side))
    })
}

pub(super) fn bar_scopes(
    scopes: &[NestedScope],
    layout: BarLayout,
    line_below_bar_of_rows: impl Fn(usize) -> Option<usize>,
) -> Vec<&NestedScope> {
    let enclosing = |rows: usize| {
        line_below_bar_of_rows(rows).map_or_else(Vec::new, |line| nesting::enclosing(scopes, line))
    };
    if layout == BarLayout::Breadcrumb {
        return match enclosing(0).is_empty() {
            true => Vec::new(),
            false => enclosing(1),
        };
    }
    let mut chain = Vec::new();
    // The bar hides the rows it covers, so it grows while the line left showing is nested deeper.
    for rows in 0..=MAX_STICKY_ROWS {
        let mut held = enclosing(rows);
        if held.len() <= rows {
            break;
        }
        if rows == MAX_STICKY_ROWS {
            held.drain(..held.len() - MAX_STICKY_ROWS);
        } else {
            held.truncate(rows + 1);
        }
        chain = held;
    }
    chain
}

pub(super) fn shown_row(
    rows: &[DiffRow],
    line_rows: &[usize],
    side: usize,
    line: usize,
) -> Option<usize> {
    let row = *line_rows.get(line)?;
    (side_line(rows.get(row)?, side) == Some(line)).then_some(row)
}

pub(super) fn row_top(rows: &Rows<'_>, tops: &[f32], row: usize) -> f32 {
    match rows {
        Rows::Uniform { height, .. } => row as f32 * height,
        Rows::Heights(_) => tops[row],
    }
}

pub(super) fn jump_offset(
    scopes: &[NestedScope],
    header: usize,
    header_top: f32,
    layout: BarLayout,
    row_height: f32,
) -> f32 {
    let above = layout.rows(nesting::enclosing(scopes, header).len());
    (header_top - above as f32 * row_height).max(0.0)
}

fn crumb_text(header: &str) -> String {
    let text = header.trim().trim_end_matches(['{', ':']).trim_end();
    if text.chars().count() <= MAX_CRUMB_CHARS {
        return text.to_owned();
    }
    let head: String = text.chars().take(MAX_CRUMB_CHARS - 1).collect();
    format!("{head}…")
}

pub(super) struct BarLook {
    pub(super) row_height: f32,
    pub(super) font: egui::FontId,
}

pub(super) fn show(
    ui: &mut egui::Ui,
    id: egui::Id,
    rect: egui::Rect,
    chain: &[&NestedScope],
    text: &str,
    layout: BarLayout,
    look: &BarLook,
) -> Option<usize> {
    if chain.is_empty() {
        return None;
    }
    let rows = layout.rows(chain.len());
    let bar = egui::Rect::from_min_size(
        rect.min,
        egui::vec2(rect.width(), rows as f32 * look.row_height),
    )
    .intersect(rect);
    ui.interact(bar, id, egui::Sense::click());
    let painter = ui.painter_at(bar);
    let visuals = ui.visuals();
    painter.rect_filled(bar, 0.0, visuals.window_fill);
    let (text_color, weak_color) = (visuals.text_color(), visuals.weak_text_color());
    let hover_fill = visuals.widgets.hovered.weak_bg_fill;
    let border = visuals.widgets.noninteractive.bg_stroke;
    let pad = ui.spacing().item_spacing.x;
    let mut clicked = None;
    let mut clickable = |rect: egui::Rect, index: usize, scope: &NestedScope| {
        let response = ui
            .interact(rect, id.with(index), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(format!("Go to line {}", scope.header + 1));
        if response.hovered() {
            painter.rect_filled(rect, 0.0, hover_fill);
        }
        if response.clicked() {
            clicked = Some(scope.header);
        }
    };
    match layout {
        BarLayout::Sticky => {
            let digits = chain
                .iter()
                .map(|s| s.header + 1)
                .max()
                .unwrap_or(1)
                .to_string()
                .len();
            for (index, scope) in chain.iter().enumerate() {
                let row = egui::Rect::from_min_size(
                    bar.min + egui::vec2(0.0, index as f32 * look.row_height),
                    egui::vec2(bar.width(), look.row_height),
                );
                clickable(row, index, scope);
                let number = format!("{:>digits$}  ", scope.header + 1);
                let left = egui::pos2(row.left() + pad, row.center().y);
                let number_rect = painter.text(
                    left,
                    egui::Align2::LEFT_CENTER,
                    number,
                    look.font.clone(),
                    weak_color,
                );
                let header = text[scope.header_text.clone()].replace('\t', "    ");
                painter.text(
                    egui::pos2(number_rect.right(), row.center().y),
                    egui::Align2::LEFT_CENTER,
                    header,
                    look.font.clone(),
                    text_color,
                );
            }
        }
        BarLayout::Breadcrumb => {
            let mut x = bar.left() + pad;
            for (index, scope) in chain.iter().enumerate() {
                if index > 0 {
                    let separator = painter.text(
                        egui::pos2(x, bar.center().y),
                        egui::Align2::LEFT_CENTER,
                        CRUMB_SEPARATOR,
                        look.font.clone(),
                        weak_color,
                    );
                    x = separator.right();
                }
                let crumb = crumb_text(&text[scope.header_text.clone()]);
                let galley = painter.layout_no_wrap(crumb, look.font.clone(), text_color);
                let crumb_rect = egui::Rect::from_min_size(
                    egui::pos2(x, bar.top()),
                    egui::vec2(galley.size().x, look.row_height),
                );
                clickable(crumb_rect, index, scope);
                let top = bar.center().y - galley.size().y / 2.0;
                painter.galley(egui::pos2(x, top), galley, text_color);
                x = crumb_rect.right();
            }
        }
    }
    painter.hline(bar.x_range(), bar.bottom() - 0.5, border);
    clicked
}
