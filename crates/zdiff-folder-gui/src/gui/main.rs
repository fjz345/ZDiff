#![windows_subsystem = "windows"]

use std::process::ExitCode;

fn main() -> ExitCode {
    let (_, folder_args) = zcommon::console::take_log_flag(std::env::args_os().skip(1));
    zdiff_folder_gui::run(folder_args)
}
