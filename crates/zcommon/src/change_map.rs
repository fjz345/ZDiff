use std::ops::Range;

pub const STRIP_BINS: usize = 128;
pub const THUMBNAIL_MAX_SIDE: u32 = 256;

/// Where a pair differs, for a hover preview: 0 is alike, 255 totally different.
#[derive(Debug, Clone, PartialEq)]
pub enum ChangeMap {
    Sides([Vec<u8>; 2]),
    Strip(Vec<u8>),
    Pixels {
        width: u32,
        height: u32,
        magnitude: Vec<u8>,
    },
}

pub fn bins(len: usize, count: usize, mut share: impl FnMut(Range<usize>) -> f64) -> Vec<u8> {
    let count = count.min(len);
    (0..count)
        .map(|bin| {
            let share = share(bin * len / count..(bin + 1) * len / count).clamp(0.0, 1.0);
            // One changed unit in a large bin still shows.
            match (share * 255.0).round() as u8 {
                0 if share > 0.0 => 1,
                value => value,
            }
        })
        .collect()
}

pub fn flag_bins(changed: &[bool], count: usize) -> Vec<u8> {
    bins(changed.len(), count, |range| {
        let units = range.len();
        changed[range].iter().filter(|&&flag| flag).count() as f64 / units as f64
    })
}

pub fn byte_bins(left: &[u8], right: &[u8], count: usize) -> Vec<u8> {
    let common = left.len().min(right.len());
    bins(left.len().max(right.len()), count, |range| {
        let overlap = range.start.min(common)..range.end.min(common);
        let unequal = left[overlap.clone()]
            .iter()
            .zip(&right[overlap.clone()])
            .filter(|(a, b)| a != b)
            .count();
        (unequal + range.len() - overlap.len()) as f64 / range.len() as f64
    })
}

/// Each output pixel is the largest magnitude of its block, so a one-pixel change stays visible.
pub fn downscale_max(width: u32, height: u32, magnitude: &[u8], max_side: u32) -> ChangeMap {
    assert!(max_side > 0);
    assert_eq!(magnitude.len(), width as usize * height as usize);
    let scale = width.max(height).div_ceil(max_side).max(1) as usize;
    let (out_width, out_height) = (
        (width as usize).div_ceil(scale),
        (height as usize).div_ceil(scale),
    );
    let mut out = vec![0; out_width * out_height];
    for (y, row) in magnitude.chunks_exact(width.max(1) as usize).enumerate() {
        let out_row = &mut out[y / scale * out_width..][..out_width];
        for (x, &value) in row.iter().enumerate() {
            let pixel = &mut out_row[x / scale];
            *pixel = (*pixel).max(value);
        }
    }
    ChangeMap::Pixels {
        width: out_width as u32,
        height: out_height as u32,
        magnitude: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_bins_mark_only_the_changed_stretch() {
        let mut changed = vec![false; 100];
        changed[40..50].fill(true);
        assert_eq!(flag_bins(&changed, 10), [0, 0, 0, 0, 255, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn fewer_units_than_bins_get_one_bin_each() {
        assert_eq!(flag_bins(&[false, true, false], 10), [0, 255, 0]);
        assert_eq!(flag_bins(&[], 10), Vec::<u8>::new());
    }

    #[test]
    fn one_changed_unit_in_a_large_bin_still_shows() {
        let mut changed = vec![false; 10_000];
        changed[7] = true;
        let bins = flag_bins(&changed, 2);
        assert_eq!(bins, [1, 0]);
    }

    #[test]
    fn bins_are_the_rounded_changed_share() {
        let changed = [true, false, false, false, true, true, true, false];
        assert_eq!(flag_bins(&changed, 2), [64, 191]);
    }

    #[test]
    fn byte_bins_count_the_longer_sides_tail_as_changed() {
        assert_eq!(byte_bins(&[1, 2, 3, 4], &[1, 2], 2), [0, 255]);
        assert_eq!(byte_bins(&[1, 2], &[1, 2, 3, 4], 2), [0, 255]);
        assert_eq!(byte_bins(&[1, 2, 3, 4], &[1, 9, 3, 4], 4), [0, 255, 0, 0]);
        assert_eq!(byte_bins(&[], &[], 4), Vec::<u8>::new());
    }

    #[test]
    fn downscale_keeps_the_largest_magnitude_of_each_block() {
        let mut magnitude = vec![0; 8 * 4];
        magnitude[8 + 5] = 200;
        magnitude[3 * 8] = 7;
        assert_eq!(
            downscale_max(8, 4, &magnitude, 4),
            ChangeMap::Pixels {
                width: 4,
                height: 2,
                magnitude: vec![0, 0, 200, 0, 7, 0, 0, 0],
            }
        );
    }

    #[test]
    fn downscale_leaves_a_small_map_as_is() {
        let magnitude = vec![1, 2, 3, 4, 5, 6];
        assert_eq!(
            downscale_max(3, 2, &magnitude, 256),
            ChangeMap::Pixels {
                width: 3,
                height: 2,
                magnitude,
            }
        );
        assert_eq!(
            downscale_max(0, 0, &[], 256),
            ChangeMap::Pixels {
                width: 0,
                height: 0,
                magnitude: vec![],
            }
        );
    }
}
