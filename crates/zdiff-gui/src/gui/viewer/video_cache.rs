//! Frame view seeking without waiting on ffmpeg: decoded frames kept by recency, the frames to
//! decode around the wanted one, and small previews of the whole video shown while scrubbing.

use std::{
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};

use super::{image::DecodedImage, video_frames::VideoTrack};

pub const FRAME_CACHE_BYTES: usize = 256 << 20;
pub const PREVIEW_BYTES: usize = 128 << 20;
pub const SCRUB_IDLE: Duration = Duration::from_millis(100);
const PREVIEW_PIXELS: f64 = 320.0 * 180.0;
const MAX_RUN: u64 = 16;

#[derive(Debug)]
pub struct FrameCache {
    budget: usize,
    bytes: usize,
    clock: u64,
    entries: Vec<CachedFrame>,
}

#[derive(Debug)]
struct CachedFrame {
    index: u64,
    frame: Arc<DecodedImage>,
    used: u64,
}

impl FrameCache {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            bytes: 0,
            clock: 0,
            entries: Vec::new(),
        }
    }

    pub fn get(&mut self, index: u64) -> Option<Arc<DecodedImage>> {
        self.clock += 1;
        let entry = self.entries.iter_mut().find(|e| e.index == index)?;
        entry.used = self.clock;
        Some(entry.frame.clone())
    }

    pub fn contains(&self, index: u64) -> bool {
        self.entries.iter().any(|e| e.index == index)
    }

    pub fn touch(&mut self, frames: Range<u64>) {
        self.clock += 1;
        for entry in &mut self.entries {
            if frames.contains(&entry.index) {
                entry.used = self.clock;
            }
        }
    }

    pub fn insert(&mut self, index: u64, frame: Arc<DecodedImage>) {
        if let Some(old) = self.entries.iter().position(|e| e.index == index) {
            self.bytes -= self.entries.swap_remove(old).frame.rgba.len();
        }
        self.clock += 1;
        self.bytes += frame.rgba.len();
        self.entries.push(CachedFrame {
            index,
            frame,
            used: self.clock,
        });
        while self.bytes > self.budget && self.entries.len() > 1 {
            let oldest = (0..self.entries.len())
                .min_by_key(|&i| self.entries[i].used)
                .expect("more than one entry");
            self.bytes -= self.entries.swap_remove(oldest).frame.rgba.len();
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    // `next_run` keeps up to run + 1 frames and adds up to run more; all must fit at once.
    pub fn run_length(&self, frame_bytes: usize) -> u64 {
        let fit = (self.budget / frame_bytes.max(1)) as u64;
        (fit.saturating_sub(1) / 2).clamp(1, MAX_RUN)
    }
}

pub fn kept_frames(wanted: u64, frame_count: u64, run: u64) -> Range<u64> {
    let half = run / 2;
    wanted.saturating_sub(half)..(wanted + half + 1).min(frame_count)
}

pub fn next_run(
    wanted: u64,
    frame_count: u64,
    run: u64,
    have: impl Fn(u64) -> bool,
) -> Option<Range<u64>> {
    assert!(wanted < frame_count && run > 0, "{wanted} of {frame_count}");
    let run_from = |start: u64| start..(start + run).min(frame_count);
    if !have(wanted) {
        return Some(run_from(wanted));
    }
    let kept = kept_frames(wanted, frame_count, run);
    if let Some(ahead) = (wanted + 1..kept.end).find(|&i| !have(i)) {
        return Some(run_from(ahead));
    }
    let behind = (kept.start..wanted).rev().find(|&i| !have(i))?;
    Some((behind + 1).saturating_sub(run)..behind + 1)
}

pub fn preview_size(width: u32, height: u32) -> (u32, u32) {
    let scale = (PREVIEW_PIXELS / (width as f64 * height as f64))
        .sqrt()
        .min(1.0);
    let side = |length: u32| ((length as f64 * scale).round() as u32).max(1);
    (side(width), side(height))
}

pub struct PreviewStrip {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    capacity: usize,
    samples: Vec<Vec<u8>>,
    complete: bool,
}

// The samples are hundreds of MB.
impl std::fmt::Debug for PreviewStrip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewStrip")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("fps", &self.fps)
            .field("samples", &self.samples.len())
            .field("complete", &self.complete)
            .finish()
    }
}

