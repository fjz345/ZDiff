pub mod duplicate_files_pane;
pub mod export;
pub mod folder_keymap;
pub mod fs_tree;
pub mod panes;
pub mod path_diff_pane;
pub mod revision_open;
pub mod roots;
pub mod side_revision;
pub mod status_line;
pub mod tools_window;
pub mod tree_compare;
pub mod tree_cursor;
pub mod tree_filter;
pub mod tree_find;
pub mod tree_guides;
pub mod tree_likeness;
pub mod tree_map;
pub mod tree_menu;
pub mod tree_sort;
pub mod tree_tolerance;
pub mod view;

use eframe::egui::{self, PointerButton};
use serde::{Deserialize, Serialize};
use zcommon_ui::{
    app::{default_zoom, deserialize_zoom, sync_zoom},
    frame_stats::FrameStats,
    shared_keymap::load_shared,
    style_window::StyleWindow,
    window_memory::WindowMemory,
};

use crate::folder::{
    roots::LaunchArgs,
    view::{FolderShell, FolderView},
};

#[derive(Default)]
enum Phase {
    #[default]
    Startup,
    Idle,
    Exit,
}

#[derive(Serialize, Deserialize)]
pub struct ZApp {
    #[serde(default = "default_zoom", deserialize_with = "deserialize_zoom")]
    zoom: f32,
    #[serde(skip)]
    phase: Phase,
    folder: FolderView,
    #[serde(skip)]
    style_window: StyleWindow,
    #[serde(skip)]
    title: String,
    #[serde(skip)]
    window_memory: WindowMemory,
    #[serde(skip)]
    frame_stats: FrameStats,
    #[serde(default)]
    show_frame_breakdown: bool,
}

impl ZApp {
    pub fn remember_window_from(&mut self, storage: Option<&dyn eframe::Storage>) {
        self.window_memory = WindowMemory::load(storage);
    }

    pub fn request_init(&mut self) {
        self.phase = Phase::Startup;
    }

    pub fn apply_launch_args(&mut self, args: &LaunchArgs) {
        self.folder.apply_launch_args(args);
    }

    pub fn new() -> Self {
        Self {
            zoom: default_zoom(),
            phase: Phase::default(),
            folder: FolderView::default(),
            style_window: StyleWindow::default(),
            title: String::new(),
            window_memory: WindowMemory::default(),
            frame_stats: FrameStats::default(),
            show_frame_breakdown: false,
        }
    }

    fn startup(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.style_window.load(ctx);
        log::debug!("pixels_per_point {}", ctx.pixels_per_point());
        log::debug!(
            "native_pixels_per_point {:?}",
            ctx.native_pixels_per_point()
        );
        ctx.set_zoom_factor(self.zoom);
        // ctx.set_debug_on_hover(true);
    }

    fn request_shutdown(&mut self) {
        self.phase = Phase::Exit;
    }

    fn process_ctx_inputs(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut user_quit: bool = false;
        {
            ctx.input(|r| {
                // Esc, unless a widget consumed it (the path bar's cancel)
                if r.key_pressed(egui::Key::Escape) {
                    user_quit = true;
                }

                // DoubleLeftClick
                if r.pointer.button_double_clicked(PointerButton::Primary)
                    && let Some(mouse_pos) = r.pointer.interact_pos()
                {
                    log::info!("double click @({},{})", mouse_pos.x, mouse_pos.y);
                }

                if r.pointer.button_clicked(PointerButton::Middle)
                    && let Some(mouse_pos) = r.pointer.interact_pos()
                {
                    log::info!("middle click @({},{})", mouse_pos.x, mouse_pos.y);
                }
            });
        }

        if user_quit {
            self.request_shutdown();
        }
    }
}

