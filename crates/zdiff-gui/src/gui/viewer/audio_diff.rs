//! Sample difference: left minus right over aligned buffers, with peak and RMS difference and the
//! first differing frame. A right side in another rate or layout is decoded again at the left's.

use std::sync::Arc;

use eframe::egui::{self, Align2, Rect, vec2};
use zcommon::ffmpeg::FfmpegPaths;
use zcommon::job::Job;
use zcommon_ui::style_window::role_color;

use super::{
    Likeness,
    audio::{
        DecodeProgress, DecodedAudio, RepaintContext, TimeView, decode_file, draw_wave,
        format_time, repaint_job,
    },
};
use crate::{file::LoadedFile, ui_egui::file_role::FileRole};

#[derive(Debug)]
pub struct SampleDifference {
    /// Left minus right; the shorter side's missing tail counts as silence.
    pub wave: DecodedAudio,
    pub peak: f32,
    pub rms: f64,
    pub side_rms: [f64; 2],
    pub first_frame: Option<usize>,
}

impl SampleDifference {
    pub fn first_secs(&self) -> Option<f64> {
        self.first_frame
            .map(|frame| frame as f64 / self.wave.sample_rate as f64)
    }

    pub fn likeness(&self) -> f64 {
        let level = self.side_rms[0] + self.side_rms[1];
        if level == 0.0 {
            return 1.0;
        }
        // At most 1 by the triangle inequality, short of rounding.
        (1.0 - self.rms / level).clamp(0.0, 1.0)
    }
}

pub fn sample_difference(left: &DecodedAudio, right: &DecodedAudio) -> SampleDifference {
    assert_eq!(
        (left.sample_rate, left.channels),
        (right.sample_rate, right.channels),
        "unaligned buffers"
    );
    let len = left.samples.len().max(right.samples.len());
    let sample = |side: &DecodedAudio, i: usize| side.samples.get(i).copied().unwrap_or(0.0);
    let mut samples = Vec::with_capacity(len);
    let (mut peak, mut sum_sq, mut first_frame) = (0.0f32, 0.0f64, None);
    let mut side_sum_sq = [0.0f64; 2];
    for i in 0..len {
        let (l, r) = (sample(left, i), sample(right, i));
        let d = l - r;
        if d != 0.0 && first_frame.is_none() {
            first_frame = Some(i / left.channels);
        }
        peak = peak.max(d.abs());
        sum_sq += d as f64 * d as f64;
        side_sum_sq[0] += l as f64 * l as f64;
        side_sum_sq[1] += r as f64 * r as f64;
        samples.push(d);
    }
    let rms = |sum_sq: f64| {
        if len == 0 {
            0.0
        } else {
            (sum_sq / len as f64).sqrt()
        }
    };
    SampleDifference {
        wave: DecodedAudio::new(left.sample_rate, left.channels, samples),
        peak,
        rms: rms(sum_sq),
        side_rms: side_sum_sq.map(rms),
        first_frame,
    }
}

/// The rate and channel count to decode the right side at, when it differs from the left.
pub fn resample_target(left: &DecodedAudio, right: &DecodedAudio) -> Option<(u32, usize)> {
    let target = (left.sample_rate, left.channels);
    (target != (right.sample_rate, right.channels)).then_some(target)
}

enum DiffState {
    Empty,
    /// `progress` is the right side's resampled decode, if it needs one.
    Running(Option<Arc<DecodeProgress>>),
    Done(Arc<SampleDifference>),
    Cancelled,
    Failed(String),
}

impl std::fmt::Debug for DiffState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiffState::Empty => write!(f, "Empty"),
            DiffState::Running(progress) => f.debug_tuple("Running").field(progress).finish(),
            DiffState::Done(difference) => f.debug_tuple("Done").field(&difference.peak).finish(),
            DiffState::Cancelled => write!(f, "Cancelled"),
            DiffState::Failed(reason) => f.debug_tuple("Failed").field(reason).finish(),
        }
    }
}

type DiffResult = Result<SampleDifference, Option<String>>;

/// A resample failure stays here, not in the side failures: no reason to leave the audio view.
#[derive(Debug)]
pub struct SampleDiffProcessor {
    /// The decoded sides `state` is for.
    inputs: Option<[Arc<DecodedAudio>; 2]>,
    resampled_to: Option<(u32, usize)>,
    state: DiffState,
    job: Job<DiffResult>,
}

impl Default for SampleDiffProcessor {
    fn default() -> Self {
        Self::new(&RepaintContext::default())
    }
}

pub type DiffInputs<'a> = (
    &'a FfmpegPaths,
    &'a LoadedFile,
    &'a Arc<DecodedAudio>,
    &'a Arc<DecodedAudio>,
);

