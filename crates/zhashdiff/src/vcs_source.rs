use std::{
    collections::BTreeSet,
    fs::File,
    io::{self, Cursor, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize},
};

use zcommon::{
    universal_path::{GitRev, UniversalPath},
    vcs::git::{self, EntryKind, GitCommand, GitError, GitRunner, TreeEntry},
};

use crate::{
    comparison::FileReader,
    scan::{
        ContentHash, FlatEntry, FlatListing, HashKind, LocalDisk, ScanOutput, SkippedEntry, scan,
    },
};

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ContentSource {
    #[default]
    Disk,
    Git(GitContent),
}

/// Node paths stay under `model_root`, the side's `path@git:rev` text, so code that opens them
/// on disk fails instead of reading the working tree.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GitContent {
    pub model_root: PathBuf,
    pub repo_root: PathBuf,
    /// Relative to `repo_root`, `/`-separated, empty for the repository root.
    pub dir: String,
    /// `None` is the index.
    pub commit: Option<String>,
}

/// `None` when cancelled. A `path@git:<rev>` root is listed from git without a checkout; any
/// other root is scanned on disk.
pub fn scan_side(
    git: &impl GitRunner,
    root: &Path,
    cancel: &AtomicBool,
    found: &AtomicUsize,
) -> Option<io::Result<ScanOutput>> {
    let UniversalPath::Git(local, rev) = UniversalPath::from(root) else {
        return scan(&LocalDisk, root, cancel, found);
    };
    let (listing, content) = match list_git(git, root, &local, &rev) {
        Ok(listed) => listed,
        Err(e) => return Some(Err(io::Error::other(e))),
    };
    scan(&listing, root, cancel, found).map(|output| {
        output.map(|mut output| {
            output.model.set_content(ContentSource::Git(content));
            output
        })
    })
}

fn list_git(
    git: &impl GitRunner,
    model_root: &Path,
    local: &Path,
    rev: &GitRev,
) -> Result<(FlatListing, GitContent), GitError> {
    let location = git::locate(git, local)?;
    let mut skipped = Vec::new();
    let (commit, entries) = match rev {
        GitRev::Rev(rev) => {
            // Listing and reads use the commit id, so a branch that moves meanwhile can't mix them.
            let commit = git::resolve_commit(git, &location.root, rev)?;
            let entries = git::list_tree(git, &location.root, &commit, &location.rel)?;
            (Some(commit), entries)
        }
        GitRev::Index => {
            let mut entries = Vec::new();
            let mut conflicted = BTreeSet::new();
            for index_entry in git::list_index(git, &location.root, &location.rel)? {
                if index_entry.stage == 0 {
                    entries.push(index_entry.entry);
                } else {
                    conflicted.insert(index_entry.entry.path);
                }
            }
            skipped.extend(conflicted.into_iter().map(|path| SkippedEntry {
                path: join_rel(model_root, &path),
                reason: "conflicted in the index".to_string(),
            }));
            (None, entries)
        }
    };
    let mut files = Vec::with_capacity(entries.len());
    for TreeEntry {
        path,
        kind,
        id,
        size,
    } in entries
    {
        if kind == EntryKind::Submodule {
            skipped.push(SkippedEntry {
                path: join_rel(model_root, &path),
                reason: format!("submodule at commit {id}"),
            });
            continue;
        }
        files.push(FlatEntry {
            rel_path: path,
            size: size.expect("the git parsers give every blob a size"),
            hash: Some(ContentHash {
                kind: HashKind::GitBlob,
                value: id,
            }),
        });
    }
    let content = GitContent {
        model_root: model_root.to_path_buf(),
        repo_root: location.root,
        dir: location.rel,
        commit,
    };
    Ok((FlatListing::new(model_root, files, skipped), content))
}

fn join_rel(root: &Path, rel_path: &str) -> PathBuf {
    rel_path
        .split('/')
        .fold(root.to_path_buf(), |path, segment| path.join(segment))
}

