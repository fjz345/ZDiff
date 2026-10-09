//! Horizontal scrolling of the text diff's two sides: offsets in points, `[left, right]`.
//! Pure; the pane measures the rows and draws the scrollbars.

use eframe::egui::{self, Modifiers, Vec2};

use super::active_side::ActiveSide;

/// The modifier that turns the mouse wheel into horizontal scrolling over the text rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum WheelModifier {
    #[default]
    Shift,
    Ctrl,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WheelScroll {
    Toolkit,
    Horizontal(f32),
    Vertical(f32),
}

pub fn wheel_scroll(delta: Vec2, modifiers: Modifiers, setting: WheelModifier) -> WheelScroll {
    match setting {
        WheelModifier::Shift => WheelScroll::Toolkit,
        WheelModifier::Ctrl if modifiers.ctrl => WheelScroll::Horizontal(delta.x + delta.y),
        WheelModifier::Ctrl if modifiers.shift => WheelScroll::Vertical(delta.x + delta.y),
        WheelModifier::Ctrl => WheelScroll::Toolkit,
    }
}

/// Rewrites `wheel_scroll`'s horizontal and vertical wheels over `text_rects` into plain wheels
/// before egui reads them, so a Ctrl wheel scrolls (smoothed) instead of feeding the zoom delta.
pub fn remap_wheel_events(
    events: &mut [egui::Event],
    mut pointer: Option<egui::Pos2>,
    text_rects: [egui::Rect; 2],
    setting: WheelModifier,
) {
    for event in events {
        match event {
            egui::Event::PointerMoved(pos) => pointer = Some(*pos),
            egui::Event::PointerGone => pointer = None,
            egui::Event::MouseWheel {
                delta, modifiers, ..
            } if pointer.is_some_and(|p| text_rects.iter().any(|r| r.contains(p))) => {
                let remapped = match wheel_scroll(*delta, *modifiers, setting) {
                    WheelScroll::Toolkit => continue,
                    WheelScroll::Horizontal(x) => egui::vec2(x, 0.0),
                    WheelScroll::Vertical(y) => egui::vec2(0.0, y),
                };
                *delta = remapped;
                *modifiers = Modifiers::NONE;
            }
            _ => {}
        }
    }
}

/// Advance width of `text` laid out on one line, given each char's advance. egui gives tabs a
/// fixed advance, and monospace fonts have no kerning, so the sum is the laid-out width.
pub fn text_width(text: &str, glyph_width: &mut impl FnMut(char) -> f32) -> f32 {
    text.chars().map(|c| glyph_width(c)).sum()
}

/// How far a side scrolls: its widest row (`content_width`) ends at the viewport's right edge.
pub fn max_offset(content_width: f32, viewport_width: f32) -> f32 {
    (content_width - viewport_width).max(0.0)
}

/// Each side's scroll range. Linked sides share the larger one, so one offset fits both.
pub fn ranges(max: [f32; 2], linked: bool) -> [f32; 2] {
    if linked { [max[0].max(max[1]); 2] } else { max }
}

/// `offsets` clamped to `max` (from `max_offset`). Linked sides that differ, as right after
/// linking, take the left side's offset.
pub fn clamp(offsets: [f32; 2], max: [f32; 2], linked: bool) -> [f32; 2] {
    let max = ranges(max, linked);
    let offsets = if linked { [offsets[0]; 2] } else { offsets };
    [offsets[0].clamp(0.0, max[0]), offsets[1].clamp(0.0, max[1])]
}

/// Sets `side`'s offset, and with linked sides the other one too.
pub fn set(
    offsets: [f32; 2],
    side: ActiveSide,
    offset: f32,
    max: [f32; 2],
    linked: bool,
) -> [f32; 2] {
    let mut offsets = if linked { [offset; 2] } else { offsets };
    offsets[side_index(side)] = offset;
    clamp(offsets, max, linked)
}