impl SampleDiffProcessor {
    pub(super) fn new(context: &RepaintContext) -> Self {
        Self {
            inputs: None,
            resampled_to: None,
            state: DiffState::Empty,
            job: repaint_job(context).on_panic(|e| Err(Some(e))),
        }
    }

    /// Restarts when either decoded side changed; `None` drops the difference.
    pub fn update(&mut self, inputs: Option<DiffInputs>) {
        let Some((ffmpeg, right_file, left, right)) = inputs else {
            self.clear();
            return;
        };
        if let Some([l, r]) = &self.inputs
            && Arc::ptr_eq(l, left)
            && Arc::ptr_eq(r, right)
        {
            return;
        }
        self.inputs = Some([left.clone(), right.clone()]);
        self.resampled_to = resample_target(left, right);
        let progress = self
            .resampled_to
            .map(|_| Arc::new(DecodeProgress::default()));
        let (ffmpeg, file) = (ffmpeg.clone(), right_file.clone());
        let (left, right) = (left.clone(), right.clone());
        let (target, worker_progress) = (self.resampled_to, progress.clone());
        self.job.start(move |ctx| {
            let difference = match target.zip(worker_progress) {
                Some((target, progress)) => {
                    decode_file(&ffmpeg, &file, Some(target), ctx.cancel_flag(), &progress)
                        .map(|resampled| sample_difference(&left, &resampled))
                        .map_err(|e| {
                            e.map(|reason| format!("{} can't be resampled: {reason}", file.path()))
                        })
                }
                None => Ok(sample_difference(&left, &right)),
            };
            ctx.send(difference);
        });
        self.state = DiffState::Running(progress);
    }

    pub fn clear(&mut self) {
        // Runs every frame while a side isn't decoded.
        if self.inputs.is_none() && matches!(self.state, DiffState::Empty) {
            return;
        }
        self.job.cancel();
        self.inputs = None;
        self.resampled_to = None;
        self.state = DiffState::Empty;
    }

    /// Kills a running difference; it stays cancelled until `decode_again`.
    pub fn cancel(&mut self) {
        if self.running() {
            self.job.cancel();
            self.state = DiffState::Cancelled;
        }
    }

    /// The next `update` starts over.
    pub fn decode_again(&mut self) {
        if self.cancelled() {
            self.inputs = None;
        }
    }

    pub fn poll(&mut self) {
        for difference in self.job.poll() {
            self.state = match difference {
                Ok(difference) => DiffState::Done(Arc::new(difference)),
                Err(Some(reason)) => DiffState::Failed(reason),
                Err(None) => DiffState::Cancelled,
            };
        }
    }

    pub fn running(&self) -> bool {
        matches!(self.state, DiffState::Running(_))
    }

    pub fn cancelled(&self) -> bool {
        matches!(self.state, DiffState::Cancelled)
    }

    /// One lane per channel of the left side, once there is a difference to show or wait for.
    pub fn lanes(&self) -> usize {
        match (&self.state, &self.inputs) {
            (DiffState::Empty, _) | (_, None) => 0,
            (_, Some([left, _])) => left.channels,
        }
    }

    pub fn likeness(&self) -> Option<Likeness> {
        match &self.state {
            DiffState::Running(_) => Some(Likeness::Pending),
            DiffState::Done(difference) => Some(Likeness::Measured {
                ratio: difference.likeness(),
                detail: "1 - RMS(left - right) / (RMS(left) + RMS(right))".to_string(),
            }),
            DiffState::Empty | DiffState::Cancelled | DiffState::Failed(_) => None,
        }
    }

    pub fn status_text(&self) -> Option<String> {
        let mut text = match &self.state {
            DiffState::Empty => return None,
            DiffState::Running(None) => "computing...".to_string(),
            DiffState::Running(Some(progress)) => match progress.fraction() {
                Some(fraction) if fraction < 1.0 => {
                    format!("resampling the right side {:.0}%", fraction * 100.0)
                }
                Some(_) => "computing...".to_string(),
                None => "resampling the right side...".to_string(),
            },
            DiffState::Done(difference) => stats_text(difference),
            DiffState::Cancelled => "cancelled".to_string(),
            DiffState::Failed(reason) => reason.clone(),
        };
        if let Some((rate, channels)) = self.resampled_to {
            text += &format!(" (right side resampled to {rate} Hz, {channels} ch)");
        }
        Some(text)
    }

    pub fn draw_lane(
        &self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        rect: Rect,
        channel: usize,
        view: TimeView,
    ) {
        let visuals = ui.visuals();
        match &self.state {
            DiffState::Done(difference) => {
                let color = role_color(ui.ctx(), FileRole::Modified);
                draw_wave(painter, rect, &difference.wave, channel, view, color);
            }
            _ => {
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    self.status_text().unwrap_or_default(),
                    egui::TextStyle::Body.resolve(ui.style()),
                    visuals.text_color(),
                );
            }
        }
        painter.text(
            rect.left_top() + vec2(4.0, 2.0),
            Align2::LEFT_TOP,
            format!("Difference {}", channel + 1),
            egui::TextStyle::Small.resolve(ui.style()),
            visuals.weak_text_color(),
        );
    }
}

