use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UniversalPath {
    /// Represented as //stream/path/file.txt
    Depot(String, Option<DepotRev>),
    /// Represented as C:\User\File.txt or /home/user/file.txt
    Local(PathBuf),
    Git(PathBuf, GitRev),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DepotRev {
    Rev(u32),
    Change(u32),
    Shelf(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GitRev {
    Index,
    Rev(String),
}

const GIT_SUFFIX: &str = "@git:";

impl std::fmt::Display for DepotRev {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DepotRev::Rev(n) => write!(f, "#{n}"),
            DepotRev::Change(n) => write!(f, "@{n}"),
            DepotRev::Shelf(n) => write!(f, "@={n}"),
        }
    }
}

impl std::fmt::Display for GitRev {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitRev::Index => write!(f, "index"),
            GitRev::Rev(rev) => write!(f, "{rev}"),
        }
    }
}

impl Default for UniversalPath {
    fn default() -> Self {
        UniversalPath::Local(PathBuf::new())
    }
}

impl std::fmt::Display for UniversalPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UniversalPath::Local(p) => write!(f, "{}", p.display()),
            UniversalPath::Depot(s, Some(rev)) => write!(f, "{}{}", s, rev),
            UniversalPath::Depot(s, None) => write!(f, "{}", s),
            UniversalPath::Git(p, rev) => write!(f, "{}{GIT_SUFFIX}{}", p.display(), rev),
        }
    }
}

impl From<&String> for UniversalPath {
    fn from(s: &String) -> Self {
        UniversalPath::new(s)
    }
}
impl From<PathBuf> for UniversalPath {
    fn from(path: PathBuf) -> Self {
        UniversalPath::new(path.as_os_str())
    }
}

impl From<&Path> for UniversalPath {
    fn from(path: &Path) -> Self {
        UniversalPath::new(path.as_os_str())
    }
}

impl From<String> for UniversalPath {
    fn from(s: String) -> Self {
        UniversalPath::new(s)
    }
}

impl From<&str> for UniversalPath {
    fn from(s: &str) -> Self {
        UniversalPath::new(s)
    }
}

impl UniversalPath {
    pub fn new<S: AsRef<OsStr>>(s: S) -> Self {
        let os_str = s.as_ref();
        let cow = os_str.to_string_lossy();

        if cow.starts_with("//") {
            let (path, rev) = split_depot_rev(&cow);
            return Self::Depot(path.to_owned(), rev);
        }
        // The revision never goes through Path, which would rewrite the `/` in `origin/main`.
        if let Some(idx) = cow.rfind(GIT_SUFFIX) {
            let rev = &cow[idx + GIT_SUFFIX.len()..];
            if !rev.is_empty() {
                let rev = match rev {
                    "index" => GitRev::Index,
                    _ => GitRev::Rev(rev.to_owned()),
                };
                return Self::Git(Self::normalize_local_path(Path::new(&cow[..idx])), rev);
            }
        }
        Self::Local(Self::normalize_local_path(Path::new(os_str)))
    }

    pub fn append(&mut self, other: UniversalPath) {
        match (self, other) {
            (
                Self::Local(base) | Self::Git(base, _),
                Self::Local(addition) | Self::Git(addition, _),
            ) => {
                base.push(addition);
                *base = Self::normalize_local_path(base);
            }
            (Self::Local(base) | Self::Git(base, _), Self::Depot(addition, _)) => {
                let clean_addition = addition.trim_start_matches('/');
                base.push(clean_addition);
                *base = Self::normalize_local_path(base);
            }
            (Self::Depot(base, _), Self::Depot(addition, _)) => {
                let clean_base = base.trim_end_matches('/');
                let clean_addition = addition.trim_start_matches('/');
                *base = format!("{}/{}", clean_base, clean_addition);
            }
            (Self::Depot(base, _), Self::Local(addition) | Self::Git(addition, _)) => {
                let clean_base = base.trim_end_matches('/');
                let add_str = addition.to_string_lossy().replace('\\', "/");
                let clean_addition = add_str.trim_start_matches('/');
                *base = format!("{}/{}", clean_base, clean_addition);
            }
        }
    }

    fn normalize_local_path(path: &Path) -> PathBuf {
        let mut components = Vec::new();

        for comp in path.components() {
            match comp {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    if let Some(std::path::Component::Normal(_)) = components.last() {
                        components.pop();
                    } else {
                        components.push(comp);
                    }
                }
                _ => components.push(comp),
            }
        }

