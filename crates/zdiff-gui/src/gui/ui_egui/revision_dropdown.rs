use std::sync::{Arc, Mutex};

use eframe::egui::{
    self, PopupCloseBehavior,
    containers::menu::{MenuButton, MenuConfig},
};
use zcommon::vcs::git::{self, GitCommand, GitRef, RefKind};
use zdiff::universal_path::{DepotRev, GitRev, UniversalPath};

use crate::p4::{P4Command, P4Stream, list_streams};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionPick {
    WorkingTree,
    Index,
    Git(String),
    Head,
    Stream(String),
    Typed(String),
}

pub fn pick_revision(
    path: &UniversalPath,
    pick: &RevisionPick,
    streams: &[P4Stream],
) -> Result<UniversalPath, String> {
    match (path, pick) {
        (UniversalPath::Local(p) | UniversalPath::Git(p, _), pick) => {
            let rev = match pick {
                RevisionPick::WorkingTree => return Ok(UniversalPath::Local(p.clone())),
                RevisionPick::Index => GitRev::Index,
                RevisionPick::Git(rev) => GitRev::Rev(rev.clone()),
                RevisionPick::Typed(text) => match text.trim() {
                    "" => return Err("Enter a branch, tag, commit or expression".to_owned()),
                    "index" => GitRev::Index,
                    rev => GitRev::Rev(rev.to_owned()),
                },
                RevisionPick::Head | RevisionPick::Stream(_) => {
                    return Err("A local file has no streams or head revision".to_owned());
                }
            };
            Ok(UniversalPath::Git(p.clone(), rev))
        }
        (UniversalPath::Depot(file, rev), pick) => {
            let rev = match pick {
                RevisionPick::Head => None,
                RevisionPick::Typed(text) => Some(parse_depot_rev(text)?),
                RevisionPick::Stream(target) => {
                    let current = stream_of(file, streams)
                        .ok_or_else(|| format!("{file} is in none of the listed streams"))?;
                    let rest = &file[trimmed_stream(current).len()..];
                    let rev = rev.filter(|rev| matches!(rev, DepotRev::Change(_)));
                    return Ok(UniversalPath::Depot(format!("{target}{rest}"), rev));
                }
                RevisionPick::WorkingTree | RevisionPick::Index | RevisionPick::Git(_) => {
                    return Err("A depot file has no git revisions".to_owned());
                }
            };
            Ok(UniversalPath::Depot(file.clone(), rev))
        }
    }
}

fn parse_depot_rev(text: &str) -> Result<DepotRev, String> {
    let text = text.trim();
    let number = |n: &str| {
        n.parse::<u32>()
            .map_err(|_| format!("Expected @change, @=shelf or #rev, not {text:?}"))
    };
    if let Some(n) = text.strip_prefix("@=") {
        number(n).map(DepotRev::Shelf)
    } else if let Some(n) = text.strip_prefix('@') {
        number(n).map(DepotRev::Change)
    } else if let Some(n) = text.strip_prefix('#') {
        number(n).map(DepotRev::Rev)
    } else {
        number(text).map(DepotRev::Change)
    }
}

fn trimmed_stream(s: &P4Stream) -> &str {
    s.stream.trim_end_matches('/')
}