fn stats_text(difference: &SampleDifference) -> String {
    let Some((frame, secs)) = difference.first_frame.zip(difference.first_secs()) else {
        return "identical".to_string();
    };
    let dbfs = |level: f64| 20.0 * level.log10();
    format!(
        "peak {:.6} ({:.1} dBFS), RMS {:.6} ({:.1} dBFS), first at {} (frame {frame})",
        difference.peak,
        dbfs(difference.peak as f64),
        difference.rms,
        dbfs(difference.rms),
        format_time(secs, 3),
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::file::BinaryFile;
    use zdiff::universal_path::UniversalPath;

    fn audio(sample_rate: u32, channels: usize, samples: &[f32]) -> DecodedAudio {
        DecodedAudio::new(sample_rate, channels, samples.to_vec())
    }

    #[test]
    fn identical_buffers_give_zero() {
        let samples: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
        let difference = sample_difference(&audio(100, 2, &samples), &audio(100, 2, &samples));
        assert_eq!(difference.peak, 0.0);
        assert_eq!(difference.rms, 0.0);
        assert_eq!(difference.first_frame, None);
        assert_eq!(difference.first_secs(), None);
        assert!(difference.wave.samples.iter().all(|&s| s == 0.0));
        assert_eq!(difference.wave.frames(), 500);
    }

    #[test]
    fn a_single_changed_sample_is_found_at_its_frames_time() {
        let left = vec![0.25; 2 * 1000];
        let mut right = left.clone();
        right[2 * 750 + 1] = -0.25;
        let difference = sample_difference(&audio(500, 2, &left), &audio(500, 2, &right));
        assert_eq!(difference.first_frame, Some(750));
        assert_eq!(difference.first_secs(), Some(1.5));
        assert_eq!(difference.peak, 0.5);
        assert_eq!(difference.wave.samples[2 * 750 + 1], 0.5);
        assert_eq!(difference.wave.samples[2 * 750], 0.0);
    }

    #[test]
    fn peak_and_rms_of_a_known_pair() {
        let left = audio(8, 1, &[0.5, 0.25, 0.0, -0.5]);
        let right = audio(8, 1, &[0.0, 0.25, 0.5, 0.5]);
        let difference = sample_difference(&left, &right);
        assert_eq!(difference.wave.samples, [0.5, 0.0, -0.5, -1.0]);
        assert_eq!(difference.peak, 1.0);
        assert!(
            (difference.rms - 0.375f64.sqrt()).abs() < 1e-12,
            "{}",
            difference.rms
        );
        assert_eq!(difference.first_frame, Some(0));
    }

    #[test]
    fn likeness_is_one_minus_the_rms_difference_over_both_levels() {
        let left = audio(8, 1, &[0.5, -0.5, 0.25, -0.25]);
        let scaled = |factor: f32| {
            let samples: Vec<f32> = left.samples.iter().map(|s| s * factor).collect();
            audio(8, 1, &samples)
        };
        let likeness = |right: &DecodedAudio| sample_difference(&left, right).likeness();
        assert_eq!(likeness(&scaled(1.0)), 1.0);
        assert_eq!(likeness(&scaled(0.0)), 0.0);
        assert_eq!(likeness(&scaled(-1.0)), 0.0);
        assert!((likeness(&scaled(0.5)) - 2.0 / 3.0).abs() < 1e-9);
        let silence = audio(8, 1, &[0.0; 4]);
        assert_eq!(sample_difference(&silence, &silence).likeness(), 1.0);
    }

    #[test]
    fn the_shorter_sides_missing_tail_is_silence() {
        let left = audio(10, 1, &[0.1, 0.2, 0.3, 0.0]);
        let right = audio(10, 1, &[0.1, 0.2]);
        let difference = sample_difference(&left, &right);
        assert_eq!(difference.wave.samples, [0.0, 0.0, 0.3, 0.0]);
        assert_eq!(difference.first_frame, Some(2));

        let reversed = sample_difference(&right, &left);
        assert_eq!(reversed.wave.samples, [0.0, 0.0, -0.3, 0.0]);

        let silent_tail = sample_difference(&audio(10, 1, &[0.1, 0.0]), &audio(10, 1, &[0.1]));
        assert_eq!(silent_tail.first_frame, None);
    }

    #[test]
    fn the_right_side_is_resampled_to_the_lefts_rate_and_layout_only_when_they_differ() {
        let stereo_48k = audio(48000, 2, &[]);
        let stereo_44k = audio(44100, 2, &[]);
        let mono_48k = audio(48000, 1, &[]);
        assert_eq!(resample_target(&stereo_48k, &audio(48000, 2, &[])), None);
        assert_eq!(resample_target(&stereo_44k, &stereo_48k), Some((44100, 2)));
        assert_eq!(resample_target(&mono_48k, &stereo_48k), Some((48000, 1)));
    }

    fn missing_ffmpeg() -> FfmpegPaths {
        let dir = std::env::temp_dir().join("zdiff-no-such-ffmpeg");
        FfmpegPaths {
            ffmpeg: dir.join("ffmpeg.exe"),
            ffprobe: dir.join("ffprobe.exe"),
        }
    }

    fn audio_file(name: &str) -> LoadedFile {
        LoadedFile::Binary(Arc::new(BinaryFile {
            path: UniversalPath::from(name),
            bytes: vec![0; 16],
        }))
    }

    fn poll_until(processor: &mut SampleDiffProcessor) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while processor.running() {
            assert!(Instant::now() < deadline, "timed out: {processor:?}");
            processor.poll();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn sides_in_the_same_layout_are_compared_without_ffmpeg() {
        let (ffmpeg, file) = (missing_ffmpeg(), audio_file("b.wav"));
        let left = Arc::new(audio(8, 1, &[0.5, 0.25, 0.0, -0.5]));
        let right = Arc::new(audio(8, 1, &[0.0, 0.25, 0.5, 0.5]));
        let mut processor = SampleDiffProcessor::default();
        processor.update(Some((&ffmpeg, &file, &left, &right)));
        assert!(processor.running());
        assert_eq!(processor.likeness(), Some(Likeness::Pending));
        poll_until(&mut processor);
        assert!(
            matches!(processor.likeness(), Some(Likeness::Measured { ratio, .. }) if ratio < 1.0),
            "{:?}",
            processor.likeness()
        );
        let text = processor.status_text().unwrap();
        assert!(text.contains("peak 1.000000 (0.0 dBFS)"), "{text}");
        assert!(text.contains("RMS 0.612372 (-4.3 dBFS)"), "{text}");
        assert!(text.contains("first at 0:00.000 (frame 0)"), "{text}");
        assert!(!text.contains("resampled"), "{text}");

        processor.update(Some((&ffmpeg, &file, &left, &right)));
        assert!(!processor.running(), "same sides: no-op");

        let same = Arc::new(audio(8, 1, &[0.5, 0.25, 0.0, -0.5]));
        processor.update(Some((&ffmpeg, &file, &left, &same)));
        poll_until(&mut processor);
        assert_eq!(processor.status_text().unwrap(), "identical");

        processor.update(None);
        assert_eq!(processor.status_text(), None);
    }

    #[test]
    fn a_right_side_in_another_rate_is_resampled_with_a_note_and_a_failure_stays_here() {
        let (ffmpeg, file) = (missing_ffmpeg(), audio_file("b.wav"));
        let left = Arc::new(audio(44100, 2, &[0.0; 8]));
        let right = Arc::new(audio(48000, 2, &[0.0; 8]));
        let mut processor = SampleDiffProcessor::default();
        processor.update(Some((&ffmpeg, &file, &left, &right)));
        poll_until(&mut processor);
        let text = processor.status_text().unwrap();
        assert!(text.contains("b.wav"), "{text}");
        assert!(text.contains("ffprobe"), "{text}");
        assert!(
            text.contains("right side resampled to 44100 Hz, 2 ch"),
            "{text}"
        );

        processor.clear();
        assert_eq!(processor.status_text(), None);
    }

    #[test]
    fn dropping_an_empty_difference_changes_nothing() {
        let mut processor = SampleDiffProcessor::default();
        processor.update(None);
        processor.clear();
        assert!(!processor.running());
        assert_eq!(processor.status_text(), None);
    }

    #[test]
    fn a_cancelled_difference_runs_again_on_request() {
        let (ffmpeg, file) = (missing_ffmpeg(), audio_file("b.wav"));
        let left = Arc::new(audio(44100, 1, &[0.0; 8]));
        let right = Arc::new(audio(48000, 1, &[0.0; 8]));
        let mut processor = SampleDiffProcessor::default();
        processor.update(Some((&ffmpeg, &file, &left, &right)));
        processor.cancel();
        assert!(processor.cancelled());
        std::thread::sleep(Duration::from_millis(50));
        processor.poll();
        processor.update(Some((&ffmpeg, &file, &left, &right)));
        assert!(processor.cancelled(), "same sides: stays cancelled");

        processor.decode_again();
        processor.update(Some((&ffmpeg, &file, &left, &right)));
        assert!(processor.running());
        poll_until(&mut processor);
        assert!(!processor.cancelled());
        assert!(processor.status_text().unwrap().contains("ffprobe"));
    }
}
