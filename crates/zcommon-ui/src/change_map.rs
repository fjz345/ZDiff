use std::sync::{Arc, atomic::AtomicBool};

use eframe::egui::{self, Color32, mutex::Mutex};
use zcommon::{change_map::ChangeMap, job::Job};

const STRIP_SIZE: egui::Vec2 = egui::vec2(256.0, 12.0);
const THUMBNAIL_SIDE: f32 = 256.0;
const LEGEND_SIZE: egui::Vec2 = egui::vec2(128.0, 8.0);
const LEGEND_STEPS: usize = 32;

const HEAT_STOPS: [[u8; 3]; 5] = [
    [0, 0, 255],
    [0, 255, 255],
    [0, 255, 0],
    [255, 255, 0],
    [255, 0, 0],
];

/// Blue at a difference of 1 through cyan, green and yellow to red at 255.
pub fn heat_ramp(magnitude: u8) -> Color32 {
    let t = magnitude.saturating_sub(1) as f32 / 254.0 * (HEAT_STOPS.len() - 1) as f32;
    let segment = (t as usize).min(HEAT_STOPS.len() - 2);
    let f = t - segment as f32;
    let [r, g, b] = std::array::from_fn(|c| {
        let (from, to) = (
            HEAT_STOPS[segment][c] as f32,
            HEAT_STOPS[segment + 1][c] as f32,
        );
        (from + (to - from) * f).round() as u8
    });
    Color32::from_rgb(r, g, b)
}

type MapResult = Result<ChangeMap, String>;

enum Preview {
    Computing(Job<MapResult>),
    Ready(ChangeMap, Option<egui::TextureHandle>),
    Failed(String),
}

struct ChangePreviews<K> {
    capacity: usize,
    // Least recently shown first.
    entries: Vec<(K, Preview)>,
}

impl<K: PartialEq> ChangePreviews<K> {
    fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            capacity,
            entries: Vec::with_capacity(capacity),
        }
    }

    fn entry(
        &mut self,
        key: K,
        wake: impl Fn() + Send + Sync + 'static,
        work: impl FnOnce(&Arc<AtomicBool>) -> Option<MapResult> + Send + 'static,
    ) -> &mut Preview {
        match self.entries.iter().position(|(shown, _)| *shown == key) {
            Some(index) => {
                let entry = self.entries.remove(index);
                self.entries.push(entry);
            }
            None => {
                if self.entries.len() == self.capacity {
                    self.entries.remove(0);
                }
                let mut job = Job::new(wake).on_panic(Err);
                job.start(move |ctx| {
                    if let Some(result) = work(ctx.cancel_flag()) {
                        ctx.send(result);
                    }
                });
                self.entries.push((key, Preview::Computing(job)));
            }
        }
        let preview = &mut self.entries.last_mut().expect("pushed above").1;
        if let Preview::Computing(job) = preview {
            // Read before the poll: a finished job has sent everything.
            let running = job.is_running();
            *preview = match job.poll().pop() {
                Some(Ok(map)) => Preview::Ready(map, None),
                Some(Err(error)) => Preview::Failed(error),
                None if !running => Preview::Failed("cancelled".to_string()),
                None => return preview,
            };
        }
        preview
    }
}

pub fn change_preview_ui<K: PartialEq + Send + 'static>(
    ui: &mut egui::Ui,
    id: egui::Id,
    capacity: usize,
    key: K,
    work: impl FnOnce(&Arc<AtomicBool>) -> Option<MapResult> + Send + 'static,
) {
    let previews = ui.data_mut(|data| {
        data.get_temp_mut_or_insert_with(id, || {
            Arc::new(Mutex::new(ChangePreviews::<K>::new(capacity)))
        })
        .clone()
    });
    let mut previews = previews.lock();
    let ctx = ui.ctx().clone();
    let preview = previews.entry(key, move || ctx.request_repaint(), work);
    ui.set_min_width(STRIP_SIZE.x);
    match preview {
        Preview::Computing(_) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Computing...");
            });
        }
        Preview::Failed(error) => {
            ui.label(format!("No preview: {error}"));
        }
        Preview::Ready(map, texture) => map_ui(ui, map, texture),
    }
}

fn map_ui(ui: &mut egui::Ui, map: &ChangeMap, texture: &mut Option<egui::TextureHandle>) {
    match map {
        ChangeMap::Sides(sides) => {
            egui::Grid::new("change_map_sides").show(ui, |ui| {
                for (label, bins) in ["Left", "Right"].into_iter().zip(sides) {
                    ui.label(label);
                    strip(ui, bins);
                    ui.end_row();
                }
            });
            legend(ui, "lines alike", "changed");
        }
        ChangeMap::Strip(bins) => {
            strip(ui, bins);
            legend(ui, "alike", "changed");
        }
        ChangeMap::Pixels { width: 0, .. } | ChangeMap::Pixels { height: 0, .. } => {
            ui.label("No pixels");
        }
        ChangeMap::Pixels {
            width,
            height,
            magnitude,
        } => {
            let texture = texture.get_or_insert_with(|| {
                let pixels = magnitude
                    .iter()
                    .map(|&value| match value {
                        0 => Color32::TRANSPARENT,
                        value => heat_ramp(value),
                    })
                    .collect();
                let image = egui::ColorImage::new([*width as usize, *height as usize], pixels);
                ui.ctx()
                    .load_texture("change_map", image, egui::TextureOptions::NEAREST)
            });
            let scale = THUMBNAIL_SIDE / (*width).max(*height) as f32;
            let size = egui::vec2(*width as f32, *height as f32) * scale;
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
            let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
            ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
            legend(ui, "alike", "different");
        }
    }
}

fn strip(ui: &mut egui::Ui, bins: &[u8]) {
    let (rect, _) = ui.allocate_exact_size(STRIP_SIZE, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
    let width = rect.width() / bins.len().max(1) as f32;
    for (i, &value) in bins.iter().enumerate().filter(|(_, value)| **value > 0) {
        let left = rect.left() + i as f32 * width;
        let bin = egui::Rect::from_x_y_ranges(left..=left + width, rect.y_range());
        painter.rect_filled(bin, 0.0, heat_ramp(value));
    }
}

fn legend(ui: &mut egui::Ui, low: &str, high: &str) {
    ui.horizontal(|ui| {
        ui.small(low);
        let (rect, _) = ui.allocate_exact_size(LEGEND_SIZE, egui::Sense::hover());
        let width = rect.width() / LEGEND_STEPS as f32;
        for step in 0..LEGEND_STEPS {
            let left = rect.left() + step as f32 * width;
            let magnitude = 1 + step * 254 / (LEGEND_STEPS - 1);
            let part = egui::Rect::from_x_y_ranges(left..=left + width, rect.y_range());
            ui.painter()
                .rect_filled(part, 0.0, heat_ramp(magnitude as u8));
        }
        ui.small(high);
    });
}
