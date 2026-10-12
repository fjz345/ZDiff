//! Hex viewer: offset-aligned byte comparison computed off the UI thread, drawn as rows of
//! 16 bytes (offset, hex, ASCII) per side. Rows are built only for the visible window.

use std::{
    cell::Cell,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

use eframe::egui::{self, Color32, FontId, text::LayoutJob};
use zcommon::ui_egui::{style_window::role_color, table::SplitColumn};
use zdiff::hex::{HexDiff, HexSide, hex_diff};

use super::hex_inspector::{Endian, inspector_ui};
use crate::{
    diff_ctx::ScrollSpan,
    file::LoadedFile,
    ui_egui::{
        file_role::FileRole,
        overview::{ChangeKind, ChangeSpan},
    },
};

pub const HEX_ROW_BYTES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexByteState {
    /// Past the end of this side.
    Missing,
    Same,
    Different,
    /// In the size-mismatch tail, so only this side has it.
    OnlyHere,
}

pub fn hex_row_count(len_1: usize, len_2: usize) -> usize {
    len_1.max(len_2).div_ceil(HEX_ROW_BYTES)
}

pub fn hex_row_for_offset(offset: usize) -> usize {
    offset / HEX_ROW_BYTES
}

/// The byte of a row drawn at character `column` of `side_row_job`'s text, in either the hex or the
/// ASCII part. The separators belong to the byte before them.
pub fn hex_byte_at_column(column: usize) -> Option<usize> {
    const HEX_START: usize = 10;
    const HEX_END: usize = HEX_START + HEX_ROW_BYTES * 3 + 1;
    const ASCII_START: usize = HEX_END + 1;
    if (HEX_START..HEX_END).contains(&column) {
        let rel = column - HEX_START;
        let half = HEX_ROW_BYTES / 2;
        Some(if rel < half * 3 + 1 {
            (rel / 3).min(half - 1)
        } else {
            (rel - 1) / 3
        })
    } else {
        let ascii = column.checked_sub(ASCII_START)?;
        (ascii < HEX_ROW_BYTES).then_some(ascii)
    }
}

/// The rows `bytes` (non-empty) covers.
fn hex_rows_span(bytes: &Range<usize>) -> ScrollSpan {
    ScrollSpan {
        start: hex_row_for_offset(bytes.start),
        maybe_end: Some(hex_row_for_offset(bytes.end - 1)),
        side: None,
    }
}

/// Navigation stops: the differing ranges, then the size-mismatch tail, so a pair that only
/// differs in size still has a stop.
pub fn hex_nav_count(diff: Option<&HexDiff>) -> usize {
    diff.map_or(0, |diff| {
        diff.ranges.len() + usize::from(diff.tail.is_some())
    })
}

/// Rows and bytes of stop `cursor`, 1-based like the text conflict cursor (0 is no stop). `None`
/// past the last stop: the conflict cursor is shared with the text viewer and isn't clamped.
pub fn hex_nav_span(diff: &HexDiff, cursor: usize) -> Option<(ScrollSpan, Range<usize>)> {
    let index = cursor.checked_sub(1)?;
    let bytes = match diff.ranges.get(index) {
        Some(range) => range.clone(),
        None if index == diff.ranges.len() => diff.tail.as_ref()?.range.clone(),
        None => return None,
    };
    Some((hex_rows_span(&bytes), bytes))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexOffsetError {
    Invalid,
    /// At or past the end of the longer side.
    OutOfRange {
        len: usize,
    },
}

impl std::fmt::Display for HexOffsetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HexOffsetError::Invalid => write!(f, "expected a decimal or 0x hex byte offset"),
            HexOffsetError::OutOfRange { len } => {
                write!(f, "offset is past the end ({len} bytes, 0x{len:X})")
            }
        }
    }
}

