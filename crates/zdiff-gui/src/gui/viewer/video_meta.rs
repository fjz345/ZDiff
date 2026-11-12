//! Video metadata table (ffprobe fields of both sides with differing values marked) and the
//! video viewer's Frames / Audio / Metadata tabs.

use std::path::Path;

use eframe::egui;
use zcommon::{
    ffmpeg::{FfmpegPaths, MediaInfo, StreamKind, probe},
    job::Job,
};

use super::{
    audio::{self, AudioDiffProcessor, AudioSettings},
    audio_meta::MetaRow,
    hex::is_same_side,
    image::ImageSettings,
    video_frames::{self, VideoFrameProcessor},
};
use crate::file::LoadedFile;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoMeta {
    pub container: Option<String>,
    pub codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<f64>,
    pub duration_secs: Option<f64>,
    pub bit_rate: Option<u64>,
    pub pixel_format: Option<String>,
    pub audio_streams: Vec<String>,
}

impl VideoMeta {
    /// The first video stream, the one the frames come from. Duration and bitrate are the
    /// container's (the stream's when it has none), so they compare alike across containers.
    pub fn from_probe(info: &MediaInfo) -> Result<Self, String> {
        let video = info
            .streams
            .iter()
            .find(|s| s.kind == StreamKind::Video)
            .ok_or_else(|| "no video stream".to_string())?;
        let audio_streams = info
            .streams
            .iter()
            .filter(|s| s.kind == StreamKind::Audio)
            .map(|s| {
                let parts: Vec<String> = [
                    s.codec_name.clone(),
                    s.sample_rate.map(|r| format!("{r} Hz")),
                    s.channels.map(|c| match &s.channel_layout {
                        Some(layout) => format!("{c} ch ({layout})"),
                        None => format!("{c} ch"),
                    }),
                ]
                .into_iter()
                .flatten()
                .collect();
                if parts.is_empty() {
                    "-".to_string()
                } else {
                    parts.join(", ")
                }
            })
            .collect();
        Ok(VideoMeta {
            container: info
                .format_long_name
                .clone()
                .or_else(|| info.format_name.clone()),
            codec: video.codec_name.clone(),
            width: video.width,
            height: video.height,
            frame_rate: video.frame_rate,
            duration_secs: info.duration_secs.or(video.duration_secs),
            bit_rate: info.bit_rate.or(video.bit_rate),
            pixel_format: video.pix_fmt.clone(),
            audio_streams,
        })
    }
}

fn shown<T>(value: Option<T>, format: impl FnOnce(T) -> String) -> String {
    value.map(format).unwrap_or_else(|| "-".to_string())
}

