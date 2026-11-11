use std::ffi::OsString;

use eframe::egui;
use serde::{Deserialize, de::DeserializeOwned};
use winit::event_loop::{DeviceEvents, EventLoop};
use zcommon::{
    console::{LOG_FLAG, open_console, take_log_flag},
    logger::{LogBuffer, init_logging},
};

pub fn start_logging(app_id: &str, install_backtrace: impl FnOnce()) -> (Vec<OsString>, LogBuffer) {
    let (log_to_console, args) = take_log_flag(std::env::args_os().skip(1));
    let console = log_to_console.then(open_console);
    // After the console opens (its stderr picks the colors), before the crash log chains the hook.
    install_backtrace();
    let crash_log = zcommon::crash::install_crash_log(eframe::storage_dir(app_id));
    let log_buffer = init_logging().expect("Failed to init logger");
    if let Some(Err(e)) = console {
        log::error!("{LOG_FLAG}: cannot open a console: {e}");
    }
    match crash_log {
        Some(path) => log::info!("Panics are appended to {}", path.display()),
        None => log::error!("No folder for crash.log: panics are not recorded"),
    }
    (args, log_buffer)
}

pub fn run<A>(
    app_id: &str,
    icon_png: &[u8],
    new_app: impl FnOnce() -> A + 'static,
    init: impl FnOnce(&mut A, &eframe::CreationContext<'_>) + 'static,
) -> eframe::Result
where
    A: eframe::App + DeserializeOwned + 'static,
{
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 1000.0])
            .with_drag_and_drop(true)
            .with_icon(
                eframe::icon_data::from_png_bytes(icon_png).expect("embedded icon.png decodes"),
            ),
        ..Default::default()
    };
    let event_loop = EventLoop::<eframe::UserEvent>::with_user_event().build()?;
    // eframe repaints the focused window on every raw mouse delta, even with the cursor outside it.
    event_loop.listen_device_events(DeviceEvents::Never);
    let mut app = eframe::create_native(
        app_id,
        native_options,
        Box::new(move |cc: &eframe::CreationContext<'_>| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            let mut app = restored(cc.storage).unwrap_or_else(new_app);
            init(&mut app, cc);
            Ok(Box::new(app))
        }),
        &event_loop,
    );
    event_loop.run_app(&mut app)?;
    Ok(())
}

pub fn default_zoom() -> f32 {
    1.0
}

fn clamp_zoom(zoom: f32) -> f32 {
    zoom.clamp(0.5, 3.0)
}

pub fn deserialize_zoom<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<f32, D::Error> {
    f32::deserialize(deserializer).map(clamp_zoom)
}

pub fn sync_zoom(ctx: &egui::Context) -> f32 {
    let zoom = clamp_zoom(ctx.zoom_factor());
    if zoom != ctx.zoom_factor() {
        ctx.set_zoom_factor(zoom);
    }
    zoom
}

fn restored<A: DeserializeOwned>(storage: Option<&dyn eframe::Storage>) -> Option<A> {
    let json = storage?.get_string(eframe::APP_KEY)?;
    match serde_json::from_str(&json) {
        Ok(app) => {
            log::info!("Found previous app storage");
            Some(app)
        }
        Err(e) => {
            log::error!("Saved app state is unreadable, using defaults: {e}");
            None
        }
    }
}
