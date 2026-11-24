use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui;
use tempfile::{TempDir, tempdir};
use zdiff::universal_path::UniversalPath;
use zdiff_folder::{
    filter::{PathFilter, PatternList},
    fs::FileSystemModel,
};

use super::{
    ZApp, folder_mode,
    state::{AppState, AppStateCtx},
};
use crate::{
    folder::{roots::Side, tree_cursor::TreeCursor, view::FolderView},
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
    let frames_until_hover_tooltips_show = (ctx.style().interaction.tooltip_delay
        / egui::RawInput::default().predicted_dt)
        .ceil() as usize;
    for _ in 0..frames_until_hover_tooltips_show + 20 {
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
    let painted = painted_texts(ctx, app, events);
    painted.into_iter().map(|(text, _)| text).collect()
}

fn painted_texts(
    ctx: &egui::Context,
    app: &mut ZApp,
    events: Vec<egui::Event>,
) -> Vec<(String, egui::Pos2)> {
    fn shape_texts(shape: &egui::Shape, texts: &mut Vec<(String, egui::Pos2)>) {
        match shape {
            egui::Shape::Text(text) => texts.push((
                text.galley.text().to_owned(),
                text.visual_bounding_rect().center(),
            )),
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
fn two_folders_on_the_command_line_show_the_folder_tree_under_the_shared_path_bars() {
    let dirs = folder_dirs();
    let mut app = ZApp::new();
    app.launch(vec![dirs[0].path().into(), dirs[1].path().into()]);

    let ctx = egui::Context::default();
    frames_until_the_folder_tree_shows(&ctx, &mut app);
    let painted = painted_texts(&ctx, &mut app, Vec::new());
    let top_of = |wanted: &dyn Fn(&str) -> bool| {
        let tops = painted.iter().filter(|(text, _)| wanted(text));
        tops.map(|(_, pos)| pos.y).min_by(f32::total_cmp).unwrap()
    };
    let tree_header = top_of(&|text| text.starts_with("Name"));

    for (dir, file) in dirs.iter().zip(FOLDER_NAMES) {
        let folder = dir.path().file_name().unwrap().to_str().unwrap();
        assert!(top_of(&|text| text == folder) < tree_header);
        assert!(top_of(&|text| text.contains(file)) > tree_header);
    }

    assert_eq!(viewers(&mut app), Some(Viewers::Folders));
    assert_eq!(
        sides(&mut app),
        [dirs[0].path(), dirs[1].path()].map(UniversalPath::from)
    );
    let title = app.window_title.as_deref().unwrap();
    assert!(title.starts_with("zdiff ["), "{title}");
}

#[test]
fn a_folder_pair_keeps_the_file_pipeline_idle() {
    let dir = tempdir().unwrap();
    let pair = write_pair(dir.path(), "txt", b"shared_line\nleft\n", b"shared_line\nright\n");
    let (ctx, mut app) = loaded_app(&pair, |_| {});
    let dirs = folder_dirs();

    set_sides(&mut app, [dirs[0].path(), dirs[1].path()]);
    frames_until_the_folder_tree_shows(&ctx, &mut app);
    let texts = frame_texts(&ctx, &mut app, Vec::new());

    assert!(!texts.iter().any(|text| text.contains("shared_line")));
    let state = app.state.as_mut().unwrap().ctx_mut();
    for file in [&mut state.file_1, &mut state.file_2] {
        assert_eq!(file.get_loading_path(), None);
        assert!(file.get_loaded_file().is_none());
    }
    assert!(state.diff_processor.in_progress_input.is_none());
    assert!(!state.diff_processor.is_in_progress());
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
fn swap_sides_refresh_and_find_keys_act_once_on_a_folder_pair() {
    let ctrl = egui::Modifiers {
        ctrl: true,
        command: true,
        ..egui::Modifiers::NONE
    };
    let scanning =
        |app: &ZApp| [Side::Folder1, Side::Folder2].map(|side| app.folder.is_scanning(side));
    let dirs = folder_dirs();
    let (ctx, mut app) = shown_folder_app(&dirs);
    frames_until(&ctx, &mut app, "finished the first scans", |app, _| {
        scanning(app) == [false; 2]
    });

    let swap = key_press(egui::Key::S, ctrl | egui::Modifiers::SHIFT);
    frame_texts(&ctx, &mut app, swap);
    assert!(app.state.as_mut().unwrap().ctx_mut().sides_swapped);

    frame_texts(&ctx, &mut app, key_press(egui::Key::R, ctrl));
    frame_texts(&ctx, &mut app, Vec::new());
    assert_eq!(scanning(&app), [true; 2]);
    frames_until(&ctx, &mut app, "finished the rescans", |app, _| {
        scanning(app) == [false; 2]
    });
    frame_texts(&ctx, &mut app, Vec::new());
    assert_eq!(scanning(&app), [false; 2]);

    frame_texts(&ctx, &mut app, ctrl_f());
    assert!(app.folder.find_open());
    assert!(!app.state.as_mut().unwrap().ctx_mut().find_open);
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

#[test]
fn a_missing_folder_next_to_a_folder_shows_the_scan_error_and_settles() {
    let dirs = folder_dirs();
    let missing = dirs[1].path().join("missing");
    let mut app = ZApp::new();
    set_sides(&mut app, [dirs[0].path(), &missing]);
    let ctx = egui::Context::default();

    frames_until(&ctx, &mut app, "showed the scan error", |app, texts| {
        app.folder.side_error(Side::Folder2).is_some()
            && texts.iter().any(|text| text.contains(FOLDER_NAMES[0]))
    });

    assert_eq!(viewers(&mut app), Some(Viewers::Folders));
    assert_eq!(app.folder.side_error(Side::Folder1), None);
    assert_eq!(sides(&mut app)[1], UniversalPath::from(&*missing));
    for _ in 0..5 {
        run_frame(&ctx, &mut app, SCREEN.x, Vec::new());
    }
    let settled = run_frame(&ctx, &mut app, SCREEN.x, Vec::new()).1;
    assert_eq!(repaint_delay(&settled), Duration::MAX);
    assert!(!app.folder.is_scanning(Side::Folder2));
}

#[test]
fn two_missing_sides_of_a_known_folder_pair_paint_both_scan_errors() {
    let dirs = folder_dirs();
    let (ctx, mut app) = shown_folder_app(&dirs);
    for dir in dirs {
        dir.close().unwrap();
    }
    let state = app.state.as_mut().unwrap().ctx_mut();
    ZApp::refresh_file_contents(&mut state.file_1, &mut state.file_2);
    app.folder.refresh();

    frames_until(&ctx, &mut app, "painted both scan errors", |app, texts| {
        [(Side::Folder1, "Left"), (Side::Folder2, "Right")]
            .into_iter()
            .all(|(side, name)| {
                app.folder.side_error(side).is_some_and(|error| {
                    let line = format!("{name}: {error}");
                    texts.iter().any(|text| text.contains(&line))
                })
            })
    });
    assert_eq!(viewers(&mut app), Some(Viewers::Folders));
}

#[test]
fn a_swapped_folder_pair_paints_the_roots_in_swapped_columns_and_back_restores_them() {
    let dirs = folder_dirs();
    let (ctx, mut app) = shown_folder_app(&dirs);
    let name_columns = |app: &mut ZApp, events: Vec<egui::Event>| {
        let painted = painted_texts(&ctx, app, events);
        FOLDER_NAMES.map(|name| {
            let (_, pos) = painted
                .iter()
                .find(|(text, _)| text.contains(name))
                .unwrap();
            pos.x
        })
    };
    let [only_left, only_right] = name_columns(&mut app, Vec::new());
    assert!(only_left < only_right);

    app.state.as_mut().unwrap().ctx_mut().sides_swapped = true;
    let [only_left, only_right] = name_columns(&mut app, Vec::new());
    assert!(only_left > only_right);

    name_columns(&mut app, mouse_button_click(egui::PointerButton::Extra1));
    let [only_left, only_right] = name_columns(&mut app, Vec::new());
    assert!(!app.state.as_mut().unwrap().ctx_mut().sides_swapped);
    assert!(only_left < only_right);
    assert_eq!(viewers(&mut app), Some(Viewers::Folders));
}

#[test]
fn a_folder_set_on_a_side_becomes_that_root() {
    let dirs = folder_dirs();
    let other = tempdir().unwrap();
    let (ctx, mut app) = shown_folder_app(&dirs);

    let picked = app.state.as_mut().unwrap().ctx_mut().file_2.get_tx();
    picked.send(UniversalPath::from(other.path())).unwrap();
    frame_texts(&ctx, &mut app, Vec::new());

    assert_eq!(
        app.folder.roots(),
        [dirs[0].path(), other.path()].map(|root| Some(root.to_path_buf()))
    );
}

#[test]
fn a_folder_pair_paints_one_pair_of_path_bars() {
    let dirs = folder_dirs();
    let (ctx, mut app) = shown_folder_app(&dirs);

    let texts = frame_texts(&ctx, &mut app, Vec::new());

    for dir in &dirs {
        let folder = dir.path().file_name().unwrap().to_str().unwrap();
        assert_eq!(texts.iter().filter(|text| *text == folder).count(), 1);
    }
    assert!(!texts.iter().any(|text| text == "Open Folder"));
}

#[test]
fn a_folder_pair_paints_one_menu_bar() {
    let dirs = folder_dirs();
    let (ctx, mut app) = shown_folder_app(&dirs);

    let texts = frame_texts(&ctx, &mut app, Vec::new());

    for menu in ["File", "Options"] {
        assert_eq!(texts.iter().filter(|text| *text == menu).count(), 1);
    }
}

fn click_text(ctx: &egui::Context, app: &mut ZApp, text: &str) {
    let painted = painted_texts(ctx, app, Vec::new());
    let (_, pos) = painted
        .iter()
        .find(|(painted, _)| painted == text)
        .unwrap_or_else(|| panic!("{text:?} is not painted"));
    frame_texts(ctx, app, vec![egui::Event::PointerMoved(*pos)]);
    for pressed in [true, false] {
        let button = egui::Event::PointerButton {
            pos: *pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame_texts(ctx, app, vec![button]);
    }
}

#[test]
fn the_shell_scroll_link_drives_the_folder_tree() {
    let name = format!("{}.txt", "long_name_".repeat(20));
    let dirs = folder_dirs();
    for dir in &dirs {
        std::fs::write(dir.path().join(&name), "same").unwrap();
    }
    let name_shifts_after_scrolling_the_left_side = |linked: bool| {
        let (ctx, mut app) = shown_folder_app(&dirs);
        app.state.as_mut().unwrap().ctx_mut().h_scroll_linked = linked;
        let mut name_positions = |events| {
            let mut positions: Vec<egui::Pos2> = painted_texts(&ctx, &mut app, events)
                .into_iter()
                .filter(|(text, _)| *text == name)
                .map(|(_, pos)| pos)
                .collect();
            positions.sort_by(|a, b| a.x.total_cmp(&b.x));
            <[egui::Pos2; 2]>::try_from(positions).expect("the name is painted on both sides")
        };
        let before = name_positions(Vec::new());
        name_positions(vec![
            egui::Event::PointerMoved(egui::pos2(200.0, before[0].y)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(-300.0, 0.0),
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        for _ in 0..30 {
            name_positions(Vec::new());
        }
        let after = name_positions(Vec::new());
        [before[0].x - after[0].x, before[1].x - after[1].x]
    };

    let [left, right] = name_shifts_after_scrolling_the_left_side(true);
    assert!(left > 0.0 && left == right, "{left} {right}");

    let [left, right] = name_shifts_after_scrolling_the_left_side(false);
    assert!(left > 0.0 && right == 0.0, "{left} {right}");
}

#[test]
fn the_view_choice_survives_a_restart() {
    let dirs = folder_dirs();
    let (ctx, mut app) = shown_folder_app(&dirs);
    assert!(!app.folder.duplicates_shown());

    click_text(&ctx, &mut app, "Duplicates");
    let json = serde_json::to_string(&app.folder).unwrap();
    let restored: FolderView = serde_json::from_str(&json).unwrap();

    assert!(app.folder.duplicates_shown());
    assert!(restored.duplicates_shown());
}

fn open_cursor_row(ctx: &egui::Context, app: &mut ZApp, rel_path: &str) {
    *app.folder.cursor_mut() = TreeCursor::at(rel_path);
    frame_texts(ctx, app, key_press(egui::Key::Enter, egui::Modifiers::NONE));
}

fn mouse_nav(ctx: &egui::Context, app: &mut ZApp, button: egui::PointerButton) {
    frame_texts(ctx, app, mouse_button_click(button));
    frame_texts(ctx, app, Vec::new());
}

fn sides_at(dirs: &[TempDir; 2], rel_path: &str) -> [UniversalPath; 2] {
    dirs.each_ref()
        .map(|dir| UniversalPath::Local(dir.path().join(rel_path)))
}

#[derive(Debug, PartialEq)]
struct FolderSnapshot {
    models: [*const FileSystemModel; 2],
    sub_collapsed: [bool; 2],
    cursor: TreeCursor,
    filter: PathFilter,
}

fn folder_snapshot(app: &mut ZApp, dirs: &[TempDir; 2]) -> FolderSnapshot {
    let cursor = app.folder.cursor_mut().clone();
    let state = app.folder.state_mut();
    let views = [
        &state.file_system_model_1_view,
        &state.file_system_model_2_view,
    ]
    .map(|view| view.as_ref().expect("both sides are scanned"));
    let sub_of = |i: usize| {
        let sub = dirs[i].path().join("sub");
        views[i].file_system.find_path(sub).unwrap()
    };
    FolderSnapshot {
        models: views.map(|view| Arc::as_ptr(&view.file_system)),
        sub_collapsed: [0, 1].map(|i| views[i].is_collapsed(sub_of(i))),
        cursor,
        filter: state.path_filter.clone(),
    }
}

#[test]
fn opening_a_row_then_back_returns_to_the_same_folder_view_and_forward_reopens_the_pair() {
    let dirs = folder_dirs();
    for (dir, text) in dirs.iter().zip(["a\nb\n", "a\nc\n"]) {
        std::fs::write(dir.path().join("a.txt"), text).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("deep.txt"), "deep").unwrap();
    }
    let (ctx, mut app) = shown_folder_app(&dirs);
    let state = app.folder.state_mut();
    state.path_filter = PathFilter {
        blacklist: PatternList::new("*.log"),
        ..PathFilter::default()
    };
    for (view, dir) in [
        &mut state.file_system_model_1_view,
        &mut state.file_system_model_2_view,
    ]
    .into_iter()
    .zip(&dirs)
    {
        let view = view.as_mut().unwrap();
        let sub = view.file_system.find_path(dir.path().join("sub")).unwrap();
        view.toggle_collapse(sub);
    }
    frame_texts(&ctx, &mut app, Vec::new());
    let folders = sides(&mut app);
    let pair = sides_at(&dirs, "a.txt");

    open_cursor_row(&ctx, &mut app, "a.txt");
    let opened = folder_snapshot(&mut app, &dirs);
    assert_eq!(opened.sub_collapsed, [true, true]);
    assert_eq!(opened.cursor, TreeCursor::at("a.txt"));
    assert_eq!(sides(&mut app), pair);
    frames_until_the_text_diff_shows(&ctx, &mut app);
    assert!(!paints_folder_names(&frame_texts(
        &ctx,
        &mut app,
        Vec::new()
    )));

    mouse_nav(&ctx, &mut app, egui::PointerButton::Extra1);
    assert_eq!(viewers(&mut app), Some(Viewers::Folders));
    assert_eq!(sides(&mut app), folders);
    assert_eq!(folder_snapshot(&mut app, &dirs), opened);
    assert!(paints_folder_names(&frame_texts(
        &ctx,
        &mut app,
        Vec::new()
    )));

    mouse_nav(&ctx, &mut app, egui::PointerButton::Extra2);
    assert_eq!(sides(&mut app), pair);
    frames_until_the_text_diff_shows(&ctx, &mut app);
}

#[test]
fn a_compare_running_when_a_row_is_opened_finishes_after_back() {
    let dirs = folder_dirs();
    for dir in &dirs {
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        for i in 0..3000 {
            std::fs::write(dir.path().join(format!("f{i:04}.txt")), "same").unwrap();
        }
    }
    let mut app = ZApp::new();
    set_sides(&mut app, [dirs[0].path(), dirs[1].path()]);
    let ctx = egui::Context::default();
    frames_until(&ctx, &mut app, "started the compare", |app, texts| {
        app.folder.is_comparing() && texts.iter().any(|text| text.contains("a.txt"))
    });

    open_cursor_row(&ctx, &mut app, "a.txt");
    assert_eq!(sides(&mut app), sides_at(&dirs, "a.txt"));
    frames_until_the_text_diff_shows(&ctx, &mut app);

    mouse_nav(&ctx, &mut app, egui::PointerButton::Extra1);
    assert_eq!(viewers(&mut app), Some(Viewers::Folders));
    frames_until(&ctx, &mut app, "finished the compare", |app, texts| {
        !app.folder.is_comparing() && texts.iter().any(|text| text.contains(", 0 pending"))
    });
}
