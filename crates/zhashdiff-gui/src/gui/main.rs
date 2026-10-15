#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// #![allow(dead_code)]
// #![allow(unreachable_patterns)]

use eframe::egui::{self};
use zcommon::logger::init_logging;

use crate::ui_egui::{app::ZApp, roots::parse_launch_args};

mod ui_egui;

fn main() -> eframe::Result {
    #[cfg(feature = "pretty-backtrace")]
    {
        color_backtrace::install();
    }

    let log_buffer = init_logging().expect("Failed to init logger");
    let launch_args = parse_launch_args(std::env::args_os().skip(1));

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
        "ZHashDiff",
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