/// A goto offset in decimal or `0x` hex. Rejected rather than clamped when it is past the longer
/// side (`len`), so a typo doesn't silently land on the last byte.
pub fn parse_hex_offset(text: &str, len: usize) -> Result<usize, HexOffsetError> {
    let text = text.trim();
    let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(digits) if digits.bytes().all(|b| b.is_ascii_hexdigit()) => {
            usize::from_str_radix(digits, 16)
        }
        None if text.bytes().all(|b| b.is_ascii_digit()) => text.parse(),
        _ => return Err(HexOffsetError::Invalid),
    };
    let offset = parsed.map_err(|_| HexOffsetError::Invalid)?;
    if offset >= len {
        return Err(HexOffsetError::OutOfRange { len });
    }
    Ok(offset)
}

/// Byte states of one side's `row`. Without a diff (one-sided pair, or still comparing) present
/// bytes are `Same`.
pub fn hex_row_states(
    diff: Option<&HexDiff>,
    side_len: usize,
    row: usize,
) -> [HexByteState; HEX_ROW_BYTES] {
    let row_start = row * HEX_ROW_BYTES;
    let mut states = [HexByteState::Missing; HEX_ROW_BYTES];
    for (i, state) in states.iter_mut().enumerate() {
        if row_start + i < side_len {
            *state = HexByteState::Same;
        }
    }
    let Some(diff) = diff else {
        return states;
    };

    let row_end = row_start + HEX_ROW_BYTES;
    let first = diff.ranges.partition_point(|range| range.end <= row_start);
    for range in diff.ranges[first..]
        .iter()
        .take_while(|range| range.start < row_end)
    {
        for offset in range.start.max(row_start)..range.end.min(row_end) {
            states[offset - row_start] = HexByteState::Different;
        }
    }
    if let Some(tail) = &diff.tail {
        for offset in tail.range.start.max(row_start)..tail.range.end.min(row_end) {
            // Tail bytes exist only on the longer side; on the shorter one they stay Missing.
            if offset < side_len {
                states[offset - row_start] = HexByteState::OnlyHere;
            }
        }
    }
    states
}

/// Overview spans over byte offsets: differing ranges are Changed, the size-mismatch tail is
/// Deleted when only the left side has it and Inserted when only the right does.
pub fn hex_change_spans(diff: &HexDiff) -> Vec<ChangeSpan> {
    let span = |range: &Range<usize>, kind| ChangeSpan {
        start: range.start,
        end: range.end,
        kind,
    };
    let tail = diff.tail.as_ref().map(|tail| {
        let kind = match tail.longer {
            HexSide::First => ChangeKind::Deleted,
            HexSide::Second => ChangeKind::Inserted,
        };
        span(&tail.range, kind)
    });
    diff.ranges
        .iter()
        .map(|range| span(range, ChangeKind::Changed))
        .chain(tail)
        .collect()
}

/// Runs the comparison for the current pair on a worker thread. A new pair cancels the running
/// one, and a result for an older pair is dropped.
#[derive(Debug)]
pub struct HexDiffProcessor {
    file_1: Option<LoadedFile>,
    file_2: Option<LoadedFile>,
    diff: Option<HexDiff>,
    change_spans: Vec<ChangeSpan>,
    in_progress: bool,
    generation: u64,
    cancel_flag: Arc<AtomicBool>,
    channel: (mpsc::Sender<(u64, HexDiff)>, mpsc::Receiver<(u64, HexDiff)>),

    /// One-shot, so going to the same offset again scrolls again.
    goto_offset: Option<usize>,
    /// The stop last scrolled to, so the table is scrolled once per cursor change, not pinned.
    last_nav: Option<Range<usize>>,
    /// Bytes of the last goto or stop, drawn on both sides.
    highlight: Option<Range<usize>>,
    /// A byte clicked this frame, taken as the cursor at the next `scroll_to_row`.
    cursor_click: Cell<Option<usize>>,
    endian: Cell<Endian>,
}

