//! Video difference timeline: both videos decoded scaled down at a common frame rate on workers,
//! the mean absolute pixel difference per frame drawn as a graph that seeks the frame compare.

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use eframe::egui::{self, Sense, Stroke, pos2, vec2};
use zcommon::ffmpeg::{FfmpegError, FfmpegPaths, run_ffmpeg, scaled_frames_args};
use zcommon::job::{Job, JobCtx};
use zcommon_ui::style_window::{diff_gradient, diff_gradient_picker, gradient_color};

use zdiff::pixel::RgbaBuffer;
use zdiff::pixel_metric::{Alignment, PixelMetric, metric_map};

use super::Likeness;
use super::video_frames::{VideoTrack, end_time, format_time};

pub const TIMELINE_WIDTH: u32 = 160;
const MAX_TIMELINE_HEIGHT: u32 = 4 * TIMELINE_WIDTH;
const FRAMES_IN_FLIGHT: usize = 8;
const GRAPH_HEIGHT: f32 = 48.0;

/// Mean absolute difference of the RGB channels, 0..=255. Alpha is ignored: ffmpeg's rgba frames
/// are opaque.
pub fn mean_abs_diff(a: &[u8], b: &[u8]) -> f32 {
    assert_eq!(a.len(), b.len(), "frames of different sizes");
    assert_eq!(a.len() % 4, 0, "not RGBA");
    if a.is_empty() {
        return 0.0;
    }
    let sum: u64 = a
        .chunks_exact(4)
        .zip(b.chunks_exact(4))
        .map(|(a, b)| (0..3).map(|c| a[c].abs_diff(b[c]) as u64).sum::<u64>())
        .sum();
    (sum as f64 / (a.len() / 4 * 3) as f64) as f32
}

/// 0..=255. Exact keeps the mean over the RGB channels; the others are the metric's mean.
pub fn frame_difference(
    metric: PixelMetric,
    (width, height): (u32, u32),
    a: &[u8],
    b: &[u8],
) -> f32 {
    if metric == PixelMetric::Exact {
        return mean_abs_diff(a, b);
    }
    let frame = |rgba| RgbaBuffer {
        width,
        height,
        rgba,
    };
    let map = metric_map(
        frame(a),
        frame(b),
        metric,
        Alignment::TopLeft,
        &AtomicBool::new(false),
    )
    .expect("never cancelled");
    (map.difference * 255.0) as f32
}

/// Joins stdout chunks into whole frames of `frame_len` bytes; a partial frame waits in `carry`.
pub fn split_frames(
    carry: &mut Vec<u8>,
    chunk: &[u8],
    frame_len: usize,
    mut emit: impl FnMut(Vec<u8>),
) {
    assert!(frame_len > 0);
    let mut chunk = chunk;
    if !carry.is_empty() {
        let take = (frame_len - carry.len()).min(chunk.len());
        carry.extend_from_slice(&chunk[..take]);
        chunk = &chunk[take..];
        if carry.len() < frame_len {
            return;
        }
        emit(std::mem::take(carry));
    }
    let mut frames = chunk.chunks_exact(frame_len);
    for frame in &mut frames {
        emit(frame.to_vec());
    }
    carry.extend_from_slice(frames.remainder());
}

/// Pairs frame i of each side; past the shorter side's end, its last frame stays, like the frame
/// compare. `push` returning false stops early.
pub fn pair_frames(
    left: impl Iterator<Item = Result<Vec<u8>, String>>,
    right: impl Iterator<Item = Result<Vec<u8>, String>>,
    difference: impl Fn(&[u8], &[u8]) -> f32,
    mut push: impl FnMut(f32) -> bool,
) -> Result<(), String> {
    let (mut left, mut right) = (left.fuse(), right.fuse());
    let mut last: [Option<Vec<u8>>; 2] = [None, None];
    loop {
        let next = [left.next().transpose()?, right.next().transpose()?];
        if next.iter().all(Option::is_none) {
            break;
        }
        for (last, next) in last.iter_mut().zip(next) {
            if next.is_some() {
                *last = next;
            }
        }
        let [Some(a), Some(b)] = &last else {
            break;
        };
        if !push(difference(a, b)) {
            return Ok(());
        }
    }
    match &last {
        [Some(_), Some(_)] => Ok(()),
        [None, None] => Err("neither video gave a frame".to_string()),
        [None, _] => Err("the left video gave no frames".to_string()),
        [_, None] => Err("the right video gave no frames".to_string()),
    }
}

