use std::{
    env::consts::EXE_SUFFIX,
    ffi::OsString,
    fmt,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::Duration,
};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegPaths {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegNotFound {
    pub searched: Vec<String>,
}

impl fmt::Display for FfmpegNotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ffmpeg and ffprobe not found. Looked in: {}",
            self.searched.join(", ")
        )
    }
}

impl std::error::Error for FfmpegNotFound {}

/// `configured` is a folder holding both executables, or the ffmpeg executable itself.
/// A place counts only if it has both ffmpeg and ffprobe.
pub fn resolve_ffmpeg(
    configured: Option<&Path>,
    exe_dir: Option<&Path>,
    is_file: impl Fn(&Path) -> bool,
    find_on_path: impl Fn(&str) -> Option<PathBuf>,
) -> Result<FfmpegPaths, FfmpegNotFound> {
    let ffmpeg_exe = format!("ffmpeg{EXE_SUFFIX}");
    let pair_in = |dir: &Path| {
        let paths = FfmpegPaths {
            ffmpeg: dir.join(&ffmpeg_exe),
            ffprobe: dir.join(format!("ffprobe{EXE_SUFFIX}")),
        };
        (is_file(&paths.ffmpeg) && is_file(&paths.ffprobe)).then_some(paths)
    };
    let mut searched = Vec::new();

    if let Some(configured) = configured {
        let dir = match configured.file_name() {
            Some(name) if name.eq_ignore_ascii_case(&ffmpeg_exe) => {
                configured.parent().unwrap_or(configured)
            }
            _ => configured,
        };
        searched.push(format!("{} (configured)", dir.display()));
        if let Some(paths) = pair_in(dir) {
            return Ok(paths);
        }
    }
    if let Some(dir) = exe_dir {
        searched.push(format!("{} (next to the app)", dir.display()));
        if let Some(paths) = pair_in(dir) {
            return Ok(paths);
        }
    }
    searched.push("PATH".to_string());
    if let (Some(ffmpeg), Some(ffprobe)) = (find_on_path("ffmpeg"), find_on_path("ffprobe")) {
        return Ok(FfmpegPaths { ffmpeg, ffprobe });
    }
    Err(FfmpegNotFound { searched })
}

pub fn find_ffmpeg(configured: Option<&Path>) -> Result<FfmpegPaths, FfmpegNotFound> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let path_var = std::env::var_os("PATH");
    resolve_ffmpeg(configured, exe_dir.as_deref(), Path::is_file, |name| {
        let exe = format!("{name}{EXE_SUFFIX}");
        std::env::split_paths(path_var.as_deref()?)
            .map(|dir| dir.join(&exe))
            .find(|path| path.is_file())
    })
}

fn quiet_args() -> Vec<OsString> {
    ["-hide_banner", "-loglevel", "error", "-nostdin"]
        .map(OsString::from)
        .to_vec()
}

fn decode_args(input: &Path, resample: Option<(u32, u32)>) -> Vec<OsString> {
    let mut args = quiet_args();
    args.extend(["-i".into(), input.into(), "-map".into(), "0:a:0".into()]);
    if let Some((sample_rate, channels)) = resample {
        args.extend([
            "-ar".into(),
            sample_rate.to_string().into(),
            "-ac".into(),
            channels.to_string().into(),
        ]);
    }
    args.extend(["-c:a", "pcm_f32le", "-f", "f32le", "-"].map(OsString::from));
    args
}

/// Interleaved f32 samples at the file's own rate and layout.
pub fn pcm_decode_args(input: &Path) -> Vec<OsString> {
    decode_args(input, None)
}

pub fn resampled_decode_args(input: &Path, sample_rate: u32, channels: u32) -> Vec<OsString> {
    decode_args(input, Some((sample_rate, channels)))
}

pub fn frames_at_args(input: &Path, time_secs: f64, count: u64) -> Vec<OsString> {
    assert!(
        time_secs.is_finite() && time_secs >= 0.0,
        "seek time {time_secs}"
    );
    assert!(count > 0, "frame count");
    let mut args = quiet_args();
    args.extend([
        "-ss".into(),
        time_secs.to_string().into(),
        "-i".into(),
        input.into(),
    ]);
    args.extend(["-map".into(), "0:v:0".into()]);
    args.extend(["-frames:v".into(), count.to_string().into()]);
    args.extend(["-pix_fmt", "rgba", "-f", "rawvideo", "-"].map(OsString::from));
    args
}