impl Default for HexDiffProcessor {
    fn default() -> Self {
        Self {
            file_1: None,
            file_2: None,
            diff: None,
            change_spans: Vec::new(),
            in_progress: false,
            generation: 0,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            channel: mpsc::channel(),
            goto_offset: None,
            last_nav: None,
            highlight: None,
            cursor_click: Cell::new(None),
            endian: Cell::new(Endian::default()),
        }
    }
}

pub(super) fn is_same_side(a: &Option<LoadedFile>, b: &Option<LoadedFile>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.is_same_load(b),
        _ => false,
    }
}

impl HexDiffProcessor {
    /// No-op for the pair already requested. Returns whether the pair changed, which also resets
    /// navigation; the caller resets the conflict cursor.
    pub fn request(&mut self, file_1: Option<LoadedFile>, file_2: Option<LoadedFile>) -> bool {
        if is_same_side(&self.file_1, &file_1) && is_same_side(&self.file_2, &file_2) {
            return false;
        }

        self.cancel_flag.store(true, Ordering::Release);
        self.cancel_flag = Arc::new(AtomicBool::new(false));
        self.generation += 1;
        self.diff = None;
        self.change_spans.clear();
        self.in_progress = false;
        self.file_1 = file_1;
        self.file_2 = file_2;
        self.goto_offset = None;
        self.last_nav = None;
        self.highlight = None;
        self.cursor_click.set(None);

        if let (Some(file_1), Some(file_2)) = (&self.file_1, &self.file_2) {
            log::info!(
                "Hex diff requested:\nSource: {}\nTarget: {}",
                file_1.path(),
                file_2.path()
            );
            let (file_1, file_2) = (file_1.clone(), file_2.clone());
            let cancel_flag = self.cancel_flag.clone();
            let tx = self.channel.0.clone();
            let generation = self.generation;
            self.in_progress = true;
            std::thread::spawn(move || {
                if let Some(diff) = hex_diff(file_1.bytes(), file_2.bytes(), &cancel_flag) {
                    let _ = tx.send((generation, diff));
                }
            });
        }
        true
    }

    pub fn nav_count(&self) -> usize {
        hex_nav_count(self.diff.as_ref())
    }

    /// Length of the longer side: goto offsets must be below it.
    pub fn max_len(&self) -> usize {
        let len = |file: &Option<LoadedFile>| file.as_ref().map_or(0, |f| f.bytes().len());
        len(&self.file_1).max(len(&self.file_2))
    }

    pub fn goto(&mut self, offset: usize) {
        log::info!("Goto byte offset: {offset} (0x{offset:X})");
        self.goto_offset = Some(offset);
    }

    /// First byte of the last goto, stop or click.
    pub fn cursor(&self) -> Option<usize> {
        self.highlight.as_ref().map(|range| range.start)
    }

    pub fn clear_highlight(&mut self) {
        self.goto_offset = None;
        self.highlight = None;
    }

    /// The rows to scroll to this frame, from a pending goto or a changed conflict cursor (goto
    /// wins, as in the text viewer), and highlights the target bytes.
    pub fn scroll_to_row(&mut self, conflict_cursor: usize) -> Option<ScrollSpan> {
        if let Some(offset) = self.cursor_click.take() {
            self.highlight = Some(offset..offset + 1);
        }
        let nav = self
            .diff
            .as_ref()
            .and_then(|diff| hex_nav_span(diff, conflict_cursor))
            .map(|(_, bytes)| bytes);
        let nav_changed = nav != self.last_nav;
        self.last_nav = nav.clone();

        let goto = self.goto_offset.take().map(|offset| offset..offset + 1);
        let target = goto.or(nav.filter(|_| nav_changed))?;
        let span = hex_rows_span(&target);
        self.highlight = Some(target);
        Some(span)
    }

    pub fn poll(&mut self) {
        while let Ok((generation, diff)) = self.channel.1.try_recv() {
            if generation == self.generation {
                self.change_spans = hex_change_spans(&diff);
                self.diff = Some(diff);
                self.in_progress = false;
            }
        }
    }

