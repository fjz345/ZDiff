use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use eframe::egui;

use crate::folder::roots::Side;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Folder,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Hidden,
    Disabled(&'static str),
    Enabled,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RowMenu {
    pub set_as_root: Item,
    pub set_as_folder: Item,
    pub open_in_explorer: Item,
    pub copy_path: Item,
}

/// `sides`: the row's entry in Folder 1 and Folder 2. None when the clicked side is empty.
pub fn row_menu(sides: [Option<Entry>; 2], clicked: Side) -> Option<RowMenu> {
    let clicked_entry = match clicked {
        Side::Folder1 => sides[0],
        Side::Folder2 => sides[1],
    }?;
    let (set_as_root, set_as_folder) = match clicked_entry {
        Entry::File => (Item::Hidden, Item::Hidden),
        Entry::Folder if sides == [Some(Entry::Folder); 2] => (Item::Enabled, Item::Enabled),
        Entry::Folder => (
            Item::Disabled("Needs this folder on both sides"),
            Item::Enabled,
        ),
    };
    Some(RowMenu {
        set_as_root,
        set_as_folder,
        open_in_explorer: Item::Enabled,
        copy_path: Item::Enabled,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum RowAction {
    SetRoots([PathBuf; 2]),
    SetRoot(Side, PathBuf),
    OpenInExplorer(PathBuf),
    CopyPath(PathBuf),
}

pub fn show_row_menu(
    ui: &mut egui::Ui,
    menu: &RowMenu,
    paths: [Option<&Path>; 2],
    clicked: Side,
) -> Option<RowAction> {
    let mut action = None;
    let mut item = |ui: &mut egui::Ui, item: Item, text: &str, on_click: &dyn Fn() -> RowAction| {
        let response = match item {
            Item::Hidden => return,
            Item::Disabled(reason) => ui
                .add_enabled(false, egui::Button::new(text))
                .on_disabled_hover_text(reason),
            Item::Enabled => ui.button(text),
        };
        if response.clicked() {
            action = Some(on_click());
        }
    };
    let path = |side: Side| {
        let path = match side {
            Side::Folder1 => paths[0],
            Side::Folder2 => paths[1],
        };
        path.expect("the menu only offers items for sides that exist")
            .to_path_buf()
    };
    item(ui, menu.set_as_root, "Set as root", &|| {
        RowAction::SetRoots([path(Side::Folder1), path(Side::Folder2)])
    });
    item(ui, menu.set_as_folder, "Set as Left Folder", &|| {
        RowAction::SetRoot(Side::Folder1, path(clicked))
    });
    item(ui, menu.set_as_folder, "Set as Right Folder", &|| {
        RowAction::SetRoot(Side::Folder2, path(clicked))
    });
    item(ui, menu.open_in_explorer, FILE_MANAGER.label(), &|| {
        RowAction::OpenInExplorer(path(clicked))
    });
    item(ui, menu.copy_path, "Copy path", &|| {
        RowAction::CopyPath(path(clicked))
    });
    action
}

pub type RootRequests = [Option<PathBuf>; 2];

pub fn apply_row_action(ctx: &egui::Context, action: RowAction, root_requests: &mut RootRequests) {
    match action {
        RowAction::SetRoots(paths) => *root_requests = paths.map(Some),
        RowAction::SetRoot(Side::Folder1, path) => root_requests[0] = Some(path),
        RowAction::SetRoot(Side::Folder2, path) => root_requests[1] = Some(path),
        RowAction::OpenInExplorer(path) => open_in_file_manager(&path),
        RowAction::CopyPath(path) => ctx.copy_text(path.to_string_lossy().into_owned()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileManager {
    Explorer,
    Finder,
    XdgOpen,
}

const FILE_MANAGER: FileManager = if cfg!(windows) {
    FileManager::Explorer
} else if cfg!(target_os = "macos") {
    FileManager::Finder
} else {
    FileManager::XdgOpen
};

impl FileManager {
    fn label(self) -> &'static str {
        match self {
            FileManager::Explorer => "Open in Explorer",
            FileManager::Finder => "Show in Finder",
            FileManager::XdgOpen => "Open in File Manager",
        }
    }

    fn command(self, path: &Path, is_dir: bool) -> (&'static str, Vec<OsString>) {
        match self {
            FileManager::Explorer => {
                let mut arg = OsString::from("/select,\"");
                arg.push(path);
                arg.push("\"");
                ("explorer.exe", vec![arg])
            }
            FileManager::Finder => ("open", vec!["-R".into(), path.into()]),
            FileManager::XdgOpen => {
                let folder = if is_dir {
                    path
                } else {
                    path.parent().unwrap_or(path)
                };
                ("xdg-open", vec![folder.into()])
            }
        }
    }
}

fn open_in_file_manager(path: &Path) {
    let (program, args) = FILE_MANAGER.command(path, path.is_dir());
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Explorer parses /select itself and needs the quotes inside the argument, so no Rust quoting.
        for arg in &args {
            command.raw_arg(arg);
        }
    }
    #[cfg(not(windows))]
    command.args(&args);
    // Explorer exits with 1 even when it worked, so only a failed spawn is an error.
    match command.spawn() {
        Ok(child) => zdiff_folder::external_diff_tool::wait_in_background(child),
        Err(e) => log::error!(
            "{} failed for {}: {e}",
            FILE_MANAGER.label(),
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_on_both_sides_offers_every_item_from_either_side() {
        let both = [Some(Entry::Folder), Some(Entry::Folder)];
        let expected = || RowMenu {
            set_as_root: Item::Enabled,
            set_as_folder: Item::Enabled,
            open_in_explorer: Item::Enabled,
            copy_path: Item::Enabled,
        };
        assert_eq!(row_menu(both, Side::Folder1), Some(expected()));
        assert_eq!(row_menu(both, Side::Folder2), Some(expected()));
    }

    #[test]
    fn a_folder_on_one_side_disables_set_as_root() {
        for (sides, clicked) in [
            ([Some(Entry::Folder), None], Side::Folder1),
            ([None, Some(Entry::Folder)], Side::Folder2),
        ] {
            let menu = row_menu(sides, clicked).unwrap();
            assert!(matches!(menu.set_as_root, Item::Disabled(_)));
            assert_eq!(menu.set_as_folder, Item::Enabled);
            assert_eq!(menu.open_in_explorer, Item::Enabled);
            assert_eq!(menu.copy_path, Item::Enabled);
        }
    }

    #[test]
    fn clicking_the_empty_side_of_a_one_sided_row_opens_no_menu() {
        assert_eq!(row_menu([Some(Entry::Folder), None], Side::Folder2), None);
        assert_eq!(row_menu([None, Some(Entry::File)], Side::Folder1), None);
    }

    #[test]
    fn a_file_offers_only_explorer_and_copy_path() {
        let expected = || RowMenu {
            set_as_root: Item::Hidden,
            set_as_folder: Item::Hidden,
            open_in_explorer: Item::Enabled,
            copy_path: Item::Enabled,
        };
        let both = [Some(Entry::File), Some(Entry::File)];
        assert_eq!(row_menu(both, Side::Folder1), Some(expected()));
        assert_eq!(row_menu(both, Side::Folder2), Some(expected()));
        assert_eq!(
            row_menu([None, Some(Entry::File)], Side::Folder2),
            Some(expected())
        );
    }

    #[test]
    fn a_folder_facing_a_file_follows_the_clicked_side() {
        let sides = [Some(Entry::Folder), Some(Entry::File)];
        let folder = row_menu(sides, Side::Folder1).unwrap();
        assert!(matches!(folder.set_as_root, Item::Disabled(_)));
        assert_eq!(folder.set_as_folder, Item::Enabled);
        let file = row_menu(sides, Side::Folder2).unwrap();
        assert_eq!(file.set_as_root, Item::Hidden);
        assert_eq!(file.set_as_folder, Item::Hidden);
        assert_eq!(file.copy_path, Item::Enabled);
    }

    #[test]
    fn explorer_selects_the_item_with_the_quotes_inside_one_argument() {
        for is_dir in [false, true] {
            assert_eq!(
                FileManager::Explorer.command(Path::new(r"C:\my ws\a b.txt"), is_dir),
                (
                    "explorer.exe",
                    vec![OsString::from(r#"/select,"C:\my ws\a b.txt""#)]
                )
            );
        }
    }

    #[test]
    fn finder_reveals_the_item() {
        for is_dir in [false, true] {
            assert_eq!(
                FileManager::Finder.command(Path::new("/my ws/a b"), is_dir),
                (
                    "open",
                    vec![OsString::from("-R"), OsString::from("/my ws/a b")]
                )
            );
        }
    }

    #[test]
    fn xdg_open_opens_the_folder_of_a_file_or_the_folder_itself() {
        assert_eq!(
            FileManager::XdgOpen.command(Path::new("/my ws/src/a b.txt"), false),
            ("xdg-open", vec![OsString::from("/my ws/src")])
        );
        assert_eq!(
            FileManager::XdgOpen.command(Path::new("/my ws/src"), true),
            ("xdg-open", vec![OsString::from("/my ws/src")])
        );
    }
}
