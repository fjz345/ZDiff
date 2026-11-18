#![cfg(feature = "gui")]

use std::{
    ffi::OsString,
    path::Path,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

use crate::{
    app::ZApp,
    cli::LaunchMode,
    folder::{app::ZApp as FolderZApp, roots::parse_launch_args},
};

mod app;
mod browser;
mod clamped_cursor;
mod cli;
mod diff_ctx;
mod diff_phases;
mod file;
mod folder;
mod keybindings;
mod language_icon;
mod p4;
mod pdb_dump;
mod phase_trace_window;
mod revert;
mod scope;
mod toggle_all;
pub mod ui_egui;
mod viewer;

#[cfg(feature = "debug_alloc")]
use stats_alloc::INSTRUMENTED_SYSTEM;
#[cfg(feature = "debug_alloc")]
use stats_alloc::StatsAlloc;
#[cfg(feature = "debug_alloc")]
use std::alloc::System;
#[cfg(feature = "debug_alloc")]
#[global_allocator]
pub static STATS_ALLOC: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const APP_ID: &str = "ZDiff";
const FOLDER_APP_ID: &str = "ZDiffFolder";

pub fn run() -> ExitCode {
    let (_, args) = zcommon::console::take_log_flag(std::env::args_os().skip(1));
    match cli::launch_mode(&args, Path::is_dir) {
        LaunchMode::Folders(folder_args) => run_folder_diff(folder_args),
        LaunchMode::Files => run_file_diff(),
    }
}

fn run_file_diff() -> ExitCode {
    let _ = zcommon_ui::app::start_logging(APP_ID, || {
        #[cfg(feature = "pretty-backtrace")]
        color_backtrace::install();
    });

    let exit_code = Arc::new(AtomicU8::new(0));
    let app_exit_code = exit_code.clone();
    let result = zcommon_ui::app::run(
        APP_ID,
        include_bytes!("../../img/icon.png"),
        ZApp::new,
        move |app, cc| {
            app.report_exit_code_to(app_exit_code);
            app.remember_window_from(cc.storage);
            app.request_init();
        },
    );
    if let Err(e) = result {
        log::error!("{e}");
        return ExitCode::FAILURE;
    }
    ExitCode::from(exit_code.load(Ordering::Relaxed))
}

fn run_folder_diff(folder_args: Vec<OsString>) -> ExitCode {
    let (_, log_buffer) = zcommon_ui::app::start_logging(FOLDER_APP_ID, || {
        #[cfg(feature = "pretty-backtrace")]
        color_backtrace::install();
    });
    let launch_args = parse_launch_args(folder_args);

    let result = zcommon_ui::app::run(
        FOLDER_APP_ID,
        include_bytes!("../../img/icon.png"),
        move || FolderZApp::new(log_buffer),
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