    pub fn view_ctx(&self) -> HexViewCtx<'_> {
        HexViewCtx {
            file_1: self.file_1.as_ref(),
            file_2: self.file_2.as_ref(),
            diff: self.diff.as_ref(),
            change_spans: &self.change_spans,
            computing: self.in_progress,
            highlight: self.highlight.clone(),
            cursor: self.cursor(),
            cursor_click: &self.cursor_click,
            endian: &self.endian,
        }
    }
}

pub struct HexViewCtx<'a> {
    pub file_1: Option<&'a LoadedFile>,
    pub file_2: Option<&'a LoadedFile>,
    pub diff: Option<&'a HexDiff>,
    pub change_spans: &'a [ChangeSpan],
    pub computing: bool,
    pub highlight: Option<Range<usize>>,
    pub cursor: Option<usize>,
    pub cursor_click: &'a Cell<Option<usize>>,
    pub endian: &'a Cell<Endian>,
}

impl HexViewCtx<'_> {
    pub fn inspector_ui(&self, ui: &mut egui::Ui) {
        fn bytes(file: Option<&LoadedFile>) -> &[u8] {
            file.map(|f| f.bytes()).unwrap_or_default()
        }
        inspector_ui(
            ui,
            self.cursor,
            [bytes(self.file_1), bytes(self.file_2)],
            self.endian,
        );
    }

    pub fn status_text(&self) -> String {
        if self.computing {
            return "Comparing bytes...".to_string();
        }
        let Some(diff) = self.diff else {
            return String::new();
        };
        if diff.ranges.is_empty() && diff.tail.is_none() {
            return "Identical bytes".to_string();
        }
        let mut text = format!("{} differing ranges", diff.ranges.len());
        if let Some(tail) = &diff.tail {
            let side = match tail.longer {
                HexSide::First => "left",
                HexSide::Second => "right",
            };
            text += &format!(", {} bytes only on the {side}", tail.range.len());
        }
        text
    }
}

pub fn byte_len(ctx: &HexViewCtx) -> usize {
    let len = |file: Option<&LoadedFile>| file.map_or(0, |f| f.bytes().len());
    len(ctx.file_1).max(len(ctx.file_2))
}

pub fn row_count(ctx: &HexViewCtx) -> usize {
    let len = |file: Option<&LoadedFile>| file.map_or(0, |f| f.bytes().len());
    hex_row_count(len(ctx.file_1), len(ctx.file_2))
}

/// One cell of the diff pane's table: a side's row, or the change marker in the center.
pub fn cell(ui: &mut egui::Ui, ctx: &HexViewCtx, row_index: usize, column: SplitColumn) {
    let bytes_1 = ctx.file_1.map(|f| f.bytes()).unwrap_or_default();
    let bytes_2 = ctx.file_2.map(|f| f.bytes()).unwrap_or_default();
    let states_1 = || hex_row_states(ctx.diff, bytes_1.len(), row_index);
    let states_2 = || hex_row_states(ctx.diff, bytes_2.len(), row_index);

    match column {
        SplitColumn::Left => {
            if ctx.file_1.is_some() {
                side_row_label(ui, ctx, bytes_1, &states_1(), row_index);
            }
        }
        SplitColumn::Center => {
            let changed = |states: &[HexByteState]| {
                states
                    .iter()
                    .any(|s| matches!(s, HexByteState::Different | HexByteState::OnlyHere))
            };
            let marker = if changed(&states_1()) || changed(&states_2()) {
                "≠"
            } else {
                " "
            };
            let color = role_color(ui.ctx(), FileRole::LineNumber);
            ui.centered_and_justified(|ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(marker).color(color)).selectable(false),
                );
            });
        }
        SplitColumn::Right => {
            if ctx.file_2.is_some() {
                side_row_label(ui, ctx, bytes_2, &states_2(), row_index);
            }
        }
    }
}

