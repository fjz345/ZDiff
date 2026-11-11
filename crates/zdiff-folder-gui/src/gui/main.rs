#![windows_subsystem = "windows"]
// #![allow(dead_code)]
// #![allow(unreachable_patterns)]

use eframe::egui::{self};
use zcommon::{
    console::{LOG_FLAG, open_console, take_log_flag},
    logger::init_logging,
};

use crate::ui_egui::{app::ZApp, roots::parse_launch_args};

mod ui_egui;

const APP_ID: &str = "ZDiffFolder";

fn main() -> eframe::Result {
    let (log_to_console, args) = take_log_flag(std::env::args_os().skip(1));
    let console = log_to_console.then(open_console);
    #[cfg(feature = "pretty-backtrace")]
    {
        color_backtrace::install();
    }
    let crash_log = zcommon::crash::install_crash_log(eframe::storage_dir(APP_ID));

    let log_buffer = init_logging().expect("Failed to init logger");
    if let Some(Err(e)) = console {
        log::error!("{LOG_FLAG}: cannot open a console: {e}");
    }
    match crash_log {
        Some(path) => log::info!("Panics are appended to {}", path.display()),
        None => log::error!("No folder for crash.log: panics are not recorded"),
    }
    let launch_args = parse_launch_args(args);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 1000.0])
            .with_drag_and_drop(true)
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!(
                    "../../../zdiff-gui/img/icon.png"
                ))
                .expect("embedded icon.png decodes"),
            ),
        ..Default::default()
    };

    eframe::run_native(
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
                                app.request_init();
                                app.apply_launch_args(&launch_args);
                                return Ok(Box::new(app));
                            }
                            Err(e) => {
                                log::error!("Saved app state is unreadable, using defaults: {e}")
                            }
                        }
                    }
                }
            }

            let mut app = ZApp::new(log_buffer.clone());
            app.request_init();
            app.apply_launch_args(&launch_args);
            Ok(Box::<ZApp>::new(app))
        }),
    )
}
