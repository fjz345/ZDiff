use std::{
    collections::BTreeMap,
    ops::{Range, RangeInclusive},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use eframe::egui;
use rayon::prelude::*;
use zcommon::find::{FindError, FindQuery, Matcher};
use zcommon::job::Job;
use zdiff::universal_path::UniversalPath;
use zdiff::{
    cached_file::CachedFile,
    change_count::{ChangeUnit, count_changes},
    char_diff::{CharRanges, character_diff_traced},
    diff_builder::{
        DiffBuilderOptions, DiffRow, LineContent, RowBg, build_diff_rows,
        build_diff_rows_cancellable,
    },
    diff_ir::{DiffIR, DiffOp},
    ignore::IgnoreOptions,
    lexer::{RawToken, TokenKind},
    myers::{
        DiffPhases, MyersDiffAlgorithm, MyersNumAddDelete, MyersPath, Pin, PinError, Side,
        line_diff, myers_count_add_deletes, myers_diff_path_traced, token_diff, validate_pins,
    },
    row_text::build_row_text,
    scope::{changed_scopes, scope_lines},
    trace::PhaseTrace,
};

use crate::{
    clamped_cursor::ClampedCursor,
    scope,
    ui_egui::active_side::ActiveSide,
    viewer::audio::{RepaintContext, repaint_job},
};

#[derive(Debug, Default, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct UpdateDiffRowsInput {
    #[cfg_attr(feature = "serde", serde(skip))]
    pub file_1: Option<Arc<CachedFile<RawToken>>>,
    #[cfg_attr(feature = "serde", serde(skip))]
    pub file_2: Option<Arc<CachedFile<RawToken>>>,
    pub options: DiffBuilderOptions,
    pub myers_diff_algorithm: MyersDiffAlgorithm,
    /// `DiffProcessor::request_update` overwrites it with the processor's pins.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub pins: Vec<Pin>,
    #[cfg_attr(feature = "serde", serde(skip))]
    pub collect_trace: bool,
    #[cfg_attr(feature = "serde", serde(skip))]
    pub pdb_dumps: [bool; 2],
    #[cfg_attr(feature = "serde", serde(skip))]
    pub strings_view: bool,
}
impl PartialEq for UpdateDiffRowsInput {
    fn eq(&self, other: &Self) -> bool {
        self.file_1.as_ref().map(|f| &f.hash) == other.file_1.as_ref().map(|f| &f.hash)
            && self.file_2.as_ref().map(|f| &f.hash) == other.file_2.as_ref().map(|f| &f.hash)
            && self.options == other.options
            && self.myers_diff_algorithm == other.myers_diff_algorithm
            && self.pins == other.pins
            && self.collect_trace == other.collect_trace
            && self.pdb_dumps == other.pdb_dumps
            && self.strings_view == other.strings_view
    }
}

/// A row shows its line's text, so `ordinal` picks the same match the pane paints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindHit {
    pub side: ActiveSide,
    /// 0-based. `row` is rebuilt from it after an expansion.
    pub line: usize,
    pub ordinal: usize,
    pub row: usize,
}

#[derive(Debug, Clone, Default)]
pub struct FindCtx {
    query: FindQuery,
    /// `None` for an empty query.
    matcher: Option<Matcher>,
    /// `None`: both sides.
    side: Option<ActiveSide>,
    /// Sorted by row, then side (left first), then position.
    hits: Vec<FindHit>,
}
impl FindCtx {
    /// Searches both sides. An empty query gives no hits; only an invalid regex is an error.
    #[cfg(test)]
    pub fn new(query: &FindQuery, diff_ctx: &MinimalDiffCtx) -> Result<Self, FindError> {
        let matcher = Self::compile(query)?;
        let files = [&diff_ctx.input.file_1, &diff_ctx.input.file_2];
        let find_ctx = Self::searched(
            query.clone(),
            matcher,
            files,
            None,
            &diff_ctx.precomputed_file_rows,
            &AtomicBool::new(false),
        );
        Ok(find_ctx.expect("never cancelled"))
    }

