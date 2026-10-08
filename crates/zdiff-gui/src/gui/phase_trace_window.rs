use std::ops::Range;

use eframe::egui;
use zdiff::trace::{PhaseTrace, TraceSpan};

const TABLE_HEIGHT: f32 = 220.0;

/// 1-based, inclusive.
fn lines(r: &Range<usize>) -> String {
    if r.is_empty() {
        format!("before {}", r.start + 1)
    } else {
        format!("{}-{}", r.start + 1, r.end)
    }
}

fn tokens(r: &Range<usize>) -> String {
    format!("{}..{}", r.start, r.end)
}

fn bytes(ranges: &[Range<usize>]) -> String {
    ranges
        .iter()
        .map(|r| format!("{}..{}", r.start, r.end))
        .collect::<Vec<_>>()
        .join(", ")
}

fn span_cells(span: &TraceSpan) -> [String; 4] {
    [
        lines(&span.source_lines),
        lines(&span.target_lines),
        tokens(&span.source),
        tokens(&span.target),
    ]
}

const SPAN_HEADER: [&str; 5] = [
    "#",
    "Left lines",
    "Right lines",
    "Left tokens",
    "Right tokens",
];

fn table<const N: usize>(
    ui: &mut egui::Ui,
    title: &str,
    header: [&str; N],
    num_rows: usize,
    row: impl Fn(usize) -> [String; N],
) {
    egui::CollapsingHeader::new(format!("{title} ({num_rows})"))
        .default_open(true)
        .show(ui, |ui| {
            if num_rows == 0 {
                ui.weak("none");
                return;
            }
            let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
            egui::ScrollArea::vertical()
                .id_salt(title)
                .max_height(TABLE_HEIGHT)
                .show_rows(ui, row_height, num_rows, |ui, rows| {
                    egui::Grid::new((title, "grid"))
                        .striped(true)
                        .show(ui, |ui| {
                            for label in header {
                                ui.strong(label);
                            }
                            ui.end_row();
                            for i in rows {
                                for cell in row(i) {
                                    ui.monospace(cell);
                                }
                                ui.end_row();
                            }
                        });
                });
        });
}

pub fn ui_phase_trace(ui: &mut egui::Ui, trace: Option<&PhaseTrace>) {
    let Some(trace) = trace else {
        ui.label("Waiting for the diff");
        return;
    };
    let times = trace.times;
    ui.label(format!(
        "Scope {:.2?} | Line {:.2?} | Token {:.2?} | Character {:.2?}",
        times.scope, times.line, times.token, times.character
    ));
    egui::ScrollArea::vertical().show(ui, |ui| {
        let spans = |spans: &[TraceSpan], i: usize| {
            let [a, b, c, d] = span_cells(&spans[i]);
            [(i + 1).to_string(), a, b, c, d]
        };
        table(
            ui,
            "Scope pairs",
            SPAN_HEADER,
            trace.scope_pairs.len(),
            |i| spans(&trace.scope_pairs, i),
        );
        table(ui, "Line hunks", SPAN_HEADER, trace.line_hunks.len(), |i| {
            spans(&trace.line_hunks, i)
        });
        table(
            ui,
            "Token hunks",
            [
                "#",
                "Left lines",
                "Right lines",
                "Equal",
                "Deleted",
                "Inserted",
            ],
            trace.token_hunks.len(),
            |i| {
                let hunk = &trace.token_hunks[i];
                [
                    (i + 1).to_string(),
                    lines(&hunk.span.source_lines),
                    lines(&hunk.span.target_lines),
                    hunk.equal.to_string(),
                    hunk.deleted.to_string(),
                    hunk.inserted.to_string(),
                ]
            },
        );
        table(
            ui,
            "Character ranges",
            [
                "#",
                "Left token",
                "Right token",
                "Left bytes",
                "Right bytes",
            ],
            trace.char_pairs.len(),
            |i| {
                let pair = &trace.char_pairs[i];
                [
                    (i + 1).to_string(),
                    pair.source_token.to_string(),
                    pair.target_token.to_string(),
                    bytes(&pair.source),
                    bytes(&pair.target),
                ]
            },
        );
    });
}
