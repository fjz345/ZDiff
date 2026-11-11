use std::sync::{Arc, Mutex};

use eframe::egui::{
    self, PopupCloseBehavior,
    containers::menu::{MenuButton, MenuConfig},
};
use zcommon::{
    job::Job,
    universal_path::{GitRev, UniversalPath},
    vcs::{
        git::RefKind,
        p4::P4Stream,
        revision::{Listing, PathKind, RevisionPick, load_listing, pick_revision, stream_of},
    },
};

pub const SOURCE_CONTROL: &str = "Source control";
const MAX_FOLDER_LABEL_CHARS: usize = 24;

struct Look {
    local: &'static str,
    git_hint: &'static str,
    depot_hint: &'static str,
    filters: bool,
    menu_min_width: Option<f32>,
    edit_width: f32,
    focus_on_open: bool,
    separators: bool,
    ignores_empty_entry: bool,
}

const FILE_LOOK: Look = Look {
    local: "Local file",
    git_hint: "Filter, or any commit or expression",
    depot_hint: "Filter, or @change, @=shelf, #rev",
    filters: true,
    menu_min_width: Some(260.0),
    edit_width: f32::INFINITY,
    focus_on_open: true,
    separators: true,
    ignores_empty_entry: false,
};

const FOLDER_LOOK: Look = Look {
    local: "Local folder",
    git_hint: "Commit or expression",
    depot_hint: "@change or @=shelf",
    filters: false,
    menu_min_width: None,
    edit_width: 220.0,
    focus_on_open: false,
    separators: false,
    ignores_empty_entry: true,
};

fn look(kind: PathKind) -> &'static Look {
    match kind {
        PathKind::File => &FILE_LOOK,
        PathKind::Folder => &FOLDER_LOOK,
    }
}

fn revision_label(path: &UniversalPath, local: &str) -> String {
    match path {
        UniversalPath::Local(_) => local.to_owned(),
        UniversalPath::Git(_, GitRev::Index) => "Index".to_owned(),
        UniversalPath::Git(_, rev) => rev.to_string(),
        UniversalPath::Depot(_, None) => "Head".to_owned(),
        UniversalPath::Depot(_, Some(rev)) => rev.to_string(),
    }
}

fn back_text(previous: &UniversalPath, look: &Look) -> String {
    match previous {
        UniversalPath::Depot(..) => previous.to_string(),
        _ => revision_label(previous, look.local),
    }
}

fn truncated(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }
    let head: String = text.chars().take(max_chars - 1).collect();
    format!("{head}…")
}

type LoadResult = Result<Listing, String>;

#[derive(Clone)]
struct State {
    path: UniversalPath,
    text: String,
    error: Option<String>,
    job: Arc<Mutex<Job<LoadResult>>>,
    listing: Option<Arc<LoadResult>>,
}

fn start(ctx: &egui::Context, path: &UniversalPath, kind: PathKind) -> State {
    let ctx = ctx.clone();
    let mut job = Job::new(move || ctx.request_repaint());
    let job_path = path.clone();
    job.start(move |job_ctx| {
        job_ctx.send(load_listing(&job_path, kind));
    });
    State {
        path: path.clone(),
        text: String::new(),
        error: None,
        job: Arc::new(Mutex::new(job)),
        listing: None,
    }
}

pub fn revision_dropdown(
    ui: &mut egui::Ui,
    id: egui::Id,
    kind: PathKind,
    path: &UniversalPath,
    previous: Option<&UniversalPath>,
) -> Option<UniversalPath> {
    let look = look(kind);
    let body = |ui: &mut egui::Ui| {
        if let Some(previous) = previous {
            let back = ui
                .selectable_label(false, format!("Back to {}", back_text(previous, look)))
                .on_hover_text(previous.to_string());
            ui.separator();
            if back.clicked() {
                ui.close();
                return Some(previous.clone());
            }
        }
        menu(ui, id, kind, path)
    };
    let label = revision_label(path, SOURCE_CONTROL);
    let picked = match kind {
        PathKind::File => {
            let button = egui::Button::new(format!("{label} \u{23f7}")).truncate();
            let (response, inner) = MenuButton::from_button(button)
                .config(MenuConfig::new().close_behavior(PopupCloseBehavior::CloseOnClickOutside))
                .ui(ui, body);
            response.on_hover_text("Source control revision");
            inner.map(|inner| inner.inner)
        }
        PathKind::Folder => {
            let menu = ui.menu_button(truncated(label, MAX_FOLDER_LABEL_CHARS), body);
            let text = revision_label(path, look.local);
            menu.response.on_hover_text(format!("Revision: {text}"));
            menu.inner
        }
    };
    if picked.is_none() {
        ui.data_mut(|d| d.remove::<State>(id));
    }
    picked.flatten()
}

