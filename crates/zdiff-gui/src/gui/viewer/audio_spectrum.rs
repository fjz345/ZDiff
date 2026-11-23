//! Spectrograms of both audio sides and their per-bin dB difference: a short-time Fourier
//! transform of each side's channel mix off the UI thread, drawn as cached lane textures.

use std::{
    f64::consts::PI,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use eframe::egui::{
    self, Align2, Color32, ColorImage, Rect, TextureHandle, TextureOptions, pos2, vec2,
};
use rayon::prelude::*;
use zcommon::job::Job;

use super::audio::{DecodedAudio, TimeView};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Complex {
    pub re: f32,
    pub im: f32,
}

impl Complex {
    pub fn new(re: f32, im: f32) -> Self {
        Self { re, im }
    }

    fn add(self, o: Complex) -> Complex {
        Complex::new(self.re + o.re, self.im + o.im)
    }

    fn sub(self, o: Complex) -> Complex {
        Complex::new(self.re - o.re, self.im - o.im)
    }

    fn mul(self, o: Complex) -> Complex {
        Complex::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }

    fn scale(self, s: f32) -> Complex {
        Complex::new(self.re * s, self.im * s)
    }

    fn conj(self) -> Complex {
        Complex::new(self.re, -self.im)
    }

    fn times_i(self) -> Complex {
        Complex::new(-self.im, self.re)
    }

    pub fn abs(self) -> f32 {
        self.re.hypot(self.im)
    }
}

/// Radix-2 FFT of `n` real samples through one complex FFT of `n / 2`.
#[derive(Debug)]
pub struct RealFft {
    n: usize,
    /// `e^(-2 pi i k / (n / 2))` for the half-size complex FFT.
    twiddles: Vec<Complex>,
    /// `e^(-2 pi i k / n)` for `k` in `0..=n / 2`, joining the even and odd halves.
    split: Vec<Complex>,
}

fn unit(angle: f64) -> Complex {
    Complex::new(angle.cos() as f32, angle.sin() as f32)
}

impl RealFft {
    pub fn new(n: usize) -> Result<Self, String> {
        if n < 2 || !n.is_power_of_two() {
            return Err(format!("FFT size {n} isn't a power of two of at least 2"));
        }
        let half = n / 2;
        Ok(Self {
            n,
            twiddles: (0..half / 2)
                .map(|k| unit(-2.0 * PI * k as f64 / half as f64))
                .collect(),
            split: (0..=half)
                .map(|k| unit(-2.0 * PI * k as f64 / n as f64))
                .collect(),
        })
    }

    /// In place, unnormalized both ways.
    fn complex_fft(&self, data: &mut [Complex], inverse: bool) {
        let len = data.len();
        let mut j = 0;
        for i in 1..len {
            let mut bit = len >> 1;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j |= bit;
            if i < j {
                data.swap(i, j);
            }
        }
        let mut size = 2;
        while size <= len {
            let (half, stride) = (size / 2, len / size);
            for block in data.chunks_exact_mut(size) {
                let (a, b) = block.split_at_mut(half);
                for k in 0..half {
                    let w = self.twiddles[k * stride];
                    let t = b[k].mul(if inverse { w.conj() } else { w });
                    b[k] = a[k].sub(t);
                    a[k] = a[k].add(t);
                }
            }
            size *= 2;
        }
    }

    /// The `n / 2 + 1` bins from DC to Nyquist, unnormalized.
    pub fn forward(&self, input: &[f32], out: &mut [Complex]) {
        assert_eq!(input.len(), self.n, "input length");
        assert_eq!(out.len(), self.n / 2 + 1, "output length");
        let half = self.n / 2;
        let mut z: Vec<Complex> = input
            .chunks_exact(2)
            .map(|pair| Complex::new(pair[0], pair[1]))
            .collect();
        self.complex_fft(&mut z, false);
        // z holds the even samples' spectrum plus i times the odd samples'; real input makes
        // each conjugate-symmetric, which separates them.
        for (k, out) in out.iter_mut().enumerate() {
            let zk = z[k % half];
            let zm = z[(half - k) % half].conj();
            let even = zk.add(zm).scale(0.5);
            let odd = zk.sub(zm).scale(-0.5).times_i();
            *out = even.add(self.split[k].mul(odd));
        }
    }

    #[cfg(test)]
    pub fn inverse(&self, spectrum: &[Complex], out: &mut [f32]) {
        assert_eq!(spectrum.len(), self.n / 2 + 1, "spectrum length");
        assert_eq!(out.len(), self.n, "output length");
        let half = self.n / 2;
        let mut z: Vec<Complex> = (0..half)
            .map(|k| {
                let xk = spectrum[k];
                let xm = spectrum[half - k].conj();
                let even = xk.add(xm).scale(0.5);
                let odd = xk.sub(xm).scale(0.5).mul(self.split[k].conj());
                even.add(odd.times_i())
            })
            .collect();
        self.complex_fft(&mut z, true);
        let scale = 1.0 / half as f32;
        for (pair, z) in out.chunks_exact_mut(2).zip(z) {
            pair[0] = z.re * scale;
            pair[1] = z.im * scale;
        }
    }
}

/// Periodic, so windows a quarter apart sum to a constant.
pub fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()) as f32)
        .collect()
}

