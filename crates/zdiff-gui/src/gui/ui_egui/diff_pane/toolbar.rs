use super::*;

pub const REGEX_IGNORE_PRESETS: &[BuiltinPreset] = &[
    BuiltinPreset {
        name: "Time",
        text: r"\b\d{1,2}:\d{2}:\d{2}([.,]\d+)?\b",
    },
    BuiltinPreset {
        name: "Date",
        text: r"\b\d{4}-\d{2}-\d{2}\b",
    },
    BuiltinPreset {
        name: "ISO timestamp",
        text: r"\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}([.,]\d+)?(Z|[+-]\d{2}:?\d{2})?",
    },
    BuiltinPreset {
        name: "Bracketed time",
        text: r"\[[^\]]*\d\d:\d\d:\d\d[^\]]*\]",
    },
    BuiltinPreset {
        name: "UE log prefix",
        text: r"^\[\d{4}\.\d\d\.\d\d-\d\d\.\d\d\.\d\d:\d{3}\]\[\s*\d+\]",
    },
    BuiltinPreset {
        name: "GUID",
        text: r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b",
    },
    BuiltinPreset {
        name: "Hex address",
        text: r"\b0x[0-9a-fA-F]+\b",
    },
];

/// Toolbar button with a popup editing the ignore patterns. Patterns are recompiled only when the
/// text changes; invalid ones are listed under the field and skipped by the diff.
pub(super) fn ignore_patterns_btn(
    ui: &mut egui::Ui,
    button_size: Vec2,
    patterns: &mut IgnorePatterns,
    enabled: bool,
    presets: &mut UserPresets,
    tooltip: &str,
) {
    let btn = egui::Button::new(egui::RichText::new(".*").monospace().strong())
        .selected(enabled && !patterns.is_empty());
    let response = ui.add_sized(button_size, btn).on_hover_text(tooltip);
    let open_id = response.id.with("ignore_patterns_open");
    let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    if response.clicked() {
        open = !open;
    }
    // The preset save dialog is a modal drawn from inside this popup: clicks in it land outside
    // the popup, and a modal leaves Esc to open popups. Closing the popup would drop the dialog.
    let dialog_open = open && ui.ctx().memory(|m| m.top_modal_layer()).is_some();
    if dialog_open {
        ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    }
    let mut keep_open = false;
    egui::Popup::from_response(&response)
        .open_bool(&mut open)
        .close_behavior(if dialog_open {
            egui::PopupCloseBehavior::IgnoreClicks
        } else {
            egui::PopupCloseBehavior::CloseOnClickOutside
        })
        .show(|ui| {
            ui.label("One regex per line, matched per line. Ignores tokens a match fully covers.");
            let picked = presets_menu(
                ui,
                "ignore_regex_presets",
                REGEX_IGNORE_PRESETS,
                presets,
                patterns.text(),
            );
            if let Some(text) = picked {
                *patterns = IgnorePatterns::new(text);
            }
            // A click in the Presets submenu (a foreground layer) closes its parents too.
            let ctx = ui.ctx();
            let on_popup = ctx
                .pointer_interact_pos()
                .and_then(|pos| ctx.layer_id_at(pos))
                .is_some_and(|layer| layer.order == egui::Order::Foreground);
            keep_open = ui.will_parent_close()
                && on_popup
                && !ui.input(|i| i.key_pressed(egui::Key::Escape));
            let mut text = patterns.text().to_owned();
            let edit = TextEdit::multiline(&mut text)
                .code_editor()
                .desired_rows(4)
                .hint_text(r"\d\d:\d\d:\d\d");
            if ui.add(edit).changed() {
                *patterns = IgnorePatterns::new(text);
            }
            for error in patterns.errors() {
                let message = format!("Line {}: {}", error.line + 1, error.message);
                let message = egui::RichText::new(message).monospace();
                ui.colored_label(ui.visuals().error_fg_color, message);
            }
        });
    ui.data_mut(|d| d.insert_temp(open_id, open || keep_open));
}

/// Auto / Text / Hex for the current pair. A warning sign carries the fallback reason when the
/// pair is shown in another viewer than it asked for.
pub(super) fn text_mode_combo(ui: &mut egui::Ui, text_mode: &mut TextMode) {
    egui::ComboBox::from_id_salt("text_mode")
        .selected_text(text_mode.label())
        .width(125.0)
        .show_ui(ui, |ui| {
            for mode in TextMode::ALL {
                ui.selectable_value(text_mode, mode, mode.label());
            }
        })
        .response
        .on_hover_text("How the text diff is laid out");
}

