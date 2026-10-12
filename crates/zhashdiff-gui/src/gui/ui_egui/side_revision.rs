use std::path::{Path, PathBuf};

use eframe::egui;
use zcommon::{
    universal_path::{GitRev, UniversalPath},
    vcs::git::{GitCommand, GitRef, RefKind, list_refs, locate},
};

const WORKING_TREE: &str = "Working tree";
const MAX_LABEL_CHARS: usize = 24;

/// The folder of a side's root and its git revision; `None` is the working tree.
pub fn split_side(root: &Path) -> (PathBuf, Option<GitRev>) {
    match UniversalPath::from(root) {
        UniversalPath::Git(folder, rev) => (folder, Some(rev)),
        _ => (root.to_path_buf(), None),
    }
}

/// Same folder at another revision; a depot root is returned as is.
pub fn with_revision(root: &Path, rev: Option<GitRev>) -> PathBuf {
    if UniversalPath::from(root).is_depot() {
        return root.to_path_buf();
    }
    let (folder, _) = split_side(root);
    match rev {
        None => folder,
        Some(rev) => PathBuf::from(UniversalPath::Git(folder, rev).to_string()),
    }
}

/// Empty text is the working tree; anything else is a revision git resolves.
pub fn parse_revision(text: &str) -> Option<GitRev> {
    match text.trim() {
        "" => None,
        "index" => Some(GitRev::Index),
        rev => Some(GitRev::Rev(rev.to_owned())),
    }
}

fn revision_text(rev: &Option<GitRev>) -> String {
    match rev {
        None => WORKING_TREE.to_owned(),
        Some(GitRev::Index) => "Index".to_owned(),
        Some(GitRev::Rev(rev)) => rev.clone(),
    }
}

#[derive(Clone)]
struct MenuState {
    folder: PathBuf,
    text: String,
    refs: Result<Vec<GitRef>, String>,
}

impl MenuState {
    fn load(folder: PathBuf) -> Self {
        let git = GitCommand::default();
        let refs = locate(&git, &folder)
            .and_then(|location| list_refs(&git, &location.root))
            .map_err(|e| e.to_string());
        Self {
            folder,
            text: String::new(),
            refs,
        }
    }
}

/// Some(revision) when one was picked or typed; `Some(None)` is the working tree. Refs are listed
/// synchronously when the menu opens.
pub fn revision_dropdown(
    ui: &mut egui::Ui,
    id_salt: usize,
    root: Option<&Path>,
) -> Option<Option<GitRev>> {
    let Some(root) = root else {
        ui.add_enabled(false, egui::Button::new(WORKING_TREE));
        return None;
    };
    let (folder, current) = split_side(root);
    let state_id = ui.make_persistent_id(("side_revision_menu", id_salt));
    let text = revision_text(&current);
    let shown = if text.chars().count() > MAX_LABEL_CHARS {
        let head: String = text.chars().take(MAX_LABEL_CHARS - 1).collect();
        format!("{head}…")
    } else {
        text.clone()
    };
    let mut picked = None;
    let menu = ui.menu_button(shown, |ui| {
        let mut state = ui
            .data(|d| d.get_temp::<MenuState>(state_id))
            .filter(|state| state.folder == folder)
            .unwrap_or_else(|| MenuState::load(folder.clone()));
        picked = menu_contents(ui, &mut state, &current);
        ui.data_mut(|d| d.insert_temp(state_id, state));
        if picked.is_some() {
            ui.close();
        }
    });
    if menu.inner.is_none() {
        ui.data_mut(|d| d.remove::<MenuState>(state_id));
    }
    menu.response.on_hover_text(format!("Revision: {text}"));
    picked
}

fn menu_contents(
    ui: &mut egui::Ui,
    state: &mut MenuState,
    current: &Option<GitRev>,
) -> Option<Option<GitRev>> {
    let mut picked = None;
    let edit = ui.add(
        egui::TextEdit::singleline(&mut state.text)
            .hint_text("Commit or expression")
            .desired_width(220.0),
    );
    let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if entered && !state.text.trim().is_empty() {
        picked = Some(parse_revision(&state.text));
    }
    if ui
        .selectable_label(current.is_none(), WORKING_TREE)
        .clicked()
    {
        picked = Some(None);
    }
    if ui
        .selectable_label(*current == Some(GitRev::Index), "Index")
        .clicked()
    {
        picked = Some(Some(GitRev::Index));
    }
    match &state.refs {
        Err(e) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        Ok(refs) => {
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    for (kind, title) in [
                        (RefKind::Branch, "Branches"),
                        (RefKind::RemoteBranch, "Remote branches"),
                        (RefKind::Tag, "Tags"),
                    ] {
                        let mut group = refs.iter().filter(|r| r.kind == kind).peekable();
                        if group.peek().is_none() {
                            continue;
                        }
                        ui.weak(title);
                        for r in group {
                            let selected =
                                matches!(current, Some(GitRev::Rev(name)) if *name == r.name);
                            if ui.selectable_label(selected, &r.name).clicked() {
                                picked = Some(Some(GitRev::Rev(r.name.clone())));
                            }
                        }
                    }
                });
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rev(name: &str) -> Option<GitRev> {
        Some(GitRev::Rev(name.to_owned()))
    }

    #[test]
    fn working_tree_to_a_branch_keeps_the_sub_path() {
        assert_eq!(
            with_revision(Path::new(r"D:\repo\src\gui"), rev("main")),
            PathBuf::from(r"D:\repo\src\gui@git:main")
        );
    }

    #[test]
    fn branch_to_a_branch_with_a_slash_keeps_the_sub_path() {
        assert_eq!(
            with_revision(Path::new(r"D:\repo\src@git:main"), rev("origin/dev")),
            PathBuf::from(r"D:\repo\src@git:origin/dev")
        );
    }

    #[test]
    fn a_revision_to_the_index_keeps_the_sub_path() {
        assert_eq!(
            with_revision(Path::new(r"D:\repo\src@git:v1.2"), Some(GitRev::Index)),
            PathBuf::from(r"D:\repo\src@git:index")
        );
    }

    #[test]
    fn a_revision_to_the_working_tree_keeps_the_sub_path() {
        assert_eq!(
            with_revision(Path::new(r"D:\repo\src@git:origin/main"), None),
            PathBuf::from(r"D:\repo\src")
        );
    }

    #[test]
    fn a_depot_root_is_left_alone() {
        assert_eq!(
            with_revision(Path::new("//depot/main/src"), rev("main")),
            PathBuf::from("//depot/main/src")
        );
    }

    #[test]
    fn a_side_splits_into_its_folder_and_revision() {
        assert_eq!(
            split_side(Path::new(r"D:\repo\src@git:origin/main")),
            (PathBuf::from(r"D:\repo\src"), rev("origin/main"))
        );
        assert_eq!(
            split_side(Path::new(r"D:\repo\src@git:index")),
            (PathBuf::from(r"D:\repo\src"), Some(GitRev::Index))
        );
        assert_eq!(
            split_side(Path::new(r"D:\repo\src")),
            (PathBuf::from(r"D:\repo\src"), None)
        );
    }

    #[test]
    fn typed_text_is_a_revision_the_index_or_the_working_tree() {
        assert_eq!(parse_revision("  HEAD~3 "), rev("HEAD~3"));
        assert_eq!(parse_revision("index"), Some(GitRev::Index));
        assert_eq!(parse_revision("   "), None);
    }
}
