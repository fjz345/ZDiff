use super::*;

/// Lines hidden in a collapsed block share another line's row, hence the line check.
pub(super) fn row_find<'a>(
    matchers: [Option<&'a Matcher>; 2],
    hit: Option<FindHit>,
    side: ActiveSide,
    row: usize,
    content: &LineContent,
) -> RowFind<'a> {
    let line_num = match content {
        LineContent::Code { line_num, .. } => usize::try_from(*line_num).ok(),
        _ => None,
    };
    let current = hit
        .filter(|hit| hit.side == side && hit.row == row && line_num == Some(hit.line + 1))
        .map(|hit| hit.ordinal);
    RowFind {
        matcher: matchers[side as usize],
        current,
    }
}

/// Ghost text is visual-only: it is kept out of the label text so egui can neither select nor
/// copy it, and each ghost is a blank gap in the layout job instead. `ghosts` is (byte offset
/// into the real text, ghost width) in row order. Ghosts at the end of the text need no gap
/// because nothing follows them.
pub(super) fn insert_ghost_gaps(job: &mut egui::text::LayoutJob, ghosts: &[(usize, f32)]) {
    for &(offset, width) in ghosts {
        if offset >= job.text.len() {
            continue;
        }
        let mut i = job
            .sections
            .iter()
            .position(|s| s.byte_range.contains(&offset))
            .expect("layout sections cover the whole text");
        let section = &mut job.sections[i];
        if section.byte_range.start < offset {
            let mut tail = section.clone();
            tail.leading_space = 0.0;
            tail.byte_range.start = offset;
            section.byte_range.end = offset;
            job.sections.insert(i + 1, tail);
            i += 1;
        }
        job.sections[i].leading_space += width;
    }
}

/// Sets the color of `ranges` (byte ranges into the job's text), splitting sections at their ends.
pub(super) fn recolor_ranges(
    job: &mut egui::text::LayoutJob,
    ranges: &[std::ops::Range<usize>],
    color: egui::Color32,
) {
    let mut split_at = |offset: usize| {
        if let Some(i) = job
            .sections
            .iter()
            .position(|s| s.byte_range.start < offset && offset < s.byte_range.end)
        {
            let section = &mut job.sections[i];
            let mut tail = section.clone();
            tail.leading_space = 0.0;
            tail.byte_range.start = offset;
            section.byte_range.end = offset;
            job.sections.insert(i + 1, tail);
        }
    };
    for range in ranges {
        split_at(range.start);
        split_at(range.end);
    }
    for section in &mut job.sections {
        let r = &section.byte_range;
        if ranges
            .iter()
            .any(|range| range.start <= r.start && r.end <= range.end)
        {
            section.format.color = color;
        }
    }
}

/// Sets the background of `ranges` (byte ranges into the job's text, ordered by start, possibly
/// overlapping), splitting sections at their ends. One pass, as a row can have many matches.
pub(super) fn highlight_ranges(
    job: &mut egui::text::LayoutJob,
    ranges: &[std::ops::Range<usize>],
    background: egui::Color32,
) {
    if ranges.is_empty() {
        return;
    }
    let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range.clone()),
        }
    }
    let mut merged = merged.into_iter().peekable();
    let mut sections = Vec::with_capacity(job.sections.len() + 2 * merged.len());
    for section in job.sections.drain(..) {
        let std::ops::Range { mut start, end } = section.byte_range.clone();
        if start == end {
            sections.push(section);
            continue;
        }
        let mut leading_space = section.leading_space;
        while start < end {
            while merged.next_if(|r| r.end <= start).is_some() {}
            let (piece_end, highlighted) = match merged.peek() {
                Some(r) if r.start <= start => (end.min(r.end), true),
                Some(r) => (end.min(r.start), false),
                None => (end, false),
            };
            let mut piece = section.clone();
            piece.byte_range = start..piece_end;
            // The gap belongs before the section's first piece only.
            piece.leading_space = std::mem::take(&mut leading_space);
            if highlighted {
                piece.format.background = background;
            }
            sections.push(piece);
            start = piece_end;
        }
    }
    job.sections = sections;
}