fn side_row_label(
    ui: &mut egui::Ui,
    ctx: &HexViewCtx,
    bytes: &[u8],
    states: &[HexByteState; HEX_ROW_BYTES],
    row: usize,
) {
    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
    let char_width = ui.fonts_mut(|fonts| fonts.glyph_width(&font_id, ' '));
    let colors = HexColors {
        text: ui.style().visuals.text_color(),
        offset: role_color(ui.ctx(), FileRole::LineNumber),
        different: role_color(ui.ctx(), FileRole::HexDifferent),
        only_here: role_color(ui.ctx(), FileRole::HexOnlyHere),
        highlight: role_color(ui.ctx(), FileRole::HexGoto),
    };
    let job = side_row_job(bytes, states, row, ctx.highlight.as_ref(), font_id, &colors);
    let response = ui.add(
        egui::Label::new(job)
            .selectable(false)
            .extend()
            .sense(egui::Sense::click()),
    );
    if !response.clicked() {
        return;
    }
    let Some(pos) = response.interact_pointer_pos() else {
        return;
    };
    let column = ((pos.x - response.rect.min.x) / char_width).max(0.0) as usize;
    if let Some(byte) = hex_byte_at_column(column) {
        let offset = row * HEX_ROW_BYTES + byte;
        if offset < byte_len(ctx) {
            ctx.cursor_click.set(Some(offset));
            ui.ctx().request_repaint();
        }
    }
}

struct HexColors {
    text: Color32,
    offset: Color32,
    different: Color32,
    only_here: Color32,
    highlight: Color32,
}

fn side_row_job(
    bytes: &[u8],
    states: &[HexByteState; HEX_ROW_BYTES],
    row: usize,
    highlight: Option<&Range<usize>>,
    font_id: FontId,
    colors: &HexColors,
) -> LayoutJob {
    let text_color = colors.text;
    let row_start = row * HEX_ROW_BYTES;
    let mut job = LayoutJob::default();
    let append = |job: &mut LayoutJob, text: &str, color: Color32, background: Color32| {
        job.append(
            text,
            0.0,
            egui::TextFormat {
                font_id: font_id.clone(),
                color,
                background,
                ..Default::default()
            },
        );
    };
    let background = |state: HexByteState| match state {
        HexByteState::Different => colors.different,
        HexByteState::OnlyHere => colors.only_here,
        HexByteState::Missing | HexByteState::Same => Color32::TRANSPARENT,
    };
    // The goto or navigation target is recolored and underlined, so the diff background stays.
    // Not on the shorter side's missing bytes.
    let append_byte = |job: &mut LayoutJob, text: &str, i: usize, state: HexByteState| {
        let marked = state != HexByteState::Missing
            && highlight.is_some_and(|range| range.contains(&(row_start + i)));
        job.append(
            text,
            0.0,
            egui::TextFormat {
                font_id: font_id.clone(),
                color: if marked { colors.highlight } else { text_color },
                background: background(state),
                underline: if marked {
                    egui::Stroke::new(1.0, colors.highlight)
                } else {
                    egui::Stroke::NONE
                },
                ..Default::default()
            },
        );
    };

    append(
        &mut job,
        &format!("{row_start:08X}  "),
        colors.offset,
        Color32::TRANSPARENT,
    );
    for (i, &state) in states.iter().enumerate() {
        let hex = match state {
            HexByteState::Missing => "  ".to_string(),
            _ => format!("{:02X}", bytes[row_start + i]),
        };
        append_byte(&mut job, &hex, i, state);
        append(
            &mut job,
            if i == HEX_ROW_BYTES / 2 - 1 {
                "  "
            } else {
                " "
            },
            text_color,
            Color32::TRANSPARENT,
        );
    }
    append(&mut job, " ", text_color, Color32::TRANSPARENT);
    for (i, &state) in states.iter().enumerate() {
        let ch = match state {
            HexByteState::Missing => ' ',
            _ => match bytes[row_start + i] {
                byte @ 0x20..=0x7E => byte as char,
                _ => '.',
            },
        };
        append_byte(&mut job, &ch.to_string(), i, state);
    }
    job
}

