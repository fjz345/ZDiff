//! Audio metadata table: ffprobe fields of both sides with differing values marked, and
//! integrated loudness (ffmpeg's ebur128 filter) measured on request.

use std::{ffi::OsString, path::Path, sync::atomic::AtomicBool};

use eframe::egui;
use zcommon::{
    ffmpeg::{FfmpegPaths, MediaInfo, StreamKind, probe, run_ffmpeg},
    job::Job,
};

use super::hex::is_same_side;
use crate::file::LoadedFile;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioMeta {
    pub codec: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub channel_layout: Option<String>,
    pub sample_format: Option<String>,
    pub duration_secs: Option<f64>,
    pub bit_rate: Option<u64>,
    pub loudness_lufs: Option<f64>,
}

impl AudioMeta {
    /// The first audio stream, the one the decoder maps; duration and bitrate fall back to the
    /// container's when the stream has none.
    pub fn from_probe(info: &MediaInfo) -> Result<Self, String> {
        let stream = info
            .streams
            .iter()
            .find(|s| s.kind == StreamKind::Audio)
            .ok_or_else(|| "no audio stream".to_string())?;
        Ok(AudioMeta {
            codec: stream.codec_name.clone(),
            sample_rate: stream.sample_rate,
            channels: stream.channels,
            channel_layout: stream.channel_layout.clone(),
            sample_format: stream.sample_fmt.clone(),
            duration_secs: stream.duration_secs.or(info.duration_secs),
            bit_rate: stream.bit_rate.or(info.bit_rate),
            loudness_lufs: None,
        })
    }
}