pub const WINDOW_SIZES: [usize; 5] = [512, 1024, 2048, 4096, 8192];

/// Levels are dB in half-dB steps from here up to 0 dBFS.
const LEVEL_FLOOR_DB: f32 = -127.5;

fn to_level(db: f32) -> u8 {
    ((db - LEVEL_FLOOR_DB) * 2.0).round().clamp(0.0, 255.0) as u8
}

fn level_db(level: u8) -> f32 {
    LEVEL_FLOOR_DB + level as f32 * 0.5
}

/// The STFT of a side's channels mixed down, with a Hann window and 75 % overlap; one column per
/// hop, centered on frame `column * hop`. A full-scale sine reads about 0 dB in its bin.
pub struct Spectrogram {
    pub sample_rate: u32,
    pub window: usize,
    pub frames: usize,
    pub columns: usize,
    /// Column-major, `bins()` per column.
    levels: Vec<u8>,
}

impl std::fmt::Debug for Spectrogram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spectrogram")
            .field("sample_rate", &self.sample_rate)
            .field("window", &self.window)
            .field("columns", &self.columns)
            .finish()
    }
}

impl Spectrogram {
    /// `Err(None)` is a cancelled computation.
    pub fn compute(
        audio: &DecodedAudio,
        window: usize,
        cancel: &AtomicBool,
    ) -> Result<Spectrogram, Option<String>> {
        if !WINDOW_SIZES.contains(&window) {
            return Err(Some(format!(
                "window size {window} isn't one of {WINDOW_SIZES:?}"
            )));
        }
        let fft = RealFft::new(window).map_err(Some)?;
        let taper = hann(window);
        let (hop, bins) = (window / 4, window / 2 + 1);
        let (frames, channels) = (audio.frames(), audio.channels);
        let columns = frames.div_ceil(hop);
        let mut levels = Vec::new();
        columns
            .checked_mul(bins)
            .and_then(|len| levels.try_reserve_exact(len).ok())
            .ok_or_else(|| {
                Some(format!(
                    "its spectrogram of {columns} x {bins} levels doesn't fit in memory"
                ))
            })?;
        levels.resize(columns * bins, 0);
        let gain = 2.0 / taper.iter().sum::<f32>();
        let mix = 1.0 / channels as f32;
        const COLUMNS_PER_TASK: usize = 64;
        let completed = levels
            .par_chunks_mut(COLUMNS_PER_TASK * bins)
            .enumerate()
            .try_for_each_init(
                || (vec![0.0; window], vec![Complex::default(); bins]),
                |(input, spectrum), (task, task_levels)| {
                    if cancel.load(Ordering::Relaxed) {
                        return None;
                    }
                    for (offset, column_levels) in task_levels.chunks_exact_mut(bins).enumerate() {
                        let column = task * COLUMNS_PER_TASK + offset;
                        let first = (column * hop) as isize - (window / 2) as isize;
                        for (i, value) in input.iter_mut().enumerate() {
                            let frame = first + i as isize;
                            *value = if (0..frames as isize).contains(&frame) {
                                let at = frame as usize * channels;
                                audio.samples[at..at + channels].iter().sum::<f32>()
                                    * mix
                                    * taper[i]
                            } else {
                                0.0
                            };
                        }
                        fft.forward(input, spectrum);
                        for (level, bin) in column_levels.iter_mut().zip(spectrum.iter()) {
                            *level = to_level(20.0 * (bin.abs() * gain).max(1e-7).log10());
                        }
                    }
                    Some(())
                },
            );
        if completed.is_none() {
            return Err(None);
        }
        Ok(Spectrogram {
            sample_rate: audio.sample_rate,
            window,
            frames,
            columns,
            levels,
        })
    }

