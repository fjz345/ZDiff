//! Hex value inspector: the bytes at a cursor offset decoded as fixed-width integers, floats and
//! a short string. Pure; the panel in the diff pane only draws the result.

use std::{cell::Cell, fmt};

use eframe::egui;
use zcommon_ui::style_window::role_color;

use crate::ui_egui::file_role::FileRole;

pub const STRING_BYTES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Endian {
    #[default]
    Little,
    Big,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorKind {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
    Text,
}

pub const INSPECTOR_KINDS: [InspectorKind; 11] = [
    InspectorKind::U8,
    InspectorKind::I8,
    InspectorKind::U16,
    InspectorKind::I16,
    InspectorKind::U32,
    InspectorKind::I32,
    InspectorKind::U64,
    InspectorKind::I64,
    InspectorKind::F32,
    InspectorKind::F64,
    InspectorKind::Text,
];

impl InspectorKind {
    pub fn label(self) -> &'static str {
        match self {
            InspectorKind::U8 => "u8",
            InspectorKind::I8 => "i8",
            InspectorKind::U16 => "u16",
            InspectorKind::I16 => "i16",
            InspectorKind::U32 => "u32",
            InspectorKind::I32 => "i32",
            InspectorKind::U64 => "u64",
            InspectorKind::I64 => "i64",
            InspectorKind::F32 => "f32",
            InspectorKind::F64 => "f64",
            InspectorKind::Text => "str",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum InspectorValue {
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    U64(u64),
    I64(i64),
    F32(f32),
    F64(f64),
    Text(String),
}

impl InspectorValue {
    pub fn byte_len(&self) -> usize {
        match self {
            InspectorValue::U8(_) | InspectorValue::I8(_) => 1,
            InspectorValue::U16(_) | InspectorValue::I16(_) => 2,
            InspectorValue::U32(_) | InspectorValue::I32(_) | InspectorValue::F32(_) => 4,
            InspectorValue::U64(_) | InspectorValue::I64(_) | InspectorValue::F64(_) => 8,
            InspectorValue::Text(text) => text.len(),
        }
    }

    pub fn hex_text(&self) -> Option<String> {
        let bits = match *self {
            InspectorValue::U8(v) => u64::from(v),
            InspectorValue::I8(v) => u64::from(v as u8),
            InspectorValue::U16(v) => u64::from(v),
            InspectorValue::I16(v) => u64::from(v as u16),
            InspectorValue::U32(v) => u64::from(v),
            InspectorValue::I32(v) => u64::from(v as u32),
            InspectorValue::U64(v) => v,
            InspectorValue::I64(v) => v as u64,
            InspectorValue::F32(v) => u64::from(v.to_bits()),
            InspectorValue::F64(v) => v.to_bits(),
            InspectorValue::Text(_) => return None,
        };
        Some(format!("0x{bits:0width$X}", width = self.byte_len() * 2))
    }
}

impl fmt::Display for InspectorValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InspectorValue::U8(v) => write!(f, "{v}"),
            InspectorValue::I8(v) => write!(f, "{v}"),
            InspectorValue::U16(v) => write!(f, "{v}"),
            InspectorValue::I16(v) => write!(f, "{v}"),
            InspectorValue::U32(v) => write!(f, "{v}"),
            InspectorValue::I32(v) => write!(f, "{v}"),
            InspectorValue::U64(v) => write!(f, "{v}"),
            InspectorValue::I64(v) => write!(f, "{v}"),
            InspectorValue::F32(v) => write_float(f, *v, f64::from(v.abs())),
            InspectorValue::F64(v) => write_float(f, *v, v.abs()),
            InspectorValue::Text(v) => write!(f, "\"{v}\""),
        }
    }
}