/// Both sides are scaled to this one size, from the left side's aspect; the height is even.
pub fn timeline_size(track: &VideoTrack) -> (u32, u32) {
    let height = TIMELINE_WIDTH as f64 * track.height as f64 / track.width as f64;
    let height = ((height / 2.0).round() as u32 * 2).clamp(2, MAX_TIMELINE_HEIGHT);
    (TIMELINE_WIDTH, height)
}

/// The faster side's rate, so every frame of both sides is compared, as the frame steps visit them.
pub fn common_fps(tracks: &[VideoTrack; 2]) -> f64 {
    tracks[0].fps.max(tracks[1].fps)
}

/// Sample i is at `i / fps`; the count is an estimate for progress only.
pub fn expected_samples(tracks: &[VideoTrack; 2], fps: f64) -> usize {
    (end_time(tracks) * fps).round() as usize + 1
}

fn decode_side(
    ffmpeg: &FfmpegPaths,
    input: &Path,
    (width, height): (u32, u32),
    fps: f64,
    cancel: &AtomicBool,
    frames: mpsc::SyncSender<Result<Vec<u8>, String>>,
) {
    let frame_len = width as usize * height as usize * 4;
    let mut carry = Vec::new();
    let args = scaled_frames_args(input, width, height, fps);
    let result = run_ffmpeg(&ffmpeg.ffmpeg, &args, cancel, |chunk| {
        // A failed send means the pairing stopped, and `cancel` is set then, which kills ffmpeg.
        split_frames(&mut carry, chunk, frame_len, |frame| {
            let _ = frames.send(Ok(frame));
        })
    });
    let error = match result {
        Ok(()) if carry.is_empty() => return,
        Err(FfmpegError::Cancelled) => return,
        Ok(()) => format!("{}: ffmpeg stopped inside a frame", input.display()),
        Err(e) => format!("{}: {e}", input.display()),
    };
    let _ = frames.send(Err(error));
}