fn stream_of<'a>(file: &str, streams: &'a [P4Stream]) -> Option<&'a P4Stream> {
    streams
        .iter()
        .filter(|s| {
            file.strip_prefix(trimmed_stream(s))
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
        .max_by_key(|s| trimmed_stream(s).len())
}

pub fn revision_label(path: &UniversalPath) -> String {
    match path {
        UniversalPath::Local(_) => "Working tree".to_owned(),
        UniversalPath::Git(_, GitRev::Index) => "Index".to_owned(),
        UniversalPath::Git(_, rev) => rev.to_string(),
        UniversalPath::Depot(_, None) => "Head".to_owned(),
        UniversalPath::Depot(_, Some(rev)) => rev.to_string(),
    }
}

enum Listing {
    Git(Vec<GitRef>),
    Streams(Vec<P4Stream>),
}

type Loading = Arc<Mutex<Option<Result<Listing, String>>>>;

#[derive(Clone)]
struct State {
    text: String,
    error: Option<String>,
    listing: Loading,
}

fn load_listing(path: &UniversalPath) -> Result<Listing, String> {
    match path {
        UniversalPath::Depot(..) => {
            let p4 = P4Command::new(false).for_file(path.clone());
            list_streams(&p4).map(Listing::Streams)
        }
        UniversalPath::Local(p) | UniversalPath::Git(p, _) => {
            let git_cmd = GitCommand::default();
            let location = git::locate(&git_cmd, p).map_err(|e| e.to_string())?;
            git::list_refs(&git_cmd, &location.root)
                .map(Listing::Git)
                .map_err(|e| e.to_string())
        }
    }
}

fn start(ctx: &egui::Context, path: &UniversalPath) -> State {
    let listing: Loading = Arc::default();
    let (shared, path, ctx) = (listing.clone(), path.clone(), ctx.clone());
    std::thread::spawn(move || {
        let result = load_listing(&path);
        *shared.lock().unwrap() = Some(result);
        ctx.request_repaint();
    });
    State {
        text: String::new(),
        error: None,
        listing,
    }
}

/// A button showing the side's revision; its menu lists what the path can switch to. The
/// returned path is the side's new path, with the file path kept.
pub fn revision_dropdown(
    ui: &mut egui::Ui,
    id_salt: &str,
    path: &UniversalPath,
) -> Option<UniversalPath> {
    let id = ui.make_persistent_id((id_salt, "revision_dropdown"));
    let button = egui::Button::new(format!("{} \u{23f7}", revision_label(path))).truncate();
    let (response, inner) = MenuButton::from_button(button)
        .config(MenuConfig::new().close_behavior(PopupCloseBehavior::CloseOnClickOutside))
        .ui(ui, |ui| menu(ui, id, path));
    response.on_hover_text("Revision");
    let Some(inner) = inner else {
        ui.data_mut(|d| d.remove::<State>(id));
        return None;
    };
    inner.inner.filter(|picked| picked != path)
}

fn menu(ui: &mut egui::Ui, id: egui::Id, path: &UniversalPath) -> Option<UniversalPath> {
    let fresh = ui.data(|d| d.get_temp::<State>(id)).is_none();
    let mut state = ui
        .data(|d| d.get_temp::<State>(id))
        .unwrap_or_else(|| start(ui.ctx(), path));
    ui.set_min_width(260.0);

    let is_depot = path.is_depot();
    let hint = if is_depot {
        "Filter, or @change, @=shelf, #rev"
    } else {
        "Filter, or any commit or expression"
    };
    let edit = ui.add(
        egui::TextEdit::singleline(&mut state.text)
            .hint_text(hint)
            .desired_width(f32::INFINITY),
    );
    if fresh {
        edit.request_focus();
    }
    let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

    let listing = state.listing.lock().unwrap();
    let streams: &[P4Stream] = match listing.as_ref() {
        Some(Ok(Listing::Streams(streams))) => streams,
        _ => &[],
    };
    let mut pick = entered.then(|| RevisionPick::Typed(state.text.clone()));
    let filter = state.text.trim().to_lowercase();
    let shown = |name: &str| filter.is_empty() || name.to_lowercase().contains(&filter);
    let mut item = |ui: &mut egui::Ui, selected: bool, name: &str, p: RevisionPick| {
        if ui.selectable_label(selected, name).clicked() {
            pick = Some(p);
        }
    };

    ui.separator();
    if is_depot {
        item(
            ui,
            matches!(path, UniversalPath::Depot(_, None)),
            "Head",
            RevisionPick::Head,
        );
    } else {
        item(
            ui,
            matches!(path, UniversalPath::Local(_)),
            "Working tree",
            RevisionPick::WorkingTree,
        );
        item(
            ui,
            matches!(path, UniversalPath::Git(_, GitRev::Index)),
            "Index",
            RevisionPick::Index,
        );
    }
    ui.separator();
    match listing.as_ref() {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading");
            });
        }
        Some(Err(e)) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        Some(Ok(Listing::Git(refs))) => {
            let current = match path {
                UniversalPath::Git(_, GitRev::Rev(rev)) => Some(rev.as_str()),
                _ => None,
            };
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    for (kind, title) in [
                        (RefKind::Branch, "Branches"),
                        (RefKind::RemoteBranch, "Remote branches"),
                        (RefKind::Tag, "Tags"),
                    ] {
                        let names: Vec<&str> = refs
                            .iter()
                            .filter(|r| r.kind == kind && shown(&r.name))
                            .map(|r| r.name.as_str())
                            .collect();
                        if names.is_empty() {
                            continue;
                        }
                        ui.weak(title);
                        for name in names {
                            let selected = current == Some(name);
                            item(ui, selected, name, RevisionPick::Git(name.to_owned()));
                        }
                    }
                });
        }
        Some(Ok(Listing::Streams(all))) => {
            let current = match path {
                UniversalPath::Depot(file, _) => stream_of(file, all).map(|s| s.stream.as_str()),
                _ => None,
            };
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    ui.weak("Streams");
                    for s in all.iter().filter(|s| shown(&s.stream)) {
                        let selected = current == Some(s.stream.as_str());
                        item(
                            ui,
                            selected,
                            &s.stream,
                            RevisionPick::Stream(s.stream.clone()),
                        );
                    }
                });
        }
    }

    if let Some(error) = &state.error {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }

    let picked = pick.map(|pick| pick_revision(path, &pick, streams));
    drop(listing);
    let result = match picked {
        Some(Ok(new_path)) => {
            ui.close();
            Some(new_path)
        }
        Some(Err(e)) => {
            state.error = Some(e);
            None
        }
        None => None,
    };
    ui.data_mut(|d| d.insert_temp(id, state));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(path: &str, pick: RevisionPick) -> Result<String, String> {
        pick_revision(&UniversalPath::from(path), &pick, &streams()).map(|p| p.to_string())
    }

    fn typed(path: &str, text: &str) -> Result<String, String> {
        pick(path, RevisionPick::Typed(text.to_owned()))
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

    #[test]
    fn a_git_revision_is_appended_and_the_file_path_kept() {
        assert_eq!(
            pick(r"C:\ws\src\a.rs", RevisionPick::Git("main".into())).unwrap(),
            r"C:\ws\src\a.rs@git:main"
        );
    }

    #[test]
    fn a_git_revision_replaces_the_current_one() {
        assert_eq!(
            pick(
                r"C:\ws\src\a.rs@git:main",
                RevisionPick::Git("origin/dev".into())
            )
            .unwrap(),
            r"C:\ws\src\a.rs@git:origin/dev"
        );
    }

    #[test]
    fn working_tree_drops_the_revision() {
        assert_eq!(
            pick(r"C:\ws\src\a.rs@git:main", RevisionPick::WorkingTree).unwrap(),
            r"C:\ws\src\a.rs"
        );
        assert_eq!(
            pick(r"C:\ws\src\a.rs", RevisionPick::WorkingTree).unwrap(),
            r"C:\ws\src\a.rs"
        );
    }

    #[test]
    fn index_keeps_the_file_path() {
        assert_eq!(
            pick(r"C:\ws\src\a.rs@git:main", RevisionPick::Index).unwrap(),
            r"C:\ws\src\a.rs@git:index"
        );
    }

    #[test]
    fn typed_text_on_a_local_path_is_a_git_revision() {
        assert_eq!(
            typed(r"C:\ws\a.rs", "HEAD~3").unwrap(),
            r"C:\ws\a.rs@git:HEAD~3"
        );
        assert_eq!(
            typed(r"C:\ws\a.rs", "  v1.2 ").unwrap(),
            r"C:\ws\a.rs@git:v1.2"
        );
        assert_eq!(
            typed(r"C:\ws\a.rs@git:main", "index").unwrap(),
            r"C:\ws\a.rs@git:index"
        );
        assert!(typed(r"C:\ws\a.rs", "  ").is_err());
    }

    #[test]
    fn typed_numbers_on_a_depot_path_pick_changelists_shelves_and_revisions() {
        assert_eq!(
            typed("//depot/a.txt", "@1234").unwrap(),
            "//depot/a.txt@1234"
        );
        assert_eq!(
            typed("//depot/a.txt#3", "@=77").unwrap(),
            "//depot/a.txt@=77"
        );
        assert_eq!(typed("//depot/a.txt@5", "#3").unwrap(), "//depot/a.txt#3");
        assert_eq!(
            typed("//depot/a.txt", "1234").unwrap(),
            "//depot/a.txt@1234"
        );
        assert!(typed("//depot/a.txt", "main").is_err());
        assert!(typed("//depot/a.txt", "@").is_err());
        assert!(typed("//depot/a.txt", "@=x").is_err());
    }

    #[test]
    fn head_drops_the_depot_revision() {
        assert_eq!(
            pick("//depot/a.txt#3", RevisionPick::Head).unwrap(),
            "//depot/a.txt"
        );
    }

    #[test]
    fn a_stream_switch_keeps_the_rest_of_the_path() {
        assert_eq!(
            pick(
                "//stream/main/src/a.txt",
                RevisionPick::Stream("//stream/dev".into())
            )
            .unwrap(),
            "//stream/dev/src/a.txt"
        );
    }

    #[test]
    fn a_stream_switch_uses_the_longest_matching_stream() {
        assert_eq!(
            pick(
                "//stream/main/rel/a.txt",
                RevisionPick::Stream("//stream/dev".into())
            )
            .unwrap(),
            "//stream/dev/a.txt"
        );
    }

    #[test]
    fn a_stream_switch_matches_whole_folders_only() {
        assert_eq!(
            pick(
                "//stream/main2/a.txt",
                RevisionPick::Stream("//stream/dev".into())
            )
            .unwrap(),
            "//stream/dev/a.txt"
        );
    }

    #[test]
    fn a_stream_switch_keeps_a_changelist_and_drops_a_file_revision_or_shelf() {
        let to_dev = || RevisionPick::Stream("//stream/dev".into());
        assert_eq!(
            pick("//stream/main/a.txt@1234", to_dev()).unwrap(),
            "//stream/dev/a.txt@1234"
        );
        assert_eq!(
            pick("//stream/main/a.txt#3", to_dev()).unwrap(),
            "//stream/dev/a.txt"
        );
        assert_eq!(
            pick("//stream/main/a.txt@=77", to_dev()).unwrap(),
            "//stream/dev/a.txt"
        );
    }

    fn pick_in(path: &str, listed: &[&str], to: &str) -> Result<String, String> {
        let listed: Vec<P4Stream> = listed.iter().map(|s| stream(s)).collect();
        pick_revision(
            &UniversalPath::from(path),
            &RevisionPick::Stream(to.into()),
            &listed,
        )
        .map(|p| p.to_string())
    }

    #[test]
    fn a_stream_listed_with_a_trailing_slash_keeps_the_separator_when_switching() {
        assert_eq!(
            pick_in(
                "//stream/main/src/a.txt",
                &["//stream/main/"],
                "//stream/dev"
            )
            .unwrap(),
            "//stream/dev/src/a.txt"
        );
    }

    #[test]
    fn a_stream_switch_with_multibyte_characters_after_the_stream_does_not_panic() {
        assert_eq!(
            pick_in(
                "//stream/main/é/ü.txt",
                &["//stream/main//"],
                "//stream/dev"
            )
            .unwrap(),
            "//stream/dev/é/ü.txt"
        );
        assert_eq!(
            pick_in("//stream/main", &["//stream/main/"], "//stream/dev").unwrap(),
            "//stream/dev"
        );
    }

    #[test]
    fn a_path_outside_every_stream_cannot_switch_stream() {
        assert!(pick("//depot/a.txt", RevisionPick::Stream("//stream/dev".into())).is_err());
    }

    #[test]
    fn picks_for_the_other_kind_of_path_are_refused() {
        assert!(pick("//depot/a.txt", RevisionPick::Git("main".into())).is_err());
        assert!(pick("//depot/a.txt", RevisionPick::Index).is_err());
        assert!(pick("//depot/a.txt", RevisionPick::WorkingTree).is_err());
        assert!(pick(r"C:\ws\a.rs", RevisionPick::Stream("//stream/dev".into())).is_err());
        assert!(pick(r"C:\ws\a.rs", RevisionPick::Head).is_err());
    }

    #[test]
    fn the_label_names_the_current_revision() {
        let label = |p: &str| revision_label(&UniversalPath::from(p));
        assert_eq!(label(r"C:\ws\a.rs"), "Working tree");
        assert_eq!(label(r"C:\ws\a.rs@git:main"), "main");
        assert_eq!(label(r"C:\ws\a.rs@git:index"), "Index");
        assert_eq!(label("//depot/a.txt"), "Head");
        assert_eq!(label("//depot/a.txt#3"), "#3");
        assert_eq!(label("//depot/a.txt@1234"), "@1234");
        assert_eq!(label("//depot/a.txt@=77"), "@=77");
    }
}
