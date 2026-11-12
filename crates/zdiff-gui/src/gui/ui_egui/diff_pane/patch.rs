use std::ops::Range;
use std::sync::Weak;

use super::*;
use crate::{diff_ctx::STAGE_CACHE_ENTRIES, ui_egui::export::patch_text_of};
use zcommon::job::Job;
use zcommon_ui::h_scroll;
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

// A `Weak` keeps its allocation, so a new IR or file never takes a dropped one's address.
#[derive(Clone)]
struct PatchKey {
    ir: Weak<DiffIR>,
    files: [Option<Weak<CachedFile<RawToken>>>; 2],
    options: DiffBuilderOptions,
}

impl PatchKey {
    fn of(diff_ctx: &MinimalDiffCtx) -> Self {
        Self {
            ir: Arc::downgrade(&diff_ctx.diff_ir),
            files: [&diff_ctx.input.file_1, &diff_ctx.input.file_2]
                .map(|file| file.as_ref().map(Arc::downgrade)),
            options: diff_ctx.input.options.clone(),
        }
    }

    fn builds(&self, diff_ctx: &MinimalDiffCtx) -> bool {
        let same_file = |key: &Option<Weak<_>>, file: &Option<Arc<_>>| match (key, file) {
            (Some(key), Some(file)) => key.as_ptr() == Arc::as_ptr(file),
            (None, None) => true,
            _ => false,
        };
        self.ir.as_ptr() == Arc::as_ptr(&diff_ctx.diff_ir)
            && same_file(&self.files[0], &diff_ctx.input.file_1)
            && same_file(&self.files[1], &diff_ctx.input.file_2)
            && !DiffBuilderOptions::need_invalidation(&self.options, &diff_ctx.input.options)
    }

    fn alive(&self) -> bool {
        self.ir.strong_count() > 0 && self.files.iter().flatten().all(|f| f.strong_count() > 0)
    }
}

pub(super) struct PatchView {
    key: PatchKey,
    patch: Option<Patch>,
    width: Option<(egui::FontId, f32, f32)>,
}

#[derive(Default)]
pub(super) struct PatchCache {
    views: Vec<PatchView>,
    building: Option<PatchKey>,
    job: Option<Job<PatchView>>,
}

fn build_patch(
    files: [Option<Arc<CachedFile<RawToken>>>; 2],
    options: &DiffBuilderOptions,
    ir: &DiffIR,
) -> Option<Patch> {
    let [Some(source), Some(target)] = files else {
        return None;
    };
    let text = patch_text_of([&source, &target], options, ir);
    let lines = patch_lines(&text);
    let hunks = lines
        .iter()
        .filter(|line| line.kind == PatchLineKind::Hunk)
        .count();
    Some(Patch { text, lines, hunks })
}

/// The patch of `diff_ctx`, or while it builds the last patch shown, flagged stale (`false`).
pub(super) fn patch_view<'a>(
    cache: &'a mut PatchCache,
    diff_ctx: &MinimalDiffCtx,
    egui_ctx: &egui::Context,
) -> Option<(&'a mut PatchView, bool)> {
    let PatchCache {
        views,
        building,
        job,
    } = cache;
    let job = job.get_or_insert_with(|| {
        let egui_ctx = egui_ctx.clone();
        Job::new(move || egui_ctx.request_repaint())
    });
    for view in job.poll() {
        *building = None;
        views.push(view);
    }
    views.retain(|view| view.key.alive());
    if views.len() > STAGE_CACHE_ENTRIES {
        views.remove(0);
    }
    if let Some(index) = views.iter().position(|view| view.key.builds(diff_ctx)) {
        let view = views.remove(index);
        views.push(view);
        return views.last_mut().map(|view| (view, true));
    }
    if !building.as_ref().is_some_and(|key| key.builds(diff_ctx)) {
        let key = PatchKey::of(diff_ctx);
        *building = Some(key.clone());
        let files = [diff_ctx.input.file_1.clone(), diff_ctx.input.file_2.clone()];
        let ir = diff_ctx.diff_ir.clone();
        job.start(move |job| {
            let patch = build_patch(files, &key.options, &ir);
            job.send(PatchView {
                key,
                patch,
                width: None,
            });
        });
    }
    views.last_mut().map(|view| (view, false))
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
            let mut glyph_width = ascii_cached_glyph_width(fonts, font);
            patch
                .lines
                .iter()
                .map(|line| h_scroll::text_width(&patch.text[line.text.clone()], &mut glyph_width))
                .fold(0.0, f32::max)
        }) + 2.0 * TEXT_MARGIN;
        self.width = Some((font.clone(), pixels_per_point, width));
        width
    }

    pub(super) fn ui(&mut self, ui: &mut egui::Ui, stale: bool) {
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
            if stale {
                ui.add(egui::Label::new(UPDATING_TEXT).selectable(false));
            }
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