#[cfg(test)]
mod tests {
    use zdiff::hex::HexTail;

    use super::*;
    use crate::ui_egui::overview::bin_changes;

    fn diff(ranges: Vec<Range<usize>>, tail: Option<HexTail>) -> HexDiff {
        HexDiff { ranges, tail }
    }

    fn byte_span(start: usize, end: usize, kind: ChangeKind) -> ChangeSpan {
        ChangeSpan { start, end, kind }
    }

    #[test]
    fn differing_ranges_are_changed_spans_over_bytes() {
        let d = diff(vec![3..4, 20..40], None);
        assert_eq!(
            hex_change_spans(&d),
            [
                byte_span(3, 4, ChangeKind::Changed),
                byte_span(20, 40, ChangeKind::Changed)
            ]
        );
    }

    #[test]
    fn tail_on_the_left_is_deleted_and_on_the_right_inserted() {
        let tail = |longer| {
            Some(HexTail {
                longer,
                range: 50..60,
            })
        };
        assert_eq!(
            hex_change_spans(&diff(vec![], tail(HexSide::First))),
            [byte_span(50, 60, ChangeKind::Deleted)]
        );
        assert_eq!(
            hex_change_spans(&diff(vec![10..50], tail(HexSide::Second))),
            [
                byte_span(10, 50, ChangeKind::Changed),
                byte_span(50, 60, ChangeKind::Inserted)
            ]
        );
    }

    #[test]
    fn identical_bytes_have_no_spans() {
        assert_eq!(hex_change_spans(&diff(vec![], None)), []);
    }

    #[test]
    fn one_differing_byte_in_a_large_file_still_marks_a_bin() {
        let d = diff(vec![5_000_000..5_000_001], None);
        let bins = bin_changes(&hex_change_spans(&d), 10_000_000, 100);
        let marked: Vec<usize> = (0..bins.len()).filter(|i| bins[*i].is_some()).collect();
        assert_eq!(marked, [50]);
        assert_eq!(bins[50], Some(ChangeKind::Changed));
    }

    #[test]
    fn tail_marks_the_bins_where_the_longer_side_continues() {
        let d = diff(
            vec![0..1],
            Some(HexTail {
                longer: HexSide::Second,
                range: 900..1000,
            }),
        );
        let bins = bin_changes(&hex_change_spans(&d), 1000, 10);
        assert_eq!(bins[0], Some(ChangeKind::Changed));
        assert_eq!(bins[9], Some(ChangeKind::Inserted));
        assert!(bins[1..9].iter().all(Option::is_none));
    }

    fn span(start: usize, end: usize) -> ScrollSpan {
        ScrollSpan {
            start,
            maybe_end: Some(end),
            side: None,
        }
    }

    #[test]
    fn offset_maps_to_its_16_byte_row() {
        assert_eq!(hex_row_for_offset(0), 0);
        assert_eq!(hex_row_for_offset(15), 0);
        assert_eq!(hex_row_for_offset(16), 1);
        assert_eq!(hex_row_for_offset(0x1234), 0x123);
    }

    #[test]
    fn range_index_maps_to_the_rows_the_range_covers() {
        let d = diff(vec![3..4, 20..40, 47..49], None);
        assert_eq!(hex_nav_count(Some(&d)), 3);
        // The cursor is 1-based like the text conflict cursor; 0 is no stop.
        assert_eq!(hex_nav_span(&d, 0), None);
        assert_eq!(hex_nav_span(&d, 1), Some((span(0, 0), 3..4)));
        assert_eq!(hex_nav_span(&d, 2), Some((span(1, 2), 20..40)));
        // A range crossing a row boundary covers both rows.
        assert_eq!(hex_nav_span(&d, 3), Some((span(2, 3), 47..49)));
    }