impl eframe::App for ZApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.window_memory.keep_restorable(storage);
        #[cfg(feature = "serde")]
        if let Ok(json) = serde_json::to_string(self) {
            storage.set_string(eframe::APP_KEY, json);
            log::debug!("App state saved");
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        view::on_exit();
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.frame_stats.begin_frame(ctx, frame);
        match self.phase {
            Phase::Startup => {
                self.startup(ctx, frame);
                load_shared(self.folder.keymap_mut());
                log::info!("STARTUP COMPLETE, TRANSITIONING TO IDLE");
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label("Loading...");
                    });
                });
                ctx.request_repaint();
                self.phase = Phase::Idle;
            }
            Phase::Idle => {
                self.zoom = sync_zoom(ctx);
                if ctx.input(|i| i.events.contains(&egui::Event::WindowFocused(true))) {
                    load_shared(self.folder.keymap_mut());
                }
                self.folder.frame(
                    ctx,
                    FolderShell {
                        frame_stats: &mut self.frame_stats,
                        show_frame_breakdown: &mut self.show_frame_breakdown,
                        style_window: &mut self.style_window,
                    },
                );
                let new_title = self.folder.title();
                // Every viewport command requests a repaint, so an unchanged title
                // would keep the app redrawing while idle.
                if new_title != self.title {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(new_title.clone()));
                    self.title = new_title;
                }
                self.process_ctx_inputs(ctx, frame);
                self.frame_stats.mark("Esc and click handling");
            }
            Phase::Exit => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                log::trace!("send_viewport_cmd sent");

                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label("Exiting...");
                    });
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Phase, ZApp};

    #[test]
    fn zoom_survives_a_restart() {
        let mut app = ZApp::new();
        app.zoom = 1.7;
        let json = serde_json::to_string(&app).unwrap();
        let restored: ZApp = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.zoom, 1.7);
    }

    #[test]
    fn a_saved_zoom_out_of_range_loads_clamped() {
        let mut json = serde_json::to_value(ZApp::new()).unwrap();
        json["zoom"] = 10.0.into();
        assert_eq!(
            serde_json::from_value::<ZApp>(json.clone()).unwrap().zoom,
            3.0
        );
        json["zoom"] = 0.1.into();
        assert_eq!(serde_json::from_value::<ZApp>(json).unwrap().zoom, 0.5);
    }

    #[test]
    fn a_save_without_a_zoom_loads_at_100_percent() {
        let mut json = serde_json::to_value(ZApp::new()).unwrap();
        json.as_object_mut().unwrap().remove("zoom").unwrap();
        let restored: ZApp = serde_json::from_value(json).unwrap();
        assert_eq!(restored.zoom, 1.0);
    }

    #[derive(Default)]
    struct MemoryStorage(std::collections::HashMap<String, String>);

    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }

        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }

        fn flush(&mut self) {}
    }

    #[test]
    fn a_window_minimized_at_exit_is_saved_as_it_was_at_startup() {
        const RESTORED: &str = "(inner_position_pixels:Some((x:169.0,y:262.0)),outer_position_pixels:Some((x:160.0,y:224.0)),fullscreen:false,maximized:false,inner_size_points:Some((x:1508.8,y:704.8)))";
        const MINIMIZED: &str = "(inner_position_pixels:Some((x:-32000.0,y:-32000.0)),outer_position_pixels:Some((x:-32000.0,y:-32000.0)),fullscreen:false,maximized:false,inner_size_points:Some((x:0.0,y:0.0)))";
        let mut storage = MemoryStorage::default();
        storage.0.insert("window".to_owned(), RESTORED.to_owned());
        let mut app = ZApp::new();
        app.remember_window_from(Some(&storage));

        storage.0.insert("window".to_owned(), MINIMIZED.to_owned());
        eframe::App::save(&mut app, &mut storage);

        assert_eq!(storage.0["window"], RESTORED);
    }

    #[test]
    fn a_restored_idle_app_runs_startup_again() {
        let mut app = ZApp::new();
        app.phase = Phase::Idle;
        app.request_init();
        assert!(matches!(app.phase, Phase::Startup));
    }
}
