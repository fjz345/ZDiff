use super::*;

/// The unified column of one set of side-by-side rows. `rows` holds each unified row's content as
/// the left side, so the widths and wrapped heights measure it like a one-sided diff.
pub(super) struct UnifiedView {
    // Kept alive so the pointer comparison can't match a new allocation at the same address.
    pub(super) source: Arc<DiffRows>,
    pub(super) mode: TextMode,
    pub(super) meta: Vec<UnifiedRow>,
    pub(super) rows: Arc<DiffRows>,
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
    cache: &mut Option<Arc<UnifiedView>>,
    source: &Arc<DiffRows>,
    mode: TextMode,
) -> Arc<UnifiedView> {
    if let Some(view) = cache
        && Arc::ptr_eq(&view.source, source)
        && view.mode == mode
    {
        return view.clone();
    }
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
    let view = Arc::new(UnifiedView {
        source: source.clone(),
        mode,
        meta,
        rows: Arc::new(rows),
        ruler: std::sync::OnceLock::new(),
    });
    *cache = Some(view.clone());
    view
}