    pub fn bins(&self) -> usize {
        self.window / 2 + 1
    }

    pub fn hop(&self) -> usize {
        self.window / 4
    }

    pub fn nyquist(&self) -> f64 {
        self.sample_rate as f64 / 2.0
    }

    #[cfg(test)]
    pub fn db(&self, column: usize, bin: usize) -> f32 {
        level_db(self.levels[column * self.bins() + bin])
    }

    /// The loudest dB under each pixel, row-major with the top row at `top_hz`; `None` outside
    /// the audio or above its Nyquist frequency. Max-pooled, so short events survive zooming out.
    pub fn pooled_db(
        &self,
        view: TimeView,
        width: usize,
        height: usize,
        top_hz: f64,
    ) -> Vec<Option<f32>> {
        let rate = self.sample_rate as f64;
        let (hop, bins) = (self.hop() as f64, self.bins());
        let bin_hz = rate / self.window as f64;
        let nearest = |position: f64, count: usize| {
            let n = position.round().clamp(0.0, count as f64 - 1.0) as usize;
            (n, n + 1)
        };
        let rows: Vec<Option<(usize, usize)>> = (0..height)
            .map(|y| {
                let hi = top_hz * (height - y) as f64 / height as f64;
                let lo = top_hz * (height - y - 1) as f64 / height as f64;
                if lo >= self.nyquist() {
                    return None;
                }
                let a = (lo / bin_hz).ceil() as usize;
                let b = ((hi / bin_hz).ceil() as usize).min(bins);
                Some(if a < b {
                    (a, b)
                } else {
                    nearest((lo + hi) / 2.0 / bin_hz, bins)
                })
            })
            .collect();

        let mut pooled = vec![None; width * height];
        let mut column_max = vec![0u8; bins];
        for x in 0..width {
            let f0 = view.time_at(x as f32) * rate;
            let f1 = view.time_at(x as f32 + 1.0) * rate;
            if self.columns == 0 || f1 <= 0.0 || f0 >= self.frames as f64 {
                continue;
            }
            let a = (f0 / hop).ceil().max(0.0) as usize;
            let b = ((f1 / hop).ceil().max(0.0) as usize).min(self.columns);
            let (a, b) = if a < b {
                (a, b)
            } else {
                nearest((f0 + f1) / 2.0 / hop, self.columns)
            };
            column_max.fill(0);
            for column in self.levels[a * bins..b * bins].chunks_exact(bins) {
                for (max, &level) in column_max.iter_mut().zip(column) {
                    *max = (*max).max(level);
                }
            }
            for (y, row) in rows.iter().enumerate() {
                if let Some((ba, bb)) = *row {
                    let level = column_max[ba..bb].iter().max().copied().unwrap_or(0);
                    pooled[y * width + x] = Some(level_db(level));
                }
            }
        }
        pooled
    }
}

const RAMP: [[u8; 3]; 5] = [
    [0, 0, 4],
    [87, 16, 110],
    [188, 55, 84],
    [249, 142, 9],
    [252, 255, 164],
];

pub fn level_color(db: f32, floor: f32, ceil: f32) -> Color32 {
    let t = ((db - floor) / (ceil - floor).max(1e-3)).clamp(0.0, 1.0) * (RAMP.len() - 1) as f32;
    let i = (t as usize).min(RAMP.len() - 2);
    let f = t - i as f32;
    let c = |k: usize| {
        let (a, b) = (RAMP[i][k] as f32, RAMP[i + 1][k] as f32);
        (a + (b - a) * f).round() as u8
    };
    Color32::from_rgb(c(0), c(1), c(2))
}

