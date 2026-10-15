use std::sync::Arc;

use eframe::egui;
use zcommon::job::Job;
use zcommon::path::split_path;
use zcommon_ui::pathbar::{PathBarAction, path_bar};
use zdiff::universal_path::UniversalPath;

use crate::p4::{P4Command, P4Runner, parse_dirs, parse_files_at_head};

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

pub struct DepotFolder;

impl FolderSource for DepotFolder {
    fn list(&self, folder: &str) -> Result<Vec<BrowserEntry>, String> {
        let p4 = P4Command::new(false).for_file(UniversalPath::Depot(folder.to_owned(), None));
        depot_listing(folder, &p4)
    }
}

fn depot_listing(folder: &str, p4: &impl P4Runner) -> Result<Vec<BrowserEntry>, String> {
    let children = format!("{}/*", folder.trim_end_matches('/'));
    let dirs = parse_dirs(&p4.run(&["-ztag", "dirs", &children])?);
    let files = parse_files_at_head(&p4.run(&["-ztag", "files", &children])?);
    let entry = |path: String, is_dir: bool| BrowserEntry {
        name: path.rsplit('/').next().unwrap_or_default().to_owned(),
        path,
        is_dir,
    };
    Ok(dirs
        .into_iter()
        .map(|path| entry(path, true))
        .chain(files.into_iter().map(|path| entry(path, false)))
        .collect())
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
    Loading,
    Done(ListingResult),
}

pub struct FolderBrowser {
    folder: String,
    source: Arc<dyn FolderSource>,
    listing: Listing,
    job: Job<ListingResult>,
}

impl FolderBrowser {
    pub fn open(ctx: &egui::Context, folder: String, source: Arc<dyn FolderSource>) -> Self {
        let ctx = ctx.clone();
        let mut browser = Self {
            folder: String::new(),
            source,
            listing: Listing::Loading,
            job: Job::new(move || ctx.request_repaint()),
        };
        browser.navigate_to(folder);
        browser
    }

    pub fn navigate(&mut self, _ctx: &egui::Context, folder: String) {
        self.navigate_to(folder);
    }

    fn navigate_to(&mut self, folder: String) {
        self.load(&folder);
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
        if !matches!(self.listing, Listing::Loading) {
            return;
        }
        let mut results = self.job.poll();
        if results.is_empty() && !self.job.is_running() {
            results = self.job.poll();
        }
        match results.pop() {
            Some(result) => self.listing = Listing::Done(result),
            None if !self.job.is_running() => {
                self.listing = Listing::Done(Err(format!(
                    "{}: listing stopped without a result",
                    self.folder
                )))
            }
            None => {}
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
            Listing::Loading => {
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
        while matches!(self.listing, Listing::Loading) {
            self.poll();
            std::thread::yield_now();
        }
        match &self.listing {
            Listing::Done(result) => result,
            Listing::Loading => unreachable!(),
        }
    }

    fn load(&mut self, folder: &str) {
        let (folder, source) = (folder.to_owned(), self.source.clone());
        self.job.start(move |job| {
            job.send(listing(&*source, &folder));
        });
        self.listing = Listing::Loading;
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use eframe::egui;
    use zdiff::universal_path::UniversalPath;

    use super::{
        BrowserAction, BrowserEntry, FolderBrowser, LocalFolder, depot_listing, listing,
        parent_folder,
    };
    use crate::p4::FakeP4;

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

    fn depot_entry(path: &str, is_dir: bool) -> BrowserEntry {
        BrowserEntry {
            name: path.rsplit('/').next().unwrap().to_owned(),
            path: path.to_owned(),
            is_dir,
        }
    }

    #[test]
    fn depot_listing_has_the_folders_and_the_files_at_head() {
        let p4 = FakeP4::new(|args: &[&str]| match args[1] {
            "dirs" => Ok("... dir //depot/main/src\n\n".to_owned()),
            "files" => Ok("... depotFile //depot/main/a b.txt\n\
                           ... rev 2\n\
                           ... action edit\n\
                           \n\
                           ... depotFile //depot/main/gone.txt\n\
                           ... rev 3\n\
                           ... action delete\n\
                           \n"
            .to_owned()),
            _ => panic!("unexpected p4 {args:?}"),
        });
        assert_eq!(
            depot_listing("//depot/main", &p4).unwrap(),
            [
                depot_entry("//depot/main/src", true),
                depot_entry("//depot/main/a b.txt", false),
            ]
        );
        assert_eq!(
            p4.calls(),
            [
                ["-ztag", "dirs", "//depot/main/*"],
                ["-ztag", "files", "//depot/main/*"],
            ]
        );
    }

    #[test]
    fn an_empty_depot_folder_lists_nothing_and_a_p4_error_is_the_listing_error() {
        let empty = FakeP4::new(|_: &[&str]| Ok(String::new()));
        assert_eq!(depot_listing("//depot/main", &empty).unwrap(), []);

        let failing = FakeP4::new(|_: &[&str]| Err("Connect to server failed".to_owned()));
        let error = depot_listing("//depot/main", &failing).unwrap_err();
        assert!(error.contains("Connect to server failed"), "{error}");
    }

    #[test]
    fn a_picked_depot_file_has_no_revision() {
        let p4 = FakeP4::new(|args: &[&str]| match args[1] {
            "files" => Ok("... depotFile //depot/main/a.txt\n... rev 7\n... action edit\n".into()),
            _ => Ok(String::new()),
        });
        let entries = depot_listing("//depot/main", &p4).unwrap();
        assert_eq!(
            UniversalPath::from(entries[0].path.as_str()),
            UniversalPath::Depot("//depot/main/a.txt".into(), None)
        );
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
