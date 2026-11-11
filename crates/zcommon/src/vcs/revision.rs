use std::path::Path;
use std::time::Duration;

use crate::universal_path::{DepotRev, GitRev, UniversalPath};
use crate::vcs::git::{self, GitCommand, GitRef};
use crate::vcs::p4::{
    P4Command, P4Stream, depot_file_of_local, depot_folder_of_local, list_streams,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    File,
    Folder,
}

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
    kind: PathKind,
    pick: &RevisionPick,
    streams: &[P4Stream],
    workspace_depot: Option<&str>,
) -> Result<UniversalPath, String> {
    if let (UniversalPath::Local(_), Some(depot)) = (path, workspace_depot)
        && *pick != RevisionPick::WorkingTree
    {
        let depot = UniversalPath::Depot(depot.to_owned(), None);
        return pick_revision(&depot, kind, pick, streams, None);
    }
    let (local, depot) = match kind {
        PathKind::File => ("A local file", "A depot file"),
        PathKind::Folder => ("A git folder", "A depot folder"),
    };
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
                    return Err(format!("{local} has no streams or head revision"));
                }
            };
            Ok(UniversalPath::Git(p.clone(), rev))
        }
        (UniversalPath::Depot(file, rev), pick) => {
            let rev = match pick {
                RevisionPick::Head => None,
                RevisionPick::Typed(text) => Some(parse_depot_rev(text, kind)?),
                RevisionPick::Stream(target) => {
                    let current = stream_of(file, streams)
                        .ok_or_else(|| format!("{file} is in none of the listed streams"))?;
                    let rest = &file[trimmed_stream(current).len()..];
                    let rev = rev.filter(|rev| matches!(rev, DepotRev::Change(_)));
                    return Ok(UniversalPath::Depot(format!("{target}{rest}"), rev));
                }
                RevisionPick::WorkingTree | RevisionPick::Index | RevisionPick::Git(_) => {
                    return Err(format!("{depot} has no git revisions"));
                }
            };
            Ok(UniversalPath::Depot(file.clone(), rev))
        }
    }
}

fn parse_depot_rev(text: &str, kind: PathKind) -> Result<DepotRev, String> {
    let text = text.trim();
    let expected = match kind {
        PathKind::File => "@change, @=shelf or #rev",
        PathKind::Folder => "@change or @=shelf",
    };
    let number = |n: &str| {
        n.parse::<u32>()
            .map_err(|_| format!("Expected {expected}, not {text:?}"))
    };
    if let Some(n) = text.strip_prefix("@=") {
        number(n).map(DepotRev::Shelf)
    } else if let Some(n) = text.strip_prefix('@') {
        number(n).map(DepotRev::Change)
    } else if let (PathKind::File, Some(n)) = (kind, text.strip_prefix('#')) {
        number(n).map(DepotRev::Rev)
    } else {
        number(text).map(DepotRev::Change)
    }
}

fn trimmed_stream(s: &P4Stream) -> &str {
    s.stream.trim_end_matches('/')
}

