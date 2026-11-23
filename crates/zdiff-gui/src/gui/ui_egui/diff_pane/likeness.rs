use std::sync::Arc;

use eframe::egui;
use zcommon::change_map::{
    ChangeMap, STRIP_BINS, THUMBNAIL_MAX_SIDE, bins, byte_bins, downscale_max, flag_bins,
};
use zcommon_ui::{
    change_map::change_preview_ui,
    likeness::{likeness_bar, percent},
};
use zdiff::{
    change_count::TextLikeness,
    diff_builder::{DiffRow, LineContent},
    myers::DiffPhases,
    pixel_metric::{MetricMap, PixelMetric},
};

use super::FileDiffPaneCtx;
use crate::{
    diff_ctx::{DiffRows, MinimalDiffCtx},
    file::LoadedFile,
    ui_egui::overview::side_changed,
    viewer::{Likeness, ViewerKind, audio_diff::SampleDifference},
};

#[derive(Clone)]
enum PreviewKey {
    Lines {
        rows: Arc<DiffRows>,
        lines: [usize; 2],
    },
    Bytes([LoadedFile; 2]),
    Pixels(Arc<MetricMap>),
    Samples(Arc<SampleDifference>),
}

impl PartialEq for PreviewKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Lines { rows: a, .. }, Self::Lines { rows: b, .. }) => Arc::ptr_eq(a, b),
            (Self::Bytes(a), Self::Bytes(b)) => a
                .iter()
                .zip(b)
                .all(|(a, b)| std::ptr::eq(a.bytes(), b.bytes())),
            (Self::Pixels(a), Self::Pixels(b)) => Arc::ptr_eq(a, b),
            (Self::Samples(a), Self::Samples(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl PreviewKey {
    fn text(diff_ctx: &MinimalDiffCtx) -> Self {
        let (left, right) = &*diff_ctx.precomputed_file_rows;
        Self::Lines {
            rows: diff_ctx.diff_rows.clone(),
            lines: [left.len(), right.len()],
        }
    }

    fn change_map(&self) -> ChangeMap {
        match self {
            Self::Lines { rows, lines } => line_map(rows, *lines),
            Self::Bytes([left, right]) => {
                ChangeMap::Strip(byte_bins(left.bytes(), right.bytes(), STRIP_BINS))
            }
            Self::Pixels(map) => {
                downscale_max(map.width, map.height, &map.magnitude, THUMBNAIL_MAX_SIDE)
            }
            Self::Samples(difference) => sample_map(difference),
        }
    }
}

// Collapsed rows hide unchanged lines only, so the flags of the shown rows cover every change.
fn line_map(rows: &[DiffRow], lines: [usize; 2]) -> ChangeMap {
    let mut changed = lines.map(|count| vec![false; count]);
    for row in rows {
        for (flags, content) in changed.iter_mut().zip([&row.left, &row.right]) {
            if let LineContent::Code { line_num, .. } = content
                && side_changed(content)
                && let Some(flag) = (*line_num as usize)
                    .checked_sub(1)
                    .and_then(|line| flags.get_mut(line))
            {
                *flag = true;
            }
        }
    }
    ChangeMap::Sides(changed.map(|flags| flag_bins(&flags, STRIP_BINS)))
}

/// The peak of left minus right per stretch of frames, full scale at 1.
fn sample_map(difference: &SampleDifference) -> ChangeMap {
    let wave = &difference.wave;
    let channels = wave.channels.max(1);
    ChangeMap::Strip(bins(wave.samples.len() / channels, STRIP_BINS, |frames| {
        wave.samples[frames.start * channels..frames.end * channels]
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs())) as f64
    }))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

    fn description(self) -> &'static str {
        match self {
            Self::Lines => "1 - changed lines / lines of both sides; ignored text is no change",
            Self::Tokens => "1 - changed tokens / tokens of both sides, ignored tokens left out",
        }
    }

    fn readout(self, likeness: TextLikeness) -> Likeness {
        let ratio = match self {
            Self::Lines => likeness.lines,
            Self::Tokens => likeness.tokens,
        };
        Likeness::Measured {
            ratio,
            detail: self.description().to_string(),
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
    Fixed {
        label: &'static str,
        about: &'static str,
    },
}

pub(super) fn likeness_ui(ui: &mut egui::Ui, ctx: &mut FileDiffPaneCtx, text_viewer: bool) {
    let Some((readout, metrics, preview)) = shown_likeness(ctx, text_viewer) else {
        return;
    };
    readout_ui(ui, &readout, preview);
    match metrics {
        Metrics::Text(chosen, phases) => {
            let shown = shown_text_metric(*chosen, phases);
            metric_combo(ui, shown.label(), |ui| {
                for &metric in text_metrics(phases) {
                    if ui
                        .selectable_label(metric == shown, metric.label())
                        .on_hover_text(metric.description())
                        .clicked()
                    {
                        *chosen = metric;
                    }
                }
            })
        }
        Metrics::Pixel(chosen) => metric_combo(ui, chosen.label(), |ui| {
            for metric in PixelMetric::ALL {
                ui.selectable_value(chosen, metric, metric.label())
                    .on_hover_text(metric.description());
            }
        }),
        Metrics::Fixed { label, about } => fixed_metric(ui, label, about),
    }
    ui.separator();
}

fn shown_likeness<'a>(
    ctx: &'a mut FileDiffPaneCtx,
    text_viewer: bool,
) -> Option<(Likeness, Metrics<'a>, Option<PreviewKey>)> {
    match ctx.media_view {
        Some(ViewerKind::Audio) => {
            let (audio, _) = ctx.audio_view.as_ref()?;
            let preview = audio.sample_difference().cloned().map(PreviewKey::Samples);
            let metric = Metrics::Fixed {
                label: "RMS",
                about: "Likeness of the samples: how loud left minus right is (RMS), \
                        relative to the loudness of both sides",
            };
            return Some((audio.likeness()?, metric, preview));
        }
        Some(ViewerKind::Video) => {
            let frames = ctx.video_frames.as_ref()?;
            let metric = Metrics::Fixed {
                label: "Frames",
                about: "Likeness of the frames: the mean difference of the difference \
                        timeline's frames, under the image Metric",
            };
            return Some((frames.likeness()?, metric, None));
        }
        _ => {}
    }
    if let Some(hex) = &ctx.hex_view {
        let preview = hex
            .file_1
            .zip(hex.file_2)
            .map(|(left, right)| PreviewKey::Bytes([left.clone(), right.clone()]));
        let metric = Metrics::Fixed {
            label: "Bytes",
            about: "Likeness of the bytes: the share equal at the same offset",
        };
        return Some((hex.likeness()?, metric, preview));
    }
    if let Some(image) = ctx.image_view.as_deref() {
        let likeness = image.likeness()?;
        let preview = image.difference_map().cloned().map(PreviewKey::Pixels);
        let settings = ctx.image_settings.as_deref_mut()?;
        return Some((likeness, Metrics::Pixel(&mut settings.metric), preview));
    }
    let phases = ctx.diff_options.phases;
    let likeness = match ctx.diff_ctx {
        Some(diff_ctx) if !ctx.diff_loading => {
            let shown = shown_text_metric(*ctx.text_likeness, phases);
            shown.readout(diff_ctx.likeness?)
        }
        _ if text_viewer
            && ctx.diff_loading
            && ctx.file_source_path_valid
            && ctx.file_target_path_valid =>
        {
            Likeness::Pending
        }
        _ => return None,
    };
    let preview = ctx.diff_ctx.map(PreviewKey::text);
    Some((
        likeness,
        Metrics::Text(&mut *ctx.text_likeness, phases),
        preview,
    ))
}

fn readout_ui(ui: &mut egui::Ui, readout: &Likeness, preview: Option<PreviewKey>) {
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
            let id = ui.id().with("likeness_preview");
            likeness_bar(ui, *ratio, size).on_hover_ui(|ui| {
                ui.label(format!("{} % alike: {detail}", percent(*ratio)));
                if let Some(key) = preview {
                    ui.separator();
                    let data = key.clone();
                    change_preview_ui(ui, id, 1, key, move |_| Some(Ok(data.change_map())));
                }
            });
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

fn fixed_metric(ui: &mut egui::Ui, label: &str, about: &str) {
    ui.add(egui::Label::new(egui::RichText::new(label).weak()).selectable(false))
        .on_hover_text(about);
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
