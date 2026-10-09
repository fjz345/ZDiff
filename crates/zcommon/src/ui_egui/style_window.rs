use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Color32, Id, Theme, ThemePreference};

use super::common::show_custom_popup;
use crate::style::{Mode, Palette, Rgba8, Role, SharedRole, Style, Tone, style_file_path};

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

fn theme_preference(mode: Mode) -> ThemePreference {
    match mode {
        Mode::Dark => ThemePreference::Dark,
        Mode::Light => ThemePreference::Light,
        Mode::System => ThemePreference::System,
    }
}

fn publish(ctx: &egui::Context, style: &Style) {
    ctx.set_theme(theme_preference(style.mode));
    ctx.data_mut(|d| d.insert_temp(palette_id(), Arc::new(style.palette.clone())));
}

#[derive(Default)]
pub struct StyleWindow {
    open: bool,
    style: Style,
    path: Option<PathBuf>,
    dirty: bool,
}

impl StyleWindow {
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
                changed = style_ui::<R>(ui, &mut self.style, app_group);
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

fn style_ui<R: Role>(ui: &mut egui::Ui, style: &mut Style, app_group: &str) -> bool {
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