/// Decodes both sides at once, so only a few frames are ever held, and sends one value per pair.
fn run_job(job: TimelineJob, fps: f64, metric: PixelMetric, ctx: &JobCtx<TimelineMessage>) {
    let size = timeline_size(&job.tracks[0]);
    let receivers = job.inputs.map(|input| {
        let (frames, receiver) = mpsc::sync_channel(FRAMES_IN_FLIGHT);
        let (ffmpeg, cancel) = (job.ffmpeg.clone(), ctx.cancel_flag().clone());
        std::thread::spawn(move || decode_side(&ffmpeg, &input, size, fps, &cancel, frames));
        receiver
    });
    let [left, right] = receivers;
    let paired = pair_frames(
        left.into_iter(),
        right.into_iter(),
        |a, b| frame_difference(metric, size, a, b),
        |value| ctx.send(TimelineMessage::Value(value)),
    );
    let result = match paired {
        _ if ctx.is_cancelled() => Err(None),
        Err(reason) => Err(Some(reason)),
        Ok(()) => Ok(()),
    };
    let failed = result.is_err();
    ctx.send(TimelineMessage::Done(result));
    if failed {
        // Stops the other side's ffmpeg; after the send, which a set flag would drop.
        ctx.cancel_flag().store(true, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, PartialEq)]
enum TimelineState {
    Running,
    Cancelled,
    Failed(String),
    Done,
}

#[derive(Debug, Clone)]
struct TimelineJob {
    ffmpeg: FfmpegPaths,
    inputs: [PathBuf; 2],
    tracks: [VideoTrack; 2],
}

#[derive(Debug)]
enum TimelineMessage {
    Value(f32),
    /// `Err(None)` is a cancelled job.
    Done(Result<(), Option<String>>),
}

/// One job per video pair, started once both sides are probed. A cancelled job keeps the samples
/// it computed until it runs again.
#[derive(Debug)]
pub struct VideoTimeline {
    job: Option<TimelineJob>,
    state: TimelineState,
    fps: f64,
    expected: usize,
    values: Vec<f32>,
    sum: f64,
    metric: PixelMetric,
    finished: Vec<FinishedMetric>,
    run_job: Job<TimelineMessage>,
}

#[derive(Debug)]
struct FinishedMetric {
    metric: PixelMetric,
    values: Vec<f32>,
    sum: f64,
}

impl Default for VideoTimeline {
    fn default() -> Self {
        Self {
            job: None,
            state: TimelineState::Done,
            fps: 1.0,
            expected: 0,
            values: Vec::new(),
            sum: 0.0,
            metric: PixelMetric::default(),
            finished: Vec::new(),
            run_job: Job::new(|| {}).on_panic(|e| TimelineMessage::Done(Err(Some(e)))),
        }
    }
}

impl VideoTimeline {
    pub fn start(&mut self, ffmpeg: &FfmpegPaths, inputs: [PathBuf; 2], tracks: [VideoTrack; 2]) {
        self.job = Some(TimelineJob {
            ffmpeg: ffmpeg.clone(),
            inputs,
            tracks,
        });
        self.finished.clear();
        self.run();
    }

    fn run(&mut self) {
        let job = self.job.clone().expect("a started timeline has a job");
        self.stop();
        log::info!(
            "Video difference timeline started: {} vs {}",
            job.inputs[0].display(),
            job.inputs[1].display()
        );
        self.fps = common_fps(&job.tracks);
        self.expected = expected_samples(&job.tracks, self.fps);
        self.clear_values();
        self.state = TimelineState::Running;
        let (fps, metric) = (self.fps, self.metric);
        self.run_job
            .start(move |ctx| run_job(job, fps, metric, ctx));
    }

    pub fn set_metric(&mut self, metric: PixelMetric) {
        if self.metric == metric {
            return;
        }
        if self.job.is_some() && self.state == TimelineState::Done {
            self.finished.push(FinishedMetric {
                metric: self.metric,
                values: std::mem::take(&mut self.values),
                sum: self.sum,
            });
        }
        self.metric = metric;
        match self.finished.iter().position(|f| f.metric == metric) {
            Some(index) => {
                self.stop();
                let finished = self.finished.swap_remove(index);
                self.values = finished.values;
                self.sum = finished.sum;
                self.state = TimelineState::Done;
            }
            None => self.compute_again(),
        }
    }

    fn stop(&mut self) {
        self.run_job.cancel();
    }

    pub fn clear(&mut self) {
        self.stop();
        self.job = None;
        self.state = TimelineState::Done;
        self.clear_values();
        self.finished.clear();
    }

    fn clear_values(&mut self) {
        self.values.clear();
        self.sum = 0.0;
    }

    fn push(&mut self, value: f32) {
        self.values.push(value);
        self.sum += value as f64;
    }

    pub fn likeness(&self) -> Option<Likeness> {
        let partial = match &self.state {
            TimelineState::Running => return Some(Likeness::Pending),
            TimelineState::Failed(_) => return None,
            _ if self.values.is_empty() => return None,
            TimelineState::Cancelled => " (cancelled, partial)",
            TimelineState::Done => "",
        };
        let mean = self.sum / self.values.len() as f64;
        Some(Likeness::Measured {
            ratio: (1.0 - mean / 255.0).clamp(0.0, 1.0),
            detail: format!(
                "1 - mean {} difference / 255 over {} timeline frames{partial}",
                self.metric.name(),
                self.values.len()
            ),
        })
    }

    pub fn is_started(&self) -> bool {
        self.job.is_some()
    }

    pub fn running(&self) -> bool {
        self.state == TimelineState::Running
    }

    pub fn cancel(&mut self) {
        if self.running() {
            self.stop();
            self.state = TimelineState::Cancelled;
        }
    }

    pub fn compute_again(&mut self) {
        if self.job.is_some() {
            self.run();
        }
    }

    pub fn poll(&mut self) {
        for message in self.run_job.poll() {
            match message {
                TimelineMessage::Value(value) => self.push(value),
                TimelineMessage::Done(Ok(())) => self.state = TimelineState::Done,
                TimelineMessage::Done(Err(None)) => self.state = TimelineState::Cancelled,
                TimelineMessage::Done(Err(Some(reason))) => {
                    log::warn!("Video difference timeline failed: {reason}");
                    self.state = TimelineState::Failed(reason);
                }
            }
        }
    }

    fn sample_time(&self, index: usize) -> f64 {
        index as f64 / self.fps
    }

    fn status_text(&self) -> String {
        let peak = self
            .values
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .filter(|(_, value)| **value > 0.0)
            .map(|(index, value)| {
                format!(
                    "peak {value:.1} at {}",
                    format_time(self.sample_time(index))
                )
            });
        match &self.state {
            TimelineState::Running => {
                let percent = self.values.len() * 100 / self.expected.max(1);
                format!("Difference timeline {}%", percent.min(99))
            }
            TimelineState::Cancelled => format!(
                "Difference timeline cancelled at {}",
                format_time(self.sample_time(self.values.len()))
            ),
            TimelineState::Failed(reason) => format!("Difference timeline failed: {reason}"),
            TimelineState::Done => match peak {
                Some(peak) if self.metric == PixelMetric::Exact => {
                    format!("Mean pixel difference per frame, {peak}")
                }
                Some(peak) => format!("Mean {} difference per frame, {peak}", self.metric.name()),
                None => "No difference between the frames".to_string(),
            },
        }
    }
}

/// The per-frame difference graph over `[0, end]`, with the frame compare's time as a cursor.
/// Returns the time a click or drag seeks to.
pub fn show(
    ui: &mut egui::Ui,
    timeline: &mut VideoTimeline,
    time_secs: f64,
    end: f64,
) -> Option<f64> {
    if !timeline.is_started() {
        return None;
    }
    if timeline.running() {
        // Samples arrive from workers; keep frames coming until they are polled.
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }
    let gradient = diff_gradient(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(timeline.status_text());
        if timeline.running() && ui.button("Cancel").clicked() {
            timeline.cancel();
        }
        if timeline.state == TimelineState::Cancelled && ui.button("Compute again").clicked() {
            timeline.compute_again();
        }
        diff_gradient_picker(ui, "video_timeline_gradient");
    });

    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), GRAPH_HEIGHT),
        Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
    if end <= 0.0 || rect.width() < 1.0 {
        return None;
    }
    let columns = rect.width() as usize;
    let mut peaks = vec![None::<f32>; columns];
    for (index, &value) in timeline.values.iter().enumerate() {
        let column = (timeline.sample_time(index) / end * columns as f64) as usize;
        let peak = &mut peaks[column.min(columns - 1)];
        *peak = Some(peak.map_or(value, |peak| peak.max(value)));
    }
    let scale = peaks.iter().flatten().fold(1.0f32, |a, &b| a.max(b));
    for (column, peak) in peaks.iter().enumerate() {
        let Some(peak) = peak else {
            continue;
        };
        let x = rect.left() + column as f32 + 0.5;
        let height = peak / scale;
        let top = rect.bottom() - (height * (rect.height() - 2.0)).max(1.0);
        let color = gradient_color(gradient, height);
        painter.line_segment(
            [pos2(x, rect.bottom()), pos2(x, top)],
            Stroke::new(1.0_f32, color),
        );
    }
    let cursor = rect.left() + (time_secs / end).clamp(0.0, 1.0) as f32 * rect.width();
    painter.line_segment(
        [pos2(cursor, rect.top()), pos2(cursor, rect.bottom())],
        Stroke::new(1.0_f32, ui.visuals().text_color()),
    );
    let top = rect.top();
    let handle = vec![
        pos2(cursor - 5.0, top),
        pos2(cursor + 5.0, top),
        pos2(cursor, top + 7.0),
    ];
    painter.add(egui::Shape::convex_polygon(
        handle,
        ui.visuals().text_color(),
        Stroke::NONE,
    ));

    let pointer = response.interact_pointer_pos()?;
    (response.clicked() || response.dragged())
        .then(|| ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * end)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn frame(pixels: &[[u8; 3]]) -> Vec<u8> {
        pixels
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect()
    }

    #[test]
    fn identical_frames_differ_by_zero() {
        let a = frame(&[[0, 0, 0], [255, 255, 255], [12, 200, 7]]);
        assert_eq!(mean_abs_diff(&a, &a), 0.0);
    }

    #[test]
    fn a_known_change_gives_its_mean_over_the_rgb_channels() {
        let a = frame(&[[0, 0, 0], [100, 100, 100]]);
        assert_eq!(
            mean_abs_diff(&a, &frame(&[[255, 0, 0], [100, 100, 100]])),
            42.5
        );
        assert_eq!(
            mean_abs_diff(&a, &frame(&[[10, 10, 10], [90, 90, 90]])),
            10.0
        );
        let mut alpha = a.clone();
        alpha[3] = 0;
        assert_eq!(mean_abs_diff(&a, &alpha), 0.0);
    }

    #[test]
    fn the_frame_difference_follows_the_metric() {
        let line = |x: usize| {
            let mut pixels = [[0; 3]; 15];
            for y in 0..3 {
                pixels[y * 5 + x] = [200; 3];
            }
            frame(&pixels)
        };
        let (a, b) = (line(1), line(2));
        assert_eq!(
            frame_difference(PixelMetric::Exact, (5, 3), &a, &b),
            mean_abs_diff(&a, &b)
        );
        assert!(mean_abs_diff(&a, &b) > 0.0);
        let three = PixelMetric::Neighborhood(zdiff::pixel_metric::NeighborhoodSize::Three);
        assert_eq!(frame_difference(three, (5, 3), &a, &b), 0.0);
        assert_eq!(frame_difference(PixelMetric::Ssim, (5, 3), &a, &a), 0.0);
        assert!(frame_difference(PixelMetric::Ssim, (5, 3), &a, &b) > 0.0);
    }

    #[test]
    fn frames_split_across_chunks_are_joined() {
        let bytes: Vec<u8> = (0..20).collect();
        let mut frames = Vec::new();
        let mut carry = Vec::new();
        for chunk in [&bytes[..3], &bytes[3..4], &bytes[4..13], &bytes[13..]] {
            split_frames(&mut carry, chunk, 4, |frame| frames.push(frame));
        }
        assert_eq!(
            frames,
            bytes.chunks(4).map(<[u8]>::to_vec).collect::<Vec<_>>()
        );
        assert!(carry.is_empty());
        split_frames(&mut carry, &[1, 2], 4, |frame| frames.push(frame));
        assert_eq!((frames.len(), carry), (5, vec![1, 2]));
    }

    fn ok_frames(values: &[u8]) -> Vec<Result<Vec<u8>, String>> {
        values.iter().map(|&v| Ok(frame(&[[v, v, v]]))).collect()
    }

    fn paired(left: &[u8], right: &[u8]) -> Result<Vec<f32>, String> {
        let mut values = Vec::new();
        pair_frames(
            ok_frames(left).into_iter(),
            ok_frames(right).into_iter(),
            mean_abs_diff,
            |v| {
                values.push(v);
                true
            },
        )
        .map(|()| values)
    }

    #[test]
    fn the_shorter_side_holds_its_last_frame() {
        assert_eq!(paired(&[0, 5, 9], &[0, 7, 9]), Ok(vec![0.0, 2.0, 0.0]));
        assert_eq!(paired(&[0, 5], &[0, 5, 8, 1]), Ok(vec![0.0, 0.0, 3.0, 4.0]));
        assert_eq!(paired(&[1, 2, 3], &[4]), Ok(vec![3.0, 2.0, 1.0]));
    }

    #[test]
    fn a_side_without_frames_or_with_an_error_fails_the_pairing() {
        assert!(paired(&[1, 2], &[]).is_err());
        assert!(paired(&[], &[]).is_err());
        let mut values = Vec::new();
        let right = ok_frames(&[1])
            .into_iter()
            .chain([Err("broken".to_string())]);
        let result = pair_frames(
            ok_frames(&[1, 2, 3]).into_iter(),
            right,
            mean_abs_diff,
            |v| {
                values.push(v);
                true
            },
        );
        assert_eq!((result, values), (Err("broken".to_string()), vec![0.0]));
    }

    #[test]
    fn pairing_stops_when_push_refuses() {
        let mut count = 0;
        let result = pair_frames(
            ok_frames(&[1, 2, 3]).into_iter(),
            ok_frames(&[1, 2, 3]).into_iter(),
            mean_abs_diff,
            |_| {
                count += 1;
                count < 2
            },
        );
        assert_eq!((result, count), (Ok(()), 2));
    }

    fn track(width: u32, height: u32, fps: f64, frame_count: u64) -> VideoTrack {
        VideoTrack {
            width,
            height,
            fps,
            frame_count,
        }
    }

    #[test]
    fn the_timeline_is_160_wide_with_the_left_sides_aspect() {
        assert_eq!(timeline_size(&track(1920, 1080, 25.0, 1)), (160, 90));
        assert_eq!(timeline_size(&track(1080, 1920, 25.0, 1)), (160, 284));
        assert_eq!(timeline_size(&track(720, 576, 25.0, 1)), (160, 128));
        assert_eq!(timeline_size(&track(10_000, 1, 25.0, 1)), (160, 2));
        assert_eq!(timeline_size(&track(1, 10_000, 25.0, 1)), (160, 640));
    }

    #[test]
    fn samples_run_at_the_faster_rate_to_the_later_end() {
        let tracks = [track(4, 2, 24.0, 48), track(4, 2, 60.0, 60)];
        assert_eq!(common_fps(&tracks), 60.0);
        assert_eq!(expected_samples(&tracks, 60.0), 119);
        let same = [track(4, 2, 25.0, 250), track(4, 2, 25.0, 250)];
        assert_eq!(expected_samples(&same, common_fps(&same)), 250);
    }

    fn missing_ffmpeg() -> FfmpegPaths {
        FfmpegPaths {
            ffmpeg: "missing-ffmpeg".into(),
            ffprobe: "missing-ffprobe".into(),
        }
    }

    fn start(timeline: &mut VideoTimeline) {
        let tracks = [track(4, 2, 25.0, 25), track(4, 2, 25.0, 25)];
        timeline.start(&missing_ffmpeg(), ["a.mp4".into(), "b.mp4".into()], tracks);
    }

    fn poll_until_stopped(timeline: &mut VideoTimeline) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while timeline.running() {
            assert!(Instant::now() < deadline, "timed out: {timeline:?}");
            timeline.poll();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn an_unrunnable_ffmpeg_fails_the_timeline_naming_the_file() {
        let mut timeline = VideoTimeline::default();
        start(&mut timeline);
        assert!(timeline.running() && timeline.is_started());
        poll_until_stopped(&mut timeline);
        let TimelineState::Failed(reason) = &timeline.state else {
            panic!("{timeline:?}");
        };
        assert!(reason.contains("missing-ffmpeg"), "{reason}");
        assert!(reason.contains(".mp4"), "{reason}");

        timeline.clear();
        assert!(!timeline.is_started() && !timeline.running());
    }

    #[test]
    fn a_cancelled_timeline_keeps_its_samples_and_computes_again_on_request() {
        let mut timeline = VideoTimeline::default();
        start(&mut timeline);
        timeline.values = vec![1.0, 2.0];
        timeline.cancel();
        std::thread::sleep(Duration::from_millis(50));
        timeline.poll();
        assert_eq!(timeline.state, TimelineState::Cancelled);
        assert_eq!(timeline.values, [1.0, 2.0]);

        timeline.compute_again();
        assert!(timeline.running() && timeline.values.is_empty());
        poll_until_stopped(&mut timeline);
        assert!(
            matches!(timeline.state, TimelineState::Failed(_)),
            "{timeline:?}"
        );
    }

    #[test]
    fn the_likeness_is_one_minus_the_mean_frame_difference_once_there_are_frames() {
        let mut timeline = VideoTimeline::default();
        assert_eq!(timeline.likeness(), None);
        timeline.push(0.0);
        timeline.push(51.0);
        let ratio = |likeness: Option<Likeness>| match likeness {
            Some(Likeness::Measured { ratio, .. }) => ratio,
            other => panic!("{other:?}"),
        };
        assert_eq!(ratio(timeline.likeness()), 0.9);
        timeline.state = TimelineState::Cancelled;
        assert_eq!(ratio(timeline.likeness()), 0.9);
        timeline.state = TimelineState::Running;
        assert_eq!(timeline.likeness(), Some(Likeness::Pending));
        timeline.state = TimelineState::Failed("broken".to_string());
        assert_eq!(timeline.likeness(), None);

        start(&mut timeline);
        assert_eq!(timeline.likeness(), Some(Likeness::Pending));
        timeline.clear();
        assert_eq!(timeline.likeness(), None);
    }

    #[test]
    fn a_metric_change_computes_a_started_timeline_again() {
        let mut timeline = VideoTimeline::default();
        timeline.set_metric(PixelMetric::Ssim);
        assert!(!timeline.is_started() && !timeline.running());

        start(&mut timeline);
        poll_until_stopped(&mut timeline);
        timeline.set_metric(PixelMetric::Ssim);
        assert!(!timeline.running());

        timeline.set_metric(PixelMetric::Exact);
        assert!(timeline.running());
        poll_until_stopped(&mut timeline);
    }

    fn finish_with(timeline: &mut VideoTimeline, values: &[f32]) {
        poll_until_stopped(timeline);
        timeline.state = TimelineState::Done;
        for &value in values {
            timeline.push(value);
        }
    }

    fn ratio(timeline: &VideoTimeline) -> f64 {
        match timeline.likeness() {
            Some(Likeness::Measured { ratio, .. }) => ratio,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn switching_back_to_a_finished_metric_restores_its_values_without_computing() {
        let mut timeline = VideoTimeline::default();
        start(&mut timeline);
        finish_with(&mut timeline, &[0.0, 51.0]);

        timeline.set_metric(PixelMetric::Ssim);
        assert!(timeline.running() && timeline.values.is_empty());
        timeline.cancel();
        timeline.compute_again();
        finish_with(&mut timeline, &[102.0]);

        timeline.set_metric(PixelMetric::Exact);
        assert!(!timeline.running());
        assert_eq!(timeline.state, TimelineState::Done);
        assert_eq!(timeline.values, [0.0, 51.0]);
        assert_eq!(ratio(&timeline), 0.9);

        timeline.set_metric(PixelMetric::Ssim);
        assert!(!timeline.running());
        assert_eq!(timeline.values, [102.0]);
        assert_eq!(ratio(&timeline), 0.6);
    }

    #[test]
    fn a_metric_left_before_it_finished_computes_again() {
        let mut timeline = VideoTimeline::default();
        start(&mut timeline);
        finish_with(&mut timeline, &[51.0]);

        timeline.set_metric(PixelMetric::Ssim);
        assert!(timeline.running());
        timeline.set_metric(PixelMetric::Exact);
        assert!(!timeline.running());
        assert_eq!(timeline.values, [51.0]);

        timeline.set_metric(PixelMetric::Ssim);
        assert!(timeline.running());
        poll_until_stopped(&mut timeline);
        assert!(matches!(timeline.state, TimelineState::Failed(_)));
        timeline.set_metric(PixelMetric::Exact);
        timeline.set_metric(PixelMetric::Ssim);
        assert!(timeline.running());
    }

    #[test]
    fn a_new_pair_drops_the_finished_metrics() {
        let mut timeline = VideoTimeline::default();
        start(&mut timeline);
        finish_with(&mut timeline, &[51.0]);
        timeline.set_metric(PixelMetric::Ssim);
        finish_with(&mut timeline, &[102.0]);

        timeline.clear();
        start(&mut timeline);
        finish_with(&mut timeline, &[10.0]);
        timeline.set_metric(PixelMetric::Exact);
        assert!(timeline.running() && timeline.values.is_empty());
    }
}
