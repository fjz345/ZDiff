use std::{
    ops::Range,
    path::Path,
    sync::{Arc, OnceLock, atomic::AtomicBool},
};

use eframe::egui::{self, Layout, UiBuilder};
use zcommon::job::Job;
use zdiff::{
    cached_file::{CachedFile, FileMetadata},
    diff_builder::{Color32, LineContent, RowBg},
    diff_ir::{DiffIR, DiffOp, DiffResult},
    ignore::IgnoreMask,
    lexer::{Lexer, RawToken, TokenKind},
    merge::{self, ChunkKind, Merge, MergeChunk, MergeLabels, MergeTexts, Pick, Resolution},
    myers::{DiffInput, DiffPhases, MyersDiffAlgorithm, myers_diff_path},
    universal_path::UniversalPath,
};

use crate::{
    file::FileProcessor,
    revert::{WriteRefusal, write_atomic},
    ui_egui::{
        active_side::ActiveSide,
        diff_pane::{FileDiffPane, RowCtx},
        jump::jump_offset,
        occurrence::{OccurrenceState, RowFind},
        syntax::detect_language,
    },
};

const STRIP_WIDTH: f32 = 250.0;
const ONE_SIDED_BG: Color32 = Color32([100, 160, 255, 30]);
const CONFLICT_BG: Color32 = Color32([255, 100, 100, 45]);
const RESOLVED_CONFLICT_BG: Color32 = Color32([255, 255, 100, 30]);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRequest {
    pub base: Option<UniversalPath>,
    pub theirs: UniversalPath,
    pub yours: UniversalPath,
    pub output: UniversalPath,
}

/// Command-line labels; a side without one shows its path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeNames {
    pub base: Option<String>,
    pub theirs: Option<String>,
    pub yours: Option<String>,
    pub output: Option<String>,
}

pub const MERGE_FIELDS: [&str; 4] = ["Base", "Theirs", "Yours", "Output"];

/// Fields in `MERGE_FIELDS` order. An empty base is a two-way merge.
pub fn merge_request(fields: &[String; 4]) -> Result<MergeRequest, String> {
    let path = |i: usize| {
        let text = fields[i].trim();
        (!text.is_empty()).then(|| UniversalPath::from(text))
    };
    let required = |i: usize| path(i).ok_or_else(|| format!("{} is required", MERGE_FIELDS[i]));
    let output = required(3)?;
    check_output(&output)?;
    Ok(MergeRequest {
        base: path(0),
        theirs: required(1)?,
        yours: required(2)?,
        output,
    })
}

pub fn check_output(output: &UniversalPath) -> Result<(), String> {
    match output.as_path() {
        None => Err("Output must be a local path".to_string()),
        Some(_) => Ok(()),
    }
}

/// One row of the top panes; a side's line is `None` where the chunk is shorter on that side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeRow {
    pub chunk: usize,
    pub first: bool,
    pub theirs: Option<usize>,
    pub base: Option<usize>,
    pub yours: Option<usize>,
}

pub fn aligned_rows(chunks: &[MergeChunk]) -> Vec<MergeRow> {
    let line = |range: &Range<usize>, row: usize| (row < range.len()).then(|| range.start + row);
    let mut rows = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let height = chunk
            .theirs
            .len()
            .max(chunk.base.len())
            .max(chunk.yours.len());
        rows.extend((0..height).map(|row| MergeRow {
            chunk: i,
            first: row == 0,
            theirs: line(&chunk.theirs, row),
            base: line(&chunk.base, row),
            yours: line(&chunk.yours, row),
        }));
    }
    rows
}

/// `total` counts the changed chunks; unchanged ones have nothing to pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeCounts {
    pub unresolved: usize,
    pub total: usize,
}

pub fn counts(merge: &Merge) -> MergeCounts {
    let changed = || {
        merge
            .chunks()
            .iter()
            .zip(merge.resolutions())
            .filter(|(chunk, _)| chunk.kind != ChunkKind::Unchanged)
    };
    MergeCounts {
        unresolved: changed()
            .filter(|(_, resolution)| **resolution == Resolution::Unresolved)
            .count(),
        total: changed().count(),
    }
}

/// The next chunk after (or before) `current`, wrapping: unresolved conflicts while any are
/// left, else every changed chunk.
pub fn step_chunk(merge: &Merge, current: Option<usize>, forward: bool) -> Option<usize> {
    let len = merge.chunks().len();
    let any_unresolved = merge.resolutions().contains(&Resolution::Unresolved);
    let wanted = |i: &usize| {
        merge.chunks()[*i].kind != ChunkKind::Unchanged
            && (!any_unresolved || merge.resolutions()[*i] == Resolution::Unresolved)
    };
    if forward {
        let after = current.map_or(0, |c| c + 1);
        (after..len).chain(0..after).find(wanted)
    } else {
        let before = current.unwrap_or(len);
        (0..before).rev().chain((before..len).rev()).find(wanted)
    }
}

pub fn picks(has_base: bool) -> Vec<Pick> {
    let mut picks = vec![
        Pick::Theirs,
        Pick::Yours,
        Pick::TheirsThenYours,
        Pick::YoursThenTheirs,
    ];
    if has_base {
        picks.push(Pick::Base);
    }
    picks
}

/// Each line's token range, line break included. Lines are the ones `merge::lines` gives.
fn line_tokens(file: &CachedFile<RawToken>) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, token) in file.tokens.iter().enumerate() {
        if token.kind == TokenKind::Newline {
            lines.push(start..i + 1);
            start = i + 1;
        }
    }
    if start < file.tokens.len() {
        lines.push(start..file.tokens.len());
    }
    assert_eq!(
        lines.len(),
        merge::lines(&file.contents).len(),
        "token lines of {} differ from the merge's lines",
        file.path
    );
    lines
}

fn text_file(path: UniversalPath, contents: String) -> CachedFile<RawToken> {
    let tokens = Lexer::new(&contents).parse();
    let metadata = FileMetadata::new(&contents);
    CachedFile {
        path,
        hash: zcommon::hash::hash_contents(b""),
        contents,
        tokens,
        metadata,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Theirs,
    Yours,
    Base,
    Edited,
    Conflict,
}

/// An output line's chunk, and where it came from; `None` for unchanged chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputLine {
    pub chunk: usize,
    pub origin: Option<Origin>,
}

/// Lays the chunks out like `Merge::output`: every non-empty piece starts on a new line, so a
/// piece of n source lines is n output lines.
fn output_lines(merge: &Merge, has_base: bool) -> Vec<OutputLine> {
    let mut out = Vec::new();
    for (i, (chunk, resolution)) in merge.chunks().iter().zip(merge.resolutions()).enumerate() {
        let mut push = |origin: Option<Origin>, n: usize| {
            out.extend(std::iter::repeat_n(OutputLine { chunk: i, origin }, n));
        };
        let side = |origin| (chunk.kind != ChunkKind::Unchanged).then_some(origin);
        let (base, theirs, yours) = (chunk.base.len(), chunk.theirs.len(), chunk.yours.len());
        match resolution {
            Resolution::Unresolved => {
                let markers = if has_base { 4 } else { 3 };
                let base = if has_base { base } else { 0 };
                push(Some(Origin::Conflict), markers + yours + base + theirs);
            }
            Resolution::Edited(text) => push(Some(Origin::Edited), merge::lines(text).len()),
            Resolution::Auto(pick) | Resolution::User(pick) => {
                let (t, y) = (side(Origin::Theirs), side(Origin::Yours));
                match pick {
                    Pick::Base => push(side(Origin::Base), base),
                    Pick::Theirs => push(t, theirs),
                    Pick::Yours => push(y, yours),
                    Pick::TheirsThenYours => {
                        push(t, theirs);
                        push(y, yours);
                    }
                    Pick::YoursThenTheirs => {
                        push(y, yours);
                        push(t, theirs);
                    }
                }
            }
        }
    }
    out
}

fn chunk_range(lines: &[OutputLine], chunk: usize) -> Range<usize> {
    lines.partition_point(|l| l.chunk < chunk)..lines.partition_point(|l| l.chunk <= chunk)
}

