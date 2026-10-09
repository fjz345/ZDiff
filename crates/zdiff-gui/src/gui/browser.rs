use std::sync::{Arc, mpsc};

use eframe::egui;
use zcommon::{
    path::split_path,
    ui_egui::pathbar::{PathBarAction, path_bar},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

pub trait FolderSource: Send + Sync {
    fn list(&self, folder: &str) -> Result<Vec<BrowserEntry>, String>;
}

pub struct LocalFolder;

impl FolderSource for LocalFolder {
    fn list(&self, folder: &str) -> Result<Vec<BrowserEntry>, String> {
        let error = |e: std::io::Error| format!("{folder}: {e}");
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(folder).map_err(error)? {
            let path = entry.map_err(error)?.path();
            entries.push(BrowserEntry {
                name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                // Follows links, so a linked folder or junction is entered like a folder.
                is_dir: path.is_dir(),
                path: path.to_string_lossy().into_owned(),
            });
        }
        Ok(entries)
    }
}

/// Folders first, then files, each by name ignoring case.
pub fn listing(source: &dyn FolderSource, folder: &str) -> Result<Vec<BrowserEntry>, String> {
    let mut entries = source.list(folder)?;
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(entries)
}

pub fn parent_folder(folder: &str) -> Option<String> {
    let mut segments = split_path(folder);
    segments.pop()?;
    segments.pop().map(|parent| parent.path)
}

#[derive(Debug, PartialEq, Eq)]
pub enum BrowserAction {
    Pick(String),
    Close,
}

type ListingResult = Result<Vec<BrowserEntry>, String>;

enum Listing {
    Loading(mpsc::Receiver<ListingResult>),
    Done(ListingResult),
}

pub struct FolderBrowser {
    folder: String,
    source: Arc<dyn FolderSource>,
    listing: Listing,
}

impl FolderBrowser {
    pub fn open(ctx: &egui::Context, folder: String, source: Arc<dyn FolderSource>) -> Self {
        let listing = load(ctx, &folder, &source);
        Self {
            folder,
            source,
            listing,
        }
    }

    pub fn navigate(&mut self, ctx: &egui::Context, folder: String) {
        self.listing = load(ctx, &folder, &self.source);
        self.folder = folder;
    }

    /// A folder is entered; a file is picked.
    pub fn activate(&mut self, ctx: &egui::Context, entry: &BrowserEntry) -> Option<BrowserAction> {
        if entry.is_dir {
            self.navigate(ctx, entry.path.clone());
            None
        } else {
            Some(BrowserAction::Pick(entry.path.clone()))
        }
    }

    fn poll(&mut self) {
        let Listing::Loading(rx) = &self.listing else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => self.listing = Listing::Done(result),
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.listing = Listing::Done(Err(format!(
                    "{}: listing stopped without a result",
                    self.folder
                )))
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<BrowserAction> {
        self.poll();
        let mut action = None;
        let mut target = None;
        let mut clicked = None;
        let segments = split_path(&self.folder);
        ui.horizontal(|ui| {
            let height = ui.spacing().interact_size.y;
            let bar_width = ui.available_width() - height - ui.spacing().item_spacing.x;
            let bar = ui
                .allocate_ui(egui::vec2(bar_width, height), |ui| {
                    path_bar(
                        ui,
                        "folder_browser_path_bar",
                        &segments,
                        0,
                        |i| i + 1 < segments.len(),
                        None,
                        false,
                    )
                })
                .inner;
            match bar {
                Some(PathBarAction::Segment(i)) => target = Some(segments[i].path.clone()),
                Some(PathBarAction::Commit(text)) => {
                    target = Some(text.trim().trim_matches('"').to_owned())
                }
                None => {}
            }
            if ui
                .add_sized([height, height], egui::Button::new("✖"))
                .on_hover_text("Close (Esc)")
                .clicked()
            {
                action = Some(BrowserAction::Close);
            }
        });
        ui.separator();
        if let Some(parent) = parent_folder(&self.folder)
            && ui.selectable_label(false, "📁 ..").clicked()
        {
            target = Some(parent);
        }
        match &self.listing {
            Listing::Loading(_) => {
                ui.spinner();
            }
            Listing::Done(Err(error)) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            Listing::Done(Ok(entries)) => {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                let row_height = ui.text_style_height(&egui::TextStyle::Button)
                    + 2.0 * ui.spacing().button_padding.y;
                egui::ScrollArea::vertical().auto_shrink(false).show_rows(
                    ui,
                    row_height,
                    entries.len(),
                    |ui, range| {
                        for entry in &entries[range] {
                            let text = if entry.is_dir {
                                format!("📁 {}", entry.name)
                            } else {
                                entry.name.clone()
                            };
                            if ui.selectable_label(false, text).clicked() {
                                clicked = Some(entry.clone());
                            }
                        }
                    },
                );
            }
        }
        let ctx = ui.ctx().clone();
        if let Some(entry) = clicked {
            action = action.or(self.activate(&ctx, &entry));
        }
        if let Some(folder) = target {
            self.navigate(&ctx, folder);
        }
        action
    }

    #[cfg(test)]
    fn wait(&mut self) -> &ListingResult {
        if let Listing::Loading(rx) = &self.listing {
            self.listing = Listing::Done(rx.recv().expect("listing thread sends a result"));
        }
        match &self.listing {
            Listing::Done(result) => result,
            Listing::Loading(_) => unreachable!(),
        }
    }
}

fn load(ctx: &egui::Context, folder: &str, source: &Arc<dyn FolderSource>) -> Listing {
    let (tx, rx) = mpsc::channel();
    let (ctx, folder, source) = (ctx.clone(), folder.to_owned(), source.clone());
    std::thread::Builder::new()
        .name("folder listing".into())
        .spawn(move || {
            // Fails only when the browser moved on or closed and dropped the receiver.
            let _ = tx.send(listing(&*source, &folder));
            ctx.request_repaint();
        })
        .expect("spawn the folder listing thread");
    Listing::Loading(rx)
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use eframe::egui;

    use super::{BrowserAction, BrowserEntry, FolderBrowser, LocalFolder, listing, parent_folder};

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for name in ["b.txt", "A.txt", "c.rs"] {
            fs::write(dir.path().join(name), name).unwrap();
        }
        for name in ["zeta", "Beta", "alpha"] {
            fs::create_dir(dir.path().join(name)).unwrap();
        }
        dir
    }

    fn entry(dir: &tempfile::TempDir, name: &str, is_dir: bool) -> BrowserEntry {
        BrowserEntry {
            name: name.to_owned(),
            path: dir.path().join(name).to_string_lossy().into_owned(),
            is_dir,
        }
    }

    #[test]
    fn local_listing_has_folders_first_then_files_each_sorted() {
        let dir = tree();
        let folder = dir.path().to_string_lossy();
        assert_eq!(
            listing(&LocalFolder, &folder).unwrap(),
            [
                entry(&dir, "alpha", true),
                entry(&dir, "Beta", true),
                entry(&dir, "zeta", true),
                entry(&dir, "A.txt", false),
                entry(&dir, "b.txt", false),
                entry(&dir, "c.rs", false),
            ]
        );
    }

    #[test]
    fn local_listing_of_a_missing_folder_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let error = listing(&LocalFolder, &missing.to_string_lossy()).unwrap_err();
        assert!(error.contains(&*missing.to_string_lossy()), "{error}");
    }

    #[test]
    fn parent_folder_is_the_previous_segment() {
        assert_eq!(parent_folder(r"C:\ws\src"), Some(r"C:\ws".to_owned()));
        assert_eq!(parent_folder(r"C:\ws"), Some(r"C:\".to_owned()));
        assert_eq!(parent_folder(r"C:\"), None);
        assert_eq!(
            parent_folder("//depot/main/src"),
            Some("//depot/main".to_owned())
        );
    }

    #[test]
    fn a_folder_is_entered_and_a_file_is_picked() {
        let dir = tree();
        let ctx = egui::Context::default();
        let mut browser = FolderBrowser::open(
            &ctx,
            dir.path().to_string_lossy().into_owned(),
            Arc::new(LocalFolder),
        );
        let entries = browser.wait().clone().unwrap();
        assert_eq!(browser.activate(&ctx, &entries[0]), None);
        assert_eq!(browser.folder, entries[0].path);
        assert_eq!(browser.wait().as_ref().unwrap(), &[]);

        let file = entry(&dir, "c.rs", false);
        assert_eq!(
            browser.activate(&ctx, &file),
            Some(BrowserAction::Pick(file.path.clone()))
        );
    }
}