/// Back-to-back `width * height * 4` byte RGBA frames.
pub fn scaled_frames_args(input: &Path, width: u32, height: u32, fps: f64) -> Vec<OsString> {
    assert!(fps.is_finite() && fps > 0.0, "fps {fps}");
    let mut args = quiet_args();
    args.extend([
        "-i".into(),
        input.into(),
        "-map".into(),
        "0:v:0".into(),
        "-vf".into(),
        format!("fps={fps},scale={width}:{height}").into(),
    ]);
    args.extend(["-pix_fmt", "rgba", "-f", "rawvideo", "-"].map(OsString::from));
    args
}

pub fn probe_args(input: &Path) -> Vec<OsString> {
    let mut args = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-print_format",
        "json",
    ]
    .map(OsString::from)
    .to_vec();
    args.extend([
        "-show_format".into(),
        "-show_streams".into(),
        "-i".into(),
        input.into(),
    ]);
    args
}

#[derive(Debug, Clone, PartialEq)]
pub enum FfmpegError {
    Spawn(String),
    Io(String),
    Cancelled,
    Failed { status: String, stderr: String },
}

impl fmt::Display for FfmpegError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FfmpegError::Spawn(e) | FfmpegError::Io(e) => write!(f, "{e}"),
            FfmpegError::Cancelled => write!(f, "cancelled"),
            FfmpegError::Failed { status, stderr } if stderr.is_empty() => write!(f, "{status}"),
            FfmpegError::Failed { status, stderr } => write!(f, "{stderr} ({status})"),
        }
    }
}

impl std::error::Error for FfmpegError {}

const STDOUT_CHUNK: usize = 1 << 16;
const CANCEL_POLL: Duration = Duration::from_millis(20);

