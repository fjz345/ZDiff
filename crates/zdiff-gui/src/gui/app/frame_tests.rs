use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use eframe::egui;
use tempfile::{TempDir, tempdir};
use zdiff::universal_path::UniversalPath;

use super::{
    ZApp, folder_mode,
    state::{AppState, AppStateCtx},
};
use crate::{
    ui_egui::text_mode::TextMode,
    viewer::{ViewerKind, Viewers, hex_strings::HexMode},
};

const FRAME_AT_60_HZ: Duration = Duration::from_micros(16_667);
const SCREEN: egui::Vec2 = egui::vec2(1600.0, 1000.0);

fn text_pair(dir: &Path) -> [PathBuf; 2] {
    let line = |i: usize, changed: bool| {
        let (ty, add) = if changed { ("i64", 2) } else { ("i32", 1) };
        format!("fn function_{i}(x: {ty}) -> {ty} {{ let y = x * {i}; y + {add} }}\n")
    };
    let (mut source, mut target) = (String::new(), String::new());
    for i in 0..30_000 {
        source.push_str(&line(i, false));
        target.push_str(&line(i, i % 30 == 0));
    }
    write_pair(dir, "txt", source.as_bytes(), target.as_bytes())
}

fn strings_pair(dir: &Path) -> [PathBuf; 2] {
    let string = |i: usize, changed: bool| {
        let word = if changed { "NUMBER" } else { "number" };
        format!("string {word} {i} here\0\0\x01\x02").into_bytes()
    };
    let (mut source, mut target) = (Vec::new(), Vec::new());
    for i in 0..20_000 {
        source.extend(string(i, false));
        target.extend(string(i, i % 25 == 0));
    }
    write_pair(dir, "bin", &source, &target)
}

fn write_pair(dir: &Path, ext: &str, source: &[u8], target: &[u8]) -> [PathBuf; 2] {
    let paths = [dir.join(format!("a.{ext}")), dir.join(format!("b.{ext}"))];
    std::fs::write(&paths[0], source).unwrap();
    std::fs::write(&paths[1], target).unwrap();
    paths
}

fn run_frame(
    ctx: &egui::Context,
    app: &mut ZApp,
    width: f32,
    events: Vec<egui::Event>,
) -> (Duration, egui::FullOutput) {
    let mut frame = eframe::Frame::_new_kittest();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(width, SCREEN.y),
        )),
        events,
        ..Default::default()
    };
    let start = Instant::now();
    let mut output = ctx.run(input, |ctx| eframe::App::update(app, ctx, &mut frame));
    let _ = ctx.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
    (start.elapsed(), output)
}

fn repaint_delay(output: &egui::FullOutput) -> Duration {
    output.viewport_output[&egui::ViewportId::ROOT].repaint_delay
}

fn loaded_app(paths: &[PathBuf; 2], setup: impl FnOnce(&mut AppStateCtx)) -> (egui::Context, ZApp) {
    let mut app = ZApp::new();
    let state = app.state.as_mut().unwrap().ctx_mut();
    state.file_1.set_path(UniversalPath::from(paths[0].clone()));
    state.file_2.set_path(UniversalPath::from(paths[1].clone()));
    setup(state);
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        run_frame(&ctx, &mut app, SCREEN.x, Vec::new());
        let diff = &mut app.state.as_mut().unwrap().ctx_mut().diff_processor;
        if diff.get_minimal_diff_ctx().is_some() && diff.in_progress_input.is_none() {
            break;
        }
        assert!(Instant::now() < deadline, "the diff never finished");
        std::thread::sleep(Duration::from_millis(20));
    }
    for _ in 0..10 {
        run_frame(&ctx, &mut app, SCREEN.x, Vec::new());
    }
    (ctx, app)
}

fn median_frame(
    ctx: &egui::Context,
    app: &mut ZApp,
    frames: usize,
    mut width_and_events: impl FnMut(usize) -> (f32, Vec<egui::Event>),
) -> Duration {
    let mut times: Vec<Duration> = (0..frames)
        .map(|i| {
            let (width, events) = width_and_events(i);
            run_frame(ctx, app, width, events).0
        })
        .collect();
    times.sort();
    times[frames / 2]
}