        components.into_iter().collect()
    }

    pub fn as_path(&self) -> Option<&Path> {
        match self {
            Self::Local(p) => Some(p.as_path()),
            Self::Depot(..) => None, // TODO: get local path for depot
            Self::Git(..) => None,
        }
    }

    /// A depot path's local file is only known to p4 (`p4 where`), so none is derived here.
    pub fn to_p4_string(&self) -> String {
        match self {
            Self::Depot(s, Some(rev)) => format!("{}{}", s, rev),
            Self::Depot(s, None) => s.clone(),
            Self::Local(p) | Self::Git(p, _) => p.to_string_lossy().replace('\\', "/"),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Depot(s, _) => s.is_empty(),
            Self::Local(p) | Self::Git(p, _) => p.as_os_str().is_empty(),
        }
    }

    pub fn is_depot(&self) -> bool {
        matches!(self, Self::Depot(..))
    }

    pub fn revision(&self) -> Option<u32> {
        match self {
            Self::Depot(_, Some(DepotRev::Rev(rev))) => Some(*rev),
            _ => None,
        }
    }

    pub fn set_revision(&mut self, new_rev: Option<u32>) {
        if let Self::Depot(_, rev) = self {
            *rev = new_rev.map(DepotRev::Rev);
        }
    }

    /// Whether this is a temporary file: a local path inside `temp_root` (in practice the OS
    /// temp dir, where p4 puts the copies it hands an external diff tool). Depot paths never
    /// are, and an empty root contains nothing.
    ///
    /// Lexical, so it is cheap enough to ask every frame. Separators, case and a `\\?\` prefix
    /// don't matter; an 8.3 short name and its long name don't match (canonicalize both first
    /// where that matters).
    pub fn is_temp(&self, temp_root: &Path) -> bool {
        let Self::Local(path) = self else {
            return false;
        };
        let root = lexical_components(temp_root);
        if root == (false, Vec::new()) {
            return false;
        }
        let path = lexical_components(path);
        path.0 == root.0 && path.1.starts_with(&root.1)
    }
}