/// Per token of theirs, base and yours: changed against base, or theirs against yours without
/// one.
fn changed_tokens(
    algorithm: MyersDiffAlgorithm,
    chunks: &[MergeChunk],
    files: [Option<&CachedFile<RawToken>>; 3],
    lines: &[Vec<Range<usize>>; 3],
) -> [Vec<bool>; 3] {
    let mut changed = files.map(|file| vec![false; file.map_or(0, |f| f.tokens.len())]);
    let tokens = |pane: usize, range: &Range<usize>| match range.is_empty() {
        true => 0..0,
        false => lines[pane][range.start].start..lines[pane][range.end - 1].end,
    };
    let pairs: &[(usize, usize)] = match files[1] {
        Some(_) => &[(1, 0), (1, 2)],
        None => &[(0, 2)],
    };
    for chunk in chunks.iter().filter(|c| c.kind != ChunkKind::Unchanged) {
        let ranges = [&chunk.theirs, &chunk.base, &chunk.yours];
        for &(s, t) in pairs {
            let (source, target) = (files[s].unwrap(), files[t].unwrap());
            let (s_tokens, t_tokens) = (tokens(s, ranges[s]), tokens(t, ranges[t]));
            let cmp = |a: &RawToken, b: &RawToken| {
                a.kind == b.kind
                    && source.contents[a.span.clone()] == target.contents[b.span.clone()]
            };
            let cancel = Arc::new(AtomicBool::new(false));
            let path = myers_diff_path(
                algorithm,
                DiffInput {
                    source: &source.tokens[s_tokens.clone()],
                    target: &target.tokens[t_tokens.clone()],
                    source_text: &source.contents,
                    target_text: &target.contents,
                    ignore: &IgnoreMask::default(),
                },
                cmp,
                DiffPhases::default(),
                &[],
                cancel.clone(),
            )
            .expect("the token diff is never cancelled");
            let ir = DiffIR::new(&path, true, cancel).expect("the token diff is never cancelled");
            for entry in ir.entries {
                match entry.operation {
                    DiffOp::Delete => {
                        changed[s][s_tokens.start + entry.token_source_idx.unwrap() as usize] = true
                    }
                    DiffOp::Insert => {
                        changed[t][t_tokens.start + entry.token_target_idx.unwrap() as usize] = true
                    }
                    DiffOp::Equal(_) => {}
                }
            }
        }
    }
    changed
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeFileLabels {
    pub base: String,
    pub theirs: String,
    pub yours: String,
}

pub struct MergeDoc {
    base: Option<Arc<CachedFile<RawToken>>>,
    theirs: Arc<CachedFile<RawToken>>,
    yours: Arc<CachedFile<RawToken>>,
    labels: MergeFileLabels,
    /// Theirs, base, yours: the top panes' order.
    lines: [Vec<Range<usize>>; 3],
    languages: [Option<&'static str>; 3],
    changed: [Vec<bool>; 3],
    merge: Merge,
    rows: Vec<MergeRow>,
    output: Arc<CachedFile<RawToken>>,
    output_lines: Vec<Range<usize>>,
    output_info: Vec<OutputLine>,
    current: Option<usize>,
}

impl MergeDoc {
    pub fn new(
        algorithm: MyersDiffAlgorithm,
        base: Option<Arc<CachedFile<RawToken>>>,
        theirs: Arc<CachedFile<RawToken>>,
        yours: Arc<CachedFile<RawToken>>,
        labels: MergeFileLabels,
    ) -> Self {
        let chunks = merge::merge_chunks(
            algorithm,
            base.as_ref().map(|base| base.contents.as_str()),
            &theirs.contents,
            &yours.contents,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("the merge is never cancelled");
        let lines = [
            line_tokens(&theirs),
            base.as_deref().map(line_tokens).unwrap_or_default(),
            line_tokens(&yours),
        ];
        let changed = changed_tokens(
            algorithm,
            &chunks,
            [Some(&*theirs), base.as_deref(), Some(&*yours)],
            &lines,
        );
        let languages = [Some(&theirs), base.as_ref(), Some(&yours)]
            .map(|file| file.and_then(|f| detect_language(&f.path, &f.contents)));
        let mut doc = Self {
            languages,
            rows: aligned_rows(&chunks),
            merge: Merge::new(chunks),
            output: Arc::new(text_file(UniversalPath::new(""), String::new())),
            output_lines: Vec::new(),
            output_info: Vec::new(),
            current: None,
            changed,
            base,
            theirs,
            yours,
            labels,
            lines,
        };
        doc.update_output();
        doc
    }

    fn update_output(&mut self) {
        let text = self.merge.output(
            MergeTexts {
                base: self.base.as_ref().map(|base| base.contents.as_str()),
                theirs: &self.theirs.contents,
                yours: &self.yours.contents,
            },
            MergeLabels {
                base: &self.labels.base,
                theirs: &self.labels.theirs,
                yours: &self.labels.yours,
            },
        );
        let output = text_file(UniversalPath::new(""), text);
        self.output_lines = line_tokens(&output);
        self.output_info = output_lines(&self.merge, self.base.is_some());
        assert_eq!(
            self.output_info.len(),
            self.output_lines.len(),
            "output lines differ from the chunk layout"
        );
        self.output = Arc::new(output);
    }

    pub fn merge(&self) -> &Merge {
        &self.merge
    }

    pub fn output_text(&self) -> &str {
        &self.output.contents
    }

    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Steps the current chunk and returns its first row.
    pub fn navigate(&mut self, forward: bool) -> Option<usize> {
        self.current = step_chunk(&self.merge, self.current, forward);
        let chunk = self.current?;
        self.rows.iter().position(|row| row.chunk == chunk)
    }

    pub fn pick(&mut self, chunk: usize, pick: Pick) {
        self.merge.pick(chunk, pick);
        self.update_output();
    }

    /// False, picking nothing, when the chunk holds a hand edit: that needs a confirmation.
    pub fn pick_unless_edited(&mut self, chunk: usize, pick: Pick) -> bool {
        if matches!(self.merge.resolutions()[chunk], Resolution::Edited(_)) {
            return false;
        }
        self.pick(chunk, pick);
        true
    }

    /// The chunk's output lines, with `\n` line breaks.
    pub fn chunk_text(&self, chunk: usize) -> String {
        let range = chunk_range(&self.output_info, chunk);
        merge::lines(&self.output.contents)[range]
            .concat()
            .replace("\r\n", "\n")
            .replace('\r', "\n")
    }

    /// A text equal to the chunk's output is no edit, so picks stay picks.
    pub fn edit(&mut self, chunk: usize, text: String) {
        if text == self.chunk_text(chunk) {
            return;
        }
        self.merge.edit(chunk, text);
        self.update_output();
    }

    pub fn undo(&mut self) -> Option<usize> {
        let chunk = self.merge.undo()?;
        self.update_output();
        Some(chunk)
    }

    pub fn redo(&mut self) -> Option<usize> {
        let chunk = self.merge.redo()?;
        self.update_output();
        Some(chunk)
    }

    /// The output text already has yours' line endings.
    pub fn save(&self, path: &Path) -> Result<SavedOutput, WriteRefusal> {
        write_atomic(path, self.output_text().as_bytes())?;
        Ok(SavedOutput {
            text: self.output_text().to_string(),
            unresolved: counts(&self.merge).unresolved,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedOutput {
    text: String,
    unresolved: usize,
}

pub fn exit_code(saved: Option<&SavedOutput>, output: &str) -> u8 {
    match saved {
        Some(saved) if saved.unresolved == 0 && saved.text == output => 0,
        _ => 1,
    }
}

fn pick_text(pick: Pick) -> (&'static str, &'static str) {
    match pick {
        Pick::Theirs => ("T", "theirs"),
        Pick::Yours => ("Y", "yours"),
        Pick::TheirsThenYours => ("TY", "theirs then yours"),
        Pick::YoursThenTheirs => ("YT", "yours then theirs"),
        Pick::Base => ("B", "base"),
    }
}

fn state_text(resolution: &Resolution) -> String {
    match resolution {
        Resolution::Unresolved => "Conflict".to_string(),
        Resolution::Auto(pick) => format!("Auto: {}", pick_text(*pick).1),
        Resolution::User(pick) => format!("Took {}", pick_text(*pick).1),
        Resolution::Edited(_) => "Edited".to_string(),
    }
}

fn chunk_bg(kind: ChunkKind, resolution: &Resolution) -> Color32 {
    match kind {
        ChunkKind::Unchanged => Color32::TRANSPARENT,
        ChunkKind::Conflict if *resolution == Resolution::Unresolved => CONFLICT_BG,
        ChunkKind::Conflict => RESOLVED_CONFLICT_BG,
        ChunkKind::TheirsOnly | ChunkKind::YoursOnly | ChunkKind::BothSame => ONE_SIDED_BG,
    }
}

/// `changed` flags the tokens drawn as changed; it may be empty.
fn line_content(
    file: &CachedFile<RawToken>,
    lines: &[Range<usize>],
    changed: &[bool],
    line: Option<usize>,
    bg: Color32,
) -> LineContent {
    let Some(line) = line else {
        return LineContent::Void;
    };
    let mut changed_chars = Vec::new();
    let tokens = lines[line]
        .clone()
        .map(|token| {
            let is_changed = changed.get(token) == Some(&true);
            let span = &file.tokens[token].span;
            if is_changed && !span.is_empty() {
                changed_chars.push(span.clone());
            }
            let result = DiffResult {
                // build_row_text maps changed chars only on non-Equal tokens.
                operation: match is_changed {
                    true => DiffOp::Delete,
                    false => DiffOp::Equal(true),
                },
                token_source_idx: Some(token as u32),
                token_target_idx: None,
                hide_in_diff: false,
            };
            (result, false, false)
        })
        .collect();
    LineContent::Code {
        tokens,
        line_num: line as i32 + 1,
        bg: RowBg::Custom(bg),
        changed_chars,
    }
}

fn draw_line(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    (side, row): (ActiveSide, usize),
    occurrences: &mut OccurrenceState,
    file: Option<Arc<CachedFile<RawToken>>>,
    content: &LineContent,
    code_language: &str,
) {
    let mut cell = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(egui::Align::Center)),
    );
    cell.set_clip_rect(rect.intersect(ui.clip_rect()));
    FileDiffPane::render_side_row(
        &mut cell,
        RowCtx {
            side,
            row_index: row,
            occurrences,
            file_source: file,
            file_target: None,
            content,
            width: rect.width(),
            wrap: false,
            is_highlighted: false,
            accent_line_num: false,
            find: RowFind::default(),
            selectable: false,
            code_language,
            trace: None,
        },
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkClick {
    Pick(usize, Pick),
    Edit(usize),
}

fn edit_toggle(ui: &mut egui::Ui, editing: bool) -> bool {
    let button = egui::Button::selectable(editing, egui::RichText::new("✏").small());
    ui.add(button)
        .on_hover_text("Edit the chunk in the output")
        .clicked()
}

fn chunk_strip(
    ui: &mut egui::Ui,
    chunk: usize,
    resolution: &Resolution,
    has_base: bool,
    editing: bool,
    row_h: f32,
) -> Option<ChunkClick> {
    ui.spacing_mut().item_spacing.x = 2.0;
    ui.spacing_mut().button_padding.y = 0.0;
    ui.spacing_mut().interact_size.y = row_h;
    let current = match resolution {
        Resolution::Auto(pick) | Resolution::User(pick) => Some(*pick),
        Resolution::Unresolved | Resolution::Edited(_) => None,
    };
    let mut picked = None;
    for pick in picks(has_base) {
        let (button, name) = pick_text(pick);
        let button =
            egui::Button::selectable(current == Some(pick), egui::RichText::new(button).small());
        if ui
            .add(button)
            .on_hover_text(format!("Take {name}"))
            .clicked()
        {
            picked = Some(ChunkClick::Pick(chunk, pick));
        }
    }
    if edit_toggle(ui, editing) {
        picked = Some(ChunkClick::Edit(chunk));
    }
    let state = egui::RichText::new(state_text(resolution)).small();
    let state = match resolution {
        Resolution::Unresolved => state.color(ui.visuals().error_fg_color),
        _ => state,
    };
    ui.add(egui::Label::new(state).selectable(false));
    picked
}

fn top_panes_ui(
    ui: &mut egui::Ui,
    doc: &MergeDoc,
    editing: Option<usize>,
    occurrences: &mut [OccurrenceState; 3],
    code_language: [&str; 3],
    scroll_to_row: Option<usize>,
    center_jumps: bool,
) -> Option<ChunkClick> {
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
    let pane_w = ((ui.available_width() - STRIP_WIDTH) / 3.0).max(0.0);
    let base_label = match doc.base {
        Some(_) => format!("Base: {}", doc.labels.base),
        None => "Base: none (two-way merge)".to_string(),
    };
    let headers = [
        format!("Theirs: {}", doc.labels.theirs),
        base_label,
        format!("Yours: {}", doc.labels.yours),
    ];
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for header in headers {
            ui.allocate_ui_with_layout(
                egui::vec2(pane_w, ui.spacing().interact_size.y),
                Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_width(pane_w);
                    ui.add(egui::Label::new(egui::RichText::new(header).strong()).truncate());
                },
            );
        }
        ui.label("Chunk");
    });

    let files = [Some(&doc.theirs), doc.base.as_ref(), Some(&doc.yours)];
    let has_base = doc.base.is_some();
    let mut picked = None;
    ui.spacing_mut().item_spacing.y = 0.0;
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt("merge_top_panes")
        .auto_shrink(false);
    if let Some(start) = scroll_to_row {
        let chunk = doc.rows[start].chunk;
        let len = doc.rows[start..]
            .iter()
            .take_while(|row| row.chunk == chunk)
            .count();
        let viewport = ui.available_height();
        scroll = scroll.vertical_scroll_offset(jump_offset(
            start..start + len,
            row_h,
            viewport,
            center_jumps,
        ));
    }
    scroll.show_rows(ui, row_h, doc.rows.len(), |ui, range| {
        for i in range {
            let row = doc.rows[i];
            let resolution = &doc.merge.resolutions()[row.chunk];
            let kind = doc.merge.chunks()[row.chunk].kind;
            let bg = chunk_bg(kind, resolution);
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), row_h),
                egui::Sense::hover(),
            );
            let lines = [row.theirs, row.base, row.yours];
            for pane in 0..3 {
                let cell = egui::Rect::from_min_size(
                    rect.min + egui::vec2(pane as f32 * pane_w, 0.0),
                    egui::vec2(pane_w, row_h),
                );
                let content = match files[pane] {
                    Some(file) => {
                        line_content(file, &doc.lines[pane], &doc.changed[pane], lines[pane], bg)
                    }
                    None => LineContent::Void,
                };
                let side = match pane {
                    1 => ActiveSide::Left,
                    _ => ActiveSide::Right,
                };
                draw_line(
                    ui,
                    cell,
                    (side, i),
                    &mut occurrences[pane],
                    files[pane].cloned(),
                    &content,
                    code_language[pane],
                );
            }
            if doc.current == Some(row.chunk) {
                let bar = egui::Rect::from_min_size(rect.min, egui::vec2(3.0, row_h));
                ui.painter()
                    .rect_filled(bar, 0.0, ui.visuals().selection.bg_fill);
            }
            if row.first && kind != ChunkKind::Unchanged {
                let strip = egui::Rect::from_min_size(
                    rect.min + egui::vec2(3.0 * pane_w + 4.0, 0.0),
                    egui::vec2(STRIP_WIDTH - 4.0, row_h),
                );
                let mut strip_ui = ui.new_child(
                    UiBuilder::new()
                        .max_rect(strip)
                        .layout(Layout::left_to_right(egui::Align::Center)),
                );
                strip_ui.set_clip_rect(strip.intersect(ui.clip_rect()));
                let is_editing = editing == Some(row.chunk);
                if let Some(click) = chunk_strip(
                    &mut strip_ui,
                    row.chunk,
                    resolution,
                    has_base,
                    is_editing,
                    row_h,
                ) {
                    picked = Some(click);
                }
            }
        }
    });
    picked
}

struct ChunkEdit {
    chunk: usize,
    text: String,
    height: f32,
    focus: bool,
}

#[derive(Default)]
struct OutputEvents {
    click: Option<ChunkClick>,
    left_edit: bool,
}

fn origin_mark(ui: &egui::Ui, origin: Origin) -> (&'static str, egui::Color32) {
    match origin {
        Origin::Theirs => ("T", egui::Color32::from_rgb(120, 170, 255)),
        Origin::Yours => ("Y", egui::Color32::from_rgb(120, 210, 140)),
        Origin::Base => ("B", egui::Color32::GRAY),
        Origin::Edited => ("E", egui::Color32::from_rgb(255, 190, 90)),
        Origin::Conflict => ("!", ui.visuals().error_fg_color),
    }
}

/// The edited chunk's rows are replaced by a text box, so rows are placed by hand.
fn output_ui(
    ui: &mut egui::Ui,
    doc: &MergeDoc,
    edit: &mut Option<ChunkEdit>,
    occurrences: &mut OccurrenceState,
    code_language: &str,
) -> OutputEvents {
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
    let gutter = 2.0 * row_h + 8.0;
    let n = doc.output_lines.len();
    let edit_rows = edit.as_ref().map(|e| {
        let range = chunk_range(&doc.output_info, e.chunk);
        (range, e.height.max(row_h))
    });
    let (skipped, edit_h) = match &edit_rows {
        Some((range, h)) => (range.len(), *h),
        None => (0, 0.0),
    };
    let y_of = |line: usize| match &edit_rows {
        Some((range, h)) if line >= range.end => (line - range.len()) as f32 * row_h + h,
        _ => line as f32 * row_h,
    };
    let line_at = |y: f32| match &edit_rows {
        Some((range, h)) if y >= range.start as f32 * row_h => {
            let below = y - range.start as f32 * row_h - h;
            range.end + (below.max(0.0) / row_h) as usize
        }
        _ => (y.max(0.0) / row_h) as usize,
    };
    let editing = edit.as_ref().map(|e| e.chunk);
    let mut events = OutputEvents::default();
    egui::ScrollArea::vertical()
        .id_salt("merge_output_rows")
        .auto_shrink(false)
        .show_viewport(ui, |ui, viewport| {
            ui.set_height((n - skipped) as f32 * row_h + edit_h);
            let origin = ui.max_rect().min;
            let width = ui.available_width();
            let row_rect = |y: f32, h: f32| {
                egui::Rect::from_min_size(origin + egui::vec2(0.0, y), egui::vec2(width, h))
            };
            let mut toggle_at = |ui: &mut egui::Ui, rect: egui::Rect, chunk: usize| {
                let toggle = egui::Rect::from_min_size(rect.min, egui::vec2(row_h + 4.0, row_h));
                let mut toggle_ui = ui.new_child(UiBuilder::new().max_rect(toggle));
                toggle_ui.spacing_mut().button_padding.y = 0.0;
                if edit_toggle(&mut toggle_ui, editing == Some(chunk)) {
                    events.click = Some(ChunkClick::Edit(chunk));
                }
            };

            // Drawn even when scrolled away, so it keeps focus, and before the rows, so the
            // toggle's auto id doesn't move with them.
            let mut left_edit = false;
            if let (Some(e), Some((range, h))) = (edit.as_mut(), &edit_rows) {
                let rect = row_rect(range.start as f32 * row_h, *h);
                toggle_at(ui, rect, e.chunk);
                let mut box_ui =
                    ui.new_child(UiBuilder::new().max_rect(rect.with_min_x(rect.min.x + gutter)));
                let rows = merge::lines(&e.text).len().max(1);
                let response = box_ui.add(
                    egui::TextEdit::multiline(&mut e.text)
                        .id(egui::Id::new("merge_chunk_edit").with(e.chunk))
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(rows),
                );
                if std::mem::take(&mut e.focus) {
                    response.request_focus();
                }
                left_edit = response.lost_focus();
                if response.rect.height() != e.height {
                    e.height = response.rect.height();
                    ui.ctx().request_repaint();
                }
            }

            let visible = line_at(viewport.min.y)..(line_at(viewport.max.y) + 1).min(n);
            for i in visible {
                if edit_rows
                    .as_ref()
                    .is_some_and(|(range, _)| range.contains(&i))
                {
                    continue;
                }
                let rect = row_rect(y_of(i), row_h);
                let info = doc.output_info[i];
                if i == 0 || doc.output_info[i - 1].chunk != info.chunk {
                    toggle_at(ui, rect, info.chunk);
                }
                if let Some(origin) = info.origin {
                    let (mark, color) = origin_mark(ui, origin);
                    let font = egui::TextStyle::Monospace.resolve(ui.style());
                    let at = egui::pos2(rect.min.x + gutter - 4.0, rect.center().y);
                    ui.painter()
                        .text(at, egui::Align2::RIGHT_CENTER, mark, font, color);
                }
                let line = rect.with_min_x(rect.min.x + gutter);
                let content = line_content(
                    &doc.output,
                    &doc.output_lines,
                    &[],
                    Some(i),
                    Color32::TRANSPARENT,
                );
                let file = Some(doc.output.clone());
                draw_line(
                    ui,
                    line,
                    (ActiveSide::Right, i),
                    occurrences,
                    file,
                    &content,
                    code_language,
                );
            }
            events.left_edit = left_edit;
        });
    events
}

enum Input {
    Loading,
    Text(Arc<CachedFile<RawToken>>),
    Failed(String),
}

fn input(file: &mut FileProcessor) -> Input {
    match file.get_loaded_file() {
        Some(loaded) if loaded.is_dump() => Input::Failed(format!(
            "{} is a PDB; its dump can't be merged",
            file.get_path_as_string()
        )),
        Some(loaded) => match loaded.text() {
            Some(text) => Input::Text(text.clone()),
            None => Input::Failed(format!("{} is not a text file", file.get_path_as_string())),
        },
        None if file.get_loading_path().is_some() => Input::Loading,
        None => Input::Failed(format!("Cannot load {}", file.get_path_as_string())),
    }
}

pub struct MergeView {
    base: Option<FileProcessor>,
    theirs: FileProcessor,
    yours: FileProcessor,
    output: UniversalPath,
    output_name: Option<String>,
    algorithm: MyersDiffAlgorithm,
    doc: Option<MergeDoc>,
    edit: Option<ChunkEdit>,
    confirm_pick: Option<(usize, Pick)>,
    top_occurrences: [OccurrenceState; 3],
    output_occurrences: OccurrenceState,
    scroll_to_row: Option<usize>,
    saved: Option<SavedOutput>,
    save_error: Option<String>,
    confirm_save: bool,
    close_prompt: Option<Closing>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closing {
    Merge,
    App,
}

enum CloseAnswer {
    Save,
    Discard,
    Cancel,
}

impl MergeView {
    pub fn new(request: MergeRequest, algorithm: MyersDiffAlgorithm) -> Self {
        let processor = |path: UniversalPath| {
            let mut file = FileProcessor::new();
            file.set_path(path);
            file
        };
        Self {
            base: request.base.map(processor),
            theirs: processor(request.theirs),
            yours: processor(request.yours),
            output: request.output,
            output_name: None,
            algorithm,
            doc: None,
            edit: None,
            confirm_pick: None,
            top_occurrences: Default::default(),
            output_occurrences: OccurrenceState::default(),
            scroll_to_row: None,
            saved: None,
            save_error: None,
            confirm_save: false,
            close_prompt: None,
        }
    }

    pub fn with_names(mut self, names: MergeNames) -> Self {
        if let Some(base) = &mut self.base {
            base.set_label(names.base);
        }
        self.theirs.set_label(names.theirs);
        self.yours.set_label(names.yours);
        self.output_name = names.output;
        self
    }

    fn output_display(&self) -> String {
        match &self.output_name {
            Some(name) => format!("{name} ({})", self.output),
            None => self.output.to_string(),
        }
    }

    pub fn navigate(&mut self, forward: bool) {
        if let Some(doc) = &mut self.doc {
            self.scroll_to_row = doc.navigate(forward);
        }
    }

    pub fn exit_code(&self) -> u8 {
        match &self.doc {
            Some(doc) => exit_code(self.saved.as_ref(), doc.output_text()),
            None => 1,
        }
    }

    pub fn request_save(&mut self) {
        self.store_edit();
        let Some(doc) = &self.doc else {
            log::warn!("Save merge: the inputs are not loaded");
            return;
        };
        if counts(doc.merge()).unresolved > 0 {
            self.confirm_save = true;
        } else {
            self.save();
        }
    }

    fn save(&mut self) -> bool {
        let Some(doc) = &self.doc else {
            return false;
        };
        let path = self
            .output
            .as_path()
            .expect("merge_request only accepts a local output");
        match doc.save(path) {
            Ok(saved) => {
                log::info!("Saved merge output {}", path.display());
                self.saved = Some(saved);
                self.save_error = None;
                true
            }
            Err(e) => {
                let message = format!("Cannot save {}: {e}", path.display());
                log::error!("{message}");
                self.save_error = Some(message);
                false
            }
        }
    }

    /// `Some` when the view can close now; otherwise it prompts and `ui` returns it later.
    pub fn request_close(&mut self, closing: Closing) -> Option<Closing> {
        self.store_edit();
        if self.doc.is_none() || self.exit_code() == 0 {
            return Some(closing);
        }
        self.close_prompt = Some(closing);
        None
    }

    fn confirm_save_ui(&mut self, ctx: &egui::Context) {
        let Some(doc) = &self.doc else {
            self.confirm_save = false;
            return;
        };
        let unresolved = counts(doc.merge()).unresolved;
        let mut answer = None;
        let modal = egui::Modal::new(egui::Id::new("merge_save_prompt")).show(ctx, |ui| {
            ui.heading("Save with unresolved conflicts?");
            ui.label(format!(
                "{unresolved} unresolved conflicts are written as conflict markers."
            ));
            ui.horizontal(|ui| {
                if ui.button("Save with markers").clicked() {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
            });
        });
        if modal.should_close() {
            answer.get_or_insert(false);
        }
        if let Some(save) = answer {
            self.confirm_save = false;
            if save {
                self.save();
            }
        }
    }

    fn close_prompt_ui(&mut self, ctx: &egui::Context, closing: Closing) -> Option<Closing> {
        let unresolved = self
            .doc
            .as_ref()
            .map_or(0, |doc| counts(doc.merge()).unresolved);
        let mut answer = None;
        let modal = egui::Modal::new(egui::Id::new("merge_close_prompt")).show(ctx, |ui| {
            ui.heading("Save the merge before closing?");
            ui.label(format!("Output: {}", self.output_display()));
            if self.saved.as_ref().map(|s| s.text.as_str())
                != self.doc.as_ref().map(MergeDoc::output_text)
            {
                ui.label("The output has unsaved changes.");
            }
            if unresolved > 0 {
                ui.label(format!(
                    "{unresolved} unresolved conflicts are saved as conflict markers."
                ));
            }
            if let Some(error) = &self.save_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    answer = Some(CloseAnswer::Save);
                }
                if ui.button("Discard").clicked() {
                    answer = Some(CloseAnswer::Discard);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(CloseAnswer::Cancel);
                }
            });
        });
        if modal.should_close() {
            answer.get_or_insert(CloseAnswer::Cancel);
        }
        match answer? {
            CloseAnswer::Save if !self.save() => None,
            CloseAnswer::Save | CloseAnswer::Discard => {
                self.close_prompt = None;
                Some(closing)
            }
            CloseAnswer::Cancel => {
                self.close_prompt = None;
                None
            }
        }
    }

    /// Builds the doc once every input is loaded, and again when one reloads.
    fn poll(&mut self) -> Result<(), String> {
        let base = self.base.as_mut().map(input);
        let theirs = input(&mut self.theirs);
        let yours = input(&mut self.yours);
        let (base, theirs, yours) = match (base, theirs, yours) {
            (Some(Input::Failed(e)), _, _)
            | (_, Input::Failed(e), _)
            | (_, _, Input::Failed(e)) => {
                self.doc = None;
                return Err(e);
            }
            (None, Input::Text(theirs), Input::Text(yours)) => (None, theirs, yours),
            (Some(Input::Text(base)), Input::Text(theirs), Input::Text(yours)) => {
                (Some(base), theirs, yours)
            }
            _ => {
                self.doc = None;
                return Ok(());
            }
        };
        let current = self.doc.as_ref().is_some_and(|doc| {
            Arc::ptr_eq(&doc.theirs, &theirs)
                && Arc::ptr_eq(&doc.yours, &yours)
                && match (&doc.base, &base) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                }
        });
        if !current {
            let labels = MergeFileLabels {
                base: self
                    .base
                    .as_ref()
                    .map(|f| f.display_name())
                    .unwrap_or_default(),
                theirs: self.theirs.display_name(),
                yours: self.yours.display_name(),
            };
            self.doc = Some(MergeDoc::new(self.algorithm, base, theirs, yours, labels));
            self.edit = None;
            self.confirm_pick = None;
        }
        Ok(())
    }

    fn store_edit(&mut self) {
        if let (Some(doc), Some(edit)) = (&mut self.doc, &self.edit) {
            doc.edit(edit.chunk, edit.text.clone());
        }
    }

    fn close_edit(&mut self) {
        self.store_edit();
        self.edit = None;
    }

    fn click(&mut self, click: ChunkClick) {
        match click {
            ChunkClick::Edit(chunk) => {
                let toggled_off = self.edit.as_ref().is_some_and(|e| e.chunk == chunk);
                self.close_edit();
                if !toggled_off && let Some(doc) = &self.doc {
                    self.edit = Some(ChunkEdit {
                        chunk,
                        text: doc.chunk_text(chunk),
                        height: 0.0,
                        focus: true,
                    });
                }
            }
            ChunkClick::Pick(chunk, pick) => {
                self.close_edit();
                if let Some(doc) = &mut self.doc
                    && !doc.pick_unless_edited(chunk, pick)
                {
                    self.confirm_pick = Some((chunk, pick));
                }
            }
        }
    }

    pub fn undo_redo(&mut self, redo: bool) {
        self.close_edit();
        if let Some(doc) = &mut self.doc {
            match redo {
                true => doc.redo(),
                false => doc.undo(),
            };
        }
    }

    fn confirm_pick_ui(&mut self, ctx: &egui::Context) {
        let Some((chunk, pick)) = self.confirm_pick else {
            return;
        };
        let mut confirmed = None;
        let modal = egui::Modal::new(egui::Id::new("merge_replace_edit")).show(ctx, |ui| {
            ui.heading("Replace the edit?");
            ui.label(format!(
                "Chunk {} was edited by hand. Take {} and lose the edit?",
                chunk + 1,
                pick_text(pick).1
            ));
            ui.horizontal(|ui| {
                if ui.button("Replace").clicked() {
                    confirmed = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    confirmed = Some(false);
                }
            });
        });
        if modal.should_close() {
            confirmed.get_or_insert(false);
        }
        match confirmed {
            Some(true) => {
                if let Some(doc) = &mut self.doc {
                    doc.pick(chunk, pick);
                }
                self.confirm_pick = None;
            }
            Some(false) => self.confirm_pick = None,
            None => {}
        }
    }

    /// `Some` once the view is done and closes.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        language_override: Option<&str>,
        center_jumps: bool,
    ) -> Option<Closing> {
        let status = self.poll();
        let scroll_to_row = self.scroll_to_row.take();
        let mut close = None;
        let mut save = false;
        let mut undo_redo = None;
        egui::TopBottomPanel::top("merge_toolbar").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Merge");
                match (&status, &self.doc) {
                    (Err(error), _) => {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    (Ok(()), Some(doc)) => {
                        let counts = counts(doc.merge());
                        ui.label(format!(
                            "Unresolved {} of {} chunks",
                            counts.unresolved, counts.total
                        ));
                        if ui.button("Undo").clicked() {
                            undo_redo = Some(false);
                        }
                        if ui.button("Redo").clicked() {
                            undo_redo = Some(true);
                        }
                    }
                    (Ok(()), None) => {
                        ui.spinner();
                        ui.ctx().request_repaint();
                    }
                }
                if let Some(error) = &self.save_error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                save = ui
                    .add_enabled(self.doc.is_some(), egui::Button::new("Save"))
                    .clicked();
                if ui.button("Close Merge").clicked() {
                    close = Some(Closing::Merge);
                }
            });
        });
        if save {
            self.request_save();
        }
        if self.confirm_save {
            self.confirm_save_ui(ui.ctx());
        }
        if let Some(closing) = close {
            close = self.request_close(closing);
        }
        if let Some(closing) = self.close_prompt {
            close = self.close_prompt_ui(ui.ctx(), closing);
        }
        let output_title = format!("Output: {}", self.output_display());
        let Some(doc) = &self.doc else {
            return close;
        };
        let language =
            |detected: Option<&'static str>| language_override.or(detected).unwrap_or("");
        let [theirs, _, yours] = doc.languages;
        let output = egui::TopBottomPanel::bottom("merge_output")
            .resizable(true)
            .default_height(ui.available_height() / 3.0)
            .show_inside(ui, |ui| {
                ui.strong(output_title);
                output_ui(
                    ui,
                    doc,
                    &mut self.edit,
                    &mut self.output_occurrences,
                    language(yours.or(theirs)),
                )
            })
            .inner;
        let editing = self.edit.as_ref().map(|e| e.chunk);
        let top_click = egui::CentralPanel::default()
            .show_inside(ui, |ui| {
                top_panes_ui(
                    ui,
                    doc,
                    editing,
                    &mut self.top_occurrences,
                    doc.languages.map(language),
                    scroll_to_row,
                    center_jumps,
                )
            })
            .inner;

        // Leaving the box stores the edit but keeps the box open; the toggle closes it.
        if output.left_edit {
            self.store_edit();
        }
        if let Some(click) = output.click.or(top_click) {
            self.click(click);
        }
        if let Some(redo) = undo_redo {
            self.undo_redo(redo);
        }
        self.confirm_pick_ui(ui.ctx());
        close
    }
}

