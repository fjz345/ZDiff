#[path = "../zdiff-gui/build_icon.rs"]
mod build_icon;

fn main() {
    build_icon::embed_exe_icon("../zdiff-gui/img/icon.png");
}