fn write_float<T: fmt::Display + fmt::LowerExp>(
    f: &mut fmt::Formatter<'_>,
    value: T,
    magnitude: f64,
) -> fmt::Result {
    let readable = magnitude == 0.0 || (1e-4..1e15).contains(&magnitude) || !magnitude.is_finite();
    if readable {
        write!(f, "{value}")
    } else {
        write!(f, "{value:e}")
    }
}

/// `None` past the end of `bytes`. Text is printable ASCII up to the first NUL or `STRING_BYTES`, others as `.`.
pub fn decode(
    bytes: &[u8],
    offset: usize,
    kind: InspectorKind,
    endian: Endian,
) -> Option<InspectorValue> {
    let rest = bytes.get(offset..)?;
    macro_rules! number {
        ($variant:ident, $ty:ty) => {{
            let raw = rest.first_chunk::<{ size_of::<$ty>() }>()?;
            InspectorValue::$variant(match endian {
                Endian::Little => <$ty>::from_le_bytes(*raw),
                Endian::Big => <$ty>::from_be_bytes(*raw),
            })
        }};
    }
    Some(match kind {
        InspectorKind::U8 => number!(U8, u8),
        InspectorKind::I8 => number!(I8, i8),
        InspectorKind::U16 => number!(U16, u16),
        InspectorKind::I16 => number!(I16, i16),
        InspectorKind::U32 => number!(U32, u32),
        InspectorKind::I32 => number!(I32, i32),
        InspectorKind::U64 => number!(U64, u64),
        InspectorKind::I64 => number!(I64, i64),
        InspectorKind::F32 => number!(F32, f32),
        InspectorKind::F64 => number!(F64, f64),
        InspectorKind::Text => {
            if rest.is_empty() {
                return None;
            }
            let text = rest
                .iter()
                .take(STRING_BYTES)
                .take_while(|&&b| b != 0)
                .map(|&b| {
                    if (0x20..=0x7E).contains(&b) {
                        b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            InspectorValue::Text(text)
        }
    })
}

pub fn decode_all(bytes: &[u8], offset: usize, endian: Endian) -> [Option<InspectorValue>; 11] {
    INSPECTOR_KINDS.map(|kind| decode(bytes, offset, kind, endian))
}

/// The panel for the bytes at `cursor` on both sides, with the byte order toggle. A value that
/// differs between the sides is marked on both.
pub fn inspector_ui(
    ui: &mut egui::Ui,
    cursor: Option<usize>,
    sides: [&[u8]; 2],
    endian: &Cell<Endian>,
    hover: &Cell<Option<InspectorKind>>,
) {
    let different = role_color(ui.ctx(), FileRole::HexDifferent);
    egui::CollapsingHeader::new("Value inspector")
        .default_open(true)
        .show(ui, |ui| {
            let Some(offset) = cursor else {
                ui.label("Click a byte or go to an offset");
                return;
            };
            ui.horizontal(|ui| {
                ui.label(format!("Offset 0x{offset:X} ({offset})"));
                for (label, value) in [
                    ("Little endian", Endian::Little),
                    ("Big endian", Endian::Big),
                ] {
                    if ui.selectable_label(endian.get() == value, label).clicked() {
                        endian.set(value);
                    }
                }
            });
            let values = sides.map(|bytes| decode_all(bytes, offset, endian.get()));
            let rows = INSPECTOR_KINDS.len().div_ceil(2);
            egui::Grid::new("hex_inspector")
                .num_columns(6)
                .show(ui, |ui| {
                    for _ in 0..2 {
                        for title in ["Type", "Left", "Right"] {
                            ui.strong(title);
                        }
                    }
                    ui.end_row();
                    for row in 0..rows {
                        for index in [row, row + rows] {
                            let Some(kind) = INSPECTOR_KINDS.get(index) else {
                                for _ in 0..3 {
                                    ui.label("");
                                }
                                continue;
                            };
                            let mut hovered = ui.label(kind.label()).hovered();
                            let marked = values_differ(&values[0][index], &values[1][index]);
                            for side in &values {
                                let text = match &side[index] {
                                    Some(value) => egui::RichText::new(value.to_string()),
                                    None => egui::RichText::new("n/a").weak(),
                                };
                                let mut response = ui.label(if marked {
                                    text.background_color(different)
                                } else {
                                    text
                                });
                                if let Some(hex) =
                                    side[index].as_ref().and_then(InspectorValue::hex_text)
                                {
                                    response = response.on_hover_text(hex);
                                }
                                hovered |= response.hovered();
                            }
                            if hovered {
                                hover.set(Some(*kind));
                            }
                        }
                        ui.end_row();
                    }
                });
        });
}

/// Whether the two sides show different text. Compared as displayed, so identical NaNs match and
/// an unavailable value differs from an available one.
pub fn values_differ(left: &Option<InspectorValue>, right: &Option<InspectorValue>) -> bool {
    let shown = |value: &Option<InspectorValue>| value.as_ref().map(ToString::to_string);
    shown(left) != shown(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BYTES: [u8; 8] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x88];

    fn at(
        bytes: &[u8],
        offset: usize,
        kind: InspectorKind,
        endian: Endian,
    ) -> Option<InspectorValue> {
        decode(bytes, offset, kind, endian)
    }

    #[test]
    fn integers_decode_little_endian() {
        let le = Endian::Little;
        assert_eq!(
            at(&BYTES, 0, InspectorKind::U8, le),
            Some(InspectorValue::U8(1))
        );
        assert_eq!(
            at(&BYTES, 0, InspectorKind::U16, le),
            Some(InspectorValue::U16(0x0201))
        );
        assert_eq!(
            at(&BYTES, 0, InspectorKind::U32, le),
            Some(InspectorValue::U32(0x04030201))
        );
        assert_eq!(
            at(&BYTES, 0, InspectorKind::U64, le),
            Some(InspectorValue::U64(0x8807060504030201))
        );
    }

    #[test]
    fn integers_decode_big_endian() {
        let be = Endian::Big;
        assert_eq!(
            at(&BYTES, 0, InspectorKind::U8, be),
            Some(InspectorValue::U8(1))
        );
        assert_eq!(
            at(&BYTES, 0, InspectorKind::U16, be),
            Some(InspectorValue::U16(0x0102))
        );
        assert_eq!(
            at(&BYTES, 0, InspectorKind::U32, be),
            Some(InspectorValue::U32(0x01020304))
        );
        assert_eq!(
            at(&BYTES, 0, InspectorKind::U64, be),
            Some(InspectorValue::U64(0x0102030405060788))
        );
    }

    #[test]
    fn signed_integers_reinterpret_the_same_bytes() {
        let bytes = [0xFF, 0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        assert_eq!(
            at(&bytes, 0, InspectorKind::I8, Endian::Little),
            Some(InspectorValue::I8(-1))
        );
        assert_eq!(
            at(&bytes, 0, InspectorKind::I16, Endian::Little),
            Some(InspectorValue::I16(-257))
        );
        assert_eq!(
            at(&bytes, 0, InspectorKind::I16, Endian::Big),
            Some(InspectorValue::I16(-2))
        );
        assert_eq!(
            at(&bytes, 0, InspectorKind::I32, Endian::Big),
            Some(InspectorValue::I32(-65537))
        );
        assert_eq!(
            at(&bytes, 0, InspectorKind::I64, Endian::Big),
            Some(InspectorValue::I64(-65537 << 32 | 0xFFFF_FFFF))
        );
    }

    #[test]
    fn floats_decode_in_both_endiannesses() {
        let f32_le = 1.5f32.to_le_bytes();
        let f32_be = 1.5f32.to_be_bytes();
        assert_eq!(
            at(&f32_le, 0, InspectorKind::F32, Endian::Little),
            Some(InspectorValue::F32(1.5))
        );
        assert_eq!(
            at(&f32_be, 0, InspectorKind::F32, Endian::Big),
            Some(InspectorValue::F32(1.5))
        );
        let f64_le = (-2.25f64).to_le_bytes();
        let f64_be = (-2.25f64).to_be_bytes();
        assert_eq!(
            at(&f64_le, 0, InspectorKind::F64, Endian::Little),
            Some(InspectorValue::F64(-2.25))
        );
        assert_eq!(
            at(&f64_be, 0, InspectorKind::F64, Endian::Big),
            Some(InspectorValue::F64(-2.25))
        );
    }

    #[test]
    fn decodes_at_a_nonzero_offset() {
        assert_eq!(
            at(&BYTES, 2, InspectorKind::U16, Endian::Big),
            Some(InspectorValue::U16(0x0304))
        );
        assert_eq!(
            at(&BYTES, 4, InspectorKind::U32, Endian::Little),
            Some(InspectorValue::U32(0x88070605))
        );
    }

    #[test]
    fn a_value_ending_exactly_at_the_end_is_available() {
        assert_eq!(
            at(&BYTES, 4, InspectorKind::U32, Endian::Little),
            Some(InspectorValue::U32(0x88070605))
        );
        assert_eq!(
            at(&BYTES, 7, InspectorKind::U8, Endian::Little),
            Some(InspectorValue::U8(0x88))
        );
    }

    #[test]
    fn reads_past_the_end_are_unavailable() {
        for endian in [Endian::Little, Endian::Big] {
            assert_eq!(at(&BYTES, 7, InspectorKind::U16, endian), None);
            assert_eq!(at(&BYTES, 5, InspectorKind::U32, endian), None);
            assert_eq!(at(&BYTES, 1, InspectorKind::U64, endian), None);
            assert_eq!(at(&BYTES, 5, InspectorKind::F32, endian), None);
            assert_eq!(at(&BYTES, 1, InspectorKind::F64, endian), None);
            assert_eq!(at(&BYTES, 8, InspectorKind::U8, endian), None);
            assert_eq!(at(&BYTES, 8, InspectorKind::Text, endian), None);
            assert_eq!(at(&[], 0, InspectorKind::U8, endian), None);
        }
    }

    #[test]
    fn an_offset_that_overflows_is_unavailable() {
        assert_eq!(
            at(&BYTES, usize::MAX, InspectorKind::U64, Endian::Little),
            None
        );
    }

    #[test]
    fn text_stops_at_nul_and_masks_unprintable_bytes() {
        let bytes = b"Hi\x01!\0rest";
        assert_eq!(
            at(bytes, 0, InspectorKind::Text, Endian::Little),
            Some(InspectorValue::Text("Hi.!".to_string()))
        );
        assert_eq!(
            at(bytes, 5, InspectorKind::Text, Endian::Little),
            Some(InspectorValue::Text("rest".to_string()))
        );
    }

    #[test]
    fn text_is_capped_and_does_not_depend_on_endianness() {
        let bytes = [b'a'; 40];
        let text = Some(InspectorValue::Text("a".repeat(STRING_BYTES)));
        assert_eq!(at(&bytes, 0, InspectorKind::Text, Endian::Little), text);
        assert_eq!(at(&bytes, 0, InspectorKind::Text, Endian::Big), text);
    }

    #[test]
    fn text_starting_on_a_nul_is_empty_not_unavailable() {
        assert_eq!(
            at(&[0, b'a'], 0, InspectorKind::Text, Endian::Little),
            Some(InspectorValue::Text(String::new()))
        );
    }

    #[test]
    fn decode_all_follows_the_kind_order() {
        let all = decode_all(&BYTES, 0, Endian::Little);
        for (kind, value) in INSPECTOR_KINDS.into_iter().zip(&all) {
            assert_eq!(*value, decode(&BYTES, 0, kind, Endian::Little));
        }
    }

    #[test]
    fn equal_unequal_and_one_sided_values_differ_correctly() {
        let a = Some(InspectorValue::U16(1));
        let b = Some(InspectorValue::U16(2));
        assert!(!values_differ(&a, &a.clone()));
        assert!(values_differ(&a, &b));
        assert!(values_differ(&a, &None));
        assert!(values_differ(&None, &b));
        assert!(!values_differ(&None, &None));
    }

    #[test]
    fn floats_in_a_readable_range_print_plainly() {
        assert_eq!(InspectorValue::F32(1.5).to_string(), "1.5");
        assert_eq!(InspectorValue::F64(-2.25).to_string(), "-2.25");
        assert_eq!(InspectorValue::F64(0.0).to_string(), "0");
        assert_eq!(InspectorValue::F64(0.001).to_string(), "0.001");
        assert_eq!(InspectorValue::F64(123456789.0).to_string(), "123456789");
    }

    #[test]
    fn huge_and_tiny_floats_print_in_scientific_notation() {
        assert_eq!(InspectorValue::F64(1e300).to_string(), "1e300");
        assert_eq!(InspectorValue::F64(-1.25e-300).to_string(), "-1.25e-300");
        assert_eq!(InspectorValue::F32(3.4e38).to_string(), "3.4e38");
        assert_eq!(
            InspectorValue::F64(f64::MIN_POSITIVE).to_string(),
            "2.2250738585072014e-308"
        );
        assert_eq!(InspectorValue::F64(0.0000123).to_string(), "1.23e-5");
    }

    #[test]
    fn non_finite_floats_print_by_name() {
        assert_eq!(InspectorValue::F64(f64::NAN).to_string(), "NaN");
        assert_eq!(InspectorValue::F32(f32::NEG_INFINITY).to_string(), "-inf");
    }

    #[test]
    fn neighbouring_floats_still_differ() {
        let a = 1e-300f64;
        let b = f64::from_bits(a.to_bits() + 1);
        assert!(values_differ(
            &Some(InspectorValue::F64(a)),
            &Some(InspectorValue::F64(b))
        ));
    }

    #[test]
    fn numbers_show_their_raw_bits_in_hex_padded_to_the_width() {
        assert_eq!(InspectorValue::U8(0xAB).hex_text().as_deref(), Some("0xAB"));
        assert_eq!(InspectorValue::U16(1).hex_text().as_deref(), Some("0x0001"));
        assert_eq!(InspectorValue::I8(-1).hex_text().as_deref(), Some("0xFF"));
        assert_eq!(
            InspectorValue::I32(-2).hex_text().as_deref(),
            Some("0xFFFFFFFE")
        );
        assert_eq!(
            InspectorValue::U64(0x1234).hex_text().as_deref(),
            Some("0x0000000000001234")
        );
        assert_eq!(
            InspectorValue::F32(1.5).hex_text().as_deref(),
            Some("0x3FC00000")
        );
        assert_eq!(
            InspectorValue::F64(-2.25).hex_text().as_deref(),
            Some("0xC002000000000000")
        );
        assert_eq!(InspectorValue::Text("ab".to_string()).hex_text(), None);
    }

    #[test]
    fn values_cover_their_width_and_text_its_length() {
        assert_eq!(InspectorValue::U8(0).byte_len(), 1);
        assert_eq!(InspectorValue::I16(0).byte_len(), 2);
        assert_eq!(InspectorValue::F32(0.0).byte_len(), 4);
        assert_eq!(InspectorValue::U64(0).byte_len(), 8);
        assert_eq!(InspectorValue::Text("abc".to_string()).byte_len(), 3);
        assert_eq!(InspectorValue::Text(String::new()).byte_len(), 0);
    }

    #[test]
    fn identical_nans_do_not_differ() {
        let nan = Some(InspectorValue::F32(f32::NAN));
        assert!(!values_differ(&nan, &nan.clone()));
    }
}