/// Position of each inline ghost relative to the galley origin: inside its gap, which is just
/// before the glyph at its offset (`ghosts` as for `insert_ghost_gaps`, all inside the text).
/// Ghosts sharing an offset sit side by side in one gap. When the text wraps right at a gap, the
/// new row starts at the glyph and the gap is gone, so the ghost goes at the end of the row
/// before, where the gap would have been.
pub(super) fn ghost_positions(galley: &egui::Galley, text: &str, ghosts: &[(usize, f32)]) -> Vec<Vec2> {
    ghosts
        .iter()
        .enumerate()
        .map(|(i, &(offset, _))| {
            let same_offset = |(o, _): &&(usize, f32)| *o == offset;
            let gap_width: f32 = ghosts.iter().filter(same_offset).map(|g| g.1).sum();
            let before_in_gap: f32 = ghosts[..i].iter().filter(same_offset).map(|g| g.1).sum();
            let mut char_index = text[..offset].chars().count();
            let (row_index, row) = galley
                .rows
                .iter()
                .enumerate()
                .find(|(_, row)| {
                    let found = char_index < row.glyphs.len();
                    if !found {
                        char_index -= row.glyphs.len();
                    }
                    found
                })
                .expect("an inline ghost's offset is inside the text");
            let gap_start = if char_index == 0 && row_index > 0 {
                let before = &galley.rows[row_index - 1];
                egui::vec2(before.size.x, before.pos.y)
            } else {
                egui::vec2(row.glyphs[char_index].pos.x - gap_width, row.pos.y)
            };
            gap_start + egui::vec2(before_in_gap, 0.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::*;
    

    
    
    use zdiff::diff_builder::{LineContent, RowBg};

    use crate::ui_egui::copy_harness::measure_in_fresh_context;
    #[test]
    fn the_current_find_hit_paints_only_on_its_own_side_row_and_line() {
        use crate::{diff_ctx::FindHit, ui_egui::active_side::ActiveSide};
        let code = |line_num| LineContent::Code {
            tokens: Vec::new(),
            line_num,
            bg: RowBg::None,
            changed_chars: Vec::new(),
        };
        let hit = FindHit {
            side: ActiveSide::Right,
            line: 9,
            ordinal: 2,
            row: 4,
        };
        let current = |side, row, content: &LineContent| {
            row_find([None; 2], Some(hit), side, row, content).current
        };

        assert_eq!(current(ActiveSide::Right, 4, &code(10)), Some(2));
        assert_eq!(current(ActiveSide::Left, 4, &code(10)), None);
        assert_eq!(current(ActiveSide::Right, 5, &code(10)), None);
        assert_eq!(current(ActiveSide::Right, 4, &code(3)), None);
        assert_eq!(current(ActiveSide::Right, 4, &LineContent::Void), None);
        assert_eq!(
            row_find([None; 2], None, ActiveSide::Right, 4, &code(10)).current,
            None
        );
    }

    #[test]
    fn a_row_paints_matches_only_with_its_own_sides_matcher() {
        use crate::ui_egui::active_side::ActiveSide;
        let matcher = Matcher::new(&zcommon::find::FindQuery {
            text: "x".into(),
            ..Default::default()
        })
        .unwrap();
        let matchers = [None, Some(&matcher)];

        let matcher_on = |side| row_find(matchers, None, side, 0, &LineContent::Void).matcher;
        assert!(matcher_on(ActiveSide::Left).is_none());
        assert!(matcher_on(ActiveSide::Right).is_some());
    }

    fn job_with_one_section(text: &str) -> eframe::egui::text::LayoutJob {
        eframe::egui::text::LayoutJob::simple_singleline(
            text.to_owned(),
            eframe::egui::FontId::monospace(12.0),
            eframe::egui::Color32::WHITE,
        )
    }

    #[test]
    fn ghost_gap_splits_the_section_without_changing_the_text() {
        let mut job = job_with_one_section("abcd");

        insert_ghost_gaps(&mut job, &[(2, 7.0)]);

        assert_eq!(job.text, "abcd");
        let sections: Vec<_> = job
            .sections
            .iter()
            .map(|s| (s.byte_range.clone(), s.leading_space))
            .collect();
        assert_eq!(sections, [(0..2, 0.0), (2..4, 7.0)]);
    }

    #[test]
    fn recolor_splits_sections_at_the_range_ends_without_changing_the_text() {
        use eframe::egui::Color32;
        let dim = Color32::from_rgba_unmultiplied(1, 2, 3, 4);
        let colors = |job: &eframe::egui::text::LayoutJob| -> Vec<_> {
            job.sections
                .iter()
                .map(|s| (s.byte_range.clone(), s.format.color))
                .collect()
        };

        let mut job = job_with_one_section("abcdef");
        recolor_ranges(&mut job, &[1..3, 5..6], dim);
        assert_eq!(job.text, "abcdef");
        assert_eq!(
            colors(&job),
            [
                (0..1, Color32::WHITE),
                (1..3, dim),
                (3..5, Color32::WHITE),
                (5..6, dim)
            ]
        );

        // A range across a section boundary recolors both parts.
        let mut job = job_with_one_section("abc");
        job.append("def", 0.0, job.sections[0].format.clone());
        recolor_ranges(&mut job, &[Range { start: 2, end: 4 }], dim);
        assert_eq!(
            colors(&job),
            [
                (0..2, Color32::WHITE),
                (2..3, dim),
                (3..4, dim),
                (4..6, Color32::WHITE)
            ]
        );
    }

    #[test]
    fn highlight_splits_sections_once_and_merges_overlapping_ranges() {
        use eframe::egui::Color32;
        let bg = Color32::from_rgba_unmultiplied(1, 2, 3, 4);
        let backgrounds = |job: &eframe::egui::text::LayoutJob| -> Vec<_> {
            job.sections
                .iter()
                .map(|s| (s.byte_range.clone(), s.format.background, s.leading_space))
                .collect()
        };
        let none = Color32::TRANSPARENT;

        // Overlapping matches ("aa" in "aaaa") light up as one run; a later match stays apart.
        let mut job = job_with_one_section("aaaa-aa");
        job.sections[0].leading_space = 7.0;
        highlight_ranges(&mut job, &[0..2, 1..3, 2..4, 5..7], bg);
        assert_eq!(job.text, "aaaa-aa");
        assert_eq!(
            backgrounds(&job),
            [(0..4, bg, 7.0), (4..5, none, 0.0), (5..7, bg, 0.0)]
        );

        // A range across a section boundary highlights both parts; the second section keeps its
        // own leading space and color.
        let mut job = job_with_one_section("abc");
        let mut format = job.sections[0].format.clone();
        format.color = Color32::RED;
        job.append("def", 2.0, format);
        highlight_ranges(&mut job, &[Range { start: 2, end: 4 }], bg);
        assert_eq!(
            backgrounds(&job),
            [
                (0..2, none, 0.0),
                (2..3, bg, 0.0),
                (3..4, bg, 2.0),
                (4..6, none, 0.0)
            ]
        );
        assert_eq!(job.sections[2].format.color, Color32::RED);
        assert_eq!(job.sections[3].format.color, Color32::RED);

        // No ranges leaves the job as it was.
        let mut job = job_with_one_section("abc");
        highlight_ranges(&mut job, &[], bg);
        assert_eq!(backgrounds(&job), [(0..3, none, 0.0)]);
    }

    #[test]
    fn ghost_gaps_at_the_same_offset_add_up_and_trailing_ghosts_add_no_gap() {
        let mut job = job_with_one_section("abcd");

        insert_ghost_gaps(&mut job, &[(0, 1.0), (0, 2.0), (4, 5.0)]);

        let sections: Vec<_> = job
            .sections
            .iter()
            .map(|s| (s.byte_range.clone(), s.leading_space))
            .collect();
        assert_eq!(sections, [(0..4, 3.0)]);
    }

    /// Lays out `text` with one inline ghost at `offset`, wrapped at `width_chars` columns.
    /// Returns the ghost's position, the char width and the line height.
    fn inline_ghost_position(
        text: &str,
        offset: usize,
        ghost: &str,
        width_chars: f32,
    ) -> (eframe::egui::Vec2, f32, f32) {
        measure_in_fresh_context(|fonts, font_id| {
            let char_width = fonts.glyph_width(font_id, 'x');
            let job = eframe::egui::text::LayoutJob::single_section(
                text.to_owned(),
                eframe::egui::TextFormat::simple(font_id.clone(), eframe::egui::Color32::WHITE),
            );
            let ghosts = [GhostInsertion {
                byte_offset: offset,
                text: ghost.to_owned(),
            }];
            let side_text = layout_side_text(
                fonts,
                job,
                &ghosts,
                eframe::egui::Color32::WHITE,
                font_id,
                width_chars * char_width,
            );
            let [(pos, _)] = side_text.ghosts[..] else {
                panic!("expected one ghost");
            };
            (pos, char_width, fonts.row_height(font_id))
        })
    }

    fn assert_near(actual: eframe::egui::Vec2, expected: eframe::egui::Vec2) {
        assert!(
            (actual - expected).length() < 0.5,
            "{actual:?} vs {expected:?}"
        );
    }

    #[test]
    fn an_inline_ghost_on_a_wrapped_row_sits_before_its_glyph_on_that_row() {
        // "aaaa bbbb " fills the first row; "cc " then the ghost, then "dd" on the second.
        let (pos, char_width, line_height) =
            inline_ghost_position("aaaa bbbb cc dd", 13, "X", 10.5);

        assert_near(pos, eframe::egui::vec2(3.0 * char_width, line_height));
    }

    #[test]
    fn an_inline_ghost_where_the_text_wraps_goes_at_the_end_of_the_row_before() {
        // The gap pushes "cccc" to the next row, which starts at its first glyph, gap gone.
        let (pos, char_width, _) = inline_ghost_position("aaaa bbbb cccc", 10, "XYZW", 10.5);

        assert_near(pos, eframe::egui::vec2(10.0 * char_width, 0.0));
    }

}
