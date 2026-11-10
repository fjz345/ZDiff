use std::{
    borrow::Cow,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};

use rayon::prelude::*;

use crate::pixel::{PixelBounds, PixelDiff, PixelStats, RgbaBuffer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PixelMetric {
    /// The largest channel difference at the same pixel.
    #[default]
    Exact,
    /// The largest channel difference to the closest match within the window in the other image,
    /// the worse of both directions.
    Neighborhood(NeighborhoodSize),
    /// Luma SSIM over a 7x7 window, as (1 - SSIM) x 255.
    Ssim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum NeighborhoodSize {
    #[default]
    Three,
    Five,
    Seven,
}

impl NeighborhoodSize {
    fn radius(self) -> usize {
        match self {
            NeighborhoodSize::Three => 1,
            NeighborhoodSize::Five => 2,
            NeighborhoodSize::Seven => 3,
        }
    }
}

impl PixelMetric {
    pub const ALL: [PixelMetric; 5] = [
        PixelMetric::Exact,
        PixelMetric::Neighborhood(NeighborhoodSize::Three),
        PixelMetric::Neighborhood(NeighborhoodSize::Five),
        PixelMetric::Neighborhood(NeighborhoodSize::Seven),
        PixelMetric::Ssim,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PixelMetric::Exact => "Exact",
            PixelMetric::Neighborhood(NeighborhoodSize::Three) => "Neighborhood 3x3",
            PixelMetric::Neighborhood(NeighborhoodSize::Five) => "Neighborhood 5x5",
            PixelMetric::Neighborhood(NeighborhoodSize::Seven) => "Neighborhood 7x7",
            PixelMetric::Ssim => "SSIM",
        }
    }
}

/// Where each image of a pair sits on the canvas, the larger width by the larger height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Alignment {
    #[default]
    TopLeft,
    Center,
    /// Each image bilinearly resampled to the whole canvas.
    ScaleToFit,
}

impl Alignment {
    pub const ALL: [Alignment; 3] = [Alignment::TopLeft, Alignment::Center, Alignment::ScaleToFit];

