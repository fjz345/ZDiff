use eframe::egui;

use super::{ZApp, state::AppStateCtx, ui::send_title};
use crate::{
    folder::{Mode, view::FolderShell},
    ui_egui::merge_pane::MergeView,
};

pub(super) fn shows_folder(state: &AppStateCtx, merge: &Option<MergeView>) -> bool {
    state.mode == Mode::Folder && merge.is_none()
}

impl ZApp {
    pub(super) fn folder_frame(&mut self, ctx: &egui::Context, _state: &mut AppStateCtx) -> bool {
        self.folder.frame(
            ctx,
            FolderShell {
                frame_stats: &mut self.frame_stats,
                show_frame_breakdown: &mut self.show_frame_breakdown,
                style_window: &mut self.style_window,
            },
        );
        send_title(ctx, &mut self.window_title, self.folder.title());
        // Read after the frame: a widget that consumed Esc (find popup, path bar) wins.
        ctx.input(|i| i.key_pressed(egui::Key::Escape))
    }

    #[cfg(test)]
    pub(crate) fn idle_showing_folder(folder: crate::folder::view::FolderView) -> ZApp {
        use super::state::AppState;

        let mut app = ZApp::new();
        app.folder = folder;
        app.state = Some(AppState::Idle(AppStateCtx {
            mode: Mode::Folder,
            ..Default::default()
        }));
        app
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use eframe::egui;
    use tempfile::tempdir;

    use super::super::{ZApp, state::AppState};
    use crate::folder::{Mode, view::FolderView};

    fn shape_texts(shape: &egui::Shape, texts: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(text) => texts.push(text.galley.text().to_owned()),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| shape_texts(s, texts)),
            _ => {}
        }
    }

    fn frame_texts(ctx: &egui::Context, app: &mut ZApp, events: Vec<egui::Event>) -> Vec<String> {
        let mut frame = eframe::Frame::_new_kittest();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 1000.0),
            )),
            events,
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| eframe::App::update(app, ctx, &mut frame));
        let mut texts = Vec::new();
        for clipped in &output.shapes {
            shape_texts(&clipped.shape, &mut texts);
        }
        texts
    }

    fn mode(app: &mut ZApp) -> Mode {
        app.state.as_mut().unwrap().ctx_mut().mode
    }

    #[test]
    fn two_folders_on_the_command_line_show_the_folder_tree_in_the_same_app() {
        let (left, right) = (tempdir().unwrap(), tempdir().unwrap());
        std::fs::write(left.path().join("only_left.txt"), "left").unwrap();
        std::fs::write(right.path().join("only_right.txt"), "right").unwrap();
        let mut app = ZApp::new();
        app.launch(vec![left.path().into(), right.path().into()]);

        let ctx = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let texts = frame_texts(&ctx, &mut app, Vec::new());
            let painted = |name: &str| texts.iter().any(|text| text.contains(name));
            if painted("only_left.txt") && painted("only_right.txt") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the folder tree never showed: {texts:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(mode(&mut app), Mode::Folder);
        let title = app.window_title.as_deref().unwrap();
        assert!(title.starts_with("zdiff ["), "{title}");
    }

    #[test]
    fn esc_closes_the_app_in_the_folder_mode_only() {
        let escape = || {
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]
        };
        let ctx = egui::Context::default();
        let mut app = ZApp::idle_showing_folder(FolderView::default());
        frame_texts(&ctx, &mut app, escape());
        assert!(matches!(app.state, Some(AppState::Exit(_))));

        let ctx = egui::Context::default();
        let mut app = ZApp::idle_showing_folder(FolderView::default());
        app.state.as_mut().unwrap().ctx_mut().mode = Mode::File;
        frame_texts(&ctx, &mut app, escape());
        assert!(matches!(app.state, Some(AppState::Idle(_))));
    }
}
