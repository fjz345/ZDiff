use eframe::egui;
use zcommon_ui::likeness::{likeness_bar, percent};
use zdiff::{change_count::TextLikeness, myers::DiffPhases, pixel_metric::PixelMetric};

use super::FileDiffPaneCtx;
use crate::viewer::{Likeness, ViewerKind};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TextMetric {
    #[default]
    Lines,
    Tokens,
}

impl TextMetric {
    fn label(self) -> &'static str {
        match self {
            Self::Lines => "Lines",
            Self::Tokens => "Tokens",
        }
    }

    fn readout(self, likeness: TextLikeness) -> Likeness {
        let (ratio, detail) = match self {
            Self::Lines => (
                likeness.lines,
                "1 - changed lines / lines of both sides; ignored text is no change",
            ),
            Self::Tokens => (
                likeness.tokens,
                "1 - changed tokens / tokens of both sides, ignored tokens left out",
            ),
        };
        Likeness::Measured {
            ratio,
            detail: detail.to_string(),
        }
    }
}

// Without the token phase a changed line's tokens all count as changed, so their share misleads.
fn text_metrics(phases: DiffPhases) -> &'static [TextMetric] {
    if phases.token {
        &[TextMetric::Lines, TextMetric::Tokens]
    } else {
        &[TextMetric::Lines]
    }
}

fn shown_text_metric(chosen: TextMetric, phases: DiffPhases) -> TextMetric {
    if text_metrics(phases).contains(&chosen) {
        chosen
    } else {
        TextMetric::Lines
    }
}

enum Metrics<'a> {
    Text(&'a mut TextMetric, DiffPhases),
    Pixel(&'a mut PixelMetric),
    Fixed(&'static str),
}

pub(super) fn likeness_ui(ui: &mut egui::Ui, ctx: &mut FileDiffPaneCtx, text_viewer: bool) {
    let Some((readout, metrics)) = shown_likeness(ctx, text_viewer) else {
        return;
    };
    readout_ui(ui, &readout);
    match metrics {
        Metrics::Text(chosen, phases) => {
            let shown = shown_text_metric(*chosen, phases);
            metric_combo(ui, shown.label(), |ui| {
                for &metric in text_metrics(phases) {
                    if ui
                        .selectable_label(metric == shown, metric.label())
                        .clicked()
                    {
                        *chosen = metric;
                    }
                }
            })
        }
        Metrics::Pixel(chosen) => metric_combo(ui, chosen.label(), |ui| {
            for metric in PixelMetric::ALL {
                ui.selectable_value(chosen, metric, metric.label());
            }
        }),
        Metrics::Fixed(label) => metric_combo(ui, label, |_| {}),
    }
    ui.separator();
}

fn shown_likeness<'a>(
    ctx: &'a mut FileDiffPaneCtx,
    text_viewer: bool,
) -> Option<(Likeness, Metrics<'a>)> {
    match ctx.media_view {
        Some(ViewerKind::Audio) => {
            let (audio, _) = ctx.audio_view.as_ref()?;
            return Some((audio.likeness()?, Metrics::Fixed("RMS")));
        }
        Some(ViewerKind::Video) => {
            let frames = ctx.video_frames.as_ref()?;
            return Some((frames.likeness()?, Metrics::Fixed("Frames")));
        }
        _ => {}
    }
    if let Some(hex) = &ctx.hex_view {
        return Some((hex.likeness()?, Metrics::Fixed("Bytes")));
    }
    if let Some(image) = ctx.image_view.as_deref() {
        let likeness = image.likeness()?;
        let settings = ctx.image_settings.as_deref_mut()?;
        return Some((likeness, Metrics::Pixel(&mut settings.metric)));
    }
    let phases = ctx.diff_options.phases;
    let likeness = match ctx.diff_ctx {
        Some(diff_ctx) => {
            let shown = shown_text_metric(*ctx.text_likeness, phases);
            shown.readout(diff_ctx.likeness?)
        }
        None if text_viewer
            && ctx.diff_loading
            && ctx.file_source_path_valid
            && ctx.file_target_path_valid =>
        {
            Likeness::Pending
        }
        None => return None,
    };
    Some((likeness, Metrics::Text(&mut *ctx.text_likeness, phases)))
}

fn readout_ui(ui: &mut egui::Ui, readout: &Likeness) {
    let size = egui::vec2(
        64.0,
        ui.text_style_height(&egui::TextStyle::Monospace) + 4.0,
    );
    match readout {
        Likeness::Pending => {
            ui.add_sized(size, egui::Spinner::new())
                .on_hover_text("Measuring the likeness...");
        }
        Likeness::Measured { ratio, detail } => {
            likeness_bar(ui, *ratio, size)
                .on_hover_text(format!("{} % alike: {detail}", percent(*ratio)));
        }
    }
}

fn metric_combo(ui: &mut egui::Ui, selected: &str, options: impl FnOnce(&mut egui::Ui)) {
    egui::ComboBox::from_id_salt("likeness_metric")
        .selected_text(selected)
        .show_ui(ui, options)
        .response
        .on_hover_text("How the likeness is measured");
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE_ONLY: DiffPhases = DiffPhases {
        scope: false,
        line: true,
        token: false,
        character: false,
    };

    #[test]
    fn the_token_metric_needs_the_token_phase() {
        assert_eq!(text_metrics(LINE_ONLY), [TextMetric::Lines]);
        assert_eq!(
            text_metrics(DiffPhases::default()),
            [TextMetric::Lines, TextMetric::Tokens]
        );
        assert_eq!(
            shown_text_metric(TextMetric::Tokens, LINE_ONLY),
            TextMetric::Lines
        );
        assert_eq!(
            shown_text_metric(TextMetric::Tokens, DiffPhases::default()),
            TextMetric::Tokens
        );
    }
}