    pub fn label(self) -> &'static str {
        match self {
            Alignment::TopLeft => "Top left",
            Alignment::Center => "Center",
            Alignment::ScaleToFit => "Scale to fit",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetricMap {
    pub width: u32,
    pub height: u32,
    /// Row-major over the canvas: 0 is alike, 255 totally different.
    pub magnitude: Vec<u8>,
    /// The canvas pixels both aligned images cover; every other pixel is changed at any tolerance.
    pub overlap: PixelBounds,
    /// The mean of `magnitude` before rounding, over the whole canvas, in 0..=1.
    pub difference: f64,
}

/// An empty image has no pixels to scale, so it stays empty at the canvas origin.
pub fn placement(
    alignment: Alignment,
    (width, height): (u32, u32),
    (canvas_width, canvas_height): (u32, u32),
) -> PixelBounds {
    assert!(
        width <= canvas_width && height <= canvas_height,
        "{width} x {height} doesn't fit on {canvas_width} x {canvas_height}"
    );
    let (x, y) = match alignment {
        Alignment::ScaleToFit if width > 0 && height > 0 => {
            return PixelBounds {
                x: 0..canvas_width,
                y: 0..canvas_height,
            };
        }
        Alignment::TopLeft | Alignment::ScaleToFit => (0, 0),
        Alignment::Center => ((canvas_width - width) / 2, (canvas_height - height) / 2),
    };
    PixelBounds {
        x: x..x + width,
        y: y..y + height,
    }
}

pub fn metric_map(
    first: RgbaBuffer,
    second: RgbaBuffer,
    metric: PixelMetric,
    alignment: Alignment,
    cancel_flag: &AtomicBool,
) -> Option<MetricMap> {
    for image in [&first, &second] {
        assert_eq!(
            image.rgba.len(),
            image.width as usize * image.height as usize * 4,
            "RGBA length doesn't match {} x {}",
            image.width,
            image.height
        );
    }
    if cancel_flag.load(Ordering::Acquire) {
        return None;
    }

    let canvas = (
        first.width.max(second.width),
        first.height.max(second.height),
    );
    let (first_rgba, first_bounds) = place(first, alignment, canvas, cancel_flag)?;
    let (second_rgba, second_bounds) = place(second, alignment, canvas, cancel_flag)?;
    let overlap = PixelBounds {
        x: intersect(&first_bounds.x, &second_bounds.x),
        y: intersect(&first_bounds.y, &second_bounds.y),
    };
    let pair = (!overlap.x.is_empty() && !overlap.y.is_empty()).then(|| {
        (
            View::crop(&first_rgba, &first_bounds, &overlap),
            View::crop(&second_rgba, &second_bounds, &overlap),
        )
    });
    let overlap = match pair {
        Some(_) => overlap,
        None => PixelBounds { x: 0..0, y: 0..0 },
    };

    let (width, height) = (canvas.0 as usize, canvas.1 as usize);
    let (overlap_x, overlap_y) = (
        overlap.x.start as usize..overlap.x.end as usize,
        overlap.y.start as usize..overlap.y.end as usize,
    );
    let mut magnitude = vec![255; width * height];
    let row_sums: Vec<f64> = magnitude
        .par_chunks_mut(width.max(1))
        .enumerate()
        .with_min_len(16)
        .map_init(Vec::new, |scratch, (y, row)| {
            if cancel_flag.load(Ordering::Acquire) {
                return None;
            }
            let Some((a, b)) = pair.filter(|_| overlap_y.contains(&y)) else {
                return Some(width as f64 * 255.0);
            };
            let out = &mut row[overlap_x.clone()];
            let y = y - overlap_y.start;
            let inside = match metric {
                PixelMetric::Exact => exact_row(a, b, y, out),
                PixelMetric::Neighborhood(size) => neighborhood_row(a, b, y, size.radius(), out),
                PixelMetric::Ssim => ssim_row(a, b, y, out, scratch),
            };
            Some(inside + (width - overlap_x.len()) as f64 * 255.0)
        })
        .collect::<Option<_>>()?;

    let total = (width * height) as f64 * 255.0;
    Some(MetricMap {
        width: canvas.0,
        height: canvas.1,
        magnitude,
        overlap,
        difference: if total == 0.0 {
            0.0
        } else {
            row_sums.iter().sum::<f64>() / total
        },
    })
}

pub fn changed_pixels(map: &MetricMap, tolerance: u8) -> PixelDiff {
    let width = map.width as usize;
    let overlap_x = map.overlap.x.start as usize..map.overlap.x.end as usize;
    let mut mask = vec![true; map.magnitude.len()];
    let changed_rows: Vec<Option<(u64, usize, usize)>> = mask
        .par_chunks_mut(width.max(1))
        .zip(map.magnitude.par_chunks(width.max(1)))
        .enumerate()
        .with_min_len(16)
        .map(|(y, (row, magnitude))| {
            if map.overlap.y.contains(&(y as u32)) {
                for (changed, &value) in row[overlap_x.clone()]
                    .iter_mut()
                    .zip(&magnitude[overlap_x.clone()])
                {
                    *changed = value > tolerance;
                }
            }
            let (Some(first_x), Some(last_x)) =
                (row.iter().position(|&c| c), row.iter().rposition(|&c| c))
            else {
                return None;
            };
            let count = row.iter().filter(|&&c| c).count() as u64;
            Some((count, first_x, last_x))
        })
        .collect();

    let mut changed = 0;
    let mut bounds: Option<PixelBounds> = None;
    for (y, row) in changed_rows.into_iter().enumerate() {
        let Some((count, first_x, last_x)) = row else {
            continue;
        };
        changed += count;
        let (x, y) = (first_x as u32..last_x as u32 + 1, y as u32);
        bounds = Some(match bounds {
            None => PixelBounds { x, y: y..y + 1 },
            Some(b) => PixelBounds {
                x: b.x.start.min(x.start)..b.x.end.max(x.end),
                y: b.y.start..y + 1,
            },
        });
    }

    PixelDiff {
        width: map.width,
        height: map.height,
        mask,
        stats: PixelStats {
            changed,
            total: map.magnitude.len() as u64,
            bounds,
        },
    }
}

fn intersect(a: &Range<u32>, b: &Range<u32>) -> Range<u32> {
    let start = a.start.max(b.start);
    start..a.end.min(b.end).max(start)
}

fn place<'a>(
    image: RgbaBuffer<'a>,
    alignment: Alignment,
    canvas: (u32, u32),
    cancel_flag: &AtomicBool,
) -> Option<(Cow<'a, [u8]>, PixelBounds)> {
    let bounds = placement(alignment, (image.width, image.height), canvas);
    let size = (bounds.x.len() as u32, bounds.y.len() as u32);
    if size == (image.width, image.height) {
        return Some((Cow::Borrowed(image.rgba), bounds));
    }
    let resampled = resample(image, size, cancel_flag)?;
    Some((Cow::Owned(resampled), bounds))
}

fn resample(
    image: RgbaBuffer,
    (width, height): (u32, u32),
    cancel_flag: &AtomicBool,
) -> Option<Vec<u8>> {
    // Pixel centers map onto pixel centers; each tap is (lower, upper, weight of upper).
    let taps = |from: u32, to: u32| -> Vec<(usize, usize, f32)> {
        (0..to)
            .map(|i| {
                let source = ((i as f32 + 0.5) * from as f32 / to as f32 - 0.5)
                    .clamp(0.0, (from - 1) as f32);
                let lower = source as usize;
                (
                    lower,
                    (lower + 1).min(from as usize - 1),
                    source - lower as f32,
                )
            })
            .collect()
    };
    let (columns, rows) = (taps(image.width, width), taps(image.height, height));
    let stride = image.width as usize * 4;
    let mut out = vec![0; width as usize * height as usize * 4];
    out.par_chunks_mut(width as usize * 4)
        .zip(rows.par_iter())
        .try_for_each(|(row, &(top, bottom, fy))| {
            if cancel_flag.load(Ordering::Acquire) {
                return None;
            }
            let (top, bottom) = (
                &image.rgba[top * stride..][..stride],
                &image.rgba[bottom * stride..][..stride],
            );
            for (pixel, &(left, right, fx)) in row.chunks_exact_mut(4).zip(&columns) {
                for (c, value) in pixel.iter_mut().enumerate() {
                    let lerp = |a: u8, b: u8, f: f32| a as f32 + (b as f32 - a as f32) * f;
                    let upper = lerp(top[left * 4 + c], top[right * 4 + c], fx);
                    let lower = lerp(bottom[left * 4 + c], bottom[right * 4 + c], fx);
                    *value = (upper + (lower - upper) * fy).round() as u8;
                }
            }
            Some(())
        })?;
    Some(out)
}

/// Same-size crops of both aligned images, over their overlap.
#[derive(Clone, Copy)]
struct View<'a> {
    rgba: &'a [u8],
    stride: usize,
    width: usize,
    height: usize,
}

