use std::{env, path::PathBuf};

#[path = "../zdiff-gui/build_icon.rs"]
mod build_icon;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    // OUT_DIR is <target dir>/<profile>/build/<pkg>-<hash>/out, next to where the binaries go.
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR is nested under the profile folder");

    let bin_name = if cfg!(windows) {
        "zdiff-gui.exe"
    } else {
        "zdiff-gui"
    };
    let bin_path = profile_dir.join(bin_name);

    // Warning if you forgot to build the workspace
    if !bin_path.exists() {
        println!(
            "cargo:warning=Sibling binary not found at {}. Did you run 'cargo build --workspace'?",
            bin_path.display()
        );
    }

    println!("cargo:rustc-env=ZDIFF_BIN_PATH={}", bin_path.display());

    // Watch the sibling crate for changes
    println!("cargo:rerun-if-changed=../zdiff-gui/src");
    println!("cargo:rerun-if-changed=../zdiff-gui/Cargo.toml");

    build_icon::embed_exe_icon("../zdiff-gui/img/icon.png");
}
