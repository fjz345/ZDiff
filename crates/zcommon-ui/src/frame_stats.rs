use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use eframe::egui;

const FPS_WINDOW: Duration = Duration::from_secs(1);
const MAX_FRAME_STARTS: usize = 1024;
const IDLE_REPAINT_AFTER: Duration = Duration::from_millis(1100);
const SAMPLES: usize = 120;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Timing {
    pub average: Duration,
    pub max: Duration,
}

#[derive(Default)]
struct Samples(VecDeque<Duration>);

impl Samples {
    fn push(&mut self, sample: Duration) {
        if self.0.len() == SAMPLES {
            self.0.pop_front();
        }
        self.0.push_back(sample);
    }

    fn timing(&self) -> Option<Timing> {
        let max = *self.0.iter().max()?;
        let average = self.0.iter().sum::<Duration>() / self.0.len() as u32;
        Some(Timing { average, max })
    }
}

struct Stage {
    name: &'static str,
    current: Duration,
    samples: Samples,
}

#[derive(Default)]
pub struct FrameStats {
    frame_starts: VecDeque<Instant>,
    fps: Option<f32>,
    last_fps: Option<f32>,
    frame_nr: Option<u64>,
    mark: Option<Instant>,
    stages: Vec<Stage>,
    update: Samples,
    cpu: Samples,
    rest: Samples,
    idle_wake: IdleWake,
}

impl FrameStats {
    pub fn begin_frame(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let now = Instant::now();
        if self.begin_frame_at(ctx.cumulative_frame_nr(), frame.info().cpu_usage, now) {
            let ctx = ctx.clone();
            // A zero delay runs two frames, and the second one would measure as live again.
            let one_frame = Duration::from_millis(1);
            self.idle_wake.arm(now + IDLE_REPAINT_AFTER, move || {
                ctx.request_repaint_after(one_frame)
            });
        }
    }

    pub fn mark(&mut self, stage: &'static str) {
        self.mark_at(stage, Instant::now());
    }

    fn begin_frame_at(&mut self, frame_nr: u64, cpu_usage: Option<f32>, now: Instant) -> bool {
        self.mark = Some(now);
        match self.frame_nr.replace(frame_nr) {
            Some(previous) if previous == frame_nr => return self.fps.is_some(),
            Some(_) => self.finish_frame(cpu_usage),
            None => {}
        }
        if self.frame_starts.len() == MAX_FRAME_STARTS {
            self.frame_starts.pop_front();
        }
        self.frame_starts.push_back(now);
        while self
            .frame_starts
            .front()
            .is_some_and(|&start| now.duration_since(start) > FPS_WINDOW)
        {
            self.frame_starts.pop_front();
        }
        let span = self
            .frame_starts
            .front()
            .map_or(0.0, |&oldest| now.duration_since(oldest).as_secs_f32());
        self.fps = (span > 0.0).then(|| (self.frame_starts.len() - 1) as f32 / span);
        if self.fps.is_some() {
            self.last_fps = self.fps;
        }
        self.fps.is_some()
    }

    // eframe reports a frame's CPU time in the next frame, so it pairs with the stages finished here.
    fn finish_frame(&mut self, cpu_usage: Option<f32>) {
        let mut update = Duration::ZERO;
        for stage in &mut self.stages {
            update += stage.current;
            stage.samples.push(std::mem::take(&mut stage.current));
        }
        self.update.push(update);
        if let Some(cpu) = cpu_usage {
            let cpu = Duration::from_secs_f32(cpu);
            self.cpu.push(cpu);
            self.rest.push(cpu.saturating_sub(update));
        }
    }

    fn mark_at(&mut self, stage: &'static str, now: Instant) {
        let since = self
            .mark
            .replace(now)
            .expect("begin_frame runs before the first mark");
        let elapsed = now.duration_since(since);
        match self.stages.iter_mut().find(|s| s.name == stage) {
            Some(stage) => stage.current += elapsed,
            None => self.stages.push(Stage {
                name: stage,
                current: elapsed,
                samples: Samples::default(),
            }),
        }
    }

    fn rows(&self) -> Vec<(&'static str, Timing)> {
        let stages = self
            .stages
            .iter()
            .filter_map(|stage| Some((stage.name, stage.samples.timing()?)));
        let rest = self.rest.timing().map(|rest| ("egui + paint", rest));
        let total = match self.cpu.timing() {
            Some(cpu) => Some(("frame CPU", cpu)),
            None => self.update.timing().map(|update| ("update", update)),
        };
        stages.chain(rest).chain(total).collect()
    }
}

// A thread, as a repaint requested inside the frame would keep its repaint_delay below MAX.
#[derive(Default)]
struct IdleWake(Arc<Mutex<Option<Instant>>>);

