use std::{
    cell::RefCell,
    collections::VecDeque,
    marker::PhantomData,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use eframe::egui;

const REPAINT_RATE_WINDOW: Duration = Duration::from_secs(1);
const MAX_FRAME_STARTS: usize = 1024;
const IDLE_REPAINT_AFTER: Duration = Duration::from_millis(1100);
const SAMPLES: usize = 120;
const TOTAL_CPU: &str = "Total CPU";
const TOTAL_APP_CODE: &str = "Total (app code only)";
const OUTSIDE_APP: &str = "egui + OpenGL (outside app code)";
const EGUI_PASS_START: &str = "egui pass start";
const EGUI_PASS_END: &str = "egui pass end";
const PAINT: &str = "Tessellation and OpenGL paint";
const REST: &str = "(rest of section)";
const SUB_INDENT: f32 = 16.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Timing {
    pub average: Duration,
    pub max: Duration,
}

#[derive(Default)]
struct Samples(VecDeque<Duration>);

impl Samples {
    fn zeros(len: usize) -> Self {
        Self(std::iter::repeat_n(Duration::ZERO, len).collect())
    }

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

thread_local! {
    static SCOPES: RefCell<Scopes> = const {
        RefCell::new(Scopes {
            depth: 0,
            closed: Vec::new(),
        })
    };
}

struct Scopes {
    depth: u32,
    closed: Vec<(&'static str, Duration)>,
}

#[must_use = "a scope measures until it is dropped"]
pub struct Scope {
    outermost: Option<&'static str>,
    start: Instant,
    _same_thread: PhantomData<*const ()>,
}

// Nested scopes count toward the outermost one only, so sub-sections never overlap.
pub fn scope(name: &'static str) -> Scope {
    let outermost = SCOPES.with_borrow_mut(|scopes| {
        scopes.depth += 1;
        scopes.depth == 1
    });
    Scope {
        outermost: outermost.then_some(name),
        start: Instant::now(),
        _same_thread: PhantomData,
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        SCOPES.with_borrow_mut(|scopes| scopes.depth -= 1);
        if let Some(name) = self.outermost {
            record_scope(name, elapsed);
        }
    }
}

fn record_scope(name: &'static str, elapsed: Duration) {
    SCOPES.with_borrow_mut(|scopes| {
        match scopes.closed.iter_mut().find(|(closed, _)| *closed == name) {
            Some((_, time)) => *time += elapsed,
            None => scopes.closed.push((name, elapsed)),
        }
    });
}

fn drop_closed_scopes() {
    SCOPES.with_borrow_mut(|scopes| scopes.closed.clear());
}

struct Sub {
    name: &'static str,
    current: Duration,
    samples: Samples,
}

struct Section {
    name: &'static str,
    current: Duration,
    samples: Samples,
    subs: Vec<Sub>,
    rest: Samples,
}

impl Section {
    // Zero-filled to the finished frames, so it averages over the same frames as the total.
    fn new(name: &'static str, finished_frames: usize) -> Self {
        Self {
            name,
            current: Duration::ZERO,
            samples: Samples::zeros(finished_frames),
            subs: Vec::new(),
            rest: Samples::zeros(finished_frames),
        }
    }

    fn add_sub(&mut self, name: &'static str, time: Duration) {
        match self.subs.iter_mut().find(|sub| sub.name == name) {
            Some(sub) => sub.current += time,
            None => self.subs.push(Sub {
                name,
                current: time,
                samples: Samples::zeros(self.samples.0.len()),
            }),
        }
    }

    fn finish_frame(&mut self) -> Duration {
        let total = std::mem::take(&mut self.current);
        let mut in_subs = Duration::ZERO;
        for sub in &mut self.subs {
            let time = std::mem::take(&mut sub.current);
            in_subs += time;
            sub.samples.push(time);
        }
        self.samples.push(total);
        self.rest.push(total.saturating_sub(in_subs));
        total
    }

    fn row(&self) -> Option<SectionRow> {
        let timing = self.samples.timing()?;
        let subs = self
            .subs
            .iter()
            .filter_map(|sub| Some((sub.name, sub.samples.timing()?)));
        let rest = self
            .rest
            .timing()
            .filter(|_| !self.subs.is_empty())
            .map(|rest| (REST, rest));
        Some(SectionRow {
            name: self.name,
            timing,
            subs: subs.chain(rest).collect(),
        })
    }
}

#[derive(Debug, PartialEq)]
struct SectionRow {
    name: &'static str,
    timing: Timing,
    subs: Vec<(&'static str, Timing)>,
}

struct Breakdown {
    total_name: &'static str,
    total: Timing,
    sections: Vec<SectionRow>,
}

#[derive(Clone, Copy, Debug, Default)]
struct EguiPasses {
    start: Duration,
    end: Duration,
}

#[derive(Default)]
struct PassTimer {
    start_began: Option<Instant>,
    end_began: Option<Instant>,
    passes: EguiPasses,
}

impl PassTimer {
    fn take(&mut self) -> EguiPasses {
        std::mem::take(&mut self.passes)
    }
}

impl egui::Plugin for PassTimer {
    fn debug_name(&self) -> &'static str {
        "frame_stats::PassTimer"
    }

    fn input_hook(&mut self, _input: &mut egui::RawInput) {
        self.start_began = Some(Instant::now());
    }

    fn on_begin_pass(&mut self, _ctx: &egui::Context) {
        if let Some(began) = self.start_began.take() {
            self.passes.start += began.elapsed();
        }
    }

    fn on_end_pass(&mut self, _ctx: &egui::Context) {
        self.end_began = Some(Instant::now());
    }

    fn output_hook(&mut self, _output: &mut egui::FullOutput) {
        if let Some(began) = self.end_began.take() {
            self.passes.end += began.elapsed();
        }
    }
}

#[derive(Default)]
pub struct FrameStats {
    frame_starts: VecDeque<Instant>,
    repaint_rate: Option<f32>,
    fps: Option<f32>,
    frame_nr: Option<u64>,
    mark: Option<Instant>,
    sections: Vec<Section>,
    update: Samples,
    cpu: Samples,
    outside_app: Option<Section>,
    egui_passes: EguiPasses,
    idle_wake: IdleWake,
}

impl FrameStats {
    pub fn begin_frame(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let now = Instant::now();
        let passes = ctx.with_plugin(PassTimer::take).unwrap_or_else(|| {
            ctx.add_plugin(PassTimer::default());
            EguiPasses::default()
        });
        let frame_nr = ctx.cumulative_frame_nr();
        if self.begin_pass_at(frame_nr, frame.info().cpu_usage, passes, now) {
            let ctx = ctx.clone();
            // A zero delay runs two frames, and the second one would measure as live again.
            let one_frame = Duration::from_millis(1);
            self.idle_wake.arm(now + IDLE_REPAINT_AFTER, move || {
                ctx.request_repaint_after(one_frame)
            });
        }
    }

    pub fn mark(&mut self, section: &'static str) {
        self.mark_at(section, Instant::now());
    }

    // The previous pass ended in the frame that begin_frame_at may finish, so its end goes first.
    fn begin_pass_at(
        &mut self,
        frame_nr: u64,
        cpu_usage: Option<f32>,
        passes: EguiPasses,
        now: Instant,
    ) -> bool {
        self.egui_passes.end += passes.end;
        let live = self.begin_frame_at(frame_nr, cpu_usage, now);
        self.egui_passes.start += passes.start;
        live
    }

    fn begin_frame_at(&mut self, frame_nr: u64, cpu_usage: Option<f32>, now: Instant) -> bool {
        drop_closed_scopes();
        self.mark = Some(now);
        match self.frame_nr.replace(frame_nr) {
            Some(previous) if previous == frame_nr => return self.repaint_rate.is_some(),
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
            .is_some_and(|&start| now.duration_since(start) > REPAINT_RATE_WINDOW)
        {
            self.frame_starts.pop_front();
        }
        let span = self
            .frame_starts
            .front()
            .map_or(0.0, |&oldest| now.duration_since(oldest).as_secs_f32());
        self.repaint_rate = (span > 0.0).then(|| (self.frame_starts.len() - 1) as f32 / span);
        self.repaint_rate.is_some()
    }

    // eframe reports a frame's CPU time in the next frame, so it pairs with the stages finished here.
    fn finish_frame(&mut self, cpu_usage: Option<f32>) {
        let update: Duration = self.sections.iter_mut().map(Section::finish_frame).sum();
        self.update.push(update);
        let passes = std::mem::take(&mut self.egui_passes);
        if let Some(cpu) = cpu_usage {
            let cpu = Duration::from_secs_f32(cpu);
            self.cpu.push(cpu);
            let outside_app = cpu.saturating_sub(update);
            let section = self
                .outside_app
                .get_or_insert_with(|| Section::new(OUTSIDE_APP, 0));
            section.current = outside_app;
            section.add_sub(EGUI_PASS_START, passes.start);
            section.add_sub(EGUI_PASS_END, passes.end);
            section.add_sub(PAINT, outside_app.saturating_sub(passes.start + passes.end));
            section.finish_frame();
        }
        if let Some((_, total)) = self.total()
            && !total.average.is_zero()
        {
            self.fps = Some(1.0 / total.average.as_secs_f32());
        }
    }

    fn total(&self) -> Option<(&'static str, Timing)> {
        match self.cpu.timing() {
            Some(cpu) => Some((TOTAL_CPU, cpu)),
            None => self.update.timing().map(|update| (TOTAL_APP_CODE, update)),
        }
    }

    fn mark_at(&mut self, section: &'static str, now: Instant) {
        let since = self
            .mark
            .replace(now)
            .expect("begin_frame runs before the first mark");
        let index = match self.sections.iter().position(|s| s.name == section) {
            Some(index) => index,
            None => {
                let finished_frames = self.update.0.len();
                self.sections.push(Section::new(section, finished_frames));
                self.sections.len() - 1
            }
        };
        let section = &mut self.sections[index];
        section.current += now.duration_since(since);
        SCOPES.with_borrow_mut(|scopes| {
            for (name, time) in scopes.closed.drain(..) {
                section.add_sub(name, time);
            }
        });
    }

    fn breakdown(&self) -> Option<Breakdown> {
        let (total_name, total) = self.total()?;
        let sections = self
            .sections
            .iter()
            .chain(&self.outside_app)
            .filter_map(Section::row)
            .collect();
        Some(Breakdown {
            total_name,
            total,
            sections,
        })
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
        if let Some(fps) = stats.fps {
            let label = egui::Label::new(
                egui::RichText::new(format!("{fps:.0} fps"))
                    .monospace()
                    .weak(),
            )
            .sense(egui::Sense::click())
            .truncate();
            clicked |= ui
                .add(label)
                .on_hover_text(format!(
                    "1 / average CPU time per frame over the last {SAMPLES} frames, \
                     vsync wait excluded. Click for the frame time breakdown."
                ))
                .clicked();
        }
        if stats.repaint_rate.is_none() {
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
    let _scope = scope("Frame time window");
    egui::Window::new("Frame Time")
        .open(open)
        .resizable(false)
        .collapsible(false)
        .pivot(egui::Align2::RIGHT_TOP)
        .default_pos(ctx.content_rect().right_top() + egui::vec2(-8.0, 32.0))
        .show(ctx, |ui| breakdown_ui(ui, stats));
}

fn breakdown_ui(ui: &mut egui::Ui, stats: &FrameStats) {
    match stats.breakdown() {
        Some(breakdown) => {
            total_block(ui, stats, &breakdown);
            ui.add_space(6.0);
            ui.separator();
            ui.label(egui::RichText::new("Breakdown").strong());
            section_grid(ui, &breakdown);
            ui.weak(format!(
                "Over the last {} frames. Indented rows split the section above them.",
                stats.update.0.len()
            ));
        }
        None => {
            ui.weak("No finished frame yet.");
        }
    }
    ui.separator();
    crate::vsync::checkbox(ui);
}

fn total_block(ui: &mut egui::Ui, stats: &FrameStats, breakdown: &Breakdown) {
    let total_tooltip = if breakdown.total_name == TOTAL_CPU {
        "eframe's CPU time per frame: input, app code, egui and OpenGL paint. \
         The v-sync wait is excluded."
    } else {
        "eframe reports no frame CPU time, so this sums the app code sections only."
    };
    egui::Frame::group(ui.style())
        .fill(ui.visuals().extreme_bg_color)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(breakdown.total_name).heading().strong())
                    .on_hover_text(total_tooltip);
                ui.label(
                    egui::RichText::new(format!(
                        "{:.3} ms avg",
                        breakdown.total.average.as_secs_f64() * 1000.0
                    ))
                    .heading(),
                );
            });
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{:.3} ms max",
                    breakdown.total.max.as_secs_f64() * 1000.0
                ));
                if let Some(fps) = stats.fps {
                    ui.label(format!("{fps:.0} fps = 1 / avg"));
                }
            });
            ui.weak(match stats.repaint_rate {
                Some(rate) => format!("Repaint rate {rate:.0}/s (follows input)"),
                None => "Repaint rate: idle".to_owned(),
            });
        });
}

