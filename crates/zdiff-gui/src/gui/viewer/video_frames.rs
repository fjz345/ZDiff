//! Video frame compare: one RGBA frame per side fetched by seeking ffmpeg, the sides matched by
//! timestamp (each side's nearest frame), drawn with the image viewer's rendering and modes.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

use eframe::egui::{self, Align2, TextStyle, pos2, vec2};
use zcommon::ffmpeg::{FfmpegPaths, MediaInfo, StreamKind, frame_at_args, probe, run_ffmpeg};

use super::{
    hex::is_same_side,
    image::{self, DecodedImage, ImageDiffProcessor, ImageSettings, ImageSide},
    video_timeline::{self, VideoTimeline},
};
use crate::file::LoadedFile;

/// The first video stream, the one `frame_at_args` maps (0:v:0).
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

pub fn frame_image(track: &VideoTrack, index: u64, rgba: Vec<u8>) -> Result<DecodedImage, String> {
    let expected = track.width as usize * track.height as usize * 4;
    if rgba.is_empty() {
        return Err(format!(
            "ffmpeg returned no frame {index} at {}",
            format_time(track.frame_time(index))
        ));
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

fn fetch_frame(
    ffmpeg: &FfmpegPaths,
    input: &Path,
    track: &VideoTrack,
    index: u64,
    cancel: &AtomicBool,
) -> Result<DecodedImage, String> {
    let mut rgba = Vec::with_capacity(track.width as usize * track.height as usize * 4);
    let args = frame_at_args(input, track.seek_time(index));
    run_ffmpeg(&ffmpeg.ffmpeg, &args, cancel, |chunk| {
        rgba.extend_from_slice(chunk)
    })
    .map_err(|e| format!("{}: {e}", input.display()))?;
    frame_image(track, index, rgba)
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
    Frame(u64, Result<DecodedImage, String>),
}

type JobMessage = (u64, usize, JobResult);
type ShownFrame = (u64, Result<Arc<DecodedImage>, String>);

/// Probes each side when the pair changes, then keeps one frame per side: the nearest to the
/// shared time. Each side runs at most one fetch, so scrubbing fetches only the latest frame.
#[derive(Debug)]
pub struct VideoFrameProcessor {
    files: [Option<LoadedFile>; 2],
    ffmpeg: Option<FfmpegPaths>,
    generation: u64,
    cancel: Arc<AtomicBool>,
    probes: [Probe; 2],
    fetching: [Option<u64>; 2],
    frames: [Option<ShownFrame>; 2],
    time_secs: f64,
    channel: (mpsc::Sender<JobMessage>, mpsc::Receiver<JobMessage>),
    image: ImageDiffProcessor,
    timeline: VideoTimeline,
}

impl Default for VideoFrameProcessor {
    fn default() -> Self {
        Self {
            files: [None, None],
            ffmpeg: None,
            generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            probes: [Probe::Idle, Probe::Idle],
            fetching: [None, None],
            frames: [None, None],
            time_secs: 0.0,
            channel: mpsc::channel(),
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
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.generation += 1;
        self.files = files;
        self.ffmpeg = ffmpeg;
        self.probes = [Probe::Idle, Probe::Idle];
        self.fetching = [None, None];
        self.frames = [None, None];
        self.time_secs = 0.0;
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

    fn spawn(&self, index: usize, job: impl FnOnce(&AtomicBool) -> JobResult + Send + 'static) {
        let (tx, generation, cancel) =
            (self.channel.0.clone(), self.generation, self.cancel.clone());
        std::thread::spawn(move || {
            let _ = tx.send((generation, index, job(&cancel)));
        });
    }

    /// Takes finished jobs, fetches the frames the time now needs and hands the frames to the
    /// image viewer. `max_texture_side` is the renderer's limit.
    pub fn poll(&mut self, max_texture_side: usize) {
        while let Ok((generation, index, result)) = self.channel.1.try_recv() {
            if generation != self.generation {
                continue;
            }
            match result {
                JobResult::Probe(probed) => self.probes[index] = Probe::Done(probed),
                JobResult::Frame(frame, fetched) => {
                    self.fetching[index] = None;
                    self.frames[index] = Some((frame, fetched.map(Arc::new)));
                }
            }
        }
        self.start_timeline();
        self.timeline.poll();
        self.fetch_wanted();
        let sides = std::array::from_fn(|index| self.image_side(index));
        self.image.set_images(sides, max_texture_side);
        self.image.poll(max_texture_side);
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
        let Some(ffmpeg) = &self.ffmpeg else {
            return;
        };
        for index in 0..2 {
            let (Probe::Done(Ok(track)), None) = (&self.probes[index], self.fetching[index]) else {
                continue;
            };
            let wanted = track.nearest_frame(self.time_secs);
            if self.frames[index].as_ref().map(|(frame, _)| *frame) == Some(wanted) {
                continue;
            }
            let file = self.files[index]
                .as_ref()
                .expect("a probed side has a file");
            let path = local_path(file)
                .expect("a probed side is local")
                .to_path_buf();
            let (ffmpeg, track) = (ffmpeg.clone(), *track);
            self.fetching[index] = Some(wanted);
            self.spawn(index, move |cancel| {
                JobResult::Frame(wanted, fetch_frame(&ffmpeg, &path, &track, wanted, cancel))
            });
        }
    }

    fn image_side(&self, index: usize) -> ImageSide {
        if self.files[index].is_none() {
            return ImageSide::Empty;
        }
        match (&self.probes[index], &self.frames[index]) {
            (Probe::Idle | Probe::Running, _) | (Probe::Done(Ok(_)), None) => ImageSide::Pending,
            (Probe::Done(Err(reason)), _) | (Probe::Done(Ok(_)), Some((_, Err(reason)))) => {
                ImageSide::Failed(reason.clone())
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
        self.time_secs = time_secs.clamp(0.0, end_time(&self.tracks()));
    }

    pub fn step(&mut self, forward: bool) {
        self.time_secs = step_time(&self.tracks(), self.time_secs, forward);
    }

    fn busy(&self) -> bool {
        self.probes
            .iter()
            .any(|probe| matches!(probe, Probe::Running))
            || self.fetching.iter().any(Option::is_some)
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
) {
    if processor.busy() {
        // Jobs finish on workers; keep frames coming until they are polled.
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
            processor.readout(index),
            font.clone(),
            color,
        );
    }

    image::show(ui, &mut processor.image, image_settings, left_width);
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
}