fn shown<T>(value: Option<T>, format: impl FnOnce(T) -> String) -> String {
    value.map(format).unwrap_or_else(|| "-".to_string())
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetaRow {
    pub label: &'static str,
    pub values: [String; 2],
    pub differs: bool,
}

type Field = (&'static str, fn(&AudioMeta) -> String);

/// Compares the displayed values, so a mark is never on two values that read the same.
pub fn meta_rows(left: &AudioMeta, right: &AudioMeta) -> Vec<MetaRow> {
    let fields: [Field; 7] = [
        ("Codec", |m| shown(m.codec.as_deref(), str::to_string)),
        ("Sample rate", |m| {
            shown(m.sample_rate, |r| format!("{r} Hz"))
        }),
        ("Channels", |m| {
            shown(m.channels, |c| match &m.channel_layout {
                Some(layout) => format!("{c} ({layout})"),
                None => c.to_string(),
            })
        }),
        ("Sample format", |m| {
            shown(m.sample_format.as_deref(), str::to_string)
        }),
        ("Duration", |m| {
            shown(m.duration_secs, |d| format!("{d:.3} s"))
        }),
        ("Bitrate", |m| {
            shown(m.bit_rate, |b| format!("{:.0} kb/s", b as f64 / 1000.0))
        }),
        ("Integrated loudness", |m| {
            shown(m.loudness_lufs, |l| format!("{l:.1} LUFS"))
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

const LOUDNESS_KEY: &str = "lavfi.r128.I";

/// Prints ebur128's running integrated loudness to stdout; the last value is the file's.
pub fn loudness_args(input: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-i"]
        .map(OsString::from)
        .to_vec();
    args.extend([
        input.into(),
        "-map".into(),
        "0:a:0".into(),
        "-af".into(),
        format!("ebur128=metadata=1,ametadata=mode=print:key={LOUDNESS_KEY}:file=-").into(),
    ]);
    args.extend(["-f", "null", "-"].map(OsString::from));
    args
}

pub fn parse_loudness(stdout: &str) -> Option<f64> {
    let prefix = format!("{LOUDNESS_KEY}=");
    let last = stdout
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(prefix.as_str()))?;
    last.parse().ok()
}

fn measure_loudness(ffmpeg: &FfmpegPaths, input: &Path, cancel: &AtomicBool) -> Measured {
    let mut stdout = Vec::new();
    run_ffmpeg(&ffmpeg.ffmpeg, &loudness_args(input), cancel, |chunk| {
        stdout.extend_from_slice(chunk)
    })
    .map_err(|e| e.to_string())?;
    parse_loudness(&String::from_utf8_lossy(&stdout))
        .ok_or_else(|| "ffmpeg measured no loudness (too short?)".to_string())
}

fn local_path(file: &LoadedFile) -> Result<&Path, String> {
    file.path()
        .as_path()
        .ok_or_else(|| format!("{}: metadata needs a local file", file.path()))
}

type Probed = Result<AudioMeta, String>;
type Measured = Result<f64, String>;

#[derive(Debug)]
enum Slot<T> {
    Idle,
    Running,
    Done(T),
}

/// Probes both sides when the pair changes; loudness runs only on request. A pair change
/// cancels running jobs and drops their results.
pub struct AudioMetaProcessor {
    files: [Option<LoadedFile>; 2],
    ffmpeg: Option<FfmpegPaths>,
    probe_jobs: [Job<Probed>; 2],
    loudness_jobs: [Job<Measured>; 2],
    probes: [Slot<Probed>; 2],
    loudness: [Slot<Measured>; 2],
}

impl std::fmt::Debug for AudioMetaProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioMetaProcessor")
            .field("probes", &self.probes)
            .field("loudness", &self.loudness)
            .finish()
    }
}

impl Default for AudioMetaProcessor {
    fn default() -> Self {
        Self {
            files: [None, None],
            ffmpeg: None,
            probe_jobs: std::array::from_fn(|_| Job::new(|| {}).on_panic(Err)),
            loudness_jobs: std::array::from_fn(|_| Job::new(|| {}).on_panic(Err)),
            probes: [Slot::Idle, Slot::Idle],
            loudness: [Slot::Idle, Slot::Idle],
        }
    }
}

impl AudioMetaProcessor {
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
        for job in &mut self.probe_jobs {
            job.cancel();
        }
        for job in &mut self.loudness_jobs {
            job.cancel();
        }
        self.files = files;
        self.ffmpeg = ffmpeg;
        self.loudness = [Slot::Idle, Slot::Idle];
        self.probes = [Slot::Idle, Slot::Idle];
        let Some(ffmpeg) = &self.ffmpeg else {
            return;
        };
        for index in 0..2 {
            let file = self.files[index].as_ref().expect("both sides requested");
            self.probes[index] = Slot::Running;
            let path = local_path(file).map(Path::to_path_buf);
            let ffmpeg = ffmpeg.clone();
            self.probe_jobs[index].start(move |ctx| {
                ctx.send(
                    path.and_then(|path| probe(&ffmpeg, &path, ctx.cancel_flag()))
                        .and_then(|info| AudioMeta::from_probe(&info)),
                );
            });
        }
    }

    pub fn measure_loudness(&mut self) {
        let Some(ffmpeg) = self.ffmpeg.clone() else {
            return;
        };
        for index in 0..2 {
            let Some(file) = &self.files[index] else {
                continue;
            };
            self.loudness[index] = Slot::Running;
            let path = local_path(file).map(Path::to_path_buf);
            let ffmpeg = ffmpeg.clone();
            self.loudness_jobs[index].start(move |ctx| {
                ctx.send(path.and_then(|path| measure_loudness(&ffmpeg, &path, ctx.cancel_flag())));
            });
        }
    }

    pub fn poll(&mut self) {
        for index in 0..2 {
            if let Some(probed) = self.probe_jobs[index].poll().pop() {
                self.probes[index] = Slot::Done(probed);
            }
            if let Some(measured) = self.loudness_jobs[index].poll().pop() {
                self.loudness[index] = Slot::Done(measured);
            }
        }
    }

    fn is_running(&self) -> bool {
        self.probes.iter().any(|j| matches!(j, Slot::Running))
            || self.loudness.iter().any(|j| matches!(j, Slot::Running))
    }
}

pub fn show(ui: &mut egui::Ui, processor: &mut AudioMetaProcessor) {
    processor.poll();
    if processor.is_running() {
        ui.ctx().request_repaint();
    }
    let metas: [Option<AudioMeta>; 2] = std::array::from_fn(|index| {
        let Slot::Done(Ok(meta)) = &processor.probes[index] else {
            return None;
        };
        let loudness_lufs = match &processor.loudness[index] {
            Slot::Done(Ok(lufs)) => Some(*lufs),
            _ => None,
        };
        Some(AudioMeta {
            loudness_lufs,
            ..meta.clone()
        })
    });

    ui.add_space(8.0);
    ui.heading("Metadata");
    for index in 0..2 {
        let side = ["Left", "Right"][index];
        match &processor.probes[index] {
            Slot::Running => {
                ui.label(format!("{side}: probing..."));
            }
            Slot::Done(Err(e)) => {
                ui.colored_label(ui.visuals().warn_fg_color, format!("{side}: {e}"));
            }
            Slot::Idle | Slot::Done(Ok(_)) => {}
        }
    }
    if let [Some(left), Some(right)] = &metas {
        let marked = ui.visuals().warn_fg_color;
        egui::Grid::new("audio_meta_grid")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                ui.label("");
                ui.strong("Left");
                ui.strong("Right");
                ui.end_row();
                for row in meta_rows(left, right) {
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

    ui.horizontal(|ui| {
        let measuring = processor
            .loudness
            .iter()
            .any(|j| matches!(j, Slot::Running));
        let button = ui.add_enabled(
            !measuring && processor.ffmpeg.is_some(),
            egui::Button::new("Measure loudness"),
        );
        if button.clicked() {
            processor.measure_loudness();
        }
        if measuring {
            ui.spinner();
        }
    });
    for index in 0..2 {
        if let Slot::Done(Err(e)) = &processor.loudness[index] {
            let side = ["Left", "Right"][index];
            ui.colored_label(ui.visuals().warn_fg_color, format!("{side} loudness: {e}"));
        }
    }
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

    fn wav() -> AudioMeta {
        AudioMeta {
            codec: Some("pcm_s16le".to_string()),
            sample_rate: Some(44100),
            channels: Some(2),
            channel_layout: Some("stereo".to_string()),
            sample_format: Some("s16".to_string()),
            duration_secs: Some(0.5),
            bit_rate: Some(1_411_200),
            loudness_lufs: None,
        }
    }

    fn differing(rows: &[MetaRow]) -> Vec<&'static str> {
        rows.iter().filter(|r| r.differs).map(|r| r.label).collect()
    }

    #[test]
    fn identical_metadata_marks_nothing() {
        let rows = meta_rows(&wav(), &wav());
        assert_eq!(differing(&rows), Vec::<&str>::new());
        assert_eq!(
            rows.iter().map(|r| r.label).collect::<Vec<_>>(),
            [
                "Codec",
                "Sample rate",
                "Channels",
                "Sample format",
                "Duration",
                "Bitrate",
                "Integrated loudness"
            ]
        );
    }

    #[test]
    fn differing_fields_are_marked() {
        let right = AudioMeta {
            sample_rate: Some(48000),
            channel_layout: Some("mono".to_string()),
            sample_format: Some("fltp".to_string()),
            bit_rate: None,
            loudness_lufs: Some(-23.0),
            ..wav()
        };
        let rows = meta_rows(&wav(), &right);
        assert_eq!(
            differing(&rows),
            [
                "Sample rate",
                "Channels",
                "Sample format",
                "Bitrate",
                "Integrated loudness"
            ]
        );
        let rate = rows.iter().find(|r| r.label == "Sample rate").unwrap();
        assert_eq!(rate.values, ["44100 Hz", "48000 Hz"]);
        let channels = rows.iter().find(|r| r.label == "Channels").unwrap();
        assert_eq!(channels.values, ["2 (stereo)", "2 (mono)"]);
        let bitrate = rows.iter().find(|r| r.label == "Bitrate").unwrap();
        assert_eq!(bitrate.values, ["1411 kb/s", "-"]);
    }

    #[test]
    fn values_that_read_the_same_are_not_marked() {
        let right = AudioMeta {
            duration_secs: Some(0.500_000_1),
            bit_rate: Some(1_411_300),
            loudness_lufs: Some(-21.04),
            ..wav()
        };
        let left = AudioMeta {
            loudness_lufs: Some(-21.0),
            ..wav()
        };
        assert_eq!(differing(&meta_rows(&left, &right)), Vec::<&str>::new());
    }

    #[test]
    fn first_audio_stream_with_container_fallbacks() {
        let info = MediaInfo {
            duration_secs: Some(3.0),
            bit_rate: Some(192_000),
            streams: vec![
                StreamInfo {
                    codec_name: Some("mjpeg".to_string()),
                    ..stream(StreamKind::Video)
                },
                StreamInfo {
                    codec_name: Some("mp3".to_string()),
                    sample_rate: Some(44100),
                    channels: Some(2),
                    sample_fmt: Some("fltp".to_string()),
                    ..stream(StreamKind::Audio)
                },
                StreamInfo {
                    codec_name: Some("aac".to_string()),
                    ..stream(StreamKind::Audio)
                },
            ],
            ..MediaInfo::default()
        };
        assert_eq!(
            AudioMeta::from_probe(&info),
            Ok(AudioMeta {
                codec: Some("mp3".to_string()),
                sample_rate: Some(44100),
                channels: Some(2),
                channel_layout: None,
                sample_format: Some("fltp".to_string()),
                duration_secs: Some(3.0),
                bit_rate: Some(192_000),
                loudness_lufs: None,
            })
        );
    }

    #[test]
    fn stream_values_win_over_the_container() {
        let info = MediaInfo {
            duration_secs: Some(3.0),
            bit_rate: Some(192_000),
            streams: vec![StreamInfo {
                duration_secs: Some(2.5),
                bit_rate: Some(128_000),
                ..stream(StreamKind::Audio)
            }],
            ..MediaInfo::default()
        };
        let meta = AudioMeta::from_probe(&info).unwrap();
        assert_eq!(meta.duration_secs, Some(2.5));
        assert_eq!(meta.bit_rate, Some(128_000));
    }

    #[test]
    fn no_audio_stream_is_an_error() {
        let info = MediaInfo {
            streams: vec![stream(StreamKind::Video)],
            ..MediaInfo::default()
        };
        assert_eq!(
            AudioMeta::from_probe(&info),
            Err("no audio stream".to_string())
        );
    }

    #[test]
    fn loudness_runs_ebur128_on_the_first_audio_stream_into_a_null_output() {
        let args: Vec<String> = loudness_args(Path::new("a.wav"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-i",
                "a.wav",
                "-map",
                "0:a:0",
                "-af",
                "ebur128=metadata=1,ametadata=mode=print:key=lavfi.r128.I:file=-",
                "-f",
                "null",
                "-"
            ]
        );
    }

    #[test]
    fn loudness_is_the_last_running_value() {
        let stdout = "frame:0    pts:0       pts_time:0\r\n\
                      lavfi.r128.I=-70.000\r\n\
                      frame:1    pts:4410    pts_time:0.1\n\
                      lavfi.r128.I=-21.070\n";
        assert_eq!(parse_loudness(stdout), Some(-21.07));
        assert_eq!(
            parse_loudness("lavfi.r128.I=-inf\n"),
            Some(f64::NEG_INFINITY)
        );
        assert_eq!(parse_loudness(""), None);
        assert_eq!(parse_loudness("lavfi.r128.I=nonsense\n"), None);
    }
}
