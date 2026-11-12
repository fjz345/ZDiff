//! Audio viewer: both sides decoded by ffmpeg to interleaved f32 PCM off the UI thread, with a
//! min/max peak pyramid per channel so a waveform reads a fixed number of bins at any zoom.

use std::{
    io::Write,
    path::Path,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use eframe::egui::{self, Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use zcommon::ffmpeg::{
    FfmpegError, FfmpegPaths, MediaInfo, StreamKind, pcm_decode_args, probe, resampled_decode_args,
    run_ffmpeg,
};
use zcommon::job::Job;
use zcommon::style::SharedRole;
use zcommon_ui::style_window::role_color;

use super::{
    Likeness,
    audio_diff::{SampleDiffProcessor, SampleDifference},
    audio_spectrum::{self, SpectrumProcessor, SpectrumSettings},
    hex::is_same_side,
    path_extension,
};
use crate::{file::LoadedFile, ui_egui::file_role::FileRole};

/// Frames per bin at the pyramid's finest level; each level above doubles it.
const PEAK_BLOCK: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MinMax {
    pub min: f32,
    pub max: f32,
}

impl MinMax {
    const EMPTY: MinMax = MinMax {
        min: f32::INFINITY,
        max: f32::NEG_INFINITY,
    };

    fn include(&mut self, sample: f32) {
        self.min = self.min.min(sample);
        self.max = self.max.max(sample);
    }

    fn merge(self, other: MinMax) -> MinMax {
        MinMax {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}

pub struct DecodedAudio {
    pub sample_rate: u32,
    pub channels: usize,
    /// Interleaved.
    pub samples: Vec<f32>,
    /// Per channel, per level: level `k` has one entry per `PEAK_BLOCK << k` frames.
    peaks: Vec<Vec<Vec<MinMax>>>,
}

// Input logging prints the decoded sides; the samples can be hundreds of MB.
impl std::fmt::Debug for DecodedAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedAudio")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("frames", &self.frames())
            .finish()
    }
}

impl DecodedAudio {
    pub fn new(sample_rate: u32, channels: usize, samples: Vec<f32>) -> Self {
        assert!(channels > 0, "no channels");
        assert_eq!(samples.len() % channels, 0, "partial frame");
        let frames = samples.len() / channels;
        let peaks = (0..channels)
            .map(|channel| {
                let finest: Vec<MinMax> = (0..frames)
                    .step_by(PEAK_BLOCK)
                    .map(|block| {
                        let mut peak = MinMax::EMPTY;
                        for frame in block..(block + PEAK_BLOCK).min(frames) {
                            peak.include(samples[frame * channels + channel]);
                        }
                        peak
                    })
                    .collect();
                let mut levels = vec![finest];
                while let Some(level) = levels.last().filter(|level| level.len() > 1) {
                    let coarser = level
                        .chunks(2)
                        .map(|pair| pair.iter().fold(MinMax::EMPTY, |a, &b| a.merge(b)))
                        .collect();
                    levels.push(coarser);
                }
                levels
            })
            .collect();
        Self {
            sample_rate,
            channels,
            samples,
            peaks,
        }
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    /// `count` bins of `frames_per_bin` frames from frame `start`, each the exact min and max of
    /// the frames it covers; `None` past either end. A bin narrower than a frame covers one.
    pub fn bins(
        &self,
        channel: usize,
        start: f64,
        frames_per_bin: f64,
        count: usize,
    ) -> Vec<Option<MinMax>> {
        assert!(channel < self.channels, "channel {channel}");
        assert!(frames_per_bin > 0.0, "{frames_per_bin} frames per bin");
        let frames = self.frames() as f64;
        (0..count)
            .map(|i| {
                let a = (start + i as f64 * frames_per_bin).floor();
                let b = (start + (i + 1) as f64 * frames_per_bin)
                    .floor()
                    .max(a + 1.0);
                let (a, b) = (a.max(0.0), b.min(frames));
                (a < b).then(|| self.range_peak(channel, a as usize, b as usize))
            })
            .collect()
    }

    /// Raw frames up to a block edge, then the largest whole blocks that fit.
    fn range_peak(&self, channel: usize, a: usize, b: usize) -> MinMax {
        let levels = &self.peaks[channel];
        let frames = self.frames();
        let fits = |p: usize, size: usize| p.is_multiple_of(size) && (p + size).min(frames) <= b;
        let mut peak = MinMax::EMPTY;
        let mut p = a;
        while p < b {
            if !fits(p, PEAK_BLOCK) {
                peak.include(self.samples[p * self.channels + channel]);
                p += 1;
                continue;
            }
            let mut level = 0;
            while level + 1 < levels.len() && fits(p, PEAK_BLOCK << (level + 1)) {
                level += 1;
            }
            let size = PEAK_BLOCK << level;
            peak = peak.merge(levels[level][p / size]);
            p += size;
        }
        peak
    }
}

/// Appends the whole f32 samples in `chunk`; a sample split across chunks waits in `carry`.
pub fn append_f32le(samples: &mut Vec<f32>, carry: &mut Vec<u8>, chunk: &[u8]) {
    let mut chunk = chunk;
    if !carry.is_empty() {
        let take = (4 - carry.len()).min(chunk.len());
        carry.extend_from_slice(&chunk[..take]);
        chunk = &chunk[take..];
        if carry.len() < 4 {
            return;
        }
        samples.push(f32::from_le_bytes(carry[..].try_into().unwrap()));
        carry.clear();
    }
    let whole = chunk.chunks_exact(4);
    carry.extend_from_slice(whole.remainder());
    samples.extend(whole.map(|b| f32::from_le_bytes(b.try_into().unwrap())));
}

pub const MIN_SECS_PER_PX: f64 = 1e-7;
pub const MAX_SECS_PER_PX: f64 = 60.0;

/// The time axis both sides share; `x` is in points from the view's left edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeView {
    pub start: f64,
    pub secs_per_px: f64,
}

impl Default for TimeView {
    fn default() -> Self {
        Self {
            start: 0.0,
            secs_per_px: 0.01,
        }
    }
}

impl TimeView {
    pub fn fit(duration: f64, width: f32) -> Self {
        Self {
            start: 0.0,
            secs_per_px: (duration / width.max(1.0) as f64).clamp(MIN_SECS_PER_PX, MAX_SECS_PER_PX),
        }
    }

    pub fn time_at(self, x: f32) -> f64 {
        self.start + x as f64 * self.secs_per_px
    }

    pub fn x_of(self, time: f64) -> f32 {
        ((time - self.start) / self.secs_per_px) as f32
    }

    pub fn zoomed_about(self, x: f32, factor: f64) -> Self {
        let secs_per_px = (self.secs_per_px / factor).clamp(MIN_SECS_PER_PX, MAX_SECS_PER_PX);
        Self {
            start: self.time_at(x) - x as f64 * secs_per_px,
            secs_per_px,
        }
    }

    pub fn clamped(self, duration: f64, width: f32) -> Self {
        let secs_per_px = if duration > 0.0 {
            self.secs_per_px.min(Self::fit(duration, width).secs_per_px)
        } else {
            self.secs_per_px
        };
        let last_start = (duration - width as f64 * secs_per_px).max(0.0);
        Self {
            start: self.start.clamp(0.0, last_start),
            secs_per_px,
        }
    }
}

/// The smallest 1, 2 or 5 times a power of ten that puts ticks at least `min_px` apart.
pub fn tick_step(secs_per_px: f64, min_px: f64) -> f64 {
    let min_step = secs_per_px * min_px;
    let mut magnitude = 10f64.powf(min_step.log10().floor());
    loop {
        for mantissa in [1.0, 2.0, 5.0] {
            let step = mantissa * magnitude;
            // The tolerance keeps an exact fit such as 0.01 * 100 px from skipping to 2.
            if step >= min_step * (1.0 - 1e-9) {
                return step;
            }
        }
        magnitude *= 10.0;
    }
}

pub fn decimals_for(step: f64) -> usize {
    (-step.log10() - 1e-9).ceil().max(0.0) as usize
}

/// `m:ss.ddd`, or `h:mm:ss.ddd` from an hour on.
pub fn format_time(secs: f64, decimals: usize) -> String {
    let scale = 10u64.pow(decimals as u32);
    let units = (secs.max(0.0) * scale as f64).round() as u64;
    let (whole, fraction) = (units / scale, units % scale);
    let (hours, minutes, seconds) = (whole / 3600, whole / 60 % 60, whole % 60);
    let mut text = if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    };
    if decimals > 0 {
        text += &format!(".{fraction:0decimals$}");
    }
    text
}

/// Samples received so far and the count the probed duration promises (0 when unknown).
#[derive(Debug, Default)]
pub struct DecodeProgress {
    decoded: AtomicU64,
    expected: AtomicU64,
}

impl DecodeProgress {
    pub fn fraction(&self) -> Option<f32> {
        let expected = self.expected.load(Ordering::Relaxed);
        let decoded = self.decoded.load(Ordering::Relaxed);
        (expected > 0).then(|| (decoded as f64 / expected as f64).min(1.0) as f32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioLayout {
    pub sample_rate: u32,
    pub channels: usize,
    /// From the probed duration; 0 when it is missing or not a finite positive number.
    pub expected_samples: u64,
}

impl AudioLayout {
    pub fn resampled(self, sample_rate: u32, channels: usize) -> Self {
        let frames = (self.expected_samples / self.channels as u64) as u128;
        let frames = (frames * sample_rate as u128).div_ceil(self.sample_rate as u128);
        Self {
            sample_rate,
            channels,
            expected_samples: u64::try_from(frames * channels as u128).unwrap_or(u64::MAX),
        }
    }
}

/// The first audio stream, which `pcm_decode_args` decodes.
pub fn audio_layout(info: &MediaInfo) -> Result<AudioLayout, String> {
    let stream = info
        .streams
        .iter()
        .find(|stream| stream.kind == StreamKind::Audio)
        .ok_or_else(|| "it has no audio stream".to_string())?;
    let (Some(sample_rate @ 1..), Some(channels @ 1..)) = (stream.sample_rate, stream.channels)
    else {
        return Err("ffprobe gave no sample rate or channel count".to_string());
    };
    let expected_samples = stream
        .duration_secs
        .or(info.duration_secs)
        .filter(|secs| secs.is_finite() && *secs > 0.0)
        .map_or(0, |secs| {
            (secs * sample_rate as f64).ceil() as u64 * channels as u64
        });
    Ok(AudioLayout {
        sample_rate,
        channels: channels as usize,
        expected_samples,
    })
}

/// `Err(None)` is a cancelled decode. `resample` is a target rate and channel count.
fn decode_audio(
    ffmpeg: &FfmpegPaths,
    input: &Path,
    resample: Option<(u32, usize)>,
    cancel: &AtomicBool,
    progress: &DecodeProgress,
) -> Result<DecodedAudio, Option<String>> {
    let cancelled = || cancel.load(Ordering::Relaxed);
    let info = probe(ffmpeg, input, cancel).map_err(|e| (!cancelled()).then_some(e))?;
    let layout = audio_layout(&info).map_err(Some)?;
    let (layout, args) = match resample {
        Some((rate, channels)) => (
            layout.resampled(rate, channels),
            resampled_decode_args(input, rate, channels as u32),
        ),
        None => (layout, pcm_decode_args(input)),
    };
    let AudioLayout {
        sample_rate,
        channels,
        expected_samples,
    } = layout;
    progress.expected.store(expected_samples, Ordering::Relaxed);

    // A second of slack, so a slightly short probed duration doesn't double the buffer.
    let reserve = expected_samples.saturating_add(sample_rate as u64 * channels as u64);
    let mut samples = Vec::new();
    usize::try_from(reserve)
        .ok()
        .and_then(|reserve| samples.try_reserve_exact(reserve).ok())
        .ok_or_else(|| {
            Some(format!(
                "its probed length of {expected_samples} samples doesn't fit in memory"
            ))
        })?;
    let mut carry = Vec::new();
    run_ffmpeg(&ffmpeg.ffmpeg, &args, cancel, |chunk| {
        append_f32le(&mut samples, &mut carry, chunk);
        progress
            .decoded
            .store(samples.len() as u64, Ordering::Relaxed);
    })
    .map_err(|e| match e {
        FfmpegError::Cancelled => None,
        e => Some(e.to_string()),
    })?;
    samples.truncate(samples.len() / channels * channels);
    Ok(DecodedAudio::new(sample_rate, channels, samples))
}

/// ffmpeg reads files, so a side without a local path (a depot revision) is written to a
/// temporary one first.
pub(super) fn decode_file(
    ffmpeg: &FfmpegPaths,
    file: &LoadedFile,
    resample: Option<(u32, usize)>,
    cancel: &AtomicBool,
    progress: &DecodeProgress,
) -> Result<DecodedAudio, Option<String>> {
    if let Some(path) = file.path().as_path() {
        return decode_audio(ffmpeg, path, resample, cancel, progress);
    }
    let suffix = path_extension(file.path()).map_or(String::new(), |ext| format!(".{ext}"));
    let mut temp = tempfile::Builder::new()
        .prefix("zdiff-audio-")
        .suffix(&suffix)
        .tempfile()
        .map_err(|e| Some(format!("creating a temporary file: {e}")))?;
    temp.write_all(file.bytes())
        .and_then(|()| temp.flush())
        .map_err(|e| Some(format!("writing {}: {e}", temp.path().display())))?;
    decode_audio(ffmpeg, temp.path(), resample, cancel, progress)
}

enum SideState {
    Empty,
    Decoding(Arc<DecodeProgress>),
    Decoded(Arc<DecodedAudio>),
    Cancelled,
    Failed(String),
}

impl std::fmt::Debug for SideState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SideState::Empty => write!(f, "Empty"),
            SideState::Decoding(progress) => f.debug_tuple("Decoding").field(progress).finish(),
            SideState::Decoded(audio) => f.debug_tuple("Decoded").field(audio).finish(),
            SideState::Cancelled => write!(f, "Cancelled"),
            SideState::Failed(reason) => f.debug_tuple("Failed").field(reason).finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AudioMode {
    #[default]
    Lanes,
    Overlay,
    Spectrogram,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct AudioSettings {
    pub mode: AudioMode,
    pub spectrum: SpectrumSettings,
}

impl AudioSettings {
    fn reset_mode(&mut self, mode: AudioMode) {
        match mode {
            AudioMode::Spectrogram => self.spectrum = SpectrumSettings::default(),
            AudioMode::Lanes | AudioMode::Overlay => {}
        }
    }
}

type DecodeResult = Result<DecodedAudio, Option<String>>;

/// Repaints the UI when a job posts; the context arrives after the jobs are created.
pub(crate) type RepaintContext = Arc<OnceLock<egui::Context>>;

pub(crate) fn repaint_job<M: Send + 'static>(context: &RepaintContext) -> Job<M> {
    let context = context.clone();
    Job::new(move || {
        if let Some(context) = context.get() {
            context.request_repaint();
        }
    })
}

/// Decodes each side on a worker thread when its load changes. A superseded or cancelled decode
/// is killed, and a result for an older load of the side is dropped.
#[derive(Debug)]
pub struct AudioDiffProcessor {
    ffmpeg: Option<FfmpegPaths>,
    files: [Option<LoadedFile>; 2],
    sides: [SideState; 2],
    jobs: [Job<DecodeResult>; 2],
    context: RepaintContext,
    view: TimeView,
    /// Fit once both sides are done decoding, so the second side arriving doesn't move the view.
    fit_pending: bool,
    difference: SampleDiffProcessor,
    spectrum: SpectrumProcessor,
}

impl Default for AudioDiffProcessor {
    fn default() -> Self {
        let context = RepaintContext::default();
        Self {
            ffmpeg: None,
            files: [None, None],
            sides: [SideState::Empty, SideState::Empty],
            jobs: std::array::from_fn(|_| repaint_job(&context).on_panic(|e| Err(Some(e)))),
            view: TimeView::default(),
            fit_pending: false,
            difference: SampleDiffProcessor::new(&context),
            spectrum: SpectrumProcessor::default(),
            context,
        }
    }
}

impl AudioDiffProcessor {
    pub fn set_context(&self, ctx: &egui::Context) {
        let _ = self.context.set(ctx.clone());
    }

    /// No-op for the loads already requested; only a changed side is decoded again.
    pub fn request(
        &mut self,
        ffmpeg: &FfmpegPaths,
        file_1: Option<LoadedFile>,
        file_2: Option<LoadedFile>,
    ) {
        self.ffmpeg = Some(ffmpeg.clone());
        for (index, file) in [file_1, file_2].into_iter().enumerate() {
            if !is_same_side(&self.files[index], &file) {
                self.files[index] = file;
                self.start(index);
            }
        }
    }

    pub fn video_pair_files(
        &self,
        audio_tab_open: bool,
        files: [Option<LoadedFile>; 2],
    ) -> [Option<LoadedFile>; 2] {
        let held = self.files.iter().any(Option::is_some)
            && (0..2).all(|index| is_same_side(&self.files[index], &files[index]));
        if audio_tab_open || held {
            files
        } else {
            [None, None]
        }
    }

    /// Drops both sides' samples and kills their decodes.
    pub fn clear(&mut self) {
        for index in 0..2 {
            if self.files[index].is_some() {
                self.files[index] = None;
                self.start(index);
            }
        }
    }

    /// Kills the running decodes; their sides stay cancelled until `decode_again`.
    pub fn cancel(&mut self) {
        for index in 0..2 {
            if matches!(self.sides[index], SideState::Decoding(_)) {
                self.jobs[index].cancel();
                self.sides[index] = SideState::Cancelled;
            }
        }
        self.difference.cancel();
    }

    pub fn decode_again(&mut self) {
        for index in 0..2 {
            if matches!(self.sides[index], SideState::Cancelled) {
                self.start(index);
            }
        }
        self.difference.decode_again();
    }

    /// Replaces the side's decode with one of `files[index]`.
    fn start(&mut self, index: usize) {
        self.jobs[index].cancel();
        self.spectrum.reset(index);
        self.fit_pending = true;
        self.difference.clear();
        let Some(file) = self.files[index].clone() else {
            self.sides[index] = SideState::Empty;
            return;
        };
        let ffmpeg = self
            .ffmpeg
            .clone()
            .expect("a side is only requested with ffmpeg");
        log::info!("Audio decode requested: {}", file.path());
        let progress = Arc::new(DecodeProgress::default());
        let worker_progress = progress.clone();
        self.jobs[index].start(move |ctx| {
            let decoded = decode_file(
                &ffmpeg,
                &file,
                None,
                ctx.cancel_flag(),
                &worker_progress,
            )
            .map_err(|e| {
                e.map(|reason| format!("{} can't be decoded as audio: {reason}", file.path()))
            });
            ctx.send(decoded);
        });
        self.sides[index] = SideState::Decoding(progress);
    }

    pub fn poll(&mut self) {
        for index in 0..2 {
            for decoded in self.jobs[index].poll() {
                self.sides[index] = match decoded {
                    Ok(audio) => SideState::Decoded(Arc::new(audio)),
                    Err(Some(reason)) => SideState::Failed(reason),
                    Err(None) => SideState::Cancelled,
                };
            }
        }
        let inputs = match (&self.ffmpeg, &self.files[1], &self.sides) {
            (Some(ffmpeg), Some(file), [SideState::Decoded(left), SideState::Decoded(right)]) => {
                Some((ffmpeg, file, left, right))
            }
            _ => None,
        };
        self.difference.update(inputs);
        self.difference.poll();
    }

    /// One reason per side ffmpeg couldn't decode.
    pub fn failures(&self) -> Vec<String> {
        self.sides
            .iter()
            .filter_map(|side| match side {
                SideState::Failed(reason) => Some(reason.clone()),
                _ => None,
            })
            .collect()
    }

    fn decoding(&self) -> bool {
        self.sides
            .iter()
            .any(|side| matches!(side, SideState::Decoding(_)))
    }

    fn busy(&self) -> bool {
        self.decoding() || self.difference.running()
    }

    pub fn likeness(&self) -> Option<Likeness> {
        if self.decoding() {
            return Some(Likeness::Pending);
        }
        self.difference.likeness()
    }

    pub fn sample_difference(&self) -> Option<&Arc<SampleDifference>> {
        self.difference.done()
    }

    fn decoded(&self, index: usize) -> Option<&Arc<DecodedAudio>> {
        match &self.sides[index] {
            SideState::Decoded(audio) => Some(audio),
            _ => None,
        }
    }
}

const SIDE_NAMES: [&str; 2] = ["Left", "Right"];
const RULER_HEIGHT: f32 = 20.0;
const MIN_TICK_PX: f64 = 90.0;

fn side_color(ctx: &egui::Context, index: usize) -> Color32 {
    role_color(ctx, [SharedRole::DeleteText, SharedRole::InsertText][index])
}

fn side_text(side: &SideState, cursor: Option<f64>) -> String {
    match side {
        SideState::Empty => "no file".to_string(),
        SideState::Decoding(progress) => match progress.fraction() {
            Some(fraction) => format!("decoding {:.0}%", fraction * 100.0),
            None => "decoding...".to_string(),
        },
        SideState::Decoded(audio) => {
            let rate = audio.sample_rate as f64;
            let duration = audio.frames() as f64 / rate;
            let mut text = format!(
                "{} Hz, {} ch, {}",
                audio.sample_rate,
                audio.channels,
                format_time(duration, 3)
            );
            match cursor {
                Some(time) if (0.0..duration).contains(&time) => {
                    let frame = (time * rate).floor() as u64;
                    text += &format!(", cursor {} (frame {frame})", format_time(time, 3));
                }
                Some(_) => text += ", cursor past the end",
                None => {}
            }
            text
        }
        SideState::Cancelled => "cancelled".to_string(),
        SideState::Failed(reason) => reason.clone(),
    }
}

struct Lane {
    label: String,
    channel: usize,
    sides: Vec<usize>,
}

fn lanes(processor: &AudioDiffProcessor, mode: AudioMode) -> Vec<Lane> {
    let channels = |index: usize| processor.decoded(index).map_or(0, |audio| audio.channels);
    match mode {
        AudioMode::Lanes => (0..2)
            .flat_map(|index| {
                (0..channels(index).max(1)).map(move |channel| Lane {
                    label: format!("{} {}", SIDE_NAMES[index], channel + 1),
                    channel,
                    sides: vec![index],
                })
            })
            .collect(),
        AudioMode::Overlay => (0..channels(0).max(channels(1)).max(1))
            .map(|channel| Lane {
                label: format!("{}", channel + 1),
                channel,
                sides: (0..2).filter(|&i| channel < channels(i)).collect(),
            })
            .collect(),
        AudioMode::Spectrogram => Vec::new(),
    }
}

pub(super) fn draw_wave(
    painter: &egui::Painter,
    rect: Rect,
    audio: &DecodedAudio,
    channel: usize,
    view: TimeView,
    color: Color32,
) {
    let rate = audio.sample_rate as f64;
    let count = rect.width().max(0.0) as usize;
    let bins = audio.bins(channel, view.start * rate, view.secs_per_px * rate, count);
    let half = rect.height() * 0.45;
    let y = |value: f32| rect.center().y - value.clamp(-1.0, 1.0) * half;
    let mut mesh = egui::Mesh::default();
    let mut previous: Option<MinMax> = None;
    for (i, bin) in bins.iter().enumerate() {
        let Some(mut peak) = *bin else {
            previous = None;
            continue;
        };
        // Reaches the previous column, so a steep edge has no gap.
        if let Some(previous) = previous {
            peak.min = peak.min.min(previous.max);
            peak.max = peak.max.max(previous.min);
        }
        previous = *bin;
        let x = rect.left() + i as f32;
        let (top, bottom) = (y(peak.max), y(peak.min));
        mesh.add_colored_rect(
            Rect::from_min_max(pos2(x, top), pos2(x + 1.0, bottom.max(top + 1.0))),
            color,
        );
    }
    painter.add(mesh);
}

fn draw_ruler(ui: &egui::Ui, rect: Rect, view: TimeView) {
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 0.0, visuals.faint_bg_color);
    let step = tick_step(view.secs_per_px, MIN_TICK_PX);
    let decimals = decimals_for(step);
    let stroke = Stroke::new(1.0_f32, visuals.weak_text_color());
    let font = egui::TextStyle::Small.resolve(ui.style());
    let mut tick = (view.start / step).ceil() * step;
    while view.x_of(tick) <= rect.width() {
        let x = rect.left() + view.x_of(tick);
        painter.line_segment(
            [pos2(x, rect.bottom() - 6.0), pos2(x, rect.bottom())],
            stroke,
        );
        painter.text(
            pos2(x + 3.0, rect.top() + 2.0),
            Align2::LEFT_TOP,
            format_time(tick, decimals),
            font.clone(),
            visuals.text_color(),
        );
        tick += step;
    }
}

/// Wheel zooms about the cursor, drag and sideways scroll pan, double-click fits. Both sides share
/// one time axis.
pub fn show(ui: &mut egui::Ui, processor: &mut AudioDiffProcessor, settings: &mut AudioSettings) {
    let _scope = zcommon_ui::frame_stats::scope("Audio view");
    let mut fit = false;
    ui.horizontal(|ui| {
        fit = ui.button("Fit").clicked();
        ui.separator();
        let modes = [
            ui.selectable_value(&mut settings.mode, AudioMode::Lanes, "Lanes")
                .on_hover_text("One lane per side and channel"),
            ui.selectable_value(&mut settings.mode, AudioMode::Overlay, "Overlay")
                .on_hover_text("Both sides over each other, one lane per channel"),
            ui.selectable_value(&mut settings.mode, AudioMode::Spectrogram, "Spectrogram")
                .on_hover_text(
                    "Each side's channel mix over frequency, and their level difference. \
                     Double-click to reset its settings",
                ),
        ];
        if modes.iter().any(egui::Response::double_clicked) {
            settings.reset_mode(settings.mode);
        }
        if settings.mode == AudioMode::Spectrogram {
            ui.separator();
            audio_spectrum::show_controls(ui, &mut settings.spectrum);
        }
        if processor.busy() {
            ui.separator();
            if ui.button("Cancel").clicked() {
                processor.cancel();
            }
        }
        if processor
            .sides
            .iter()
            .any(|side| matches!(side, SideState::Cancelled))
            || processor.difference.cancelled()
        {
            ui.separator();
            if ui.button("Decode again").clicked() {
                processor.decode_again();
            }
        }
    });
    if processor.busy() {
        // Progress and the result arrive from workers; keep frames coming until they are polled.
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }

    let area = ui.available_rect_before_wrap();
    let difference_text = processor.difference.status_text();
    let status_line = ui.text_style_height(&egui::TextStyle::Body) + 2.0;
    let status_height = (2 + difference_text.is_some() as usize) as f32 * status_line;
    let view_rect = Rect::from_min_max(area.min, pos2(area.right(), area.bottom() - status_height));
    let ruler_rect = Rect::from_min_size(view_rect.min, vec2(view_rect.width(), RULER_HEIGHT));
    let lanes_rect = Rect::from_min_max(pos2(view_rect.left(), ruler_rect.bottom()), view_rect.max);
    let width = view_rect.width();
    let duration = (0..2)
        .filter_map(|i| processor.decoded(i))
        .map(|audio| audio.frames() as f64 / audio.sample_rate as f64)
        .fold(0.0, f64::max);

    let response = ui.allocate_rect(view_rect, Sense::click_and_drag());
    if (processor.fit_pending && !processor.decoding()) || fit || response.double_clicked() {
        processor.view = TimeView::fit(duration, width);
        processor.fit_pending = false;
    }
    if response.dragged() {
        processor.view.start -= response.drag_delta().x as f64 * processor.view.secs_per_px;
    }
    let pointer = response.hover_pos();
    if let Some(pointer) = pointer {
        let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
        let factor = (scroll.y / 200.0).exp() * pinch;
        if factor != 1.0 {
            processor.view = processor
                .view
                .zoomed_about(pointer.x - view_rect.left(), factor as f64);
        }
        processor.view.start -= scroll.x as f64 * processor.view.secs_per_px;
    }
    processor.view = processor.view.clamped(duration, width);
    let view = processor.view;

    draw_ruler(ui, ruler_rect, view);
    let lanes = lanes(processor, settings.mode);
    let spectrum_lanes = match settings.mode {
        AudioMode::Spectrogram => audio_spectrum::LANES.len(),
        AudioMode::Lanes | AudioMode::Overlay => 0,
    };
    let wave_lanes = lanes.len() + spectrum_lanes;
    let difference_lanes = processor.difference.lanes();
    let lane_height = lanes_rect.height() / (wave_lanes + difference_lanes) as f32;
    let lane_rect = |i: usize| {
        Rect::from_min_size(
            pos2(lanes_rect.left(), lanes_rect.top() + i as f32 * lane_height),
            vec2(lanes_rect.width(), lane_height),
        )
    };
    if spectrum_lanes > 0 {
        let audio = [0, 1].map(|index| processor.decoded(index).cloned());
        let colors = [0, 1].map(|index| side_color(ui.ctx(), index));
        let sides = [0, 1].map(|index| {
            let status = side_text(&processor.sides[index], None);
            (audio[index].as_ref(), status)
        });
        audio_spectrum::show_lanes(
            ui,
            lane_rect(0).union(lane_rect(spectrum_lanes - 1)),
            &mut processor.spectrum,
            sides,
            view,
            &settings.spectrum,
            colors,
        );
    }
    let painter = ui.painter_at(lanes_rect);
    let visuals = ui.visuals();
    let line = Stroke::new(1.0_f32, visuals.widgets.noninteractive.bg_stroke.color);
    let overlay = settings.mode == AudioMode::Overlay;
    for (i, lane) in lanes.iter().enumerate() {
        let rect = lane_rect(i);
        painter.hline(rect.x_range(), rect.center().y, line);
        painter.hline(rect.x_range(), rect.bottom(), line);
        for &side in &lane.sides {
            let Some(audio) = processor.decoded(side) else {
                continue;
            };
            let color = side_color(ui.ctx(), side);
            let color = if overlay {
                color.gamma_multiply(0.6)
            } else {
                color
            };
            draw_wave(&painter, rect, audio, lane.channel, view, color);
        }
        painter.text(
            rect.left_top() + vec2(4.0, 2.0),
            Align2::LEFT_TOP,
            &lane.label,
            egui::TextStyle::Small.resolve(ui.style()),
            visuals.weak_text_color(),
        );
        if let [side] = lane.sides[..]
            && processor.decoded(side).is_none()
        {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                side_text(&processor.sides[side], None),
                egui::TextStyle::Body.resolve(ui.style()),
                visuals.text_color(),
            );
        }
    }
    for channel in 0..difference_lanes {
        let rect = lane_rect(wave_lanes + channel);
        painter.hline(rect.x_range(), rect.center().y, line);
        painter.hline(rect.x_range(), rect.bottom(), line);
        processor
            .difference
            .draw_lane(ui, &painter, rect, channel, view);
    }

    let cursor = pointer.map(|pointer| view.time_at(pointer.x - view_rect.left()));
    if let Some(pointer) = pointer {
        ui.painter_at(view_rect).vline(
            pointer.x,
            view_rect.y_range(),
            Stroke::new(1.0_f32, visuals.strong_text_color()),
        );
    }
    let status_painter = ui.painter_at(area);
    for (index, (name, side)) in SIDE_NAMES.iter().zip(&processor.sides).enumerate() {
        let y = view_rect.bottom() + 2.0 + index as f32 * status_line;
        status_painter.text(
            pos2(area.left() + 4.0, y),
            Align2::LEFT_TOP,
            format!("{name}: {}", side_text(side, cursor)),
            egui::TextStyle::Body.resolve(ui.style()),
            side_color(ui.ctx(), index),
        );
    }
    if let Some(text) = difference_text {
        status_painter.text(
            pos2(
                area.left() + 4.0,
                view_rect.bottom() + 2.0 + 2.0 * status_line,
            ),
            Align2::LEFT_TOP,
            format!("Difference: {text}"),
            egui::TextStyle::Body.resolve(ui.style()),
            role_color(ui.ctx(), FileRole::Modified),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::file::BinaryFile;
    use zcommon::ffmpeg::parse_probe;
    use zdiff::universal_path::UniversalPath;

    fn audio_file(name: &str) -> LoadedFile {
        LoadedFile::Binary(Arc::new(BinaryFile {
            path: UniversalPath::from(name),
            bytes: vec![0; 16],
        }))
    }

    fn missing_ffmpeg() -> FfmpegPaths {
        let dir = std::env::temp_dir().join("zdiff-no-such-ffmpeg");
        FfmpegPaths {
            ffmpeg: dir.join("ffmpeg.exe"),
            ffprobe: dir.join("ffprobe.exe"),
        }
    }

    fn layout(stream: &str, duration: &str) -> Result<AudioLayout, String> {
        let json = format!(
            r#"{{"streams": [{{"codec_type": "video"}}, {stream}], "format": {{"duration": "{duration}"}}}}"#
        );
        audio_layout(&parse_probe(&json).unwrap())
    }

    #[test]
    fn the_layout_is_the_first_audio_streams_rate_and_channels() {
        let stereo = r#"{"codec_type": "audio", "sample_rate": "48000", "channels": 2}"#;
        assert_eq!(
            layout(stereo, "1.5"),
            Ok(AudioLayout {
                sample_rate: 48000,
                channels: 2,
                expected_samples: 144000,
            })
        );
        for unknown in ["N/A", "inf", "-3.0", "NaN"] {
            assert_eq!(
                layout(stereo, unknown).unwrap().expected_samples,
                0,
                "{unknown}"
            );
        }
    }

    #[test]
    fn a_resampled_layout_expects_the_same_duration_at_the_target_rate_and_channels() {
        let stereo = r#"{"codec_type": "audio", "sample_rate": "48000", "channels": 2}"#;
        assert_eq!(
            layout(stereo, "1.5").unwrap().resampled(44100, 1),
            AudioLayout {
                sample_rate: 44100,
                channels: 1,
                expected_samples: 66150,
            }
        );
        assert_eq!(
            layout(stereo, "N/A")
                .unwrap()
                .resampled(44100, 1)
                .expected_samples,
            0
        );
    }

    #[test]
    fn a_stream_without_a_usable_rate_or_channel_count_has_no_layout() {
        for stream in [
            r#"{"codec_type": "audio", "sample_rate": "0", "channels": 2}"#,
            r#"{"codec_type": "audio", "sample_rate": "48000", "channels": 0}"#,
            r#"{"codec_type": "audio", "sample_rate": "48000"}"#,
            r#"{"codec_type": "audio", "channels": 2}"#,
            r#"{"codec_type": "subtitle"}"#,
        ] {
            assert!(layout(stream, "1.0").is_err(), "{stream}");
        }
    }

    fn poll_until(processor: &mut AudioDiffProcessor, done: impl Fn(&AudioDiffProcessor) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(processor) {
            assert!(Instant::now() < deadline, "timed out: {processor:?}");
            processor.poll();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_side_ffmpeg_cant_decode_is_a_failure_naming_the_file() {
        let mut processor = AudioDiffProcessor::default();
        processor.request(&missing_ffmpeg(), Some(audio_file("a.wav")), None);
        poll_until(&mut processor, |p| !p.decoding());
        let failures = processor.failures();
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("a.wav"), "{failures:?}");
        assert!(failures[0].contains("ffprobe"), "{failures:?}");

        processor.clear();
        assert!(processor.failures().is_empty());
    }

    #[test]
    fn a_cancelled_side_is_not_a_failure_and_decodes_again_on_request() {
        let mut processor = AudioDiffProcessor::default();
        let (a, b) = (audio_file("a.wav"), audio_file("b.wav"));
        processor.request(&missing_ffmpeg(), Some(a.clone()), Some(b.clone()));
        processor.cancel();
        std::thread::sleep(Duration::from_millis(50));
        processor.poll();
        assert!(processor.failures().is_empty(), "{processor:?}");
        assert!(matches!(
            processor.sides,
            [SideState::Cancelled, SideState::Cancelled]
        ));

        processor.request(&missing_ffmpeg(), Some(a), Some(b));
        assert!(
            matches!(processor.sides[0], SideState::Cancelled),
            "same loads: no-op"
        );
        processor.decode_again();
        poll_until(&mut processor, |p| !p.decoding());
        assert_eq!(processor.failures().len(), 2, "{processor:?}");
    }

    #[test]
    fn the_difference_follows_both_decoded_sides_and_a_new_request_drops_it() {
        let mut processor = AudioDiffProcessor::default();
        processor.request(&missing_ffmpeg(), None, None);
        processor.files[1] = Some(audio_file("b.wav"));
        let decoded = || SideState::Decoded(Arc::new(DecodedAudio::new(8, 1, vec![0.5; 8])));
        processor.sides = [decoded(), decoded()];
        processor.poll();
        assert!(processor.difference.running(), "{processor:?}");
        poll_until(&mut processor, |p| !p.difference.running());
        assert_eq!(
            processor.difference.status_text().as_deref(),
            Some("identical")
        );

        processor.request(&missing_ffmpeg(), None, Some(audio_file("c.wav")));
        assert_eq!(processor.difference.status_text(), None);
        poll_until(&mut processor, |p| !p.decoding());
        assert_eq!(processor.difference.status_text(), None);
    }

    #[test]
    fn a_video_pairs_audio_decodes_from_the_first_audio_tab_until_the_pair_changes() {
        fn frame(processor: &mut AudioDiffProcessor, audio_tab_open: bool, files: [LoadedFile; 2]) {
            match processor.video_pair_files(audio_tab_open, files.map(Some)) {
                [None, None] => processor.clear(),
                [file_1, file_2] => processor.request(&missing_ffmpeg(), file_1, file_2),
            }
        }
        let cleared = |p: &AudioDiffProcessor| {
            matches!(p.sides, [SideState::Empty, SideState::Empty]) && p.failures().is_empty()
        };
        let mut processor = AudioDiffProcessor::default();
        let (a, b) = (audio_file("a.mp4"), audio_file("b.mp4"));
        let pair = || [a.clone(), b.clone()];

        frame(&mut processor, false, pair());
        assert!(cleared(&processor), "{processor:?}");

        frame(&mut processor, true, pair());
        poll_until(&mut processor, |p| !p.decoding());
        assert_eq!(processor.failures().len(), 2, "{processor:?}");

        frame(&mut processor, false, pair());
        assert_eq!(processor.failures().len(), 2, "{processor:?}");
        frame(&mut processor, true, pair());
        assert_eq!(processor.failures().len(), 2, "{processor:?}");

        frame(&mut processor, false, [b.clone(), a.clone()]);
        assert!(cleared(&processor), "{processor:?}");
        frame(&mut processor, false, pair());
        assert!(cleared(&processor), "{processor:?}");

        frame(&mut processor, true, pair());
        frame(&mut processor, false, [a.clone(), audio_file("b.mp4")]);
        assert!(cleared(&processor), "{processor:?}");
    }

    fn assert_close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} vs {b}");
    }

    #[test]
    fn fit_shows_the_whole_duration() {
        let view = TimeView::fit(10.0, 1000.0);
        assert_close(view.time_at(0.0), 0.0);
        assert_close(view.time_at(1000.0), 10.0);
        assert!((view.x_of(5.0) - 500.0).abs() < 1e-3);
    }

    #[test]
    fn zooming_keeps_the_time_under_the_cursor_in_place() {
        let view = TimeView::fit(10.0, 1000.0);
        let under = view.time_at(250.0);
        let zoomed = view.zoomed_about(250.0, 4.0);
        assert_close(zoomed.time_at(250.0), under);
        assert_close(zoomed.secs_per_px, view.secs_per_px / 4.0);
    }

    #[test]
    fn zoom_is_clamped_and_still_keeps_the_cursor_time() {
        let view = TimeView::fit(10.0, 1000.0);
        let under = view.time_at(600.0);
        let deep = view.zoomed_about(600.0, 1e12);
        assert_eq!(deep.secs_per_px, MIN_SECS_PER_PX);
        assert_close(deep.time_at(600.0), under);
        assert_eq!(view.zoomed_about(600.0, 1e-12).secs_per_px, MAX_SECS_PER_PX);
    }

    #[test]
    fn zoom_out_stops_at_the_whole_duration() {
        let fit = TimeView::fit(10.0, 1000.0);
        let out = fit.zoomed_about(600.0, 1e-12).clamped(10.0, 1000.0);
        assert_close(out.secs_per_px, fit.secs_per_px);
        assert_eq!(out.start, 0.0);

        let zoomed = fit.zoomed_about(250.0, 4.0).clamped(10.0, 1000.0);
        assert_close(zoomed.secs_per_px, fit.secs_per_px / 4.0);
        let back = zoomed.zoomed_about(250.0, 0.1).clamped(10.0, 1000.0);
        assert_close(back.secs_per_px, fit.secs_per_px);
        assert_eq!(back.start, 0.0);

        let wider = fit.clamped(10.0, 2000.0);
        assert_close(wider.secs_per_px, TimeView::fit(10.0, 2000.0).secs_per_px);
        assert_eq!(fit.clamped(10.0, 500.0), fit);
    }

    #[test]
    fn a_view_with_nothing_decoded_keeps_its_zoom() {
        let view = TimeView {
            start: 0.0,
            secs_per_px: 0.5,
        };
        assert_eq!(view.clamped(0.0, 1000.0), view);
    }

    #[test]
    fn resetting_the_spectrogram_mode_restores_its_settings_only() {
        let changed = AudioSettings {
            mode: AudioMode::Spectrogram,
            spectrum: SpectrumSettings {
                window: 512,
                floor_db: -60.0,
                ceil_db: -6.0,
                diff_range_db: 3.0,
            },
        };
        for mode in [AudioMode::Lanes, AudioMode::Overlay] {
            let mut settings = changed.clone();
            settings.reset_mode(mode);
            assert_eq!(settings, changed, "{mode:?}");
        }
        let mut settings = changed.clone();
        settings.reset_mode(AudioMode::Spectrogram);
        assert_eq!(
            settings,
            AudioSettings {
                mode: AudioMode::Spectrogram,
                spectrum: SpectrumSettings::default(),
            }
        );
    }

    #[test]
    fn scrolling_stops_at_the_ends() {
        let view = TimeView {
            start: -3.0,
            secs_per_px: 0.01,
        };
        assert_eq!(view.clamped(60.0, 1000.0).start, 0.0);
        let late = TimeView {
            start: 58.0,
            ..view
        };
        assert_close(late.clamped(60.0, 1000.0).start, 50.0);
        let wide = TimeView { start: 2.0, ..view };
        assert_eq!(wide.clamped(5.0, 1000.0).start, 0.0);
    }

    #[test]
    fn ticks_step_in_ones_twos_and_fives() {
        assert_close(tick_step(0.01, 80.0), 1.0);
        assert_close(tick_step(0.01, 120.0), 2.0);
        assert_close(tick_step(0.01, 300.0), 5.0);
        assert_close(tick_step(0.01, 600.0), 10.0);
        assert_close(tick_step(1e-5, 80.0), 0.001);
    }

    #[test]
    fn times_are_minutes_and_seconds_with_the_steps_decimals() {
        assert_eq!(decimals_for(1.0), 0);
        assert_eq!(decimals_for(0.5), 1);
        assert_eq!(decimals_for(0.05), 2);
        assert_eq!(decimals_for(0.001), 3);
        assert_eq!(format_time(83.5, 1), "1:23.5");
        assert_eq!(format_time(0.25, 2), "0:00.25");
        assert_eq!(format_time(59.9996, 3), "1:00.000");
        assert_eq!(format_time(3725.0, 0), "1:02:05");
    }

    fn brute_force(samples: &[f32], channels: usize, channel: usize, a: usize, b: usize) -> MinMax {
        let mut peak = MinMax::EMPTY;
        for frame in a..b {
            peak.include(samples[frame * channels + channel]);
        }
        peak
    }

    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames).map(|i| i as f32).collect()
    }

    fn mm(min: f32, max: f32) -> Option<MinMax> {
        Some(MinMax { min, max })
    }

    #[test]
    fn a_ramp_gives_each_bins_first_and_last_frame_at_several_zoom_levels() {
        let audio = DecodedAudio::new(48000, 1, ramp(1000));
        assert_eq!(
            audio.bins(0, 0.0, 1.0, 3),
            vec![mm(0.0, 0.0), mm(1.0, 1.0), mm(2.0, 2.0)]
        );
        assert_eq!(
            audio.bins(0, 10.0, 10.0, 2),
            vec![mm(10.0, 19.0), mm(20.0, 29.0)]
        );
        assert_eq!(
            audio.bins(0, 0.0, 128.0, 2),
            vec![mm(0.0, 127.0), mm(128.0, 255.0)]
        );
        assert_eq!(
            audio.bins(0, 0.0, 512.0, 2),
            vec![mm(0.0, 511.0), mm(512.0, 999.0)]
        );
        assert_eq!(
            audio.bins(0, 30.0, 100.0, 2),
            vec![mm(30.0, 129.0), mm(130.0, 229.0)]
        );
    }

    #[test]
    fn a_square_wave_gives_full_swing_only_in_bins_that_span_an_edge() {
        let square: Vec<f32> = (0..4096)
            .map(|i| if (i / 256) % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let audio = DecodedAudio::new(48000, 1, square);
        assert_eq!(
            audio.bins(0, 0.0, 256.0, 2),
            vec![mm(1.0, 1.0), mm(-1.0, -1.0)]
        );
        assert_eq!(audio.bins(0, 128.0, 256.0, 1), vec![mm(-1.0, 1.0)]);
        assert_eq!(audio.bins(0, 0.0, 1024.0, 4), vec![mm(-1.0, 1.0); 4]);
    }

    #[test]
    fn bins_past_either_end_are_none_and_a_partial_bin_is_clipped() {
        let audio = DecodedAudio::new(48000, 1, ramp(100));
        assert_eq!(
            audio.bins(0, -20.0, 10.0, 3),
            vec![None, None, mm(0.0, 9.0)]
        );
        assert_eq!(audio.bins(0, 90.0, 20.0, 2), vec![mm(90.0, 99.0), None]);
        assert_eq!(audio.bins(0, -5.0, 10.0, 1), vec![mm(0.0, 4.0)]);
    }

    #[test]
    fn a_bin_narrower_than_a_frame_shows_the_frame_it_starts_in() {
        let audio = DecodedAudio::new(48000, 1, ramp(10));
        assert_eq!(
            audio.bins(0, 2.0, 0.5, 4),
            vec![mm(2.0, 2.0), mm(2.0, 2.0), mm(3.0, 3.0), mm(3.0, 3.0)]
        );
    }

    #[test]
    fn empty_audio_has_no_bins() {
        let audio = DecodedAudio::new(48000, 2, Vec::new());
        assert_eq!(audio.frames(), 0);
        assert_eq!(audio.bins(1, 0.0, 64.0, 2), vec![None, None]);
    }

    #[test]
    fn stereo_channels_are_read_apart() {
        let samples: Vec<f32> = (0..2000).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let audio = DecodedAudio::new(44100, 2, samples);
        assert_eq!(audio.frames(), 2000);
        assert_eq!(audio.bins(0, 100.0, 300.0, 1), vec![mm(100.0, 399.0)]);
        assert_eq!(audio.bins(1, 100.0, 300.0, 1), vec![mm(-399.0, -100.0)]);
    }

    #[test]
    fn every_bin_matches_a_brute_force_scan() {
        let channels = 3;
        let frames = 5000;
        let samples: Vec<f32> = (0..frames * channels)
            .map(|i| ((i * 7919) % 1013) as f32 - 506.0)
            .collect();
        let audio = DecodedAudio::new(48000, channels, samples.clone());
        for (start, frames_per_bin) in [
            (0.0, 1.0),
            (3.0, 63.0),
            (17.5, 200.3),
            (64.0, 1024.0),
            (999.0, 4096.0),
        ] {
            for channel in 0..channels {
                let bins = audio.bins(channel, start, frames_per_bin, 8);
                for (i, bin) in bins.into_iter().enumerate() {
                    let a = (start + i as f64 * frames_per_bin).floor() as usize;
                    let b =
                        ((start + (i + 1) as f64 * frames_per_bin).floor() as usize).min(frames);
                    let expected = (a < b).then(|| brute_force(&samples, channels, channel, a, b));
                    assert_eq!(
                        bin, expected,
                        "start {start}, {frames_per_bin} per bin, bin {i}"
                    );
                }
            }
        }
    }

    #[test]
    fn samples_split_across_chunks_are_joined() {
        let values = [0.5f32, -1.0, 0.25, 3.0];
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let (mut samples, mut carry) = (Vec::new(), Vec::new());
        for chunk in [&bytes[..3], &bytes[3..9], &bytes[9..10], &bytes[10..]] {
            append_f32le(&mut samples, &mut carry, chunk);
        }
        assert_eq!(samples, values);
        assert!(carry.is_empty());
    }
}
