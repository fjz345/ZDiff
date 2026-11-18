use std::{
    fmt,
    path::{Path, PathBuf},
};

use zcommon::universal_path::UniversalPath;

use crate::folder::status_line::ToolMessage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Folder1,
    Folder2,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Side::Folder1 => write!(f, "Left"),
            Side::Folder2 => write!(f, "Right"),
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

pub fn set_root_or_report(
    root: &mut Option<PathBuf>,
    side: Side,
    path: &Path,
    message: &mut Option<ToolMessage>,
) {
    *message = set_root(root, side, path)
        .err()
        .map(|e| error_message(format!("{side}: {e}")));
}

fn error_message(error: String) -> ToolMessage {
    ToolMessage {
        error: Some(error),
        ..ToolMessage::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcommon::universal_path::GitRev;

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
    fn a_rejected_root_is_reported_until_a_root_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let mut root = Some(dir.path().to_path_buf());
        let mut message = None;
        set_root_or_report(&mut root, Side::Folder2, &missing, &mut message);
        let error = format!("Right: {}", InvalidRoot::Missing(missing));
        assert_eq!(message.as_ref().and_then(|m| m.error.clone()), Some(error));
        assert_eq!(root.as_deref(), Some(dir.path()));
        set_root_or_report(&mut root, Side::Folder2, dir.path(), &mut message);
        assert_eq!(message, None);
    }
}
