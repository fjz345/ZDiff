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

const DIST_PACKAGE: &str = "zdiff-gui";

const USAGE: &str = "usage: cargo xtask <fetch-ffmpeg|check-features|dist>";

fn main() -> ExitCode {
    let result = match env::args().nth(1).as_deref() {
        Some("fetch-ffmpeg") => fetch_ffmpeg(),
        Some("check-features") => check_features(),
        Some("dist") => dist(),
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
    fetch_ffmpeg_supported(env::consts::OS)?;
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

fn fetch_ffmpeg_supported(os: &str) -> Result<(), String> {
    if os == "windows" {
        return Ok(());
    }
    Err(format!(
        "fetch-ffmpeg downloads a Windows build. On {os}, install ffmpeg with the package manager \
         (apt install ffmpeg, dnf install ffmpeg, brew install ffmpeg); zdiff-gui finds ffmpeg and \
         ffprobe on PATH."
    ))
}

fn dist_ffmpeg_files(os: &str) -> Vec<&'static str> {
    if os != "windows" {
        return Vec::new();
    }
    FFMPEG_EXTRACT
        .iter()
        .map(|(_, name)| *name)
        .chain([FFMPEG_NOTE])
        .collect()
}

fn dist() -> Result<(), String> {
    let root = workspace_root();
    let ffmpeg_dir = root.join("ffmpeg");
    let ffmpeg_files = dist_ffmpeg_files(env::consts::OS);
    let missing: Vec<&str> = ffmpeg_files
        .iter()
        .copied()
        .filter(|name| !ffmpeg_dir.join(name).is_file())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} missing in {}. Run 'cargo xtask fetch-ffmpeg'.",
            missing.join(", "),
            ffmpeg_dir.display()
        ));
    }

    let version = workspace_packages()?
        .into_iter()
        .find(|package| package.name == DIST_PACKAGE)
        .ok_or(format!("cargo metadata: no package {DIST_PACKAGE}"))?
        .version;

    run(Command::new(cargo_program()).current_dir(root).args([
        "build",
        "--release",
        "-p",
        DIST_PACKAGE,
    ]))?;

    let out_dir = root
        .join("target")
        .join("dist")
        .join(format!("ZDiff_{version}"));
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir).map_err(|e| format!("remove {}: {e}", out_dir.display()))?;
    }
    fs::create_dir_all(&out_dir).map_err(|e| format!("create {}: {e}", out_dir.display()))?;

    let mut files = vec![
        root.join("target")
            .join("release")
            .join(format!("{DIST_PACKAGE}{}", env::consts::EXE_SUFFIX)),
        root.join("LICENSE"),
    ];
    files.extend(ffmpeg_files.iter().map(|name| ffmpeg_dir.join(name)));
    for src in &files {
        let dst = out_dir.join(src.file_name().unwrap());
        fs::copy(src, &dst)
            .map_err(|e| format!("copy {} to {}: {e}", src.display(), dst.display()))?;
    }
    println!("Release files in {}", out_dir.display());
    Ok(())
}

struct Package {
    name: String,
    version: String,
    features: Vec<String>,
}

fn feature_args(features: &[String]) -> Vec<Vec<String>> {
    let mut combos = vec![vec![], vec!["--no-default-features".to_string()]];
    for feature in features.iter().filter(|f| *f != "default") {
        combos.push(vec![
            "--no-default-features".to_string(),
            "--features".to_string(),
            feature.clone(),
        ]);
    }
    combos
}

fn cargo_program() -> std::ffi::OsString {
    env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

fn workspace_packages() -> Result<Vec<Package>, String> {
    let output = Command::new(cargo_program())
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("start cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("parse cargo metadata: {e}"))?;
    let packages = meta["packages"]
        .as_array()
        .ok_or("cargo metadata: missing packages")?;
    let mut result = Vec::new();
    for package in packages {
        let name = package["name"]
            .as_str()
            .ok_or("cargo metadata: package without name")?;
        if name == "xtask" {
            continue;
        }
        let mut features: Vec<String> = package["features"]
            .as_object()
            .ok_or("cargo metadata: package without features")?
            .keys()
            .cloned()
            .collect();
        features.sort();
        let version = package["version"]
            .as_str()
            .ok_or("cargo metadata: package without version")?;
        result.push(Package {
            name: name.to_string(),
            version: version.to_string(),
            features,
        });
    }
    Ok(result)
}

fn check_features() -> Result<(), String> {
    let mut failures = Vec::new();
    for package in workspace_packages()? {
        for extra in feature_args(&package.features) {
            let shown = format!(
                "cargo check -p {} --all-targets {}",
                package.name,
                extra.join(" ")
            );
            let shown = shown.trim_end();
            println!("> {shown}");
            let status = Command::new(cargo_program())
                .current_dir(workspace_root())
                .args(["check", "-p", &package.name, "--all-targets"])
                .args(&extra)
                .status();
            match status {
                Ok(status) if status.success() => {}
                Ok(status) => failures.push(format!("{shown} ({status})")),
                Err(e) => failures.push(format!("{shown} (start failed: {e})")),
            }
        }
    }
    if failures.is_empty() {
        println!("check-features: all combinations passed");
        return Ok(());
    }
    println!("check-features: {} failing combination(s):", failures.len());
    for failure in &failures {
        println!("  {failure}");
    }
    Err(format!("{} combination(s) failed", failures.len()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_args_cover_defaults_none_and_each_feature_alone() {
        let features = ["default".to_string(), "a".to_string(), "b".to_string()];
        let combos = feature_args(&features);
        assert_eq!(combos.len(), 4);
        assert!(combos[0].is_empty());
        assert_eq!(combos[1], ["--no-default-features"]);
        assert_eq!(combos[2], ["--no-default-features", "--features", "a"]);
        assert_eq!(combos[3], ["--no-default-features", "--features", "b"]);
    }

    #[test]
    fn fetch_ffmpeg_is_windows_only() {
        assert_eq!(fetch_ffmpeg_supported("windows"), Ok(()));
        for os in ["linux", "macos"] {
            let err = fetch_ffmpeg_supported(os).unwrap_err();
            assert!(err.contains("apt install ffmpeg"), "{err}");
            assert!(err.contains("brew install ffmpeg"), "{err}");
        }
    }

    #[test]
    fn dist_bundles_ffmpeg_only_on_windows() {
        assert_eq!(
            dist_ffmpeg_files("windows"),
            [
                "ffmpeg.exe",
                "ffprobe.exe",
                "ffmpeg-LICENSE.txt",
                "ffmpeg-SOURCE.txt"
            ]
        );
        assert!(dist_ffmpeg_files("linux").is_empty());
        assert!(dist_ffmpeg_files("macos").is_empty());
    }
}
