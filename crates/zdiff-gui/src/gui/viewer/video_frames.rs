//! Video frame compare: RGBA frames per side decoded by seeking ffmpeg, the sides matched by
//! timestamp (each side's nearest frame), drawn with the image viewer's rendering and modes.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

use eframe::egui::{self, Align2, TextStyle, pos2, vec2};
use zcommon::{
    ffmpeg::{
        FfmpegError, FfmpegPaths, MediaInfo, StreamKind, frames_at_args, probe, run_ffmpeg,
        scaled_frames_args,
    },
    job::{Job, JobCtx},
};

use super::{
    hex::is_same_side,
    image::{self, DecodedImage, ImageDiffProcessor, ImageSettings, ImageSide},
    video_cache::{
        FRAME_CACHE_BYTES, FrameCache, PREVIEW_BYTES, PreviewStrip, Scrub, kept_frames, next_run,
    },
    video_timeline::{self, VideoTimeline, split_frames},
};
use crate::file::LoadedFile;

/// The first video stream, the one `frames_at_args` maps (0:v:0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoTrack {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub frame_count: u64,
}

impl VideoTrack {
    pub fn from_probe(info: &MediaInfo) -> Result<Self, String> {
        let stream = info
            .streams
            .iter()
            .find(|s| s.kind == StreamKind::Video)
            .ok_or_else(|| "no video stream".to_string())?;
        let (Some(width), Some(height)) = (stream.width, stream.height) else {
            return Err("the video stream has no frame size".to_string());
        };
        if width == 0 || height == 0 {
            return Err(format!("the video stream is {width} x {height}"));
        }
        let fps = stream
            .frame_rate
            .ok_or_else(|| "the video stream has no frame rate".to_string())?;
        let frame_count = stream
            .frame_count
            .filter(|&count| count > 0)
            .or_else(|| {
                let duration = stream.duration_secs.or(info.duration_secs)?;
                Some((duration * fps).round() as u64).filter(|&count| count > 0)
            })
            .ok_or_else(|| "the video stream has no frame count or duration".to_string())?;
        Ok(Self {
            width,
            height,
            fps,
            frame_count,
        })
    }

    pub fn nearest_frame(&self, time_secs: f64) -> u64 {
        let index = (time_secs * self.fps).round();
        if index > 0.0 {
            (index as u64).min(self.frame_count - 1)
        } else {
            0
        }
    }

    pub fn frame_time(&self, index: u64) -> f64 {
        index as f64 / self.fps
    }

    /// ffmpeg's input seek gives the first frame at or after the seek time; half a frame early
    /// absorbs the rounding of the time to microseconds.
    pub fn seek_time(&self, index: u64) -> f64 {
        ((index as f64 - 0.5) / self.fps).max(0.0)
    }

    pub fn last_frame_time(&self) -> f64 {
        self.frame_time(self.frame_count - 1)
    }
}

pub fn end_time(tracks: &[VideoTrack]) -> f64 {
    tracks
        .iter()
        .map(VideoTrack::last_frame_time)
        .fold(0.0, f64::max)
}

/// Steps on the frame grid of the side with the higher frame rate, so every frame of both sides
/// is visited; past the shorter side's end that side stays on its last frame.
pub fn step_time(tracks: &[VideoTrack], time_secs: f64, forward: bool) -> f64 {
    let Some(fps) = tracks.iter().map(|t| t.fps).reduce(f64::max) else {
        return time_secs;
    };
    let index = (time_secs * fps).round() + if forward { 1.0 } else { -1.0 };
    (index / fps).clamp(0.0, end_time(tracks))
}

pub fn format_time(secs: f64) -> String {
    let millis = (secs.max(0.0) * 1000.0).round() as u64;
    let (hours, minutes) = (millis / 3_600_000, millis / 60_000 % 60);
    let (seconds, millis) = (millis / 1000 % 60, millis % 1000);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}.{millis:03}")
    } else {
        format!("{minutes:02}:{seconds:02}.{millis:03}")
    }
}

fn no_frame(track: &VideoTrack, index: u64) -> String {
    format!(
        "ffmpeg returned no frame {index} at {}",
        format_time(track.frame_time(index))
    )
}

pub fn frame_image(track: &VideoTrack, index: u64, rgba: Vec<u8>) -> Result<DecodedImage, String> {
    let expected = track.width as usize * track.height as usize * 4;
    if rgba.is_empty() {
        return Err(no_frame(track, index));
    }
    if rgba.len() != expected {
        return Err(format!(
            "frame {index} is {} bytes, expected {} x {} RGBA ({expected} bytes)",
            rgba.len(),
            track.width,
            track.height
        ));
    }
    Ok(DecodedImage {
        width: track.width,
        height: track.height,
        format: "Frame",
        rgba,
    })
}

fn local_path(file: &LoadedFile) -> Result<&Path, String> {
    file.path()
        .as_path()
        .ok_or_else(|| format!("{}: video frames need a local file", file.path()))
}

