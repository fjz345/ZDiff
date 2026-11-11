#![windows_subsystem = "windows"]

use std::{
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

use crate::app::ZApp;

mod app;
mod browser;
mod clamped_cursor;
mod cli;
mod diff_ctx;
mod diff_phases;
mod file;
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

fn main() -> ExitCode {
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
