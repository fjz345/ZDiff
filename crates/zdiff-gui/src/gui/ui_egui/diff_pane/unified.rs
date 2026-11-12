use super::*;

/// The unified column of one set of side-by-side rows. `rows` holds each unified row's content as
/// the left side, so the widths and wrapped heights measure it like a one-sided diff.
pub(super) struct UnifiedView {
    pub(super) mode: TextMode,
    pub(super) meta: Vec<UnifiedRow>,
    pub(super) rows: Arc<DiffRows>,
    pub(super) row_chars: Vec<[RowChars; 2]>,
    pub(super) ruler: std::sync::OnceLock<Vec<ChangeSpan>>,
}

pub(super) fn unified_change_spans(meta: &[UnifiedRow]) -> Vec<ChangeSpan> {
    let mut spans: Vec<ChangeSpan> = Vec::new();
    for (idx, row) in meta.iter().enumerate() {
        let kind = match row.kind {
            UnifiedKind::Delete => ChangeKind::Deleted,
            UnifiedKind::Insert => ChangeKind::Inserted,
            UnifiedKind::Context | UnifiedKind::Collapsed => continue,
        };
        match spans.last_mut() {
            Some(last) if last.end == idx && last.kind == kind => last.end = idx + 1,
            _ => spans.push(ChangeSpan {
                start: idx,
                end: idx + 1,
                kind,
            }),
        }
    }
    spans
}

pub(super) fn unified_view(
    cache: &mut RowsMemo<Arc<UnifiedView>>,
    source: &Arc<DiffRows>,
    source_chars: &[[RowChars; 2]],
    mode: TextMode,
) -> Arc<UnifiedView> {
    let _scope = zcommon_ui::frame_stats::scope("Unified view");
    cache
        .get_or_insert_with(
            source,
            |view| view.mode == mode,
            || Arc::new(build_unified_view(source, source_chars, mode)),
        )
        .clone()
}

fn build_unified_view(
    source: &[DiffRow],
    source_chars: &[[RowChars; 2]],
    mode: TextMode,
) -> UnifiedView {
    let meta = match mode.single_side() {
        Some(side) => single_file_rows(source, side),
        None => unified_rows(source),
    };
    let rows = meta
        .iter()
        .map(|u| {
            let row = &source[u.row];
            let mut left = match u.side {
                ActiveSide::Left => row.left.clone(),
                ActiveSide::Right => row.right.clone(),
            };
            // Ghosts pad a side with the other side's text; a patch shows only the line's own.
            if let LineContent::Code { tokens, .. } = &mut left {
                tokens.retain(|(_, _, is_ghost)| !is_ghost);
            }
            DiffRow {
                left,
                right: LineContent::Void,
            }
        })
        .collect();
    let row_chars = meta
        .iter()
        .map(|u| {
            let text = source_chars[u.row][u.side as usize].text;
            [
                RowChars {
                    text,
                    ghosts: CharCount::default(),
                },
                RowChars::default(),
            ]
        })
        .collect();
    UnifiedView {
        mode,
        meta,
        rows: Arc::new(rows),
        row_chars,
        ruler: std::sync::OnceLock::new(),
    }
}