fn decode_run(
    ffmpeg: &FfmpegPaths,
    input: &Path,
    track: &VideoTrack,
    run: Range<u64>,
    ctx: &JobCtx<JobResult>,
) {
    let started = Instant::now();
    let frame_len = track.width as usize * track.height as usize * 4;
    let args = frames_at_args(input, track.seek_time(run.start), run.end - run.start);
    let (mut carry, mut next, mut first, mut bad_frame) = (Vec::new(), run.start, None, None);
    let result = run_ffmpeg(&ffmpeg.ffmpeg, &args, ctx.cancel_flag(), |chunk| {
        // One allocation per frame instead of regrowing it chunk by chunk.
        carry.reserve_exact(frame_len.saturating_sub(carry.len()));
        split_frames(&mut carry, chunk, frame_len, |rgba| {
            if next == run.end || bad_frame.is_some() {
                return;
            }
            match frame_image(track, next, rgba) {
                Ok(frame) => {
                    ctx.send(JobResult::Frame(next, frame));
                    first.get_or_insert(started.elapsed());
                    next += 1;
                }
                Err(e) => bad_frame = Some(e),
            }
        })
    });
    let error = match result {
        Err(FfmpegError::Cancelled) => return,
        Err(e) => Some(format!("{}: {e}", input.display())),
        Ok(()) if next < run.end && !carry.is_empty() => Some(format!(
            "{}: ffmpeg stopped inside frame {next}",
            input.display()
        )),
        Ok(()) => bad_frame,
    };
    log::debug!(
        "Video frames {}..{next} of {} in {:?}, the first after {first:?}",
        run.start,
        input.display(),
        started.elapsed()
    );
    ctx.send(JobResult::RunDone {
        missing: next..run.end,
        error,
    });
}

fn decode_previews(
    ffmpeg: &FfmpegPaths,
    input: &Path,
    (width, height): (u32, u32),
    fps: f64,
    ctx: &JobCtx<PreviewMessage>,
) {
    let started = Instant::now();
    let frame_len = width as usize * height as usize * 4;
    let mut carry = Vec::new();
    let args = scaled_frames_args(input, width, height, fps);
    let result = run_ffmpeg(&ffmpeg.ffmpeg, &args, ctx.cancel_flag(), |chunk| {
        split_frames(&mut carry, chunk, frame_len, |sample| {
            ctx.send(PreviewMessage::Sample(sample));
        })
    });
    let result = match result {
        Err(FfmpegError::Cancelled) => return,
        Err(e) => Err(format!("{}: {e}", input.display())),
        Ok(()) => Ok(()),
    };
    log::debug!(
        "Video previews of {} in {:?}",
        input.display(),
        started.elapsed()
    );
    ctx.send(PreviewMessage::Done(result));
}

#[derive(Debug)]
enum Probe {
    Idle,
    Running,
    Done(Result<VideoTrack, String>),
}

#[derive(Debug)]
enum JobResult {
    Probe(Result<VideoTrack, String>),
    Frame(u64, DecodedImage),
    RunDone {
        missing: Range<u64>,
        error: Option<String>,
    },
}

enum PreviewMessage {
    Sample(Vec<u8>),
    Done(Result<(), String>),
}

type ShownFrame = (u64, Result<Arc<DecodedImage>, String>);

/// Probes each side when the pair changes, then shows per side the frame nearest to the shared
/// time. Runs of frames around it are decoded into a cache; previews stand in while scrubbing.
#[derive(Debug)]
pub struct VideoFrameProcessor {
    files: [Option<LoadedFile>; 2],
    ffmpeg: Option<FfmpegPaths>,
    probes: [Probe; 2],
    /// The wanted frame a side's run is decoding; the other side waits for it.
    fetching: [Option<u64>; 2],
    frames: [Option<ShownFrame>; 2],
    time_secs: f64,
    jobs: [Job<JobResult>; 2],
    runs: [Option<Range<u64>>; 2],
    caches: [FrameCache; 2],
    /// Frames a finished run didn't give, so they aren't decoded again ahead of time.
    missing: [Vec<Range<u64>>; 2],
    previews: [Option<PreviewStrip>; 2],
    preview_jobs: [Job<PreviewMessage>; 2],
    shown_previews: [Option<(usize, Arc<DecodedImage>)>; 2],
    scrub: Scrub,
    image: ImageDiffProcessor,
    timeline: VideoTimeline,
}

impl Default for VideoFrameProcessor {
    fn default() -> Self {
        Self {
            files: [None, None],
            ffmpeg: None,
            probes: [Probe::Idle, Probe::Idle],
            fetching: [None, None],
            frames: [None, None],
            time_secs: 0.0,
            jobs: [Job::new(|| {}), Job::new(|| {})],
            runs: [None, None],
            caches: [
                FrameCache::new(FRAME_CACHE_BYTES),
                FrameCache::new(FRAME_CACHE_BYTES),
            ],
            missing: [Vec::new(), Vec::new()],
            previews: [None, None],
            preview_jobs: [Job::new(|| {}), Job::new(|| {})],
            shown_previews: [None, None],
            scrub: Scrub::default(),
            image: ImageDiffProcessor::default(),
            timeline: VideoTimeline::default(),
        }
    }
}