fn split_depot_rev(s: &str) -> (&str, Option<DepotRev>) {
    let Some(idx) = s.rfind(['#', '@']) else {
        return (s, None);
    };
    let (path, spec) = s.split_at(idx);
    let number = |n: &str| {
        (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .then(|| n.parse::<u32>().ok())
            .flatten()
    };
    let rev = if let Some(n) = spec.strip_prefix("@=") {
        number(n).map(DepotRev::Shelf)
    } else if let Some(n) = spec.strip_prefix('@') {
        number(n).map(DepotRev::Change)
    } else {
        number(&spec[1..]).map(DepotRev::Rev)
    };
    match rev {
        Some(rev) => (path, Some(rev)),
        None => (s, None),
    }
}

/// Whether the path starts at a root separator, and its lowercased components, with either
/// separator and the `\\?\` and `\\?\UNC\` verbatim prefixes taken as their plain forms.
fn lexical_components(path: &Path) -> (bool, Vec<String>) {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = if let Some(unc) = s.strip_prefix("//?/UNC/") {
        format!("//{unc}")
    } else {
        s.strip_prefix("//?/").map(str::to_owned).unwrap_or(s)
    };
    let components = s
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .map(str::to_lowercase)
        .collect();
    (s.starts_with('/'), components)
}

impl AsRef<OsStr> for UniversalPath {
    fn as_ref(&self) -> &OsStr {
        match self {
            Self::Depot(s, _) => OsStr::new(s),
            Self::Local(p) | Self::Git(p, _) => p.as_os_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::os_path;

    #[test]
    fn test_parsing() {
        let depot = UniversalPath::new("//stream/main/file.txt");
        assert!(matches!(depot, UniversalPath::Depot(..)));
        assert_eq!(depot.to_p4_string(), "//stream/main/file.txt");

        let local = UniversalPath::new(r"C:\User\File.txt");
        assert!(matches!(local, UniversalPath::Local(..)));
        assert_eq!(local.to_p4_string(), "C:/User/File.txt");
    }

    #[test]
    fn test_append_local_to_local() {
        let mut base = UniversalPath::new(os_path(r"C:\User\Project"));
        let addition = UniversalPath::new(os_path(r"src\main.rs"));
        base.append(addition);
        assert_eq!(
            base,
            UniversalPath::Local(PathBuf::from(os_path(r"C:\User\Project\src\main.rs")))
        );
    }

    #[test]
    fn test_append_depot_to_local() {
        let mut base = UniversalPath::new(os_path(r"C:\User\Project"));
        let addition = UniversalPath::new("//depot/src/main.rs");
        base.append(addition);
        assert_eq!(
            base,
            UniversalPath::Local(PathBuf::from(os_path(r"C:\User\Project\depot\src\main.rs")))
        );
    }

    #[test]
    fn test_append_depot_to_depot() {
        let mut base = UniversalPath::new("//stream/main/");
        let addition = UniversalPath::new("//folder/file.txt#3");
        base.append(addition);
        assert_eq!(
            base,
            UniversalPath::Depot(String::from("//stream/main/folder/file.txt"), None)
        );
    }

    const TEMP: &str = r"C:\Users\me\AppData\Local\Temp";

    fn is_temp(path: &str, root: &str) -> bool {
        UniversalPath::new(path).is_temp(Path::new(root))
    }

    #[test]
    fn a_path_inside_the_temp_root_is_temp() {
        assert!(is_temp(
            r"C:\Users\me\AppData\Local\Temp\p4v\file#3.rs",
            TEMP
        ));
        assert!(is_temp(r"C:\Users\me\AppData\Local\Temp\a.txt", TEMP));
        assert!(is_temp(TEMP, TEMP));
    }

    #[test]
    fn a_path_outside_the_temp_root_is_not_temp() {
        assert!(!is_temp(r"C:\Users\me\work\src\main.rs", TEMP));
        assert!(!is_temp(r"D:\Users\me\AppData\Local\Temp\a.txt", TEMP));
        assert!(!is_temp(r"C:\Users\me\AppData\Local", TEMP));
        // Same leading characters, another directory.
        assert!(!is_temp(r"C:\Users\me\AppData\Local\Temp2\a.txt", TEMP));
        assert!(!is_temp(r"C:\Users\me\AppData\Local\Tempfile.txt", TEMP));
        // Relative paths are not under an absolute root, even with matching names.
        assert!(!is_temp(r"Users\me\AppData\Local\Temp\a.txt", TEMP));
        assert!(!is_temp("tmp/a.txt", "/tmp"));
    }

    #[test]
    fn case_and_separators_do_not_matter() {
        assert!(is_temp(r"c:\users\ME\appdata\local\TEMP\a.txt", TEMP));
        assert!(is_temp("C:/Users/me/AppData/Local/Temp/p4v/a.txt", TEMP));
        assert!(is_temp(
            r"C:\Users\me\AppData\Local\Temp\a.txt",
            "C:/users/me/appdata/local/temp"
        ));
        // GetTempPath returns the root with a trailing separator.
        assert!(is_temp(
            r"C:\Users\me\AppData\Local\Temp\a.txt",
            r"C:\Users\me\AppData\Local\Temp\"
        ));
        assert!(is_temp(r"C:\Users\me\AppData\Local\Temp\\p4v\\a.txt", TEMP));
        assert!(is_temp("/tmp/p4/a.txt", "/tmp/"));
    }

    #[test]
    fn a_verbatim_prefix_does_not_matter() {
        // canonicalize returns \\?\ paths; a path it couldn't resolve keeps its plain form.
        assert!(is_temp(
            r"C:\Users\me\AppData\Local\Temp\a.txt",
            r"\\?\C:\Users\me\AppData\Local\Temp"
        ));
        assert!(is_temp(r"\\?\C:\Users\me\AppData\Local\Temp\a.txt", TEMP));
        assert!(is_temp(
            r"\\?\UNC\server\share\tmp\a.txt",
            r"\\server\share\tmp"
        ));
        assert!(!is_temp(
            r"\\?\UNC\server\share\a.txt",
            r"\\server\share\tmp"
        ));
    }

    #[test]
    fn depot_paths_are_never_temp() {
        assert!(!is_temp("//depot/main/file.rs#3", TEMP));
        assert!(!is_temp("//depot/main/file.rs", "//depot/main"));
        assert!(!is_temp("//depot/main/file.rs", "/"));
    }

    #[test]
    fn an_empty_root_contains_nothing() {
        assert!(!is_temp(r"C:\Users\me\AppData\Local\Temp\a.txt", ""));
        assert!(!is_temp("a.txt", ""));
        assert!(!is_temp("", ""));
    }

    fn git(path: &str, rev: &str) -> UniversalPath {
        UniversalPath::Git(PathBuf::from(path), GitRev::Rev(rev.into()))
    }

    fn depot(path: &str, rev: Option<DepotRev>) -> UniversalPath {
        UniversalPath::Depot(path.into(), rev)
    }

    fn assert_round_trip(text: &str, expected: UniversalPath) {
        let parsed = UniversalPath::new(text);
        assert_eq!(parsed, expected, "parsing {text}");
        assert_eq!(parsed.to_string(), text);
        assert_eq!(UniversalPath::new(parsed.to_string()), parsed);
    }

    #[test]
    fn a_plain_local_path_is_the_working_tree() {
        assert_round_trip(
            r"D:\repo\src\a.rs",
            UniversalPath::Local(PathBuf::from(r"D:\repo\src\a.rs")),
        );
    }

    #[test]
    fn git_suffix_parses_branches_tags_and_expressions() {
        assert_round_trip(
            r"D:\repo\src\a.rs@git:main",
            git(r"D:\repo\src\a.rs", "main"),
        );
        assert_round_trip(
            r"D:\repo\src\a.rs@git:v1.2",
            git(r"D:\repo\src\a.rs", "v1.2"),
        );
        assert_round_trip(
            r"D:\repo\src\a.rs@git:HEAD~3",
            git(r"D:\repo\src\a.rs", "HEAD~3"),
        );
        assert_round_trip(
            r"D:\repo\src\a.rs@git:main^",
            git(r"D:\repo\src\a.rs", "main^"),
        );
        assert_round_trip(
            r"D:\repo\src\a.rs@git:0eb9a9aa2f44",
            git(r"D:\repo\src\a.rs", "0eb9a9aa2f44"),
        );
        assert_round_trip(r"D:\repo\src@git:main", git(r"D:\repo\src", "main"));
    }

    #[test]
    fn git_revisions_with_slashes_and_at_signs_are_kept_verbatim() {
        assert_round_trip(
            r"D:\repo\src\a.rs@git:origin/main",
            git(r"D:\repo\src\a.rs", "origin/main"),
        );
        assert_round_trip(
            r"D:\repo\src\a.rs@git:feature\x",
            git(r"D:\repo\src\a.rs", r"feature\x"),
        );
        assert_round_trip(
            r"D:\repo\a.rs@git:HEAD@{1}",
            git(r"D:\repo\a.rs", "HEAD@{1}"),
        );
    }

    #[test]
    fn git_index_is_the_staged_version() {
        assert_round_trip(
            r"D:\repo\src\a.rs@git:index",
            UniversalPath::Git(PathBuf::from(r"D:\repo\src\a.rs"), GitRev::Index),
        );
        assert_round_trip(
            r"D:\repo\src\a.rs@git:Index",
            git(r"D:\repo\src\a.rs", "Index"),
        );
    }

    #[test]
    fn only_the_last_git_suffix_counts() {
        assert_round_trip(
            r"D:\repo\x@git:a\b.rs@git:main",
            git(r"D:\repo\x@git:a\b.rs", "main"),
        );
    }

    #[test]
    fn a_local_path_with_at_but_no_git_suffix_stays_local() {
        assert_round_trip(
            r"D:\users\me@corp\a@b.rs",
            UniversalPath::Local(PathBuf::from(r"D:\users\me@corp\a@b.rs")),
        );
        assert_round_trip(
            r"D:\repo\a.rs@gitx:main",
            UniversalPath::Local(PathBuf::from(r"D:\repo\a.rs@gitx:main")),
        );
    }

    #[test]
    fn an_empty_git_revision_is_not_a_git_suffix() {
        assert_round_trip(
            r"D:\repo\a.rs@git:",
            UniversalPath::Local(PathBuf::from(r"D:\repo\a.rs@git:")),
        );
    }

    #[test]
    fn the_git_path_is_normalized_like_a_local_path() {
        assert_eq!(
            UniversalPath::new(os_path(r"D:\repo\src\..\.\a.rs@git:main")),
            git(&os_path(r"D:\repo\a.rs"), "main")
        );
    }

    #[test]
    fn depot_revision_specs_parse_and_print() {
        assert_round_trip("//depot/a/b.txt", depot("//depot/a/b.txt", None));
        assert_round_trip(
            "//depot/a/b.txt#3",
            depot("//depot/a/b.txt", Some(DepotRev::Rev(3))),
        );
        assert_round_trip(
            "//depot/a/b.txt@1234",
            depot("//depot/a/b.txt", Some(DepotRev::Change(1234))),
        );
        assert_round_trip(
            "//depot/a/b.txt@=1234",
            depot("//depot/a/b.txt", Some(DepotRev::Shelf(1234))),
        );
        assert_round_trip("//stream/main/sub", depot("//stream/main/sub", None));
    }

    #[test]
    fn depot_specs_print_in_the_p4_form() {
        assert_eq!(
            depot("//depot/a.txt", Some(DepotRev::Change(12))).to_p4_string(),
            "//depot/a.txt@12"
        );
        assert_eq!(
            depot("//depot/a.txt", Some(DepotRev::Shelf(12))).to_p4_string(),
            "//depot/a.txt@=12"
        );
    }

    #[test]
    fn a_non_numeric_depot_suffix_stays_in_the_path() {
        assert_round_trip("//depot/a.txt#head", depot("//depot/a.txt#head", None));
        assert_round_trip("//depot/a.txt@label", depot("//depot/a.txt@label", None));
        assert_round_trip("//depot/a.txt@=x", depot("//depot/a.txt@=x", None));
        assert_round_trip("//depot/a.txt@", depot("//depot/a.txt@", None));
        assert_round_trip("//depot/a.txt#+5", depot("//depot/a.txt#+5", None));
    }

    #[test]
    fn only_the_last_depot_spec_counts() {
        assert_round_trip(
            "//depot/a#1@2/b.txt@=3",
            depot("//depot/a#1@2/b.txt", Some(DepotRev::Shelf(3))),
        );
        assert_round_trip(
            "//depot/a@=1/b.txt#2",
            depot("//depot/a@=1/b.txt", Some(DepotRev::Rev(2))),
        );
        assert_round_trip(
            "//depot/a#1/b.txt@2",
            depot("//depot/a#1/b.txt", Some(DepotRev::Change(2))),
        );
    }

    #[test]
    fn a_depot_path_never_takes_a_git_suffix() {
        assert_round_trip(
            "//depot/a.txt@git:main",
            depot("//depot/a.txt@git:main", None),
        );
    }

    #[test]
    fn appending_to_a_git_path_keeps_its_revision() {
        let mut base = UniversalPath::new(os_path(r"D:\repo@git:main"));
        base.append(UniversalPath::new(os_path(r"src\a.rs")));
        assert_eq!(base, git(&os_path(r"D:\repo\src\a.rs"), "main"));
    }

    #[test]
    fn revision_is_the_depot_file_revision_only() {
        assert_eq!(UniversalPath::new("//depot/a.txt#3").revision(), Some(3));
        assert_eq!(UniversalPath::new("//depot/a.txt@3").revision(), None);
        assert_eq!(UniversalPath::new("//depot/a.txt@=3").revision(), None);
        assert_eq!(UniversalPath::new(r"D:\a.rs@git:main").revision(), None);

        let mut path = UniversalPath::new("//depot/a.txt@=3");
        path.set_revision(Some(5));
        assert_eq!(path.to_string(), "//depot/a.txt#5");
        path.set_revision(None);
        assert_eq!(path.to_string(), "//depot/a.txt");
    }

    #[test]
    fn a_git_path_is_not_a_local_file() {
        let path = UniversalPath::new(r"D:\repo\a.rs@git:main");
        assert_eq!(path.as_path(), None);
        assert!(!path.is_depot());
        assert!(!path.is_empty());
        assert!(!path.is_temp(Path::new(r"D:\repo")));
    }

    #[test]
    fn test_append_local_to_depot() {
        let mut base = UniversalPath::new("//stream/main");
        let addition = UniversalPath::new(r"folder\file.txt");
        base.append(addition);
        assert_eq!(
            base,
            UniversalPath::Depot(String::from("//stream/main/folder/file.txt"), None)
        );
    }
}
