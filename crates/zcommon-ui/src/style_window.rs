use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Color32, Id, Theme, ThemePreference};

use super::common::show_custom_popup;
use zcommon::style::{
    Mode, Palette, Rgba8, Role, RowLook, SYNTAX_PRESETS, SharedRole, Style, SyntaxCategory,
    SyntaxColors, Tone, style_file_path,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoRole {}

impl Role for NoRole {
    fn all() -> &'static [Self] {
        &[]
    }

    fn name(self) -> &'static str {
        match self {}
    }

    fn label(self) -> &'static str {
        match self {}
    }

    fn dark(self) -> Rgba8 {
        match self {}
    }

    fn light(self) -> Rgba8 {
        match self {}
    }
}

fn palette_id() -> Id {
    Id::new("zcommon_style_palette")
}

fn row_looks_id() -> Id {
    Id::new("zcommon_style_row_looks")
}

pub fn tone(ctx: &egui::Context) -> Tone {
    match ctx.theme() {
        Theme::Dark => Tone::Dark,
        Theme::Light => Tone::Light,
    }
}

pub fn role_color<R: Role>(ctx: &egui::Context, role: R) -> Color32 {
    let tone = tone(ctx);
    let palette = ctx.data(|d| d.get_temp::<Arc<Palette>>(palette_id()));
    let Rgba8([r, g, b, a]) = match palette {
        Some(palette) => palette.get(role, tone),
        None => role.default_color(tone),
    };
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

fn row_looks(ctx: &egui::Context) -> (RowLook, RowLook) {
    ctx.data(|d| d.get_temp(row_looks_id())).unwrap_or_default()
}

pub fn void_look(ctx: &egui::Context) -> RowLook {
    row_looks(ctx).0
}

pub fn collapsed_look(ctx: &egui::Context) -> RowLook {
    row_looks(ctx).1
}

pub fn syntax_colors(ctx: &egui::Context) -> SyntaxColors {
    let tone = tone(ctx);
    let palette = ctx.data(|d| d.get_temp::<Arc<Palette>>(palette_id()));
    match palette {
        Some(palette) => palette.syntax_colors(tone),
        None => Palette::default().syntax_colors(tone),
    }
}

fn theme_preference(mode: Mode) -> ThemePreference {
    match mode {
        Mode::Dark => ThemePreference::Dark,
        Mode::Light => ThemePreference::Light,
        Mode::System => ThemePreference::System,
    }
}

fn publish(ctx: &egui::Context, style: &Style) {
    ctx.set_theme(theme_preference(style.mode));
    ctx.data_mut(|d| {
        d.insert_temp(palette_id(), Arc::new(style.palette.clone()));
        d.insert_temp(row_looks_id(), (style.void_look, style.collapsed_look));
    });
}

#[derive(Default)]
pub struct StyleWindow {
    open: bool,
    style: Style,
    path: Option<PathBuf>,
    dirty: bool,
    row_looks: bool,
    syntax: bool,
}

impl StyleWindow {
    pub fn with_row_looks(self) -> Self {
        Self {
            row_looks: true,
            ..self
        }
    }

    pub fn with_syntax(self) -> Self {
        Self {
            syntax: true,
            ..self
        }
    }

    pub fn load(&mut self, ctx: &egui::Context) {
        self.path = style_file_path();
        self.style = match &self.path {
            Some(path) => Style::load_or_default(path),
            None => {
                log::warn!(
                    "No settings folder found; using the default style, changes are not saved"
                );
                Style::default()
            }
        };
        self.dirty = false;
        publish(ctx, &self.style);
    }

    // Reloads first: the other app may have saved since this one loaded, and a save writes it all.
    pub fn open(&mut self, ctx: &egui::Context) {
        if !self.open && !self.dirty {
            self.load(ctx);
        }
        self.open = true;
    }

    // Call every frame, open or not: a change is written once the pointer is released.
    pub fn show<R: Role>(&mut self, ctx: &egui::Context, app_group: &str) {
        if self.open {
            let mut changed = false;
            show_custom_popup(ctx, &mut self.open, "Style", true, |ui| {
                changed = style_ui::<R>(ui, &mut self.style, app_group, self.syntax);
                if self.row_looks {
                    changed |= row_looks_ui(ui, &mut self.style);
                }
            });
            if changed {
                publish(ctx, &self.style);
                self.dirty = true;
            }
        }
        if self.dirty && !ctx.input(|i| i.pointer.any_down()) {
            self.save();
        }
    }

    fn save(&mut self) {
        self.dirty = false;
        let Some(path) = &self.path else {
            return;
        };
        if let Err(e) = self.style.save(path) {
            log::error!("Failed to save style {}: {e}", path.display());
        }
    }
}

fn style_ui<R: Role>(ui: &mut egui::Ui, style: &mut Style, app_group: &str, syntax: bool) -> bool {
    let mut changed = false;
    let tone = tone(ui.ctx());
    ui.horizontal(|ui| {
        ui.label("Mode");
        for (mode, label) in [
            (Mode::Dark, "Dark"),
            (Mode::Light, "Light"),
            (Mode::System, "System"),
        ] {
            changed |= ui.radio_value(&mut style.mode, mode, label).changed();
        }
        if ui.button("Reset all").clicked() {
            style.palette.reset_all::<SharedRole>();
            style.palette.reset_all::<R>();
            if syntax {
                style.palette.reset_all::<SyntaxCategory>();
            }
            changed = true;
        }
    });
    ui.weak(match tone {
        Tone::Dark => "Editing the dark mode colors",
        Tone::Light => "Editing the light mode colors",
    });
    changed |= role_group::<SharedRole>(ui, "Shared", &mut style.palette, tone);
    if !R::all().is_empty() {
        changed |= role_group::<R>(ui, app_group, &mut style.palette, tone);
    }
    if syntax {
        changed |= syntax_group(ui, &mut style.palette, tone);
    }
    changed
}

fn syntax_group(ui: &mut egui::Ui, palette: &mut Palette, tone: Tone) -> bool {
    let mut changed = role_group::<SyntaxCategory>(ui, "Syntax", palette, tone);
    let current = palette.syntax_colors(tone);
    let selected = SYNTAX_PRESETS
        .iter()
        .find(|preset| preset.colors == current)
        .map_or("Custom", |preset| preset.name);
    ui.horizontal(|ui| {
        ui.label("Preset");
        egui::ComboBox::from_id_salt("style_syntax_preset")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                for preset in SYNTAX_PRESETS {
                    if ui
                        .selectable_label(preset.name == selected, preset.name)
                        .clicked()
                    {
                        preset.apply(palette, tone);
                        changed = true;
                    }
                }
            });
    });
    changed
}