pub struct OpenMergeForm {
    pub open: bool,
    fields: [String; 4],
    error: Option<String>,
    ctx: Arc<OnceLock<egui::Context>>,
    browse: Job<(usize, String)>,
}

impl Default for OpenMergeForm {
    fn default() -> Self {
        let ctx = Arc::new(OnceLock::<egui::Context>::new());
        let wake_ctx = ctx.clone();
        Self {
            open: false,
            fields: Default::default(),
            error: None,
            ctx,
            browse: Job::new(move || {
                if let Some(ctx) = wake_ctx.get() {
                    ctx.request_repaint();
                }
            }),
        }
    }
}

impl OpenMergeForm {
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<MergeRequest> {
        self.ctx.get_or_init(|| ctx.clone());
        for (field, path) in self.browse.poll() {
            self.fields[field] = path;
        }
        let mut open = self.open;
        let mut request = None;
        egui::Window::new("Open Merge")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("open_merge_fields")
                    .num_columns(3)
                    .show(ui, |ui| {
                        for (field, name) in MERGE_FIELDS.iter().enumerate() {
                            ui.label(*name);
                            let hint = if field == 0 { "optional" } else { "" };
                            ui.add(
                                egui::TextEdit::singleline(&mut self.fields[field])
                                    .desired_width(400.0)
                                    .hint_text(hint),
                            );
                            if ui.button("...").clicked() {
                                browse(&mut self.browse, field);
                            }
                            ui.end_row();
                        }
                    });
                if let Some(error) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if ui.button("Open").clicked() {
                    match merge_request(&self.fields) {
                        Ok(r) => request = Some(r),
                        Err(error) => self.error = Some(error),
                    }
                }
            });
        if request.is_some() {
            self.error = None;
        }
        self.open = open && request.is_none();
        request
    }
}