impl VideoFrameProcessor {
    /// No-op for the pair already requested. Without ffmpeg nothing is kept.
    pub fn request(&mut self, files: [Option<LoadedFile>; 2], ffmpeg: Option<&FfmpegPaths>) {
        let files = if ffmpeg.is_some() {
            files
        } else {
            [None, None]
        };
        let ffmpeg = ffmpeg.cloned();
        if is_same_side(&self.files[0], &files[0])
            && is_same_side(&self.files[1], &files[1])
            && self.ffmpeg == ffmpeg
        {
            return;
        }
        for job in &mut self.jobs {
            job.cancel();
        }
        for job in &mut self.preview_jobs {
            job.cancel();
        }
        self.files = files;
        self.ffmpeg = ffmpeg;
        self.probes = [Probe::Idle, Probe::Idle];
        self.fetching = [None, None];
        self.frames = [None, None];
        self.time_secs = 0.0;
        self.runs = [None, None];
        for cache in &mut self.caches {
            cache.clear();
        }
        self.missing = [Vec::new(), Vec::new()];
        self.previews = [None, None];
        self.shown_previews = [None, None];
        self.scrub = Scrub::default();
        self.timeline.clear();
        // A pending side keeps its shown image; the new pair must not show the old pair's frames.
        self.image
            .set_images([ImageSide::Empty, ImageSide::Empty], usize::MAX);
        let Some(ffmpeg) = self.ffmpeg.clone() else {
            return;
        };
        for index in 0..2 {
            let Some(file) = &self.files[index] else {
                continue;
            };
            log::info!("Video probe requested: {}", file.path());
            self.probes[index] = Probe::Running;
            let path = local_path(file).map(Path::to_path_buf);
            let ffmpeg = ffmpeg.clone();
            self.spawn(index, move |cancel| {
                JobResult::Probe(path.and_then(|path| {
                    probe(&ffmpeg, &path, cancel)
                        .and_then(|info| VideoTrack::from_probe(&info))
                        .map_err(|e| format!("{}: {e}", path.display()))
                }))
            });
        }
    }

    fn spawn(&mut self, index: usize, job: impl FnOnce(&AtomicBool) -> JobResult + Send + 'static) {
        self.jobs[index].start(move |ctx| {
            ctx.send(job(ctx.cancel_flag()));
        });
    }

    /// Takes finished jobs, fetches the frames the time now needs and hands the frames to the
    /// image viewer. `max_texture_side` is the renderer's limit.
    pub fn poll(&mut self, max_texture_side: usize) {
        for index in 0..2 {
            for result in self.jobs[index].poll() {
                self.take_result(index, result);
            }
            for message in self.preview_jobs[index].poll() {
                let Some(strip) = &mut self.previews[index] else {
                    continue;
                };
                match message {
                    PreviewMessage::Sample(rgba) => strip.push(rgba),
                    PreviewMessage::Done(Ok(())) => strip.finish(),
                    // The samples so far stay; later times fetch exact frames only.
                    PreviewMessage::Done(Err(reason)) => {
                        log::warn!("Video previews failed: {reason}")
                    }
                }
            }
        }
        self.start_timeline();
        self.timeline.poll();
        self.start_previews();
        self.fetch_wanted();
        self.update_previews();
        let sides = std::array::from_fn(|index| self.image_side(index));
        self.image.set_images(sides, max_texture_side);
        self.image.poll(max_texture_side);
    }

    fn take_result(&mut self, index: usize, result: JobResult) {
        match result {
            JobResult::Probe(probed) => self.probes[index] = Probe::Done(probed),
            JobResult::Frame(frame, image) => {
                let image = Arc::new(image);
                self.caches[index].insert(frame, image.clone());
                if let Some(run) = &mut self.runs[index] {
                    run.start = frame + 1;
                }
                if self.fetching[index] == Some(frame) {
                    self.fetching[index] = None;
                    self.frames[index] = Some((frame, Ok(image)));
                }
            }
            JobResult::RunDone { missing, error } => {
                self.runs[index] = None;
                if let Some(wanted) = self.fetching[index].filter(|w| missing.contains(w)) {
                    let track = self.track(index).expect("a decoding side is probed");
                    let reason = error.clone().unwrap_or_else(|| no_frame(&track, wanted));
                    self.fetching[index] = None;
                    self.frames[index] = Some((wanted, Err(reason)));
                } else if let Some(reason) = &error {
                    log::warn!("Decoding video frames ahead failed: {reason}");
                }
                if !missing.is_empty() {
                    self.missing[index].push(missing);
                }
            }
        }
    }

    fn start_previews(&mut self) {
        let Some(ffmpeg) = &self.ffmpeg else {
            return;
        };
        for index in 0..2 {
            // After the first frame, so the previews' decode doesn't delay it.
            if self.previews[index].is_some() || self.frames[index].is_none() {
                continue;
            }
            let Some(track) = self.track(index) else {
                continue;
            };
            let strip = PreviewStrip::plan(&track, PREVIEW_BYTES);
            let (path, ffmpeg) = (self.local_input(index), ffmpeg.clone());
            let (size, fps) = ((strip.width, strip.height), strip.fps);
            self.preview_jobs[index]
                .start(move |ctx| decode_previews(&ffmpeg, &path, size, fps, ctx));
            self.previews[index] = Some(strip);
        }
    }