/// The blob of a node of a git side, as stored (no checkout filters).
pub fn read_git_file(
    git: &impl GitRunner,
    content: &GitContent,
    path: &Path,
) -> io::Result<Vec<u8>> {
    let rel = path.strip_prefix(&content.model_root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} is not under {}",
                path.display(),
                content.model_root.display()
            ),
        )
    })?;
    let rel = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let repo_path = if content.dir.is_empty() {
        rel
    } else {
        format!("{}/{rel}", content.dir)
    };
    match &content.commit {
        Some(commit) => git::read_file(git, &content.repo_root, commit, &repo_path),
        None => git::read_index_file(git, &content.repo_root, &repo_path),
    }
    .map_err(io::Error::other)
}

impl FileReader for ContentSource {
    type File = Box<dyn Read>;
    fn open(&self, path: &Path) -> io::Result<Box<dyn Read>> {
        match self {
            ContentSource::Disk => Ok(Box::new(File::open(path)?)),
            ContentSource::Git(content) => Ok(Box::new(Cursor::new(read_git_file(
                &GitCommand::default(),
                content,
                path,
            )?))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{FileSystemModel, FsNode};
    use std::cell::RefCell;
    use zcommon::vcs::git::GitErrorKind;

    const COMMIT: &str = "1111111111111111111111111111111111111111";
    const BLOB_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BLOB_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const BLOB_C: &str = "cccccccccccccccccccccccccccccccccccccccc";

    struct FakeGit {
        prefix: &'static str,
        tree: String,
        index: String,
        calls: RefCell<Vec<String>>,
    }

    impl FakeGit {
        fn new(prefix: &'static str) -> Self {
            Self {
                prefix,
                tree: String::new(),
                index: String::new(),
                calls: RefCell::default(),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl GitRunner for FakeGit {
        fn run(&self, cwd: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
            self.calls.borrow_mut().push(args.join(" "));
            let out = match args[0] {
                "rev-parse" if args[1] == "--show-toplevel" => {
                    format!("{}\n{}\n", cwd.display(), self.prefix)
                }
                "rev-parse" if args[3] == "main^{commit}" => format!("{COMMIT}\n"),
                "ls-tree" => self.tree.clone(),
                "ls-files" => self.index.clone(),
                "cat-file" => format!("bytes of {}", args[3]),
                _ => {
                    return Err(GitError {
                        command: format!("git {}", args.join(" ")),
                        kind: GitErrorKind::Failed {
                            status: "exit code: 128".into(),
                            stderr: "fatal: bad revision".into(),
                        },
                    });
                }
            };
            Ok(out.into_bytes())
        }
    }

    fn scan_root(git: &FakeGit, root: &Path) -> io::Result<ScanOutput> {
        scan_side(git, root, &AtomicBool::new(false), &AtomicUsize::new(0)).unwrap()
    }

    fn node<'a>(model: &'a FileSystemModel, path: &Path) -> &'a FsNode {
        model.get_node(model.find_path(path).unwrap()).unwrap()
    }

    fn blob(value: &str) -> Option<ContentHash> {
        Some(ContentHash {
            kind: HashKind::GitBlob,
            value: value.into(),
        })
    }

    fn git_root(dir: &Path, rev: &str) -> PathBuf {
        PathBuf::from(format!("{}@git:{rev}", dir.display()))
    }

    #[test]
    fn a_revision_root_lists_its_tree_with_blob_ids() {
        let repo = tempfile::tempdir().unwrap();
        let mut git = FakeGit::new("");
        git.tree = [
            format!("100644 blob {BLOB_A}       5\ta.txt"),
            format!("100755 blob {BLOB_B}      12\tsrc/run.sh"),
            format!("160000 commit {BLOB_C}       -\tvendor/lib"),
        ]
        .map(|record| record + "\0")
        .concat();
        let root = git_root(repo.path(), "main");

        let output = scan_root(&git, &root).unwrap();
        let model = &output.model;

        FileSystemModel::assert_tree_consistency(model);
        assert_eq!(model.total_files(), 2);
        let a = node(model, &root.join("a.txt"));
        assert_eq!((a.size, &a.hash), (5, &blob(BLOB_A)));
        let run = node(model, &root.join("src").join("run.sh"));
        assert_eq!((run.size, &run.hash), (12, &blob(BLOB_B)));
        assert_eq!(output.skipped.len(), 1);
        assert_eq!(output.skipped[0].path, root.join("vendor").join("lib"));
        assert!(output.skipped[0].reason.contains("submodule"));
        assert_eq!(
            model.content(),
            &ContentSource::Git(GitContent {
                model_root: root.clone(),
                repo_root: repo.path().to_path_buf(),
                dir: String::new(),
                commit: Some(COMMIT.into()),
            })
        );
        assert_eq!(
            git.calls(),
            vec![
                "rev-parse --show-toplevel --show-prefix".to_string(),
                "rev-parse --verify --end-of-options main^{commit}".to_string(),
                format!("ls-tree -r -l -z --end-of-options {COMMIT}:"),
            ]
        );
    }

    #[test]
    fn a_subfolder_root_lists_the_tree_of_that_folder() {
        let repo = tempfile::tempdir().unwrap();
        let mut git = FakeGit::new("src/");
        git.tree = format!("100644 blob {BLOB_A}       5\tlib.rs\0");
        let root = git_root(repo.path(), "main");

        let output = scan_root(&git, &root).unwrap();

        assert!(git.calls()[2].ends_with(&format!("{COMMIT}:src")));
        assert_eq!(node(&output.model, &root.join("lib.rs")).hash, blob(BLOB_A));
        let ContentSource::Git(content) = output.model.content() else {
            panic!("not a git side");
        };
        assert_eq!(content.dir, "src");
    }

    #[test]
    fn an_index_root_lists_resolved_entries_and_skips_conflicts() {
        let repo = tempfile::tempdir().unwrap();
        let mut git = FakeGit::new("");
        git.index = [
            format!("100644 {BLOB_A} 0 5\ta.txt"),
            format!("100644 {BLOB_B} 1 6\tc.txt"),
            format!("100644 {BLOB_B} 2 6\tc.txt"),
            format!("100644 {BLOB_C} 3 7\tc.txt"),
        ]
        .map(|record| record + "\0")
        .concat();
        let root = git_root(repo.path(), "index");

        let output = scan_root(&git, &root).unwrap();
        let model = &output.model;

        assert_eq!(model.total_files(), 1);
        assert_eq!(node(model, &root.join("a.txt")).hash, blob(BLOB_A));
        assert_eq!(output.skipped.len(), 1);
        assert_eq!(output.skipped[0].path, root.join("c.txt"));
        assert!(output.skipped[0].reason.contains("conflicted"));
        let ContentSource::Git(content) = model.content() else {
            panic!("not a git side");
        };
        assert_eq!(content.commit, None);
    }

    #[test]
    fn a_bad_revision_is_an_error_naming_the_command() {
        let repo = tempfile::tempdir().unwrap();
        let git = FakeGit::new("");

        let error = scan_root(&git, &git_root(repo.path(), "nope")).unwrap_err();

        assert!(error.to_string().contains("nope^{commit}"));
        assert!(error.to_string().contains("bad revision"));
    }

    #[test]
    fn a_plain_root_is_scanned_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let git = FakeGit::new("");

        let output = scan_root(&git, dir.path()).unwrap();

        assert_eq!(output.model.total_files(), 1);
        assert_eq!(output.model.content(), &ContentSource::Disk);
        assert!(git.calls().is_empty());
    }

    fn content(commit: Option<&str>) -> GitContent {
        GitContent {
            model_root: PathBuf::from(r"D:\repo\src@git:origin/main"),
            repo_root: PathBuf::from("D:/repo"),
            dir: "src".into(),
            commit: commit.map(Into::into),
        }
    }

    #[test]
    fn a_node_is_read_at_the_listed_commit() {
        let git = FakeGit::new("");
        let content = content(Some(COMMIT));
        let path = content.model_root.join("a").join("b.rs");

        let bytes = read_git_file(&git, &content, &path).unwrap();

        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            format!("bytes of {COMMIT}:src/a/b.rs")
        );
    }

    #[test]
    fn an_index_node_is_read_from_the_index() {
        let git = FakeGit::new("");
        let content = content(None);
        let path = content.model_root.join("b.rs");

        let bytes = read_git_file(&git, &content, &path).unwrap();

        assert_eq!(String::from_utf8(bytes).unwrap(), "bytes of :src/b.rs");
    }

    #[test]
    fn a_path_outside_the_side_is_not_read() {
        let git = FakeGit::new("");

        let error = read_git_file(&git, &content(Some(COMMIT)), Path::new(r"D:\repo\src\b.rs"))
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(git.calls().is_empty());
    }
}