fn browse(job: &mut Job<(usize, String)>, field: usize) {
    job.start(move |ctx| {
        let dialog = rfd::FileDialog::new();
        let picked = if MERGE_FIELDS[field] == "Output" {
            dialog.save_file()
        } else {
            dialog.pick_file()
        };
        if let Some(file) = picked {
            ctx.send((field, file.display().to_string()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(
        kind: ChunkKind,
        base: Range<usize>,
        theirs: Range<usize>,
        yours: Range<usize>,
    ) -> MergeChunk {
        MergeChunk {
            kind,
            base,
            theirs,
            yours,
        }
    }

    fn row(
        chunk: usize,
        first: bool,
        t: Option<usize>,
        b: Option<usize>,
        y: Option<usize>,
    ) -> MergeRow {
        MergeRow {
            chunk,
            first,
            theirs: t,
            base: b,
            yours: y,
        }
    }

    fn file(name: &str, text: &str) -> Arc<CachedFile<RawToken>> {
        Arc::new(text_file(UniversalPath::from(name), text.to_string()))
    }

    fn labels() -> MergeFileLabels {
        MergeFileLabels {
            base: "base".into(),
            theirs: "theirs".into(),
            yours: "yours".into(),
        }
    }

    fn doc(base: Option<&str>, theirs: &str, yours: &str) -> MergeDoc {
        MergeDoc::new(
            MyersDiffAlgorithm::Linear,
            base.map(|b| file("base", b)),
            file("theirs", theirs),
            file("yours", yours),
            labels(),
        )
    }

    #[test]
    fn rows_align_each_chunk_to_its_longest_side() {
        let chunks = [
            chunk(ChunkKind::Unchanged, 0..2, 0..2, 0..2),
            chunk(ChunkKind::Conflict, 2..3, 2..4, 2..2),
            chunk(ChunkKind::YoursOnly, 3..3, 4..4, 2..3),
        ];
        assert_eq!(
            aligned_rows(&chunks),
            vec![
                row(0, true, Some(0), Some(0), Some(0)),
                row(0, false, Some(1), Some(1), Some(1)),
                row(1, true, Some(2), Some(2), None),
                row(1, false, Some(3), None, None),
                row(2, true, None, None, Some(2)),
            ]
        );
    }

    #[test]
    fn without_a_base_the_base_column_is_empty() {
        let doc = doc(None, "a\nt\n", "a\ny\nz\n");
        assert_eq!(
            doc.rows,
            vec![
                row(0, true, Some(0), None, Some(0)),
                row(1, true, Some(1), None, Some(1)),
                row(1, false, None, None, Some(2)),
            ]
        );
    }

    #[test]
    fn line_tokens_cover_the_lines_merge_splits() {
        for text in [
            "",
            "a",
            "a\r\nb\r\n",
            "a\rb\r",
            "x /* one\ntwo */ y\n\"s\" // c\n",
            "#define A \\\n  1\nend",
            "s = \"one\ntwo\";\nr = r#\"x\r\ny\"#;\n",
        ] {
            let file = text_file(UniversalPath::from("t"), text.to_string());
            let joined: Vec<&str> = line_tokens(&file)
                .into_iter()
                .map(|l| &text[file.tokens[l.start].span.start..file.tokens[l.end - 1].span.end])
                .collect();
            assert_eq!(joined, merge::lines(text), "{text:?}");
        }
    }

    #[test]
    fn the_counter_counts_unresolved_and_changed_chunks() {
        let mut doc = doc(Some("1\n2\n3\n4\n"), "1\nT\n3\n4\nend\n", "1\nY\n3\n4\n");
        assert_eq!(
            counts(doc.merge()),
            MergeCounts {
                unresolved: 1,
                total: 2
            }
        );
        doc.pick(1, Pick::Theirs);
        assert_eq!(
            counts(doc.merge()),
            MergeCounts {
                unresolved: 0,
                total: 2
            }
        );
    }

    #[test]
    fn stepping_visits_unresolved_conflicts_first_then_all_changed_chunks() {
        use ChunkKind::*;
        let mut merge = Merge::new(
            [
                Unchanged, Conflict, TheirsOnly, Conflict, Unchanged, Conflict, YoursOnly,
            ]
            .map(|kind| chunk(kind, 0..0, 0..0, 0..0))
            .to_vec(),
        );
        let next = |merge: &Merge, at| step_chunk(merge, at, true);
        let prev = |merge: &Merge, at| step_chunk(merge, at, false);

        assert_eq!(next(&merge, None), Some(1));
        assert_eq!(prev(&merge, None), Some(5));
        assert_eq!(next(&merge, Some(1)), Some(3));
        assert_eq!(next(&merge, Some(3)), Some(5));
        assert_eq!(next(&merge, Some(5)), Some(1));
        assert_eq!(prev(&merge, Some(1)), Some(5));
        assert_eq!(next(&merge, Some(2)), Some(3));
        assert_eq!(prev(&merge, Some(2)), Some(1));

        merge.pick(3, Pick::Theirs);
        assert_eq!(next(&merge, Some(3)), Some(5));
        assert_eq!(prev(&merge, Some(3)), Some(1));
        assert_eq!(next(&merge, Some(1)), Some(5));

        merge.pick(1, Pick::Yours);
        assert_eq!(next(&merge, Some(5)), Some(5));
        assert_eq!(prev(&merge, Some(5)), Some(5));

        merge.pick(5, Pick::Base);
        assert_eq!(next(&merge, None), Some(1));
        assert_eq!(next(&merge, Some(1)), Some(2));
        assert_eq!(next(&merge, Some(3)), Some(5));
        assert_eq!(next(&merge, Some(6)), Some(1));
        assert_eq!(prev(&merge, Some(1)), Some(6));
        assert_eq!(prev(&merge, Some(5)), Some(3));
        assert_eq!(prev(&merge, None), Some(6));
    }

    #[test]
    fn stepping_without_changed_chunks_goes_nowhere() {
        let unchanged = Merge::new(vec![chunk(ChunkKind::Unchanged, 0..1, 0..1, 0..1)]);
        assert_eq!(step_chunk(&unchanged, Some(0), true), None);
        assert_eq!(step_chunk(&unchanged, Some(0), false), None);
        for merge in [unchanged, Merge::new(Vec::new())] {
            assert_eq!(step_chunk(&merge, None, true), None);
            assert_eq!(step_chunk(&merge, None, false), None);
        }
    }

    #[test]
    fn navigating_the_doc_goes_to_the_first_row_of_the_chunk() {
        let mut doc = doc(Some("1\n2\n3\n4\n"), "1\nT\n3\n4\nend\n", "1\nY\n3\n4\n");
        assert_eq!(doc.current(), None);
        assert_eq!(doc.navigate(true), Some(1));
        assert_eq!(doc.current(), Some(1));
        doc.pick(1, Pick::Theirs);
        assert_eq!(doc.navigate(true), Some(4));
        assert_eq!(doc.current(), Some(3));
        assert_eq!(doc.navigate(false), Some(1));
    }

    #[test]
    fn each_pick_updates_the_output() {
        let mut doc = doc(Some("1\n2\n3\n"), "1\nT\n3\nend\n", "1\nY\n3\n");
        assert_eq!(
            doc.output_text(),
            "1\n<<<<<<< yours\nY\n||||||| base\n2\n=======\nT\n>>>>>>> theirs\n3\nend\n"
        );
        for (pick, expected) in [
            (Pick::Theirs, "1\nT\n3\nend\n"),
            (Pick::Yours, "1\nY\n3\nend\n"),
            (Pick::TheirsThenYours, "1\nT\nY\n3\nend\n"),
            (Pick::YoursThenTheirs, "1\nY\nT\n3\nend\n"),
            (Pick::Base, "1\n2\n3\nend\n"),
        ] {
            doc.pick(1, pick);
            assert_eq!(doc.output_text(), expected, "{pick:?}");
            assert_eq!(
                doc.output_lines.len(),
                merge::lines(expected).len(),
                "{pick:?}"
            );
        }
        doc.pick(3, Pick::Base);
        assert_eq!(doc.output_text(), "1\n2\n3\n");
    }

    fn changed(doc: &MergeDoc, pane: usize) -> Vec<&str> {
        let file = [Some(&doc.theirs), doc.base.as_ref(), Some(&doc.yours)][pane].unwrap();
        file.tokens
            .iter()
            .zip(&doc.changed[pane])
            .filter(|(token, changed)| **changed && token.kind != TokenKind::Newline)
            .map(|(token, _)| &file.contents[token.span.clone()])
            .collect()
    }

    #[test]
    fn changed_tokens_are_marked_against_base() {
        let doc = doc(
            Some("same\na b c\nend\n"),
            "same\na x c\nend\n",
            "same\na b c\nend\nmore\n",
        );
        assert_eq!(changed(&doc, 0), ["x"]);
        assert_eq!(changed(&doc, 1), ["b"]);
        assert_eq!(changed(&doc, 2), ["more"]);
    }

    #[test]
    fn without_a_base_theirs_and_yours_are_marked_against_each_other() {
        let doc = doc(None, "same\na b\n", "same\na c\n");
        assert_eq!(changed(&doc, 0), ["b"]);
        assert_eq!(changed(&doc, 2), ["c"]);
        assert!(doc.changed[1].is_empty());
    }

    fn origins(doc: &MergeDoc) -> Vec<(usize, Option<Origin>)> {
        doc.output_info
            .iter()
            .map(|l| (l.chunk, l.origin))
            .collect()
    }

    #[test]
    fn output_lines_name_their_chunk_and_origin() {
        use Origin::*;
        let mut doc = doc(Some("1\n2\n3\n"), "1\nT\n3\nend\n", "1\nY\n3\n");
        let conflict = vec![(1, Some(Conflict)); 7];
        let expected = [
            vec![(0, None)],
            conflict,
            vec![(2, None), (3, Some(Theirs))],
        ]
        .concat();
        assert_eq!(origins(&doc), expected);

        doc.pick(1, Pick::YoursThenTheirs);
        let expected = [
            (0, None),
            (1, Some(Yours)),
            (1, Some(Theirs)),
            (2, None),
            (3, Some(Theirs)),
        ];
        assert_eq!(origins(&doc), expected);

        doc.pick(1, Pick::Base);
        assert_eq!(origins(&doc)[1], (1, Some(Base)));

        doc.edit(1, "e1\ne2\ne3".to_string());
        let expected = [
            (0, None),
            (1, Some(Edited)),
            (1, Some(Edited)),
            (1, Some(Edited)),
            (2, None),
            (3, Some(Theirs)),
        ];
        assert_eq!(origins(&doc), expected);

        doc.edit(0, String::new());
        assert_eq!(origins(&doc)[0], (1, Some(Edited)));
    }

    #[test]
    fn output_lines_follow_the_layout_without_a_base() {
        let doc = doc(None, "a\nt\n", "a\ny\nz\n");
        assert_eq!(
            origins(&doc),
            [vec![(0, None)], vec![(1, Some(Origin::Conflict)); 6]].concat()
        );
    }

    #[test]
    fn chunk_text_is_the_chunks_output_with_lf_breaks() {
        let mut crlf = doc(Some("1\r\n2\r\n3"), "1\r\nT\r\n3", "1\r\nY\r\n3");
        assert_eq!(
            crlf.chunk_text(1),
            "<<<<<<< yours\nY\n||||||| base\n2\n=======\nT\n>>>>>>> theirs\n"
        );
        assert_eq!(crlf.chunk_text(0), "1\n");
        assert_eq!(crlf.chunk_text(2), "3");
        crlf.pick(1, Pick::TheirsThenYours);
        assert_eq!(crlf.chunk_text(1), "T\nY\n");

        let mut deleted = doc(Some("1\n2\n3\n"), "1\n2\n3\n", "1\n3\n");
        assert_eq!(deleted.chunk_text(1), "");
        deleted.pick(1, Pick::Theirs);
        assert_eq!(deleted.chunk_text(1), "2\n");
    }

    #[test]
    fn an_edit_replaces_the_chunk_and_the_same_text_is_no_edit() {
        let mut doc = doc(Some("1\n2\n3\n"), "1\nT\n3\n", "1\nY\n3\n");
        doc.edit(1, doc.chunk_text(1));
        assert_eq!(doc.merge().resolutions()[1], Resolution::Unresolved);

        doc.edit(1, "E\n".to_string());
        assert_eq!(
            doc.merge().resolutions()[1],
            Resolution::Edited("E\n".into())
        );
        assert_eq!(doc.output_text(), "1\nE\n3\n");
        assert_eq!(doc.output_lines.len(), 3);
        assert_eq!(counts(doc.merge()).unresolved, 0);

        doc.edit(1, "E\nF".to_string());
        assert_eq!(doc.output_text(), "1\nE\nF\n3\n");
    }

    #[test]
    fn an_edit_may_leave_comments_strings_and_continuations_open() {
        for text in [
            "/* comment out\nfoo\n",
            "s = \"open\nnext\n",
            "r = r#\"raw\nnext\n",
            "#define A \\\n",
            "x \\",
            "' open\n",
        ] {
            let mut doc = doc(Some("1\n2\n3\n"), "1\nT\n3\n", "1\nY\n3\n");
            doc.edit(1, text.to_string());
            let expected = format!("1\n{}\n3\n", text.trim_end_matches('\n'));
            assert_eq!(doc.output_text(), expected);
            assert_eq!(
                doc.output_lines.len(),
                merge::lines(doc.output_text()).len()
            );
            assert_eq!(doc.chunk_text(1).trim_end(), text.trim_end(), "{text:?}");
        }
    }

    #[test]
    fn a_pick_on_an_edited_chunk_waits_for_confirmation() {
        let mut doc = doc(Some("1\n2\n3\n4\n"), "1\nT\n3\nU\n", "1\nY\n3\nV\n");
        assert!(doc.pick_unless_edited(3, Pick::Yours));
        assert_eq!(
            doc.output_text(),
            "1\n<<<<<<< yours\nY\n||||||| base\n2\n=======\nT\n>>>>>>> theirs\n3\nV\n"
        );

        doc.edit(1, "E\n".to_string());
        assert!(!doc.pick_unless_edited(1, Pick::Theirs));
        assert_eq!(doc.output_text(), "1\nE\n3\nV\n");

        doc.pick(1, Pick::Theirs);
        assert_eq!(doc.output_text(), "1\nT\n3\nV\n");
    }

    #[test]
    fn undo_and_redo_restore_picks_and_edits() {
        let mut doc = doc(Some("1\n2\n3\n"), "1\nT\n3\n", "1\nY\n3\n");
        let conflict = doc.output_text().to_string();
        doc.pick(1, Pick::Theirs);
        doc.edit(1, "E\n".to_string());

        assert_eq!(doc.undo(), Some(1));
        assert_eq!(doc.output_text(), "1\nT\n3\n");
        assert_eq!(origins(&doc)[1], (1, Some(Origin::Theirs)));
        assert_eq!(doc.undo(), Some(1));
        assert_eq!(doc.output_text(), conflict);
        assert_eq!(doc.undo(), None);

        assert_eq!(doc.redo(), Some(1));
        assert_eq!(doc.output_text(), "1\nT\n3\n");
        assert_eq!(doc.redo(), Some(1));
        assert_eq!(doc.output_text(), "1\nE\n3\n");
        assert_eq!(origins(&doc)[1], (1, Some(Origin::Edited)));
        assert_eq!(doc.redo(), None);
    }

    fn saved(text: &str, unresolved: usize) -> SavedOutput {
        SavedOutput {
            text: text.to_string(),
            unresolved,
        }
    }

    #[test]
    fn exit_code_is_0_only_for_a_clean_save_with_nothing_changed_since() {
        assert_eq!(exit_code(None, "x\n"), 1);
        assert_eq!(exit_code(Some(&saved("x\n", 0)), "x\n"), 0);
        assert_eq!(exit_code(Some(&saved("x\n", 0)), "y\n"), 1);
        assert_eq!(exit_code(Some(&saved("x\n", 1)), "x\n"), 1);
    }

    #[test]
    fn saving_writes_the_output_with_yours_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("merged.txt");
        let mut doc = doc(Some("1\n2\n3\n"), "1\nT\n3\n", "1\r\nY\r\n3\r\n");

        let first = doc.save(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "<<<<<<< yours\r\n1\r\nY\r\n3\r\n||||||| base\r\n1\r\n2\r\n3\r\n=======\r\n1\r\nT\r\n3\r\n>>>>>>> theirs\r\n"
        );
        assert_eq!(exit_code(Some(&first), doc.output_text()), 1);

        doc.pick(0, Pick::Theirs);
        assert_eq!(exit_code(Some(&first), doc.output_text()), 1);
        let second = doc.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "1\r\nT\r\n3\r\n");
        assert_eq!(exit_code(Some(&second), doc.output_text()), 0);

        doc.pick(0, Pick::Yours);
        assert_eq!(exit_code(Some(&second), doc.output_text()), 1);
    }

    #[test]
    fn save_and_close_include_the_text_in_an_open_edit_box() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name).to_str().unwrap().to_string();
        for (name, text) in [("b", "1\n2\n3\n"), ("t", "1\nT\n3\n"), ("y", "1\nY\n3\n")] {
            std::fs::write(path(name), text).unwrap();
        }
        let request = merge_request(&fields(&path("b"), &path("t"), &path("y"), &path("o")));
        let mut view = MergeView::new(request.unwrap(), MyersDiffAlgorithm::Linear);
        let start = std::time::Instant::now();
        while view.doc.is_none() {
            assert!(start.elapsed().as_secs() < 10, "{:?}", view.poll());
            std::thread::sleep(std::time::Duration::from_millis(5));
            view.poll().unwrap();
        }

        view.click(ChunkClick::Edit(1));
        view.edit.as_mut().unwrap().text = "E\n".to_string();
        view.request_save();
        assert_eq!(std::fs::read_to_string(path("o")).unwrap(), "1\nE\n3\n");
        assert_eq!(view.exit_code(), 0);

        view.edit.as_mut().unwrap().text = "F\n".to_string();
        assert_eq!(view.request_close(Closing::Merge), None);
        assert_eq!(view.exit_code(), 1);
    }

    #[test]
    fn names_replace_the_paths_in_the_conflict_markers() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name).to_str().unwrap().to_string();
        for (name, text) in [("b", "1\n2\n3\n"), ("t", "1\nT\n3\n"), ("y", "1\nY\n3\n")] {
            std::fs::write(path(name), text).unwrap();
        }
        let request = merge_request(&fields(&path("b"), &path("t"), &path("y"), &path("o")));
        let names = MergeNames {
            base: Some("Base".into()),
            theirs: Some("Theirs".into()),
            yours: Some("Yours".into()),
            output: Some("Result".into()),
        };
        let mut view =
            MergeView::new(request.unwrap(), MyersDiffAlgorithm::Linear).with_names(names);
        let start = std::time::Instant::now();
        while view.doc.is_none() {
            assert!(start.elapsed().as_secs() < 10, "{:?}", view.poll());
            std::thread::sleep(std::time::Duration::from_millis(5));
            view.poll().unwrap();
        }

        assert!(view.save());
        assert_eq!(
            std::fs::read_to_string(path("o")).unwrap(),
            "1\n<<<<<<< Yours\nY\n||||||| Base\n2\n=======\nT\n>>>>>>> Theirs\n3\n"
        );
    }

    fn fields(base: &str, theirs: &str, yours: &str, output: &str) -> [String; 4] {
        [base, theirs, yours, output].map(String::from)
    }

    #[test]
    fn a_request_takes_trimmed_paths_and_depot_inputs() {
        let request =
            merge_request(&fields(" //depot/a.txt#3 ", "t.txt", "y.txt ", "out.txt")).unwrap();
        assert_eq!(
            request,
            MergeRequest {
                base: Some(UniversalPath::from("//depot/a.txt#3")),
                theirs: UniversalPath::from("t.txt"),
                yours: UniversalPath::from("y.txt"),
                output: UniversalPath::from("out.txt"),
            }
        );
        assert!(request.base.unwrap().is_depot());
    }

    #[test]
    fn an_empty_base_is_a_two_way_merge() {
        let request = merge_request(&fields("  ", "t", "y", "o")).unwrap();
        assert_eq!(request.base, None);
    }

    #[test]
    fn theirs_yours_and_a_local_output_are_required() {
        for (fields, error) in [
            (fields("b", "", "y", "o"), "Theirs"),
            (fields("b", "t", " ", "o"), "Yours"),
            (fields("b", "t", "y", ""), "Output"),
            (fields("b", "t", "y", "//depot/o.txt"), "Output"),
        ] {
            let message = merge_request(&fields).unwrap_err();
            assert!(message.starts_with(error), "{message}");
        }
    }

    #[test]
    fn base_is_picked_only_with_a_base() {
        assert_eq!(
            picks(true),
            [
                Pick::Theirs,
                Pick::Yours,
                Pick::TheirsThenYours,
                Pick::YoursThenTheirs,
                Pick::Base
            ]
        );
        assert!(!picks(false).contains(&Pick::Base));
    }
}