    fn start_timeline(&mut self) {
        let (Some(ffmpeg), [Some(left), Some(right)]) =
            (&self.ffmpeg, [self.track(0), self.track(1)])
        else {
            return;
        };
        if self.timeline.is_started() {
            return;
        }
        let inputs = self.files.each_ref().map(|file| {
            let file = file.as_ref().expect("a probed side has a file");
            local_path(file)
                .expect("a probed side is local")
                .to_path_buf()
        });
        self.timeline.start(ffmpeg, inputs, [left, right]);
    }

    fn fetch_wanted(&mut self) {
        let Some(ffmpeg) = self.ffmpeg.clone() else {
            return;
        };
        let mut needed = [None, None];
        for (index, needed) in needed.iter_mut().enumerate() {
            let Some(track) = self.track(index) else {
                continue;
            };
            let wanted = track.nearest_frame(self.time_secs);
            let run = self.run_length(index, &track);
            self.caches[index].touch(kept_frames(wanted, track.frame_count, run));
            if self.frames[index].as_ref().map(|(frame, _)| *frame) == Some(wanted) {
                continue;
            }
            if let Some(frame) = self.caches[index].get(wanted) {
                self.frames[index] = Some((wanted, Ok(frame)));
            } else if self.runs[index]
                .as_ref()
                .is_some_and(|r| r.contains(&wanted))
            {
                self.fetching[index] = Some(wanted);
            } else {
                *needed = Some(wanted);
            }
        }
        // Both sides fetch for the same time, so the pair shown and compared is one moment.
        if self.fetching.iter().any(Option::is_some) {
            return;
        }
        let scrubbing = self.scrub.active(Instant::now());
        // A drag shows previews; the exact frames are fetched once it pauses or ends.
        let previewed =
            |index: usize| needed[index].is_none() || self.preview_sample(index).is_some();
        if scrubbing && (0..2).all(previewed) {
            return;
        }
        for (index, wanted) in needed.into_iter().enumerate() {
            let (Some(wanted), Some(track)) = (wanted, self.track(index)) else {
                continue;
            };
            let end = (wanted + self.run_length(index, &track)).min(track.frame_count);
            self.fetching[index] = Some(wanted);
            self.start_run(index, &ffmpeg, wanted..end);
        }
        if scrubbing || self.fetching.iter().any(Option::is_some) {
            return;
        }
        for index in 0..2 {
            let Some(track) = self.track(index).filter(|_| self.runs[index].is_none()) else {
                continue;
            };
            let (cache, missing) = (&self.caches[index], &self.missing[index]);
            let have = |frame| cache.contains(frame) || missing.iter().any(|m| m.contains(&frame));
            let wanted = track.nearest_frame(self.time_secs);
            let run = self.run_length(index, &track);
            if let Some(run) = next_run(wanted, track.frame_count, run, have) {
                self.start_run(index, &ffmpeg, run);
            }
        }
    }

    fn start_run(&mut self, index: usize, ffmpeg: &FfmpegPaths, run: Range<u64>) {
        let track = self.track(index).expect("a decoding side is probed");
        let (path, ffmpeg) = (self.local_input(index), ffmpeg.clone());
        self.runs[index] = Some(run.clone());
        self.jobs[index].start(move |ctx| decode_run(&ffmpeg, &path, &track, run, ctx));
    }

    fn run_length(&self, index: usize, track: &VideoTrack) -> u64 {
        self.caches[index].run_length(track.width as usize * track.height as usize * 4)
    }

    fn local_input(&self, index: usize) -> PathBuf {
        let file = self.files[index]
            .as_ref()
            .expect("a probed side has a file");
        local_path(file)
            .expect("a probed side is local")
            .to_path_buf()
    }

    fn preview_sample(&self, index: usize) -> Option<usize> {
        self.previews[index].as_ref()?.sample_at(self.time_secs)
    }

    fn update_previews(&mut self) {
        for index in 0..2 {
            let (Some(track), Some(sample)) = (self.track(index), self.preview_sample(index))
            else {
                continue;
            };
            let wanted = track.nearest_frame(self.time_secs);
            let exact = self.frames[index]
                .as_ref()
                .is_some_and(|(frame, _)| *frame == wanted);
            let shown = self.shown_previews[index]
                .as_ref()
                .is_some_and(|(shown, _)| *shown == sample);
            if exact || shown {
                continue;
            }
            let strip = self.previews[index].as_ref().expect("a sample has a strip");
            let preview = strip.upscaled(sample, track.width, track.height);
            self.shown_previews[index] = Some((sample, Arc::new(preview)));
        }
    }

    fn preview(&self, index: usize) -> Option<Arc<DecodedImage>> {
        let sample = self.preview_sample(index)?;
        let (shown, preview) = self.shown_previews[index].as_ref()?;
        (*shown == sample).then(|| preview.clone())
    }

    fn exact_or_preview(&self, index: usize) -> Option<ImageSide> {
        if self.files[index].is_none() {
            return Some(ImageSide::Empty);
        }
        let wanted = match &self.probes[index] {
            Probe::Idle | Probe::Running => return None,
            Probe::Done(Err(reason)) => return Some(ImageSide::Failed(reason.clone())),
            Probe::Done(Ok(track)) => track.nearest_frame(self.time_secs),
        };
        match &self.frames[index] {
            Some((frame, Ok(image))) if *frame == wanted => Some(ImageSide::Image(image.clone())),
            Some((frame, Err(reason))) if *frame == wanted => {
                Some(ImageSide::Failed(reason.clone()))
            }
            _ => self.preview(index).map(ImageSide::Image),
        }
    }