impl PreviewStrip {
    /// Sample i is at `i / fps`: every frame, or every nth when they don't all fit in `budget`.
    pub fn plan(track: &VideoTrack, budget: usize) -> Self {
        let (width, height) = preview_size(track.width, track.height);
        let capacity = (budget / (width as usize * height as usize * 4)).max(1);
        let stride = track.frame_count.div_ceil(capacity as u64).max(1);
        Self {
            width,
            height,
            fps: track.fps / stride as f64,
            capacity,
            samples: Vec::new(),
            complete: false,
        }
    }

    pub fn push(&mut self, rgba: Vec<u8>) {
        assert_eq!(rgba.len(), self.width as usize * self.height as usize * 4);
        if self.samples.len() < self.capacity {
            self.samples.push(rgba);
        }
    }

    pub fn finish(&mut self) {
        self.complete = true;
    }

    pub fn sample_at(&self, time_secs: f64) -> Option<usize> {
        let sample = (time_secs * self.fps).round().max(0.0) as usize;
        if sample < self.samples.len() {
            Some(sample)
        } else if self.complete {
            self.samples.len().checked_sub(1)
        } else {
            None
        }
    }

    pub fn upscaled(&self, sample: usize, width: u32, height: u32) -> DecodedImage {
        DecodedImage {
            width,
            height,
            format: "Preview",
            rgba: upscale_nearest(
                &self.samples[sample],
                (self.width, self.height),
                (width, height),
            ),
        }
    }
}

pub fn upscale_nearest(rgba: &[u8], from: (u32, u32), to: (u32, u32)) -> Vec<u8> {
    let [from_width, from_height, to_width, to_height] =
        [from.0, from.1, to.0, to.1].map(|side| side as usize);
    assert_eq!(rgba.len(), from_width * from_height * 4);
    let nearest = |to_index: usize, from_len: usize, to_len: usize| {
        (2 * to_index + 1) * from_len / (2 * to_len)
    };
    let columns: Vec<usize> = (0..to_width)
        .map(|x| nearest(x, from_width, to_width) * 4)
        .collect();
    let row_len = to_width * 4;
    let mut out = Vec::with_capacity(row_len * to_height);
    let mut last_row = None;
    for y in 0..to_height {
        let from_row = nearest(y, from_height, to_height);
        if last_row == Some(from_row) {
            out.extend_from_within(out.len() - row_len..);
            continue;
        }
        let row = &rgba[from_row * from_width * 4..][..from_width * 4];
        for &x in &columns {
            out.extend_from_slice(&row[x..x + 4]);
        }
        last_row = Some(from_row);
    }
    out
}

#[derive(Debug, Default)]
pub struct Scrub {
    last_change: Option<Instant>,
    held: bool,
}

impl Scrub {
    pub fn change(&mut self, now: Instant) {
        self.last_change = Some(now);
    }

    pub fn hold(&mut self, held: bool) {
        self.held = held;
    }

