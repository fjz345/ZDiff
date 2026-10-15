use std::{
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
};

use zcommon::universal_path::UniversalPath;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Folder1,
    Folder2,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Side::Folder1 => write!(f, "Folder 1"),
            Side::Folder2 => write!(f, "Folder 2"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum InvalidRoot {
    Missing(PathBuf),
    NotAFolder(PathBuf),
    Unreadable(PathBuf, String),
}

impl fmt::Display for InvalidRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidRoot::Missing(path) => write!(f, "{} does not exist", path.display()),
            InvalidRoot::NotAFolder(path) => write!(f, "{} is not a folder", path.display()),
            InvalidRoot::Unreadable(path, e) => write!(f, "{} can't be read: {e}", path.display()),
        }
    }
}

/// A `folder@git:<rev>` root is not checked on disk: the folder may exist only at that revision.
/// A `//depot/folder` root is kept as typed: `metadata` would look it up as a network share and
/// `absolute` would turn the `//` into `\\`.
pub fn validate_root(path: &Path) -> Result<PathBuf, InvalidRoot> {
    match UniversalPath::from(path) {
        UniversalPath::Depot(..) => return Ok(path.to_path_buf()),
        UniversalPath::Git(local, rev) => {
            // Only the folder goes through `absolute`, which would turn `origin/main` into `origin\main`.
            let local = std::path::absolute(&local)
                .map_err(|e| InvalidRoot::Unreadable(path.to_path_buf(), e.to_string()))?;
            return Ok(PathBuf::from(UniversalPath::Git(local, rev).to_string()));
        }
        UniversalPath::Local(_) => {}
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => std::path::absolute(path)
            .map_err(|e| InvalidRoot::Unreadable(path.to_path_buf(), e.to_string())),
        Ok(_) => Err(InvalidRoot::NotAFolder(path.to_path_buf())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(InvalidRoot::Missing(path.to_path_buf()))
        }
        Err(e) => Err(InvalidRoot::Unreadable(path.to_path_buf(), e.to_string())),
    }
}

/// The only way a side's root is set.
pub fn set_root(root: &mut Option<PathBuf>, side: Side, path: &Path) -> Result<(), InvalidRoot> {
    match validate_root(path) {
        Ok(path) => {
            *root = Some(path);
            Ok(())
        }
        Err(e) => {
            log::error!("{side}: {e}");
            Err(e)
        }
    }
}