fn role_group<R: Role>(ui: &mut egui::Ui, title: &str, palette: &mut Palette, tone: Tone) -> bool {
    let mut changed = false;
    ui.separator();
    ui.strong(title);
    egui::Grid::new(("style_roles", title))
        .num_columns(3)
        .show(ui, |ui| {
            for &role in R::all() {
                ui.label(role.label());
                let Rgba8(mut color) = palette.get(role, tone);
                let response = if role.has_alpha() {
                    ui.color_edit_button_srgba_unmultiplied(&mut color)
                } else {
                    let mut rgb = [color[0], color[1], color[2]];
                    let response = ui.color_edit_button_srgb(&mut rgb);
                    color[..3].copy_from_slice(&rgb);
                    response
                };
                if response.changed() {
                    palette.set(role, tone, Rgba8(color));
                    changed = true;
                }
                let is_default = palette.get(role, tone) == role.default_color(tone);
                if ui
                    .add_enabled(!is_default, egui::Button::new("Reset"))
                    .clicked()
                {
                    palette.reset(role, tone);
                    changed = true;
                }
                ui.end_row();
            }
        });
    changed
}

fn row_looks_ui(ui: &mut egui::Ui, style: &mut Style) -> bool {
    let mut changed = false;
    ui.separator();
    ui.strong("Rows");
    egui::Grid::new("style_row_looks")
        .num_columns(4)
        .show(ui, |ui| {
            for (label, look) in [
                ("Void rows", &mut style.void_look),
                ("Collapsed rows", &mut style.collapsed_look),
            ] {
                ui.label(label);
                for (value, text) in [
                    (RowLook::Solid, "Solid"),
                    (RowLook::Stripes, "Stripes"),
                    (RowLook::Empty, "Empty"),
                ] {
                    changed |= ui.radio_value(look, value, text).changed();
                }
                ui.end_row();
            }
        });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_looks_are_solid_until_a_style_is_published() {
        let ctx = egui::Context::default();
        assert_eq!(void_look(&ctx), RowLook::Solid);
        assert_eq!(collapsed_look(&ctx), RowLook::Solid);

        let style = Style {
            void_look: RowLook::Stripes,
            collapsed_look: RowLook::Empty,
            ..Style::default()
        };
        publish(&ctx, &style);
        assert_eq!(void_look(&ctx), RowLook::Stripes);
        assert_eq!(collapsed_look(&ctx), RowLook::Empty);
    }
}
