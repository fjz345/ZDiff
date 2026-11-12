use eframe::egui::{self, Rect, UiBuilder};
use zcommon_ui::table::SplitColumn;
use zdiff::diff_builder::{LineContent, RowBg};

use crate::{
    diff_ctx::{MinimalDiffCtx, ScrollSpan},
    ui_egui::jump::{jump_offset, span_rows},
    viewer::{
        ViewerKind,
        audio::{self, AudioDiffProcessor, AudioSettings},
        hex::{self, HexViewCtx},
        image::{self, ImageDiffProcessor, ImageSettings},
        video_frames::{self, VideoFrameProcessor},
    },
};

/// A pair whose sides resolve to different viewers. Every processor holds its side in the first
/// slot, and the text side is diffed alone, so its rows are the left side of `text`.
pub struct SideViewers<'a> {
    pub kinds: [ViewerKind; 2],
    pub text: Option<&'a MinimalDiffCtx>,
    pub text_scroll: Option<ScrollSpan>,
    pub hex: Option<HexViewCtx<'a>>,
    pub hex_scroll: Option<ScrollSpan>,
    pub center_jumps: bool,
    pub image: Option<&'a mut ImageDiffProcessor>,
    pub image_settings: Option<&'a mut ImageSettings>,
    pub audio: Option<(&'a mut AudioDiffProcessor, &'a mut AudioSettings)>,
    pub video: Option<&'a mut VideoFrameProcessor>,
}

pub fn status_text([first, second]: [ViewerKind; 2]) -> String {
    format!(
        "Types differ ({}, {}): each side is shown on its own, not diffed",
        first.name(),
        second.name()
    )
}

/// A text row without its diff coloring.
pub fn plain_content(content: &LineContent) -> LineContent {
    match content {
        LineContent::Code {
            tokens, line_num, ..
        } => LineContent::Code {
            tokens: tokens.clone(),
            line_num: *line_num,
            bg: RowBg::None,
            changed_chars: Vec::new(),
        },
        other => other.clone(),
    }
}

/// Each side's viewer below the header, in the table's side columns `columns`. `text_row` draws
/// one text row.
pub fn show(
    ui: &mut egui::Ui,
    sides: &mut SideViewers,
    columns: [Rect; 2],
    mut text_row: impl FnMut(&mut egui::Ui, usize),
) {
    let _scope = zcommon_ui::frame_stats::scope("Side viewers");
    let area = ui.available_rect_before_wrap();
    let row_height =
        ui.text_style_height(&egui::TextStyle::Monospace) + ui.spacing().item_spacing.y;
    for (index, kind) in sides.kinds.into_iter().enumerate() {
        let rect = Rect::from_x_y_ranges(columns[index].x_range(), area.y_range());
        let mut half = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .id_salt(("side_viewer", index)),
        );
        half.set_clip_rect(rect.intersect(ui.clip_rect()));
        match kind {
            ViewerKind::Text => {
                if let Some(text) = sides.text {
                    let rows = text.diff_rows.len();
                    scrolled_rows(
                        &mut half,
                        row_height,
                        rows,
                        sides.text_scroll,
                        sides.center_jumps,
                        &mut text_row,
                    );
                }
            }
            ViewerKind::Hex => {
                if let Some(view) = &sides.hex {
                    let rows = hex::row_count(view);
                    scrolled_rows(
                        &mut half,
                        row_height,
                        rows,
                        sides.hex_scroll,
                        sides.center_jumps,
                        |ui, row| hex::cell(ui, view, row, SplitColumn::Left),
                    );
                }
            }
            ViewerKind::Image => {
                if let (Some(processor), Some(settings)) = (
                    sides.image.as_deref_mut(),
                    sides.image_settings.as_deref_mut(),
                ) {
                    image::show(&mut half, processor, settings, rect.width(), [0, 1]);
                }
            }
            ViewerKind::Audio => {
                if let Some((processor, settings)) = sides.audio.as_mut() {
                    audio::show(&mut half, processor, settings);
                }
            }
            ViewerKind::Video => {
                if let (Some(processor), Some(settings)) = (
                    sides.video.as_deref_mut(),
                    sides.image_settings.as_deref_mut(),
                ) {
                    video_frames::show(&mut half, processor, settings, rect.width(), [0, 1]);
                }
            }
        }
    }
    ui.advance_cursor_after_rect(area);
}

fn scrolled_rows(
    ui: &mut egui::Ui,
    row_height: f32,
    rows: usize,
    scroll_to: Option<ScrollSpan>,
    center: bool,
    mut row: impl FnMut(&mut egui::Ui, usize),
) {
    let mut area = egui::ScrollArea::both().auto_shrink(false);
    if let Some(span) = scroll_to {
        let viewport = ui.available_height();
        area = area.vertical_scroll_offset(jump_offset(
            span_rows(&span),
            row_height,
            viewport,
            center,
        ));
    }
    area.show_rows(ui, row_height, rows, |ui, range| {
        for index in range {
            row(ui, index);
        }
    });
}