pub(super) fn viewer_override_combo(
    ui: &mut egui::Ui,
    viewer_override: &mut Option<ViewerKind>,
    fallback: Option<&str>,
) {
    let name = |kind: Option<ViewerKind>| kind.map_or("Auto", ViewerKind::name);
    egui::ComboBox::from_id_salt("viewer_override")
        .selected_text(name(*viewer_override))
        .width(50.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(viewer_override, None, name(None));
            for kind in ViewerKind::ALL {
                ui.selectable_value(viewer_override, Some(kind), name(Some(kind)));
            }
        })
        .response
        .on_hover_text("Viewer for this pair; resets when another pair is opened");
    if let Some(reason) = fallback {
        ui.add(egui::Label::new("⚠").selectable(false))
            .on_hover_text(reason);
    }
}

/// Label and per-stage tooltip of a completed diff. Plain text so the segment can move into a
/// shared status line.
pub(super) fn diff_status_text(
    num_add_deletes: MyersNumAddDelete,
    unit: Option<ChangeUnit>,
    times: &DiffStageTimes,
) -> (String, String) {
    let (adds, deletes) = num_add_deletes;
    let unit = unit.map_or(String::new(), |unit| format!(" {}", unit.label()));
    (
        format!("+{adds}/-{deletes}{unit}  {:.1?}", times.total()),
        format!(
            "Line diff: {:.1?}\nToken diff: {:.1?}\nIR: {:.1?}\nRows: {:.1?}",
            times.line_diff, times.token_diff, times.diff_ir, times.diff_rows
        ),
    )
}