type Field = (&'static str, fn(&VideoMeta) -> String);

/// Compares the displayed values, like the audio table.
pub fn video_meta_rows(left: &VideoMeta, right: &VideoMeta) -> Vec<MetaRow> {
    let fields: [Field; 8] = [
        ("Container", |m| {
            shown(m.container.as_deref(), str::to_string)
        }),
        ("Codec", |m| shown(m.codec.as_deref(), str::to_string)),
        ("Resolution", |m| match (m.width, m.height) {
            (Some(w), Some(h)) => format!("{w}x{h}"),
            _ => "-".to_string(),
        }),
        ("Frame rate", |m| {
            shown(m.frame_rate, |r| format!("{r:.3} fps"))
        }),
        ("Duration", |m| {
            shown(m.duration_secs, |d| format!("{d:.3} s"))
        }),
        ("Bitrate", |m| {
            shown(m.bit_rate, |b| format!("{:.0} kb/s", b as f64 / 1000.0))
        }),
        ("Pixel format", |m| {
            shown(m.pixel_format.as_deref(), str::to_string)
        }),
        ("Audio streams", |m| {
            if m.audio_streams.is_empty() {
                "none".to_string()
            } else {
                m.audio_streams.join("\n")
            }
        }),
    ];
    fields
        .into_iter()
        .map(|(label, show)| {
            let values = [show(left), show(right)];
            MetaRow {
                label,
                differs: values[0] != values[1],
                values,
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
enum AudioTab {
    Probing,
    NoAudio,
    Viewer { notes: Vec<String> },
}

/// `None` is a side still being probed.
fn audio_tab(has_audio: [Option<bool>; 2]) -> AudioTab {
    let [Some(left), Some(right)] = has_audio else {
        return AudioTab::Probing;
    };
    if !left && !right {
        return AudioTab::NoAudio;
    }
    let notes = [left, right]
        .iter()
        .zip(SIDE_NAMES)
        .filter(|(has, _)| !**has)
        .map(|(_, side)| format!("{side}: no audio track"))
        .collect();
    AudioTab::Viewer { notes }
}

const SIDE_NAMES: [&str; 2] = ["Left", "Right"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoTab {
    #[default]
    Frames,
    Audio,
    Metadata,
}

type Probed = Result<VideoMeta, String>;

/// Probes both sides when the pair changes; a pair change cancels running probes and drops
/// their results.
#[derive(Debug)]
pub struct VideoMetaProcessor {
    pub tab: VideoTab,
    files: [Option<LoadedFile>; 2],
    ffmpeg: Option<FfmpegPaths>,
    probes: [Option<Probed>; 2],
    jobs: [Job<Probed>; 2],
}

impl Default for VideoMetaProcessor {
    fn default() -> Self {
        Self {
            tab: VideoTab::default(),
            files: [None, None],
            ffmpeg: None,
            probes: [None, None],
            jobs: [Job::new(|| {}).on_panic(Err), Job::new(|| {}).on_panic(Err)],
        }
    }
}

impl VideoMetaProcessor {
    /// No-op for the pair already requested.
    pub fn request(&mut self, pair: Option<([LoadedFile; 2], &FfmpegPaths)>) {
        let (files, ffmpeg) = match pair {
            Some(([a, b], ffmpeg)) => ([Some(a), Some(b)], Some(ffmpeg.clone())),
            None => ([None, None], None),
        };
        if is_same_side(&self.files[0], &files[0])
            && is_same_side(&self.files[1], &files[1])
            && self.ffmpeg == ffmpeg
        {
            return;
        }
        self.files = files;
        self.ffmpeg = ffmpeg;
        self.probes = [None, None];
        for job in &mut self.jobs {
            job.cancel();
        }
        let Some(ffmpeg) = &self.ffmpeg else {
            return;
        };
        for index in 0..2 {
            let file = self.files[index].as_ref().expect("both sides requested");
            let path = file
                .path()
                .as_path()
                .map(Path::to_path_buf)
                .ok_or_else(|| format!("{}: metadata needs a local file", file.path()));
            let ffmpeg = ffmpeg.clone();
            self.jobs[index].start(move |ctx| {
                let probed = path
                    .and_then(|path| probe(&ffmpeg, &path, ctx.cancel_flag()))
                    .and_then(|info| VideoMeta::from_probe(&info));
                ctx.send(probed);
            });
        }
    }

    pub fn poll(&mut self) {
        for (probe, job) in self.probes.iter_mut().zip(&mut self.jobs) {
            if let Some(probed) = job.poll().pop() {
                *probe = Some(probed);
            }
        }
    }

    fn probing(&self) -> bool {
        self.ffmpeg.is_some() && self.probes.iter().any(Option::is_none)
    }

    /// A failed probe counts as having audio, so the decoder tries and reports its own reason.
    fn has_audio(&self) -> [Option<bool>; 2] {
        self.probes.each_ref().map(|probe| {
            probe.as_ref().map(|probed| match probed {
                Ok(meta) => !meta.audio_streams.is_empty(),
                Err(_) => true,
            })
        })
    }
}

/// The tab bar, then the selected tab. `audio` is the pair's audio viewer on the Audio tab.
pub fn show(
    ui: &mut egui::Ui,
    processor: &mut VideoMetaProcessor,
    frames: &mut VideoFrameProcessor,
    image_settings: &mut ImageSettings,
    left_width: f32,
    shown_sides: [usize; 2],
    audio: Option<(&mut AudioDiffProcessor, &mut AudioSettings)>,
) {
    let _scope = zcommon_ui::frame_stats::scope("Video view");
    processor.poll();
    if processor.probing() {
        ui.ctx().request_repaint();
    }
    ui.horizontal(|ui| {
        for (tab, label) in [
            (VideoTab::Frames, "Frames"),
            (VideoTab::Audio, "Audio"),
            (VideoTab::Metadata, "Metadata"),
        ] {
            ui.selectable_value(&mut processor.tab, tab, label);
        }
    });
    match processor.tab {
        VideoTab::Frames => video_frames::show(ui, frames, image_settings, left_width, shown_sides),
        VideoTab::Audio => match audio_tab(processor.has_audio()) {
            AudioTab::Probing => {
                ui.label("Probing...");
            }
            AudioTab::NoAudio => {
                ui.label("No audio track");
            }
            AudioTab::Viewer { notes } => {
                for note in notes {
                    ui.label(note);
                }
                if let Some((processor, settings)) = audio {
                    audio::show(ui, processor, settings);
                }
            }
        },
        VideoTab::Metadata => show_metadata(ui, processor),
    }
}

fn show_metadata(ui: &mut egui::Ui, processor: &VideoMetaProcessor) {
    for (index, side) in SIDE_NAMES.iter().enumerate() {
        match &processor.probes[index] {
            None if processor.ffmpeg.is_some() => {
                ui.label(format!("{side}: probing..."));
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().warn_fg_color, format!("{side}: {e}"));
            }
            _ => {}
        }
    }
    let [Some(Ok(left)), Some(Ok(right))] = &processor.probes else {
        return;
    };
    let marked = ui.visuals().warn_fg_color;
    egui::Grid::new("video_meta_grid")
        .num_columns(3)
        .striped(true)
        .show(ui, |ui| {
            ui.label("");
            ui.strong("Left");
            ui.strong("Right");
            ui.end_row();
            for row in video_meta_rows(left, right) {
                ui.label(row.label);
                for value in row.values {
                    if row.differs {
                        ui.colored_label(marked, value);
                    } else {
                        ui.label(value);
                    }
                }
                ui.end_row();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcommon::ffmpeg::StreamInfo;

    fn stream(kind: StreamKind) -> StreamInfo {
        StreamInfo {
            index: None,
            kind,
            codec_name: None,
            codec_long_name: None,
            duration_secs: None,
            bit_rate: None,
            sample_rate: None,
            channels: None,
            channel_layout: None,
            sample_fmt: None,
            width: None,
            height: None,
            pix_fmt: None,
            frame_rate: None,
            frame_count: None,
        }
    }

    fn mp4() -> VideoMeta {
        VideoMeta {
            container: Some("QuickTime / MOV".to_string()),
            codec: Some("h264".to_string()),
            width: Some(1920),
            height: Some(1080),
            frame_rate: Some(30000.0 / 1001.0),
            duration_secs: Some(10.0),
            bit_rate: Some(5_000_000),
            pixel_format: Some("yuv420p".to_string()),
            audio_streams: vec!["aac, 48000 Hz, 2 ch (stereo)".to_string()],
        }
    }

    fn differing(rows: &[MetaRow]) -> Vec<&'static str> {
        rows.iter().filter(|r| r.differs).map(|r| r.label).collect()
    }

    fn values<'a>(rows: &'a [MetaRow], label: &str) -> &'a [String; 2] {
        &rows.iter().find(|r| r.label == label).unwrap().values
    }

    #[test]
    fn identical_video_metadata_marks_nothing() {
        let rows = video_meta_rows(&mp4(), &mp4());
        assert_eq!(differing(&rows), Vec::<&str>::new());
        assert_eq!(
            rows.iter().map(|r| r.label).collect::<Vec<_>>(),
            [
                "Container",
                "Codec",
                "Resolution",
                "Frame rate",
                "Duration",
                "Bitrate",
                "Pixel format",
                "Audio streams"
            ]
        );
        assert_eq!(values(&rows, "Resolution"), &["1920x1080", "1920x1080"]);
        assert_eq!(values(&rows, "Frame rate"), &["29.970 fps", "29.970 fps"]);
    }

    #[test]
    fn video_metadata_diff_marks_differing_fields() {
        let right = VideoMeta {
            container: Some("Matroska / WebM".to_string()),
            codec: Some("vp9".to_string()),
            height: Some(720),
            frame_rate: Some(25.0),
            pixel_format: None,
            audio_streams: vec![
                "aac, 48000 Hz, 2 ch (stereo)".to_string(),
                "ac3, 48000 Hz, 6 ch (5.1)".to_string(),
            ],
            ..mp4()
        };
        let rows = video_meta_rows(&mp4(), &right);
        assert_eq!(
            differing(&rows),
            [
                "Container",
                "Codec",
                "Resolution",
                "Frame rate",
                "Pixel format",
                "Audio streams"
            ]
        );
        assert_eq!(values(&rows, "Resolution"), &["1920x1080", "1920x720"]);
        assert_eq!(values(&rows, "Frame rate"), &["29.970 fps", "25.000 fps"]);
        assert_eq!(values(&rows, "Pixel format"), &["yuv420p", "-"]);
        assert_eq!(
            values(&rows, "Audio streams"),
            &[
                "aac, 48000 Hz, 2 ch (stereo)",
                "aac, 48000 Hz, 2 ch (stereo)\nac3, 48000 Hz, 6 ch (5.1)"
            ]
        );
    }

    #[test]
    fn video_values_that_read_the_same_are_not_marked() {
        let right = VideoMeta {
            frame_rate: Some(29.970_000_1),
            duration_secs: Some(10.000_000_1),
            bit_rate: Some(5_000_300),
            ..mp4()
        };
        assert_eq!(
            differing(&video_meta_rows(&mp4(), &right)),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn no_audio_streams_read_none() {
        let silent = VideoMeta {
            audio_streams: Vec::new(),
            ..mp4()
        };
        let rows = video_meta_rows(&silent, &silent);
        assert_eq!(values(&rows, "Audio streams"), &["none", "none"]);
    }

    #[test]
    fn first_video_stream_and_every_audio_stream() {
        let info = MediaInfo {
            format_name: Some("mov,mp4,m4a,3gp,3g2,mj2".to_string()),
            format_long_name: Some("QuickTime / MOV".to_string()),
            duration_secs: Some(10.0),
            bit_rate: Some(5_000_000),
            streams: vec![
                StreamInfo {
                    codec_name: Some("aac".to_string()),
                    sample_rate: Some(48000),
                    channels: Some(2),
                    channel_layout: Some("stereo".to_string()),
                    ..stream(StreamKind::Audio)
                },
                StreamInfo {
                    codec_name: Some("h264".to_string()),
                    width: Some(1920),
                    height: Some(1080),
                    frame_rate: Some(25.0),
                    pix_fmt: Some("yuv420p".to_string()),
                    duration_secs: Some(9.96),
                    bit_rate: Some(4_800_000),
                    ..stream(StreamKind::Video)
                },
                StreamInfo {
                    codec_name: Some("mjpeg".to_string()),
                    ..stream(StreamKind::Video)
                },
                StreamInfo {
                    codec_name: Some("opus".to_string()),
                    channels: Some(1),
                    ..stream(StreamKind::Audio)
                },
                StreamInfo {
                    ..stream(StreamKind::Audio)
                },
            ],
            ..MediaInfo::default()
        };
        assert_eq!(
            VideoMeta::from_probe(&info),
            Ok(VideoMeta {
                container: Some("QuickTime / MOV".to_string()),
                codec: Some("h264".to_string()),
                width: Some(1920),
                height: Some(1080),
                frame_rate: Some(25.0),
                duration_secs: Some(10.0),
                bit_rate: Some(5_000_000),
                pixel_format: Some("yuv420p".to_string()),
                audio_streams: vec![
                    "aac, 48000 Hz, 2 ch (stereo)".to_string(),
                    "opus, 1 ch".to_string(),
                    "-".to_string(),
                ],
            })
        );
    }

    #[test]
    fn the_video_streams_values_fill_in_for_the_container() {
        let info = MediaInfo {
            format_name: Some("matroska,webm".to_string()),
            streams: vec![StreamInfo {
                duration_secs: Some(9.96),
                bit_rate: Some(4_800_000),
                ..stream(StreamKind::Video)
            }],
            ..MediaInfo::default()
        };
        let meta = VideoMeta::from_probe(&info).unwrap();
        assert_eq!(meta.container.as_deref(), Some("matroska,webm"));
        assert_eq!(meta.duration_secs, Some(9.96));
        assert_eq!(meta.bit_rate, Some(4_800_000));
    }

    #[test]
    fn no_video_stream_is_an_error() {
        let info = MediaInfo {
            streams: vec![stream(StreamKind::Audio)],
            ..MediaInfo::default()
        };
        assert_eq!(
            VideoMeta::from_probe(&info),
            Err("no video stream".to_string())
        );
    }

    #[test]
    fn the_audio_tab_explains_a_missing_audio_track() {
        assert_eq!(audio_tab([None, Some(true)]), AudioTab::Probing);
        assert_eq!(audio_tab([Some(false), Some(false)]), AudioTab::NoAudio);
        assert_eq!(
            audio_tab([Some(true), Some(false)]),
            AudioTab::Viewer {
                notes: vec!["Right: no audio track".to_string()]
            }
        );
        assert_eq!(
            audio_tab([Some(true), Some(true)]),
            AudioTab::Viewer { notes: Vec::new() }
        );
    }
}