fn pointer_over_rows(i: usize) -> (f32, Vec<egui::Event>) {
    let pos = egui::pos2(300.0 + i as f32 * 23.0, 300.0 + i as f32 * 9.0);
    (SCREEN.x, vec![egui::Event::PointerMoved(pos)])
}

fn assert_idle_and_pointer_moved_frames_fit(ctx: &egui::Context, app: &mut ZApp) {
    let idle = median_frame(ctx, app, 31, |_| (SCREEN.x, Vec::new()));
    let pointer_moved = median_frame(ctx, app, 31, pointer_over_rows);

    assert!(idle < FRAME_AT_60_HZ, "idle frame took {idle:?}");
    assert!(
        pointer_moved < FRAME_AT_60_HZ,
        "pointer-moved frame took {pointer_moved:?}"
    );
}

#[test]
fn idle_and_pointer_moved_frames_of_a_30k_line_diff_fit_a_60_hz_frame() {
    let dir = tempdir().unwrap();
    let (ctx, mut app) = loaded_app(&text_pair(dir.path()), |_| {});

    assert_idle_and_pointer_moved_frames_fit(&ctx, &mut app);
}

#[test]
fn idle_and_pointer_moved_frames_of_a_20k_string_strings_diff_fit_a_60_hz_frame() {
    let dir = tempdir().unwrap();
    let (ctx, mut app) = loaded_app(&strings_pair(dir.path()), |state| {
        state.hex_settings.mode = HexMode::Strings;
    });

    assert_idle_and_pointer_moved_frames_fit(&ctx, &mut app);
}

#[test]
fn idle_and_pointer_moved_frames_of_a_30k_line_patch_fit_a_60_hz_frame() {
    let dir = tempdir().unwrap();
    let (ctx, mut app) = loaded_app(&text_pair(dir.path()), |state| {
        state.text_mode = TextMode::Patch;
    });

    assert_idle_and_pointer_moved_frames_fit(&ctx, &mut app);
}

fn assert_resizes_fit(paths: &[PathBuf; 2], strings: bool) {
    let (ctx, mut app) = loaded_app(paths, |state| {
        state.word_wrap = true;
        if strings {
            state.hex_settings.mode = HexMode::Strings;
        }
    });

    let resizing = median_frame(&ctx, &mut app, 21, |i| {
        (SCREEN.x + (i % 2) as f32 * 7.0, Vec::new())
    });

    assert!(resizing < FRAME_AT_60_HZ, "resized frame took {resizing:?}");
}

#[test]
fn resizing_a_wrapped_30k_line_diff_fits_a_60_hz_frame() {
    let dir = tempdir().unwrap();
    assert_resizes_fit(&text_pair(dir.path()), false);
}

#[test]
fn resizing_a_wrapped_20k_string_strings_diff_fits_a_60_hz_frame() {
    let dir = tempdir().unwrap();
    assert_resizes_fit(&strings_pair(dir.path()), true);
}

fn repaint_delay_after_pointer_motion(
    ctx: &egui::Context,
    app: &mut ZApp,
    at: egui::Pos2,
) -> Duration {
    for step in [egui::vec2(0.0, 0.0), egui::vec2(2.0, 1.0)] {
        run_frame(
            ctx,
            app,
            SCREEN.x,
            vec![egui::Event::PointerMoved(at + step)],
        );
    }
    for _ in 0..20 {
        run_frame(ctx, app, SCREEN.x, Vec::new());
    }
    repaint_delay(&run_frame(ctx, app, SCREEN.x, Vec::new()).1)
}

#[test]
fn an_idle_diff_with_the_frame_time_breakdown_open_stops_repainting() {
    let dir = tempdir().unwrap();
    let pair = write_pair(dir.path(), "txt", b"a\nb\n", b"a\nc\n");
    let (ctx, mut app) = loaded_app(&pair, |_| {});
    app.show_frame_breakdown = true;

    assert_eq!(
        repaint_delay_after_pointer_motion(&ctx, &mut app, egui::pos2(400.0, 400.0)),
        Duration::MAX
    );
}