pub fn side_index(side: ActiveSide) -> usize {
    match side {
        ActiveSide::Left => 0,
        ActiveSide::Right => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_mode_leaves_every_wheel_to_the_toolkit() {
        let delta = egui::vec2(0.0, -1.0);
        for modifiers in [
            Modifiers::NONE,
            Modifiers::SHIFT,
            Modifiers::CTRL,
            Modifiers::CTRL | Modifiers::SHIFT,
        ] {
            assert_eq!(
                wheel_scroll(delta, modifiers, WheelModifier::Shift),
                WheelScroll::Toolkit,
                "{modifiers:?}"
            );
        }
    }

    #[test]
    fn ctrl_mode_turns_ctrl_wheel_horizontal_and_shift_wheel_vertical() {
        let delta = egui::vec2(0.0, -1.0);
        assert_eq!(
            wheel_scroll(delta, Modifiers::CTRL, WheelModifier::Ctrl),
            WheelScroll::Horizontal(-1.0)
        );
        assert_eq!(
            wheel_scroll(delta, Modifiers::SHIFT, WheelModifier::Ctrl),
            WheelScroll::Vertical(-1.0)
        );
        assert_eq!(
            wheel_scroll(
                delta,
                Modifiers::CTRL | Modifiers::SHIFT,
                WheelModifier::Ctrl
            ),
            WheelScroll::Horizontal(-1.0)
        );
        assert_eq!(
            wheel_scroll(delta, Modifiers::NONE, WheelModifier::Ctrl),
            WheelScroll::Toolkit
        );
        assert_eq!(
            wheel_scroll(egui::vec2(2.0, 0.0), Modifiers::SHIFT, WheelModifier::Ctrl),
            WheelScroll::Vertical(2.0)
        );
    }

    fn wheel(modifiers: Modifiers) -> egui::Event {
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, -1.0),
            modifiers,
        }
    }

    fn line_wheel(delta: Vec2) -> egui::Event {
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta,
            modifiers: Modifiers::NONE,
        }
    }

    #[test]
    fn only_wheels_over_the_text_rows_are_rewritten() {
        let left = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0));
        let right = egui::Rect::from_min_max(egui::pos2(110.0, 0.0), egui::pos2(200.0, 100.0));
        let mut events = vec![
            wheel(Modifiers::CTRL),
            wheel(Modifiers::SHIFT),
            egui::Event::PointerMoved(egui::pos2(300.0, 50.0)),
            wheel(Modifiers::CTRL),
            egui::Event::PointerMoved(egui::pos2(150.0, 50.0)),
            wheel(Modifiers::CTRL),
            egui::Event::PointerGone,
            wheel(Modifiers::CTRL),
        ];

        remap_wheel_events(
            &mut events,
            Some(egui::pos2(50.0, 50.0)),
            [left, right],
            WheelModifier::Ctrl,
        );

        assert_eq!(
            events,
            [
                line_wheel(egui::vec2(-1.0, 0.0)),
                line_wheel(egui::vec2(0.0, -1.0)),
                egui::Event::PointerMoved(egui::pos2(300.0, 50.0)),
                wheel(Modifiers::CTRL),
                egui::Event::PointerMoved(egui::pos2(150.0, 50.0)),
                line_wheel(egui::vec2(-1.0, 0.0)),
                egui::Event::PointerGone,
                wheel(Modifiers::CTRL),
            ]
        );
    }

    #[test]
    fn shift_mode_rewrites_no_wheel() {
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0));
        let mut events = vec![wheel(Modifiers::CTRL), wheel(Modifiers::SHIFT)];

        remap_wheel_events(
            &mut events,
            Some(egui::pos2(50.0, 50.0)),
            [rect, rect],
            WheelModifier::Shift,
        );

        assert_eq!(events, [wheel(Modifiers::CTRL), wheel(Modifiers::SHIFT)]);
    }

    #[test]
    fn the_longest_lines_end_is_reachable_with_nothing_beyond_it() {
        let widths = [120.0, 450.0, 300.0];
        let content = widths.into_iter().fold(0.0, f32::max);
        let viewport = 200.0;

        let max = max_offset(content, viewport);

        assert_eq!(max, 250.0);
        assert_eq!(max + viewport, 450.0);
        let scrolled = set([0.0, 0.0], ActiveSide::Left, 10_000.0, [max, 0.0], false);
        assert_eq!(scrolled, [250.0, 0.0]);
    }

    #[test]
    fn content_narrower_than_the_viewport_or_no_rows_does_not_scroll() {
        assert_eq!(max_offset(150.0, 200.0), 0.0);
        assert_eq!(max_offset(200.0, 200.0), 0.0);
        assert_eq!(max_offset(0.0, 200.0), 0.0);
        assert_eq!(
            set([0.0, 0.0], ActiveSide::Right, 50.0, [0.0, 0.0], false),
            [0.0, 0.0]
        );
    }

    #[test]
    fn offsets_are_clamped_to_their_range_and_never_negative() {
        assert_eq!(clamp([300.0, -5.0], [250.0, 40.0], false), [250.0, 0.0]);
        // A shorter file or a wider window pulls a stored offset back.
        assert_eq!(clamp([100.0, 30.0], [60.0, 10.0], false), [60.0, 10.0]);
        assert_eq!(clamp([20.0, 5.0], [60.0, 10.0], false), [20.0, 5.0]);
    }

    #[test]
    fn unlinked_sides_scroll_independently_within_their_own_range() {
        let max = [100.0, 300.0];

        let offsets = set([10.0, 20.0], ActiveSide::Right, 250.0, max, false);
        assert_eq!(offsets, [10.0, 250.0]);

        let offsets = set(offsets, ActiveSide::Left, 250.0, max, false);
        assert_eq!(offsets, [100.0, 250.0]);
    }

    #[test]
    fn linked_sides_share_one_offset_within_the_larger_range() {
        let max = [100.0, 300.0];
        assert_eq!(ranges(max, true), [300.0, 300.0]);
        assert_eq!(ranges(max, false), max);

        // Past the left side's own end: the right side's longest line stays reachable.
        assert_eq!(
            set([0.0, 0.0], ActiveSide::Right, 250.0, max, true),
            [250.0, 250.0]
        );
        assert_eq!(
            set([0.0, 0.0], ActiveSide::Left, 250.0, max, true),
            [250.0, 250.0]
        );
        assert_eq!(
            set([0.0, 0.0], ActiveSide::Left, 999.0, max, true),
            [300.0, 300.0]
        );
    }

    #[test]
    fn linking_unequal_offsets_aligns_them_to_the_left_side() {
        assert_eq!(clamp([40.0, 90.0], [100.0, 300.0], true), [40.0, 40.0]);
        assert_eq!(clamp([400.0, 90.0], [100.0, 300.0], true), [300.0, 300.0]);
    }

    #[test]
    fn text_width_sums_each_chars_advance() {
        let mut glyph = |c: char| if c == '\t' { 4.0 } else { 1.0 };
        assert_eq!(text_width("", &mut glyph), 0.0);
        assert_eq!(text_width("ab\tcd", &mut glyph), 8.0);
        assert_eq!(text_width("åäö→", &mut glyph), 4.0);
    }

    fn laid_out_and_summed(text: &str) -> (f32, f32) {
        let ctx = egui::Context::default();
        let mut widths = (0.0, 0.0);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let font_id = egui::FontId::monospace(12.0);
            widths = ctx.fonts_mut(|fonts| {
                let galley =
                    fonts.layout_no_wrap(text.to_owned(), font_id.clone(), egui::Color32::WHITE);
                let summed = text_width(text, &mut |c| fonts.glyph_width(&font_id, c));
                (galley.size().x, summed)
            });
        });
        widths
    }

    #[test]
    fn text_width_matches_egui_monospace_layout() {
        // Layout rounds glyph positions to pixels, hence the tolerance.
        for text in [
            "fn main() { let x = 1; }",
            "\tindented\t tab",
            "åäö → ∑",
            "",
        ] {
            let (laid_out, summed) = laid_out_and_summed(text);
            assert!(
                (laid_out - summed).abs() < 0.5,
                "{text:?}: laid out {laid_out}, summed {summed}"
            );
        }
    }

    #[test]
    fn text_width_is_never_short_of_the_layout_for_fallback_glyphs() {
        // Emoji come from a fallback font that layout scales down; over-measuring only leaves
        // some blank space past the end of the line.
        let (laid_out, summed) = laid_out_and_summed("let 💥 = 1;");
        assert!(
            summed >= laid_out - 0.5,
            "laid out {laid_out}, summed {summed}"
        );
    }
}