/// `left - right` after both are raised to `floor`, from `neutral` at 0 towards `colors[0]` when
/// the left side is louder and `colors[1]` when the right one is, full at `range` dB.
pub fn diff_color(
    left: f32,
    right: f32,
    floor: f32,
    range: f32,
    neutral: Color32,
    colors: [Color32; 2],
) -> Color32 {
    let difference = left.max(floor) - right.max(floor);
    let t = (difference.abs() / range.max(1e-3)).min(1.0);
    neutral.lerp_to_gamma(colors[usize::from(difference < 0.0)], t)
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SpectrumSettings {
    pub window: usize,
    pub floor_db: f32,
    pub ceil_db: f32,
    pub diff_range_db: f32,
}

impl Default for SpectrumSettings {
    fn default() -> Self {
        Self {
            window: 2048,
            floor_db: -100.0,
            ceil_db: 0.0,
            diff_range_db: 24.0,
        }
    }
}

enum SideState {
    Empty,
    Computing,
    Ready(Arc<Spectrogram>),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
struct RenderKey {
    view: TimeView,
    width: usize,
    height: usize,
    settings: SpectrumSettings,
    colors: [Color32; 2],
    revisions: [u64; 2],
}

pub(super) const LANES: [&str; 3] = ["Left", "Right", "Difference"];
const DIFF_NEUTRAL: Color32 = Color32::from_gray(24);

type ComputeResult = Result<Spectrogram, Option<String>>;
type RenderResult = (RenderKey, [ColorImage; 3]);

/// Computes a side's spectrogram when it is decoded or the window changes, and renders the
/// visible range when the view, size or settings change, one render at a time.
pub struct SpectrumProcessor {
    sides: [SideState; 2],
    windows: [Option<usize>; 2],
    revisions: [u64; 2],
    compute_jobs: [Job<ComputeResult>; 2],
    rendering: bool,
    render_job: Job<RenderResult>,
    textures: Option<(RenderKey, [TextureHandle; 3])>,
}

impl std::fmt::Debug for SpectrumProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpectrumProcessor")
            .field("windows", &self.windows)
            .field("revisions", &self.revisions)
            .finish()
    }
}

impl Default for SpectrumProcessor {
    fn default() -> Self {
        Self {
            sides: [SideState::Empty, SideState::Empty],
            windows: [None, None],
            revisions: [0, 0],
            compute_jobs: std::array::from_fn(|_| Job::new(|| {}).on_panic(|e| Err(Some(e)))),
            rendering: false,
            render_job: Job::new(|| {}),
            textures: None,
        }
    }
}

impl SpectrumProcessor {
    /// Drops the side's spectrogram and stops its computation; the audio it came from changed.
    pub fn reset(&mut self, index: usize) {
        self.compute_jobs[index].cancel();
        self.revisions[index] += 1;
        self.windows[index] = None;
        self.sides[index] = SideState::Empty;
    }

    /// No-op once the side was computed (or is computing) with `window`.
    pub fn request(&mut self, index: usize, audio: Option<&Arc<DecodedAudio>>, window: usize) {
        let Some(audio) = audio else {
            return;
        };
        if self.windows[index] == Some(window) {
            return;
        }
        self.reset(index);
        self.windows[index] = Some(window);
        self.sides[index] = SideState::Computing;
        let audio = audio.clone();
        self.compute_jobs[index].start(move |ctx| {
            ctx.send(Spectrogram::compute(&audio, window, ctx.cancel_flag()));
        });
    }

    pub fn poll(&mut self) {
        for index in 0..2 {
            if let Some(computed) = self.compute_jobs[index].poll().pop() {
                self.sides[index] = match computed {
                    Ok(spectrogram) => SideState::Ready(Arc::new(spectrogram)),
                    Err(Some(reason)) => SideState::Failed(reason),
                    Err(None) => SideState::Empty,
                };
            }
        }
    }

    fn busy(&self) -> bool {
        self.rendering
            || self
                .sides
                .iter()
                .any(|side| matches!(side, SideState::Computing))
    }

    fn ready(&self, index: usize) -> Option<&Arc<Spectrogram>> {
        match &self.sides[index] {
            SideState::Ready(spectrogram) => Some(spectrogram),
            _ => None,
        }
    }

    fn render(&mut self, key: RenderKey) {
        self.rendering = true;
        let spectra = [self.ready(0).cloned(), self.ready(1).cloned()];
        self.render_job.start(move |ctx| {
            let images = render_lanes(&spectra, &key);
            ctx.send((key, images));
        });
    }
}

