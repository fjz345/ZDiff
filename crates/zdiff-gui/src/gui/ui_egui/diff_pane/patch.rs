use std::ops::Range;

use super::*;
use crate::ui_egui::export::patch_text;
use zdiff::{diff_ir::DiffIR, lexer::RawToken};

const TEXT_MARGIN: f32 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PatchLineKind {
    Header,
    Hunk,
    Context,
    Delete,
    Insert,
    NoNewline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PatchLine {
    pub(super) kind: PatchLineKind,
    pub(super) text: Range<usize>,
}

pub(super) fn patch_lines(patch: &str) -> Vec<PatchLine> {
    let mut lines = Vec::new();
    let mut in_hunk = false;
    let mut start = 0;
    for line in patch.split_inclusive('\n') {
        let text = line.strip_suffix('\n').unwrap_or(line);
        // Only lines above the first hunk are file headers: a changed line may start with "--".
        let kind = match (text.as_bytes().first(), in_hunk) {
            (Some(b'@'), _) => PatchLineKind::Hunk,
            (_, false) => PatchLineKind::Header,
            (Some(b'-'), true) => PatchLineKind::Delete,
            (Some(b'+'), true) => PatchLineKind::Insert,
            (Some(b'\\'), true) => PatchLineKind::NoNewline,
            (_, true) => PatchLineKind::Context,
        };
        in_hunk |= kind == PatchLineKind::Hunk;
        lines.push(PatchLine {
            kind,
            text: start..start + text.len(),
        });
        start += line.len();
    }
    lines
}

struct Patch {
    text: String,
    lines: Vec<PatchLine>,
    hunks: usize,
}

pub(super) struct PatchView {
    // Kept alive so the pointer comparisons can't match a new allocation at the same address.
    ir: Arc<DiffIR>,
    files: [Option<Arc<CachedFile<RawToken>>>; 2],
    options: DiffBuilderOptions,
    patch: Option<Patch>,
    width: Option<(egui::FontId, f32, f32)>,
}

fn same_file(a: &Option<Arc<CachedFile<RawToken>>>, b: &Option<Arc<CachedFile<RawToken>>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

pub(super) fn patch_view<'a>(
    cache: &'a mut Option<PatchView>,
    diff_ctx: &MinimalDiffCtx,
) -> &'a mut PatchView {
    let files = [diff_ctx.input.file_1.clone(), diff_ctx.input.file_2.clone()];
    let fresh = cache.as_ref().is_some_and(|view| {
        Arc::ptr_eq(&view.ir, &diff_ctx.diff_ir)
            && same_file(&view.files[0], &files[0])
            && same_file(&view.files[1], &files[1])
            && !DiffBuilderOptions::need_invalidation(&view.options, &diff_ctx.input.options)
    });
    if !fresh {
        let patch = patch_text(diff_ctx).map(|text| {
            let lines = patch_lines(&text);
            let hunks = lines
                .iter()
                .filter(|line| line.kind == PatchLineKind::Hunk)
                .count();
            Patch { text, lines, hunks }
        });
        *cache = Some(PatchView {
            ir: diff_ctx.diff_ir.clone(),
            files,
            options: diff_ctx.input.options.clone(),
            patch,
            width: None,
        });
    }
    cache.as_mut().expect("the cache was just filled")
}

impl PatchView {
    fn width(&mut self, ui: &egui::Ui, font: &egui::FontId) -> f32 {
        let pixels_per_point = ui.ctx().pixels_per_point();
        if let Some((cached_font, cached_ppp, width)) = &self.width
            && cached_font == font
            && *cached_ppp == pixels_per_point
        {
            return *width;
        }
        let Some(patch) = &self.patch else {
            return 0.0;
        };
        let width = ui.fonts_mut(|fonts| {
            patch
                .lines
                .iter()
                .map(|line| {
                    patch.text[line.text.clone()]
                        .chars()
                        .map(|c| fonts.glyph_width(font, c))
                        .sum::<f32>()
                })
                .fold(0.0, f32::max)
        }) + 2.0 * TEXT_MARGIN;
        self.width = Some((font.clone(), pixels_per_point, width));
        width
    }

