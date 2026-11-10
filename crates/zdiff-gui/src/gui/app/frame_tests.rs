use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use eframe::egui;
use tempfile::tempdir;
use zdiff::universal_path::UniversalPath;

use super::{ZApp, state::AppStateCtx};
use crate::viewer::hex_strings::HexMode;

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
