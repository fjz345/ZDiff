use std::{
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use zcommon::{
    change_map::{ChangeMap, STRIP_BINS, THUMBNAIL_MAX_SIDE, byte_bins, downscale_max, flag_bins},
    kinds::{FileKind, KindTable, normalize_extension},
};
use zdiff::{
    ignore::IgnoreMask,
    lexer::{Lexer, RawToken, TokenKind},
    myers::{MyersDiffAlgorithm, Side, line_diff},
    pixel::{RgbaBuffer, pixel_diff},
    pixel_metric::{Alignment, PixelMetric, metric_map},
};

// Worst case (completely different, 40-byte lines) in release: ~1.1 s at this size, 19.5 s at 1 MiB.
pub const TEXT_LINES_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measure {
    Lines,
    Pixels,
    Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LikenessReason {
    DimensionsDiffer { left: (u32, u32), right: (u32, u32) },
    ImageDecode(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Likeness {
    pub ratio: f64,
    pub measure: Measure,
    pub reason: Option<LikenessReason>,
}

impl Likeness {
    fn bytes(left: &[u8], right: &[u8], reason: Option<LikenessReason>) -> Self {
        Likeness {
            ratio: byte_likeness(left, right),
            measure: Measure::Bytes,
            reason,
        }
    }
}

/// `None` when cancelled.
pub fn likeness(
    kinds: &KindTable,
    extension: &str,
    left: &[u8],
    right: &[u8],
    pixel_tolerance: u8,
    cancel: &Arc<AtomicBool>,
) -> Option<Likeness> {
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    match kinds.kind(extension) {
        FileKind::Text if left.len().max(right.len()) <= TEXT_LINES_MAX_BYTES => {
            // Lossy decoding would make different invalid bytes equal.
            let (Ok(left_text), Ok(right_text)) =
                (std::str::from_utf8(left), std::str::from_utf8(right))
            else {
                return Some(Likeness::bytes(left, right, None));
            };
            Some(Likeness {
                ratio: text_likeness(left_text, right_text, cancel)?,
                measure: Measure::Lines,
                reason: None,
            })
        }
        FileKind::Image => image_likeness(extension, left, right, pixel_tolerance, cancel),
        _ => Some(Likeness::bytes(left, right, None)),
    }
}

pub fn change_map(
    kinds: &KindTable,
    extension: &str,
    left: &[u8],
    right: &[u8],
    cancel: &Arc<AtomicBool>,
) -> Option<ChangeMap> {
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let bytes = || Some(ChangeMap::Strip(byte_bins(left, right, STRIP_BINS)));
    match kinds.kind(extension) {
        FileKind::Text if left.len().max(right.len()) <= TEXT_LINES_MAX_BYTES => {
            let (Ok(left_text), Ok(right_text)) =
                (std::str::from_utf8(left), std::str::from_utf8(right))
            else {
                return bytes();
            };
            let changed = changed_lines(left_text, right_text, cancel)?;
            Some(ChangeMap::Sides(
                changed.map(|flags| flag_bins(&flags, STRIP_BINS)),
            ))
        }
        FileKind::Image => {
            let (Ok(left_image), Ok(right_image)) = (
                decode_image(left, extension),
                decode_image(right, extension),
            ) else {
                return bytes();
            };
            let map = metric_map(
                buffer(&left_image),
                buffer(&right_image),
                PixelMetric::Exact,
                Alignment::TopLeft,
                cancel,
            )?;
            Some(downscale_max(
                map.width,
                map.height,
                &map.magnitude,
                THUMBNAIL_MAX_SIDE,
            ))
        }
        _ => bytes(),
    }
}

fn text_likeness(left: &str, right: &str, cancel: &Arc<AtomicBool>) -> Option<f64> {
    let [left_changed, right_changed] = changed_lines(left, right, cancel)?;
    let total = left_changed.len() + right_changed.len();
    if total == 0 {
        return Some(1.0);
    }
    let unchanged = |flags: &[bool]| flags.iter().filter(|&&changed| !changed).count();
    let matching = unchanged(&left_changed);
    debug_assert_eq!(matching, unchanged(&right_changed));
    Some(2.0 * matching as f64 / total as f64)
}

fn changed_lines(left: &str, right: &str, cancel: &Arc<AtomicBool>) -> Option<[Vec<bool>; 2]> {
    let left_tokens: Vec<RawToken> = Lexer::new(left).parse();
    let right_tokens: Vec<RawToken> = Lexer::new(right).parse();
    let mut changed = [&left_tokens, &right_tokens].map(|tokens| vec![false; count_lines(tokens)]);
    if changed.iter().all(Vec::is_empty) {
        return Some(changed);
    }
    let key = |side: Side, t: &RawToken| {
        let text = match side {
            Side::Source => left,
            Side::Target => right,
        };
        (t.kind, &text.as_bytes()[t.span.clone()])
    };
    let hunks = line_diff(
        MyersDiffAlgorithm::Linear,
        &left_tokens,
        &right_tokens,
        key,
        &IgnoreMask::default(),
        cancel.clone(),
    )?;
    let lines_before = [&left_tokens, &right_tokens].map(|tokens| {
        let mut before = Vec::with_capacity(tokens.len() + 1);
        before.push(0);
        let mut lines = 0;
        for token in tokens.iter() {
            lines += (token.kind == TokenKind::Newline) as usize;
            before.push(lines);
        }
        before
    });
    for hunk in &hunks {
        for (side, tokens, range) in [
            (0, &left_tokens, &hunk.source),
            (1, &right_tokens, &hunk.target),
        ] {
            let first = lines_before[side][range.start];
            changed[side][first..first + count_lines(&tokens[range.clone()])].fill(true);
        }
    }
    Some(changed)
}

// Hunks start and end on line starts, so this also counts the lines of a hunk.
fn count_lines(tokens: &[RawToken]) -> usize {
    let newlines = tokens
        .iter()
        .filter(|token| token.kind == TokenKind::Newline)
        .count();
    let unterminated = tokens
        .last()
        .is_some_and(|token| token.kind != TokenKind::Newline);
    newlines + unterminated as usize
}

fn image_likeness(
    extension: &str,
    left: &[u8],
    right: &[u8],
    tolerance: u8,
    cancel: &Arc<AtomicBool>,
) -> Option<Likeness> {
    let (left_image, right_image) = match (
        decode_image(left, extension),
        decode_image(right, extension),
    ) {
        (Ok(l), Ok(r)) => (l, r),
        (Err(e), _) | (_, Err(e)) => {
            return Some(Likeness::bytes(
                left,
                right,
                Some(LikenessReason::ImageDecode(e)),
            ));
        }
    };
    let dimensions = |image: &image::RgbaImage| (image.width(), image.height());
    if dimensions(&left_image) != dimensions(&right_image) {
        return Some(Likeness {
            ratio: 0.0,
            measure: Measure::Pixels,
            reason: Some(LikenessReason::DimensionsDiffer {
                left: dimensions(&left_image),
                right: dimensions(&right_image),
            }),
        });
    }
    let stats = pixel_diff(buffer(&left_image), buffer(&right_image), tolerance, cancel)?.stats;
    let ratio = if stats.total == 0 {
        1.0
    } else {
        1.0 - stats.changed as f64 / stats.total as f64
    };
    Some(Likeness {
        ratio,
        measure: Measure::Pixels,
        reason: None,
    })
}

fn buffer(image: &image::RgbaImage) -> RgbaBuffer<'_> {
    RgbaBuffer {
        width: image.width(),
        height: image.height(),
        rgba: image.as_raw(),
    }
}

// Same as zdiff-gui's image viewer decoder, which this crate can't depend on.
fn decode_image(bytes: &[u8], extension: &str) -> Result<image::RgbaImage, String> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes));
    if let Some(format) = image::ImageFormat::from_extension(normalize_extension(extension)) {
        reader.set_format(format);
    }
    let reader = reader.with_guessed_format().map_err(|e| e.to_string())?;
    if reader.format().is_none() {
        return Err("not a known image format".to_string());
    }
    Ok(reader.decode().map_err(|e| e.to_string())?.into_rgba8())
}

