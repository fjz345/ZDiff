#![windows_subsystem = "windows"]

use std::{
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

use eframe::egui;
use zcommon::console::{LOG_FLAG, open_console, take_log_flag};

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
    let (log_to_console, _) = take_log_flag(std::env::args_os().skip(1));
    let console = log_to_console.then(open_console);
    #[cfg(feature = "pretty-backtrace")]
    {
        color_backtrace::install();
    }
    let crash_log = zcommon::crash::install_crash_log(eframe::storage_dir(APP_ID));
    zcommon::logger::init_logging().expect("Failed to init logger");
    if let Some(Err(e)) = console {
        log::error!("{LOG_FLAG}: cannot open a console: {e}");
    }
    match crash_log {
        Some(path) => log::info!("Panics are appended to {}", path.display()),
        None => log::error!("No folder for crash.log: panics are not recorded"),
    }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 1000.0])
            .with_drag_and_drop(true)
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../../img/icon.png"))
                    .expect("embedded icon.png decodes"),
            ),
        ..Default::default()
    };

    let exit_code = Arc::new(AtomicU8::new(0));
    let app_exit_code = exit_code.clone();
    let result = eframe::run_native(
        APP_ID,
        native_options,
        Box::new(move |cc: &eframe::CreationContext<'_>| {
            egui_extras::install_image_loaders(&cc.egui_ctx);

            #[cfg(feature = "serde")]
            {
                // Try to load saved state from storage
                if let Some(storage) = cc.storage {
                    if let Some(json) = storage.get_string(eframe::APP_KEY) {
                        match serde_json::from_str::<ZApp>(&json) {
                            Ok(mut app) => {
                                log::info!("Found previous app storage");
                                app.report_exit_code_to(app_exit_code);
                                app.remember_window_from(cc.storage);
                                app.request_init();
                                return Ok(Box::new(app));
                            }
                            Err(e) => {
                                log::error!("Saved app state is unreadable, using defaults: {e}")
                            }
                        }
                    }
                }
            }

            let mut app = ZApp::new();
            app.report_exit_code_to(app_exit_code);
            app.remember_window_from(cc.storage);
            app.request_init();
            Ok(Box::<ZApp>::new(app))
        }),
    );
    if let Err(e) = result {
        log::error!("{e}");
        return ExitCode::FAILURE;
    }
    ExitCode::from(exit_code.load(Ordering::Relaxed))
}
