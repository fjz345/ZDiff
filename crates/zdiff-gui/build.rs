mod build_icon;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    build_icon::embed_exe_icon("img/icon.png");
}