impl IdleWake {
    fn arm(&self, at: Instant, repaint: impl FnOnce() + Send + 'static) {
        let waiting = self.0.lock().expect("idle wake lock").replace(at).is_some();
        if waiting {
            return;
        }
        let deadline = Arc::clone(&self.0);
        thread::spawn(move || {
            loop {
                let wait = {
                    let mut deadline = deadline.lock().expect("idle wake lock");
                    let at = deadline.expect("armed until it repaints");
                    let wait = at.saturating_duration_since(Instant::now());
                    if wait.is_zero() {
                        *deadline = None;
                    }
                    wait
                };
                if wait.is_zero() {
                    break;
                }
                thread::sleep(wait);
            }
            repaint();
        });
    }
}

pub fn fps_label(ui: &mut egui::Ui, stats: &FrameStats, show_breakdown: &mut bool) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let mut clicked = false;
        if let Some(fps) = stats.fps.or(stats.last_fps) {
            let label = egui::Label::new(
                egui::RichText::new(format!("{fps:.0} fps"))
                    .monospace()
                    .weak(),
            )
            .sense(egui::Sense::click())
            .truncate();
            clicked |= ui
                .add(label)
                .on_hover_text("Frame time breakdown")
                .clicked();
        }
        if stats.fps.is_none() {
            clicked |= idle_tag(ui).on_hover_text("Frame time breakdown").clicked();
        }
        if clicked {
            *show_breakdown = !*show_breakdown;
        }
    });
}

fn idle_tag(ui: &mut egui::Ui) -> egui::Response {
    let color = ui.visuals().weak_text_color();
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, color))
        .corner_radius(3.0)
        .inner_margin(egui::Margin::symmetric(4, 0))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(egui::RichText::new("idle").monospace().color(color))
                    .selectable(false),
            )
        })
        .response
        .interact(egui::Sense::click())
}

pub fn breakdown_window(ctx: &egui::Context, stats: &FrameStats, open: &mut bool) {
    egui::Window::new("Frame Time")
        .open(open)
        .resizable(false)
        .collapsible(false)
        .pivot(egui::Align2::RIGHT_TOP)
        .default_pos(ctx.content_rect().right_top() + egui::vec2(-8.0, 32.0))
        .show(ctx, |ui| breakdown_ui(ui, stats));
}

fn breakdown_ui(ui: &mut egui::Ui, stats: &FrameStats) {
    ui.label(match stats.fps {
        Some(fps) => format!("{fps:.0} fps, {:.2} ms apart", 1000.0 / fps),
        None => "idle".to_owned(),
    });
    let rows = stats.rows();
    let total = rows
        .last()
        .map_or(Duration::ZERO, |(_, total)| total.average);
    let ms = |time: Duration| format!("{:7.3}", time.as_secs_f64() * 1000.0);
    egui::Grid::new("frame_time_breakdown")
        .num_columns(4)
        .striped(true)
        .show(ui, |ui| {
            ui.strong("Stage");
            ui.strong("avg ms");
            ui.strong("max ms");
            ui.end_row();
            for (name, timing) in rows {
                ui.label(name);
                ui.monospace(ms(timing.average));
                ui.monospace(ms(timing.max));
                share_bar(ui, timing.average, total);
                ui.end_row();
            }
        });
    ui.weak(format!(
        "Over the last {} frames. Frame CPU excludes vsync.",
        stats.update.0.len()
    ));
}