impl<'a> View<'a> {
    fn crop(rgba: &'a [u8], image: &PixelBounds, area: &PixelBounds) -> Self {
        let stride = image.x.len();
        let x = (area.x.start - image.x.start) as usize;
        let y = (area.y.start - image.y.start) as usize;
        View {
            rgba: &rgba[(y * stride + x) * 4..],
            stride,
            width: area.x.len(),
            height: area.y.len(),
        }
    }

    fn row(&self, y: usize) -> &'a [u8] {
        let start = y * self.stride * 4;
        &self.rgba[start..start + self.width * 4]
    }
}

fn largest_channel_difference(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0)
}

fn window(center: usize, radius: usize, len: usize) -> Range<usize> {
    center.saturating_sub(radius)..(center + radius + 1).min(len)
}

fn exact_row(a: View, b: View, y: usize, out: &mut [u8]) -> f64 {
    let (row_a, row_b) = (a.row(y), b.row(y));
    // Equal rows (most of them, in a typical edit) skip the per-pixel walk.
    if row_a == row_b {
        out.fill(0);
        return 0.0;
    }
    let mut sum = 0;
    for (value, (a, b)) in out
        .iter_mut()
        .zip(row_a.chunks_exact(4).zip(row_b.chunks_exact(4)))
    {
        *value = largest_channel_difference(a, b);
        sum += *value as u64;
    }
    sum as f64
}