#[test]
fn the_fps_label_turning_idle_costs_one_frame() {
    let dir = tempdir().unwrap();
    let pair = write_pair(dir.path(), "txt", b"a\nb\n", b"a\nc\n");
    let (ctx, mut app) = loaded_app(&pair, |_| {});
    app.show_frame_breakdown = true;
    repaint_delay_after_pointer_motion(&ctx, &mut app, egui::pos2(400.0, 400.0));

    std::thread::sleep(Duration::from_millis(1300));
    assert!(ctx.has_requested_repaint());
    let idle_frame = repaint_delay(&run_frame(&ctx, &mut app, SCREEN.x, Vec::new()).1);
    std::thread::sleep(Duration::from_millis(1300));

    assert_eq!(idle_frame, Duration::MAX);
    assert!(!ctx.has_requested_repaint());
}

fn mouse_button_click(button: egui::PointerButton) -> Vec<egui::Event> {
    [true, false]
        .map(|pressed| egui::Event::PointerButton {
            pos: egui::Pos2::ZERO,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        })
        .to_vec()
}

#[test]
fn the_mouse_back_and_forward_buttons_close_and_reopen_a_folder_view() {
    let dir = tempdir().unwrap();
    let pair = write_pair(dir.path(), "txt", b"a\nb\n", b"a\nc\n");
    let (ctx, mut app) = loaded_app(&pair, |_| {});
    let folder = dir.path().to_string_lossy().into_owned();
    let shown_after =|app: &mut ZApp, events: Vec<egui::Event>| {
        run_frame(&ctx, app, SCREEN.x, events);
        run_frame(&ctx, app, SCREEN.x, Vec::new());
        let state = app.state.as_mut().unwrap().ctx_mut();
        state.browser_folders.clone()
    };

    app.state.as_mut().unwrap().ctx_mut().browser_folders[0] = Some(folder.clone());
    assert_eq!(
        shown_after(&mut app, Vec::new()),
        [Some(folder.clone()), None]
    );
    assert_eq!(
        shown_after(&mut app, mouse_button_click(egui::PointerButton::Extra1)),
        [None, None]
    );
    assert_eq!(
        shown_after(&mut app, mouse_button_click(egui::PointerButton::Extra2)),
        [Some(folder), None]
    );
}

#[test]
fn an_idle_text_diff_stops_repainting_after_pointer_motion() {
    let dir = tempdir().unwrap();
    let (ctx, mut app) = loaded_app(&text_pair(dir.path()), |_| {});

    for (place, at) in [
        ("menu bar", egui::pos2(30.0, 8.0)),
        ("path bar", egui::pos2(300.0, 90.0)),
        ("left text row", egui::pos2(400.0, 400.0)),
        ("center column", egui::pos2(SCREEN.x / 2.0, 400.0)),
        ("right text row", egui::pos2(1200.0, 400.0)),
    ] {
        assert_eq!(
            repaint_delay_after_pointer_motion(&ctx, &mut app, at),
            Duration::MAX,
            "kept repainting with the pointer over the {place}"
        );
    }
}

fn frame_texts(ctx: &egui::Context, app: &mut ZApp, events: Vec<egui::Event>) -> Vec<String> {
    fn shape_texts(shape: &egui::Shape, texts: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(text) => texts.push(text.galley.text().to_owned()),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| shape_texts(s, texts)),
            _ => {}
        }
    }
    let mut frame = eframe::Frame::_new_kittest();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
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

const FOLDER_NAMES: [&str; 2] = ["only_left.txt", "only_right.txt"];

fn folder_dirs() -> [TempDir; 2] {
    let dirs = [tempdir().unwrap(), tempdir().unwrap()];
    for (dir, name) in dirs.iter().zip(FOLDER_NAMES) {
        std::fs::write(dir.path().join(name), name).unwrap();
    }
    dirs
}

fn set_sides(app: &mut ZApp, sides: [&Path; 2]) {
    let state = app.state.as_mut().unwrap().ctx_mut();
    for (file, side) in [&mut state.file_1, &mut state.file_2]
        .into_iter()
        .zip(sides)
    {
        file.restore_path(UniversalPath::from(side), None);
    }
}