fn share_bar(ui: &mut egui::Ui, part: Duration, total: Duration) {
    let size = egui::vec2(80.0, ui.spacing().interact_size.y * 0.5);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let share = if total.is_zero() {
        0.0
    } else {
        (part.as_secs_f32() / total.as_secs_f32()).min(1.0)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    let mut filled = rect;
    filled.set_width(rect.width() * share);
    painter.rect_filled(filled, 2.0, ui.visuals().selection.bg_fill);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn ms(base: Instant, ms: f64) -> Instant {
        base + Duration::from_secs_f64(ms / 1000.0)
    }

    fn frames(stats: &mut FrameStats, base: Instant, first_nr: u64, times_ms: &[f64]) {
        for (i, &time) in times_ms.iter().enumerate() {
            stats.begin_frame_at(first_nr + i as u64, None, ms(base, time));
        }
    }

    fn stage(stats: &FrameStats, name: &str) -> Option<Timing> {
        let stage = stats.stages.iter().find(|stage| stage.name == name)?;
        stage.samples.timing()
    }

    #[test]
    fn sixty_frames_16_7_ms_apart_measure_60_fps() {
        let mut stats = FrameStats::default();
        let times: Vec<f64> = (0..60).map(|i| i as f64 * 1000.0 / 60.0).collect();

        frames(&mut stats, Instant::now(), 0, &times);

        let fps = stats.fps.unwrap();
        assert!((fps - 60.0).abs() < 0.01, "{fps}");
    }

    #[test]
    fn a_single_frame_has_no_fps() {
        let mut stats = FrameStats::default();

        let live = stats.begin_frame_at(0, None, Instant::now());

        assert!(!live);
        assert_eq!(stats.fps, None);
    }

    #[test]
    fn a_frame_after_a_gap_longer_than_the_window_is_idle_and_keeps_the_last_fps() {
        let mut stats = FrameStats::default();
        let base = Instant::now();
        frames(&mut stats, base, 0, &[0.0, 10.0, 20.0]);

        let live = stats.begin_frame_at(3, None, ms(base, 20.0 + 1100.0));

        assert!(!live);
        assert_eq!(stats.fps, None);
        assert_eq!(stats.last_fps, Some(100.0));
    }

    #[test]
    fn a_burst_after_a_long_gap_measures_the_burst_rate() {
        let mut stats = FrameStats::default();
        let base = Instant::now();
        frames(&mut stats, base, 0, &[0.0, 100.0, 200.0]);

        frames(&mut stats, base, 3, &[5000.0, 5005.0, 5010.0]);

        assert_eq!(stats.fps, Some(200.0));
    }

    #[test]
    fn the_frame_starts_stay_capped() {
        let mut stats = FrameStats::default();
        let times: Vec<f64> = (0..5000).map(|i| i as f64 * 0.1).collect();

        frames(&mut stats, Instant::now(), 0, &times);

        assert_eq!(stats.frame_starts.len(), MAX_FRAME_STARTS);
        let fps = stats.fps.unwrap();
        assert!((fps - 10_000.0).abs() < 1.0, "{fps}");
    }

    #[test]
    fn repeated_passes_of_one_frame_count_once_and_add_up_their_stage_time() {
        let mut stats = FrameStats::default();
        let base = Instant::now();
        for pass_start in [0.0, 10.0] {
            stats.begin_frame_at(7, None, ms(base, pass_start));
            stats.mark_at("ui", ms(base, pass_start + 3.0));
        }

        stats.begin_frame_at(8, None, ms(base, 16.0));

        assert_eq!(stats.frame_starts.len(), 2);
        let ui = stage(&stats, "ui").unwrap();
        assert_eq!(ui.average, Duration::from_millis(6));
    }

    #[test]
    fn stage_marks_split_the_frame_and_average_over_finished_frames() {
        let mut stats = FrameStats::default();
        let base = Instant::now();
        for (frame, (inputs, panes)) in [(1.0, 4.0), (3.0, 2.0)].into_iter().enumerate() {
            let start = frame as f64 * 16.0;
            stats.begin_frame_at(frame as u64, None, ms(base, start));
            stats.mark_at("inputs", ms(base, start + inputs));
            stats.mark_at("panes", ms(base, start + inputs + panes));
        }

        stats.begin_frame_at(2, None, ms(base, 32.0));

        let timing = |average: u64, max: u64| Timing {
            average: Duration::from_millis(average),
            max: Duration::from_millis(max),
        };
        assert_eq!(stage(&stats, "inputs"), Some(timing(2, 3)));
        assert_eq!(stage(&stats, "panes"), Some(timing(3, 4)));
        assert_eq!(stats.update.timing(), Some(timing(5, 5)));
    }

    #[test]
    fn the_cpu_time_beyond_the_stages_pairs_with_the_previous_frames_update() {
        let mut stats = FrameStats::default();
        let base = Instant::now();
        stats.begin_frame_at(0, None, base);
        stats.mark_at("ui", ms(base, 3.0));

        stats.begin_frame_at(1, Some(0.005), ms(base, 16.0));

        let cpu = stats.cpu.timing().unwrap();
        let rest = stats.rest.timing().unwrap();
        assert!((cpu.average.as_secs_f64() - 0.005).abs() < 1e-6, "{cpu:?}");
        assert!(
            (rest.average.as_secs_f64() - 0.002).abs() < 1e-6,
            "{rest:?}"
        );
    }

    #[test]
    fn samples_keep_only_the_latest_frames() {
        let mut samples = Samples::default();
        for i in 0..SAMPLES as u64 + 10 {
            samples.push(Duration::from_millis(if i < 10 { 100 } else { 1 }));
        }

        assert_eq!(
            samples.timing(),
            Some(Timing {
                average: Duration::from_millis(1),
                max: Duration::from_millis(1),
            })
        );
    }

    #[test]
    fn rearming_the_idle_wake_repaints_once_after_the_last_deadline() {
        let wake = IdleWake::default();
        let (sender, repaints) = mpsc::channel();
        let mut last_deadline = Instant::now();
        for _ in 0..5 {
            last_deadline = Instant::now() + Duration::from_millis(40);
            let sender = sender.clone();
            wake.arm(last_deadline, move || sender.send(Instant::now()).unwrap());
            thread::sleep(Duration::from_millis(10));
        }
        drop(sender);

        let repainted = repaints.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(repainted >= last_deadline);
        assert_eq!(
            repaints.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }

    #[test]
    fn the_idle_wake_arms_again_after_it_repainted() {
        let wake = IdleWake::default();
        let (sender, repaints) = mpsc::channel();
        for _ in 0..2 {
            let sender = sender.clone();
            wake.arm(Instant::now() + Duration::from_millis(5), move || {
                sender.send(()).unwrap()
            });
            repaints.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }
}