    fn compile(query: &FindQuery) -> Result<Option<Matcher>, FindError> {
        match Matcher::new(query) {
            Ok(matcher) => Ok(Some(matcher)),
            Err(FindError::EmptyQuery) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn searched(
        query: FindQuery,
        matcher: Option<Matcher>,
        files: [&Option<Arc<CachedFile<RawToken>>>; 2],
        side: Option<ActiveSide>,
        file_rows: &PrecomputedFileRows,
        cancel: &AtomicBool,
    ) -> Option<Self> {
        let search = |file_side, file: &Option<Arc<CachedFile<RawToken>>>| match (file, &matcher) {
            (Some(file), Some(matcher)) if side.is_none_or(|side| side == file_side) => {
                Self::search(file, matcher, file_side, cancel)
            }
            _ => Vec::new(),
        };
        let (left, right) = rayon::join(
            || search(ActiveSide::Left, files[0]),
            || search(ActiveSide::Right, files[1]),
        );
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let mut find_ctx = Self {
            query,
            matcher,
            side,
            hits: [left, right].concat(),
        };
        find_ctx.map_to_rows(file_rows);
        log::debug!(
            "create_find_ctx: {} hits for {:?}",
            find_ctx.hits.len(),
            find_ctx.query
        );
        Some(find_ctx)
    }

    #[cfg(test)]
    pub fn query(&self) -> &FindQuery {
        &self.query
    }

    pub fn matcher_on(&self, side: ActiveSide) -> Option<&Matcher> {
        self.matcher
            .as_ref()
            .filter(|_| self.side.is_none_or(|searched| searched == side))
    }

    pub fn hits(&self) -> &[FindHit] {
        &self.hits
    }

    /// Searches each line as the pane's row text has it, so ordinals match what the pane paints.
    fn search(
        file: &CachedFile<RawToken>,
        matcher: &Matcher,
        side: ActiveSide,
        cancel: &AtomicBool,
    ) -> Vec<FindHit> {
        let contents = &file.contents;
        // `line_starts` holds the index of each line's '\n', not of the byte after it.
        let starts = &file.metadata.line_starts;
        (0..starts.len())
            .into_par_iter()
            .with_min_len(4096)
            .flat_map_iter(|line| {
                let cancelled = cancel.load(Ordering::Relaxed);
                let start = if line == 0 {
                    starts[0]
                } else {
                    starts[line] + 1
                };
                let end = starts.get(line + 1).copied().unwrap_or(contents.len());
                let text = if cancelled { "" } else { &contents[start..end] };
                let text = text.strip_suffix('\r').unwrap_or(text);
                matcher
                    .find_iter(text)
                    .enumerate()
                    .map(move |(ordinal, _)| FindHit {
                        side,
                        line,
                        ordinal,
                        row: 0,
                    })
            })
            .collect()
    }

    fn map_to_rows(&mut self, file_rows: &PrecomputedFileRows) {
        for hit in &mut self.hits {
            let line_to_row = match hit.side {
                ActiveSide::Left => &file_rows.0,
                ActiveSide::Right => &file_rows.1,
            };
            hit.row = line_to_row[hit.line];
        }
        self.hits.sort_by_key(|hit| {
            (
                hit.row,
                hit.side == ActiveSide::Right,
                hit.line,
                hit.ordinal,
            )
        });
    }
}

#[derive(Debug)]
pub struct DiffSpan {
    start: usize,
    end: usize,
}
impl DiffSpan {
    pub fn rows(&self) -> RangeInclusive<usize> {
        self.start..=self.end
    }
}
pub type PrecomputedDiffs = Vec<DiffSpan>; // list spans with indicies of diff_rows of DiffOp != Equal from diff_rows
pub type PrecomputedFileRows = (Vec<usize>, Vec<usize>); // line mapping from DiffRow index to DiffRow line number
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct ScrollSpan {
    pub start: usize,
    pub maybe_end: Option<usize>,
    pub side: Option<ActiveSide>,
}
pub type DiffRows = Vec<DiffRow>; // Span with optional end

#[derive(Debug, PartialEq, Clone, Copy)]
enum HighlightSource {
    Conflict,
    Find,
    Goto,
}

/// The rows a `Collapsed` row stands for, kept so the block can be expanded without
/// recomputing the diff.
#[derive(Debug)]
pub struct CollapsedBlock {
    /// Index of the block's `Collapsed` row in the unexpanded rows. Identifies the block.
    pub row: usize,
    hidden: Vec<DiffRow>,
}
pub type CollapsedBlocks = Vec<CollapsedBlock>;

/// Where a collapsed block sits in the shown rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowBlock {
    /// `CollapsedBlock::row` of the block.
    pub key: usize,
    /// The block's `Collapsed` row while any of its rows is hidden, then its revealed rows. Rows
    /// are revealed from the block's bottom up.
    pub rows: Range<usize>,
    /// Every hidden row is revealed, so the block has no `Collapsed` row.
    pub expanded: bool,
}

/// What a collapsed block's buttons ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockToggle {
    Expand,
    ExpandToScope,
    Collapse,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MyersCtxInput {
    pub file_1: Option<Arc<CachedFile<RawToken>>>,
    pub file_2: Option<Arc<CachedFile<RawToken>>>,
    pub algo: MyersDiffAlgorithm,
    /// Ignore options decide line equality, so changing them recomputes this stage.
    pub ignore: IgnoreOptions,
    pub phases: DiffPhases,
    pub pins: Vec<Pin>,
    pub collect_trace: bool,
}
impl From<UpdateDiffRowsInput> for MyersCtxInput {
    fn from(input: UpdateDiffRowsInput) -> Self {
        Self {
            file_1: input.file_1,
            file_2: input.file_2,
            algo: input.myers_diff_algorithm,
            ignore: input.options.ignore,
            phases: input.options.phases,
            pins: input.pins,
            collect_trace: input.collect_trace,
        }
    }
}

impl From<&UpdateDiffRowsInput> for MyersCtxInput {
    fn from(input: &UpdateDiffRowsInput) -> Self {
        Self {
            file_1: input.file_1.clone(),
            file_2: input.file_2.clone(),
            algo: input.myers_diff_algorithm,
            ignore: input.options.ignore.clone(),
            phases: input.options.phases,
            pins: input.pins.clone(),
            collect_trace: input.collect_trace,
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct DiffIRInput {
    pub myers_path: Arc<MyersPath>,
    pub char_ranges: Arc<CharRanges>,
}
#[derive(Debug, Clone)]
pub struct DiffRowsInput {
    pub file_1: Option<Arc<CachedFile<RawToken>>>,
    pub file_2: Option<Arc<CachedFile<RawToken>>>,
    pub diff_ir: Arc<DiffIR>,
    pub diff_options: DiffBuilderOptions,
}
impl PartialEq for DiffRowsInput {
    fn eq(&self, other: &Self) -> bool {
        self.file_1 == other.file_1
            && self.file_2 == other.file_2
            && self.diff_ir == other.diff_ir
            && !DiffBuilderOptions::need_invalidation(&self.diff_options, &other.diff_options)
    }
}
#[derive(Debug)]
pub struct MyersCtx {
    input: MyersCtxInput,

    num_add_delete: MyersNumAddDelete,
    change_unit: Option<ChangeUnit>,
    path: Arc<MyersPath>,
    char_ranges: Arc<CharRanges>,
    ignored_pins: Option<PinError>,
    phase_trace: Option<Arc<PhaseTrace>>,
    line_elapsed: Duration,
    token_elapsed: Duration,
}
#[derive(Debug)]
pub struct DiffIRCtx {
    input: DiffIRInput,
    diff_ir: Arc<DiffIR>,
    elapsed: Duration,
}
#[derive(Debug)]
pub struct DiffRowsCtx {
    input: DiffRowsInput,
    rows: Arc<DiffRows>,
    precomputed_diffs: Arc<PrecomputedDiffs>,
    precomputed_file_rows: Arc<PrecomputedFileRows>,
    collapsed_blocks: Arc<CollapsedBlocks>,
    /// Every block unexpanded.
    row_blocks: Arc<Vec<RowBlock>>,
    changed_scopes: Arc<[Vec<Range<usize>>; 2]>,
    elapsed: Duration,
}

/// Compute time of each stage behind a diff. A stage served from its cache keeps the time it
/// took when it ran, so the total is what the shown diff cost, not what the last request did.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct DiffStageTimes {
    /// The Myers stage's two phases.
    pub line_diff: Duration,
    pub token_diff: Duration,
    pub diff_ir: Duration,
    pub diff_rows: Duration,
}
impl DiffStageTimes {
    pub fn total(&self) -> Duration {
        self.line_diff + self.token_diff + self.diff_ir + self.diff_rows
    }
}

#[derive(Debug)]
pub struct MinimalDiffCtx {
    #[cfg(debug_assertions)]
    pub debug_file_1_path: UniversalPath,
    #[cfg(debug_assertions)]
    pub debug_file_2_path: UniversalPath,

    pub input: UpdateDiffRowsInput,

    pub num_add_deletes: MyersNumAddDelete,
    /// The unit `num_add_deletes` counts in; `None` with every phase off.
    pub change_unit: Option<ChangeUnit>,
    /// Why the input's pins don't fit its files; the diff then ran without them.
    pub ignored_pins: Option<PinError>,
    /// Only when the input's `collect_trace` is set.
    pub phase_trace: Option<Arc<PhaseTrace>>,
    pub stage_times: DiffStageTimes,
    pub precomputed_diffs: Arc<PrecomputedDiffs>,
    pub precomputed_file_rows: Arc<PrecomputedFileRows>,
    pub diff_rows: Arc<DiffRows>,
    /// The IR the rows were built from. Revert reads the token alignment from it.
    pub diff_ir: Arc<DiffIR>,
    /// Hidden rows of each collapsed block, ordered by row.
    pub collapsed_blocks: Arc<CollapsedBlocks>,
    /// Each collapsed block's place in `diff_rows`, ordered by row.
    pub row_blocks: Arc<Vec<RowBlock>>,
    /// Per side, 0-based line ranges of the top-level scopes holding a visible change.
    pub changed_scopes: Arc<[Vec<Range<usize>>; 2]>,
}

macro_rules! check_cancel {
    ($flag:expr, $step:expr) => {
        if $flag.load(Ordering::Relaxed) {
            log::debug!("cancel_flag: {}", $step);
            return None;
        }
    };
}

#[cfg(feature = "debug_alloc")]
macro_rules! track_alloc {
    ($reg:expr, $step:expr) => {
        log::debug!("Allocations {}: {:?}", $step, $reg.change_and_reset());
    };
}

#[cfg(not(feature = "debug_alloc"))]
macro_rules! track_alloc {
    ($reg:expr, $step:expr) => {};
}

macro_rules! poll_ctx_channel {
    ($channel:expr, $inflight:expr, $ctx:expr, $transform:expr) => {
        for result in $channel.poll() {
            match result {
                Some(res) => {
                    if let Some(pending) = &$inflight {
                        if *pending == res.input {
                            $ctx = $transform(Some(res));
                            $inflight = None;
                        }
                    }
                }
                None => {
                    $inflight = None;
                }
            }
        }
    };
    ($channel:expr, $inflight:expr, $ctx:expr) => {
        poll_ctx_channel!($channel, $inflight, $ctx, |x| x)
    };
}

#[derive(Debug)]
pub struct DiffCtx {
    pub update_diff_rows_input: UpdateDiffRowsInput,

    #[cfg(debug_assertions)]
    pub debug_file_1_path: UniversalPath,
    #[cfg(debug_assertions)]
    pub debug_file_2_path: UniversalPath,

    myers_job: Job<Option<MyersCtx>>,
    diff_ir_job: Job<Option<DiffIRCtx>>,
    diff_rows_job: Job<Option<DiffRowsCtx>>,

    myers_inflight_input: Option<MyersCtxInput>,
    myers_ctx: Option<MyersCtx>,

    diff_ir_inflight_input: Option<DiffIRInput>,
    diff_ir_ctx: Option<DiffIRCtx>,

    diff_rows_inflight_input: Option<DiffRowsInput>,
    diff_rows_ctx: Option<Arc<DiffRowsCtx>>,

    repaint: RepaintContext,
}
impl Default for DiffCtx {
    fn default() -> Self {
        Self::new(
            UpdateDiffRowsInput {
                file_1: None,
                file_2: None,
                options: Default::default(),
                myers_diff_algorithm: Default::default(),
                pins: Vec::new(),
                collect_trace: false,
                pdb_dumps: [false; 2],
                strings_view: false,
            },
            RepaintContext::default(),
        )
    }
}

#[derive(Debug)]
pub enum OneSidedMode {
    TwoSided,
    OnlyLeft,  // No right file
    OnlyRight, // No left file
}

impl DiffCtx {
    #[allow(dead_code)]
    pub fn new(input: UpdateDiffRowsInput, repaint: RepaintContext) -> Self {
        Self {
            update_diff_rows_input: input.clone(),
            #[cfg(debug_assertions)]
            debug_file_1_path: input
                .file_1
                .as_ref()
                .map(|f| f.path.clone())
                .unwrap_or_default(),
            #[cfg(debug_assertions)]
            debug_file_2_path: input
                .file_2
                .as_ref()
                .map(|f| f.path.clone())
                .unwrap_or_default(),
            myers_job: repaint_job(&repaint),
            diff_ir_job: repaint_job(&repaint),
            diff_rows_job: repaint_job(&repaint),
            myers_inflight_input: None,
            myers_ctx: None,
            diff_ir_inflight_input: None,
            diff_ir_ctx: None,
            diff_rows_inflight_input: None,
            diff_rows_ctx: None,
            repaint,
        }
    }

    pub fn get_one_sided_mode(&self) -> OneSidedMode {
        match (
            self.update_diff_rows_input.file_1.is_some(),
            self.update_diff_rows_input.file_2.is_some(),
        ) {
            (true, true) => OneSidedMode::TwoSided,
            (true, false) => OneSidedMode::OnlyLeft,
            (false, true) => OneSidedMode::OnlyRight,
            (false, false) => panic!("Only call this function with one of two files valid"),
        }
    }

    pub fn set_input(&mut self, input: UpdateDiffRowsInput) {
        log::info!(
            "Diff Ctx recieved new input:\nSource: {}\nTarget: {}",
            &input
                .file_1
                .as_ref()
                .map(|f| f.path.to_string())
                .unwrap_or("None".to_string()),
            &input
                .file_2
                .as_ref()
                .map(|f| f.path.to_string())
                .unwrap_or("None".to_string()),
        );
        self.update_diff_rows_input = input;
    }

    /// A cancelled stage never sends, so its in-flight input would never clear and would block
    /// restarting the same input later.
    pub fn cancel_all(&mut self) {
        self.myers_job.cancel();
        self.diff_ir_job.cancel();
        self.diff_rows_job.cancel();
        self.myers_inflight_input = None;
        self.diff_ir_inflight_input = None;
        self.diff_rows_inflight_input = None;
    }

    pub fn poll(&mut self) {
        for myers_res in self.myers_job.poll() {
            match myers_res {
                Some(res) => {
                    if Some(&res.input) == self.myers_inflight_input.as_ref() {
                        self.myers_ctx = Some(res);
                        self.myers_inflight_input = None;
                    }
                }
                None => self.myers_inflight_input = None,
            }
        }

        for ir_res in self.diff_ir_job.poll() {
            match ir_res {
                Some(res) => {
                    if Some(&res.input) == self.diff_ir_inflight_input.as_ref() {
                        self.diff_ir_ctx = Some(res);
                        self.diff_ir_inflight_input = None;
                    }
                }
                None => self.diff_ir_inflight_input = None,
            }
        }

        for rows_res in self.diff_rows_job.poll() {
            match rows_res {
                Some(res) => {
                    if Some(&res.input) == self.diff_rows_inflight_input.as_ref() {
                        self.diff_rows_ctx = Some(Arc::new(res));
                        self.diff_rows_inflight_input = None;
                    }
                }
                None => self.diff_rows_inflight_input = None,
            }
        }
    }

    pub fn request_myers(&mut self) -> Option<&MyersCtx> {
        let expected_input: MyersCtxInput = (&self.update_diff_rows_input).into();

        poll_ctx_channel!(self.myers_job, self.myers_inflight_input, self.myers_ctx);

        if self
            .myers_ctx
            .as_ref()
            .map_or(false, |ctx| ctx.input == expected_input)
        {
            return self.myers_ctx.as_ref();
        }

        if expected_input.file_1.is_none() && expected_input.file_2.is_none() {
            return None;
        }

        if self.myers_inflight_input.as_ref() != Some(&expected_input) {
            self.myers_inflight_input = Some(expected_input.clone());
            let input = expected_input;

            log::info!("new request_myers");
            self.myers_job.start(move |job| {
                let cancel = job.cancel_flag().clone();
                let start = Instant::now();
                let (c1, c2, one_sided) = resolve_files(&input.file_1, &input.file_2);
                let cmp = |a: &RawToken, b: &RawToken| compare_tokens(a, b, c1, c2);

                let (t1, t2) = (&c1.tokens, &c2.tokens);
                let ignore = input.ignore.mask(t1, &c1.contents, t2, &c2.contents);
                let pins: &[Pin] = if one_sided.is_some() {
                    &[]
                } else {
                    &input.pins
                };
                let ignored_pins =
                    validate_pins(pins, pin_line_count(t1), pin_line_count(t2)).err();
                if let Some(err) = &ignored_pins {
                    log::warn!("Diffing without pins: {err}");
                }
                let pins: &[Pin] = if ignored_pins.is_some() { &[] } else { pins };
                let phases = input.phases;
                let mut trace = input.collect_trace.then(PhaseTrace::default);
                // The default phases without pins run as the driver does, but timed separately.
                let result = if pins.is_empty()
                    && phases.line
                    && phases.token
                    && !phases.scope
                    && trace.is_none()
                {
                    line_diff(
                        input.algo,
                        t1,
                        t2,
                        token_key(c1, c2),
                        &ignore,
                        cancel.clone(),
                    )
                    .and_then(|hunks| {
                        let line_elapsed = start.elapsed();
                        let start = Instant::now();
                        let path =
                            token_diff(input.algo, t1, t2, &hunks, &cmp, &ignore, cancel.clone())?;
                        Some((path, line_elapsed, start.elapsed()))
                    })
                } else {
                    myers_diff_path_traced(
                        input.algo,
                        t1,
                        t2,
                        &c1.contents,
                        &c2.contents,
                        cmp,
                        &ignore,
                        phases,
                        pins,
                        cancel.clone(),
                        trace.as_mut(),
                    )
                    .map(|path| {
                        if phases.token {
                            (path, Duration::ZERO, start.elapsed())
                        } else {
                            (path, start.elapsed(), Duration::ZERO)
                        }
                    })
                };
                let result = result.and_then(|(path, line_elapsed, mut token_elapsed)| {
                    let mut char_ranges = CharRanges::default();
                    if phases.character {
                        let start = Instant::now();
                        char_ranges = character_diff_traced(
                            input.algo,
                            &path,
                            t1,
                            t2,
                            &c1.contents,
                            &c2.contents,
                            &ignore,
                            cancel.clone(),
                            trace.as_mut(),
                        )?;
                        token_elapsed += start.elapsed();
                    }
                    Some((path, char_ranges, line_elapsed, token_elapsed))
                });
                if let Some((path, char_ranges, line_elapsed, token_elapsed)) = result {
                    let change_unit = ChangeUnit::finest(phases);
                    let num_add_delete = change_unit.map_or_else(Default::default, |unit| {
                        count_changes(
                            unit,
                            &path,
                            t1,
                            t2,
                            &c1.contents,
                            &c2.contents,
                            &char_ranges,
                        )
                    });
                    job.send(Some(MyersCtx {
                        input,
                        num_add_delete,
                        change_unit,
                        path: Arc::new(path),
                        char_ranges: Arc::new(char_ranges),
                        ignored_pins,
                        phase_trace: trace.map(Arc::new),
                        line_elapsed,
                        token_elapsed,
                    }));
                } else {
                    job.send(None);
                }
            });
        }
        None
    }

    pub fn request_diff_ir(&mut self) -> Option<&DiffIRCtx> {
        let myers_ctx = self.request_myers()?;
        let expected_input = DiffIRInput {
            myers_path: myers_ctx.path.clone(),
            char_ranges: myers_ctx.char_ranges.clone(),
        };

        poll_ctx_channel!(
            self.diff_ir_job,
            self.diff_ir_inflight_input,
            self.diff_ir_ctx
        );

        if self
            .diff_ir_ctx
            .as_ref()
            .map_or(false, |ctx| ctx.input == expected_input)
        {
            return self.diff_ir_ctx.as_ref();
        }

        if self.diff_ir_inflight_input.as_ref() != Some(&expected_input) {
            self.diff_ir_inflight_input = Some(expected_input.clone());
            let input = expected_input;
            let is_equal_left = !matches!(self.get_one_sided_mode(), OneSidedMode::OnlyRight);

            log::info!("new request_diff_ir");
            self.diff_ir_job.start(move |job| {
                let start = Instant::now();
                let cancel = job.cancel_flag().clone();
                if let Some(mut diff_ir) = DiffIR::new(&input.myers_path, is_equal_left, cancel) {
                    diff_ir.char_ranges = (*input.char_ranges).clone();
                    job.send(Some(DiffIRCtx {
                        input,
                        diff_ir: Arc::new(diff_ir),
                        elapsed: start.elapsed(),
                    }));
                } else {
                    job.send(None);
                }
            });
        }
        None
    }

    pub fn request_diff_rows(&mut self) -> Option<&DiffRowsCtx> {
        let file_1 = self.update_diff_rows_input.file_1.clone();
        let file_2 = self.update_diff_rows_input.file_2.clone();
        let ir_ctx = self.request_diff_ir()?;
        let expected_input = DiffRowsInput {
            file_1: file_1,
            file_2: file_2,
            diff_ir: ir_ctx.diff_ir.clone(),
            diff_options: self.update_diff_rows_input.options.clone(),
        };

        poll_ctx_channel!(
            self.diff_rows_job,
            self.diff_rows_inflight_input,
            self.diff_rows_ctx,
            |res: Option<DiffRowsCtx>| res.map(Arc::new)
        );

        if self
            .diff_rows_ctx
            .as_ref()
            .map_or(false, |ctx| ctx.input == expected_input)
        {
            return self.diff_rows_ctx.as_deref();
        }

        if self.diff_rows_inflight_input.as_ref() != Some(&expected_input) {
            self.diff_rows_inflight_input = Some(expected_input.clone());
            let input = expected_input;

            log::info!("new request_diff_rows");
            self.diff_rows_job.start(move |job| {
                let start = Instant::now();
                let cancel = job.cancel_flag().clone();
                let (c1, c2, _) = resolve_files(&input.file_1, &input.file_2);
                let Some(diff_rows) = build_diff_rows_cancellable(
                    (*input.diff_ir).clone(),
                    Some(&c1.tokens),
                    Some(&c2.tokens),
                    &c1.contents,
                    &c2.contents,
                    &input.diff_options,
                    c1.metadata.num_lines().max(c2.metadata.num_lines()),
                    &cancel,
                ) else {
                    return;
                };

                if let Some((rows, precomputed_diffs, collapsed_blocks)) =
                    finalize_diff_rows(diff_rows, &input.diff_options, &cancel)
                {
                    let precomputed_file_rows = precompute_file_rows(
                        &rows,
                        c1.metadata.line_starts.len(),
                        c2.metadata.line_starts.len(),
                    );
                    let row_blocks = unexpanded_row_blocks(&collapsed_blocks);
                    let changed_scopes = changed_scope_lines(&rows, [c1, c2]);
                    job.send(Some(DiffRowsCtx {
                        input,
                        rows: Arc::new(rows),
                        precomputed_diffs: Arc::new(precomputed_diffs),
                        precomputed_file_rows: Arc::new(precomputed_file_rows),
                        collapsed_blocks: Arc::new(collapsed_blocks),
                        row_blocks: Arc::new(row_blocks),
                        changed_scopes: Arc::new(changed_scopes),
                        elapsed: start.elapsed(),
                    }));
                } else {
                    job.send(None);
                }
            });
        }
        None
    }

    pub fn request_minimal_diff_ctx(&mut self) -> Option<MinimalDiffCtx> {
        let myers_ctx = self.request_myers()?;
        let num_add_deletes = myers_ctx.num_add_delete;
        let change_unit = myers_ctx.change_unit;
        let ignored_pins = myers_ctx.ignored_pins.clone();
        let phase_trace = myers_ctx.phase_trace.clone();
        let (line_elapsed, token_elapsed) = (myers_ctx.line_elapsed, myers_ctx.token_elapsed);
        let diff_row_ctx = self.request_diff_rows()?;

        let diff_rows = diff_row_ctx.rows.clone();
        let precomputed_diffs = diff_row_ctx.precomputed_diffs.clone();
        let precomputed_file_rows = diff_row_ctx.precomputed_file_rows.clone();
        let diff_ir = diff_row_ctx.input.diff_ir.clone();
        let collapsed_blocks = diff_row_ctx.collapsed_blocks.clone();
        let row_blocks = diff_row_ctx.row_blocks.clone();
        let changed_scopes = diff_row_ctx.changed_scopes.clone();
        let diff_rows_elapsed = diff_row_ctx.elapsed;
        // Rows are only returned when they were built from the current IR ctx. Read it here
        // rather than requesting it again, which would clone and compare the Myers path.
        let diff_ir_elapsed = self
            .diff_ir_ctx
            .as_ref()
            .expect("diff rows without a diff IR ctx")
            .elapsed;
        let stage_times = DiffStageTimes {
            line_diff: line_elapsed,
            token_diff: token_elapsed,
            diff_ir: diff_ir_elapsed,
            diff_rows: diff_rows_elapsed,
        };

        Some(MinimalDiffCtx {
            #[cfg(debug_assertions)]
            debug_file_1_path: self.debug_file_1_path.clone(),
            #[cfg(debug_assertions)]
            debug_file_2_path: self.debug_file_2_path.clone(),
            input: self.update_diff_rows_input.clone(),
            num_add_deletes,
            change_unit,
            ignored_pins,
            phase_trace,
            stage_times,
            precomputed_diffs,
            precomputed_file_rows,
            diff_rows,
            diff_ir,
            collapsed_blocks,
            row_blocks,
            changed_scopes,
        })
    }
}

#[derive(Debug)]
pub struct DiffProcessor {
    ctx: DiffCtx,
    pub in_progress_input: Option<UpdateDiffRowsInput>,

    // Active user state
    pub conflict_cursor: ClampedCursor,
    pub active_highlights: Arc<[usize]>,
    /// `None`: both sides.
    pub highlight_side: Option<ActiveSide>,
    highlight_source: Option<HighlightSource>,
    /// Session only: kept across recomputes of the same paths, cleared when a path changes.
    pins: Vec<Pin>,
    /// Sides of the last request. Not reset with the ctx, which a revert or refresh rebuilds.
    pin_paths: [Option<UniversalPath>; 2],
    pub find_cursor: ClampedCursor,
    pub find_ctx: FindCtx,
    find_job: Job<(FindCtx, Arc<PrecomputedFileRows>)>,
    /// The last non-empty valid query, searched again when the active side changes.
    find_request: Option<FindQuery>,
    /// Set by a side switch, so its results keep the cursor near this row and don't scroll.
    find_keep_row: Option<usize>,
    goto_line_number: Option<usize>,
    pub active_side: ActiveSide,
    pub find_both_sides: bool,

    last_conflict_scroll_to_row: Option<ScrollSpan>,
    last_find_scroll_to_row: Option<ScrollSpan>,
    /// `None` while every block is collapsed.
    expansion: Option<RowExpansion>,
}

impl Default for DiffProcessor {
    fn default() -> Self {
        let ctx = DiffCtx::default();
        Self {
            find_job: repaint_job(&ctx.repaint),
            ctx,
            conflict_cursor: ClampedCursor::default(),
            active_highlights: Arc::default(),
            highlight_side: None,
            highlight_source: None,
            pins: Vec::new(),
            pin_paths: Default::default(),
            in_progress_input: None,
            find_cursor: ClampedCursor::default(),
            find_ctx: FindCtx::default(),
            find_request: None,
            find_keep_row: None,
            goto_line_number: None,
            active_side: ActiveSide::default(),
            find_both_sides: false,
            last_conflict_scroll_to_row: None,
            last_find_scroll_to_row: None,
            expansion: None,
        }
    }
}

impl DiffProcessor {
    pub fn reset_ctx(&mut self) {
        self.ctx = DiffCtx::new(UpdateDiffRowsInput::default(), self.ctx.repaint.clone());
        self.reset_ui();
    }

    pub fn set_repaint(&mut self, ctx: &egui::Context) {
        let _ = self.ctx.repaint.set(ctx.clone());
    }
    pub fn reset_ui(&mut self) {
        self.update_find(FindCtx::default());
        self.find_request = None;
        self.conflict_cursor.set(0);
        self.goto_line_number = None;
        self.active_highlights = Arc::default();
        self.highlight_side = None;
        self.highlight_source = None;
        self.last_conflict_scroll_to_row = None;
        self.last_find_scroll_to_row = None;
        self.expansion = None;
    }

    pub fn is_in_progress(&self) -> bool {
        self.in_progress_input.is_some()
    }

    pub fn cancel_in_progress(&mut self) {
        if self.is_in_progress() {
            self.ctx.cancel_all();
            log::info!("Diff Processor cancelled the in-progress diff");
        }
    }

    pub fn pins(&self) -> &[Pin] {
        &self.pins
    }

    /// Takes effect on the next `request_update`.
    pub fn set_pins(&mut self, pins: Vec<Pin>) {
        self.pins = pins;
    }

    pub fn request_update(&mut self, mut input: UpdateDiffRowsInput) {
        if input.file_1.is_none() && input.file_2.is_none() {
            log::warn!("request update was called with no file_1 or file_2");
            return;
        }

        // By path, not by the input's hash equality: a reload of the same file keeps its pins.
        let paths = [&input.file_1, &input.file_2].map(|f| f.as_ref().map(|f| f.path.clone()));
        if paths != std::mem::replace(&mut self.pin_paths, paths.clone()) && !self.pins.is_empty() {
            log::info!("File path changed, clearing {} pins", self.pins.len());
            self.pins.clear();
        }
        input.pins = self.pins.clone();

        self.cancel_in_progress();
        self.ctx.cancel_all();
        self.in_progress_input = Some(input.clone());
        log::trace!(
            "Diff Processor new in_progress_input: {:?}",
            input.myers_diff_algorithm
        );
        self.ctx.set_input(input);
    }

    pub fn update(&mut self) {
        let mut reset_ui = false;

        for (find_ctx, mapped_to) in self.find_job.poll() {
            self.apply_find(find_ctx, &mapped_to);
        }

        let shown_rows = self.ctx.diff_rows_ctx.clone();
        self.ctx.poll();

        if self.is_in_progress() && self.ctx.request_minimal_diff_ctx().is_some() {
            self.in_progress_input = None;
            // Render-only option changes are served from the rows cache: same rows, same UI state.
            reset_ui = !matches!(
                (&shown_rows, &self.ctx.diff_rows_ctx),
                (Some(old), Some(new)) if Arc::ptr_eq(old, new)
            );
        }

        if reset_ui {
            self.reset_ui();
        }
    }

    pub fn set_active_side(&mut self, side: ActiveSide) {
        let switched = self.active_side != side;
        self.active_side = side;
        if switched
            && !self.find_both_sides
            && let Some(query) = self.find_request.clone()
        {
            let row = self.current_find_hit().map(|hit| hit.row);
            self.update_find_query(&query)
                .expect("the query compiled when it was requested");
            self.find_keep_row = row;
        }
    }

    pub fn update_goto(&mut self, line_number: Option<usize>) {
        log::info!("Goto to line: {:?}", line_number);
        self.goto_line_number = line_number;
    }

    pub fn update_find(&mut self, find_ctx: FindCtx) {
        self.find_job.cancel();
        self.find_ctx = find_ctx;
        self.find_cursor
            .set_max(self.find_ctx.hits.len().saturating_sub(1));
        self.find_cursor.set(0);
    }

    /// Without a finished diff there is nothing to search and the query isn't checked.
    pub fn update_find_query(&mut self, query: &FindQuery) -> Result<(), FindError> {
        self.find_keep_row = None;
        let Some(diff_ctx) = self.get_minimal_diff_ctx() else {
            return Ok(());
        };
        let Some(matcher) = FindCtx::compile(query)? else {
            self.find_request = None;
            self.update_find(FindCtx {
                query: query.clone(),
                ..FindCtx::default()
            });
            return Ok(());
        };
        self.find_request = Some(query.clone());
        let query = query.clone();
        let files = [diff_ctx.input.file_1, diff_ctx.input.file_2];
        // A one-sided diff has no file on the other side to search.
        let side = (!self.find_both_sides && files[self.active_side as usize].is_some())
            .then_some(self.active_side);
        let file_rows = diff_ctx.precomputed_file_rows;
        self.find_job.start(move |job| {
            let [file_1, file_2] = &files;
            let cancel = job.cancel_flag();
            let searched = FindCtx::searched(
                query,
                Some(matcher),
                [file_1, file_2],
                side,
                &file_rows,
                cancel,
            );
            if let Some(find_ctx) = searched {
                job.send((find_ctx, file_rows));
            }
        });
        Ok(())
    }

    fn apply_find(&mut self, mut find_ctx: FindCtx, mapped_to: &Arc<PrecomputedFileRows>) {
        if let Some(diff_ctx) = self.get_minimal_diff_ctx()
            && !Arc::ptr_eq(&diff_ctx.precomputed_file_rows, mapped_to)
        {
            find_ctx.map_to_rows(&diff_ctx.precomputed_file_rows);
        }
        self.update_find(find_ctx);
        if let Some(row) = self.find_keep_row.take() {
            let hits = &self.find_ctx.hits;
            let near = hits.iter().position(|hit| hit.row >= row);
            self.find_cursor
                .set(near.unwrap_or(hits.len().saturating_sub(1)));
            self.last_find_scroll_to_row = self.find_scroll_to_row();
        }
    }

    #[cfg(test)]
    pub fn wait_for_find(&mut self) {
        let start = Instant::now();
        while self.find_job.is_running() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "find did not finish"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        self.update();
    }

    pub fn clear_results(&mut self) {
        self.update_find(FindCtx::default());
        self.find_request = None;
        self.goto_line_number = None;
        self.active_highlights = Arc::default();
        self.highlight_side = None;
        self.highlight_source = None;
    }

    pub fn current_find_hit(&self) -> Option<FindHit> {
        self.find_ctx.hits.get(self.find_cursor.get()).copied()
    }

    pub fn get_scroll_to_row(&mut self) -> Option<ScrollSpan> {
        let check_update = |new: Option<ScrollSpan>, last: &mut Option<ScrollSpan>| {
            if new != *last {
                *last = new;
                new
            } else {
                None
            }
        };

        let conflict = check_update(
            self.conflict_scroll_to_row(),
            &mut self.last_conflict_scroll_to_row,
        );

        let goto_side = self.active_side;
        let goto = self.goto_scroll_to_row();

        let find = check_update(self.find_scroll_to_row(), &mut self.last_find_scroll_to_row);

        let scroll_to_row = find.or(goto).or(conflict);

        if let Some(ScrollSpan {
            start, maybe_end, ..
        }) = &scroll_to_row
        {
            self.highlight_side = (find.is_none() && goto.is_some()).then_some(goto_side);
            self.highlight_source = Some(if find.is_some() {
                HighlightSource::Find
            } else if goto.is_some() {
                HighlightSource::Goto
            } else {
                HighlightSource::Conflict
            });
            self.active_highlights = (*start..=maybe_end.unwrap_or(*start)).collect();
        } else if self.conflict_cursor.get() == 0
            && self.highlight_source == Some(HighlightSource::Conflict)
        {
            self.active_highlights = Arc::default();
            self.highlight_source = None;
        }

        scroll_to_row
    }

    /// One-shot: the request is kept until the diff is ready, then consumed, so going to the
    /// same line again scrolls again.
    fn goto_scroll_to_row(&mut self) -> Option<ScrollSpan> {
        let line = self.goto_line_number?;
        let side = self.active_side;
        let diff_ctx = self.get_minimal_diff_ctx()?;
        self.goto_line_number = None;
        let line_to_row = match side {
            ActiveSide::Left => &diff_ctx.precomputed_file_rows.0,
            ActiveSide::Right => &diff_ctx.precomputed_file_rows.1,
        };
        row_for_line(line, line_to_row).map(|start| ScrollSpan {
            start,
            maybe_end: None,
            side: Some(side),
        })
    }

    pub fn conflict_scroll_to_row(&mut self) -> Option<ScrollSpan> {
        let cursor_val = self.conflict_cursor.get();
        let mut ret = None;
        if let Some(diff_ctx) = self.get_minimal_diff_ctx() {
            if cursor_val > 0 {
                let conflict_idx_span = &diff_ctx.precomputed_diffs[cursor_val.saturating_sub(1)];
                ret = Some(ScrollSpan {
                    start: conflict_idx_span.start,
                    maybe_end: Some(conflict_idx_span.end),
                    side: None,
                });
            } else {
                ret = None;
            }
        }
        ret
    }

    pub fn find_scroll_to_row(&self) -> Option<ScrollSpan> {
        assert_eq!(
            self.find_cursor.get_max(),
            self.find_ctx.hits.len().saturating_sub(1)
        );

        self.current_find_hit().map(|hit| ScrollSpan {
            start: hit.row,
            maybe_end: None,
            side: Some(hit.side),
        })
    }

    pub fn get_minimal_diff_ctx(&mut self) -> Option<MinimalDiffCtx> {
        if self.is_in_progress() {
            return None;
        }
        let mut ctx = self.ctx.request_minimal_diff_ctx()?;
        if let Some(expansion) = &self.expansion {
            if Arc::ptr_eq(&expansion.base, &ctx.diff_rows) {
                expansion.apply(&mut ctx);
            } else {
                // reset_ui drops the expansion whenever a rebuild completes.
                log::error!("Rows were rebuilt under an expansion; dropping it");
                self.expansion = None;
            }
        }
        Some(ctx)
    }

    pub fn toggle_block(&mut self, key: usize, toggle: BlockToggle) {
        match toggle {
            BlockToggle::Expand => self.set_block_expanded(key, true),
            BlockToggle::ExpandToScope => self.expand_block_to_scope(key),
            BlockToggle::Collapse => self.set_block_expanded(key, false),
        }
    }

    /// Expands or re-collapses the whole block whose `RowBlock::key` is `key`.
    pub fn set_block_expanded(&mut self, key: usize, expanded: bool) {
        self.set_block_revealed_from(key, expanded.then_some(0));
    }

    /// Reveals the block's rows from the line opening the scope that encloses the change below
    /// the block. The whole block when that line isn't hidden in it.
    pub fn expand_block_to_scope(&mut self, key: usize) {
        let from = self
            .get_minimal_diff_ctx()
            .and_then(|ctx| scope_reveal_from(&ctx, key))
            .unwrap_or(0);
        self.set_block_revealed_from(key, Some(from));
    }

    /// Reveals the block's hidden rows from index `from` down, or none of them. Row indices held
    /// across frames (find hits, highlights, the last scroll targets) follow their rows, so the
    /// view doesn't jump.
    fn set_block_revealed_from(&mut self, key: usize, from: Option<usize>) {
        let Some(old) = self.get_minimal_diff_ctx() else {
            log::warn!("No diff shown to expand a block in");
            return;
        };
        if !old.row_blocks.iter().any(|block| block.key == key) {
            log::error!("No collapsed block at row {key}");
            return;
        }

        let mut keys = self
            .expansion
            .take()
            .map(|expansion| expansion.expanded)
            .unwrap_or_default();
        match from {
            Some(from) => keys.insert(key, from),
            None => keys.remove(&key),
        };
        if !keys.is_empty() {
            let base = self.get_minimal_diff_ctx().expect("diff shown above");
            self.expansion = Some(RowExpansion::new(&base, keys));
        }
        let new = self.get_minimal_diff_ctx().expect("diff shown above");

        let remap = |row| remap_row(&old.row_blocks, &new.row_blocks, row);
        let current = self.current_find_hit();
        self.find_ctx.map_to_rows(&new.precomputed_file_rows);
        if let Some(current) = current {
            let same = |hit: &FindHit| {
                (hit.side, hit.line, hit.ordinal) == (current.side, current.line, current.ordinal)
            };
            let index = self.find_ctx.hits.iter().position(same);
            self.find_cursor
                .set(index.expect("a remap keeps every hit"));
        }
        let mut highlights: Vec<usize> = self.active_highlights.iter().map(|&r| remap(r)).collect();
        assert!(highlights.is_sorted(), "remap_row must keep the row order");
        highlights.dedup();
        self.active_highlights = highlights.into();
        self.last_conflict_scroll_to_row = self.conflict_scroll_to_row();
        self.last_find_scroll_to_row = self.find_scroll_to_row();
    }
}

/// Index of the first hidden row of block `key` that a scope expansion reveals: the line above the
/// scope opening, which holds the signature of an Allman-style `{`. `None` when no scope opens
/// inside the block, or no change follows it.
fn scope_reveal_from(ctx: &MinimalDiffCtx, key: usize) -> Option<usize> {
    let block = ctx.row_blocks.iter().find(|block| block.key == key)?;
    let hidden = &ctx
        .collapsed_blocks
        .iter()
        .find(|block| block.row == key)?
        .hidden;
    // Blocks lie between changes, so the rows down to the next change are shown context.
    let change = ctx
        .precomputed_diffs
        .iter()
        .find(|span| span.start >= block.rows.end)?
        .start;
    // Context rows show the same line on both sides (up to ignore options), so either will do.
    let text = |row: &DiffRow| match (&row.left, &row.right) {
        (LineContent::Code { tokens, .. }, _) | (_, LineContent::Code { tokens, .. }) => {
            build_row_text(
                tokens,
                &[],
                ctx.input.file_1.as_deref(),
                ctx.input.file_2.as_deref(),
            )
            .text
        }
        _ => String::new(),
    };
    let hidden: Vec<String> = hidden.iter().map(text).collect();
    let below: Vec<String> = ctx.diff_rows[block.rows.end..change]
        .iter()
        .map(text)
        .collect();
    scope::scope_opening(&hidden, &below).map(|opening| opening.saturating_sub(1))
}

/// The rows shown with some collapsed blocks expanded, built once per change.
#[derive(Debug)]
struct RowExpansion {
    /// The unexpanded rows the keys refer to. Rebuilt rows invalidate the keys.
    base: Arc<DiffRows>,
    /// Block key to the index of its first revealed hidden row.
    expanded: BTreeMap<usize, usize>,
    diff_rows: Arc<DiffRows>,
    precomputed_diffs: Arc<PrecomputedDiffs>,
    precomputed_file_rows: Arc<PrecomputedFileRows>,
    row_blocks: Arc<Vec<RowBlock>>,
}
impl RowExpansion {
    fn new(base: &MinimalDiffCtx, expanded: BTreeMap<usize, usize>) -> Self {
        let (rows, row_blocks) = expand_rows(&base.diff_rows, &base.collapsed_blocks, &expanded);
        let (c1, c2, _) = resolve_files(&base.input.file_1, &base.input.file_2);
        let precomputed_file_rows = precompute_file_rows(
            &rows,
            c1.metadata.line_starts.len(),
            c2.metadata.line_starts.len(),
        );
        Self {
            base: base.diff_rows.clone(),
            expanded,
            precomputed_diffs: Arc::new(precompute_diff_spans(&rows)),
            precomputed_file_rows: Arc::new(precomputed_file_rows),
            diff_rows: Arc::new(rows),
            row_blocks: Arc::new(row_blocks),
        }
    }

    fn apply(&self, ctx: &mut MinimalDiffCtx) {
        ctx.diff_rows = self.diff_rows.clone();
        ctx.precomputed_diffs = self.precomputed_diffs.clone();
        ctx.precomputed_file_rows = self.precomputed_file_rows.clone();
        ctx.row_blocks = self.row_blocks.clone();
    }
}

/// Diff row of the 1-based file `line`, given each file line's row. Line 0 counts as the first
/// line and a line past the end as the last. `None` for an empty file.
fn row_for_line(line: usize, line_to_row: &[usize]) -> Option<usize> {
    let last = line_to_row.len().checked_sub(1)?;
    line_to_row.get(line.saturating_sub(1).min(last)).copied()
}

pub(crate) fn precompute_diff_spans(diff_rows: &[DiffRow]) -> PrecomputedDiffs {
    let has_change = |content: &LineContent| match content {
        LineContent::Code { tokens, .. } => tokens
            .iter()
            .any(|(res, _, _)| !res.hide_in_diff && !matches!(res.operation, DiffOp::Equal(_))),
        _ => false,
    };

    let diff_indices: Vec<usize> = diff_rows
        .iter()
        .enumerate()
        .filter_map(|(idx, row)| {
            if has_change(&row.left) || has_change(&row.right) {
                Some(idx)
            } else {
                None
            }
        })
        .collect();

    diff_indices
        .chunk_by(|&a, &b| b == a + 1)
        .map(|chunk| DiffSpan {
            start: *chunk.first().unwrap(),
            end: *chunk.last().unwrap(),
        })
        .collect()
}

pub(crate) fn precompute_file_rows(
    diff_rows: &[DiffRow],
    file_1_line_count: usize,
    file_2_line_count: usize,
) -> PrecomputedFileRows {
    let mut file_1_to_diff = vec![usize::MAX; file_1_line_count];
    let mut file_2_to_diff = vec![usize::MAX; file_2_line_count];

    for (row_idx, row) in diff_rows.iter().enumerate() {
        if let LineContent::Code { line_num, .. } = row.left {
            if line_num > 0 {
                let idx = line_num as usize - 1;
                if idx < file_1_line_count && file_1_to_diff[idx] == usize::MAX {
                    file_1_to_diff[idx] = row_idx;
                }
            }
        }

        if let LineContent::Code { line_num, .. } = row.right {
            if line_num > 0 {
                let idx = line_num as usize - 1;
                if idx < file_2_line_count && file_2_to_diff[idx] == usize::MAX {
                    file_2_to_diff[idx] = row_idx;
                }
            }
        }
    }

    for val in file_1_to_diff.iter_mut() {
        if *val == usize::MAX {
            *val = 0;
        }
    }
    for val in file_2_to_diff.iter_mut() {
        if *val == usize::MAX {
            *val = 0;
        }
    }

    (file_1_to_diff, file_2_to_diff)
}

/// Lines as the phase driver splits them, so a last line without a line break counts.
fn pin_line_count(tokens: &[RawToken]) -> usize {
    let breaks = tokens
        .iter()
        .filter(|t| t.kind == TokenKind::Newline)
        .count();
    breaks + usize::from(tokens.last().is_some_and(|t| t.kind != TokenKind::Newline))
}

/// Line counts pins are validated against. `None` unless both sides are loaded.
pub fn pin_line_counts(input: &UpdateDiffRowsInput) -> Option<(usize, usize)> {
    let (Some(f1), Some(f2)) = (&input.file_1, &input.file_2) else {
        return None;
    };
    Some((pin_line_count(&f1.tokens), pin_line_count(&f2.tokens)))
}

pub fn insert_pin(
    pins: &[Pin],
    pin: Pin,
    (source_lines, target_lines): (usize, usize),
) -> Result<Vec<Pin>, PinError> {
    let mut inserted = pins.to_vec();
    inserted.insert(pins.partition_point(|p| p.source < pin.source), pin);
    validate_pins(&inserted, source_lines, target_lines)?;
    Ok(inserted)
}

// Initial MinimalDiffCtx code, does not handle partially invalidating the diffctx
#[allow(dead_code)]
fn update_diff_rows_minimal_diff_ctx(
    input: UpdateDiffRowsInput,
    cancel_flag: Arc<AtomicBool>,
) -> Option<MinimalDiffCtx> {
    #[cfg(feature = "debug_alloc")]
    let mut reg = stats_alloc::Region::new(&crate::STATS_ALLOC);
    track_alloc!(reg, "update_diff_rows");

    let (c1, c2, one_sided_diff_is_left) = resolve_files(&input.file_1, &input.file_2);
    let cmp = |a: &RawToken, b: &RawToken| compare_tokens(a, b, c1, c2);

    track_alloc!(reg, "before myers_diff");
    let (algo, t1, t2) = (input.myers_diff_algorithm, &c1.tokens, &c2.tokens);
    let start = Instant::now();
    let ignore = input
        .options
        .ignore
        .mask(t1, &c1.contents, t2, &c2.contents);
    let hunks = line_diff(
        algo,
        t1,
        t2,
        token_key(c1, c2),
        &ignore,
        cancel_flag.clone(),
    )?;
    let line_elapsed = start.elapsed();
    let start = Instant::now();
    let myers_path = token_diff(algo, t1, t2, &hunks, &cmp, &ignore, cancel_flag.clone())?;
    let token_elapsed = start.elapsed();
    track_alloc!(reg, "myers_diff");
    check_cancel!(cancel_flag, "myers_diff_path");

    let is_equal_left = one_sided_diff_is_left.unwrap_or(true);
    let start = Instant::now();
    let diff_ir = DiffIR::new(&myers_path, is_equal_left, cancel_flag.clone())?;
    let diff_ir_elapsed = start.elapsed();
    track_alloc!(reg, "DiffIR::new()");
    check_cancel!(cancel_flag, "DiffIR::new");

    track_alloc!(reg, "hash_file");
    let start = Instant::now();
    let diff_rows = build_diff_rows(
        diff_ir.clone(),
        Some(&c1.tokens),
        Some(&c2.tokens),
        &c1.contents,
        &c2.contents,
        &input.options,
        c1.metadata.num_lines().max(c2.metadata.num_lines()),
    );
    track_alloc!(reg, "build_diff_rows");
    check_cancel!(cancel_flag, "build_diff_rows");

    let (final_rows, precomputed_diffs, collapsed_blocks) =
        finalize_diff_rows(diff_rows, &input.options, &cancel_flag)?;

    let precomputed_file_rows = precompute_file_rows(
        &final_rows,
        c1.metadata.line_starts.len(),
        c2.metadata.line_starts.len(),
    );
    let changed_scopes = changed_scope_lines(&final_rows, [c1, c2]);
    let stage_times = DiffStageTimes {
        line_diff: line_elapsed,
        token_diff: token_elapsed,
        diff_ir: diff_ir_elapsed,
        diff_rows: start.elapsed(),
    };
    Some(MinimalDiffCtx {
        #[cfg(debug_assertions)]
        debug_file_1_path: c1.path.clone(),
        #[cfg(debug_assertions)]
        debug_file_2_path: c2.path.clone(),
        input: input.clone(),
        num_add_deletes: myers_count_add_deletes(&myers_path),
        change_unit: Some(ChangeUnit::Token),
        ignored_pins: None,
        phase_trace: None,
        stage_times,
        precomputed_diffs: Arc::new(precomputed_diffs),
        precomputed_file_rows: Arc::new(precomputed_file_rows),
        diff_rows: Arc::new(final_rows),
        diff_ir: Arc::new(diff_ir),
        row_blocks: Arc::new(unexpanded_row_blocks(&collapsed_blocks)),
        collapsed_blocks: Arc::new(collapsed_blocks),
        changed_scopes: Arc::new(changed_scopes),
    })
}

fn changed_scope_lines(
    rows: &[DiffRow],
    files: [&CachedFile<RawToken>; 2],
) -> [Vec<Range<usize>>; 2] {
    let changed_line = |content: &LineContent| match content {
        LineContent::Code {
            line_num,
            bg: RowBg::Delete | RowBg::Insert,
            ..
        } if *line_num > 0 => Some(*line_num as usize - 1),
        _ => None,
    };
    [0, 1].map(|side| {
        let changed = rows
            .iter()
            .filter_map(|row| changed_line(if side == 0 { &row.left } else { &row.right }));
        let file = files[side];
        changed_scopes(&scope_lines(&file.tokens, &file.contents), changed)
    })
}

fn resolve_files<'a>(
    f1: &'a Option<Arc<CachedFile<RawToken>>>,
    f2: &'a Option<Arc<CachedFile<RawToken>>>,
) -> (
    &'a CachedFile<RawToken>,
    &'a CachedFile<RawToken>,
    Option<bool>,
) {
    match (f1, f2) {
        (Some(c1), Some(c2)) => (c1, c2, None),
        (Some(c1), None) => (c1, c1, Some(true)),
        (None, Some(c2)) => (c2, c2, Some(false)),
        (None, None) => panic!("Only call this function with one of two files valid"),
    }
}

fn compare_tokens(
    a: &RawToken,
    b: &RawToken,
    c1: &CachedFile<RawToken>,
    c2: &CachedFile<RawToken>,
) -> bool {
    if a.as_ref().kind != b.as_ref().kind {
        return false;
    }
    let a_len = a.span.end - a.span.start;
    let b_len = b.span.end - b.span.start;

    if a_len != b_len {
        return false;
    }
    let a_bytes = &c1.contents.as_bytes()[a.span.start..a.span.end];
    let b_bytes = &c2.contents.as_bytes()[b.span.start..b.span.end];

    a_bytes == b_bytes
}

fn token_key<'a>(
    c1: &'a CachedFile<RawToken>,
    c2: &'a CachedFile<RawToken>,
) -> impl Fn(Side, &RawToken) -> (TokenKind, &'a [u8]) {
    move |side, t| {
        let file = match side {
            Side::Source => c1,
            Side::Target => c2,
        };
        (t.kind, &file.contents.as_bytes()[t.span.clone()])
    }
}

/// Diff-only collapsing. A block collapsed with zero context rows leaves no
/// `Collapsed` row, so it has no `CollapsedBlock` and can't be expanded.
pub(crate) fn finalize_diff_rows(
    mut diff_rows: DiffRows,
    options: &DiffBuilderOptions,
    cancel_flag: &Arc<AtomicBool>,
) -> Option<(DiffRows, PrecomputedDiffs, CollapsedBlocks)> {
    let mut precomputed_diffs = precompute_diff_spans(&diff_rows);
    let mut collapsed_blocks = CollapsedBlocks::new();

    check_cancel!(cancel_flag, "precomputed_diffs");

    if let Some(diff_only_rows) = options.diff_only_with_extra_rows {
        let mut keep_indices = vec![false; diff_rows.len()];

        for &DiffSpan { start, end } in &precomputed_diffs {
            let bound_start = start.saturating_sub(diff_only_rows);
            let bound_end = (end + diff_only_rows).min(diff_rows.len().saturating_sub(1));
            for idx in bound_start..=bound_end {
                if idx < keep_indices.len() {
                    keep_indices[idx] = true;
                }
            }
        }

        let mut filtered_rows = Vec::with_capacity(diff_rows.len());
        let mut in_gap = false;

        for (idx, row) in diff_rows.into_iter().enumerate() {
            if keep_indices[idx] {
                filtered_rows.push(row);
                in_gap = false;
            } else if diff_only_rows == 0 {
                // No Collapsed row to expand from, so the row is dropped.
            } else {
                if !in_gap {
                    collapsed_blocks.push(CollapsedBlock {
                        row: filtered_rows.len(),
                        hidden: Vec::new(),
                    });
                    filtered_rows.push(DiffRow {
                        left: LineContent::Collapsed,
                        right: LineContent::Collapsed,
                    });
                    in_gap = true;
                }
                collapsed_blocks
                    .last_mut()
                    .expect("a gap starts with a block")
                    .hidden
                    .push(row);
            }
        }
        diff_rows = filtered_rows;
        precomputed_diffs = precompute_diff_spans(&diff_rows);
    }

    Some((diff_rows, precomputed_diffs, collapsed_blocks))
}

/// `rows` (the unexpanded rows) with the hidden rows of every block in `expanded` put back,
/// and where each block ends up. `expanded` maps a block's key to the index of its first
/// revealed hidden row. The rows above it stay behind the `Collapsed` row; from 0 the whole
/// block replaces it.
pub(crate) fn expand_rows(
    rows: &[DiffRow],
    blocks: &[CollapsedBlock],
    expanded: &BTreeMap<usize, usize>,
) -> (DiffRows, Vec<RowBlock>) {
    let extra: usize = blocks
        .iter()
        .filter_map(|block| {
            let from = *expanded.get(&block.row)?;
            Some(block.hidden.len() - from - usize::from(from == 0))
        })
        .sum();
    let mut shown = Vec::with_capacity(rows.len() + extra);
    let mut row_blocks = Vec::with_capacity(blocks.len());
    let mut next = 0;
    for block in blocks {
        assert!(
            matches!(rows[block.row].left, LineContent::Collapsed),
            "block key {} is not a collapsed row",
            block.row
        );
        shown.extend_from_slice(&rows[next..block.row]);
        let start = shown.len();
        let from = expanded.get(&block.row).copied();
        match from {
            None => shown.push(rows[block.row].clone()),
            Some(from) => {
                assert!(
                    from < block.hidden.len(),
                    "block key {} reveals from {from} of {} hidden rows",
                    block.row,
                    block.hidden.len()
                );
                if from > 0 {
                    shown.push(rows[block.row].clone());
                }
                shown.extend_from_slice(&block.hidden[from..]);
            }
        }
        row_blocks.push(RowBlock {
            key: block.row,
            rows: start..shown.len(),
            expanded: from == Some(0),
        });
        next = block.row + 1;
    }
    shown.extend_from_slice(&rows[next..]);
    (shown, row_blocks)
}

/// Every block of `blocks` unexpanded.
fn unexpanded_row_blocks(blocks: &[CollapsedBlock]) -> Vec<RowBlock> {
    blocks
        .iter()
        .map(|block| RowBlock {
            key: block.row,
            rows: block.row..block.row + 1,
            expanded: false,
        })
        .collect()
}

/// The shown row `row` of a view laid out as `from`, in a view of the same rows laid out as
/// `to`. A row inside a block that `to` hides maps to its `Collapsed` row.
fn remap_row(from: &[RowBlock], to: &[RowBlock], row: usize) -> usize {
    // Shown rows minus unexpanded rows, after a block.
    let shift = |block: &RowBlock| block.rows.end - (block.key + 1);

    // A block's hidden rows are revealed from its bottom up, so a revealed row is identified by
    // how far above the block's last row it is. `None` for rows outside blocks and Collapsed rows.
    let mut unexpanded = (row, None);
    for block in from {
        if row < block.rows.start {
            break;
        }
        if block.rows.contains(&row) {
            let is_collapsed_row = !block.expanded && row == block.rows.start;
            unexpanded = (
                block.key,
                (!is_collapsed_row).then(|| block.rows.end - 1 - row),
            );
            break;
        }
        unexpanded = (row - shift(block), None);
    }

    let (row, above_bottom) = unexpanded;
    let mut shown = row;
    for block in to {
        if row < block.key {
            break;
        }
        if row == block.key {
            let revealed = block.rows.len() - usize::from(!block.expanded);
            return match above_bottom {
                Some(above) if above < revealed => block.rows.end - 1 - above,
                _ => block.rows.start,
            };
        }
        shown = row + shift(block);
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    // Row of each line, 0-based. A ghost/void row sits at row 1, another at row 4.
    const LINE_TO_ROW: [usize; 4] = [0, 2, 3, 5];

    #[test]
    fn goto_line_one_is_the_first_line() {
        assert_eq!(row_for_line(1, &LINE_TO_ROW), Some(0));
    }

    #[test]
    fn goto_line_after_ghost_rows_lands_on_that_lines_row() {
        assert_eq!(row_for_line(2, &LINE_TO_ROW), Some(2));
        assert_eq!(row_for_line(4, &LINE_TO_ROW), Some(5));
    }

    #[test]
    fn goto_past_the_last_line_clamps_to_it() {
        assert_eq!(row_for_line(5, &LINE_TO_ROW), Some(5));
        assert_eq!(row_for_line(usize::MAX, &LINE_TO_ROW), Some(5));
    }

    #[test]
    fn goto_line_zero_is_the_first_line() {
        assert_eq!(row_for_line(0, &LINE_TO_ROW), Some(0));
    }

    #[test]
    fn goto_in_an_empty_file_has_no_row() {
        assert_eq!(row_for_line(1, &[]), None);
    }

    #[test]
    fn remap_moves_rows_by_the_rows_an_expansion_adds_or_removes() {
        let block = |key, rows, expanded| RowBlock {
            key,
            rows,
            expanded,
        };
        // Blocks at unexpanded rows 2 and 5; the first hides 4 rows.
        let collapsed = [block(2, 2..3, false), block(5, 5..6, false)];
        let expanded = [block(2, 2..6, true), block(5, 8..9, false)];

        let forth: Vec<_> = (0..7)
            .map(|row| remap_row(&collapsed, &expanded, row))
            .collect();
        assert_eq!(forth, [0, 1, 2, 6, 7, 8, 9]);
        let back: Vec<_> = (0..10)
            .map(|row| remap_row(&expanded, &collapsed, row))
            .collect();
        assert_eq!(back, [0, 1, 2, 2, 2, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn remap_keeps_rows_revealed_from_the_bottom_of_a_partly_expanded_block() {
        let block = |key, rows, expanded| RowBlock {
            key,
            rows,
            expanded,
        };
        // The first block hides 6 rows. Partly expanded, its last 3 show below its Collapsed row.
        let collapsed = [block(2, 2..3, false), block(5, 5..6, false)];
        let partial = [block(2, 2..6, false), block(5, 8..9, false)];
        let full = [block(2, 2..8, true), block(5, 10..11, false)];
        let remap = |from: &[RowBlock], to: &[RowBlock], rows: usize| -> Vec<usize> {
            (0..rows).map(|row| remap_row(from, to, row)).collect()
        };

        assert_eq!(remap(&collapsed, &partial, 7), [0, 1, 2, 6, 7, 8, 9]);
        assert_eq!(
            remap(&partial, &collapsed, 10),
            [0, 1, 2, 2, 2, 2, 3, 4, 5, 6]
        );
        assert_eq!(remap(&partial, &full, 10), [0, 1, 2, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(
            remap(&full, &partial, 12),
            [0, 1, 2, 2, 2, 3, 4, 5, 6, 7, 8, 9]
        );
    }

    mod pipeline {
        use std::{
            path::Path,
            time::{Duration, Instant},
        };

        use zcommon::logger::LogCollector;
        use zdiff::{ignore::IgnorePatterns, universal_path::UniversalPath};

        use super::*;
        use crate::file::FileProcessor;

        const SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

        /// `lines` lines where every 7th line depends on `seed`, so two seeds give a scattered diff.
        fn write_source(path: &Path, lines: usize, seed: usize) -> UniversalPath {
            let contents: String = (0..lines)
                .map(|i| {
                    if i % 7 == 0 {
                        format!("let edited_{i} = {};\n", seed * 31 + i)
                    } else {
                        format!("fn line_{i}() -> usize {{ {} }}\n", i * 3)
                    }
                })
                .collect();
            std::fs::write(path, contents).unwrap();
            UniversalPath::from(path.to_path_buf())
        }

        /// Loads through a `FileProcessor` like the app does. `None` when the load failed.
        fn load(path: &UniversalPath) -> Option<Arc<CachedFile<RawToken>>> {
            let mut file = FileProcessor::new();
            file.set_path(path.clone());
            let start = Instant::now();
            loop {
                let cached = file.get_cached_file();
                if file.get_loading_path().is_none() {
                    return cached;
                }
                assert!(
                    start.elapsed() < SETTLE_TIMEOUT,
                    "load of {path:?} never finished"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        fn input(
            file_1: &Option<Arc<CachedFile<RawToken>>>,
            file_2: &Option<Arc<CachedFile<RawToken>>>,
        ) -> UpdateDiffRowsInput {
            UpdateDiffRowsInput {
                file_1: file_1.clone(),
                file_2: file_2.clone(),
                ..Default::default()
            }
        }

        /// One frame of the app opening a pair: request it, then poll once.
        fn open(processor: &mut DiffProcessor, input: &UpdateDiffRowsInput) {
            processor.request_update(input.clone());
            processor.update();
        }

        fn settle(processor: &mut DiffProcessor) -> Option<MinimalDiffCtx> {
            let start = Instant::now();
            while processor.is_in_progress() {
                if start.elapsed() > SETTLE_TIMEOUT {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(1));
                processor.update();
            }
            processor.get_minimal_diff_ctx()
        }

        fn plain(text: &str, case_sensitive: bool) -> FindQuery {
            FindQuery {
                text: text.to_owned(),
                case_sensitive,
                ..Default::default()
            }
        }

        fn plain_find(text: &str, case_sensitive: bool, ctx: &MinimalDiffCtx) -> FindCtx {
            FindCtx::new(&plain(text, case_sensitive), ctx).unwrap()
        }

        #[test]
        fn reopening_a_pair_after_a_cached_pair_interrupted_it_completes() {
            let dir = tempfile::tempdir().unwrap();
            let a = (
                load(&write_source(&dir.path().join("a1.rs"), 1000, 1)),
                load(&write_source(&dir.path().join("a2.rs"), 1000, 2)),
            );
            let b = (
                load(&write_source(&dir.path().join("b1.rs"), 40, 3)),
                load(&write_source(&dir.path().join("b2.rs"), 40, 4)),
            );
            let (input_a, input_b) = (input(&a.0, &a.1), input(&b.0, &b.1));
            let mut processor = DiffProcessor::default();

            open(&mut processor, &input_b);
            assert!(
                settle(&mut processor).is_some(),
                "first diff of B never completed"
            );

            open(&mut processor, &input_a);
            assert!(processor.is_in_progress(), "A should still be diffing");
            open(&mut processor, &input_b);
            assert!(
                !processor.is_in_progress(),
                "B should be served from the stage caches"
            );
            open(&mut processor, &input_a);

            let ctx = settle(&mut processor).expect("re-opened pair A never completed");
            assert!(ctx.input == input_a, "diff shows {:?}", ctx.input);
        }

        #[test]
        fn failed_load_mid_sequence_is_logged_and_next_request_diffs() {
            let logs = LogCollector::init().expect("no other logger in the test binary");
            let dir = tempfile::tempdir().unwrap();
            let a = (
                load(&write_source(&dir.path().join("a1.rs"), 300, 1)),
                load(&write_source(&dir.path().join("a2.rs"), 300, 2)),
            );
            let b = (
                load(&write_source(&dir.path().join("b1.rs"), 300, 3)),
                load(&write_source(&dir.path().join("b2.rs"), 300, 4)),
            );
            let mut processor = DiffProcessor::default();

            open(&mut processor, &input(&b.0, &b.1));
            assert!(
                settle(&mut processor).is_some(),
                "diff of B never completed"
            );
            open(&mut processor, &input(&a.0, &a.1));

            let missing = dir.path().join("missing.rs");
            let missing_file = load(&UniversalPath::from(missing.clone()));
            assert!(missing_file.is_none());
            let missing_display = missing.display().to_string();
            assert!(
                logs.lock()
                    .unwrap()
                    .iter()
                    .any(|l| l.starts_with("[ERROR]") && l.contains(&missing_display)),
                "no error logged for {missing_display}"
            );

            // The app diffs whatever side did load, so the pair becomes one-sided.
            open(&mut processor, &input(&missing_file, &b.1));
            let one_sided = settle(&mut processor)
                .expect("one-sided diff after the failed load never completed");
            assert!(one_sided.input.file_1.is_none());
            open(&mut processor, &input(&a.0, &a.1));

            let ctx = settle(&mut processor).expect("pair after the failed load never completed");
            assert!(ctx.input == input(&a.0, &a.1), "diff shows {:?}", ctx.input);
            assert!(!ctx.diff_rows.is_empty());
        }

        #[test]
        fn completed_diff_carries_each_stage_time_and_their_sum() {
            let dir = tempfile::tempdir().unwrap();
            let a = (
                load(&write_source(&dir.path().join("a1.rs"), 1000, 1)),
                load(&write_source(&dir.path().join("a2.rs"), 1000, 2)),
            );
            let mut processor = DiffProcessor::default();

            open(&mut processor, &input(&a.0, &a.1));
            let times = settle(&mut processor)
                .expect("diff never completed")
                .stage_times;

            assert!(times.line_diff > Duration::ZERO, "{times:?}");
            assert!(times.token_diff > Duration::ZERO, "{times:?}");
            assert!(times.diff_ir > Duration::ZERO, "{times:?}");
            assert!(times.diff_rows > Duration::ZERO, "{times:?}");
            assert_eq!(
                times.total(),
                times.line_diff + times.token_diff + times.diff_ir + times.diff_rows
            );
        }

        #[test]
        fn no_stage_times_until_a_diff_completes_and_none_while_another_is_in_flight() {
            let dir = tempfile::tempdir().unwrap();
            let a = (
                load(&write_source(&dir.path().join("a1.rs"), 1000, 1)),
                load(&write_source(&dir.path().join("a2.rs"), 1000, 2)),
            );
            let b = (
                load(&write_source(&dir.path().join("b1.rs"), 40, 3)),
                load(&write_source(&dir.path().join("b2.rs"), 40, 4)),
            );
            let mut processor = DiffProcessor::default();
            assert!(processor.get_minimal_diff_ctx().is_none());

            open(&mut processor, &input(&b.0, &b.1));
            assert!(processor.get_minimal_diff_ctx().is_none());
            let times_b = settle(&mut processor)
                .expect("diff of B never completed")
                .stage_times;

            open(&mut processor, &input(&a.0, &a.1));
            assert!(processor.is_in_progress(), "A should still be diffing");
            assert!(processor.get_minimal_diff_ctx().is_none());

            // B again is served from the stage caches, so it shows what B cost when it ran.
            open(&mut processor, &input(&b.0, &b.1));
            assert!(!processor.is_in_progress());
            let ctx = processor.get_minimal_diff_ctx().expect("cached B");
            assert_eq!(ctx.stage_times, times_b);
        }

        #[test]
        fn toggling_ignore_whitespace_recomputes_the_diff_stage_and_highlight_rows_does_not() {
            let dir = tempfile::tempdir().unwrap();
            let a = (
                load(&write_source(&dir.path().join("a1.rs"), 300, 1)),
                load(&write_source(&dir.path().join("a2.rs"), 300, 2)),
            );
            let mut processor = DiffProcessor::default();
            let mut current = input(&a.0, &a.1);
            open(&mut processor, &current);
            let times = settle(&mut processor)
                .expect("first diff never completed")
                .stage_times;

            // A stage served from its cache keeps its time, and spawns no thread.
            current.options.highlight_rows = !current.options.highlight_rows;
            open(&mut processor, &current);
            assert!(processor.ctx.myers_inflight_input.is_none());
            assert!(processor.ctx.diff_rows_inflight_input.is_none());
            let after = settle(&mut processor).expect("highlight toggle never completed");
            assert_eq!(after.stage_times, times);
            assert!(after.input == current);

            current.options.ignore.whitespace = !current.options.ignore.whitespace;
            open(&mut processor, &current);
            assert!(
                processor.ctx.myers_inflight_input.is_some(),
                "ignore-whitespace must recompute the diff stage"
            );
            let ctx = settle(&mut processor).expect("ignore toggle never completed");
            assert!(ctx.input == current, "diff shows {:?}", ctx.input);
        }

        #[test]
        fn a_finished_diff_wakes_the_ui_after_a_ctx_reset_too() {
            let dir = tempfile::tempdir().unwrap();
            let a = (
                load(&write_source(&dir.path().join("a1.rs"), 300, 1)),
                load(&write_source(&dir.path().join("a2.rs"), 300, 2)),
            );
            let repaint = egui::Context::default();
            for _ in 0..3 {
                let _ = repaint.run(egui::RawInput::default(), |_| {});
            }
            let mut processor = DiffProcessor::default();
            processor.set_repaint(&repaint);
            processor.reset_ctx();
            assert!(!repaint.has_requested_repaint());

            open(&mut processor, &input(&a.0, &a.1));
            settle(&mut processor).expect("diff never completed");
            let start = Instant::now();
            while !repaint.has_requested_repaint() {
                assert!(start.elapsed() < SETTLE_TIMEOUT, "no repaint requested");
                std::thread::yield_now();
            }
        }

        #[test]
        fn toggling_ignore_comments_recomputes_the_diff_stage() {
            let dir = tempfile::tempdir().unwrap();
            let a = (
                load(&write_source(&dir.path().join("a1.rs"), 300, 1)),
                load(&write_source(&dir.path().join("a2.rs"), 300, 2)),
            );
            let mut processor = DiffProcessor::default();
            let mut current = input(&a.0, &a.1);
            open(&mut processor, &current);
            settle(&mut processor).expect("first diff never completed");

            current.options.ignore.comments = !current.options.ignore.comments;
            open(&mut processor, &current);
            assert!(
                processor.ctx.myers_inflight_input.is_some(),
                "ignore-comments must recompute the diff stage"
            );
            let ctx = settle(&mut processor).expect("ignore toggle never completed");
            assert!(ctx.input == current, "diff shows {:?}", ctx.input);
        }

        #[test]
        fn ignoring_line_endings_and_file_markers_recomputes_and_leaves_no_diff_span() {
            let dir = tempfile::tempdir().unwrap();
            let write = |name: &str, contents: &str| {
                let path = dir.path().join(name);
                std::fs::write(&path, contents).unwrap();
                load(&UniversalPath::from(path))
            };
            let mut current = input(
                &write("crlf.rs", "\u{feff}fn a() {}\r\nfn b() {}\r\n"),
                &write("lf.rs", "fn a() {}\nfn b() {}"),
            );
            let mut processor = DiffProcessor::default();
            open(&mut processor, &current);
            let ctx = settle(&mut processor).expect("first diff never completed");
            assert!(!ctx.precomputed_diffs.is_empty());

            current.options.ignore.line_endings = true;
            open(&mut processor, &current);
            assert!(processor.ctx.myers_inflight_input.is_some());
            let ctx = settle(&mut processor).expect("line endings toggle never completed");
            assert!(ctx.input == current, "diff shows {:?}", ctx.input);
            assert!(!ctx.precomputed_diffs.is_empty());

            current.options.ignore.file_markers = true;
            open(&mut processor, &current);
            assert!(processor.ctx.myers_inflight_input.is_some());
            let ctx = settle(&mut processor).expect("file markers toggle never completed");
            assert!(ctx.input == current, "diff shows {:?}", ctx.input);
            assert!(
                ctx.precomputed_diffs.is_empty(),
                "{:?}",
                ctx.precomputed_diffs
            );
            assert_eq!(ctx.diff_rows.len(), 2);
        }

        #[test]
        fn editing_ignore_patterns_recomputes_the_diff_stage() {
            let dir = tempfile::tempdir().unwrap();
            let a = (
                load(&write_source(&dir.path().join("a1.rs"), 300, 1)),
                load(&write_source(&dir.path().join("a2.rs"), 300, 2)),
            );
            let mut processor = DiffProcessor::default();
            let mut current = input(&a.0, &a.1);
            open(&mut processor, &current);
            settle(&mut processor).expect("first diff never completed");

            current.options.ignore.patterns = IgnorePatterns::new("\\d+");
            open(&mut processor, &current);
            assert!(
                processor.ctx.myers_inflight_input.is_some(),
                "a pattern edit must recompute the diff stage"
            );
            let ctx = settle(&mut processor).expect("pattern edit never completed");
            assert!(ctx.input == current, "diff shows {:?}", ctx.input);
        }

        mod phases {
            use super::*;

            fn changed_chars(ctx: &MinimalDiffCtx) -> Vec<Range<usize>> {
                ctx.diff_rows
                    .iter()
                    .flat_map(|row| [&row.left, &row.right])
                    .flat_map(|side| match side {
                        LineContent::Code { changed_chars, .. } => changed_chars.clone(),
                        _ => Vec::new(),
                    })
                    .collect()
            }

            fn digit_pair(dir: &Path) -> UpdateDiffRowsInput {
                let write = |name: &str, contents: &str| {
                    let path = dir.join(name);
                    std::fs::write(&path, contents).unwrap();
                    load(&UniversalPath::from(path))
                };
                input(
                    &write("left.rs", "let a = foo123;\n"),
                    &write("right.rs", "let a = foo124;\n"),
                )
            }

            #[test]
            fn toggling_a_phase_recomputes_only_from_the_diff_stage() {
                let dir = tempfile::tempdir().unwrap();
                let a = (
                    load(&write_source(&dir.path().join("a1.rs"), 300, 1)),
                    load(&write_source(&dir.path().join("a2.rs"), 300, 2)),
                );
                let mut processor = DiffProcessor::default();
                let mut current = input(&a.0, &a.1);
                open(&mut processor, &current);
                let line_and_token = settle(&mut processor).expect("first diff never completed");

                current.options.phases.token = false;
                open(&mut processor, &current);
                assert!(
                    processor.ctx.myers_inflight_input.is_some(),
                    "a phase change must recompute the diff stage"
                );
                let line_only = settle(&mut processor).expect("phase toggle never completed");
                assert!(
                    line_only.input == current,
                    "diff shows {:?}",
                    line_only.input
                );
                assert_ne!(line_only.diff_ir, line_and_token.diff_ir);
                assert_eq!(line_and_token.change_unit, Some(ChangeUnit::Token));
                assert_eq!(line_only.change_unit, Some(ChangeUnit::Line));

                current.options.phases.token = true;
                current.options.phases.line = false;
                open(&mut processor, &current);
                let token_only = settle(&mut processor).expect("phase toggle never completed");
                assert!(
                    token_only.input == current,
                    "diff shows {:?}",
                    token_only.input
                );
                assert_ne!(token_only.diff_ir, line_only.diff_ir);
                assert_eq!(token_only.change_unit, Some(ChangeUnit::Token));
            }

            #[test]
            fn no_phases_show_no_diff() {
                let dir = tempfile::tempdir().unwrap();
                let mut current = digit_pair(dir.path());
                current.options.phases.line = false;
                current.options.phases.token = false;
                let mut processor = DiffProcessor::default();
                open(&mut processor, &current);
                let ctx = settle(&mut processor).expect("diff never completed");
                assert!(
                    ctx.precomputed_diffs.is_empty(),
                    "{:?}",
                    ctx.precomputed_diffs
                );
                assert_eq!(ctx.diff_rows.len(), 1);
                assert_eq!(ctx.num_add_deletes, Default::default());
                assert_eq!(ctx.change_unit, None);
            }

            #[test]
            fn changed_scopes_are_the_scopes_holding_a_visible_change() {
                let dir = tempfile::tempdir().unwrap();
                let write = |name: &str, contents: &str| {
                    let path = dir.path().join(name);
                    std::fs::write(&path, contents).unwrap();
                    load(&UniversalPath::from(path))
                };
                let current = input(
                    &write("left.rs", "fn a() {\n    x();\n}\nfn b() {\n    y();\n}\n"),
                    &write("right.rs", "fn a() {\n    x();\n}\nfn b() {\n    z();\n}\n"),
                );
                let mut processor = DiffProcessor::default();
                open(&mut processor, &current);
                let ctx = settle(&mut processor).expect("diff never completed");
                assert_eq!(*ctx.changed_scopes, [vec![3..6], vec![3..6]]);
            }

            #[test]
            fn the_character_phase_marks_changed_chars_in_the_rows_only_while_on() {
                let dir = tempfile::tempdir().unwrap();
                let mut current = digit_pair(dir.path());
                let mut processor = DiffProcessor::default();
                open(&mut processor, &current);
                let off = settle(&mut processor).expect("diff never completed");
                assert_eq!(changed_chars(&off), Vec::<Range<usize>>::new());

                current.options.phases.character = true;
                open(&mut processor, &current);
                let on = settle(&mut processor).expect("character toggle never completed");
                assert_eq!(changed_chars(&on), vec![13..14, 13..14]);

                current.options.phases.character = false;
                open(&mut processor, &current);
                let off_again = settle(&mut processor).expect("character toggle never completed");
                assert_eq!(changed_chars(&off_again), Vec::<Range<usize>>::new());
            }

            #[test]
            fn the_phase_trace_is_collected_only_while_requested() {
                let dir = tempfile::tempdir().unwrap();
                let mut current = digit_pair(dir.path());
                current.options.phases.character = true;
                let mut processor = DiffProcessor::default();
                open(&mut processor, &current);
                let off = settle(&mut processor).expect("diff never completed");
                assert!(off.phase_trace.is_none(), "{:?}", off.phase_trace);

                current.collect_trace = true;
                open(&mut processor, &current);
                assert!(
                    processor.ctx.myers_inflight_input.is_some(),
                    "requesting the trace must recompute the diff stage"
                );
                let on = settle(&mut processor).expect("trace request never completed");
                assert!(on.input == current, "diff shows {:?}", on.input);
                let trace = on.phase_trace.as_ref().expect("trace requested");
                assert_eq!(trace.line_hunks.len(), 1, "{trace:?}");
                assert_eq!(trace.token_hunks.len(), 1, "{trace:?}");
                assert_eq!(trace.char_pairs.len(), 1, "{trace:?}");
                assert_eq!(on.num_add_deletes, off.num_add_deletes);
                assert_eq!(changed_chars(&on), changed_chars(&off));

                current.collect_trace = false;
                open(&mut processor, &current);
                let off_again = settle(&mut processor).expect("trace drop never completed");
                assert!(
                    off_again.phase_trace.is_none(),
                    "{:?}",
                    off_again.phase_trace
                );
            }
        }

        mod expansion {
            use super::*;
            use crate::revert::{RevertRequest, RevertTarget, plan_hunk_revert};

            type Pair = (
                Option<Arc<CachedFile<RawToken>>>,
                Option<Arc<CachedFile<RawToken>>>,
            );

            /// 20 lines where lines 3 and 18 differ, so 2 context rows collapse lines 6..=15.
            fn gap_pair(dir: &Path) -> Pair {
                let write = |name: &str, edit: bool| {
                    let contents: String = (1..=20)
                        .map(|n| match edit && (n == 3 || n == 18) {
                            true => format!("edit_{n}\n"),
                            false => format!("keep_{n}\n"),
                        })
                        .collect();
                    let path = dir.join(name);
                    std::fs::write(&path, contents).unwrap();
                    load(&UniversalPath::from(path))
                };
                (write("left.rs", false), write("right.rs", true))
            }

            fn diff_only(pair: &Pair, context_rows: usize) -> UpdateDiffRowsInput {
                let mut input = input(&pair.0, &pair.1);
                input.options.diff_only_with_extra_rows = Some(context_rows);
                input
            }

            fn opened(input: &UpdateDiffRowsInput) -> (DiffProcessor, MinimalDiffCtx) {
                let mut processor = DiffProcessor::default();
                open(&mut processor, input);
                let ctx = settle(&mut processor).expect("diff never completed");
                (processor, ctx)
            }

            /// (left, right) line number, 0 where a side has no line.
            fn line_nums(row: &DiffRow) -> (i32, i32) {
                let num = |content: &LineContent| match content {
                    LineContent::Code { line_num, .. } => *line_num,
                    _ => 0,
                };
                (num(&row.left), num(&row.right))
            }

            fn collapsed_rows(rows: &[DiffRow]) -> Vec<usize> {
                (0..rows.len())
                    .filter(|&i| matches!(rows[i].left, LineContent::Collapsed))
                    .collect()
            }

            fn row_of_left_line(rows: &[DiffRow], line: i32) -> usize {
                rows.iter()
                    .position(|row| line_nums(row).0 == line)
                    .unwrap_or_else(|| panic!("no row shows left line {line}"))
            }

            fn only_block(ctx: &MinimalDiffCtx) -> usize {
                let [key] = collapsed_rows(&ctx.diff_rows)[..] else {
                    panic!("expected one collapsed row: {:#?}", ctx.diff_rows);
                };
                key
            }

            #[test]
            fn expanding_restores_the_hidden_rows_at_the_block_with_their_line_numbers() {
                let dir = tempfile::tempdir().unwrap();
                let pair = gap_pair(dir.path());
                let (mut processor, collapsed) = opened(&diff_only(&pair, 2));
                let key = only_block(&collapsed);
                assert_eq!(
                    *collapsed.row_blocks,
                    vec![RowBlock {
                        key,
                        rows: key..key + 1,
                        expanded: false
                    }]
                );

                processor.set_block_expanded(key, true);
                // Polling and other state changes leave the expansion alone.
                processor.update();
                processor.conflict_cursor.set_max(2);
                processor.conflict_cursor.set(1);
                processor.get_scroll_to_row();
                let ctx = processor.get_minimal_diff_ctx().expect("diff still shown");

                assert!(collapsed_rows(&ctx.diff_rows).is_empty());
                let shown: Vec<_> = ctx.diff_rows[key..key + 10].iter().map(line_nums).collect();
                let expected: Vec<_> = (6..=15).map(|n| (n, n)).collect();
                assert_eq!(shown, expected);
                assert_eq!(
                    *ctx.row_blocks,
                    vec![RowBlock {
                        key,
                        rows: key..key + 10,
                        expanded: true
                    }]
                );
                assert_eq!(
                    format!("{:?}", &ctx.diff_rows[..key]),
                    format!("{:?}", &collapsed.diff_rows[..key])
                );
                assert_eq!(
                    format!("{:?}", &ctx.diff_rows[key + 10..]),
                    format!("{:?}", &collapsed.diff_rows[key + 1..])
                );
                let (_, full) = opened(&input(&pair.0, &pair.1));
                assert_eq!(
                    format!("{:?}", ctx.diff_rows),
                    format!("{:?}", full.diff_rows)
                );
            }

            #[test]
            fn re_collapsing_restores_the_collapsed_rows() {
                let dir = tempfile::tempdir().unwrap();
                let pair = gap_pair(dir.path());
                let (mut processor, collapsed) = opened(&diff_only(&pair, 2));
                let key = only_block(&collapsed);

                processor.set_block_expanded(key, true);
                processor.set_block_expanded(key, false);
                let ctx = processor.get_minimal_diff_ctx().expect("diff still shown");

                assert_eq!(
                    format!("{:?}", ctx.diff_rows),
                    format!("{:?}", collapsed.diff_rows)
                );
                assert_eq!(ctx.row_blocks, collapsed.row_blocks);
                assert_eq!(
                    format!("{:?}", ctx.precomputed_diffs),
                    format!("{:?}", collapsed.precomputed_diffs)
                );
                assert_eq!(ctx.precomputed_file_rows, collapsed.precomputed_file_rows);
            }

            #[test]
            fn a_row_rebuild_clears_the_expansion() {
                let dir = tempfile::tempdir().unwrap();
                let pair = gap_pair(dir.path());
                let (mut processor, collapsed) = opened(&diff_only(&pair, 2));
                processor.set_block_expanded(only_block(&collapsed), true);

                open(&mut processor, &diff_only(&pair, 3));
                let rebuilt = settle(&mut processor).expect("rebuild never completed");
                only_block(&rebuilt);
                assert!(rebuilt.row_blocks.iter().all(|block| !block.expanded));

                open(&mut processor, &diff_only(&pair, 2));
                let back = settle(&mut processor).expect("rebuild never completed");
                assert_eq!(
                    format!("{:?}", back.diff_rows),
                    format!("{:?}", collapsed.diff_rows)
                );
                assert_eq!(back.row_blocks, collapsed.row_blocks);
            }

            #[test]
            fn toggling_highlight_rows_keeps_the_rows_expansion_find_and_conflict_cursor() {
                let dir = tempfile::tempdir().unwrap();
                let pair = gap_pair(dir.path());
                let mut current = diff_only(&pair, 2);
                let (mut processor, collapsed) = opened(&current);
                processor.set_block_expanded(only_block(&collapsed), true);
                processor
                    .update_find_query(&plain("keep_10", true))
                    .unwrap();
                processor.wait_for_find();
                processor.conflict_cursor.set_max(2);
                processor.conflict_cursor.set(1);
                let expanded = processor.get_minimal_diff_ctx().expect("diff still shown");
                let hits = processor.find_ctx.hits().to_vec();
                assert!(!hits.is_empty());

                current.options.highlight_rows = !current.options.highlight_rows;
                open(&mut processor, &current);
                let toggled = settle(&mut processor).expect("highlight toggle never completed");

                assert!(toggled.input == current);
                assert!(Arc::ptr_eq(&toggled.diff_rows, &expanded.diff_rows));
                assert_eq!(processor.find_ctx.hits(), hits);
                assert_eq!(processor.conflict_cursor.get(), 1);
            }

            #[test]
            fn conflicts_find_goto_and_revert_land_on_the_right_rows_after_an_expansion() {
                let dir = tempfile::tempdir().unwrap();
                let pair = gap_pair(dir.path());
                let (mut processor, collapsed) = opened(&diff_only(&pair, 2));
                let key = only_block(&collapsed);
                let revert = RevertRequest {
                    hunk: 1,
                    target: RevertTarget::Left,
                };
                let planned = plan_hunk_revert(&collapsed, revert, std::path::Path::new(""))
                    .expect("revert planned");

                // The second conflict and a find hit below the block were navigated to.
                assert_eq!(collapsed.precomputed_diffs.len(), 2);
                processor.conflict_cursor.set_max(2);
                processor.conflict_cursor.set(2);
                processor.update_find(plain_find("keep_19", true, &collapsed));
                assert!(processor.get_scroll_to_row().is_some());

                processor.set_block_expanded(key, true);
                assert_eq!(
                    processor.get_scroll_to_row(),
                    None,
                    "expanding must not scroll"
                );

                let ctx = processor.get_minimal_diff_ctx().expect("diff still shown");
                let rows = &ctx.diff_rows;
                let found = row_of_left_line(rows, 19);
                assert_eq!(*processor.active_highlights, [found]);
                assert_eq!(processor.find_scroll_to_row().map(|s| s.start), Some(found));

                let conflict = processor.conflict_scroll_to_row().expect("conflict 2");
                let conflict_rows = conflict.start..=conflict.maybe_end.expect("a span");
                assert!(
                    rows[conflict_rows.clone()]
                        .iter()
                        .any(|row| line_nums(row).1 == 18),
                    "conflict 2 is rows {conflict_rows:?}"
                );
                assert_eq!(ctx.precomputed_diffs[1].rows(), conflict_rows);

                // Line 10 was hidden before the expansion.
                processor.active_side = ActiveSide::Left;
                processor.update_goto(Some(10));
                let goto = processor.get_scroll_to_row().expect("goto scrolls");
                assert_eq!(goto.start, row_of_left_line(rows, 10));
                assert_eq!(ctx.precomputed_file_rows.0[9], goto.start);

                assert_eq!(
                    plan_hunk_revert(&ctx, revert, std::path::Path::new("")),
                    Ok(planned)
                );
            }

            /// `gap_pair` where a function opens on line 8, inside the block of lines 6..=15, and
            /// holds the line 18 change. Its inner `if` closes before the change.
            fn scope_pair(dir: &Path) -> Pair {
                let write = |name: &str, edit: bool| {
                    let contents: String = (1..=20)
                        .map(|n| match n {
                            8 => "fn scoped() {\n".to_string(),
                            9 => "    if a {\n".to_string(),
                            11 => "    }\n".to_string(),
                            19 => "}\n".to_string(),
                            3 | 18 if edit => format!("    edit_{n}\n"),
                            _ => format!("    keep_{n}\n"),
                        })
                        .collect();
                    let path = dir.join(name);
                    std::fs::write(&path, contents).unwrap();
                    load(&UniversalPath::from(path))
                };
                (write("left.rs", false), write("right.rs", true))
            }

            #[test]
            fn expanding_to_scope_reveals_from_above_the_scope_opening_to_the_block_bottom() {
                let dir = tempfile::tempdir().unwrap();
                let pair = scope_pair(dir.path());
                let (mut processor, collapsed) = opened(&diff_only(&pair, 2));
                let key = only_block(&collapsed);

                processor.expand_block_to_scope(key);
                let ctx = processor.get_minimal_diff_ctx().expect("diff still shown");

                // Line 6 stays behind the Collapsed row.
                assert_eq!(collapsed_rows(&ctx.diff_rows), vec![key]);
                let shown: Vec<_> = ctx.diff_rows[key + 1..key + 10]
                    .iter()
                    .map(line_nums)
                    .collect();
                let expected: Vec<_> = (7..=15).map(|n| (n, n)).collect();
                assert_eq!(shown, expected);
                assert_eq!(
                    *ctx.row_blocks,
                    vec![RowBlock {
                        key,
                        rows: key..key + 10,
                        expanded: false
                    }]
                );
                assert_eq!(
                    format!("{:?}", &ctx.diff_rows[..=key]),
                    format!("{:?}", &collapsed.diff_rows[..=key])
                );
                assert_eq!(
                    format!("{:?}", &ctx.diff_rows[key + 10..]),
                    format!("{:?}", &collapsed.diff_rows[key + 1..])
                );
                assert_eq!(ctx.precomputed_file_rows.0[6], key + 1);
                assert_eq!(ctx.precomputed_file_rows.1[14], key + 9);
            }

            #[test]
            fn a_scope_expansion_expands_fully_and_re_collapses() {
                let dir = tempfile::tempdir().unwrap();
                let pair = scope_pair(dir.path());
                let (mut processor, collapsed) = opened(&diff_only(&pair, 2));
                let key = only_block(&collapsed);
                let (_, full) = opened(&input(&pair.0, &pair.1));

                processor.expand_block_to_scope(key);
                processor.set_block_expanded(key, true);
                let ctx = processor.get_minimal_diff_ctx().expect("diff still shown");
                assert_eq!(
                    format!("{:?}", ctx.diff_rows),
                    format!("{:?}", full.diff_rows)
                );

                processor.expand_block_to_scope(key);
                processor.set_block_expanded(key, false);
                let ctx = processor.get_minimal_diff_ctx().expect("diff still shown");
                assert_eq!(
                    format!("{:?}", ctx.diff_rows),
                    format!("{:?}", collapsed.diff_rows)
                );
                assert_eq!(ctx.row_blocks, collapsed.row_blocks);
            }

            #[test]
            fn expanding_to_scope_with_no_scope_in_the_block_expands_all_of_it() {
                let dir = tempfile::tempdir().unwrap();
                let pair = gap_pair(dir.path());
                let (mut processor, collapsed) = opened(&diff_only(&pair, 2));
                let key = only_block(&collapsed);
                let (_, full) = opened(&input(&pair.0, &pair.1));

                processor.expand_block_to_scope(key);
                let ctx = processor.get_minimal_diff_ctx().expect("diff still shown");
                assert_eq!(
                    format!("{:?}", ctx.diff_rows),
                    format!("{:?}", full.diff_rows)
                );
                assert!(ctx.row_blocks[0].expanded);
            }

            #[test]
            fn expanding_to_a_scope_opening_on_the_first_hidden_line_expands_all_of_it() {
                let dir = tempfile::tempdir().unwrap();
                let write = |name: &str, edit: bool| {
                    let contents: String = (1..=20)
                        .map(|n| match n {
                            5 => "fn scoped()\n".to_string(),
                            6 => "{\n".to_string(),
                            19 => "}\n".to_string(),
                            3 | 18 if edit => format!("    edit_{n}\n"),
                            _ => format!("    keep_{n}\n"),
                        })
                        .collect();
                    let path = dir.path().join(name);
                    std::fs::write(&path, contents).unwrap();
                    load(&UniversalPath::from(path))
                };
                let pair = (write("left.rs", false), write("right.rs", true));
                let (mut processor, collapsed) = opened(&diff_only(&pair, 2));
                let key = only_block(&collapsed);
                let (_, full) = opened(&input(&pair.0, &pair.1));

                processor.expand_block_to_scope(key);
                let ctx = processor.get_minimal_diff_ctx().expect("diff still shown");
                assert_eq!(
                    format!("{:?}", ctx.diff_rows),
                    format!("{:?}", full.diff_rows)
                );
                assert!(ctx.row_blocks[0].expanded);
            }

            #[test]
            fn a_find_hit_inside_the_block_lands_on_its_line_once_expanded() {
                let dir = tempfile::tempdir().unwrap();
                let pair = gap_pair(dir.path());
                let (mut processor, collapsed) = opened(&diff_only(&pair, 2));
                // Hidden lines have no row of their own, so the hit sits on row 0.
                assert_eq!(collapsed.precomputed_file_rows.0[9], 0);
                processor.update_find(plain_find("keep_10", true, &collapsed));
                processor.get_scroll_to_row();

                processor.set_block_expanded(only_block(&collapsed), true);
                assert_eq!(
                    processor.get_scroll_to_row(),
                    None,
                    "expanding must not scroll"
                );

                let ctx = processor.get_minimal_diff_ctx().expect("diff still shown");
                assert_eq!(
                    processor.find_scroll_to_row().map(|s| s.start),
                    Some(row_of_left_line(&ctx.diff_rows, 10))
                );
            }

            #[test]
            fn the_current_find_match_stays_current_when_an_expansion_reorders_the_hits() {
                let dir = tempfile::tempdir().unwrap();
                let pair = gap_pair(dir.path());
                let (mut processor, collapsed) = opened(&diff_only(&pair, 2));
                processor.update_find(plain_find("keep_1", true, &collapsed));
                let is_left_12 = |hit: &FindHit| hit.side == ActiveSide::Left && hit.line == 11;
                let before = processor.find_ctx.hits().iter().position(is_left_12);
                processor
                    .find_cursor
                    .set(before.expect("left line 12 is a hit"));

                processor.set_block_expanded(only_block(&collapsed), true);

                let ctx = processor.get_minimal_diff_ctx().expect("diff still shown");
                let current = processor.current_find_hit().expect("a current hit");
                assert!(is_left_12(&current), "{current:?}");
                assert_eq!(current.row, row_of_left_line(&ctx.diff_rows, 12));
                assert_ne!(
                    processor.find_ctx.hits().iter().position(is_left_12),
                    before
                );
            }
        }

        mod find_and_goto {
            use super::*;
            use ActiveSide::{Left, Right};

            fn opened_pair(dir: &Path, left: &str, right: &str) -> (DiffProcessor, MinimalDiffCtx) {
                let write = |name: &str, contents: &str| {
                    let path = dir.join(name);
                    std::fs::write(&path, contents).unwrap();
                    load(&UniversalPath::from(path))
                };
                let mut processor = DiffProcessor::default();
                let pair = input(&write("left.rs", left), &write("right.rs", right));
                open(&mut processor, &pair);
                let ctx = settle(&mut processor).expect("diff never completed");
                (processor, ctx)
            }

            fn foo_pair(dir: &Path) -> (DiffProcessor, MinimalDiffCtx) {
                let opened = opened_pair(dir, "foo foo\nbar\nfoo\n", "foo foo\nbar foo\nfoo\n");
                let rows = &opened.1.precomputed_file_rows;
                assert_eq!(
                    (&rows.0[..3], &rows.1[..3]),
                    (&[0, 1, 2][..], &[0, 1, 2][..])
                );
                opened
            }

            fn hit(hit: Option<FindHit>) -> Option<(ActiveSide, usize, usize)> {
                hit.map(|h| (h.side, h.row, h.ordinal))
            }

            #[test]
            fn find_lists_every_match_on_both_sides_by_row_then_side() {
                let dir = tempfile::tempdir().unwrap();
                let (_, ctx) = foo_pair(dir.path());

                let find = plain_find("foo", true, &ctx);

                let hits: Vec<_> = find.hits().iter().map(|&h| hit(Some(h)).unwrap()).collect();
                assert_eq!(
                    hits,
                    [
                        (Left, 0, 0),
                        (Left, 0, 1),
                        (Right, 0, 0),
                        (Right, 0, 1),
                        (Right, 1, 0),
                        (Left, 2, 0),
                        (Right, 2, 0),
                    ]
                );
                assert_eq!(find.query().text, "foo");
            }

            #[test]
            fn find_ignores_case_unless_asked_not_to() {
                let dir = tempfile::tempdir().unwrap();
                let (_, ctx) = foo_pair(dir.path());

                let ignoring_case = plain_find("FOO", false, &ctx);
                let exact = plain_find("foo", true, &ctx);
                assert!(!ignoring_case.query().case_sensitive);
                assert_eq!(ignoring_case.hits(), exact.hits());
                assert_eq!(ignoring_case.hits().len(), 7);
                assert!(plain_find("FOO", true, &ctx).hits().is_empty());
            }

            #[test]
            fn find_counts_overlapping_matches_like_the_row_highlight() {
                let dir = tempfile::tempdir().unwrap();
                let (_, ctx) = opened_pair(dir.path(), "aaa\n", "x\n");

                let find = plain_find("aa", true, &ctx);

                let ordinals: Vec<_> = find.hits().iter().map(|h| h.ordinal).collect();
                assert_eq!(ordinals, [0, 1]);
            }

            #[test]
            fn a_find_without_matches_has_no_current_hit_and_does_not_scroll() {
                let dir = tempfile::tempdir().unwrap();
                let (mut processor, ctx) = foo_pair(dir.path());

                for needle in ["zzz", ""] {
                    processor.update_find(plain_find(needle, true, &ctx));
                    assert!(processor.find_ctx.hits().is_empty(), "{needle:?}");
                    assert_eq!(processor.current_find_hit(), None, "{needle:?}");
                    assert_eq!(processor.find_scroll_to_row(), None, "{needle:?}");
                    assert_eq!(processor.get_scroll_to_row(), None, "{needle:?}");
                    assert!(processor.active_highlights.is_empty(), "{needle:?}");
                }
            }

            #[test]
            fn stepping_find_visits_each_match_and_lights_up_both_sides() {
                let dir = tempfile::tempdir().unwrap();
                let (mut processor, ctx) = foo_pair(dir.path());
                processor.update_find(plain_find("foo", true, &ctx));

                assert_eq!(hit(processor.current_find_hit()), Some((Left, 0, 0)));
                assert_eq!(processor.get_scroll_to_row().map(|s| s.start), Some(0));
                assert_eq!(processor.highlight_side, None);

                processor.find_cursor.inc();
                processor.find_cursor.inc();
                assert_eq!(hit(processor.current_find_hit()), Some((Right, 0, 0)));
                processor.find_cursor.inc();
                processor.find_cursor.inc();
                assert_eq!(hit(processor.current_find_hit()), Some((Right, 1, 0)));
                assert_eq!(processor.get_scroll_to_row().map(|s| s.start), Some(1));
                assert_eq!(*processor.active_highlights, [1]);
            }

            #[test]
            fn clear_results_drops_find_and_goto() {
                let dir = tempfile::tempdir().unwrap();
                let (mut processor, ctx) = foo_pair(dir.path());
                processor.update_find(plain_find("bar", true, &ctx));
                assert_eq!(processor.get_scroll_to_row().map(|s| s.start), Some(1));

                processor.clear_results();
                assert!(processor.find_ctx.hits().is_empty());
                assert_eq!(processor.find_ctx.query().text, "");
                assert!(processor.active_highlights.is_empty());
                assert_eq!(processor.get_scroll_to_row(), None);

                processor.update_find(plain_find("bar", true, &ctx));
                assert_eq!(processor.get_scroll_to_row().map(|s| s.start), Some(1));

                processor.active_side = Right;
                processor.update_goto(Some(2));
                processor.clear_results();
                assert_eq!(processor.get_scroll_to_row(), None);
                assert!(processor.active_highlights.is_empty());
                assert_eq!(processor.highlight_side, None);
            }

            #[test]
            fn goto_lights_up_only_its_side_until_the_next_navigation() {
                let dir = tempfile::tempdir().unwrap();
                let (mut processor, ctx) = foo_pair(dir.path());
                processor.active_side = Right;
                processor.update_goto(Some(2));

                assert_eq!(processor.get_scroll_to_row().map(|s| s.start), Some(1));
                assert_eq!(*processor.active_highlights, [1]);
                assert_eq!(processor.highlight_side, Some(Right));

                assert_eq!(processor.get_scroll_to_row(), None);
                assert_eq!(processor.highlight_side, Some(Right));

                processor.update_find(plain_find("bar", true, &ctx));
                assert_eq!(processor.get_scroll_to_row().map(|s| s.start), Some(1));
                assert_eq!(processor.highlight_side, None);
            }

            #[test]
            fn stepping_the_conflict_cursor_to_zero_clears_its_highlight() {
                let dir = tempfile::tempdir().unwrap();
                let (mut processor, ctx) = foo_pair(dir.path());
                processor
                    .conflict_cursor
                    .set_max(ctx.precomputed_diffs.len());
                processor.conflict_cursor.set(1);

                assert!(processor.get_scroll_to_row().is_some());
                assert!(!processor.active_highlights.is_empty());

                processor.conflict_cursor.set(0);
                assert_eq!(processor.get_scroll_to_row(), None);
                assert!(processor.active_highlights.is_empty());
            }

            #[test]
            fn a_find_highlight_stays_at_conflict_zero() {
                let dir = tempfile::tempdir().unwrap();
                let (mut processor, ctx) = foo_pair(dir.path());
                processor
                    .conflict_cursor
                    .set_max(ctx.precomputed_diffs.len());
                processor.update_find(plain_find("bar", true, &ctx));

                assert_eq!(processor.get_scroll_to_row().map(|s| s.start), Some(1));
                assert_eq!(processor.conflict_cursor.get(), 0);
                assert_eq!(processor.get_scroll_to_row(), None);
                assert_eq!(*processor.active_highlights, [1]);
            }

            #[test]
            fn a_goto_highlight_stays_at_conflict_zero() {
                let dir = tempfile::tempdir().unwrap();
                let (mut processor, ctx) = foo_pair(dir.path());
                processor
                    .conflict_cursor
                    .set_max(ctx.precomputed_diffs.len());
                processor.active_side = Right;
                processor.update_goto(Some(2));

                assert_eq!(processor.get_scroll_to_row().map(|s| s.start), Some(1));
                assert_eq!(processor.conflict_cursor.get(), 0);
                assert_eq!(processor.get_scroll_to_row(), None);
                assert_eq!(*processor.active_highlights, [1]);
                assert_eq!(processor.highlight_side, Some(Right));
            }

            fn regex(text: &str, case_sensitive: bool) -> FindQuery {
                FindQuery {
                    regex: true,
                    ..plain(text, case_sensitive)
                }
            }

            fn lines_on(find: &FindCtx, side: ActiveSide) -> Vec<(usize, usize)> {
                let mut lines: Vec<_> = find
                    .hits()
                    .iter()
                    .filter(|h| h.side == side)
                    .map(|h| (h.line, h.ordinal))
                    .collect();
                lines.sort();
                lines
            }

            #[test]
            fn find_with_whole_word_skips_matches_inside_identifiers() {
                let dir = tempfile::tempdir().unwrap();
                let (_, ctx) = opened_pair(dir.path(), "id width _id id_x (id)\n", "width\n");
                let query = FindQuery {
                    whole_word: true,
                    ..plain("id", true)
                };

                let find = FindCtx::new(&query, &ctx).unwrap();

                assert_eq!(lines_on(&find, Left), [(0, 0), (0, 1)]);
                assert_eq!(lines_on(&find, Right), []);
                assert!(find.query().whole_word);
            }

            #[test]
            fn find_with_regex_anchors_to_each_line() {
                let dir = tempfile::tempdir().unwrap();
                let (_, ctx) = opened_pair(dir.path(), "foo\nfoo bar foo\nbar\n", "xfoo\nfoo\n");

                let find = FindCtx::new(&regex("^foo", true), &ctx).unwrap();

                assert_eq!(lines_on(&find, Left), [(0, 0), (1, 0)]);
                assert_eq!(lines_on(&find, Right), [(1, 0)]);
            }

            #[test]
            fn find_with_regex_anchors_to_line_ends_in_a_crlf_file() {
                let dir = tempfile::tempdir().unwrap();
                let (_, ctx) = opened_pair(dir.path(), "a foo\r\nfoo\r\nfoo x\r\n", "x\r\n");

                let find = FindCtx::new(&regex("foo$", true), &ctx).unwrap();

                assert_eq!(lines_on(&find, Left), [(0, 0), (1, 0)]);
            }

            #[test]
            fn find_with_regex_never_matches_across_lines() {
                let dir = tempfile::tempdir().unwrap();
                let (_, ctx) = opened_pair(dir.path(), "foo\nbar\n", "foo bar\n");

                let find = FindCtx::new(&regex(r"o\sb", true), &ctx).unwrap();

                assert_eq!(lines_on(&find, Left), []);
                assert_eq!(lines_on(&find, Right), [(0, 0)]);
            }

            #[test]
            fn find_with_regex_follows_the_case_flag_and_counts_each_match_in_a_line() {
                let dir = tempfile::tempdir().unwrap();
                let (_, ctx) = opened_pair(dir.path(), "x\n", "Foo foo\n");

                let ignoring_case = FindCtx::new(&regex("FO+", false), &ctx).unwrap();
                let exact = FindCtx::new(&regex("fo+", true), &ctx).unwrap();

                assert_eq!(lines_on(&ignoring_case, Right), [(0, 0), (0, 1)]);
                assert_eq!(lines_on(&exact, Right), [(0, 0)]);
            }

            #[test]
            fn an_invalid_regex_is_an_error_and_keeps_the_previous_results() {
                let dir = tempfile::tempdir().unwrap();
                let (mut processor, _) = foo_pair(dir.path());
                processor.update_find_query(&plain("foo", true)).unwrap();
                processor.wait_for_find();
                processor.find_cursor.inc();

                let result = processor.update_find_query(&regex("foo(", true));

                assert!(matches!(result, Err(FindError::InvalidRegex(_))));
                assert_eq!(processor.find_ctx.hits().len(), 3);
                assert_eq!(processor.find_ctx.query().text, "foo");
                assert_eq!(processor.find_cursor.get(), 1);
            }

            #[test]
            fn an_empty_query_clears_the_results_without_an_error() {
                let dir = tempfile::tempdir().unwrap();
                let (mut processor, _) = foo_pair(dir.path());
                processor.update_find_query(&plain("foo", true)).unwrap();

                processor.update_find_query(&regex("", true)).unwrap();
                processor.wait_for_find();

                assert!(processor.find_ctx.hits().is_empty());
            }

            #[test]
            fn a_superseded_query_never_replaces_the_latest_results() {
                let dir = tempfile::tempdir().unwrap();
                let (mut processor, _) = foo_pair(dir.path());
                processor.update_find_query(&plain("foo", true)).unwrap();

                processor.update_find_query(&plain("bar", true)).unwrap();
                processor.wait_for_find();

                assert_eq!(processor.find_ctx.query().text, "bar");
                assert_eq!(processor.find_ctx.hits().len(), 1);
            }

            fn hit_sides(processor: &DiffProcessor) -> Vec<ActiveSide> {
                processor
                    .find_ctx
                    .hits()
                    .iter()
                    .map(|hit| hit.side)
                    .collect()
            }

            #[test]
            fn find_searches_only_the_active_side_and_follows_a_switch() {
                let dir = tempfile::tempdir().unwrap();
                let (mut processor, _) = foo_pair(dir.path());
                processor.update_find_query(&plain("foo", true)).unwrap();
                processor.wait_for_find();

                assert_eq!(hit_sides(&processor), [Left; 3]);
                assert!(processor.find_ctx.matcher_on(Left).is_some());
                assert!(processor.find_ctx.matcher_on(Right).is_none());

                processor.set_active_side(Right);
                processor.wait_for_find();

                assert_eq!(hit_sides(&processor), [Right; 4]);
                assert!(processor.find_ctx.matcher_on(Left).is_none());
                assert!(processor.find_ctx.matcher_on(Right).is_some());
            }

            #[test]
            fn switching_sides_keeps_the_find_cursor_near_its_row_without_scrolling() {
                let dir = tempfile::tempdir().unwrap();
                let (mut processor, _) = foo_pair(dir.path());
                processor.update_find_query(&plain("foo", true)).unwrap();
                processor.wait_for_find();
                processor.find_cursor.set(2);
                assert_eq!(processor.get_scroll_to_row().map(|s| s.start), Some(2));

                processor.set_active_side(Right);
                processor.wait_for_find();

                assert_eq!(processor.get_scroll_to_row(), None);
                let current = processor.current_find_hit().expect("a current hit");
                assert_eq!((current.side, current.row), (Right, 2));
            }

            #[test]
            fn find_on_both_sides_lists_both_and_ignores_a_switch() {
                let dir = tempfile::tempdir().unwrap();
                let (mut processor, _) = foo_pair(dir.path());
                processor.find_both_sides = true;
                processor.update_find_query(&plain("foo", true)).unwrap();
                processor.wait_for_find();
                let hits = processor.find_ctx.hits().to_vec();

                processor.set_active_side(Right);
                processor.wait_for_find();

                assert_eq!(hits.len(), 7);
                assert_eq!(processor.find_ctx.hits(), hits);
                assert!(processor.find_ctx.matcher_on(Left).is_some());
                assert!(processor.find_ctx.matcher_on(Right).is_some());
            }

            #[test]
            fn a_one_sided_diff_finds_on_its_only_side() {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("only.rs");
                std::fs::write(&path, "foo\nfoo\n").unwrap();
                let mut processor = DiffProcessor::default();
                open(
                    &mut processor,
                    &input(&load(&UniversalPath::from(path)), &None),
                );
                settle(&mut processor).expect("diff never completed");
                processor.active_side = Right;

                processor.update_find_query(&plain("foo", true)).unwrap();
                processor.wait_for_find();

                assert_eq!(hit_sides(&processor), [Left; 2]);
            }
        }

        mod pins {
            use zdiff::myers::{Pin, PinError};

            use super::*;

            fn pin(source: usize, target: usize) -> Pin {
                Pin { source, target }
            }

            fn write(path: &Path, contents: &str) -> Option<Arc<CachedFile<RawToken>>> {
                std::fs::write(path, contents).unwrap();
                load(&UniversalPath::from(path.to_path_buf()))
            }

            fn same_row(ctx: &MinimalDiffCtx, pin: Pin) -> bool {
                let (left, right) = &*ctx.precomputed_file_rows;
                left[pin.source] == right[pin.target]
            }

            fn diff(processor: &mut DiffProcessor, input: &UpdateDiffRowsInput) -> MinimalDiffCtx {
                open(processor, input);
                settle(processor).expect("diff never completed")
            }

            #[test]
            fn pins_rerun_the_diff_and_survive_an_option_toggle() {
                let dir = tempfile::tempdir().unwrap();
                let left = write(&dir.path().join("l.txt"), "a\nb\nc\nd\n");
                let right = write(&dir.path().join("r.txt"), "w\nx\na\nb\nc\nd\n");
                let mut processor = DiffProcessor::default();
                assert!(!same_row(
                    &diff(&mut processor, &input(&left, &right)),
                    pin(0, 0)
                ));

                processor.set_pins(vec![pin(0, 0)]);
                let ctx = diff(&mut processor, &input(&left, &right));
                assert_eq!(ctx.input.pins, [pin(0, 0)]);
                assert!(same_row(&ctx, pin(0, 0)));

                let mut toggled = input(&left, &right);
                toggled.options.ghost_rows = !toggled.options.ghost_rows;
                let ctx = diff(&mut processor, &toggled);
                assert_eq!(processor.pins(), [pin(0, 0)]);
                assert!(same_row(&ctx, pin(0, 0)));

                processor.set_pins(Vec::new());
                let ctx = diff(&mut processor, &toggled);
                assert!(!same_row(&ctx, pin(0, 0)));
            }

            #[test]
            fn pins_are_cleared_on_a_path_change() {
                let dir = tempfile::tempdir().unwrap();
                let left = write(&dir.path().join("l.txt"), "a\nb\n");
                let right = write(&dir.path().join("r.txt"), "x\na\nb\n");
                let other = write(&dir.path().join("other.txt"), "x\na\nb\n");
                let mut processor = DiffProcessor::default();
                diff(&mut processor, &input(&left, &right));
                processor.set_pins(vec![pin(0, 0)]);
                diff(&mut processor, &input(&left, &right));
                assert_eq!(processor.pins(), [pin(0, 0)]);

                let ctx = diff(&mut processor, &input(&left, &other));
                assert!(processor.pins().is_empty());
                assert!(ctx.input.pins.is_empty());
                assert!(!same_row(&ctx, pin(0, 0)));
            }

            #[test]
            fn pins_are_kept_when_a_side_reloads_from_the_same_path() {
                let dir = tempfile::tempdir().unwrap();
                let left = write(&dir.path().join("l.txt"), "a\nb\n");
                let right_path = dir.path().join("r.txt");
                let right = write(&right_path, "x\na\nb\n");
                let mut processor = DiffProcessor::default();
                diff(&mut processor, &input(&left, &right));
                processor.set_pins(vec![pin(0, 0)]);
                diff(&mut processor, &input(&left, &right));

                let reloaded = write(&right_path, "y\na\nb\n");
                let ctx = diff(&mut processor, &input(&left, &reloaded));
                assert_eq!(processor.pins(), [pin(0, 0)]);
                assert!(same_row(&ctx, pin(0, 0)));
            }

            #[test]
            fn pins_survive_the_ctx_reset_of_a_revert_or_refresh() {
                let dir = tempfile::tempdir().unwrap();
                let left = write(&dir.path().join("l.txt"), "a\nb\n");
                let right_path = dir.path().join("r.txt");
                let right = write(&right_path, "x\na\nb\n");
                let mut processor = DiffProcessor::default();
                diff(&mut processor, &input(&left, &right));
                processor.set_pins(vec![pin(0, 0)]);
                diff(&mut processor, &input(&left, &right));

                processor.reset_ctx();
                let ctx = diff(&mut processor, &input(&left, &right));
                assert_eq!(processor.pins(), [pin(0, 0)]);
                assert!(same_row(&ctx, pin(0, 0)));

                processor.reset_ctx();
                let reverted = write(&right_path, "y\na\nb\n");
                let ctx = diff(&mut processor, &input(&left, &reverted));
                assert_eq!(processor.pins(), [pin(0, 0)]);
                assert!(same_row(&ctx, pin(0, 0)));
            }

            #[test]
            fn pins_a_reloaded_file_no_longer_fits_are_ignored_with_the_reason() {
                let dir = tempfile::tempdir().unwrap();
                let left = write(&dir.path().join("l.txt"), "a\nb\nc\nd\n");
                let right_path = dir.path().join("r.txt");
                let right = write(&right_path, "a\nb\nc\nd\n");
                let mut processor = DiffProcessor::default();
                diff(&mut processor, &input(&left, &right));
                processor.set_pins(vec![pin(3, 3)]);
                assert_eq!(
                    diff(&mut processor, &input(&left, &right)).ignored_pins,
                    None
                );

                let shrunk = write(&right_path, "a\n");
                let ctx = diff(&mut processor, &input(&left, &shrunk));
                assert!(
                    matches!(ctx.ignored_pins, Some(PinError::OutOfRange { .. })),
                    "{:?}",
                    ctx.ignored_pins
                );
                assert!(!ctx.diff_rows.is_empty());
            }

            #[test]
            fn a_pin_is_inserted_in_line_order_or_rejected_with_the_reason() {
                assert_eq!(insert_pin(&[], pin(2, 3), (5, 5)), Ok(vec![pin(2, 3)]));
                assert_eq!(
                    insert_pin(&[pin(0, 0), pin(3, 4)], pin(1, 2), (5, 5)),
                    Ok(vec![pin(0, 0), pin(1, 2), pin(3, 4)])
                );
                assert_eq!(
                    insert_pin(&[pin(1, 3)], pin(2, 1), (5, 5)),
                    Err(PinError::NotIncreasing {
                        pin: pin(2, 1),
                        previous: pin(1, 3)
                    })
                );
                assert!(matches!(
                    insert_pin(&[], pin(5, 0), (5, 5)),
                    Err(PinError::OutOfRange { .. })
                ));
            }

            #[test]
            fn pin_line_counts_include_a_last_line_without_a_line_break() {
                let dir = tempfile::tempdir().unwrap();
                let ended = write(&dir.path().join("a.txt"), "a\nb\n");
                let unended = write(&dir.path().join("b.txt"), "a\nb\nc");
                assert_eq!(pin_line_counts(&input(&ended, &unended)), Some((2, 3)));
                assert_eq!(pin_line_counts(&input(&ended, &None)), None);

                let mut processor = DiffProcessor::default();
                diff(&mut processor, &input(&ended, &unended));
                processor.set_pins(vec![pin(1, 2)]);
                let ctx = diff(&mut processor, &input(&ended, &unended));
                assert_eq!(ctx.ignored_pins, None);
                assert_eq!(ctx.input.pins, [pin(1, 2)]);
            }
        }
    }
}
