use std::{
    env,
    fmt::Write as _,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use sha2::{Digest, Sha256};

const FFMPEG_VERSION: &str = "n9.0.2-17-g2a571b6068";
const FFMPEG_URL: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/ffmpeg-n9.0.2-17-g2a571b6068-win64-lgpl-9.0.zip";
const FFMPEG_SHA256: &str = "6b264b9e6019103f601d98c292bd332fd87acf1c5e941ddff4fb71760fe63432";
const FFMPEG_ARCHIVE_TOP: &str = "ffmpeg-n9.0.2-17-g2a571b6068-win64-lgpl-9.0";
const FFMPEG_SOURCE_URL: &str =
    "https://github.com/FFmpeg/FFmpeg/commit/2a571b606854520cf89804d8030c8b328e621689";
const FFMPEG_BUILD_URL: &str =
    "https://github.com/BtbN/FFmpeg-Builds/tree/autobuild-2026-09-30-13-08";

// Must match FFMPEG_FILES in crates/zdiff-gui/build.rs.
const FFMPEG_EXTRACT: [(&str, &str); 3] = [
    ("bin/ffmpeg.exe", "ffmpeg.exe"),
    ("bin/ffprobe.exe", "ffprobe.exe"),
    ("LICENSE.txt", "ffmpeg-LICENSE.txt"),
];
const FFMPEG_NOTE: &str = "ffmpeg-SOURCE.txt";

const USAGE: &str = "usage: cargo xtask fetch-ffmpeg";

fn main() -> ExitCode {
    let result = match env::args().nth(1).as_deref() {
        Some("fetch-ffmpeg") => fetch_ffmpeg(),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
}

fn fetch_ffmpeg() -> Result<(), String> {
    let out_dir = workspace_root().join("ffmpeg");
    let download_dir = out_dir.join("download");
    if download_dir.exists() {
        fs::remove_dir_all(&download_dir)
            .map_err(|e| format!("remove {}: {e}", download_dir.display()))?;
    }
    fs::create_dir_all(&download_dir)
        .map_err(|e| format!("create {}: {e}", download_dir.display()))?;

    let archive = download_dir.join(format!("{FFMPEG_ARCHIVE_TOP}.zip"));
    println!("Downloading {FFMPEG_URL}");
    run(Command::new("curl")
        .args(["-L", "--fail", "--progress-bar", "-o"])
        .arg(&archive)
        .arg(FFMPEG_URL))?;

    let actual = sha256_file(&archive)?;
    if actual != FFMPEG_SHA256 {
        fs::remove_file(&archive).map_err(|e| format!("remove {}: {e}", archive.display()))?;
        return Err(format!(
            "SHA-256 mismatch for {FFMPEG_URL}\n  expected {FFMPEG_SHA256}\n  actual   {actual}\nThe download was deleted."
        ));
    }
    println!("SHA-256 ok");

    let members: Vec<String> = FFMPEG_EXTRACT
        .iter()
        .map(|(member, _)| format!("{FFMPEG_ARCHIVE_TOP}/{member}"))
        .collect();
    run(Command::new(bsdtar())
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&download_dir)
        .args(&members))?;

    for (member, name) in FFMPEG_EXTRACT {
        let from = download_dir.join(FFMPEG_ARCHIVE_TOP).join(member);
        let to = out_dir.join(name);
        fs::rename(&from, &to)
            .map_err(|e| format!("move {} to {}: {e}", from.display(), to.display()))?;
    }
    let note = format!(
        "ffmpeg.exe and ffprobe.exe are unmodified FFmpeg {FFMPEG_VERSION} builds, licensed under\r\n\
         the GNU LGPL v3 (see ffmpeg-LICENSE.txt). They are run as separate programs.\r\n\
         \r\n\
         Binaries:      {FFMPEG_URL}\r\n\
         FFmpeg source: {FFMPEG_SOURCE_URL}\r\n\
         Build scripts and dependency versions: {FFMPEG_BUILD_URL}\r\n"
    );
    let note_path = out_dir.join(FFMPEG_NOTE);
    fs::write(&note_path, note).map_err(|e| format!("write {}: {e}", note_path.display()))?;

    fs::remove_dir_all(&download_dir)
        .map_err(|e| format!("remove {}: {e}", download_dir.display()))?;
    println!(
        "Placed ffmpeg in {}. Build zdiff-gui to copy it next to the exe.",
        out_dir.display()
    );
    Ok(())
}

// Git for Windows puts GNU tar first on PATH, and GNU tar can't read zip.
fn bsdtar() -> PathBuf {
    match env::var_os("SystemRoot") {
        Some(root) => Path::new(&root).join("System32").join("tar.exe"),
        None => PathBuf::from("tar"),
    }
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let status = cmd
        .status()
        .map_err(|e| format!("start {:?}: {e}", cmd.get_program()))?;
    if !status.success() {
        return Err(format!("{:?} failed: {status}", cmd.get_program()));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(hex, "{byte:02x}").unwrap();
    }
    Ok(hex)
}