    #[test]
    fn the_size_mismatch_tail_is_the_last_stop() {
        let d = diff(
            vec![5..6],
            Some(HexTail {
                longer: HexSide::Second,
                range: 32..100,
            }),
        );
        assert_eq!(hex_nav_count(Some(&d)), 2);
        assert_eq!(hex_nav_span(&d, 2), Some((span(2, 6), 32..100)));

        let size_only = diff(
            vec![],
            Some(HexTail {
                longer: HexSide::First,
                range: 8..9,
            }),
        );
        assert_eq!(hex_nav_count(Some(&size_only)), 1);
        assert_eq!(hex_nav_span(&size_only, 1), Some((span(0, 0), 8..9)));
    }

    #[test]
    fn a_cursor_past_the_last_stop_or_no_diff_has_no_stop() {
        // The shared conflict cursor can be left above the hex count (set_max doesn't clamp).
        let d = diff(vec![3..4], None);
        assert_eq!(hex_nav_span(&d, 2), None);
        assert_eq!(hex_nav_span(&diff(vec![], None), 1), None);
        assert_eq!(hex_nav_count(Some(&diff(vec![], None))), 0);
        assert_eq!(hex_nav_count(None), 0);
    }

    #[test]
    fn clear_highlight_drops_the_goto_highlight() {
        let mut processor = HexDiffProcessor::default();
        processor.goto(5);
        assert!(processor.scroll_to_row(0).is_some());
        assert_eq!(processor.highlight, Some(5..6));

        processor.clear_highlight();
        assert_eq!(processor.highlight, None);
        assert_eq!(processor.scroll_to_row(0), None);
    }

    #[test]
    fn row_colors_come_from_the_given_colors() {
        let colors = HexColors {
            text: Color32::from_rgb(1, 1, 1),
            offset: Color32::from_rgb(2, 2, 2),
            different: Color32::from_rgb(3, 3, 3),
            only_here: Color32::from_rgb(4, 4, 4),
            highlight: Color32::from_rgb(5, 5, 5),
        };
        let mut states = [HexByteState::Same; HEX_ROW_BYTES];
        states[0] = HexByteState::Different;
        states[1] = HexByteState::OnlyHere;
        let bytes = [0u8; HEX_ROW_BYTES];
        let job = side_row_job(
            &bytes,
            &states,
            0,
            Some(&(2..3)),
            FontId::default(),
            &colors,
        );

        let format = |section: usize| &job.sections[section].format;
        assert_eq!(format(0).color, colors.offset);
        assert_eq!(format(1).background, colors.different);
        assert_eq!(format(1).color, colors.text);
        assert_eq!(format(3).background, colors.only_here);
        assert_eq!(format(5).background, Color32::TRANSPARENT);
        assert_eq!(format(5).color, colors.highlight);
        assert_eq!(
            format(5).underline,
            egui::Stroke::new(1.0, colors.highlight)
        );
    }

    #[test]
    fn offset_parses_as_decimal_or_0x_hex() {
        assert_eq!(parse_hex_offset("0", 100), Ok(0));
        assert_eq!(parse_hex_offset("42", 100), Ok(42));
        assert_eq!(parse_hex_offset("0x1F", 100), Ok(0x1F));
        assert_eq!(parse_hex_offset("0X1f", 100), Ok(0x1F));
        assert_eq!(parse_hex_offset("  0x10 ", 100), Ok(16));
    }

    #[test]
    fn invalid_offset_text_is_rejected() {
        for text in [
            "", "  ", "0x", "1F", "-1", "0x-1", "12a", "0xG", "x10", "1.5",
        ] {
            assert_eq!(
                parse_hex_offset(text, 100),
                Err(HexOffsetError::Invalid),
                "{text:?}"
            );
        }
        assert_eq!(
            parse_hex_offset("0xFFFFFFFFFFFFFFFFFF", 100),
            Err(HexOffsetError::Invalid)
        );
    }

