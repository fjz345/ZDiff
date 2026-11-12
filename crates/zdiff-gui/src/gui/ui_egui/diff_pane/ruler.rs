use super::*;

/// The overview ruler's change spans, built once per rows.
pub(super) fn ruler_spans<'c>(
    cache: &'c mut RowsMemo<Vec<ChangeSpan>>,
    diff_ctx: &MinimalDiffCtx,
) -> &'c [ChangeSpan] {
    let rows = &diff_ctx.diff_rows;
    cache.get_or_insert_with(rows, |_| true, || row_change_spans(rows))
}
