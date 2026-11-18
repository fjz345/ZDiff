use eframe::egui;
use zcommon::{universal_path::UniversalPath, vcs::revision::PathKind};

pub fn revision_dropdown(
    ui: &mut egui::Ui,
    id_salt: &str,
    kind: PathKind,
    path: &UniversalPath,
    previous: Option<&UniversalPath>,
) -> Option<UniversalPath> {
    let id = ui.make_persistent_id((id_salt, "revision_dropdown"));
    zcommon_ui::revision::revision_dropdown(ui, id, kind, path, previous)
        .filter(|picked| picked != path)
}