fn sides(app: &mut ZApp) -> [UniversalPath; 2] {
    let state = app.state.as_mut().unwrap().ctx_mut();
    [state.file_1.get_full_path(), state.file_2.get_full_path()]
}

fn viewers(app: &mut ZApp) -> Option<Viewers> {
    app.state.as_mut().unwrap().ctx_mut().viewers
}

fn paints_folder_names(texts: &[String]) -> bool {
    FOLDER_NAMES
        .iter()
        .all(|name| texts.iter().any(|text| text.contains(name)))
}

fn frames_until(
    ctx: &egui::Context,
    app: &mut ZApp,
    what: &str,
    done: impl Fn(&mut ZApp, &[String]) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let texts = frame_texts(ctx, app, Vec::new());
        if done(app, &texts) {
            return;
        }
        assert!(Instant::now() < deadline, "never {what}: {texts:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn frames_until_the_folder_tree_shows(ctx: &egui::Context, app: &mut ZApp) {
    frames_until(ctx, app, "showed the folder tree", |_, texts| {
        paints_folder_names(texts)
    });
}

fn frames_until_the_text_diff_shows(ctx: &egui::Context, app: &mut ZApp) {
    frames_until(ctx, app, "showed the text diff", |app, _| {
        let state = app.state.as_mut().unwrap().ctx_mut();
        state.viewers == Some(Viewers::Pair(ViewerKind::Text))
            && state.diff_processor.get_minimal_diff_ctx().is_some()
    });
}

fn shown_folder_app(dirs: &[TempDir; 2]) -> (egui::Context, ZApp) {
    let mut app = ZApp::new();
    set_sides(&mut app, [dirs[0].path(), dirs[1].path()]);
    let ctx = egui::Context::default();
    frames_until_the_folder_tree_shows(&ctx, &mut app);
    (ctx, app)
}

fn key_press(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    vec![egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }]
}

fn ctrl_f() -> Vec<egui::Event> {
    let ctrl = egui::Modifiers {
        ctrl: true,
        command: true,
        ..egui::Modifiers::NONE
    };
    key_press(egui::Key::F, ctrl)
}

fn is_idle(app: &ZApp) -> bool {
    matches!(app.state, Some(AppState::Idle(_)))
}

#[test]
fn two_folders_on_the_command_line_show_the_folder_tree() {
    let dirs = folder_dirs();
    let mut app = ZApp::new();
    app.launch(vec![dirs[0].path().into(), dirs[1].path().into()]);

    let ctx = egui::Context::default();
    frames_until_the_folder_tree_shows(&ctx, &mut app);

    assert_eq!(viewers(&mut app), Some(Viewers::Folders));
    assert_eq!(
        sides(&mut app),
        [dirs[0].path(), dirs[1].path()].map(UniversalPath::from)
    );
    let title = app.window_title.as_deref().unwrap();
    assert!(title.starts_with("zdiff ["), "{title}");
}

#[test]
fn setting_both_sides_to_folders_shows_the_folder_viewer_and_files_show_the_file_diff_again() {
    let dir = tempdir().unwrap();
    let pair = write_pair(dir.path(), "txt", b"a\nb\n", b"a\nc\n");
    let files = [&pair[0], &pair[1]].map(|path| UniversalPath::from(path.clone()));
    let dirs = folder_dirs();
    let (ctx, mut app) = loaded_app(&pair, |_| {});
    assert_eq!(viewers(&mut app), Some(Viewers::Pair(ViewerKind::Text)));

    set_sides(&mut app, [dirs[0].path(), dirs[1].path()]);
    frames_until_the_folder_tree_shows(&ctx, &mut app);
    assert_eq!(viewers(&mut app), Some(Viewers::Folders));

    frame_texts(
        &ctx,
        &mut app,
        mouse_button_click(egui::PointerButton::Extra1),
    );
    assert_eq!(sides(&mut app), files);
    frames_until_the_text_diff_shows(&ctx, &mut app);
    assert!(!paints_folder_names(&frame_texts(
        &ctx,
        &mut app,
        Vec::new()
    )));

    frame_texts(
        &ctx,
        &mut app,
        mouse_button_click(egui::PointerButton::Extra2),
    );
    frames_until_the_folder_tree_shows(&ctx, &mut app);
    assert_eq!(viewers(&mut app), Some(Viewers::Folders));
}