fn section_grid(ui: &mut egui::Ui, breakdown: &Breakdown) {
    let total = breakdown.total.average;
    egui::Grid::new("frame_time_breakdown")
        .num_columns(4)
        .striped(true)
        .show(ui, |ui| {
            ui.weak("Section");
            ui.weak("avg ms");
            ui.weak("max ms");
            ui.weak("share of total");
            ui.end_row();
            for section in &breakdown.sections {
                with_tooltip(ui.strong(section.name), section.name);
                ui.monospace(ms(section.timing.average));
                ui.monospace(ms(section.timing.max));
                share_bar(ui, section.timing.average, total, false);
                ui.end_row();
                for &(name, timing) in section.subs.iter().filter(|(_, t)| !t.max.is_zero()) {
                    let label = ui
                        .horizontal(|ui| {
                            ui.add_space(SUB_INDENT);
                            ui.label(egui::RichText::new(name).weak())
                        })
                        .inner;
                    with_tooltip(label, name);
                    ui.label(egui::RichText::new(ms(timing.average)).monospace().weak());
                    ui.label(egui::RichText::new(ms(timing.max)).monospace().weak());
                    share_bar(ui, timing.average, total, true);
                    ui.end_row();
                }
            }
        });
}

fn with_tooltip(label: egui::Response, name: &str) {
    let tooltip = match name {
        OUTSIDE_APP => {
            "Frame CPU outside the app's update code: egui's own work before and after it, \
             and eframe's OpenGL backend."
        }
        EGUI_PASS_START => "egui begin_pass: input events, widget hit testing and focus.",
        EGUI_PASS_END => {
            "egui end_pass: shapes collected by layer, font atlas and texture changes, \
             accessibility tree and window commands."
        }
        PAINT => {
            "Shapes turned into triangles, OpenGL draw calls and texture uploads, \
             cursor and clipboard output. The v-sync wait is excluded."
        }
        REST => "Time in this section outside its indented sub-sections.",
        _ => return,
    };
    label.on_hover_text(tooltip);
}

