use super::*;

pub(super) const LINE_HUNK_BAR: egui::Color32 = egui::Color32::from_rgb(230, 140, 40);
pub(super) const SCOPE_PAIR_COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(255, 120, 200),
    egui::Color32::from_rgb(120, 220, 255),
    egui::Color32::from_rgb(255, 220, 90),
    egui::Color32::from_rgb(160, 255, 120),
    egui::Color32::from_rgb(200, 150, 255),
    egui::Color32::from_rgb(255, 150, 100),
];
pub(super) const TOKEN_DELETE_OUTLINE: egui::Color32 = egui::Color32::from_rgb(255, 90, 90);
pub(super) const TOKEN_INSERT_OUTLINE: egui::Color32 = egui::Color32::from_rgb(90, 230, 90);
pub(super) const CHAR_UNDERLINE: egui::Color32 = egui::Color32::from_rgb(0, 220, 255);

/// Scope hunks are numbered like the Phase Trace window's table.
pub(super) fn paint_trace_gutter(ui: &egui::Ui, trace: &PhaseTrace, side: ActiveSide, content: &LineContent) {
    let LineContent::Code { line_num, .. } = content else {
        return;
    };
    let Ok(line) = usize::try_from(line_num - 1) else {
        return;
    };
    let lines = |span: &TraceSpan| match side {
        ActiveSide::Left => span.source_lines.clone(),
        ActiveSide::Right => span.target_lines.clone(),
    };
    let rect = ui.max_rect();
    let painter = ui.painter();
    if trace
        .line_hunks
        .iter()
        .any(|hunk| lines(hunk).contains(&line))
    {
        let bar = egui::Rect::from_min_size(
            rect.min + egui::vec2(4.0, 0.0),
            egui::vec2(2.0, rect.height()),
        );
        painter.rect_filled(bar, 0.0, LINE_HUNK_BAR);
    }
    let x = rect.left() + GUTTER_WIDTH;
    for (i, pair) in trace.scope_hunks.iter().enumerate() {
        let lines = lines(pair);
        if !lines.contains(&line) {
            continue;
        }
        let stroke = egui::Stroke::new(1.5_f32, SCOPE_PAIR_COLORS[i % SCOPE_PAIR_COLORS.len()]);
        let (first, last) = (line == lines.start, line + 1 == lines.end);
        let top = if first { rect.top() + 1.0 } else { rect.top() };
        let bottom = if last {
            rect.bottom() - 1.0
        } else {
            rect.bottom()
        };
        painter.vline(x, top..=bottom, stroke);
        if first {
            painter.hline(x..=x + GUTTER_GAP, top, stroke);
            let number = painter.layout_no_wrap(
                (i + 1).to_string(),
                egui::FontId::monospace(9.0),
                stroke.color,
            );
            let pos = egui::pos2(x - 1.0 - number.size().x, rect.top());
            let behind = egui::Rect::from_min_size(pos, number.size());
            painter.rect_filled(behind, 0.0, egui::Color32::from_black_alpha(200));
            painter.galley(pos, number, stroke.color);
        }
        if last {
            painter.hline(x..=x + GUTTER_GAP, bottom, stroke);
        }
    }
}

pub(super) fn token_hunk_outlines<T: RawTokenTrait>(
    trace: &PhaseTrace,
    tokens: &[(DiffResult, IsDimmed, IsGhost)],
    row_text: &RowText,
    file_source: Option<&CachedFile<T>>,
    file_target: Option<&CachedFile<T>>,
) -> Vec<(std::ops::Range<usize>, egui::Color32)> {
    let in_token_hunk = |idx: Option<u32>, side: fn(&TraceSpan) -> &std::ops::Range<usize>| {
        idx.is_some_and(|i| {
            let i = i as usize;
            trace
                .token_hunks
                .iter()
                .any(|hunk| side(&hunk.span).contains(&i))
        })
    };
    let mut outlines = Vec::new();
    let mut start = 0;
    for token in tokens {
        let result = &token.0;
        // One token at a time, so each range is exactly what build_row_text adds to the row.
        let row = build_row_text(std::slice::from_ref(token), &[], file_source, file_target);
        let len = row.text.len();
        let range = start..start + len;
        start += len;
        if len == 0 || result.hide_in_diff {
            continue;
        }
        match result.operation {
            DiffOp::Delete if in_token_hunk(result.token_source_idx, |s| &s.source) => {
                outlines.push((range, TOKEN_DELETE_OUTLINE));
            }
            DiffOp::Insert if in_token_hunk(result.token_target_idx, |s| &s.target) => {
                outlines.push((range, TOKEN_INSERT_OUTLINE));
            }
            _ => {}
        }
    }
    debug_assert_eq!(start, row_text.text.len());
    outlines
}

/// `origin` is where `galley` was painted.
pub(super) fn paint_trace_row(
    painter: &egui::Painter,
    outlines: &[(std::ops::Range<usize>, egui::Color32)],
    row_text: &RowText,
    galley: &egui::Galley,
    origin: egui::Pos2,
) {
    for (range, color) in outlines {
        for rect in text_range_rects(galley, &row_text.text, range.clone()) {
            painter.rect_stroke(
                rect.translate(origin.to_vec2()),
                0.0,
                egui::Stroke::new(1.0_f32, *color),
                egui::StrokeKind::Inside,
            );
        }
    }
    for range in &row_text.changed_chars {
        for rect in text_range_rects(galley, &row_text.text, range.clone()) {
            let rect = rect.translate(origin.to_vec2());
            painter.hline(
                rect.x_range(),
                rect.bottom() - 1.0,
                egui::Stroke::new(1.5_f32, CHAR_UNDERLINE),
            );
        }
    }
}

/// One galley-relative rect per laid-out row that the byte range of `text` covers.
pub(super) fn text_range_rects(
    galley: &egui::Galley,
    text: &str,
    range: std::ops::Range<usize>,
) -> Vec<egui::Rect> {
    let start = text[..range.start].chars().count();
    let end = text[..range.end].chars().count();
    let mut rects = Vec::new();
    let mut row_start = 0;
    for row in &galley.rows {
        let row_end = row_start + row.glyphs.len();
        let (a, b) = (start.max(row_start), end.min(row_end));
        if a < b {
            let first = &row.glyphs[a - row_start];
            let last = &row.glyphs[b - 1 - row_start];
            rects.push(egui::Rect::from_x_y_ranges(
                row.pos.x + first.pos.x..=row.pos.x + last.pos.x + last.advance_width,
                row.pos.y..=row.pos.y + row.size.y,
            ));
        }
        row_start = row_end;
    }
    rects
}

