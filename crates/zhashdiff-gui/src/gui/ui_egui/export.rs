use std::collections::HashMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

use zhashdiff::conflict::DuplicateGroup;

use crate::ui_egui::fs_tree::{DiffState, VisibleRowTwoFolderDiff};
use crate::ui_egui::status_line::ToolMessage;

fn root_line(out: &mut String, side: usize, root: Option<&Path>) {
    let root = root.map_or("(none)".into(), |root| root.display().to_string());
    writeln!(out, "Folder {side}: {root}").unwrap();
}

fn state_name(state: &DiffState) -> &'static str {
    match state {
        DiffState::Different(..) => "different",
        DiffState::Same(..) => "same",
        DiffState::Similar(..) => "similar",
        DiffState::OnlyInFirst(..) => "only in 1",
        DiffState::OnlyInSecond(..) => "only in 2",
        DiffState::Pending(..) => "pending",
    }
}

pub fn folder_diff_report(
    roots: [Option<&Path>; 2],
    counts: &str,
    rows: &[VisibleRowTwoFolderDiff],
) -> String {
    let mut out = String::new();
    root_line(&mut out, 1, roots[0]);
    root_line(&mut out, 2, roots[1]);
    writeln!(out, "{counts}\n").unwrap();
    for row in rows.iter().filter(|row| !row.rel_path.is_empty()) {
        let slash = if row.is_dir { "/" } else { "" };
        let state = state_name(&row.diff_state);
        writeln!(out, "{state:<9}  {}{slash}", row.rel_path).unwrap();
    }
    out
}

pub fn duplicates_report(
    root: Option<&Path>,
    groups: &[DuplicateGroup],
    keepers: &HashMap<String, PathBuf>,
    hashing: bool,
) -> String {
    let mut out = String::new();
    root_line(&mut out, 1, root);
    let files: usize = groups.iter().map(|group| group.paths.len()).sum();
    let extra_bytes: u64 = groups
        .iter()
        .map(|group| (group.paths.len() as u64 - 1) * group.size)
        .sum();
    writeln!(
        out,
        "{} groups, {files} files, {extra_bytes} bytes in extra copies",
        groups.len()
    )
    .unwrap();
    if hashing {
        out.push_str("Hashing is still running: the groups may be incomplete.\n");
    }
    for (index, group) in groups.iter().enumerate() {
        writeln!(
            out,
            "\nGroup {}: {} files of {} bytes, hash {}",
            index + 1,
            group.paths.len(),
            group.size,
            group.hash
        )
        .unwrap();
        let keeper = keepers
            .get(&group.hash)
            .filter(|keeper| group.paths.contains(keeper));
        for path in &group.paths {
            let mark = match keeper {
                Some(keeper) if keeper == path => "keep",
                Some(_) => "delete",
                None => "",
            };
            writeln!(out, "  {mark:<6}  {}", path.display()).unwrap();
        }
    }
    out
}

pub fn save_report(text: &str, file_name: &str) -> Option<ToolMessage> {
    let path = rfd::FileDialog::new()
        .set_file_name(file_name)
        .add_filter("Text", &["txt"])
        .save_file()?;
    Some(match std::fs::write(&path, text) {
        Ok(()) => ToolMessage {
            status: Some(format!("Exported to {}", path.display())),
            ..ToolMessage::default()
        },
        Err(e) => ToolMessage {
            error: Some(format!("Can not write {}: {e}", path.display())),
            ..ToolMessage::default()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_egui::fs_tree::DiffState;

    fn row(rel_path: &str, is_dir: bool, diff_state: DiffState) -> VisibleRowTwoFolderDiff {
        VisibleRowTwoFolderDiff {
            rel_path: rel_path.to_owned(),
            is_dir,
            depth: rel_path.split('/').filter(|c| !c.is_empty()).count() as _,
            diff_state,
            file_kinds: [None, None],
            blueprints: [false, false],
            folder_likeness: None,
        }
    }

    #[test]
    fn the_folder_report_lists_each_shown_row_with_its_state_under_the_roots_and_counts() {
        let rows = [
            row("", true, DiffState::Different(0, 0)),
            row("src", true, DiffState::Different(1, 1)),
            row("src/main.rs", false, DiffState::Different(2, 2)),
            row("src/lib.rs", false, DiffState::Same(3, 3)),
            row("old.txt", false, DiffState::OnlyInFirst(4)),
            row("new", true, DiffState::OnlyInSecond(4)),
            row("big.bin", false, DiffState::Pending(5, 5)),
            row("photo.png", false, DiffState::Similar(6, 6)),
        ];
        assert_eq!(
            folder_diff_report(
                [Some(Path::new("C:/a")), Some(Path::new("C:/b"))],
                "1 different, 1 only in 1",
                &rows
            ),
            "Folder 1: C:/a\n\
             Folder 2: C:/b\n\
             1 different, 1 only in 1\n\
             \n\
             different  src/\n\
             different  src/main.rs\n\
             same       src/lib.rs\n\
             only in 1  old.txt\n\
             only in 2  new/\n\
             pending    big.bin\n\
             similar    photo.png\n"
        );
    }

    #[test]
    fn a_folder_report_without_a_second_root_names_it_as_none() {
        assert_eq!(
            folder_diff_report([Some(Path::new("C:/a")), None], "", &[]),
            "Folder 1: C:/a\nFolder 2: (none)\n\n\n"
        );
    }

    fn group(size: u64, hash: &str, paths: &[&str]) -> DuplicateGroup {
        DuplicateGroup {
            size,
            hash: hash.to_owned(),
            paths: paths.iter().map(PathBuf::from).collect(),
        }
    }

    #[test]
    fn the_duplicates_report_lists_each_group_and_marks_keepers_and_files_a_resolve_deletes() {
        let groups = [
            group(10, "aa", &["C:/a/x", "C:/a/y", "C:/a/z"]),
            group(4, "bb", &["C:/a/p", "C:/a/q"]),
        ];
        let keepers = HashMap::from([("aa".to_owned(), PathBuf::from("C:/a/y"))]);
        assert_eq!(
            duplicates_report(Some(Path::new("C:/a")), &groups, &keepers, false),
            "Folder 1: C:/a\n\
             2 groups, 5 files, 24 bytes in extra copies\n\
             \n\
             Group 1: 3 files of 10 bytes, hash aa\n\
             \x20 delete  C:/a/x\n\
             \x20 keep    C:/a/y\n\
             \x20 delete  C:/a/z\n\
             \n\
             Group 2: 2 files of 4 bytes, hash bb\n\
             \x20         C:/a/p\n\
             \x20         C:/a/q\n"
        );
    }

    #[test]
    fn a_duplicates_report_made_while_hashing_says_the_groups_may_be_incomplete() {
        assert_eq!(
            duplicates_report(Some(Path::new("C:/a")), &[], &HashMap::new(), true),
            "Folder 1: C:/a\n\
             0 groups, 0 files, 0 bytes in extra copies\n\
             Hashing is still running: the groups may be incomplete.\n"
        );
    }
}