fn ms(time: Duration) -> String {
    format!("{:7.3}", time.as_secs_f64() * 1000.0)
}

fn share_bar(ui: &mut egui::Ui, part: Duration, total: Duration, sub: bool) {
    let height = ui.spacing().interact_size.y * if sub { 0.25 } else { 0.5 };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(80.0, height), egui::Sense::hover());
    let share = if total.is_zero() {
        0.0
    } else {
        (part.as_secs_f32() / total.as_secs_f32()).min(1.0)
    };
    let fill = ui.visuals().selection.bg_fill;
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    let mut filled = rect;
    filled.set_width(rect.width() * share);
    painter.rect_filled(
        filled,
        2.0,
        if sub { fill.gamma_multiply(0.6) } else { fill },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn ms(base: Instant, ms: f64) -> Instant {
        base + Duration::from_secs_f64(ms / 1000.0)
    }

    fn millis(ms: f64) -> Duration {
        Duration::from_micros((ms * 1000.0).round() as u64)
    }

    fn timing(average: f64, max: f64) -> Timing {
        Timing {
            average: millis(average),
            max: millis(max),
        }
    }

    fn frames(stats: &mut FrameStats, base: Instant, first_nr: u64, times_ms: &[f64]) {
        for (i, &time) in times_ms.iter().enumerate() {
            stats.begin_frame_at(first_nr + i as u64, None, ms(base, time));
        }
    }

    fn cpu_frames(stats: &mut FrameStats, base: Instant, cpu_secs: &[f32], times_ms: &[f64]) {
        for (nr, &time) in times_ms.iter().enumerate() {
            let cpu = cpu_secs[nr % cpu_secs.len()];
            stats.begin_frame_at(nr as u64, Some(cpu), ms(base, time));
        }
    }

    fn section(stats: &FrameStats, name: &str) -> Option<Timing> {
        let section = stats.sections.iter().find(|section| section.name == name)?;
        section.samples.timing()
    }

    fn section_row(stats: &FrameStats, name: &str) -> SectionRow {
        let breakdown = stats.breakdown().unwrap();
        breakdown
            .sections
            .into_iter()
            .find(|section| section.name == name)
            .unwrap()
    }

    #[test]
    fn the_fps_is_the_same_for_frames_100_ms_and_5_ms_apart() {
        let slow_times: Vec<f64> = (0..30).map(|i| i as f64 * 100.0).collect();
        let fast_times: Vec<f64> = (0..30).map(|i| i as f64 * 5.0).collect();
        let (mut slow, mut fast) = (FrameStats::default(), FrameStats::default());

        cpu_frames(&mut slow, Instant::now(), &[0.004], &slow_times);
        cpu_frames(&mut fast, Instant::now(), &[0.004], &fast_times);

        let (slow, fast) = (slow.fps.unwrap(), fast.fps.unwrap());
        assert!((slow - 250.0).abs() < 0.1, "{slow}");
        assert!((fast - 250.0).abs() < 0.1, "{fast}");
    }

    #[test]
    fn the_fps_inverts_the_average_frame_cpu_not_the_average_rate() {
        let times: Vec<f64> = (0..41).map(|i| i as f64 * 16.0).collect();
        let mut stats = FrameStats::default();

        cpu_frames(&mut stats, Instant::now(), &[0.001, 0.003], &times);

        let fps = stats.fps.unwrap();
        assert!((fps - 500.0).abs() < 0.1, "{fps}");
    }

    #[test]
    fn the_fps_averages_the_frame_cpu_of_the_last_samples_frames() {
        let times: Vec<f64> = (0..=SAMPLES + 20).map(|i| i as f64 * 16.0).collect();
        let cpu: Vec<f32> = (0..=SAMPLES + 20)
            .map(|nr| if nr < 20 { 0.1 } else { 0.002 })
            .collect();
        let mut stats = FrameStats::default();

        cpu_frames(&mut stats, Instant::now(), &cpu, &times);

        let fps = stats.fps.unwrap();
        assert!((fps - 500.0).abs() < 0.1, "{fps}");
    }

    #[test]
    fn without_frame_cpu_the_fps_inverts_the_average_update_time() {
        let mut stats = FrameStats::default();
        let base = Instant::now();
        for frame in 0..4 {
            let start = frame as f64 * 50.0;
            stats.begin_frame_at(frame, None, ms(base, start));
            stats.mark_at("ui", ms(base, start + 4.0));
        }

        stats.begin_frame_at(4, None, ms(base, 200.0));

        let fps = stats.fps.unwrap();
        assert!((fps - 250.0).abs() < 0.1, "{fps}");
    }

    #[test]
    fn zero_frame_cpu_has_no_fps() {
        let times: Vec<f64> = (0..10).map(|i| i as f64 * 16.0).collect();
        let mut stats = FrameStats::default();

        cpu_frames(&mut stats, Instant::now(), &[0.0], &times);

        assert_eq!(stats.fps, None);
    }

    #[test]
    fn sixty_frames_16_7_ms_apart_repaint_60_times_a_second() {
        let mut stats = FrameStats::default();
        let times: Vec<f64> = (0..60).map(|i| i as f64 * 1000.0 / 60.0).collect();

        frames(&mut stats, Instant::now(), 0, &times);

        let rate = stats.repaint_rate.unwrap();
        assert!((rate - 60.0).abs() < 0.01, "{rate}");
    }

    #[test]
    fn a_single_frame_is_not_live_and_has_no_fps() {
        let mut stats = FrameStats::default();

        let live = stats.begin_frame_at(0, Some(0.004), Instant::now());

        assert!(!live);
        assert_eq!(stats.repaint_rate, None);
        assert_eq!(stats.fps, None);
    }

    #[test]
    fn a_frame_after_a_gap_longer_than_the_window_is_idle_and_keeps_the_fps() {
        let mut stats = FrameStats::default();
        let base = Instant::now();
        cpu_frames(&mut stats, base, &[0.004], &[0.0, 10.0, 20.0]);

        let live = stats.begin_frame_at(3, Some(0.004), ms(base, 20.0 + 1100.0));

        assert!(!live);
        assert_eq!(stats.repaint_rate, None);
        let fps = stats.fps.unwrap();
        assert!((fps - 250.0).abs() < 0.1, "{fps}");
    }

    #[test]
    fn a_burst_after_a_long_gap_measures_the_burst_repaint_rate() {
        let mut stats = FrameStats::default();
        let base = Instant::now();
        frames(&mut stats, base, 0, &[0.0, 100.0, 200.0]);

        frames(&mut stats, base, 3, &[5000.0, 5005.0, 5010.0]);

        assert_eq!(stats.repaint_rate, Some(200.0));
    }

    #[test]
    fn the_frame_starts_stay_capped() {
        let mut stats = FrameStats::default();
        let times: Vec<f64> = (0..5000).map(|i| i as f64 * 0.1).collect();

        frames(&mut stats, Instant::now(), 0, &times);

        assert_eq!(stats.frame_starts.len(), MAX_FRAME_STARTS);
        let rate = stats.repaint_rate.unwrap();
        assert!((rate - 10_000.0).abs() < 1.0, "{rate}");
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
        let ui = section(&stats, "ui").unwrap();
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

        assert_eq!(section(&stats, "inputs"), Some(timing(2.0, 3.0)));
        assert_eq!(section(&stats, "panes"), Some(timing(3.0, 4.0)));
        assert_eq!(stats.update.timing(), Some(timing(5.0, 5.0)));
    }

    #[test]
    fn a_section_splits_into_its_sub_scopes_and_the_rest_of_each_frame() {
        let mut stats = FrameStats::default();
        let base = Instant::now();
        for (frame, (length, table, ruler)) in [(10.0, 4.0, None), (6.0, 1.0, Some(2.0))]
            .into_iter()
            .enumerate()
        {
            let start = frame as f64 * 16.0;
            stats.begin_frame_at(frame as u64, None, ms(base, start));
            record_scope("table", millis(table));
            if let Some(ruler) = ruler {
                record_scope("ruler", millis(ruler));
            }
            stats.mark_at("panes", ms(base, start + length));
        }

        stats.begin_frame_at(2, None, ms(base, 32.0));

        assert_eq!(
            section_row(&stats, "panes"),
            SectionRow {
                name: "panes",
                timing: timing(8.0, 10.0),
                subs: vec![
                    ("table", timing(2.5, 4.0)),
                    ("ruler", timing(1.0, 2.0)),
                    (REST, timing(4.5, 6.0)),
                ],
            }
        );
    }

    #[test]
    fn a_section_first_marked_late_averages_over_every_finished_frame() {
        let mut stats = FrameStats::default();
        let base = Instant::now();
        stats.begin_frame_at(0, None, base);
        stats.mark_at("inputs", ms(base, 2.0));
        stats.begin_frame_at(1, None, ms(base, 16.0));
        stats.mark_at("inputs", ms(base, 18.0));
        stats.mark_at("viewers", ms(base, 22.0));

        stats.begin_frame_at(2, None, ms(base, 32.0));

        assert_eq!(section(&stats, "viewers"), Some(timing(2.0, 4.0)));
        assert_eq!(stats.update.timing(), Some(timing(4.0, 6.0)));
    }

    #[test]
    fn scopes_closed_outside_a_frames_marks_count_toward_no_section() {
        let mut stats = FrameStats::default();
        let base = Instant::now();
        record_scope("before the frame", millis(5.0));
        stats.begin_frame_at(0, None, base);
        record_scope("table", millis(1.0));
        stats.mark_at("panes", ms(base, 2.0));
        record_scope("after the last mark", millis(5.0));
        stats.begin_frame_at(1, None, ms(base, 16.0));
        stats.mark_at("panes", ms(base, 18.0));

        stats.begin_frame_at(2, None, ms(base, 32.0));

        let names: Vec<_> = section_row(&stats, "panes")
            .subs
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(names, ["table", REST]);
    }

    #[test]
    fn a_scope_inside_another_counts_only_toward_the_outer_one() {
        let mut stats = FrameStats::default();
        stats.begin_frame_at(0, None, Instant::now());
        {
            let _outer = scope("outer");
            let _inner = scope("inner");
        }
        stats.mark_at("panes", Instant::now());

        stats.begin_frame_at(1, None, Instant::now());

        let names: Vec<_> = section_row(&stats, "panes")
            .subs
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(names, ["outer", REST]);
    }

    #[test]
    fn the_cpu_outside_app_code_splits_into_the_same_frames_egui_passes_and_paint() {
        let mut stats = FrameStats::default();
        let base = Instant::now();
        let passes = |start: f64, end: f64| EguiPasses {
            start: millis(start),
            end: millis(end),
        };
        stats.begin_pass_at(0, None, passes(1.0, 0.0), base);
        stats.mark_at("ui", ms(base, 3.0));

        stats.begin_pass_at(1, Some(0.010), passes(0.5, 2.0), ms(base, 16.0));

        let cpu = stats.cpu.timing().unwrap();
        assert!((cpu.average.as_secs_f64() - 0.010).abs() < 1e-6, "{cpu:?}");
        let outside = section_row(&stats, OUTSIDE_APP);
        let near = |actual: Timing, expected_ms: f64| {
            (actual.average.as_secs_f64() * 1000.0 - expected_ms).abs() < 1e-3
        };
        assert!(near(outside.timing, 7.0), "{outside:?}");
        let [start, end, paint, ..] = &outside.subs[..] else {
            panic!("{outside:?}");
        };
        assert_eq!(*start, (EGUI_PASS_START, timing(1.0, 1.0)));
        assert_eq!(*end, (EGUI_PASS_END, timing(2.0, 2.0)));
        assert_eq!(paint.0, PAINT);
        assert!(near(paint.1, 4.0), "{outside:?}");
    }

    #[test]
    fn samples_keep_only_the_latest_frames() {
        let mut samples = Samples::default();
        for i in 0..SAMPLES as u64 + 10 {
            samples.push(Duration::from_millis(if i < 10 { 100 } else { 1 }));
        }

        assert_eq!(samples.timing(), Some(timing(1.0, 1.0)));
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
