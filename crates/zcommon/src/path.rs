#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSegment {
    pub name: String,
    pub path: String,
}

/// Roots are `C:\`, `\\server\share`, `//depot` or `/`; each segment's path is a prefix of `path`.
pub fn split_path(path: &str) -> Vec<PathSegment> {
    let depot = is_depot(path);
    let is_sep = |c: char| c == '/' || (!depot && c == '\\');
    let root_end = root_len(path, is_sep);
    let mut segments = Vec::new();
    if root_end > 0 {
        segments.push(PathSegment {
            name: path[..root_end].to_owned(),
            path: path[..root_end].to_owned(),
        });
    }
    let mut start = root_end;
    for part in path[root_end..].split(is_sep) {
        let end = start + part.len();
        if !part.is_empty() {
            segments.push(PathSegment {
                name: part.to_owned(),
                path: path[..end].to_owned(),
            });
        }
        start = end + 1;
    }
    segments
}

/// Shared leading [`split_path`] segments, root included; a file counts as its parent folder.
pub fn common_parent(a: &str, a_is_file: bool, b: &str, b_is_file: bool) -> usize {
    let depot = is_depot(a);
    if depot != is_depot(b) {
        return 0;
    }
    let folders = |path: &str, is_file: bool| {
        let mut segments = split_path(path);
        if is_file {
            segments.pop();
        }
        segments
    };
    let (a, b) = (folders(a, a_is_file), folders(b, b_is_file));
    a.iter()
        .zip(&b)
        .take_while(|(a, b)| {
            if depot {
                a.name == b.name
            } else {
                local_key(&a.name) == local_key(&b.name)
            }
        })
        .count()
}

// Mirrors UniversalPath: `//` is depot, so a forward-slash UNC path reads as depot.
fn is_depot(path: &str) -> bool {
    path.starts_with("//")
}

fn root_len(path: &str, is_sep: impl Fn(char) -> bool + Copy) -> usize {
    let component_end = |start: usize| {
        let start = start.min(path.len());
        path[start..].find(is_sep).map_or(path.len(), |i| start + i)
    };
    let mut chars = path.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some(a), Some(b), _) if is_sep(a) && is_sep(b) => {
            let server_end = component_end(2);
            if is_depot(path) {
                server_end
            } else {
                component_end(server_end + 1)
            }
        }
        (Some(drive), Some(':'), sep) if drive.is_ascii_alphabetic() => {
            if sep.is_some_and(is_sep) {
                3
            } else {
                2
            }
        }
        (Some(a), ..) if is_sep(a) => 1,
        _ => 0,
    }
}