    fn image_side(&self, index: usize) -> ImageSide {
        if self.files[index].is_none() {
            return ImageSide::Empty;
        }
        if (0..2).all(|side| self.exact_or_preview(side).is_some())
            && let Some(side) = self.exact_or_preview(index)
        {
            return side;
        }
        match (&self.probes[index], &self.frames[index]) {
            (Probe::Idle | Probe::Running, _) | (Probe::Done(Ok(_)), None) => ImageSide::Pending,
            (Probe::Done(Err(reason)), _) | (Probe::Done(Ok(_)), Some((_, Err(reason)))) => {
                ImageSide::Failed(reason.clone())
            }
            (Probe::Done(Ok(_)), Some((_, Ok(_)))) if self.fetching.iter().any(Option::is_some) => {
                ImageSide::Pending
            }
            (Probe::Done(Ok(_)), Some((_, Ok(frame)))) => ImageSide::Image(frame.clone()),
        }
    }

    fn track(&self, index: usize) -> Option<VideoTrack> {
        match &self.probes[index] {
            Probe::Done(Ok(track)) => Some(*track),
            _ => None,
        }
    }

    fn tracks(&self) -> Vec<VideoTrack> {
        (0..2).filter_map(|index| self.track(index)).collect()
    }

    /// One reason per side that can't be probed as a video.
    pub fn failures(&self) -> Vec<String> {
        self.probes
            .iter()
            .filter_map(|probe| match probe {
                Probe::Done(Err(reason)) => Some(reason.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn seek(&mut self, time_secs: f64) {
        self.set_time(time_secs.clamp(0.0, end_time(&self.tracks())));
    }

    pub fn step(&mut self, forward: bool) {
        self.set_time(step_time(&self.tracks(), self.time_secs, forward));
    }

    fn set_time(&mut self, time_secs: f64) {
        if time_secs != self.time_secs {
            self.scrub.change(Instant::now());
        }
        self.time_secs = time_secs;
    }

    fn busy(&self) -> bool {
        self.probes
            .iter()
            .any(|probe| matches!(probe, Probe::Running))
            || self.fetching.iter().any(Option::is_some)
            || self.runs.iter().any(Option::is_some)
    }

    fn readout(&self, index: usize) -> String {
        match (&self.probes[index], self.track(index)) {
            (Probe::Running, _) => "Probing...".to_string(),
            (_, Some(track)) => {
                let frame = track.nearest_frame(self.time_secs);
                format!(
                    "{}  frame {frame} / {}  {} fps",
                    format_time(track.frame_time(frame)),
                    track.frame_count - 1,
                    (track.fps * 1000.0).round() / 1000.0
                )
            }
            _ => String::new(),
        }
    }
}

/// The scrubber and per-side readouts above the image viewer, its left column `left_width` wide.
pub fn show(
    ui: &mut egui::Ui,
    processor: &mut VideoFrameProcessor,
    image_settings: &mut ImageSettings,
    left_width: f32,
    shown_sides: [usize; 2],
) {
    let scrubbing = processor.scrub.active(Instant::now());
    processor.scrub.hold(ui.input(|i| i.pointer.any_down()));
    if processor.busy() || scrubbing {
        // Jobs finish on workers and a paused drag fetches later; keep frames coming until then.
        ui.ctx().request_repaint();
    }
    let end = end_time(&processor.tracks());
    ui.horizontal(|ui| {
        if ui.button("<").on_hover_text("Previous frame").clicked() {
            processor.step(false);
        }
        if ui.button(">").on_hover_text("Next frame").clicked() {
            processor.step(true);
        }
        // A pair seeks on the timeline graph instead.
        if processor.files.iter().all(Option::is_some) {
            return;
        }
        ui.spacing_mut().slider_width = ui.available_width().max(50.0);
        let mut time = processor.time_secs;
        let slider = ui.add_enabled(
            end > 0.0,
            egui::Slider::new(&mut time, 0.0..=end).show_value(false),
        );
        if slider.changed() {
            processor.seek(time);
        }
        // A focused slider moves on the arrow keys too, doubling the frame step keys.
        if slider.has_focus() {
            slider.surrender_focus();
        }
    });
    processor.timeline.set_metric(image_settings.metric);
    if let Some(time) = video_timeline::show(ui, &mut processor.timeline, processor.time_secs, end)
    {
        processor.seek(time);
    }

    let height = ui.text_style_height(&TextStyle::Monospace) + 4.0;
    let (_, row) = ui.allocate_space(vec2(ui.available_width(), height));
    let gap = 12.0 + 2.0 * ui.spacing().item_spacing.x;
    let font = TextStyle::Monospace.resolve(ui.style());
    let color = ui.visuals().text_color();
    for (index, x) in [row.left(), row.left() + left_width + gap]
        .into_iter()
        .enumerate()
    {
        ui.painter().text(
            pos2(x, row.center().y),
            Align2::LEFT_CENTER,
            processor.readout(shown_sides[index]),
            font.clone(),
            color,
        );
    }

    image::show(
        ui,
        &mut processor.image,
        image_settings,
        left_width,
        shown_sides,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcommon::ffmpeg::StreamInfo;

    const NTSC: f64 = 30000.0 / 1001.0;

    fn track(fps: f64, frame_count: u64) -> VideoTrack {
        VideoTrack {
            width: 4,
            height: 2,
            fps,
            frame_count,
        }
    }

    fn video_stream() -> StreamInfo {
        StreamInfo {
            index: Some(0),
            kind: StreamKind::Video,
            codec_name: Some("h264".into()),
            codec_long_name: None,
            duration_secs: Some(2.0),
            bit_rate: None,
            sample_rate: None,
            channels: None,
            channel_layout: None,
            sample_fmt: None,
            width: Some(1920),
            height: Some(1080),
            pix_fmt: Some("yuv420p".into()),
            frame_rate: Some(25.0),
            frame_count: Some(50),
        }
    }

    #[test]
    fn two_frame_rates_map_timestamps_to_their_nearest_frames() {
        let (pal, ntsc) = (track(25.0, 250), track(NTSC, 300));
        let cases = [
            (0.0, 0, 0),
            (0.03, 1, 1),
            (0.05, 1, 1),
            (0.07, 2, 2),
            (1.0, 25, 30),
            (1.001, 25, 30),
            (5.0, 125, 150),
            (8.0, 200, 240),
            (9.95, 249, 298),
            (10.0, 249, 299),
            (60.0, 249, 299),
            (-1.0, 0, 0),
        ];
        for (time, pal_index, ntsc_index) in cases {
            assert_eq!(
                (pal.nearest_frame(time), ntsc.nearest_frame(time)),
                (pal_index, ntsc_index),
                "{time}"
            );
        }
    }

    #[test]
    fn a_frame_maps_back_to_itself_through_its_time() {
        for fps in [24.0, 25.0, NTSC, 60.0] {
            let track = track(fps, 1000);
            for index in [0, 1, 7, 333, 999] {
                assert_eq!(track.nearest_frame(track.frame_time(index)), index, "{fps}");
            }
        }
    }

    #[test]
    fn seeking_lands_half_a_frame_early_and_never_before_zero() {
        let track = track(25.0, 250);
        assert_eq!(track.seek_time(0), 0.0);
        assert_eq!(track.seek_time(1), 0.02);
        assert_eq!(track.seek_time(10), 0.38);
        assert_eq!(track.last_frame_time(), 9.96);
    }

    #[test]
    fn stepping_visits_every_frame_of_both_sides_and_stops_at_the_last_frame() {
        let (slow, fast) = (track(24.0, 24), track(60.0, 30));
        let tracks = [slow, fast];
        let mut time = 0.0;
        let mut visited = vec![(0, 0)];
        loop {
            let next = step_time(&tracks, time, true);
            if next == time {
                break;
            }
            time = next;
            visited.push((slow.nearest_frame(time), fast.nearest_frame(time)));
        }
        assert_eq!(time, slow.last_frame_time());
        assert_eq!(visited.len(), 59);
        assert_eq!(visited[1..4], [(0, 1), (1, 2), (1, 3)]);
        assert_eq!(visited[visited.len() - 1], (23, 29));
        for index in 0..24 {
            assert!(visited.iter().any(|v| v.0 == index), "slow {index}");
        }
        for index in 0..30 {
            assert!(visited.iter().any(|v| v.1 == index), "fast {index}");
        }
        assert_eq!(step_time(&tracks, 0.0, false), 0.0);
        assert_eq!(step_time(&tracks, 0.5, false), 29.0 / 60.0);
        assert_eq!(step_time(&[], 1.0, true), 1.0);
    }

    #[test]
    fn the_readout_time_has_milliseconds_and_hours_only_when_needed() {
        assert_eq!(format_time(0.0), "00:00.000");
        assert_eq!(format_time(1.04), "00:01.040");
        assert_eq!(format_time(61.5), "01:01.500");
        assert_eq!(format_time(3725.0), "1:02:05.000");
    }

    #[test]
    fn the_first_video_stream_gives_size_rate_and_count() {
        let info = MediaInfo {
            duration_secs: Some(2.0),
            streams: vec![
                StreamInfo {
                    kind: StreamKind::Audio,
                    ..video_stream()
                },
                video_stream(),
                StreamInfo {
                    frame_rate: Some(60.0),
                    ..video_stream()
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            VideoTrack::from_probe(&info),
            Ok(VideoTrack {
                width: 1920,
                height: 1080,
                fps: 25.0,
                frame_count: 50,
            })
        );
    }

    #[test]
    fn a_missing_frame_count_comes_from_the_stream_then_the_container_duration() {
        let stream = StreamInfo {
            frame_count: None,
            ..video_stream()
        };
        let info = |stream: StreamInfo, duration_secs| MediaInfo {
            duration_secs,
            streams: vec![stream],
            ..Default::default()
        };
        let count = |info: MediaInfo| VideoTrack::from_probe(&info).map(|t| t.frame_count);
        assert_eq!(count(info(stream.clone(), Some(9.0))), Ok(50));
        let no_duration = StreamInfo {
            duration_secs: None,
            ..stream
        };
        assert_eq!(count(info(no_duration.clone(), Some(3.0))), Ok(75));
        assert!(count(info(no_duration, None)).is_err());
    }

    #[test]
    fn a_stream_without_video_size_or_rate_is_an_error() {
        let info = |stream: StreamInfo| MediaInfo {
            streams: vec![stream],
            ..Default::default()
        };
        let error = |info: MediaInfo| VideoTrack::from_probe(&info).unwrap_err();
        assert_eq!(
            error(info(StreamInfo {
                kind: StreamKind::Audio,
                ..video_stream()
            })),
            "no video stream"
        );
        assert!(
            !error(info(StreamInfo {
                frame_rate: None,
                ..video_stream()
            }))
            .is_empty()
        );
        assert!(
            !error(info(StreamInfo {
                width: Some(0),
                ..video_stream()
            }))
            .is_empty()
        );
    }

    #[test]
    fn a_frame_must_fill_the_probed_size() {
        let track = track(25.0, 250);
        let frame = frame_image(&track, 3, vec![7; 4 * 2 * 4]).unwrap();
        assert_eq!((frame.width, frame.height, frame.rgba.len()), (4, 2, 32));
        assert!(frame_image(&track, 3, vec![7; 31]).is_err());
        assert!(frame_image(&track, 249, Vec::new()).is_err());
    }

    #[test]
    fn a_new_pair_drops_the_old_pairs_frames() {
        let file = |name: &str| {
            Some(LoadedFile::Binary(Arc::new(crate::file::BinaryFile {
                path: zdiff::universal_path::UniversalPath::from(name),
                bytes: vec![0; 4],
            })))
        };
        let ffmpeg = FfmpegPaths {
            ffmpeg: "missing-ffmpeg".into(),
            ffprobe: "missing-ffprobe".into(),
        };
        let mut processor = VideoFrameProcessor::default();
        let frame = Arc::new(frame_image(&track(25.0, 1), 0, vec![7; 32]).unwrap());
        processor.image.set_images(
            [ImageSide::Image(frame.clone()), ImageSide::Image(frame)],
            usize::MAX,
        );
        assert_eq!(processor.image.status_text().0, "4 x 2");

        processor.request([file("a.mp4"), file("b.mp4")], Some(&ffmpeg));
        assert_eq!(processor.image.status_text(), (String::new(), false));
    }

    #[test]
    fn a_new_pair_drops_the_old_pairs_timeline() {
        let file = |name: &str| {
            Some(LoadedFile::Binary(Arc::new(crate::file::BinaryFile {
                path: zdiff::universal_path::UniversalPath::from(name),
                bytes: vec![0; 4],
            })))
        };
        let ffmpeg = FfmpegPaths {
            ffmpeg: "missing-ffmpeg".into(),
            ffprobe: "missing-ffprobe".into(),
        };
        let mut processor = VideoFrameProcessor::default();
        let tracks = [track(25.0, 25), track(25.0, 25)];
        processor
            .timeline
            .start(&ffmpeg, ["a.mp4".into(), "b.mp4".into()], tracks);
        assert!(processor.timeline.is_started());

        processor.request([file("c.mp4"), file("d.mp4")], Some(&ffmpeg));
        assert!(!processor.timeline.is_started());
    }

    fn probed_pair(ffmpeg: Option<FfmpegPaths>) -> VideoFrameProcessor {
        let file = |name: &str| {
            Some(LoadedFile::Binary(Arc::new(crate::file::BinaryFile {
                path: zdiff::universal_path::UniversalPath::from(name),
                bytes: vec![0; 4],
            })))
        };
        let frame = |index| {
            let track = track(25.0, 25);
            (
                index,
                Ok(Arc::new(frame_image(&track, index, vec![7; 32]).unwrap())),
            )
        };
        VideoFrameProcessor {
            files: [file("a.mp4"), file("b.mp4")],
            ffmpeg,
            probes: [
                Probe::Done(Ok(track(25.0, 25))),
                Probe::Done(Ok(track(25.0, 25))),
            ],
            frames: [Some(frame(0)), Some(frame(0))],
            ..VideoFrameProcessor::default()
        }
    }

    #[test]
    fn a_side_keeps_its_shown_frame_until_the_other_sides_fetch_lands() {
        let mut processor = probed_pair(None);
        processor.frames[0].as_mut().unwrap().0 = 1;
        processor.fetching[1] = Some(1);
        assert!(matches!(processor.image_side(0), ImageSide::Pending));
        assert!(matches!(processor.image_side(1), ImageSide::Pending));

        processor.fetching[1] = None;
        assert!(matches!(processor.image_side(0), ImageSide::Image(_)));
        assert!(matches!(processor.image_side(1), ImageSide::Image(_)));
    }

    #[test]
    fn both_sides_fetch_the_next_time_together() {
        let ffmpeg = FfmpegPaths {
            ffmpeg: "missing-ffmpeg".into(),
            ffprobe: "missing-ffprobe".into(),
        };
        let mut processor = probed_pair(Some(ffmpeg));
        processor.fetching[1] = Some(0);
        processor.seek(track(25.0, 25).frame_time(1));
        processor.fetch_wanted();
        assert_eq!(processor.fetching, [None, Some(0)]);

        processor.fetching[1] = None;
        processor.fetch_wanted();
        assert_eq!(processor.fetching, [Some(1), Some(1)]);
    }

    fn missing_ffmpeg() -> FfmpegPaths {
        FfmpegPaths {
            ffmpeg: "missing-ffmpeg".into(),
            ffprobe: "missing-ffprobe".into(),
        }
    }

    fn cached_frame(processor: &mut VideoFrameProcessor, index: u64) -> Arc<DecodedImage> {
        let frame = Arc::new(frame_image(&track(25.0, 25), index, vec![3; 32]).unwrap());
        for cache in &mut processor.caches {
            cache.insert(index, frame.clone());
        }
        frame
    }

    fn with_previews(processor: &mut VideoFrameProcessor) {
        for strip in &mut processor.previews {
            let mut previews = PreviewStrip::plan(&track(25.0, 25), PREVIEW_BYTES);
            for _ in 0..25 {
                previews.push(vec![9; 32]);
            }
            *strip = Some(previews);
        }
    }

    #[test]
    fn a_cached_frame_shows_at_once_and_the_frames_ahead_decode_next() {
        let mut processor = probed_pair(Some(missing_ffmpeg()));
        let frame = cached_frame(&mut processor, 3);
        processor.seek(track(25.0, 25).frame_time(3));
        processor.fetch_wanted();
        assert_eq!(processor.fetching, [None, None]);
        assert_eq!(processor.runs, [Some(4..20), Some(4..20)]);
        let ImageSide::Image(shown) = processor.image_side(0) else {
            panic!("not shown");
        };
        assert!(Arc::ptr_eq(&shown, &frame));
    }

    #[test]
    fn a_run_on_its_way_to_the_wanted_frame_is_waited_for() {
        let mut processor = probed_pair(Some(missing_ffmpeg()));
        processor.runs = [Some(1..17), Some(1..17)];
        processor.seek(track(25.0, 25).frame_time(5));
        processor.fetch_wanted();
        assert_eq!(processor.fetching, [Some(5), Some(5)]);
        assert_eq!(processor.runs, [Some(1..17), Some(1..17)]);
    }

    #[test]
    fn a_run_gives_the_wanted_frame_or_the_reason_it_could_not() {
        let mut processor = probed_pair(None);
        processor.fetching = [Some(3), Some(3)];
        processor.runs = [Some(3..8), Some(3..8)];
        let frame = frame_image(&track(25.0, 25), 3, vec![3; 32]).unwrap();
        processor.take_result(0, JobResult::Frame(3, frame));
        assert_eq!(
            (processor.fetching[0], &processor.runs[0]),
            (None, &Some(4..8))
        );
        assert!(processor.caches[0].contains(3));
        assert!(matches!(&processor.frames[0], Some((3, Ok(_)))));

        let done = JobResult::RunDone {
            missing: 3..8,
            error: None,
        };
        processor.take_result(1, done);
        assert_eq!((processor.fetching[1], &processor.runs[1]), (None, &None));
        assert_eq!(processor.missing[1].first(), Some(&(3..8)));
        assert_eq!(processor.missing[1].len(), 1);
        let Some((3, Err(reason))) = &processor.frames[1] else {
            panic!("{:?}", processor.frames[1]);
        };
        assert_eq!(reason, "ffmpeg returned no frame 3 at 00:00.120");
    }

    #[test]
    fn scrubbing_shows_previews_and_fetches_the_exact_frames_once_released() {
        let mut processor = probed_pair(Some(missing_ffmpeg()));
        with_previews(&mut processor);
        processor.scrub.hold(true);
        processor.seek(track(25.0, 25).frame_time(7));
        processor
            .scrub
            .change(Instant::now() + std::time::Duration::from_secs(3600));
        processor.fetch_wanted();
        processor.update_previews();
        assert_eq!(
            (processor.fetching, &processor.runs),
            ([None, None], &[None, None])
        );
        let ImageSide::Image(preview) = processor.image_side(1) else {
            panic!("no preview");
        };
        assert_eq!(
            (preview.format, preview.width, preview.height),
            ("Preview", 4, 2)
        );

        processor.scrub.hold(false);
        processor.fetch_wanted();
        assert_eq!(processor.fetching, [Some(7), Some(7)]);
        assert!(matches!(processor.image_side(0), ImageSide::Image(i) if i.format == "Preview"));
    }

    #[test]
    fn a_new_pair_drops_the_old_pairs_cached_frames_and_previews() {
        let mut processor = probed_pair(Some(missing_ffmpeg()));
        cached_frame(&mut processor, 3);
        with_previews(&mut processor);
        let file = |name: &str| {
            Some(LoadedFile::Binary(Arc::new(crate::file::BinaryFile {
                path: zdiff::universal_path::UniversalPath::from(name),
                bytes: vec![0; 4],
            })))
        };
        processor.request([file("c.mp4"), file("d.mp4")], Some(&missing_ffmpeg()));
        assert!(processor.caches.iter().all(|cache| !cache.contains(3)));
        assert!(processor.previews.iter().all(Option::is_none));
    }
}