    pub fn active(&self, now: Instant) -> bool {
        self.held
            && self
                .last_change
                .is_some_and(|last| now.saturating_duration_since(last) < SCRUB_IDLE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(bytes: usize) -> Arc<DecodedImage> {
        Arc::new(DecodedImage {
            width: (bytes / 4) as u32,
            height: 1,
            format: "Frame",
            rgba: vec![0; bytes],
        })
    }

    fn cached(cache: &FrameCache) -> Vec<u64> {
        let mut indices: Vec<u64> = (0..1000).filter(|&i| cache.contains(i)).collect();
        indices.sort();
        indices
    }

    #[test]
    fn the_least_recently_used_frames_go_first_when_over_budget() {
        let mut cache = FrameCache::new(3 * 8);
        for index in 0..3 {
            cache.insert(index, frame(8));
        }
        assert!(cache.get(0).is_some());
        cache.insert(3, frame(8));
        assert_eq!(cached(&cache), [0, 2, 3]);
        cache.touch(2..3);
        cache.insert(4, frame(16));
        assert_eq!(cached(&cache), [2, 4]);
        assert_eq!(cache.bytes, 24);
    }

    #[test]
    fn a_frame_replaces_its_older_copy_and_one_over_budget_is_kept_alone() {
        let mut cache = FrameCache::new(16);
        cache.insert(5, frame(8));
        cache.insert(5, frame(8));
        assert_eq!((cached(&cache), cache.bytes), (vec![5], 8));
        cache.insert(6, frame(40));
        assert_eq!((cached(&cache), cache.bytes), (vec![6], 40));
        cache.clear();
        assert_eq!((cached(&cache), cache.bytes), (vec![], 0));
        assert!(cache.get(6).is_none());
    }

    #[test]
    fn a_run_fits_twice_in_the_budget_with_the_kept_frames() {
        let cache = FrameCache::new(100);
        assert_eq!(cache.run_length(10), 4);
        assert_eq!(cache.run_length(1), MAX_RUN);
        assert_eq!(cache.run_length(30), 1);
        assert_eq!(cache.run_length(200), 1);
    }

    #[test]
    fn the_wanted_frame_comes_first_then_the_frames_ahead_then_behind() {
        let have = |cached: &'static [u64]| move |i| cached.contains(&i);
        assert_eq!(next_run(10, 100, 8, have(&[])), Some(10..18));
        assert_eq!(next_run(98, 100, 8, have(&[])), Some(98..100));
        assert_eq!(next_run(10, 100, 8, have(&[10, 11, 13])), Some(12..20));
        assert_eq!(
            next_run(10, 100, 8, have(&[10, 11, 12, 13, 14, 8, 9])),
            Some(0..8)
        );
        assert_eq!(next_run(2, 100, 8, have(&[2, 3, 4, 5, 6])), Some(0..2));
        assert_eq!(
            next_run(10, 100, 8, have(&[6, 7, 8, 9, 10, 11, 12, 13, 14])),
            None
        );
        assert_eq!(next_run(99, 100, 8, have(&[95, 96, 97, 98, 99])), None);
        assert_eq!(next_run(10, 100, 1, have(&[10])), None);
        assert_eq!(kept_frames(10, 100, 8), 6..15);
        assert_eq!(kept_frames(1, 3, 8), 0..3);
    }

    #[test]
    fn filling_around_each_wanted_frame_ends_with_all_kept_frames_cached() {
        let frame_bytes = 4;
        let mut cache = FrameCache::new(21 * frame_bytes);
        let run = cache.run_length(frame_bytes);
        let frame_count = 200;
        let walk = (0..40).chain((0..40).rev()).chain([150, 3, 199, 198, 100]);
        for wanted in walk {
            let mut runs = 0;
            loop {
                cache.touch(kept_frames(wanted, frame_count, run));
                let Some(next) = next_run(wanted, frame_count, run, |i| cache.contains(i)) else {
                    break;
                };
                for index in next {
                    cache.insert(index, frame(frame_bytes));
                }
                runs += 1;
                assert!(runs <= 3, "{wanted} keeps decoding");
            }
            for index in kept_frames(wanted, frame_count, run) {
                assert!(cache.contains(index), "{wanted}: {index} evicted");
            }
        }
    }

    #[test]
    fn previews_are_about_320_by_180_in_the_videos_aspect_and_never_upscaled() {
        assert_eq!(preview_size(1920, 1080), (320, 180));
        assert_eq!(preview_size(3840, 2160), (320, 180));
        assert_eq!(preview_size(1080, 1920), (180, 320));
        assert_eq!(preview_size(320, 240), (277, 208));
        assert_eq!(preview_size(160, 90), (160, 90));
        assert_eq!(preview_size(10_000, 1), (10_000, 1));
        assert_eq!(preview_size(1_000_000, 1), (240_000, 1));
    }

    fn track(width: u32, height: u32, fps: f64, frame_count: u64) -> VideoTrack {
        VideoTrack {
            width,
            height,
            fps,
            frame_count,
        }
    }

    #[test]
    fn a_long_video_keeps_every_nth_frame_as_a_preview_within_the_budget() {
        let sample = 320 * 180 * 4;
        let short = PreviewStrip::plan(&track(1920, 1080, 30.0, 300), 1000 * sample);
        assert_eq!((short.fps, short.capacity), (30.0, 1000));
        let long = PreviewStrip::plan(&track(1920, 1080, 30.0, 1800), 500 * sample + 1);
        assert_eq!((long.fps, long.capacity), (7.5, 500));
        let tiny = PreviewStrip::plan(&track(1920, 1080, 30.0, 1800), 10);
        assert_eq!((tiny.capacity, tiny.fps), (1, 30.0 / 1800.0));
    }

    #[test]
    fn a_preview_exists_once_decoded_and_the_last_one_holds_past_the_end() {
        let mut strip = PreviewStrip::plan(&track(2, 1, 10.0, 100), 3 * 8);
        assert_eq!(strip.fps, 10.0 / 34.0);
        let mut strip_10 = PreviewStrip::plan(&track(2, 1, 10.0, 3), 1 << 20);
        assert_eq!(strip_10.sample_at(0.0), None);
        strip_10.push(vec![1; 8]);
        strip_10.push(vec![2; 8]);
        assert_eq!(strip_10.sample_at(0.04), Some(0));
        assert_eq!(strip_10.sample_at(0.06), Some(1));
        assert_eq!(strip_10.sample_at(0.2), None);
        assert_eq!(strip_10.sample_at(-1.0), Some(0));
        strip_10.finish();
        assert_eq!(strip_10.sample_at(0.2), Some(1));
        assert_eq!(strip_10.sample_at(60.0), Some(1));

        for value in 0..5 {
            strip.push(vec![value; 8]);
        }
        strip.finish();
        assert_eq!(strip.sample_at(1e9), Some(2));
        let preview = strip.upscaled(2, 4, 2);
        assert_eq!(
            (preview.width, preview.height, preview.format),
            (4, 2, "Preview")
        );
        assert_eq!(preview.rgba, vec![2; 32]);
    }

    #[test]
    fn upscaling_repeats_the_nearest_source_pixel() {
        let rgba: Vec<u8> = (0..6u8).flat_map(|p| [p, p, p, 255]).collect();
        let pixel =
            |image: &[u8], width: u32, x: u32, y: u32| image[((y * width + x) * 4) as usize];
        let double = upscale_nearest(&rgba, (3, 2), (6, 4));
        assert_eq!(double.len(), 6 * 4 * 4);
        let row = |y| (0..6).map(|x| pixel(&double, 6, x, y)).collect::<Vec<_>>();
        assert_eq!(row(0), [0, 0, 1, 1, 2, 2]);
        assert_eq!(row(1), row(0));
        assert_eq!(row(3), [3, 3, 4, 4, 5, 5]);
        let odd = upscale_nearest(&rgba, (3, 2), (4, 3));
        let row = |y| (0..4).map(|x| pixel(&odd, 4, x, y)).collect::<Vec<_>>();
        assert_eq!(
            [row(0), row(1), row(2)],
            [[0, 1, 1, 2], [3, 4, 4, 5], [3, 4, 4, 5]]
        );
        assert_eq!(upscale_nearest(&rgba, (3, 2), (3, 2)), rgba);
    }

    #[test]
    fn scrubbing_is_a_held_pointer_moving_the_time_recently() {
        let start = Instant::now();
        let mut scrub = Scrub::default();
        assert!(!scrub.active(start));
        scrub.change(start);
        assert!(!scrub.active(start));
        scrub.hold(true);
        assert!(scrub.active(start + SCRUB_IDLE / 2));
        assert!(!scrub.active(start + SCRUB_IDLE));
        scrub.change(start + SCRUB_IDLE);
        assert!(scrub.active(start + SCRUB_IDLE));
        scrub.hold(false);
        assert!(!scrub.active(start + SCRUB_IDLE));
    }
}
