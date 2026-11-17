#![windows_subsystem = "windows"]

use std::process::ExitCode;

fn main() -> ExitCode {
    zdiff_gui::run()
}