pub fn clear_root(root: &mut Option<PathBuf>) {
    *root = None;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LaunchArgs {
    pub folders: Vec<PathBuf>,
    pub ignored: Vec<OsString>,
}

/// `args` excludes the program name.
pub fn parse_launch_args(args: impl IntoIterator<Item = OsString>) -> LaunchArgs {
    let mut parsed = LaunchArgs::default();
    for arg in args {
        if parsed.folders.len() < 2 {
            parsed.folders.push(arg.into());
        } else {
            parsed.ignored.push(arg);
        }
    }
    parsed
}

/// Any folder argument overrides both remembered roots, so one argument opens only Folder 1.
pub fn apply_launch_args(
    args: &LaunchArgs,
    root_1: &mut Option<PathBuf>,
    root_2: &mut Option<PathBuf>,
) {
    if !args.ignored.is_empty() {
        log::warn!("Ignoring extra command-line arguments: {:?}", args.ignored);
    }
    if args.folders.is_empty() {
        return;
    }
    let sides = [(root_1, Side::Folder1), (root_2, Side::Folder2)];
    for (i, (root, side)) in sides.into_iter().enumerate() {
        clear_root(root);
        if let Some(folder) = args.folders.get(i) {
            let _ = set_root(root, side, folder);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcommon::universal_path::GitRev;

    fn args(list: &[&Path]) -> LaunchArgs {
        parse_launch_args(list.iter().map(|p| p.as_os_str().to_owned()))
    }

    #[test]
    fn folder_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let mut root = None;
        assert_eq!(set_root(&mut root, Side::Folder1, dir.path()), Ok(()));
        assert_eq!(root.as_deref(), Some(dir.path()));
    }

    #[test]
    fn file_is_rejected_and_root_kept() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "a").unwrap();
        let mut root = Some(dir.path().to_path_buf());
        let err = set_root(&mut root, Side::Folder2, &file).unwrap_err();
        assert_eq!(err, InvalidRoot::NotAFolder(file.clone()));
        assert_eq!(
            err.to_string(),
            format!("{} is not a folder", file.display())
        );
        assert_eq!(root.as_deref(), Some(dir.path()));
    }

    #[test]
    fn missing_path_is_rejected_and_root_kept() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let mut root = None;
        let err = set_root(&mut root, Side::Folder1, &missing).unwrap_err();
        assert_eq!(err, InvalidRoot::Missing(missing.clone()));
        assert_eq!(
            err.to_string(),
            format!("{} does not exist", missing.display())
        );
        assert_eq!(root, None);
    }

    #[test]
    fn relative_folder_is_stored_absolute() {
        let mut root = None;
        set_root(&mut root, Side::Folder1, Path::new("src")).unwrap();
        let root = root.unwrap();
        assert!(root.is_absolute());
        assert!(root.ends_with("src"));
    }

    #[test]
    fn git_revision_folder_is_stored_absolute_with_its_revision() {
        let mut root = None;
        set_root(&mut root, Side::Folder1, Path::new("src@git:origin/main")).unwrap();
        let root = root.unwrap();
        let UniversalPath::Git(local, rev) = UniversalPath::from(&*root) else {
            panic!("not a git root: {}", root.display());
        };
        assert!(local.is_absolute());
        assert!(local.ends_with("src"));
        assert_eq!(rev, GitRev::Rev("origin/main".into()));
        assert!(root.to_str().unwrap().ends_with(r"\src@git:origin/main"));
    }

    #[test]
    fn git_revision_of_a_folder_missing_from_the_working_tree_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let text = format!("{}@git:index", dir.path().join("gone").display());
        let mut root = None;
        assert_eq!(set_root(&mut root, Side::Folder2, Path::new(&text)), Ok(()));
        assert_eq!(root, Some(PathBuf::from(text)));
    }

    #[test]
    fn depot_folders_are_accepted_as_typed() {
        for text in [
            "//depot/main",
            "//depot/main/sub",
            "//depot/main@1234",
            "//depot/main@=1234",
            "//depot",
        ] {
            let mut root = None;
            assert_eq!(
                set_root(&mut root, Side::Folder1, Path::new(text)),
                Ok(()),
                "{text}"
            );
            assert_eq!(root, Some(PathBuf::from(text)), "{text}");
        }
    }

    #[test]
    fn clear_root_empties_the_side() {
        let mut root = Some(PathBuf::from("x"));
        clear_root(&mut root);
        assert_eq!(root, None);
    }

    #[test]
    fn parse_zero_one_two_and_extra_arguments() {
        let (a, b, c, d) = (
            Path::new("a"),
            Path::new("b"),
            Path::new("c"),
            Path::new("d"),
        );
        assert_eq!(args(&[]), LaunchArgs::default());
        assert_eq!(args(&[a]).folders, vec![a.to_path_buf()]);
        assert_eq!(
            args(&[a, b]),
            LaunchArgs {
                folders: vec![a.to_path_buf(), b.to_path_buf()],
                ignored: vec![],
            }
        );
        assert_eq!(
            args(&[a, b, c, d]),
            LaunchArgs {
                folders: vec![a.to_path_buf(), b.to_path_buf()],
                ignored: vec![c.into(), d.into()],
            }
        );
    }

    #[test]
    fn no_arguments_keep_remembered_roots() {
        let (mut root_1, mut root_2) = (Some(PathBuf::from("r1")), Some(PathBuf::from("r2")));
        apply_launch_args(&args(&[]), &mut root_1, &mut root_2);
        assert_eq!(root_1, Some(PathBuf::from("r1")));
        assert_eq!(root_2, Some(PathBuf::from("r2")));
    }

    #[test]
    fn one_argument_opens_only_folder_1() {
        let dir = tempfile::tempdir().unwrap();
        let (mut root_1, mut root_2) = (Some(PathBuf::from("r1")), Some(PathBuf::from("r2")));
        apply_launch_args(&args(&[dir.path()]), &mut root_1, &mut root_2);
        assert_eq!(root_1.as_deref(), Some(dir.path()));
        assert_eq!(root_2, None);
    }

    #[test]
    fn two_arguments_override_both_roots() {
        let (dir_1, dir_2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (mut root_1, mut root_2) = (Some(PathBuf::from("r1")), Some(PathBuf::from("r2")));
        apply_launch_args(
            &args(&[dir_1.path(), dir_2.path()]),
            &mut root_1,
            &mut root_2,
        );
        assert_eq!(root_1.as_deref(), Some(dir_1.path()));
        assert_eq!(root_2.as_deref(), Some(dir_2.path()));
    }

    #[test]
    fn non_folder_argument_leaves_its_side_empty() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "a").unwrap();
        let (mut root_1, mut root_2) = (Some(PathBuf::from("r1")), Some(PathBuf::from("r2")));
        apply_launch_args(&args(&[&file, dir.path()]), &mut root_1, &mut root_2);
        assert_eq!(root_1, None);
        assert_eq!(root_2.as_deref(), Some(dir.path()));
    }

    #[test]
    fn extra_arguments_are_ignored() {
        let (dir_1, dir_2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (mut root_1, mut root_2) = (None, None);
        apply_launch_args(
            &args(&[dir_1.path(), dir_2.path(), Path::new("extra")]),
            &mut root_1,
            &mut root_2,
        );
        assert_eq!(root_1.as_deref(), Some(dir_1.path()));
        assert_eq!(root_2.as_deref(), Some(dir_2.path()));
    }
}