/// Blocks until the process exits, so call it from a worker thread. `on_stdout` runs on the
/// calling thread. Setting `cancel` kills the process.
pub fn run_ffmpeg(
    exe: &Path,
    args: &[OsString],
    cancel: &AtomicBool,
    mut on_stdout: impl FnMut(&[u8]),
) -> Result<(), FfmpegError> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashing up per spawn from the GUI apps.
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| FfmpegError::Spawn(format!("{}: {e}", exe.display())))?;

    let mut stderr = child.stderr.take().expect("stderr is piped");
    let stderr_thread = thread::spawn(move || {
        let mut text = Vec::new();
        if let Err(e) = stderr.read_to_end(&mut text) {
            text.extend_from_slice(format!("\n(reading stderr failed: {e})").as_bytes());
        }
        text
    });

    let mut stdout = child.stdout.take().expect("stdout is piped");
    let (tx, rx) = mpsc::sync_channel::<io::Result<Vec<u8>>>(4);
    let stdout_thread = thread::spawn(move || {
        loop {
            let mut chunk = vec![0u8; STDOUT_CHUNK];
            let sent = match stdout.read(&mut chunk) {
                Ok(0) => return,
                Ok(len) => {
                    chunk.truncate(len);
                    tx.send(Ok(chunk))
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => tx.send(Err(e)),
            };
            if sent.is_err() {
                return;
            }
        }
    });

    let mut read_error = None;
    let cancelled = loop {
        if cancel.load(Ordering::Relaxed) {
            break true;
        }
        match rx.recv_timeout(CANCEL_POLL) {
            Ok(Ok(chunk)) => on_stdout(&chunk),
            Ok(Err(e)) => {
                read_error = Some(e);
                break false;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break false,
        }
    };

    if cancelled || read_error.is_some() {
        child
            .kill()
            .map_err(|e| FfmpegError::Io(format!("killing {}: {e}", exe.display())))?;
    }
    // Unblocks the reader if it is waiting on a full channel.
    drop(rx);
    let status = child
        .wait()
        .map_err(|e| FfmpegError::Io(format!("waiting for {}: {e}", exe.display())))?;
    stdout_thread.join().expect("stdout reader panicked");
    let stderr = stderr_thread.join().expect("stderr reader panicked");

    if cancelled {
        return Err(FfmpegError::Cancelled);
    }
    if let Some(e) = read_error {
        return Err(FfmpegError::Io(format!(
            "reading {} output: {e}",
            exe.display()
        )));
    }
    if !status.success() {
        return Err(FfmpegError::Failed {
            status: status.to_string(),
            stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
        });
    }
    Ok(())
}

pub fn probe(paths: &FfmpegPaths, input: &Path, cancel: &AtomicBool) -> Result<MediaInfo, String> {
    let mut json = Vec::new();
    run_ffmpeg(&paths.ffprobe, &probe_args(input), cancel, |chunk| {
        json.extend_from_slice(chunk)
    })
    .map_err(|e| e.to_string())?;
    parse_probe(&String::from_utf8_lossy(&json))
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaInfo {
    pub format_name: Option<String>,
    pub format_long_name: Option<String>,
    pub duration_secs: Option<f64>,
    pub bit_rate: Option<u64>,
    pub size_bytes: Option<u64>,
    pub streams: Vec<StreamInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamKind {
    Audio,
    Video,
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub index: Option<u32>,
    pub kind: StreamKind,
    pub codec_name: Option<String>,
    pub codec_long_name: Option<String>,
    pub duration_secs: Option<f64>,
    pub bit_rate: Option<u64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub channel_layout: Option<String>,
    pub sample_fmt: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub pix_fmt: Option<String>,
    pub frame_rate: Option<f64>,
    pub frame_count: Option<u64>,
}

fn json_string(obj: &Value, key: &str) -> Option<String> {
    obj.get(key)?.as_str().map(str::to_owned)
}

/// ffprobe writes most numbers as strings ("44100", "0.500000", "N/A").
fn json_number<T: std::str::FromStr>(obj: &Value, key: &str) -> Option<T> {
    match obj.get(key)? {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.to_string().parse().ok(),
        _ => None,
    }
}

fn json_rate(obj: &Value, key: &str) -> Option<f64> {
    let rate = json_string(obj, key)?;
    let (num, den) = rate.split_once('/')?;
    let rate = num.parse::<f64>().ok()? / den.parse::<f64>().ok()?;
    (rate.is_finite() && rate > 0.0).then_some(rate)
}

fn parse_stream(stream: &Value) -> StreamInfo {
    StreamInfo {
        index: json_number(stream, "index"),
        kind: match json_string(stream, "codec_type")
            .unwrap_or_default()
            .as_str()
        {
            "audio" => StreamKind::Audio,
            "video" => StreamKind::Video,
            other => StreamKind::Other(other.to_string()),
        },
        codec_name: json_string(stream, "codec_name"),
        codec_long_name: json_string(stream, "codec_long_name"),
        duration_secs: json_number(stream, "duration"),
        bit_rate: json_number(stream, "bit_rate"),
        sample_rate: json_number(stream, "sample_rate"),
        channels: json_number(stream, "channels"),
        channel_layout: json_string(stream, "channel_layout"),
        sample_fmt: json_string(stream, "sample_fmt"),
        width: json_number(stream, "width"),
        height: json_number(stream, "height"),
        pix_fmt: json_string(stream, "pix_fmt"),
        frame_rate: json_rate(stream, "avg_frame_rate")
            .or_else(|| json_rate(stream, "r_frame_rate")),
        frame_count: json_number(stream, "nb_frames"),
    }
}

/// Parses `ffprobe -print_format json -show_format -show_streams`. Missing or malformed fields
/// are `None`; only output that isn't a JSON object is an error.
pub fn parse_probe(json: &str) -> Result<MediaInfo, String> {
    let root: Value = serde_json::from_str(json).map_err(|e| format!("ffprobe output: {e}"))?;
    if !root.is_object() {
        return Err("ffprobe output is not a JSON object".to_string());
    }
    let format = root.get("format").unwrap_or(&Value::Null);
    let streams = match root.get("streams") {
        Some(Value::Array(streams)) => streams.iter().map(parse_stream).collect(),
        _ => Vec::new(),
    };
    Ok(MediaInfo {
        format_name: json_string(format, "format_name"),
        format_long_name: json_string(format, "format_long_name"),
        duration_secs: json_number(format, "duration"),
        bit_rate: json_number(format, "bit_rate"),
        size_bytes: json_number(format, "size"),
        streams,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn exe(name: &str) -> String {
        format!("{name}{}", std::env::consts::EXE_SUFFIX)
    }

    fn files(paths: &[PathBuf]) -> impl Fn(&Path) -> bool {
        let paths = paths.to_vec();
        move |p: &Path| paths.iter().any(|f| f == p)
    }

    fn pair_in(dir: &Path) -> Vec<PathBuf> {
        vec![dir.join(exe("ffmpeg")), dir.join(exe("ffprobe"))]
    }

    fn on_path(dir: &'static str) -> impl Fn(&str) -> Option<PathBuf> {
        move |name: &str| Some(Path::new(dir).join(exe(name)))
    }

    fn nothing_on_path(_: &str) -> Option<PathBuf> {
        None
    }

    #[test]
    fn configured_folder_wins_over_exe_dir_and_path() {
        let cfg = Path::new("C:/cfg");
        let app = Path::new("C:/app");
        let mut existing = pair_in(cfg);
        existing.extend(pair_in(app));
        let found =
            resolve_ffmpeg(Some(cfg), Some(app), files(&existing), on_path("C:/bin")).unwrap();
        assert_eq!(found.ffmpeg, cfg.join(exe("ffmpeg")));
        assert_eq!(found.ffprobe, cfg.join(exe("ffprobe")));
    }

    #[test]
    fn configured_ffmpeg_exe_uses_its_folder() {
        let cfg = Path::new("C:/cfg");
        let found = resolve_ffmpeg(
            Some(&cfg.join(exe("ffmpeg"))),
            None,
            files(&pair_in(cfg)),
            nothing_on_path,
        )
        .unwrap();
        assert_eq!(found.ffmpeg, cfg.join(exe("ffmpeg")));
        assert_eq!(found.ffprobe, cfg.join(exe("ffprobe")));
    }

    #[test]
    fn next_to_the_exe_wins_over_path() {
        let app = Path::new("C:/app");
        let found = resolve_ffmpeg(
            Some(Path::new("C:/missing")),
            Some(app),
            files(&pair_in(app)),
            on_path("C:/bin"),
        )
        .unwrap();
        assert_eq!(found.ffmpeg, app.join(exe("ffmpeg")));
        assert_eq!(found.ffprobe, app.join(exe("ffprobe")));
    }

    #[test]
    fn ffmpeg_without_ffprobe_in_a_folder_is_skipped() {
        let app = Path::new("C:/app");
        let found = resolve_ffmpeg(
            None,
            Some(app),
            files(&[app.join(exe("ffmpeg"))]),
            on_path("C:/bin"),
        )
        .unwrap();
        assert_eq!(found.ffmpeg, Path::new("C:/bin").join(exe("ffmpeg")));
    }

    #[test]
    fn path_is_used_last() {
        let found = resolve_ffmpeg(
            None,
            Some(Path::new("C:/app")),
            files(&[]),
            on_path("C:/bin"),
        )
        .unwrap();
        assert_eq!(found.ffmpeg, Path::new("C:/bin").join(exe("ffmpeg")));
        assert_eq!(found.ffprobe, Path::new("C:/bin").join(exe("ffprobe")));
    }

    #[test]
    fn nothing_found_lists_every_place() {
        let err = resolve_ffmpeg(
            Some(Path::new("C:/cfg")),
            Some(Path::new("C:/app")),
            files(&[]),
            nothing_on_path,
        )
        .unwrap_err();
        assert_eq!(err.searched.len(), 3);
        let text = err.to_string();
        assert!(text.contains("C:/cfg"), "{text}");
        assert!(text.contains("C:/app"), "{text}");
        assert!(text.contains("PATH"), "{text}");
    }

    #[test]
    fn nothing_configured_lists_exe_dir_and_path() {
        let err = resolve_ffmpeg(None, Some(Path::new("C:/app")), files(&[]), nothing_on_path)
            .unwrap_err();
        assert_eq!(err.searched.len(), 2);
    }

    #[test]
    fn pcm_decode_is_first_audio_stream_as_f32le_on_stdout() {
        assert_eq!(
            strings(&pcm_decode_args(Path::new("in.wav"))),
            [
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-i",
                "in.wav",
                "-map",
                "0:a:0",
                "-c:a",
                "pcm_f32le",
                "-f",
                "f32le",
                "-"
            ]
        );
    }

    #[test]
    fn resampled_decode_sets_rate_and_channels() {
        assert_eq!(
            strings(&resampled_decode_args(Path::new("in.wav"), 48000, 2)),
            [
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-i",
                "in.wav",
                "-map",
                "0:a:0",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-c:a",
                "pcm_f32le",
                "-f",
                "f32le",
                "-"
            ]
        );
    }

    #[test]
    fn frame_at_seeks_before_input_and_reads_one_rgba_frame() {
        assert_eq!(
            strings(&frames_at_args(Path::new("in.mp4"), 1.5, 1)),
            [
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-ss",
                "1.5",
                "-i",
                "in.mp4",
                "-map",
                "0:v:0",
                "-frames:v",
                "1",
                "-pix_fmt",
                "rgba",
                "-f",
                "rawvideo",
                "-"
            ]
        );
    }

    #[test]
    fn frames_at_reads_a_run_of_frames_from_one_seek() {
        let args = strings(&frames_at_args(Path::new("in.mp4"), 0.0, 16));
        let count = args.iter().position(|a| a == "-frames:v").unwrap() + 1;
        assert_eq!(args[count], "16");
        assert_eq!(args.iter().filter(|a| *a == "-ss").count(), 1);
    }

    #[test]
    #[should_panic(expected = "frame count")]
    fn frames_at_needs_a_frame() {
        frames_at_args(Path::new("in.mp4"), 0.0, 0);
    }

    #[test]
    fn scaled_frames_resample_fps_then_scale() {
        assert_eq!(
            strings(&scaled_frames_args(Path::new("in.mp4"), 160, 90, 10.0)),
            [
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-i",
                "in.mp4",
                "-map",
                "0:v:0",
                "-vf",
                "fps=10,scale=160:90",
                "-pix_fmt",
                "rgba",
                "-f",
                "rawvideo",
                "-"
            ]
        );
    }

    #[test]
    fn probe_prints_format_and_streams_as_json() {
        assert_eq!(
            strings(&probe_args(Path::new("in.mp4"))),
            [
                "-hide_banner",
                "-loglevel",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
                "-i",
                "in.mp4"
            ]
        );
    }

    #[test]
    fn probe_audio_file() {
        let info = parse_probe(include_str!("ffmpeg/probe_audio.json")).unwrap();
        assert_eq!(info.format_name.as_deref(), Some("wav"));
        assert_eq!(info.duration_secs, Some(0.5));
        assert_eq!(info.bit_rate, Some(1412448));
        assert_eq!(info.size_bytes, Some(88278));
        assert_eq!(info.streams.len(), 1);
        let s = &info.streams[0];
        assert_eq!(s.index, Some(0));
        assert_eq!(s.kind, StreamKind::Audio);
        assert_eq!(s.codec_name.as_deref(), Some("pcm_s16le"));
        assert_eq!(s.sample_rate, Some(44100));
        assert_eq!(s.channels, Some(2));
        assert_eq!(s.channel_layout, None);
        assert_eq!(s.sample_fmt.as_deref(), Some("s16"));
        assert_eq!(s.duration_secs, Some(0.5));
        assert_eq!(s.bit_rate, Some(1411200));
        assert_eq!(s.frame_rate, None);
        assert_eq!(s.width, None);
    }

    #[test]
    fn probe_video_file() {
        let info = parse_probe(include_str!("ffmpeg/probe_video.json")).unwrap();
        assert_eq!(info.format_name.as_deref(), Some("mov,mp4,m4a,3gp,3g2,mj2"));
        assert_eq!(info.format_long_name.as_deref(), Some("QuickTime / MOV"));
        assert_eq!(info.duration_secs, Some(0.4));
        assert_eq!(info.streams.len(), 2);
        let v = &info.streams[0];
        assert_eq!(v.kind, StreamKind::Video);
        assert_eq!(v.codec_name.as_deref(), Some("h264"));
        assert_eq!((v.width, v.height), (Some(64), Some(48)));
        assert_eq!(v.pix_fmt.as_deref(), Some("yuv420p"));
        assert_eq!(v.frame_rate, Some(25.0));
        assert_eq!(v.frame_count, Some(10));
        assert_eq!(v.bit_rate, Some(30740));
        let a = &info.streams[1];
        assert_eq!(a.kind, StreamKind::Audio);
        assert_eq!(a.codec_name.as_deref(), Some("aac"));
        assert_eq!(a.sample_rate, Some(48000));
        assert_eq!(a.channels, Some(1));
        assert_eq!(a.channel_layout.as_deref(), Some("mono"));
    }

    #[test]
    fn probe_ntsc_frame_rate() {
        let info = parse_probe(
            r#"{"streams":[{"codec_type":"video","avg_frame_rate":"30000/1001","r_frame_rate":"30000/1001"}]}"#,
        )
        .unwrap();
        assert!((info.streams[0].frame_rate.unwrap() - 29.97).abs() < 0.001);
    }

    #[test]
    fn probe_falls_back_to_r_frame_rate() {
        let info = parse_probe(
            r#"{"streams":[{"codec_type":"video","avg_frame_rate":"0/0","r_frame_rate":"24/1"}]}"#,
        )
        .unwrap();
        assert_eq!(info.streams[0].frame_rate, Some(24.0));
    }

    #[test]
    fn probe_missing_fields_are_none() {
        let info = parse_probe(r#"{"streams":[{}, {"codec_type":"subtitle"}]}"#).unwrap();
        assert_eq!(info.format_name, None);
        assert_eq!(info.duration_secs, None);
        assert_eq!(info.streams[0].kind, StreamKind::Other(String::new()));
        assert_eq!(info.streams[0].sample_rate, None);
        assert_eq!(info.streams[1].kind, StreamKind::Other("subtitle".into()));
        assert_eq!(parse_probe("{}").unwrap(), MediaInfo::default());
    }

    #[test]
    fn probe_wrong_types_are_none() {
        let info = parse_probe(
            r#"{"format":{"duration":"N/A","bit_rate":[]},"streams":[{"codec_type":3,"width":"x","sample_rate":-5}]}"#,
        )
        .unwrap();
        assert_eq!(info.duration_secs, None);
        assert_eq!(info.bit_rate, None);
        assert_eq!(info.streams[0].width, None);
        assert_eq!(info.streams[0].sample_rate, None);
    }

    #[test]
    fn probe_invalid_json_is_an_error() {
        assert!(parse_probe("not json").is_err());
        assert!(parse_probe("[]").is_err());
    }

    #[test]
    fn run_missing_exe_is_a_spawn_error() {
        let cancel = AtomicBool::new(false);
        let err = run_ffmpeg(
            Path::new("C:/does/not/exist/ffmpeg.exe"),
            &[],
            &cancel,
            |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, FfmpegError::Spawn(_)), "{err:?}");
    }

    fn wav_16bit_mono(rate: u32, samples: &[i16]) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    #[ignore = "needs ffmpeg and ffprobe: next to the test exe or on PATH"]
    fn decodes_a_generated_wav_through_real_ffmpeg() {
        let paths = find_ffmpeg(None).unwrap_or_else(|e| panic!("{e}"));
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("tiny.wav");
        let samples = [0i16, 16384, -16384, 32767, -32768, 8192, 0, -8192];
        std::fs::write(&wav, wav_16bit_mono(8000, &samples)).unwrap();

        let cancel = AtomicBool::new(false);
        let mut bytes = Vec::new();
        run_ffmpeg(&paths.ffmpeg, &pcm_decode_args(&wav), &cancel, |chunk| {
            bytes.extend_from_slice(chunk)
        })
        .unwrap();
        let decoded: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let expected: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();
        assert_eq!(decoded, expected);

        let info = probe(&paths, &wav, &cancel).unwrap();
        assert_eq!(info.streams[0].sample_rate, Some(8000));
        assert_eq!(info.streams[0].channels, Some(1));

        let err = run_ffmpeg(
            &paths.ffmpeg,
            &pcm_decode_args(&dir.path().join("missing.wav")),
            &cancel,
            |_| {},
        )
        .unwrap_err();
        assert!(
            matches!(&err, FfmpegError::Failed { stderr, .. } if stderr.contains("missing.wav")),
            "{err:?}"
        );

        let endless: Vec<OsString> = ["-nostdin", "-f", "lavfi", "-i", "sine", "-f", "f32le", "-"]
            .map(OsString::from)
            .to_vec();
        let err = run_ffmpeg(&paths.ffmpeg, &endless, &cancel, |_| {
            cancel.store(true, Ordering::Relaxed)
        })
        .unwrap_err();
        assert_eq!(err, FfmpegError::Cancelled);
    }
}