fn render_lanes(spectra: &[Option<Arc<Spectrogram>>; 2], key: &RenderKey) -> [ColorImage; 3] {
    let RenderKey {
        view,
        width,
        height,
        settings,
        colors,
        ..
    } = key;
    let top_hz = spectra
        .iter()
        .flatten()
        .map(|spectrogram| spectrogram.nyquist())
        .fold(0.0, f64::max);
    let pooled = spectra.clone().map(|spectrogram| {
        spectrogram.map(|spectrogram| spectrogram.pooled_db(*view, *width, *height, top_hz))
    });
    let image = |pixel: &dyn Fn(usize) -> Option<Color32>| {
        ColorImage::new(
            [*width, *height],
            (0..width * height)
                .map(|i| pixel(i).unwrap_or(Color32::TRANSPARENT))
                .collect(),
        )
    };
    let side = |index: usize| {
        image(&|i| {
            let db = pooled[index].as_ref()?[i]?;
            Some(level_color(db, settings.floor_db, settings.ceil_db))
        })
    };
    let diff = image(&|i| {
        let left = pooled[0].as_ref()?[i]?;
        let right = pooled[1].as_ref()?[i]?;
        Some(diff_color(
            left,
            right,
            settings.floor_db,
            settings.diff_range_db,
            DIFF_NEUTRAL,
            *colors,
        ))
    });
    [side(0), side(1), diff]
}

pub fn show_controls(ui: &mut egui::Ui, settings: &mut SpectrumSettings) {
    egui::ComboBox::from_id_salt("audio_spectrum_window")
        .selected_text(format!("Window {}", settings.window))
        .show_ui(ui, |ui| {
            for size in WINDOW_SIZES {
                ui.selectable_value(&mut settings.window, size, size.to_string());
            }
        })
        .response
        .on_hover_text("FFT window size: larger gives finer frequencies, coarser time");
    ui.label("Range");
    ui.add(
        egui::DragValue::new(&mut settings.floor_db)
            .range(LEVEL_FLOOR_DB..=settings.ceil_db - 1.0)
            .suffix(" dB"),
    )
    .on_hover_text("Level shown black");
    ui.add(
        egui::DragValue::new(&mut settings.ceil_db)
            .range(settings.floor_db + 1.0..=0.0)
            .suffix(" dB"),
    )
    .on_hover_text("Level shown brightest");
    ui.label("Difference");
    ui.add(
        egui::DragValue::new(&mut settings.diff_range_db)
            .range(1.0..=120.0)
            .prefix("±")
            .suffix(" dB"),
    )
    .on_hover_text("Level difference shown in full color");
}

