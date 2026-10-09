use std::{
    ops::Range,
    sync::{Arc, atomic::AtomicBool, mpsc},
};

use eframe::egui::{self, Layout, UiBuilder};
use zdiff::{
    cached_file::{CachedFile, FileMetadata},
    diff_builder::{Color32, LineContent},
    diff_ir::{DiffOp, DiffResult},
    lexer::{LEXER_MODE_DEFAULT, LexerDefault, RawToken, TokenKind},
    merge::{self, ChunkKind, Merge, MergeChunk, MergeLabels, MergeTexts, Pick, Resolution},
    myers::MyersDiffAlgorithm,
    universal_path::UniversalPath,
};

use crate::{
    file::FileProcessor,
    ui_egui::{
        active_side::ActiveSide,
        diff_pane::FileDiffPane,
        occurrence::{OccurrenceState, RowFind},
    },
};

const STRIP_WIDTH: f32 = 220.0;
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

pub const MERGE_FIELDS: [&str; 4] = ["Base", "Theirs", "Yours", "Output"];

/// Fields in `MERGE_FIELDS` order. An empty base is a two-way merge.
pub fn merge_request(fields: &[String; 4]) -> Result<MergeRequest, String> {
    let path = |i: usize| {
        let text = fields[i].trim();
        (!text.is_empty()).then(|| UniversalPath::from(text))
    };
    let required = |i: usize| path(i).ok_or_else(|| format!("{} is required", MERGE_FIELDS[i]));
    let output = required(3)?;
    if output.is_depot() {
        return Err("Output must be a local path".to_string());
    }
    Ok(MergeRequest {
        base: path(0),
        theirs: required(1)?,
        yours: required(2)?,
        output,
    })
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
    let tokens = LexerDefault::<RawToken>::new(&contents).parse();
    let metadata = FileMetadata::new(&contents);
    CachedFile {
        path,
        hash: String::new(),
        contents,
        tokens,
        metadata,
        lexer_mode: LEXER_MODE_DEFAULT,
    }
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
    merge: Merge,
    rows: Vec<MergeRow>,
    output: Arc<CachedFile<RawToken>>,
    output_lines: Vec<Range<usize>>,
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
        let mut doc = Self {
            rows: aligned_rows(&chunks),
            merge: Merge::new(chunks),
            output: Arc::new(text_file(UniversalPath::new(""), String::new())),
            output_lines: Vec::new(),
            current: None,
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

fn line_content(lines: &[Range<usize>], line: Option<usize>, bg: Color32) -> LineContent {
    let Some(line) = line else {
        return LineContent::Void;
    };
    let tokens = lines[line]
        .clone()
        .map(|token| {
            let result = DiffResult {
                operation: DiffOp::Equal(true),
                token_source_idx: Some(token as u32),
                token_target_idx: None,
                hide_in_diff: false,
            };
            (result, Color32::WHITE, false)
        })
        .collect();
    LineContent::Code {
        tokens,
        line_num: line as i32 + 1,
        bg,
        changed_chars: Vec::new(),
    }
}

fn draw_line(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    row: usize,
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
        ActiveSide::Left,
        row,
        occurrences,
        file,
        None,
        content,
        rect.width(),
        false,
        false,
        false,
        RowFind::default(),
        false,
        code_language,
        None,
    );
}

fn chunk_strip(
    ui: &mut egui::Ui,
    resolution: &Resolution,
    has_base: bool,
    row_h: f32,
) -> Option<Pick> {
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
            picked = Some(pick);
        }
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
    occurrences: &mut [OccurrenceState; 3],
    code_language: &str,
    scroll_to_row: Option<usize>,
) -> Option<(usize, Pick)> {
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
    if let Some(row) = scroll_to_row {
        scroll = scroll.vertical_scroll_offset(row as f32 * row_h);
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
                let content = line_content(&doc.lines[pane], lines[pane], bg);
                let file = files[pane].cloned();
                draw_line(
                    ui,
                    cell,
                    i,
                    &mut occurrences[pane],
                    file,
                    &content,
                    code_language,
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
                if let Some(pick) = chunk_strip(&mut strip_ui, resolution, has_base, row_h) {
                    picked = Some((row.chunk, pick));
                }
            }
        }
    });
    picked
}