#[test]
fn a_folder_against_a_file_reports_the_folder_side_as_an_error() {
    let dir = tempdir().unwrap();
    let pair = write_pair(dir.path(), "txt", b"a\nb\n", b"a\nc\n");
    let mut app = ZApp::new();
    set_sides(&mut app, [dir.path(), &pair[1]]);
    let ctx = egui::Context::default();
    frames_until_the_text_diff_shows(&ctx, &mut app);

    let state = app.state.as_mut().unwrap().ctx_mut();
    let [folder_error, file_error] = folder_mode::load_errors(&mut state.file_1, &mut state.file_2);
    let folder_error = folder_error.expect("the folder side reports an error");
    assert!(folder_error.contains("is a folder"), "{folder_error}");
    assert_eq!(file_error, None);
    assert_eq!(state.file_1.load_error(), None);
    assert_eq!(state.file_1.get_loading_path(), None);
    assert!(state.file_1.get_loaded_file().is_none());
    assert!(!state.file_1.path_valid());
}

#[test]
fn esc_does_not_close_the_window() {
    let escape = || key_press(egui::Key::Escape, egui::Modifiers::NONE);
    let dirs = folder_dirs();
    let (ctx, mut app) = shown_folder_app(&dirs);
    frame_texts(&ctx, &mut app, escape());
    assert!(is_idle(&app));
    assert_eq!(viewers(&mut app), Some(Viewers::Folders));

    frame_texts(&ctx, &mut app, ctrl_f());
    frame_texts(&ctx, &mut app, Vec::new());
    assert!(app.folder.find_open());
    frame_texts(&ctx, &mut app, escape());
    assert!(!app.folder.find_open());
    assert!(is_idle(&app));
    frame_texts(&ctx, &mut app, escape());
    assert!(is_idle(&app));

    let dir = tempdir().unwrap();
    let pair = write_pair(dir.path(), "txt", b"a\nb\n", b"a\nc\n");
    let (ctx, mut app) = loaded_app(&pair, |_| {});
    let folder = dir.path().to_string_lossy().into_owned();
    app.state.as_mut().unwrap().ctx_mut().browser_folders[0] = Some(folder);
    frame_texts(&ctx, &mut app, Vec::new());
    frame_texts(&ctx, &mut app, escape());
    assert!(is_idle(&app));
    frame_texts(&ctx, &mut app, escape());
    assert!(is_idle(&app));
}

#[test]
fn tab_on_a_folder_pair_does_not_toggle_the_active_side() {
    let dirs = folder_dirs();
    let (ctx, mut app) = shown_folder_app(&dirs);

    frame_texts(
        &ctx,
        &mut app,
        key_press(egui::Key::Tab, egui::Modifiers::NONE),
    );
    assert!(!app.toggle_active_side);

    frame_texts(&ctx, &mut app, ctrl_f());
    assert!(!app.state.as_mut().unwrap().ctx_mut().find_open);
    assert!(app.folder.find_open());
}

#[test]
fn a_scan_running_while_a_file_pair_is_shown_completes_after_returning() {
    let dir = tempdir().unwrap();
    let pair = write_pair(dir.path(), "txt", b"a\nb\n", b"a\nc\n");
    let dirs = folder_dirs();
    let folders = [dirs[0].path(), dirs[1].path()];
    let mut app = ZApp::new();
    set_sides(&mut app, folders);
    let ctx = egui::Context::default();
    frame_texts(&ctx, &mut app, Vec::new());
    frame_texts(&ctx, &mut app, Vec::new());
    assert_eq!(viewers(&mut app), Some(Viewers::Folders));

    set_sides(&mut app, [&pair[0], &pair[1]]);
    frames_until_the_text_diff_shows(&ctx, &mut app);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!paints_folder_names(&frame_texts(
        &ctx,
        &mut app,
        Vec::new()
    )));

    set_sides(&mut app, folders);
    let returned = frame_texts(&ctx, &mut app, Vec::new());
    assert!(paints_folder_names(&returned), "{returned:?}");
}