pub fn show_lanes(
    ui: &egui::Ui,
    rect: Rect,
    processor: &mut SpectrumProcessor,
    sides: [(Option<&Arc<DecodedAudio>>, String); 2],
    view: TimeView,
    settings: &SpectrumSettings,
    colors: [Color32; 2],
) {
    processor.poll();
    for (index, (audio, _)) in sides.iter().enumerate() {
        processor.request(index, *audio, settings.window);
    }
    let status = sides.map(|(_, status)| status);
    for (key, images) in processor.render_job.poll() {
        processor.rendering = false;
        let options = TextureOptions::LINEAR;
        let textures = match processor.textures.take() {
            Some((_, mut textures)) => {
                for (texture, image) in textures.iter_mut().zip(images) {
                    texture.set(image, options);
                }
                textures
            }
            None => images.map(|image| ui.ctx().load_texture("audio_spectrum", image, options)),
        };
        processor.textures = Some((key, textures));
    }

    let ppp = ui.ctx().pixels_per_point();
    let lane_height = rect.height() / LANES.len() as f32;
    let key = RenderKey {
        view: TimeView {
            secs_per_px: view.secs_per_px / ppp as f64,
            ..view
        },
        width: (rect.width() * ppp).round().max(1.0) as usize,
        height: (lane_height * ppp).round().max(1.0) as usize,
        settings: settings.clone(),
        colors,
        revisions: processor.revisions,
    };
    let computing = processor
        .sides
        .iter()
        .any(|side| matches!(side, SideState::Computing));
    let stale = processor
        .textures
        .as_ref()
        .is_none_or(|(old, _)| *old != key);
    if !computing && !processor.rendering && stale {
        processor.render(key);
    }
    if processor.busy() {
        ui.ctx().request_repaint_after(Duration::from_millis(50));
    }

    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    let line = egui::Stroke::new(1.0_f32, visuals.widgets.noninteractive.bg_stroke.color);
    for (i, label) in LANES.iter().enumerate() {
        let lane = Rect::from_min_size(
            pos2(rect.left(), rect.top() + i as f32 * lane_height),
            vec2(rect.width(), lane_height),
        );
        if let Some((old, textures)) = &processor.textures {
            let start = view.x_of(old.view.start);
            let end = view.x_of(old.view.start + old.width as f64 * old.view.secs_per_px);
            painter.image(
                textures[i].id(),
                Rect::from_x_y_ranges(lane.left() + start..=lane.left() + end, lane.y_range()),
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        painter.hline(lane.x_range(), lane.bottom(), line);
        painter.text(
            lane.left_top() + vec2(4.0, 2.0),
            Align2::LEFT_TOP,
            *label,
            egui::TextStyle::Small.resolve(ui.style()),
            visuals.weak_text_color(),
        );
        let message = match i {
            2 => (0..2).find_map(|index| side_message(processor, index, &status[index])),
            index => side_message(processor, index, &status[index]),
        };
        if let Some(message) = message {
            painter.text(
                lane.center(),
                Align2::CENTER_CENTER,
                message,
                egui::TextStyle::Body.resolve(ui.style()),
                visuals.text_color(),
            );
        }
    }
}

fn side_message(processor: &SpectrumProcessor, index: usize, status: &str) -> Option<String> {
    match &processor.sides[index] {
        SideState::Empty => Some(status.to_string()),
        SideState::Computing => Some("computing spectrogram...".to_string()),
        SideState::Ready(_) => None,
        SideState::Failed(reason) => Some(reason.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize) -> Vec<f32> {
        let mut state = 0x2545_f491_u32;
        (0..n)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1 << 23) as f32 - 1.0
            })
            .collect()
    }

    fn sine(n: usize, cycles_per_n: f64, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| (2.0 * PI * cycles_per_n * i as f64 / n as f64).sin() as f32)
            .collect()
    }

    #[test]
    fn sizes_that_are_not_a_power_of_two_are_rejected() {
        for n in [0, 1, 3, 1000, 2047] {
            assert!(RealFft::new(n).is_err(), "{n}");
        }
        for n in [2, 4, 512, 8192] {
            assert!(RealFft::new(n).is_ok(), "{n}");
        }
    }

    #[test]
    fn a_pure_sine_lands_in_its_bin() {
        for (n, k) in [(8, 1), (64, 5), (2048, 300), (8192, 4095)] {
            let fft = RealFft::new(n).unwrap();
            let mut out = vec![Complex::default(); n / 2 + 1];
            fft.forward(&sine(n, k as f64, n), &mut out);
            for (bin, value) in out.iter().enumerate() {
                let expected = if bin == k { n as f32 / 2.0 } else { 0.0 };
                assert!(
                    (value.abs() - expected).abs() < 1e-3 * n as f32,
                    "n {n} bin {bin}: {} != {expected}",
                    value.abs()
                );
            }
        }
    }

    #[test]
    fn dc_and_nyquist_are_real() {
        let fft = RealFft::new(4).unwrap();
        let mut out = vec![Complex::default(); 3];
        let close =
            |a: Complex, b: Complex| (a.re - b.re).abs() < 1e-5 && (a.im - b.im).abs() < 1e-5;
        fft.forward(&[1.0, -1.0, 1.0, -1.0], &mut out);
        assert!(close(out[0], Complex::new(0.0, 0.0)), "{out:?}");
        assert!(close(out[2], Complex::new(4.0, 0.0)), "{out:?}");
        fft.forward(&[1.0, 2.0, 3.0, 4.0], &mut out);
        assert!(close(out[0], Complex::new(10.0, 0.0)), "{out:?}");
        assert!(close(out[1], Complex::new(-2.0, 2.0)), "{out:?}");
        assert!(close(out[2], Complex::new(-2.0, 0.0)), "{out:?}");
    }

    #[test]
    fn the_inverse_round_trip_is_close_to_identity() {
        for n in [2, 4, 16, 512, 8192] {
            let fft = RealFft::new(n).unwrap();
            let input = noise(n);
            let mut spectrum = vec![Complex::default(); n / 2 + 1];
            let mut output = vec![0.0; n];
            fft.forward(&input, &mut spectrum);
            fft.inverse(&spectrum, &mut output);
            for (i, (a, b)) in input.iter().zip(&output).enumerate() {
                assert!((a - b).abs() < 1e-4, "n {n} sample {i}: {a} != {b}");
            }
        }
    }

    #[test]
    fn hann_windows_a_quarter_apart_sum_to_a_constant() {
        let window = hann(16);
        assert_eq!(window[0], 0.0);
        assert!((window[8] - 1.0).abs() < 1e-6);
        for i in 0..4 {
            let sum: f32 = (0..4).map(|k| window[i + 4 * k]).sum();
            assert!((sum - 2.0).abs() < 1e-5, "{sum}");
        }
    }

    fn audio(sample_rate: u32, channels: usize, samples: Vec<f32>) -> DecodedAudio {
        DecodedAudio::new(sample_rate, channels, samples)
    }

    fn compute(audio: &DecodedAudio, window: usize) -> Spectrogram {
        Spectrogram::compute(audio, window, &AtomicBool::new(false)).unwrap()
    }

    fn loudest_bin(spectrogram: &Spectrogram, column: usize) -> usize {
        (0..spectrogram.bins())
            .max_by(|&a, &b| {
                spectrogram
                    .db(column, a)
                    .total_cmp(&spectrogram.db(column, b))
            })
            .unwrap()
    }

    #[test]
    fn a_full_scale_sine_reads_about_0_db_in_its_bin_with_a_quarter_window_hop() {
        let window = 512;
        let spectrogram = compute(&audio(8000, 1, sine(window, 40.0, 4000)), window);
        assert_eq!(spectrogram.hop(), 128);
        assert_eq!(spectrogram.columns, 4000usize.div_ceil(128));
        assert_eq!(spectrogram.bins(), 257);
        let middle = spectrogram.columns / 2;
        assert_eq!(loudest_bin(&spectrogram, middle), 40);
        assert!(spectrogram.db(middle, 40).abs() <= 0.5);
        assert!(spectrogram.db(middle, 100) < -80.0);
    }

    #[test]
    fn the_channels_are_mixed_down() {
        let left = sine(512, 40.0, 2048);
        let right = sine(512, 100.0, 2048);
        let samples = left
            .iter()
            .zip(&right)
            .flat_map(|(&l, &r)| [l, r])
            .collect();
        let spectrogram = compute(&audio(8000, 2, samples), 512);
        let column = spectrogram.columns / 2;
        for bin in [40, 100] {
            assert!((spectrogram.db(column, bin) + 6.0).abs() < 0.6);
        }
    }

    #[test]
    fn unsupported_windows_and_cancellation_are_errors() {
        let audio = audio(8000, 1, sine(512, 40.0, 2048));
        let error = Spectrogram::compute(&audio, 1000, &AtomicBool::new(false)).unwrap_err();
        assert!(error.unwrap().contains("1000"));
        assert!(
            Spectrogram::compute(&audio, 512, &AtomicBool::new(true)).is_err_and(|e| e.is_none())
        );
    }

    #[test]
    fn short_audio_has_one_column_and_empty_audio_none() {
        assert_eq!(compute(&audio(8000, 1, vec![0.5; 10]), 512).columns, 1);
        assert_eq!(compute(&audio(8000, 1, Vec::new()), 512).columns, 0);
    }

    #[test]
    fn pooling_puts_a_sine_in_its_row_and_nothing_outside_the_audio() {
        let spectrogram = compute(&audio(8000, 1, sine(512, 128.0, 8000)), 512);
        let view = TimeView {
            start: -0.25,
            secs_per_px: 0.25,
        };
        let pooled = spectrogram.pooled_db(view, 6, 8, 4000.0);
        let at = |x: usize, y: usize| pooled[y * 6 + x];
        for y in 0..8 {
            assert_eq!(at(0, y), None);
            assert_eq!(at(5, y), None);
        }
        for x in 1..5 {
            let loudest = (0..8)
                .max_by(|&a, &b| at(x, a).unwrap().total_cmp(&at(x, b).unwrap()))
                .unwrap();
            assert_eq!(loudest, 3, "column {x}");
            assert!(at(x, 3).unwrap().abs() <= 0.5);
        }
    }

    #[test]
    fn rows_above_the_nyquist_frequency_are_empty() {
        let spectrogram = compute(&audio(8000, 1, sine(512, 128.0, 8000)), 512);
        let view = TimeView {
            start: 0.0,
            secs_per_px: 0.5,
        };
        let pooled = spectrogram.pooled_db(view, 2, 4, 8000.0);
        assert!(pooled[..4].iter().all(Option::is_none));
        assert!(pooled[4..].iter().all(Option::is_some));
    }

    #[test]
    fn a_zoomed_out_pixel_keeps_a_short_burst() {
        let mut samples = vec![0.0; 48000];
        samples[24000..24256].copy_from_slice(&sine(512, 64.0, 256));
        let spectrogram = compute(&audio(48000, 1, samples), 512);
        let view = TimeView {
            start: 0.0,
            secs_per_px: 1.0,
        };
        let pooled = spectrogram.pooled_db(view, 1, 1, 24000.0);
        assert!(pooled[0].unwrap() > -20.0, "{:?}", pooled[0]);
    }

    #[test]
    fn a_zoomed_in_pixel_reads_the_nearest_column() {
        let mut samples = sine(512, 64.0, 4096);
        samples[2048..].fill(0.0);
        let spectrogram = compute(&audio(8000, 1, samples), 512);
        let px = |time: f64| {
            let view = TimeView {
                start: time,
                secs_per_px: 1e-5,
            };
            spectrogram.pooled_db(view, 1, 1, 4000.0)[0].unwrap()
        };
        assert!(px(0.1) > -1.0);
        assert!(px(0.45) < -100.0);
    }

    #[test]
    fn the_level_ramp_runs_from_black_to_pale_yellow_and_clamps() {
        assert_eq!(level_color(-100.0, -100.0, 0.0), Color32::from_rgb(0, 0, 4));
        assert_eq!(level_color(-150.0, -100.0, 0.0), Color32::from_rgb(0, 0, 4));
        assert_eq!(
            level_color(0.0, -100.0, 0.0),
            Color32::from_rgb(252, 255, 164)
        );
        assert_eq!(
            level_color(10.0, -100.0, 0.0),
            Color32::from_rgb(252, 255, 164)
        );
        assert_eq!(
            level_color(-50.0, -100.0, 0.0),
            Color32::from_rgb(188, 55, 84)
        );
        assert_eq!(level_color(-5.0, 0.0, 0.0), Color32::from_rgb(0, 0, 4));
    }

    #[test]
    fn differences_run_from_neutral_to_the_louder_sides_color() {
        let neutral = Color32::from_gray(24);
        let colors = [Color32::RED, Color32::GREEN];
        let diff = |left, right| diff_color(left, right, -100.0, 20.0, neutral, colors);
        assert_eq!(diff(-30.0, -30.0), neutral);
        assert_eq!(diff(-10.0, -30.0), Color32::RED);
        assert_eq!(diff(-50.0, -10.0), Color32::GREEN);
        assert_eq!(diff(-120.0, -140.0), neutral);
        assert_eq!(diff(-20.0, -30.0), neutral.lerp_to_gamma(Color32::RED, 0.5));
    }

    #[test]
    fn a_side_is_computed_off_the_ui_thread_and_reset_when_its_audio_changes() {
        let audio = Arc::new(audio(8000, 1, sine(512, 40.0, 4000)));
        let mut processor = SpectrumProcessor::default();
        processor.request(0, Some(&audio), 512);
        let start = std::time::Instant::now();
        while matches!(processor.sides[0], SideState::Computing) {
            assert!(start.elapsed() < Duration::from_secs(20), "never finished");
            std::thread::sleep(Duration::from_millis(5));
            processor.poll();
        }
        assert_eq!(processor.ready(0).unwrap().window, 512);
        processor.request(0, Some(&audio), 512);
        assert!(processor.ready(0).is_some());
        processor.request(0, Some(&audio), 1024);
        assert!(matches!(processor.sides[0], SideState::Computing));
        processor.reset(0);
        assert!(matches!(processor.sides[0], SideState::Empty));
        assert!(processor.ready(1).is_none());
    }
}