fn neighborhood_row(a: View, b: View, y: usize, radius: usize, out: &mut [u8]) -> f64 {
    let (row_a, row_b) = (a.row(y), b.row(y));
    if row_a == row_b {
        out.fill(0);
        return 0.0;
    }
    let rows = window(y, radius, a.height);
    let mut sum = 0;
    for (x, value) in out.iter_mut().enumerate() {
        let (pixel_a, pixel_b) = (&row_a[x * 4..][..4], &row_b[x * 4..][..4]);
        let center = largest_channel_difference(pixel_a, pixel_b);
        if center > 0 {
            let columns = window(x, radius, a.width);
            let forward = closest(pixel_a, b, rows.clone(), columns.clone(), center, 0);
            *value = closest(pixel_b, a, rows.clone(), columns, center, forward).max(forward);
        } else {
            *value = 0;
        }
        sum += *value as u64;
    }
    sum as f64
}

// Stops once the best is at `floor` or below, where the caller's max can't change any more.
fn closest(
    pixel: &[u8],
    other: View,
    rows: Range<usize>,
    columns: Range<usize>,
    center: u8,
    floor: u8,
) -> u8 {
    let mut best = center;
    for y in rows {
        let row = other.row(y);
        for x in columns.clone() {
            best = best.min(largest_channel_difference(pixel, &row[x * 4..][..4]));
            if best <= floor {
                return best;
            }
        }
    }
    best
}

const SSIM_RADIUS: usize = 3;
const SSIM_C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
const SSIM_C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);

// Premultiplied, so the hidden color of a transparent pixel doesn't count.
fn luma(pixel: &[u8]) -> f64 {
    (0.2126 * pixel[0] as f64 + 0.7152 * pixel[1] as f64 + 0.0722 * pixel[2] as f64)
        * pixel[3] as f64
        / 255.0
}