// `label` is diff_status_text's, which leads with the counts.
pub(super) fn diff_status_job(
    label: &str,
    (adds, deletes): MyersNumAddDelete,
    font_id: &egui::FontId,
    [insert, delete]: [egui::Color32; 2],
) -> egui::text::LayoutJob {
    let (adds, deletes) = (format!("+{adds}"), format!("-{deletes}"));
    let counts = format!("{adds}/{deletes}");
    assert!(
        label.starts_with(&counts),
        "{label:?} doesn't lead with {counts:?}"
    );
    let mut job = egui::text::LayoutJob::default();
    for (text, color) in [
        (adds.as_str(), insert),
        ("/", egui::Color32::PLACEHOLDER),
        (deletes.as_str(), delete),
        (&label[counts.len()..], egui::Color32::PLACEHOLDER),
    ] {
        job.append(text, 0.0, egui::TextFormat::simple(font_id.clone(), color));
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    
    
    use zdiff::{
        change_count::ChangeUnit,
        diff_builder::DiffBuilderOptions,
        ignore::{IgnoreOptions, IgnorePatterns},
    };

    use crate::diff_ctx::DiffStageTimes;
    #[cfg(feature = "serde")]
    #[test]
    fn ignore_comments_is_persisted_with_the_diff_options() {
        let options = DiffBuilderOptions {
            ignore: IgnoreOptions {
                comments: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let json = serde_json::to_value(&options).unwrap();
        assert_eq!(json["ignore_comments"], true);
        let loaded: DiffBuilderOptions = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(loaded, options);

        // State saved before the option existed still loads, with the option off.
        let mut old = json;
        old.as_object_mut().unwrap().remove("ignore_comments");
        let loaded: DiffBuilderOptions = serde_json::from_value(old).unwrap();
        assert_eq!(loaded, DiffBuilderOptions::default());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn ignore_patterns_are_persisted_with_the_diff_options() {
        // The invalid line is kept, so a typo isn't lost on restart.
        let text = "\\d\\d:\\d\\d\n(";
        let options = DiffBuilderOptions {
            ignore: IgnoreOptions {
                patterns: IgnorePatterns::new(text),
                ..Default::default()
            },
            ..Default::default()
        };
        let json = serde_json::to_value(&options).unwrap();
        assert_eq!(json["ignore_regex"], text);
        let loaded: DiffBuilderOptions = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(loaded, options);
        // Loading compiles the patterns again.
        assert!(!loaded.ignore.patterns.is_empty());
        assert_eq!(loaded.ignore.patterns.errors().len(), 1);

        // State saved before the option existed still loads, with no patterns.
        let mut old = json;
        old.as_object_mut().unwrap().remove("ignore_regex");
        let loaded: DiffBuilderOptions = serde_json::from_value(old).unwrap();
        assert_eq!(loaded, DiffBuilderOptions::default());
    }

    fn ignored_runs<'a>(patterns: &IgnorePatterns, text: &'a str) -> Vec<&'a str> {
        use zdiff::lexer::Lexer;
        let tokens = Lexer::new(text).parse();
        let flags = patterns.flags(&tokens, text);
        let mut runs = Vec::new();
        let mut run: Option<std::ops::Range<usize>> = None;
        for (token, flag) in tokens.iter().zip(flags) {
            match (&mut run, flag) {
                (Some(r), true) => r.end = token.span.end,
                (None, true) => run = Some(token.span.clone()),
                (Some(_), false) => runs.push(&text[run.take().unwrap()]),
                (None, false) => {}
            }
        }
        runs.extend(run.map(|r| &text[r]));
        runs
    }

    #[test]
    fn regex_ignore_presets_compile_and_match_only_their_sample() {
        let samples: &[(&str, &str, &[&str])] = &[
            (
                "Time",
                "LogTemp: 12:34:56.789 started, took 3:04:05\n",
                &["12:34:56.789", "3:04:05"],
            ),
            ("Date", "Build 2024-03-15 v1.2-3\n", &["2024-03-15"]),
            (
                "ISO timestamp",
                "at 2024-03-15T10:20:30.123Z done\nat 2024-03-15 10:20:30+01:00 done\n",
                &["2024-03-15T10:20:30.123Z", "2024-03-15 10:20:30+01:00"],
            ),
            (
                "Bracketed time",
                "[2024-03-15 10:20:30.5] [Info] Loaded\n",
                &["[2024-03-15 10:20:30.5]"],
            ),
            (
                "UE log prefix",
                "[2024.03.15-10.20.30:123][  0]LogTemp: Display: [1]\n",
                &["[2024.03.15-10.20.30:123][  0]"],
            ),
            (
                "GUID",
                "Guid=3F2504E0-4F89-11D3-9A0C-0305E82C3301 id-1234\n",
                &["3F2504E0-4F89-11D3-9A0C-0305E82C3301"],
            ),
            (
                "Hex address",
                "at 0x7ff6a1b2c3d4 in main+42\n",
                &["0x7ff6a1b2c3d4"],
            ),
        ];
        assert_eq!(super::REGEX_IGNORE_PRESETS.len(), samples.len());
        for preset in super::REGEX_IGNORE_PRESETS {
            let (_, text, expected) = samples
                .iter()
                .find(|(name, ..)| *name == preset.name)
                .unwrap_or_else(|| panic!("no sample for preset \"{}\"", preset.name));
            let patterns = IgnorePatterns::new(preset.text);
            assert_eq!(patterns.errors(), [], "{}", preset.name);
            assert!(!patterns.is_empty(), "{}", preset.name);
            assert_eq!(ignored_runs(&patterns, text), *expected, "{}", preset.name);
        }
    }

    #[test]
    fn diff_status_shows_counts_and_total_time_with_each_stage_in_the_tooltip() {
        let times = DiffStageTimes {
            line_diff: Duration::from_micros(12_340),
            token_diff: Duration::from_micros(1_000),
            diff_ir: Duration::from_micros(500),
            diff_rows: Duration::from_millis(2),
        };

        let (label, tooltip) = diff_status_text((7, 3), Some(ChangeUnit::Line), &times);

        assert_eq!(label, "+7/-3 lines  15.8ms");
        assert_eq!(
            tooltip,
            "Line diff: 12.3ms\nToken diff: 1.0ms\nIR: 500.0µs\nRows: 2.0ms"
        );
        assert_eq!(
            diff_status_text((2, 1), Some(ChangeUnit::Scope), &times).0,
            "+2/-1 scopes  15.8ms"
        );
        assert_eq!(diff_status_text((0, 0), None, &times).0, "+0/-0  15.8ms");
    }

    #[test]
    fn diff_status_counts_use_the_insert_and_delete_colors() {
        use eframe::egui::{Color32, FontId};
        let (insert, delete, rest) = (Color32::GREEN, Color32::RED, Color32::PLACEHOLDER);

        let job = diff_status_job(
            "+17/-3  15.8ms",
            (17, 3),
            &FontId::default(),
            [insert, delete],
        );

        assert_eq!(job.text, "+17/-3  15.8ms");
        let sections: Vec<_> = job
            .sections
            .iter()
            .map(|s| (s.byte_range.clone(), s.format.color))
            .collect();
        assert_eq!(
            sections,
            [(0..3, insert), (3..4, rest), (4..6, delete), (6..14, rest)]
        );
    }

}
