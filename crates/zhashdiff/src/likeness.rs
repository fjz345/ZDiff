use std::{
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use zcommon::kinds::{FileKind, KindTable, normalize_extension};
use zdiff::{
    ignore::IgnoreMask,
    lexer::{LexerGreedy, RawToken, TokenKind},
    myers::{MyersDiffAlgorithm, line_diff},
    pixel::{RgbaBuffer, pixel_diff},
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

fn text_likeness(left: &str, right: &str, cancel: &Arc<AtomicBool>) -> Option<f64> {
    let left_tokens: Vec<RawToken> = LexerGreedy::new(left).parse();
    let right_tokens: Vec<RawToken> = LexerGreedy::new(right).parse();
    let (left_lines, right_lines) = (count_lines(&left_tokens), count_lines(&right_tokens));
    let total = left_lines + right_lines;
    if total == 0 {
        return Some(1.0);
    }
    let cmp = |a: &RawToken, b: &RawToken| {
        a.kind == b.kind && left.as_bytes()[a.span.clone()] == right.as_bytes()[b.span.clone()]
    };
    let hunks = line_diff(
        MyersDiffAlgorithm::Linear,
        &left_tokens,
        &right_tokens,
        cmp,
        &IgnoreMask::default(),
        cancel.clone(),
    )?;
    let left_changed: usize = hunks
        .iter()
        .map(|hunk| count_lines(&left_tokens[hunk.source.clone()]))
        .sum();
    let right_changed: usize = hunks
        .iter()
        .map(|hunk| count_lines(&right_tokens[hunk.target.clone()]))
        .sum();
    let matching = left_lines - left_changed;
    debug_assert_eq!(matching, right_lines - right_changed);
    Some(2.0 * matching as f64 / total as f64)
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
}