pub fn stream_of<'a>(depot_path: &str, streams: &'a [P4Stream]) -> Option<&'a P4Stream> {
    streams
        .iter()
        .filter(|s| {
            depot_path
                .strip_prefix(trimmed_stream(s))
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
        .max_by_key(|s| trimmed_stream(s).len())
}

pub enum Listing {
    Git(Vec<GitRef>),
    Streams(Vec<P4Stream>),
    Workspace {
        depot: String,
        streams: Vec<P4Stream>,
    },
    Unversioned {
        detail: String,
    },
}

pub fn load_listing(path: &UniversalPath, kind: PathKind) -> Result<Listing, String> {
    match path {
        UniversalPath::Depot(..) => {
            let p4 = P4Command::new(false).for_file(path.clone());
            list_streams(&p4).map(Listing::Streams)
        }
        UniversalPath::Local(p) | UniversalPath::Git(p, _) => {
            let git_cmd = GitCommand::default();
            let location = match git::locate(&git_cmd, p) {
                Ok(location) => location,
                Err(git_err) if matches!(path, UniversalPath::Local(_)) => {
                    return Ok(load_workspace(p, kind).unwrap_or_else(|p4_err| {
                        let detail = format!("git: {git_err}\np4: {p4_err}");
                        log::debug!("{} is not under source control\n{detail}", p.display());
                        Listing::Unversioned { detail }
                    }));
                }
                Err(e) => return Err(e.to_string()),
            };
            git::list_refs(&git_cmd, &location.root)
                .map(Listing::Git)
                .map_err(|e| e.to_string())
        }
    }
}

fn load_workspace(local: &Path, kind: PathKind) -> Result<Listing, String> {
    // p4 runs in the path's parent folder, which for a folder must be the folder itself.
    let p4_path = match kind {
        PathKind::File => local.to_path_buf(),
        PathKind::Folder => local.join("..."),
    };
    let p4 = P4Command::new(false).for_file(UniversalPath::Local(p4_path));
    p4.check_server_reachable(Duration::from_secs(1))?;
    let depot = match kind {
        PathKind::File => depot_file_of_local(&p4, local)?,
        PathKind::Folder => depot_folder_of_local(&p4, local)?,
    };
    list_streams(&p4).map(|streams| Listing::Workspace { depot, streams })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn only_a_file_takes_a_depot_file_revision() {
        assert_eq!(parse_depot_rev("#3", PathKind::File), Ok(DepotRev::Rev(3)));
        assert_eq!(
            parse_depot_rev("#3", PathKind::Folder),
            Err("Expected @change or @=shelf, not \"#3\"".to_owned())
        );
        for kind in [PathKind::File, PathKind::Folder] {
            assert_eq!(parse_depot_rev(" @12 ", kind), Ok(DepotRev::Change(12)));
            assert_eq!(parse_depot_rev("@=7", kind), Ok(DepotRev::Shelf(7)));
            assert_eq!(parse_depot_rev("12", kind), Ok(DepotRev::Change(12)));
        }
    }

    mod file {
        use super::*;

        fn pick(path: &str, pick: RevisionPick) -> Result<String, String> {
            pick_revision(
                &UniversalPath::from(path),
                PathKind::File,
                &pick,
                &streams(),
                None,
            )
            .map(|p| p.to_string())
        }

        fn pick_in_workspace(path: &str, pick: RevisionPick) -> Result<String, String> {
            pick_revision(
                &UniversalPath::from(path),
                PathKind::File,
                &pick,
                &streams(),
                Some("//stream/main/src/a.rs"),
            )
            .map(|p| p.to_string())
        }

        fn typed(path: &str, text: &str) -> Result<String, String> {
            pick(path, RevisionPick::Typed(text.to_owned()))
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
                PathKind::File,
                &RevisionPick::Stream(to.into()),
                &listed,
                None,
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
        fn depot_picks_on_a_workspace_file_use_its_depot_path() {
            let local = r"C:\ws\src\a.rs";
            assert_eq!(
                pick_in_workspace(local, RevisionPick::Head).unwrap(),
                "//stream/main/src/a.rs"
            );
            assert_eq!(
                pick_in_workspace(local, RevisionPick::Typed("@1234".into())).unwrap(),
                "//stream/main/src/a.rs@1234"
            );
            assert_eq!(
                pick_in_workspace(local, RevisionPick::Stream("//stream/dev".into())).unwrap(),
                "//stream/dev/src/a.rs"
            );
        }

        #[test]
        fn working_tree_on_a_workspace_file_stays_local() {
            assert_eq!(
                pick_in_workspace(r"C:\ws\src\a.rs", RevisionPick::WorkingTree).unwrap(),
                r"C:\ws\src\a.rs"
            );
        }

        #[test]
        fn git_picks_on_a_workspace_file_are_refused() {
            let local = r"C:\ws\src\a.rs";
            assert!(pick_in_workspace(local, RevisionPick::Typed("main".into())).is_err());
            assert!(pick_in_workspace(local, RevisionPick::Index).is_err());
            assert!(pick_in_workspace(local, RevisionPick::Git("main".into())).is_err());
        }
    }

    mod folder {
        use super::*;

        fn pick(root: &str, pick: RevisionPick) -> Result<String, String> {
            pick_revision(
                &UniversalPath::from(root),
                PathKind::Folder,
                &pick,
                &streams(),
                None,
            )
            .map(|p| p.to_string())
        }

        fn pick_in_workspace(root: &str, pick: RevisionPick) -> Result<String, String> {
            pick_revision(
                &UniversalPath::from(root),
                PathKind::Folder,
                &pick,
                &streams(),
                Some("//stream/main/src"),
            )
            .map(|p| p.to_string())
        }

        #[test]
        fn depot_picks_on_a_workspace_folder_use_its_depot_folder() {
            let local = r"C:\ws\src";
            assert_eq!(
                pick_in_workspace(local, RevisionPick::Head).unwrap(),
                "//stream/main/src"
            );
            assert_eq!(
                pick_in_workspace(local, RevisionPick::Typed("@1234".into())).unwrap(),
                "//stream/main/src@1234"
            );
            assert_eq!(
                pick_in_workspace(local, RevisionPick::Stream("//stream/dev".into())).unwrap(),
                "//stream/dev/src"
            );
        }

        #[test]
        fn working_tree_on_a_workspace_folder_stays_local() {
            assert_eq!(
                pick_in_workspace(r"C:\ws\src", RevisionPick::WorkingTree).unwrap(),
                r"C:\ws\src"
            );
        }

        #[test]
        fn git_picks_on_a_workspace_folder_report_why() {
            let local = r"C:\ws\src";
            assert!(pick_in_workspace(local, RevisionPick::Typed("main".into())).is_err());
            assert!(pick_in_workspace(local, RevisionPick::Index).is_err());
            assert!(pick_in_workspace(local, RevisionPick::Git("main".into())).is_err());
        }

        #[test]
        fn typed_text_is_a_revision_or_the_index() {
            let typed = |text: &str| pick(r"D:\repo\src", RevisionPick::Typed(text.to_owned()));
            assert_eq!(typed("  HEAD~3 ").unwrap(), r"D:\repo\src@git:HEAD~3");
            assert_eq!(typed("index").unwrap(), r"D:\repo\src@git:index");
            assert!(typed("   ").is_err());
        }

        #[test]
        fn typed_numbers_on_a_depot_root_pick_changes_and_shelves() {
            let typed = |text: &str| pick("//depot/main/src", RevisionPick::Typed(text.to_owned()));
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
                pick("//depot/main/src@1234", RevisionPick::Head).unwrap(),
                "//depot/main/src"
            );
        }

        #[test]
        fn a_stream_pick_swaps_the_stream_prefix_and_keeps_a_change() {
            assert_eq!(
                pick(
                    "//stream/main/src/gui@1234",
                    RevisionPick::Stream("//stream/dev".into())
                )
                .unwrap(),
                "//stream/dev/src/gui@1234"
            );
            assert_eq!(
                pick(
                    "//stream/main/rel/src",
                    RevisionPick::Stream("//stream/dev".into())
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
                    RevisionPick::Stream("//stream/dev".into())
                )
                .unwrap(),
                "//stream/dev/src"
            );
        }

        #[test]
        fn a_stream_pick_on_an_unlisted_stream_reports_why() {
            assert!(pick("//other/x", RevisionPick::Stream("//stream/dev".into())).is_err());
        }

        #[test]
        fn git_picks_on_a_depot_root_report_why() {
            for p in [
                RevisionPick::WorkingTree,
                RevisionPick::Index,
                RevisionPick::Git("main".into()),
            ] {
                assert!(pick("//depot/main/src", p).is_err());
            }
        }

        #[test]
        fn depot_picks_on_a_git_root_report_why() {
            for p in [
                RevisionPick::Head,
                RevisionPick::Stream("//stream/dev".into()),
            ] {
                assert!(pick(r"D:\repo\src", p).is_err());
            }
        }

        #[test]
        fn git_picks_on_a_git_root_keep_the_sub_path() {
            assert_eq!(
                pick(
                    r"D:\repo\src@git:main",
                    RevisionPick::Git("origin/dev".into())
                )
                .unwrap(),
                r"D:\repo\src@git:origin/dev"
            );
            assert_eq!(
                pick(r"D:\repo\src@git:main", RevisionPick::WorkingTree).unwrap(),
                r"D:\repo\src"
            );
            assert_eq!(
                pick(r"D:\repo\src", RevisionPick::Typed("HEAD~3".into())).unwrap(),
                r"D:\repo\src@git:HEAD~3"
            );
        }
    }
}