fn ssim_row(a: View, b: View, y: usize, out: &mut [u8], sums: &mut Vec<[f64; 5]>) -> f64 {
    let rows = window(y, SSIM_RADIUS, a.height);
    if rows.clone().all(|y| a.row(y) == b.row(y)) {
        out.fill(0);
        return 0.0;
    }
    // Per column over the window's rows: sum of a, b, a^2, b^2 and ab.
    sums.clear();
    sums.resize(a.width, [0.0; 5]);
    for y in rows.clone() {
        let pixels = a.row(y).chunks_exact(4).zip(b.row(y).chunks_exact(4));
        for (sum, (pixel_a, pixel_b)) in sums.iter_mut().zip(pixels) {
            let (la, lb) = (luma(pixel_a), luma(pixel_b));
            for (sum, value) in sum.iter_mut().zip([la, lb, la * la, lb * lb, la * lb]) {
                *sum += value;
            }
        }
    }
    let mut total = 0.0;
    for (x, value) in out.iter_mut().enumerate() {
        let columns = window(x, SSIM_RADIUS, a.width);
        let count = (rows.len() * columns.len()) as f64;
        let mut moments = [0.0; 5];
        for sum in &sums[columns] {
            for (moment, sum) in moments.iter_mut().zip(sum) {
                *moment += sum;
            }
        }
        let [mean_a, mean_b, square_a, square_b, product] = moments.map(|m| m / count);
        let (variance_a, variance_b) = (square_a - mean_a * mean_a, square_b - mean_b * mean_b);
        let covariance = product - mean_a * mean_b;
        let ssim = ((2.0 * mean_a * mean_b + SSIM_C1) * (2.0 * covariance + SSIM_C2))
            / ((mean_a * mean_a + mean_b * mean_b + SSIM_C1) * (variance_a + variance_b + SSIM_C2));
        let difference = (1.0 - ssim.clamp(0.0, 1.0)) * 255.0;
        *value = difference.round() as u8;
        total += difference;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(width: u32, height: u32, rgba: &[u8]) -> RgbaBuffer<'_> {
        RgbaBuffer {
            width,
            height,
            rgba,
        }
    }

    fn gray(values: &[u8]) -> Vec<u8> {
        values.iter().flat_map(|&v| [v, v, v, 255]).collect()
    }

    fn map(
        first: RgbaBuffer,
        second: RgbaBuffer,
        metric: PixelMetric,
        alignment: Alignment,
    ) -> MetricMap {
        metric_map(first, second, metric, alignment, &AtomicBool::new(false))
            .expect("not cancelled")
    }

    const THREE: PixelMetric = PixelMetric::Neighborhood(NeighborhoodSize::Three);

    // A one-pixel vertical line at `line_x` on black.
    fn line(width: u32, height: u32, line_x: u32) -> Vec<u8> {
        let row: Vec<u8> = (0..width)
            .map(|x| if x == line_x { 200 } else { 0 })
            .collect();
        gray(&row.repeat(height as usize))
    }

    #[test]
    fn placement_puts_each_image_at_the_corner_the_middle_or_over_the_canvas() {
        let bounds = |x: Range<u32>, y: Range<u32>| PixelBounds { x, y };
        assert_eq!(
            placement(Alignment::TopLeft, (1, 2), (4, 5)),
            bounds(0..1, 0..2)
        );
        assert_eq!(
            placement(Alignment::Center, (1, 2), (4, 5)),
            bounds(1..2, 1..3)
        );
        assert_eq!(
            placement(Alignment::ScaleToFit, (1, 2), (4, 5)),
            bounds(0..4, 0..5)
        );
        assert_eq!(
            placement(Alignment::ScaleToFit, (0, 0), (4, 5)),
            bounds(0..0, 0..0)
        );
        assert_eq!(
            placement(Alignment::Center, (4, 5), (4, 5)),
            bounds(0..4, 0..5)
        );
    }

    #[test]
    fn exact_magnitude_is_the_largest_channel_difference_and_the_rest_is_full() {
        let first = [10, 20, 30, 255, 7, 7, 7, 7];
        let second = [12, 90, 25, 255, 7, 7, 7, 7, 1, 1, 1, 1];
        let m = map(
            buffer(2, 1, &first),
            buffer(3, 1, &second),
            PixelMetric::Exact,
            Alignment::TopLeft,
        );
        assert_eq!((m.width, m.height), (3, 1));
        assert_eq!(m.magnitude, [70, 0, 255]);
        assert_eq!(m.overlap, PixelBounds { x: 0..2, y: 0..1 });
        assert!((m.difference - (70.0 + 255.0) / (3.0 * 255.0)).abs() < 1e-12);
    }

    #[test]
    fn outside_the_overlap_is_changed_even_at_the_largest_tolerance() {
        let first = gray(&[5]);
        let second = gray(&[5, 5]);
        let m = map(
            buffer(1, 1, &first),
            buffer(2, 1, &second),
            PixelMetric::Exact,
            Alignment::TopLeft,
        );
        let diff = changed_pixels(&m, 255);
        assert_eq!(diff.mask, [false, true]);
        assert_eq!(
            diff.stats,
            PixelStats {
                changed: 1,
                total: 2,
                bounds: Some(PixelBounds { x: 1..2, y: 0..1 }),
            }
        );
    }

    #[test]
    fn neighborhood_forgives_a_one_pixel_shift() {
        let (first, second) = (line(9, 4, 4), line(9, 4, 5));
        let (first, second) = (buffer(9, 4, &first), buffer(9, 4, &second));
        let exact = map(first, second, PixelMetric::Exact, Alignment::TopLeft);
        assert_eq!(changed_pixels(&exact, 0).stats.changed, 8);

        let near = map(first, second, THREE, Alignment::TopLeft);
        assert_eq!(near.magnitude, vec![0; 36]);
        assert_eq!(near.difference, 0.0);

        let far = line(9, 4, 6);
        let far = buffer(9, 4, &far);
        assert_eq!(
            changed_pixels(&map(first, far, THREE, Alignment::TopLeft), 0)
                .stats
                .changed,
            8
        );
        let five = PixelMetric::Neighborhood(NeighborhoodSize::Five);
        assert_eq!(
            map(first, far, five, Alignment::TopLeft).magnitude,
            vec![0; 36]
        );
    }

    #[test]
    fn neighborhood_still_flags_a_whole_changed_block() {
        let black = gray(&[0; 81]);
        let mut block = black.clone();
        for y in 3..6 {
            for x in 3..6 {
                block[(y * 9 + x) * 4] = 255;
            }
        }
        let m = map(
            buffer(9, 9, &black),
            buffer(9, 9, &block),
            THREE,
            Alignment::TopLeft,
        );
        let diff = changed_pixels(&m, 0);
        assert_eq!(diff.stats.changed, 9);
        assert_eq!(diff.stats.bounds, Some(PixelBounds { x: 3..6, y: 3..6 }));
        assert!(
            m.magnitude
                .iter()
                .enumerate()
                .all(|(i, &v)| v == if diff.mask[i] { 255 } else { 0 })
        );
    }

    #[test]
    fn neighborhood_is_symmetric() {
        let first: Vec<u8> = (0..64u32)
            .flat_map(|i| [(i * 37 % 251) as u8, 9, 3, 255])
            .collect();
        let second: Vec<u8> = (0..64u32)
            .flat_map(|i| [(i * 53 % 241) as u8, 9, 4, 255])
            .collect();
        for size in [NeighborhoodSize::Three, NeighborhoodSize::Seven] {
            let metric = PixelMetric::Neighborhood(size);
            let forward = map(
                buffer(8, 8, &first),
                buffer(8, 8, &second),
                metric,
                Alignment::TopLeft,
            );
            let backward = map(
                buffer(8, 8, &second),
                buffer(8, 8, &first),
                metric,
                Alignment::TopLeft,
            );
            assert_eq!(forward, backward, "{size:?}");
            assert!(forward.difference > 0.0);
        }
    }

    #[test]
    fn ssim_of_identical_images_is_alike_everywhere() {
        let rgba: Vec<u8> = (0..100u32)
            .flat_map(|i| [(i * 7) as u8, (i * 3) as u8, 50, 255])
            .collect();
        let m = map(
            buffer(10, 10, &rgba),
            buffer(10, 10, &rgba),
            PixelMetric::Ssim,
            Alignment::TopLeft,
        );
        assert_eq!(m.magnitude, vec![0; 100]);
        assert_eq!(m.difference, 0.0);
    }

    #[test]
    fn ssim_of_a_flat_luma_offset_matches_the_closed_form() {
        let (low, high) = (gray(&[100; 12]), gray(&[120; 12]));
        let m = map(
            buffer(4, 3, &low),
            buffer(4, 3, &high),
            PixelMetric::Ssim,
            Alignment::TopLeft,
        );
        let c1 = (0.01f64 * 255.0).powi(2);
        let ssim = (2.0 * 100.0 * 120.0 + c1) / (100.0f64.powi(2) + 120.0f64.powi(2) + c1);
        assert!(
            (m.difference - (1.0 - ssim)).abs() < 1e-9,
            "{}",
            m.difference
        );
        assert_eq!(m.magnitude, vec![((1.0 - ssim) * 255.0).round() as u8; 12]);
        assert_eq!(m.magnitude[0], 4);
    }

    #[test]
    fn ssim_counts_transparent_color_as_black() {
        let first = [0, 0, 0, 255];
        let second = [200, 200, 200, 0];
        let m = map(
            buffer(1, 1, &first),
            buffer(1, 1, &second),
            PixelMetric::Ssim,
            Alignment::TopLeft,
        );
        assert_eq!(m.magnitude, [0]);
    }

    #[test]
    fn scale_to_fit_makes_a_resized_copy_alike() {
        let (small, large) = (gray(&[60; 6]), gray(&[60; 24]));
        let (small, large) = (buffer(3, 2, &small), buffer(6, 4, &large));
        let top_left = map(small, large, PixelMetric::Exact, Alignment::TopLeft);
        assert_eq!(changed_pixels(&top_left, 0).stats.changed, 18);

        for metric in PixelMetric::ALL {
            let fitted = map(small, large, metric, Alignment::ScaleToFit);
            assert_eq!(fitted.overlap, PixelBounds { x: 0..6, y: 0..4 });
            assert_eq!(fitted.magnitude, vec![0; 24], "{metric:?}");
        }
    }

    #[test]
    fn scaling_interpolates_between_pixel_centers() {
        let small = gray(&[0, 255]);
        let large = gray(&[0, 64, 191, 255]);
        let m = map(
            buffer(2, 1, &small),
            buffer(4, 1, &large),
            PixelMetric::Exact,
            Alignment::ScaleToFit,
        );
        assert_eq!(m.magnitude, [0; 4]);
        let nearest = gray(&[0, 0, 255, 255]);
        let m = map(
            buffer(2, 1, &small),
            buffer(4, 1, &nearest),
            PixelMetric::Exact,
            Alignment::ScaleToFit,
        );
        assert_eq!(m.magnitude, [0, 64, 64, 0]);
    }

    #[test]
    fn center_alignment_compares_the_middles() {
        let mut large = gray(&[0; 9]);
        large[4 * 4..4 * 4 + 4].copy_from_slice(&[90, 80, 70, 255]);
        let small = [90, 80, 70, 255];
        let (large, small) = (buffer(3, 3, &large), buffer(1, 1, &small));
        let centered = map(large, small, PixelMetric::Exact, Alignment::Center);
        assert_eq!(centered.overlap, PixelBounds { x: 1..2, y: 1..2 });
        let mut expected = vec![255; 9];
        expected[4] = 0;
        assert_eq!(centered.magnitude, expected);

        let top_left = map(large, small, PixelMetric::Exact, Alignment::TopLeft);
        assert_eq!(top_left.magnitude[0], 90);
        assert_eq!(top_left.magnitude[4], 255);
    }

    #[test]
    fn every_metric_and_alignment_cancels() {
        let rgba = gray(&[1; 16]);
        let other = gray(&[2; 4]);
        for metric in PixelMetric::ALL {
            for alignment in Alignment::ALL {
                assert_eq!(
                    metric_map(
                        buffer(4, 4, &rgba),
                        buffer(2, 2, &other),
                        metric,
                        alignment,
                        &AtomicBool::new(true)
                    ),
                    None
                );
            }
        }
    }

    #[test]
    fn empty_images_give_an_empty_or_fully_changed_canvas() {
        let rgba = gray(&[1; 4]);
        for metric in PixelMetric::ALL {
            for alignment in Alignment::ALL {
                let m = map(buffer(0, 0, &[]), buffer(2, 2, &rgba), metric, alignment);
                assert_eq!(m.magnitude, [255; 4], "{metric:?} {alignment:?}");
                assert_eq!(m.difference, 1.0);
                let m = map(buffer(0, 0, &[]), buffer(0, 0, &[]), metric, alignment);
                assert!(m.magnitude.is_empty());
                assert_eq!(m.difference, 0.0);
            }
        }
    }
}
