use super::*;

/// The overview ruler's change spans, built once per rows.
pub(super) struct RulerSpans {
    // Kept alive so the pointer comparison can't match a new allocation at the same address.
    pub(super) rows: Arc<DiffRows>,
    pub(super) spans: Vec<ChangeSpan>,
}

pub(super) fn ruler_spans<'c>(
    cache: &'c mut Option<RulerSpans>,
    diff_ctx: &MinimalDiffCtx,
) -> &'c [ChangeSpan] {
    let hit = cache
        .as_ref()
        .is_some_and(|cached| Arc::ptr_eq(&cached.rows, &diff_ctx.diff_rows));
    if !hit {
        *cache = Some(RulerSpans {
            rows: diff_ctx.diff_rows.clone(),
            spans: row_change_spans(&diff_ctx.diff_rows),
        });
    }
    &cache.as_ref().expect("filled above").spans
}

