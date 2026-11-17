#![cfg(feature = "gui")]

use std::{ffi::OsString, process::ExitCode};

use crate::ui_egui::{app::ZApp, roots::parse_launch_args};

mod ui_egui;

const APP_ID: &str = "ZDiffFolder";

pub fn run(folder_args: Vec<OsString>) -> ExitCode {
    let (_, log_buffer) = zcommon_ui::app::start_logging(APP_ID, || {
        #[cfg(feature = "pretty-backtrace")]
        color_backtrace::install();
    });
    let launch_args = parse_launch_args(folder_args);

    let result = zcommon_ui::app::run(
        APP_ID,
        include_bytes!("../../../zdiff-gui/img/icon.png"),
        move || ZApp::new(log_buffer),
        move |app, cc| {
            app.remember_window_from(cc.storage);
            app.request_init();
            app.apply_launch_args(&launch_args);
        },
    );
    if let Err(e) = result {
        log::error!("{e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