fn output_ui(
    ui: &mut egui::Ui,
    doc: &MergeDoc,
    occurrences: &mut OccurrenceState,
    code_language: &str,
) {
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical()
        .id_salt("merge_output_rows")
        .auto_shrink(false)
        .show_rows(ui, row_h, doc.output_lines.len(), |ui, range| {
            for i in range {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), row_h),
                    egui::Sense::hover(),
                );
                let content = line_content(&doc.output_lines, Some(i), Color32::TRANSPARENT);
                let file = Some(doc.output.clone());
                draw_line(ui, rect, i, occurrences, file, &content, code_language);
            }
        });
}

enum Input {
    Loading,
    Text(Arc<CachedFile<RawToken>>),
    Failed(String),
}

fn input(file: &mut FileProcessor) -> Input {
    match file.get_loaded_file() {
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
    algorithm: MyersDiffAlgorithm,
    doc: Option<MergeDoc>,
    top_occurrences: [OccurrenceState; 3],
    output_occurrences: OccurrenceState,
    scroll_to_row: Option<usize>,
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
            algorithm,
            doc: None,
            top_occurrences: Default::default(),
            output_occurrences: OccurrenceState::default(),
            scroll_to_row: None,
        }
    }

    pub fn navigate(&mut self, forward: bool) {
        if let Some(doc) = &mut self.doc {
            self.scroll_to_row = doc.navigate(forward);
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
        }
        Ok(())
    }

    /// True when the view asks to be closed.
    pub fn ui(&mut self, ui: &mut egui::Ui, code_language: &str) -> bool {
        let status = self.poll();
        let scroll_to_row = self.scroll_to_row.take();
        let mut close = false;
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
                    }
                    (Ok(()), None) => {
                        ui.spinner();
                        ui.ctx().request_repaint();
                    }
                }
                if ui.button("Close Merge").clicked() {
                    close = true;
                }
            });
        });
        let Some(doc) = &mut self.doc else {
            return close;
        };
        egui::TopBottomPanel::bottom("merge_output")
            .resizable(true)
            .default_height(ui.available_height() / 3.0)
            .show_inside(ui, |ui| {
                ui.strong(format!("Output: {}", self.output));
                output_ui(ui, doc, &mut self.output_occurrences, code_language);
            });
        let picked = egui::CentralPanel::default()
            .show_inside(ui, |ui| {
                top_panes_ui(
                    ui,
                    doc,
                    &mut self.top_occurrences,
                    code_language,
                    scroll_to_row,
                )
            })
            .inner;
        if let Some((chunk, pick)) = picked {
            doc.pick(chunk, pick);
        }
        close
    }
}

pub struct OpenMergeForm {
    pub open: bool,
    fields: [String; 4],
    error: Option<String>,
    browse_tx: mpsc::Sender<(usize, String)>,
    browse_rx: mpsc::Receiver<(usize, String)>,
}

impl Default for OpenMergeForm {
    fn default() -> Self {
        let (browse_tx, browse_rx) = mpsc::channel();
        Self {
            open: false,
            fields: Default::default(),
            error: None,
            browse_tx,
            browse_rx,
        }
    }
}

impl OpenMergeForm {
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<MergeRequest> {
        while let Ok((field, path)) = self.browse_rx.try_recv() {
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
                                browse(ctx.clone(), field, self.browse_tx.clone());
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

fn browse(ctx: egui::Context, field: usize, tx: mpsc::Sender<(usize, String)>) {
    std::thread::spawn(move || {
        let dialog = rfd::AsyncFileDialog::new();
        let picked = if MERGE_FIELDS[field] == "Output" {
            pollster::block_on(dialog.save_file())
        } else {
            pollster::block_on(dialog.pick_file())
        };
        if let Some(file) = picked {
            if let Err(e) = tx.send((field, file.path().display().to_string())) {
                log::error!("Failed to send path: {e}");
            }
            ctx.request_repaint();
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
