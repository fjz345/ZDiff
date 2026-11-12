use std::{
    io,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use eframe::egui;

const FILE_NAME: &str = "vsync.txt";

struct Setting {
    path: Option<PathBuf>,
    running: bool,
    saved: Mutex<Saved>,
}

struct Saved {
    on: bool,
    error: Option<String>,
}

static SETTING: OnceLock<Setting> = OnceLock::new();

pub(crate) fn load(app_id: &str) -> bool {
    let path = eframe::storage_dir(app_id).map(|dir| dir.join(FILE_NAME));
    let on = path.as_deref().is_none_or(read);
    log::info!("V-sync {}", on_off(on));
    let setting = Setting {
        path,
        running: on,
        saved: Mutex::new(Saved { on, error: None }),
    };
    if SETTING.set(setting).is_err() {
        panic!("the v-sync setting loads once per process");
    }
    on
}

fn read(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Ok(text) => match text.trim() {
            "on" => true,
            "off" => false,
            other => {
                log::warn!(
                    "{}: expected on or off, found {other:?}; v-sync stays on",
                    path.display()
                );
                true
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => {
            log::warn!("Cannot read {}: {e}; v-sync stays on", path.display());
            true
        }
    }
}

fn write(path: &Path, on: bool) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, on_off(on))
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

pub(crate) fn checkbox(ui: &mut egui::Ui) {
    let Some(setting) = SETTING.get() else {
        return;
    };
    let mut saved = setting.saved.lock().expect("v-sync setting lock");
    let mut on = saved.on;
    ui.horizontal(|ui| {
        let toggled = ui
            .checkbox(&mut on, "V-sync (applies on next start)")
            .on_hover_text(
                "eframe sets the swap interval only when it creates the window, so a change \
                 takes effect when the app starts again. Off lets back-to-back repaints \
                 (video, animations, pointer motion) run as fast as the CPU allows.",
            )
            .changed();
        if toggled {
            saved.error = match &setting.path {
                Some(path) => write(path, on)
                    .err()
                    .map(|e| format!("Cannot save to {}: {e}", path.display())),
                None => Some("No app data folder, so the setting cannot be saved".to_owned()),
            };
            match &saved.error {
                Some(error) => log::error!("V-sync: {error}"),
                None => saved.on = on,
            }
        }
        ui.weak(format!("running: {}", on_off(setting.running)));
    });
    if let Some(error) = &saved.error {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_keeps_vsync_on() {
        let dir = tempfile::tempdir().unwrap();

        assert!(read(&dir.path().join(FILE_NAME)));
    }

    #[test]
    fn the_file_turns_vsync_off_or_on() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);

        std::fs::write(&path, "off\r\n").unwrap();
        assert!(!read(&path));
        std::fs::write(&path, "on").unwrap();
        assert!(read(&path));
    }

    #[test]
    fn unexpected_content_keeps_vsync_on() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "false").unwrap();

        assert!(read(&path));
    }

    #[test]
    fn a_written_setting_reads_back_and_creates_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ZDiff").join("data").join(FILE_NAME);

        write(&path, false).unwrap();
        assert!(!read(&path));
        write(&path, true).unwrap();
        assert!(read(&path));
    }
}