fn menu(
    ui: &mut egui::Ui,
    id: egui::Id,
    kind: PathKind,
    path: &UniversalPath,
) -> Option<UniversalPath> {
    let look = look(kind);
    let saved = ui
        .data(|d| d.get_temp::<State>(id))
        .filter(|state| state.path == *path);
    let fresh = saved.is_none();
    let mut state = saved.unwrap_or_else(|| start(ui.ctx(), path, kind));
    if let Some(width) = look.menu_min_width {
        ui.set_min_width(width);
    }

    if let Some(result) = state.job.lock().unwrap().poll().pop() {
        state.listing = Some(Arc::new(result));
    }
    let listing = state.listing.clone();
    let listing = listing.as_deref();
    let (streams, workspace_depot): (&[P4Stream], Option<&str>) = match listing {
        Some(Ok(Listing::Streams(streams))) => (streams, None),
        Some(Ok(Listing::Workspace { depot, streams })) => (streams, Some(depot)),
        _ => (&[], None),
    };
    let unversioned = matches!(listing, Some(Ok(Listing::Unversioned { .. })));
    let is_depot = path.is_depot() || workspace_depot.is_some();
    let entered = if unversioned {
        false
    } else {
        let edit = ui.add(
            egui::TextEdit::singleline(&mut state.text)
                .hint_text(if is_depot {
                    look.depot_hint
                } else {
                    look.git_hint
                })
                .desired_width(look.edit_width),
        );
        if fresh && look.focus_on_open {
            edit.request_focus();
        }
        if look.separators {
            ui.separator();
        }
        edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
    };

    let ignored = look.ignores_empty_entry && state.text.trim().is_empty();
    // Before the listing arrives, a workspace path's typed text would be read as a git revision.
    let mut pick =
        (entered && listing.is_some() && !ignored).then(|| RevisionPick::Typed(state.text.clone()));
    let filter = if look.filters {
        state.text.trim().to_lowercase()
    } else {
        String::new()
    };
    let shown = |name: &str| filter.is_empty() || name.to_lowercase().contains(&filter);
    let mut item = |ui: &mut egui::Ui, selected: bool, name: &str, p: RevisionPick| {
        if ui.selectable_label(selected, name).clicked() {
            pick = Some(p);
        }
    };

    if !path.is_depot() {
        item(
            ui,
            matches!(path, UniversalPath::Local(_)),
            look.local,
            RevisionPick::WorkingTree,
        );
    }
    if is_depot {
        item(
            ui,
            matches!(path, UniversalPath::Depot(_, None)),
            "Head",
            RevisionPick::Head,
        );
    } else if !unversioned {
        item(
            ui,
            matches!(path, UniversalPath::Git(_, GitRev::Index)),
            "Index",
            RevisionPick::Index,
        );
    }
    if look.separators {
        ui.separator();
    }
    match listing {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading");
            });
        }
        Some(Err(e)) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        Some(Ok(Listing::Unversioned { detail })) => {
            ui.weak("Not under source control").on_hover_text(detail);
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
        Some(Ok(Listing::Streams(all) | Listing::Workspace { streams: all, .. })) => {
            let depot_path = match path {
                UniversalPath::Depot(depot_path, _) => Some(depot_path.as_str()),
                _ => workspace_depot,
            };
            let current = depot_path
                .and_then(|depot_path| stream_of(depot_path, all))
                .map(|s| s.stream.as_str());
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

    let picked = pick.map(|pick| pick_revision(path, kind, &pick, streams, workspace_depot));
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
    if let Some(error) = &state.error {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
    ui.data_mut(|d| d.insert_temp(id, state));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_names_the_current_revision() {
        let label = |p: &str| revision_label(&UniversalPath::from(p), SOURCE_CONTROL);
        assert_eq!(label(r"C:\ws\a.rs"), "Source control");
        assert_eq!(label(r"C:\ws\a.rs@git:main"), "main");
        assert_eq!(label(r"C:\ws\a.rs@git:index"), "Index");
        assert_eq!(label("//depot/a.txt"), "Head");
        assert_eq!(label("//depot/a.txt#3"), "#3");
        assert_eq!(label("//depot/a.txt@1234"), "@1234");
        assert_eq!(label("//depot/a.txt@=77"), "@=77");
    }
}