fn local_key(name: &str) -> String {
    let name = name.replace('/', "\\");
    if cfg!(windows) {
        name.to_lowercase()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(name: &str, path: &str) -> PathSegment {
        PathSegment {
            name: name.to_owned(),
            path: path.to_owned(),
        }
    }

    #[test]
    fn a_drive_path_splits_into_the_drive_root_and_each_component() {
        assert_eq!(
            split_path(r"C:\a\b\c.txt"),
            [
                seg(r"C:\", r"C:\"),
                seg("a", r"C:\a"),
                seg("b", r"C:\a\b"),
                seg("c.txt", r"C:\a\b\c.txt"),
            ]
        );
    }

    #[test]
    fn a_bare_drive_root_is_one_segment() {
        assert_eq!(split_path(r"D:\"), [seg(r"D:\", r"D:\")]);
    }

    #[test]
    fn a_unc_path_keeps_server_and_share_together_as_the_root() {
        assert_eq!(
            split_path(r"\\server\share\a"),
            [
                seg(r"\\server\share", r"\\server\share"),
                seg("a", r"\\server\share\a"),
            ]
        );
    }

    #[test]
    fn a_relative_path_has_no_root_segment() {
        assert_eq!(split_path(r"a\b"), [seg("a", "a"), seg("b", r"a\b")]);
        assert_eq!(
            split_path("../x/y"),
            [seg("..", ".."), seg("x", "../x"), seg("y", "../x/y")]
        );
    }

    #[test]
    fn a_depot_path_keeps_the_revision_on_the_last_segment() {
        assert_eq!(
            split_path("//depot/a/b.txt#3"),
            [
                seg("//depot", "//depot"),
                seg("a", "//depot/a"),
                seg("b.txt#3", "//depot/a/b.txt#3"),
            ]
        );
    }

    #[test]
    fn trailing_and_doubled_separators_add_no_empty_segments() {
        assert_eq!(
            split_path(r"C:\a\b\"),
            [seg(r"C:\", r"C:\"), seg("a", r"C:\a"), seg("b", r"C:\a\b")]
        );
        assert_eq!(
            split_path(r"C:\a\\b\\"),
            [seg(r"C:\", r"C:\"), seg("a", r"C:\a"), seg("b", r"C:\a\\b")]
        );
        assert_eq!(
            split_path("//depot/a/"),
            [seg("//depot", "//depot"), seg("a", "//depot/a")]
        );
        assert_eq!(
            split_path(r"\\server\share\"),
            [seg(r"\\server\share", r"\\server\share")]
        );
    }

    #[test]
    fn forward_slashes_split_local_paths_too() {
        assert_eq!(
            split_path("C:/a/b"),
            [seg("C:/", "C:/"), seg("a", "C:/a"), seg("b", "C:/a/b")]
        );
    }

    #[test]
    fn a_rooted_path_without_a_drive_has_the_separator_as_its_root() {
        assert_eq!(
            split_path("/home/x"),
            [seg("/", "/"), seg("home", "/home"), seg("x", "/home/x")]
        );
    }

    #[test]
    fn an_empty_path_has_no_segments() {
        assert_eq!(split_path(""), []);
    }

    #[test]
    fn folders_share_their_common_leading_segments() {
        assert_eq!(common_parent(r"C:\a\b\c", false, r"C:\a\b\d\e", false), 3);
        assert_eq!(common_parent(r"C:\a", false, r"C:\b", false), 1);
    }

    #[test]
    fn files_compare_their_parent_folders() {
        assert_eq!(
            common_parent(r"C:\a\b\x.txt", true, r"C:\a\c\x.txt", true),
            2
        );
        assert_eq!(common_parent(r"C:\a\x", true, r"C:\a\x", false), 2);
    }

    #[test]
    fn identical_paths_share_every_folder_segment() {
        assert_eq!(common_parent(r"C:\a\b", false, r"C:\a\b", false), 3);
        assert_eq!(
            common_parent(r"C:\a\b\x.txt", true, r"C:\a\b\x.txt", true),
            3
        );
    }

    #[test]
    fn one_path_a_prefix_of_the_other_shares_the_shorter() {
        assert_eq!(common_parent(r"C:\a", false, r"C:\a\b", false), 2);
        assert_eq!(common_parent(r"C:\a\b", false, r"C:\a", false), 2);
        assert_eq!(common_parent(r"C:\a\x.txt", true, r"C:\a\b\y.txt", true), 2);
    }

    #[test]
    fn different_roots_share_nothing() {
        assert_eq!(common_parent(r"C:\a\b", false, r"D:\a\b", false), 0);
        assert_eq!(
            common_parent(r"\\s\share\a", false, r"\\s\other\a", false),
            0
        );
        assert_eq!(common_parent(r"C:\a", false, "a", false), 0);
    }

    #[test]
    fn local_and_depot_paths_share_nothing() {
        assert_eq!(
            common_parent(r"\\depot\a\b.txt", true, "//depot/a/b.txt", true),
            0
        );
        assert_eq!(
            common_parent("/depot/a/b.txt", true, "//depot/a/b.txt", true),
            0
        );
    }

    #[test]
    fn depot_paths_compare_exactly() {
        assert_eq!(
            common_parent("//depot/a/b/x.txt#3", true, "//depot/a/c/x.txt#4", true),
            2
        );
        assert_eq!(
            common_parent("//depot/A/b.txt", true, "//depot/a/b.txt", true),
            1
        );
        assert_eq!(common_parent("//Depot/a", false, "//depot/a", false), 0);
    }

    #[test]
    fn separator_style_does_not_matter_for_local_paths() {
        assert_eq!(common_parent(r"C:\a\b", false, "C:/a/b", false), 3);
    }

    #[cfg(windows)]
    #[test]
    fn local_segments_compare_case_insensitively_on_windows() {
        assert_eq!(
            common_parent(r"D:\Proj\Src", false, r"d:\proj\src", false),
            3
        );
        assert_eq!(
            common_parent(r"\\Server\Share\Ä", false, r"\\server\SHARE\ä", false),
            2
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn local_segments_compare_case_sensitively_off_windows() {
        assert_eq!(common_parent("/home/Proj", false, "/home/proj", false), 2);
    }
}