fn byte_likeness(left: &[u8], right: &[u8]) -> f64 {
    let larger = left.len().max(right.len());
    if larger == 0 {
        return 1.0;
    }
    let equal = left.iter().zip(right).filter(|(a, b)| a == b).count();
    equal as f64 / larger as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba, RgbaImage};
    use std::{io::Cursor, sync::atomic::Ordering};

    fn measure(extension: &str, left: &[u8], right: &[u8], tolerance: u8) -> Likeness {
        likeness(
            &KindTable::default(),
            extension,
            left,
            right,
            tolerance,
            &Arc::new(AtomicBool::new(false)),
        )
        .expect("not cancelled")
    }

    fn lines(ratio: f64) -> Likeness {
        Likeness {
            ratio,
            measure: Measure::Lines,
            reason: None,
        }
    }

    fn bytes(ratio: f64) -> Likeness {
        Likeness {
            ratio,
            measure: Measure::Bytes,
            reason: None,
        }
    }

    fn pixels(ratio: f64) -> Likeness {
        Likeness {
            ratio,
            measure: Measure::Pixels,
            reason: None,
        }
    }

    fn numbered_lines(count: usize) -> Vec<String> {
        (0..count).map(|i| format!("line {i}\n")).collect()
    }

    fn png(width: u32, height: u32, edits: &[(u32, u32, u8)]) -> Vec<u8> {
        let mut image = RgbaImage::from_pixel(width, height, Rgba([100, 100, 100, 255]));
        for &(x, y, delta) in edits {
            image.put_pixel(x, y, Rgba([100 + delta, 100, 100, 255]));
        }
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn text_likeness_matches_the_diff_viewers_line_likeness() {
        use zdiff::{
            change_count::text_likeness,
            myers::{DiffPhases, myers_diff_path},
        };
        let line_only = DiffPhases {
            scope: false,
            line: true,
            token: false,
            character: false,
        };
        let numbered = numbered_lines(30).concat();
        let edited = numbered
            .replace("line 3\n", "")
            .replace("line 17\n", "line 17 changed\nnew\n");
        let pairs = [
            ("", ""),
            ("a\n", ""),
            ("a\nb\nc\n", "a\nx\nc\nd"),
            (
                "fn a() {\n    1;\n}\n",
                "fn a() {\n    2;\n}\n\nfn b() {}\n",
            ),
            (numbered.as_str(), edited.as_str()),
        ];
        for (left, right) in pairs {
            let (ts, tt): (Vec<RawToken>, Vec<RawToken>) =
                (Lexer::new(left).parse(), Lexer::new(right).parse());
            let cmp = |a: &RawToken, b: &RawToken| {
                a.kind == b.kind && left[a.span.clone()] == right[b.span.clone()]
            };
            let mask = IgnoreMask::default();
            let path = myers_diff_path(
                MyersDiffAlgorithm::Linear,
                &ts,
                &tt,
                left,
                right,
                cmp,
                &mask,
                line_only,
                &[],
                Arc::default(),
            )
            .unwrap();
            let viewer = text_likeness(&path, &ts, &tt, &mask).lines;
            let folder = text_likeness_of(left, right);
            assert!(
                (viewer - folder).abs() < 1e-12,
                "{left:?} {right:?}: {viewer} vs {folder}"
            );
        }
    }

    fn text_likeness_of(left: &str, right: &str) -> f64 {
        text_likeness(left, right, &Arc::new(AtomicBool::new(false))).unwrap()
    }

    #[test]
    fn text_identical_is_full() {
        let text = b"fn main() {\n    run();\n}\n";
        assert_eq!(measure("rs", text, text, 0), lines(1.0));
    }

    #[test]
    fn text_one_line_changed_in_many() {
        let left = numbered_lines(100);
        let mut right = left.clone();
        right[42] = "changed\n".to_string();
        assert_eq!(
            measure(
                "txt",
                left.concat().as_bytes(),
                right.concat().as_bytes(),
                0
            ),
            lines(0.99)
        );
    }

    #[test]
    fn text_completely_different_is_zero() {
        assert_eq!(measure("txt", b"a\nb\nc\n", b"x\ny\nz\n", 0), lines(0.0));
    }

    #[test]
    fn text_both_empty_is_full() {
        assert_eq!(measure("txt", b"", b"", 0), lines(1.0));
    }

    #[test]
    fn text_one_side_empty_is_zero() {
        assert_eq!(measure("txt", b"", b"a\nb\n", 0), lines(0.0));
        assert_eq!(measure("txt", b"a\nb\n", b"", 0), lines(0.0));
    }

    #[test]
    fn text_counts_lines_on_both_sides() {
        assert_eq!(
            measure("txt", b"a\nb\n", b"a\nb\nc\nd\n", 0),
            lines(4.0 / 6.0)
        );
    }

    #[test]
    fn text_last_line_without_newline_counts() {
        assert_eq!(measure("txt", b"a\nb", b"a\nc", 0), lines(0.5));
    }

    #[test]
    fn text_blank_lines_and_crlf_count_as_lines() {
        assert_eq!(
            measure("txt", b"a\n\nb\n", b"a\n\nc\n", 0),
            lines(4.0 / 6.0)
        );
        assert_eq!(
            measure("txt", b"a\r\n\r\nb\r\n", b"a\r\n\r\nc\r\n", 0),
            lines(4.0 / 6.0)
        );
    }

    #[test]
    fn text_above_cap_uses_bytes() {
        let left = vec![b'a'; TEXT_LINES_MAX_BYTES + 1];
        let mut right = left.clone();
        right[0] = b'b';
        let expected = TEXT_LINES_MAX_BYTES as f64 / (TEXT_LINES_MAX_BYTES + 1) as f64;
        assert_eq!(measure("txt", &left, &right, 0), bytes(expected));
    }

    #[test]
    fn text_not_utf8_uses_bytes() {
        assert_eq!(measure("txt", b"a\n\xff\n", b"a\n\xfe\n", 0), bytes(0.75));
    }

    #[test]
    fn cancelled_is_none() {
        let cancel = Arc::new(AtomicBool::new(false));
        cancel.store(true, Ordering::Relaxed);
        let result = likeness(&KindTable::default(), "txt", b"a\n", b"b\n", 0, &cancel);
        assert_eq!(result, None);
    }

    #[test]
    fn image_identical_is_full() {
        let image = png(4, 4, &[]);
        assert_eq!(measure("png", &image, &image, 10), pixels(1.0));
    }

    #[test]
    fn image_changes_under_tolerance_are_full() {
        let left = png(4, 4, &[]);
        let right = png(4, 4, &[(0, 0, 5), (3, 2, 5)]);
        assert_eq!(measure("png", &left, &right, 10), pixels(1.0));
    }

    #[test]
    fn image_changes_over_tolerance_count() {
        let left = png(4, 4, &[]);
        let right = png(4, 4, &[(0, 0, 50), (3, 2, 50)]);
        assert_eq!(measure("png", &left, &right, 10), pixels(14.0 / 16.0));
    }

    #[test]
    fn image_different_dimensions_is_zero_with_reason() {
        let left = png(4, 4, &[]);
        let right = png(4, 2, &[]);
        assert_eq!(
            measure("png", &left, &right, 10),
            Likeness {
                ratio: 0.0,
                measure: Measure::Pixels,
                reason: Some(LikenessReason::DimensionsDiffer {
                    left: (4, 4),
                    right: (4, 2)
                }),
            }
        );
    }

    #[test]
    fn image_not_decodable_uses_bytes_with_reason() {
        let result = measure("png", b"not a png", b"not a png", 0);
        assert_eq!((result.ratio, result.measure), (1.0, Measure::Bytes));
        assert!(matches!(
            result.reason,
            Some(LikenessReason::ImageDecode(_))
        ));
    }

    #[test]
    fn bytes_identical_is_full() {
        assert_eq!(measure("bin", &[1, 2, 3, 4], &[1, 2, 3, 4], 0), bytes(1.0));
    }

    #[test]
    fn bytes_one_changed() {
        let left = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut right = left;
        right[5] = 0;
        assert_eq!(measure("bin", &left, &right, 0), bytes(7.0 / 8.0));
    }

    #[test]
    fn bytes_different_lengths_divide_by_the_larger() {
        assert_eq!(measure("bin", &[1, 2, 3, 4], &[1, 2], 0), bytes(0.5));
        assert_eq!(measure("bin", &[1, 2], &[1, 2, 3, 4], 0), bytes(0.5));
    }

    #[test]
    fn bytes_both_empty_is_full() {
        assert_eq!(measure("bin", &[], &[], 0), bytes(1.0));
    }

    #[test]
    fn other_kinds_use_bytes() {
        assert_eq!(measure("zip", &[1, 2], &[1, 3], 0), bytes(0.5));
    }

    fn map_of(extension: &str, left: &[u8], right: &[u8]) -> ChangeMap {
        change_map(
            &KindTable::default(),
            extension,
            left,
            right,
            &Arc::new(AtomicBool::new(false)),
        )
        .expect("not cancelled")
    }

    fn line_bins(lines: usize, changed: std::ops::Range<usize>) -> Vec<u8> {
        (0..lines)
            .map(|line| if changed.contains(&line) { 255 } else { 0 })
            .collect()
    }

    #[test]
    fn text_map_flags_the_changed_lines_on_both_sides() {
        let left = numbered_lines(100);
        let mut right = left.clone();
        for line in &mut right[40..50] {
            *line = format!("changed {line}");
        }
        let map = map_of("txt", left.concat().as_bytes(), right.concat().as_bytes());
        assert_eq!(
            map,
            ChangeMap::Sides([line_bins(100, 40..50), line_bins(100, 40..50)])
        );
    }

    #[test]
    fn text_map_flags_inserted_lines_on_their_side_only() {
        let left = numbered_lines(10);
        let mut right = left.clone();
        right.insert(3, "new\n".to_string());
        right.push("tail without newline".to_string());
        let map = map_of("txt", left.concat().as_bytes(), right.concat().as_bytes());
        let mut right_bins = line_bins(12, 3..4);
        right_bins[11] = 255;
        assert_eq!(map, ChangeMap::Sides([line_bins(10, 0..0), right_bins]));
    }

    #[test]
    fn text_map_agrees_with_the_ratio() {
        let cases = [
            ("a\nb\nc\n", "a\nx\nc\nd\n"),
            ("one\ntwo", "one\r\ntwo\r\n"),
            ("", "only right\n"),
            ("same\n", "same\n"),
        ];
        for (left, right) in cases {
            let ChangeMap::Sides([left_bins, right_bins]) =
                map_of("txt", left.as_bytes(), right.as_bytes())
            else {
                panic!("text measures lines");
            };
            let unchanged = |bins: &[u8]| bins.iter().filter(|&&bin| bin == 0).count();
            let total = left_bins.len() + right_bins.len();
            let ratio = 2.0 * unchanged(&left_bins) as f64 / total as f64;
            assert_eq!(unchanged(&left_bins), unchanged(&right_bins), "{left:?}");
            assert_eq!(
                measure("txt", left.as_bytes(), right.as_bytes(), 0),
                lines(ratio)
            );
        }
    }

    #[test]
    fn pixel_map_marks_only_the_changed_pixel() {
        let ChangeMap::Pixels {
            width,
            height,
            magnitude,
        } = map_of("png", &png(4, 3, &[]), &png(4, 3, &[(2, 1, 50)]))
        else {
            panic!("images measure pixels");
        };
        assert_eq!((width, height), (4, 3));
        let mut expected = vec![0; 12];
        expected[4 + 2] = 50;
        assert_eq!(magnitude, expected);
    }

    #[test]
    fn pixel_map_of_sized_pairs_is_the_union_canvas() {
        let ChangeMap::Pixels {
            width,
            height,
            magnitude,
        } = map_of("png", &png(2, 2, &[]), &png(3, 2, &[]))
        else {
            panic!("images measure pixels");
        };
        assert_eq!((width, height), (3, 2));
        assert_eq!(magnitude, [0, 0, 255, 0, 0, 255]);
    }

    #[test]
    fn undecodable_image_and_other_kinds_map_bytes() {
        assert_eq!(
            map_of("png", b"not a png", b"not a pnG"),
            ChangeMap::Strip(vec![0, 0, 0, 0, 0, 0, 0, 0, 255])
        );
        assert_eq!(
            map_of("zip", &[1, 2], &[1, 3]),
            ChangeMap::Strip(vec![0, 255])
        );
    }

    #[test]
    fn cancelled_map_is_none() {
        let cancel = Arc::new(AtomicBool::new(true));
        assert_eq!(
            change_map(&KindTable::default(), "txt", b"a\n", b"b\n", &cancel),
            None
        );
    }
}
