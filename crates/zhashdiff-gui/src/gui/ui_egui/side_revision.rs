use std::path::{Path, PathBuf};

use eframe::egui;
use zcommon::{
    universal_path::{DepotRev, GitRev, UniversalPath},
    vcs::{
        git::{GitCommand, GitRef, RefKind, list_refs, locate},
        p4::{P4Command, P4Stream, list_streams},
    },
};

const WORKING_TREE: &str = "Working tree";
const HEAD: &str = "Head";
const MAX_LABEL_CHARS: usize = 24;

/// A side's revision; `Git(None)` is the working tree and `Depot(None)` is head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideRev {
    Git(Option<GitRev>),
    Depot(Option<DepotRev>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootPick {
    WorkingTree,
    Index,
    Git(String),
    Head,
    Stream(String),
    Typed(String),
}

/// The folder of a side's root and its revision.
pub fn split_side(root: &Path) -> (PathBuf, SideRev) {
    match UniversalPath::from(root) {
        UniversalPath::Git(folder, rev) => (folder, SideRev::Git(Some(rev))),
        UniversalPath::Depot(folder, rev) => (PathBuf::from(folder), SideRev::Depot(rev)),
        UniversalPath::Local(_) => (root.to_path_buf(), SideRev::Git(None)),
    }
}

/// Same folder at another revision of the same kind as `rev`.
pub fn with_revision(root: &Path, rev: SideRev) -> PathBuf {
    let (folder, _) = split_side(root);
    match rev {
        SideRev::Git(None) => folder,
        SideRev::Git(Some(rev)) => PathBuf::from(UniversalPath::Git(folder, rev).to_string()),
        SideRev::Depot(rev) => {
            let folder = folder.to_string_lossy().into_owned();
            PathBuf::from(UniversalPath::Depot(folder, rev).to_string())
        }
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

/// A folder has no `#rev`, so only `@change` and `@=shelf` (a bare number is a change) are accepted.
fn parse_depot_rev(text: &str) -> Result<DepotRev, String> {
    let text = text.trim();
    let number = |n: &str| {
        n.parse::<u32>()
            .map_err(|_| format!("Expected @change or @=shelf, not {text:?}"))
    };
    if let Some(n) = text.strip_prefix("@=") {
        number(n).map(DepotRev::Shelf)
    } else if let Some(n) = text.strip_prefix('@') {
        number(n).map(DepotRev::Change)
    } else {
        number(text).map(DepotRev::Change)
    }
}

fn stream_of<'a>(folder: &str, streams: &'a [P4Stream]) -> Option<&'a P4Stream> {
    streams
        .iter()
        .filter(|s| {
            folder
                .strip_prefix(s.stream.trim_end_matches('/'))
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
        .max_by_key(|s| s.stream.len())
}

/// The root after `pick`, or why the pick does not apply to this kind of root.
pub fn pick_revision(
    root: &Path,
    pick: &RootPick,
    streams: &[P4Stream],
) -> Result<PathBuf, String> {
    let (folder, current) = split_side(root);
    let rev = match (&current, pick) {
        (SideRev::Git(_), RootPick::WorkingTree) => SideRev::Git(None),
        (SideRev::Git(_), RootPick::Index) => SideRev::Git(Some(GitRev::Index)),
        (SideRev::Git(_), RootPick::Git(rev)) => SideRev::Git(Some(GitRev::Rev(rev.clone()))),
        (SideRev::Git(_), RootPick::Typed(text)) => SideRev::Git(parse_revision(text)),
        (SideRev::Git(_), RootPick::Head | RootPick::Stream(_)) => {
            return Err("A git folder has no streams or head revision".to_owned());
        }
        (SideRev::Depot(_), RootPick::Head) => SideRev::Depot(None),
        (SideRev::Depot(_), RootPick::Typed(text)) => SideRev::Depot(Some(parse_depot_rev(text)?)),
        (SideRev::Depot(rev), RootPick::Stream(target)) => {
            let folder = folder.to_string_lossy();
            let stream = stream_of(&folder, streams)
                .ok_or_else(|| format!("{folder} is in none of the listed streams"))?;
            let rest = &folder[stream.stream.trim_end_matches('/').len()..];
            let rev = rev.filter(|rev| matches!(rev, DepotRev::Change(_)));
            return Ok(PathBuf::from(
                UniversalPath::Depot(format!("{target}{rest}"), rev).to_string(),
            ));
        }
        (SideRev::Depot(_), RootPick::WorkingTree | RootPick::Index | RootPick::Git(_)) => {
            return Err("A depot folder has no git revisions".to_owned());
        }
    };
    Ok(with_revision(root, rev))
}

fn revision_text(rev: &SideRev) -> String {
    match rev {
        SideRev::Git(None) => WORKING_TREE.to_owned(),
        SideRev::Git(Some(GitRev::Index)) => "Index".to_owned(),
        SideRev::Git(Some(GitRev::Rev(rev))) => rev.clone(),
        SideRev::Depot(None) => HEAD.to_owned(),
        SideRev::Depot(Some(rev)) => rev.to_string(),
    }
}

#[derive(Clone)]
enum Listing {
    Git(Vec<GitRef>),
    Streams(Vec<P4Stream>),
}

#[derive(Clone)]
struct MenuState {
    folder: PathBuf,
    text: String,
    error: Option<String>,
    listing: Result<Listing, String>,
}

impl MenuState {
    fn load(folder: PathBuf, depot: bool) -> Self {
        let listing = if depot {
            let depot_folder = UniversalPath::Depot(folder.to_string_lossy().into_owned(), None);
            list_streams(&P4Command::new(false).for_file(depot_folder)).map(Listing::Streams)
        } else {
            let git = GitCommand::default();
            locate(&git, &folder)
                .and_then(|location| list_refs(&git, &location.root))
                .map(Listing::Git)
                .map_err(|e| e.to_string())
        };
        Self {
            folder,
            text: String::new(),
            error: None,
            listing,
        }
    }
}

/// Some(root) when a revision was picked or typed. Refs and streams are listed synchronously when
/// the menu opens.
pub fn revision_dropdown(
    ui: &mut egui::Ui,
    id_salt: usize,
    root: Option<&Path>,
) -> Option<PathBuf> {
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
            .unwrap_or_else(|| {
                MenuState::load(folder.clone(), matches!(current, SideRev::Depot(_)))
            });
        picked = menu_contents(ui, &mut state, root, &current);
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
    root: &Path,
    current: &SideRev,
) -> Option<PathBuf> {
    let depot = matches!(current, SideRev::Depot(_));
    let mut pick = None;
    let edit = ui.add(
        egui::TextEdit::singleline(&mut state.text)
            .hint_text(if depot {
                "@change or @=shelf"
            } else {
                "Commit or expression"
            })
            .desired_width(220.0),
    );
    let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if entered && !state.text.trim().is_empty() {
        pick = Some(RootPick::Typed(state.text.clone()));
    }
    if depot {
        if ui
            .selectable_label(*current == SideRev::Depot(None), HEAD)
            .clicked()
        {
            pick = Some(RootPick::Head);
        }
    } else {
        if ui
            .selectable_label(*current == SideRev::Git(None), WORKING_TREE)
            .clicked()
        {
            pick = Some(RootPick::WorkingTree);
        }
        if ui
            .selectable_label(*current == SideRev::Git(Some(GitRev::Index)), "Index")
            .clicked()
        {
            pick = Some(RootPick::Index);
        }
    }
    let mut streams: &[P4Stream] = &[];
    match &state.listing {
        Err(e) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        Ok(Listing::Git(refs)) => {
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
                            let selected = matches!(
                                current,
                                SideRev::Git(Some(GitRev::Rev(name))) if *name == r.name
                            );
                            if ui.selectable_label(selected, &r.name).clicked() {
                                pick = Some(RootPick::Git(r.name.clone()));
                            }
                        }
                    }
                });
        }
        Ok(Listing::Streams(all)) => {
            streams = all;
            let current_stream = stream_of(&state.folder.to_string_lossy(), all);
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    ui.weak("Streams");
                    for s in all {
                        let selected = current_stream.is_some_and(|c| c.stream == s.stream);
                        if ui.selectable_label(selected, &s.stream).clicked() {
                            pick = Some(RootPick::Stream(s.stream.clone()));
                        }
                    }
                });
        }
    }
    let result = pick.map(|pick| pick_revision(root, &pick, streams));
    match result {
        Some(Ok(root)) => Some(root),
        Some(Err(e)) => {
            state.error = Some(e);
            None
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rev(name: &str) -> SideRev {
        SideRev::Git(Some(GitRev::Rev(name.to_owned())))
    }

    fn stream(path: &str) -> P4Stream {
        P4Stream {
            stream: path.to_owned(),
            name: path.rsplit('/').next().unwrap().to_owned(),
            kind: "development".to_owned(),
            parent: None,
        }
    }

    fn streams() -> Vec<P4Stream> {
        vec![
            stream("//stream/main"),
            stream("//stream/main/rel"),
            stream("//stream/dev"),
            stream("//stream/main2"),
        ]
    }

    fn pick(root: &str, pick: RootPick) -> Result<String, String> {
        pick_revision(Path::new(root), &pick, &streams()).map(|p| p.to_string_lossy().into_owned())
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
            with_revision(
                Path::new(r"D:\repo\src@git:v1.2"),
                SideRev::Git(Some(GitRev::Index))
            ),
            PathBuf::from(r"D:\repo\src@git:index")
        );
    }

    #[test]
    fn a_revision_to_the_working_tree_keeps_the_sub_path() {
        assert_eq!(
            with_revision(
                Path::new(r"D:\repo\src@git:origin/main"),
                SideRev::Git(None)
            ),
            PathBuf::from(r"D:\repo\src")
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
            (
                PathBuf::from(r"D:\repo\src"),
                SideRev::Git(Some(GitRev::Index))
            )
        );
        assert_eq!(
            split_side(Path::new(r"D:\repo\src")),
            (PathBuf::from(r"D:\repo\src"), SideRev::Git(None))
        );
    }

    #[test]
    fn a_depot_root_splits_into_a_depot_side() {
        assert_eq!(
            split_side(Path::new("//depot/main/src")),
            (PathBuf::from("//depot/main/src"), SideRev::Depot(None))
        );
        assert_eq!(
            split_side(Path::new("//depot/main/src@1234")),
            (
                PathBuf::from("//depot/main/src"),
                SideRev::Depot(Some(DepotRev::Change(1234)))
            )
        );
        assert_eq!(
            split_side(Path::new("//depot/main/src@=77")),
            (
                PathBuf::from("//depot/main/src"),
                SideRev::Depot(Some(DepotRev::Shelf(77)))
            )
        );
    }

    #[test]
    fn typed_text_is_a_revision_the_index_or_the_working_tree() {
        assert_eq!(
            parse_revision("  HEAD~3 "),
            Some(GitRev::Rev("HEAD~3".into()))
        );
        assert_eq!(parse_revision("index"), Some(GitRev::Index));
        assert_eq!(parse_revision("   "), None);
    }

    #[test]
    fn typed_numbers_on_a_depot_root_pick_changes_and_shelves() {
        let typed = |text: &str| pick("//depot/main/src", RootPick::Typed(text.to_owned()));
        assert_eq!(typed("@1234").unwrap(), "//depot/main/src@1234");
        assert_eq!(typed("@=77").unwrap(), "//depot/main/src@=77");
        assert_eq!(typed(" 1234 ").unwrap(), "//depot/main/src@1234");
        assert!(typed("main").is_err());
        assert!(typed("@").is_err());
        assert!(typed("#3").is_err());
    }

    #[test]
    fn head_drops_the_depot_revision() {
        assert_eq!(
            pick("//depot/main/src@1234", RootPick::Head).unwrap(),
            "//depot/main/src"
        );
    }

    #[test]
    fn a_stream_pick_swaps_the_stream_prefix_and_keeps_a_change() {
        assert_eq!(
            pick(
                "//stream/main/src/gui@1234",
                RootPick::Stream("//stream/dev".into())
            )
            .unwrap(),
            "//stream/dev/src/gui@1234"
        );
        assert_eq!(
            pick(
                "//stream/main/rel/src",
                RootPick::Stream("//stream/dev".into())
            )
            .unwrap(),
            "//stream/dev/src"
        );
    }

    #[test]
    fn a_stream_pick_drops_a_shelf() {
        assert_eq!(
            pick(
                "//stream/main/src@=77",
                RootPick::Stream("//stream/dev".into())
            )
            .unwrap(),
            "//stream/dev/src"
        );
    }

    #[test]
    fn a_stream_pick_on_an_unlisted_stream_reports_why() {
        assert!(pick("//other/x", RootPick::Stream("//stream/dev".into())).is_err());
    }

    #[test]
    fn git_picks_on_a_depot_root_report_why() {
        for p in [
            RootPick::WorkingTree,
            RootPick::Index,
            RootPick::Git("main".into()),
        ] {
            assert!(pick("//depot/main/src", p).is_err());
        }
    }

    #[test]
    fn depot_picks_on_a_git_root_report_why() {
        for p in [RootPick::Head, RootPick::Stream("//stream/dev".into())] {
            assert!(pick(r"D:\repo\src", p).is_err());
        }
    }

    #[test]
    fn git_picks_on_a_git_root_keep_the_sub_path() {
        assert_eq!(
            pick(r"D:\repo\src@git:main", RootPick::Git("origin/dev".into())).unwrap(),
            r"D:\repo\src@git:origin/dev"
        );
        assert_eq!(
            pick(r"D:\repo\src@git:main", RootPick::WorkingTree).unwrap(),
            r"D:\repo\src"
        );
        assert_eq!(
            pick(r"D:\repo\src", RootPick::Typed("HEAD~3".into())).unwrap(),
            r"D:\repo\src@git:HEAD~3"
        );
    }
}
