use std::{env, fs, path::PathBuf};

mod build_icon;

// Placed by `cargo xtask fetch-ffmpeg` (xtask/src/main.rs).
const FFMPEG_FILES: [&str; 4] = [
    "ffmpeg.exe",
    "ffprobe.exe",
    "ffmpeg-LICENSE.txt",
    "ffmpeg-SOURCE.txt",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    build_icon::embed_exe_icon("img/icon.png");
    copy_ffmpeg();
}

fn copy_ffmpeg() {
    // Other targets use the ffmpeg on PATH (zcommon::ffmpeg::find_ffmpeg).
    if env::var("CARGO_CFG_TARGET_OS").unwrap() != "windows" {
        return;
    }
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();
    let src_dir = workspace_root.join("ffmpeg");
    // The folder is tracked (README.md): a missing watched path would rerun this on every build.
    println!("cargo:rerun-if-changed={}", src_dir.display());

    let missing: Vec<&str> = FFMPEG_FILES
        .into_iter()
        .filter(|name| !src_dir.join(name).is_file())
        .collect();
    if !missing.is_empty() {
        println!(
            "cargo:warning=ffmpeg not packaged: {} missing in {}. Run 'cargo xtask fetch-ffmpeg'.",
            missing.join(", "),
            src_dir.display()
        );
        return;
    }

    let out_folder = if env::var("PROFILE").unwrap() == "debug" {
        "debug"
    } else {
        "release"
    };
    let bin_dir = workspace_root.join("target").join(out_folder);
    fs::create_dir_all(&bin_dir).unwrap_or_else(|e| panic!("create {}: {e}", bin_dir.display()));
    for name in FFMPEG_FILES {
        let src = src_dir.join(name);
        let dst = bin_dir.join(name);
        let src_meta = fs::metadata(&src).unwrap_or_else(|e| panic!("stat {}: {e}", src.display()));
        // fs::copy keeps the source mtime, so equal len and mtime means already copied.
        let up_to_date = fs::metadata(&dst).is_ok_and(|dst_meta| {
            dst_meta.len() == src_meta.len() && dst_meta.modified().ok() == src_meta.modified().ok()
        });
        if !up_to_date {
            fs::copy(&src, &dst)
                .unwrap_or_else(|e| panic!("copy {} to {}: {e}", src.display(), dst.display()));
        }
    }
}