    #[test]
    fn offset_past_the_longer_side_is_rejected() {
        assert_eq!(parse_hex_offset("99", 100), Ok(99));
        assert_eq!(
            parse_hex_offset("100", 100),
            Err(HexOffsetError::OutOfRange { len: 100 })
        );
        assert_eq!(
            parse_hex_offset("0x64", 100),
            Err(HexOffsetError::OutOfRange { len: 100 })
        );
        assert_eq!(
            parse_hex_offset("0", 0),
            Err(HexOffsetError::OutOfRange { len: 0 })
        );
    }

    #[test]
    fn columns_map_to_the_byte_drawn_there() {
        assert_eq!(hex_byte_at_column(0), None);
        assert_eq!(hex_byte_at_column(9), None);
        assert_eq!(hex_byte_at_column(10), Some(0));
        assert_eq!(hex_byte_at_column(11), Some(0));
        assert_eq!(hex_byte_at_column(13), Some(1));
        assert_eq!(hex_byte_at_column(31), Some(7));
        assert_eq!(hex_byte_at_column(32), Some(7));
        assert_eq!(hex_byte_at_column(35), Some(8));
        assert_eq!(hex_byte_at_column(56), Some(15));
        assert_eq!(hex_byte_at_column(57), Some(15));
        assert_eq!(hex_byte_at_column(58), Some(15));
        assert_eq!(hex_byte_at_column(59), None);
        assert_eq!(hex_byte_at_column(60), Some(0));
        assert_eq!(hex_byte_at_column(75), Some(15));
        assert_eq!(hex_byte_at_column(76), None);
    }

    #[test]
    fn columns_match_the_drawn_row() {
        let bytes: Vec<u8> = (0..HEX_ROW_BYTES as u8).collect();
        let states = [HexByteState::Same; HEX_ROW_BYTES];
        let colors = HexColors {
            text: Color32::WHITE,
            offset: Color32::WHITE,
            different: Color32::WHITE,
            only_here: Color32::WHITE,
            highlight: Color32::WHITE,
        };
        let job = side_row_job(&bytes, &states, 0, None, FontId::default(), &colors);
        for (column, ch) in job.text.chars().enumerate() {
            match hex_byte_at_column(column) {
                Some(i) if column < 59 => {
                    let digits = format!("{:02X}", bytes[i]);
                    assert!(
                        digits.contains(ch) || ch == ' ',
                        "column {column} is {ch:?}, byte {i} is {digits}"
                    );
                }
                Some(i) => assert_eq!(ch, if bytes[i] == 0x20 { ' ' } else { '.' }),
                None => assert!(ch == ' ' || column < 8, "column {column} is {ch:?}"),
            }
        }
        assert_eq!(job.text.chars().count(), 76);
    }

    #[test]
    fn goto_and_stops_put_the_cursor_on_their_first_byte() {
        let mut processor = HexDiffProcessor::default();
        assert_eq!(processor.cursor(), None);
        processor.goto(37);
        processor.scroll_to_row(0);
        assert_eq!(processor.cursor(), Some(37));
        processor.clear_highlight();
        assert_eq!(processor.cursor(), None);
    }

    #[test]
    fn a_clicked_byte_moves_the_cursor_without_scrolling() {
        let mut processor = HexDiffProcessor::default();
        processor.goto(5);
        processor.scroll_to_row(0);

        processor.view_ctx().cursor_click.set(Some(40));
        assert_eq!(processor.scroll_to_row(0), None);
        assert_eq!(processor.cursor(), Some(40));
        assert_eq!(processor.highlight, Some(40..41));
        assert_eq!(processor.view_ctx().cursor, Some(40));
        assert_eq!(processor.view_ctx().cursor_click.get(), None);
    }

    #[test]
    fn a_goto_in_the_same_frame_wins_over_a_click() {
        let mut processor = HexDiffProcessor::default();
        processor.view_ctx().cursor_click.set(Some(40));
        processor.goto(7);
        assert!(processor.scroll_to_row(0).is_some());
        assert_eq!(processor.cursor(), Some(7));
    }
}