    pub(super) fn ui(&mut self, ui: &mut egui::Ui) {
        let _scope = zcommon_ui::frame_stats::scope("Patch view");
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let width = self.width(ui, &font);
        let patch = match &self.patch {
            Some(patch) if !patch.lines.is_empty() => patch,
            patch => {
                let message = match patch {
                    Some(_) => "The files have no differences.",
                    None => "A patch needs both files.",
                };
                ui.centered_and_justified(|ui| ui.label(message));
                return;
            }
        };
        ui.horizontal(|ui| {
            if ui.button("Copy patch").clicked() {
                ui.ctx().copy_text(patch.text.clone());
            }
            let hunks = match patch.hunks {
                1 => "1 hunk".to_owned(),
                n => format!("{n} hunks"),
            };
            ui.add(egui::Label::new(hunks).selectable(false));
        });
        let look = line_look(ui);
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
        egui::ScrollArea::both()
            .id_salt("patch_view")
            .auto_shrink(false)
            .show_rows(ui, row_height, patch.lines.len(), |ui, range| {
                let row_width = width.max(ui.available_width());
                for line in &patch.lines[range] {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(row_width, row_height),
                        egui::Sense::hover(),
                    );
                    let (bg, color) = look(line.kind);
                    if let Some(bg) = bg {
                        ui.painter().rect_filled(rect, 0.0, bg);
                    }
                    ui.painter().text(
                        rect.left_center() + egui::vec2(TEXT_MARGIN, 0.0),
                        egui::Align2::LEFT_CENTER,
                        &patch.text[line.text.clone()],
                        font.clone(),
                        color,
                    );
                }
            });
    }
}

fn line_look(
    ui: &egui::Ui,
) -> impl Fn(PatchLineKind) -> (Option<egui::Color32>, egui::Color32) + use<> {
    let ctx = ui.ctx();
    let (text, strong) = (ui.visuals().text_color(), ui.visuals().strong_text_color());
    let hunk_bg = role_color(ctx, FileRole::CollapsedRow);
    let no_newline = role_color(ctx, FileRole::DimmedText);
    let delete = (
        role_color(ctx, SharedRole::DeleteBackground),
        role_color(ctx, SharedRole::DeleteText),
    );
    let insert = (
        role_color(ctx, SharedRole::InsertBackground),
        role_color(ctx, SharedRole::InsertText),
    );
    move |kind| match kind {
        PatchLineKind::Header => (None, strong),
        PatchLineKind::Hunk => (Some(hunk_bg), strong),
        PatchLineKind::Context => (None, text),
        PatchLineKind::Delete => (Some(delete.0), delete.1),
        PatchLineKind::Insert => (Some(insert.0), insert.1),
        PatchLineKind::NoNewline => (None, no_newline),
    }
}

#[cfg(test)]
mod tests {
    use super::{PatchLineKind::*, *};

    fn kinds(patch: &str) -> Vec<(PatchLineKind, &str)> {
        patch_lines(patch)
            .into_iter()
            .map(|line| (line.kind, &patch[line.text]))
            .collect()
    }

    #[test]
    fn each_patch_line_is_classified_by_its_prefix() {
        let patch = "--- a.txt\n+++ b.txt\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+c\n\\ No newline at end of file\n";
        assert_eq!(
            kinds(patch),
            [
                (Header, "--- a.txt"),
                (Header, "+++ b.txt"),
                (Hunk, "@@ -1,2 +1,2 @@"),
                (Context, " a"),
                (Delete, "-b"),
                (NoNewline, "\\ No newline at end of file"),
                (Insert, "+c"),
                (NoNewline, "\\ No newline at end of file"),
            ]
        );
    }

    #[test]
    fn changed_lines_that_look_like_file_headers_are_changes() {
        let patch = "--- a.sql\n+++ b.sql\n@@ -1,2 +1,2 @@\n--- old\n+++ new\n @@ x\n";
        assert_eq!(
            kinds(patch),
            [
                (Header, "--- a.sql"),
                (Header, "+++ b.sql"),
                (Hunk, "@@ -1,2 +1,2 @@"),
                (Delete, "--- old"),
                (Insert, "+++ new"),
                (Context, " @@ x"),
            ]
        );
    }
}
