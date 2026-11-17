use super::*;
use crate::diff_ctx::DiffSpan;
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

pub(super) fn block_scopes<'a>(
    scopes: &'a [NestedScope],
    rows: &[DiffRow],
    changes: &[DiffSpan],
    block_end: usize,
    side: usize,
) -> Vec<&'a NestedScope> {
    let below = changes.partition_point(|change| *change.rows().end() < block_end);
    let Some(change) = changes.get(below) else {
        return Vec::new();
    };
    let row = (*change.rows().start()).max(block_end);
    let Some(line) = line_near(rows, side, row) else {
        return Vec::new();
    };
    let held = nesting::enclosing(scopes, line);
    if line == 0 || side_line(&rows[row], side).is_some() {
        return held;
    }
    // This side has no line in the change: it lands between two lines, in the deeper scope.
    let above = nesting::enclosing(scopes, line - 1);
    if above.len() > held.len() { above } else { held }
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

fn crumb_clicked(
    ui: &egui::Ui,
    painter: &egui::Painter,
    rect: egui::Rect,
    id: egui::Id,
    scope: &NestedScope,
) -> bool {
    let response = ui
        .interact(rect, id, egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!("Go to line {}", scope.header + 1));
    if response.hovered() {
        painter.rect_filled(rect, 0.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    response.clicked()
}

pub(super) fn show_crumbs(
    ui: &egui::Ui,
    painter: &egui::Painter,
    id: egui::Id,
    row: egui::Rect,
    chain: &[&NestedScope],
    text: &str,
    font: &egui::FontId,
) -> Option<usize> {
    let visuals = ui.visuals();
    let (text_color, weak_color) = (visuals.text_color(), visuals.weak_text_color());
    let mut clicked = None;
    let mut x = row.left();
    for (index, scope) in chain.iter().enumerate() {
        if index > 0 {
            let separator = painter.text(
                egui::pos2(x, row.center().y),
                egui::Align2::LEFT_CENTER,
                CRUMB_SEPARATOR,
                font.clone(),
                weak_color,
            );
            x = separator.right();
        }
        let crumb = crumb_text(&text[scope.header_text.clone()]);
        let galley = painter.layout_no_wrap(crumb, font.clone(), text_color);
        let crumb_rect = egui::Rect::from_min_size(
            egui::pos2(x, row.top()),
            egui::vec2(galley.size().x, row.height()),
        );
        if crumb_clicked(ui, painter, crumb_rect, id.with(index), scope) {
            clicked = Some(scope.header);
        }
        let top = row.center().y - galley.size().y / 2.0;
        painter.galley(egui::pos2(x, top), galley, text_color);
        x = crumb_rect.right();
    }
    clicked
}

pub(super) fn request_jump(
    ui: &egui::Ui,
    jump_id: egui::Id,
    diff_ctx: &MinimalDiffCtx,
    side: usize,
    line: usize,
    reveal_line_request: &mut Option<(ActiveSide, usize)>,
) {
    let file_rows = &diff_ctx.precomputed_file_rows;
    let line_rows = [&file_rows.0, &file_rows.1][side];
    if shown_row(&diff_ctx.diff_rows, line_rows, side, line).is_none() {
        let active = [ActiveSide::Left, ActiveSide::Right][side];
        *reveal_line_request = Some((active, line));
    }
    ui.data_mut(|d| d.insert_temp(jump_id, (side, line)));
    ui.ctx().request_repaint();
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
    let border = visuals.widgets.noninteractive.bg_stroke;
    let pad = ui.spacing().item_spacing.x;
    let mut clicked = None;
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
                if crumb_clicked(ui, &painter, row, id.with(index), scope) {
                    clicked = Some(scope.header);
                }
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
            let row = bar.with_min_x(bar.left() + pad);
            clicked = show_crumbs(ui, &painter, id, row, chain, text, &look.font);
        }
    }
    painter.hline(bar.x_range(), bar.bottom() - 0.5, border);
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_ctx::finalize_diff_rows;
    use crate::ui_egui::copy_harness::CopyHarness;
    use zdiff::diff_builder::DiffBuilderOptions;

    const TWO_SCOPES: &str = "fn first() {\n    a1\n    a2\n    a3\n    a4\n    a5\n    keep_first\n    a7\n}\nfn second() {\n    if cond {\n        b1\n        b2\n        b3\n        b4\n        keep_second\n    }\n}\ntail1\ntail2\ntail3\ntail4\n";

    fn block_chains(source: &str, target: &str) -> Vec<[Vec<usize>; 2]> {
        let options = DiffBuilderOptions {
            diff_only_with_extra_rows: Some(1),
            ..Default::default()
        };
        let harness = CopyHarness::new(source, target, &options);
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (rows, changes, _) =
            finalize_diff_rows(harness.rows().to_vec(), &options, &cancel).unwrap();
        let scopes = harness
            .files()
            .map(|file| nesting::nested_scopes(&file.tokens, &file.contents));
        (0..rows.len())
            .filter(|&row| matches!(rows[row].left, LineContent::Collapsed))
            .map(|row| {
                [0, 1].map(|side| {
                    block_scopes(&scopes[side], &rows, &changes, row + 1, side)
                        .iter()
                        .map(|scope| scope.header)
                        .collect()
                })
            })
            .collect()
    }

    #[test]
    fn two_changes_in_different_scopes_give_two_different_chains() {
        let target = TWO_SCOPES
            .replace("keep_first", "edit_first")
            .replace("keep_second", "edit_second");

        let chains = block_chains(TWO_SCOPES, &target);

        assert_eq!(chains[0], [vec![0], vec![0]]);
        assert_eq!(chains[1], [vec![9, 10], vec![9, 10]]);
    }

    #[test]
    fn a_block_with_no_change_below_has_no_chain() {
        let target = TWO_SCOPES.replace("keep_second", "edit_second");

        let chains = block_chains(TWO_SCOPES, &target);

        assert_eq!(chains.len(), 2);
        assert!(chains[1].iter().all(Vec::is_empty));
    }

    #[test]
    fn a_change_outside_every_scope_has_no_chain() {
        let source = "top1\ntop2\ntop3\ntop4\ntop5\nkeep\nfn later() {\n    body\n}\n";
        let target = source.replace("keep", "edit");

        let chains = block_chains(source, &target);

        assert!(chains[0].iter().all(Vec::is_empty));
    }

    #[test]
    fn the_side_an_insertion_leaves_empty_gets_the_scope_it_lands_in() {
        let appended = TWO_SCOPES.replace("    a7\n", "    a7\n    added\n");
        let prepended = TWO_SCOPES.replace("    if cond {\n", "    if cond {\n        added\n");

        assert_eq!(block_chains(TWO_SCOPES, &appended)[0], [vec![0], vec![0]]);
        assert_eq!(
            block_chains(TWO_SCOPES, &prepended)[0],
            [vec![9, 10], vec![9, 10]]
        );
    }
}
