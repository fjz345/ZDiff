use std::{
    collections::{BTreeSet, HashMap},
    fs::File,
    io::{self, Cursor, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize},
};

use zcommon::{
    universal_path::{DepotRev, GitRev, UniversalPath},
    vcs::{
        git::{self, EntryKind, GitCommand, GitError, GitRunner, TreeEntry},
        p4::{self, P4Command, P4Runner, P4ShelvedFile},
    },
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
    Depot(DepotContent),
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

/// Node paths stay under `model_root` as for `GitContent`.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DepotContent {
    pub model_root: PathBuf,
    /// Without a trailing `/`.
    pub depot_folder: String,
    pub revisions: DepotRevisions,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DepotRevisions {
    /// Each listed file's revision by `/`-separated path relative to the folder, so a read gets
    /// the revision that was compared even if the depot moved on.
    Files(HashMap<String, u32>),
    Shelf(u32),
}

/// `None` when cancelled. Git and depot roots are listed without a checkout or sync.
pub fn scan_side(
    git: &impl GitRunner,
    p4: &impl P4Runner,
    root: &Path,
    cancel: &AtomicBool,
    found: &AtomicUsize,
) -> Option<io::Result<ScanOutput>> {
    let listed = match UniversalPath::from(root) {
        UniversalPath::Local(_) => return scan(&LocalDisk, root, cancel, found),
        UniversalPath::Git(local, rev) => list_git(git, root, &local, &rev)
            .map(|(listing, content)| (listing, ContentSource::Git(content)))
            .map_err(io::Error::other),
        UniversalPath::Depot(folder, rev) => list_depot(p4, root, &folder, rev)
            .map(|(listing, content)| (listing, ContentSource::Depot(content)))
            .map_err(io::Error::other),
    };
    let (listing, content) = match listed {
        Ok(listed) => listed,
        Err(e) => return Some(Err(e)),
    };
    scan(&listing, root, cancel, found).map(|output| {
        output.map(|mut output| {
            output.model.set_content(content);
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

type DepotFiles = (Vec<FlatEntry>, Vec<SkippedEntry>, DepotRevisions);

fn list_depot(
    p4: &impl P4Runner,
    model_root: &Path,
    folder: &str,
    rev: Option<DepotRev>,
) -> Result<(FlatListing, DepotContent), String> {
    let folder = folder.trim_end_matches('/');
    let (files, skipped, revisions) = match rev {
        None => list_depot_files(p4, model_root, folder, "")?,
        Some(DepotRev::Change(change)) => {
            list_depot_files(p4, model_root, folder, &format!("@{change}"))?
        }
        Some(DepotRev::Shelf(change)) => list_shelf_files(p4, model_root, folder, change)?,
        Some(DepotRev::Rev(rev)) => {
            return Err(format!(
                "{}: a folder has no revision #{rev}; use @<changelist>",
                model_root.display()
            ));
        }
    };
    let content = DepotContent {
        model_root: model_root.to_path_buf(),
        depot_folder: folder.to_owned(),
        revisions,
    };
    Ok((FlatListing::new(model_root, files, skipped), content))
}

fn list_depot_files(
    p4: &impl P4Runner,
    model_root: &Path,
    folder: &str,
    revision: &str,
) -> Result<DepotFiles, String> {
    let prefix = format!("{folder}/");
    let (mut files, mut skipped, mut revisions) = (Vec::new(), Vec::new(), HashMap::new());
    for file in p4::list_files_with_digests(p4, folder, revision)? {
        let Some(rel) = file.depot_file.strip_prefix(&prefix) else {
            skipped.push(SkippedEntry {
                path: PathBuf::from(&file.depot_file),
                reason: format!("outside {folder}"),
            });
            continue;
        };
        let Some(size) = file.size else {
            skipped.push(SkippedEntry {
                path: join_rel(model_root, rel),
                reason: "p4 reports no size (archived?)".to_string(),
            });
            continue;
        };
        revisions.insert(rel.to_owned(), file.rev);
        files.push(FlatEntry {
            rel_path: rel.to_owned(),
            size,
            hash: p4_hash(file.digest),
        });
    }
    Ok((files, skipped, DepotRevisions::Files(revisions)))
}

/// Deleted files are not on the shelf side; they are on the base side.
fn list_shelf_files(
    p4: &impl P4Runner,
    model_root: &Path,
    folder: &str,
    change: u32,
) -> Result<DepotFiles, String> {
    let prefix = format!("{folder}/");
    let (mut files, mut skipped) = (Vec::new(), Vec::new());
    for file in shelf_files_under(p4::list_shelf(p4, change)?, &prefix) {
        let rel = &file.depot_file[prefix.len()..];
        if matches!(file.action.as_str(), "delete" | "move/delete") {
            continue;
        }
        let Some(size) = file.size else {
            skipped.push(SkippedEntry {
                path: join_rel(model_root, rel),
                reason: "p4 reports no size for the shelved file".to_string(),
            });
            continue;
        };
        files.push(FlatEntry {
            rel_path: rel.to_owned(),
            size,
            hash: p4_hash(file.digest),
        });
    }
    Ok((files, skipped, DepotRevisions::Shelf(change)))
}

fn shelf_files_under(
    files: Vec<P4ShelvedFile>,
    prefix: &str,
) -> impl Iterator<Item = P4ShelvedFile> {
    let prefix = prefix.to_owned();
    files
        .into_iter()
        .filter(move |file| file.depot_file.starts_with(&prefix))
}

fn p4_hash(digest: Option<String>) -> Option<ContentHash> {
    digest.map(|value| ContentHash {
        kind: HashKind::P4Digest,
        value,
    })
}

/// The shelved files of a `//depot/folder@=<shelf>` root at the revisions they were shelved
/// from, deleted ones included, new ones left out: the other side of a shelf review.
pub fn shelf_base_paths(p4: &impl P4Runner, root: &Path) -> Result<Vec<UniversalPath>, String> {
    let UniversalPath::Depot(folder, Some(DepotRev::Shelf(change))) = UniversalPath::from(root)
    else {
        return Err(format!(
            "{} is not a shelf folder (//depot/folder@=<shelf>)",
            root.display()
        ));
    };
    let prefix = format!("{}/", folder.trim_end_matches('/'));
    Ok(shelf_files_under(p4::list_shelf(p4, change)?, &prefix)
        .filter_map(|file| {
            let base = file.base_rev?;
            Some(UniversalPath::Depot(
                file.depot_file,
                Some(DepotRev::Rev(base)),
            ))
        })
        .collect())
}

fn join_rel(root: &Path, rel_path: &str) -> PathBuf {
    rel_path
        .split('/')
        .fold(root.to_path_buf(), |path, segment| path.join(segment))
}

/// `path` relative to `model_root`, `/`-separated.
fn rel_under(model_root: &Path, path: &Path) -> io::Result<String> {
    let rel = path.strip_prefix(model_root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not under {}", path.display(), model_root.display()),
        )
    })?;
    Ok(rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

/// The depot path and revision of a node of a depot side, as it was listed.
pub fn depot_node(content: &DepotContent, path: &Path) -> io::Result<(String, DepotRev)> {
    let rel = rel_under(&content.model_root, path)?;
    let revision = match &content.revisions {
        DepotRevisions::Files(revisions) => {
            let rev = revisions.get(&rel).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{rel} was not listed in {}", content.depot_folder),
                )
            })?;
            DepotRev::Rev(*rev)
        }
        DepotRevisions::Shelf(change) => DepotRev::Shelf(*change),
    };
    Ok((format!("{}/{rel}", content.depot_folder), revision))
}

/// The file of a node of a depot side at the revision it was listed with.
pub fn read_depot_file(
    p4: &impl P4Runner,
    content: &DepotContent,
    path: &Path,
) -> io::Result<Vec<u8>> {
    let (depot_path, revision) = depot_node(content, path)?;
    p4::print_file(p4, &depot_path, &revision.to_string()).map_err(io::Error::other)
}

/// The blob of a node of a git side, as stored (no checkout filters).
pub fn read_git_file(
    git: &impl GitRunner,
    content: &GitContent,
    path: &Path,
) -> io::Result<Vec<u8>> {
    let rel = rel_under(&content.model_root, path)?;
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
            ContentSource::Depot(content) => {
                let p4 = P4Command::new(false)
                    .for_file(UniversalPath::Depot(content.depot_folder.clone(), None));
                Ok(Box::new(Cursor::new(read_depot_file(&p4, content, path)?)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{FileSystemModel, FsNode};
    use std::cell::RefCell;
    use zcommon::vcs::{git::GitErrorKind, p4::FakeP4};

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
        let no_p4 = FakeP4::new(|args: &[&str]| panic!("p4 must not run: {args:?}"));
        scan_side(
            git,
            &no_p4,
            root,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )
        .unwrap()
    }

    fn scan_depot(p4: &impl P4Runner, root: impl AsRef<Path>) -> io::Result<ScanOutput> {
        let no_git = FakeGit::new("");
        scan_side(
            &no_git,
            p4,
            root.as_ref(),
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )
        .unwrap()
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

    const DIGEST_A: &str = "0CC175B9C0F1B6A831C399E269772661";
    const DIGEST_B: &str = "92EB5FFEE6AE2FEC3AD71C777531578F";

    const FSTAT: &str = "... depotFile //depot/main/a.txt\n\
                         ... headAction edit\n\
                         ... headRev 4\n\
                         ... fileSize 5\n\
                         ... digest 0CC175B9C0F1B6A831C399E269772661\n\
                         \n\
                         ... depotFile //depot/main/src/run.sh\n\
                         ... headAction add\n\
                         ... headRev 1\n\
                         ... fileSize 12\n\
                         ... digest 92EB5FFEE6AE2FEC3AD71C777531578F\n\
                         \n\
                         ... depotFile //depot/main/gone.txt\n\
                         ... headAction delete\n\
                         ... headRev 3\n\
                         \n\
                         ... depotFile //depot/main/src/moved.txt\n\
                         ... headAction move/delete\n\
                         ... headRev 2\n\
                         \n\
                         ... depotFile //depot/main/archived.bin\n\
                         ... headAction archive\n\
                         ... headRev 9\n\
                         \n\
                         ... depotFile //depot/mainline/x.txt\n\
                         ... headAction edit\n\
                         ... headRev 1\n\
                         ... fileSize 1\n\
                         \n";

    fn digest(value: &str) -> Option<ContentHash> {
        Some(ContentHash {
            kind: HashKind::P4Digest,
            value: value.into(),
        })
    }

    fn fstat_p4() -> FakeP4<impl Fn(&[&str]) -> Result<String, String>> {
        FakeP4::new(|args: &[&str]| match args[1] {
            "fstat" => Ok(FSTAT.to_string()),
            _ => panic!("unexpected {args:?}"),
        })
    }

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn a_depot_folder_lists_its_files_at_head_with_digests() {
        let p4 = fstat_p4();
        let root = PathBuf::from("//depot/main");

        let output = scan_depot(&p4, &root).unwrap();
        let model = &output.model;

        FileSystemModel::assert_tree_consistency(model);
        assert_eq!(model.total_files(), 2);
        let a = node(model, &root.join("a.txt"));
        assert_eq!((a.size, &a.hash), (5, &digest(DIGEST_A)));
        let run = node(model, &root.join("src").join("run.sh"));
        assert_eq!((run.size, &run.hash), (12, &digest(DIGEST_B)));
        assert!(model.find_path(root.join("gone.txt")).is_none());
        assert!(
            model
                .find_path(root.join("src").join("moved.txt"))
                .is_none()
        );
        assert_eq!(
            model.content(),
            &ContentSource::Depot(DepotContent {
                model_root: root.clone(),
                depot_folder: "//depot/main".into(),
                revisions: DepotRevisions::Files(
                    [("a.txt".to_string(), 4), ("src/run.sh".to_string(), 1)].into()
                ),
            })
        );
        assert_eq!(
            p4.calls(),
            vec![args(&["-ztag", "fstat", "-Ol", "//depot/main/..."])]
        );
    }

    #[test]
    fn files_without_a_size_or_outside_the_folder_are_skipped_with_a_reason() {
        let root = PathBuf::from("//depot/main");

        let output = scan_depot(&fstat_p4(), &root).unwrap();

        let mut skipped: Vec<_> = output
            .skipped
            .iter()
            .map(|entry| (entry.path.clone(), entry.reason.as_str()))
            .collect();
        skipped.sort();
        assert_eq!(skipped.len(), 2);
        assert_eq!(skipped[0].0, root.join("archived.bin"));
        assert!(skipped[0].1.contains("size"), "{}", skipped[0].1);
        assert_eq!(skipped[1].0, PathBuf::from("//depot/mainline/x.txt"));
        assert!(skipped[1].1.contains("outside"), "{}", skipped[1].1);
    }

    #[test]
    fn a_depot_folder_at_a_changelist_lists_that_change() {
        let p4 = fstat_p4();
        let root = PathBuf::from("//depot/main@1234");

        let output = scan_depot(&p4, &root).unwrap();

        assert!(output.model.find_path(root.join("a.txt")).is_some());
        assert_eq!(
            p4.calls(),
            vec![args(&["-ztag", "fstat", "-Ol", "//depot/main/...@1234"])]
        );
    }

    #[test]
    fn a_depot_root_at_the_server_level_keeps_its_node_paths() {
        let p4 = FakeP4::new(|_: &[&str]| {
            Ok(
                "... depotFile //depot/a.txt\n... headAction edit\n... headRev 2\n... fileSize 5\n"
                    .to_string(),
            )
        });
        let root = PathBuf::from("//depot");

        let output = scan_depot(&p4, &root).unwrap();

        let a = node(&output.model, &root.join("a.txt"));
        assert_eq!(a.size, 5);
        assert_eq!(
            p4.calls(),
            vec![args(&["-ztag", "fstat", "-Ol", "//depot/..."])]
        );
    }

    #[test]
    fn a_revision_number_on_a_folder_is_an_error() {
        let error = scan_depot(&fstat_p4(), "//depot/main#3").unwrap_err();

        assert!(error.to_string().contains("#3"), "{error}");
    }

    #[test]
    fn a_p4_failure_is_the_scan_error_naming_the_command() {
        let p4 = FakeP4::new(|_: &[&str]| Err("Perforce client error: connect failed".to_string()));

        let error = scan_depot(&p4, "//depot/main").unwrap_err();

        assert!(error.to_string().contains("fstat"), "{error}");
        assert!(error.to_string().contains("connect failed"), "{error}");
    }

    fn describe_output(files: &[(&str, &str, u32, Option<u64>)]) -> String {
        let mut out =
            String::from("... change 1234\n... desc a shelf\n\nwith a blank line\n... shelved \n");
        for (i, (path, action, rev, size)) in files.iter().enumerate() {
            out += &format!("... depotFile{i} {path}\n... action{i} {action}\n... rev{i} {rev}\n");
            if let Some(size) = size {
                out += &format!("... fileSize{i} {size}\n... digest{i} {DIGEST_A}\n");
            }
        }
        out
    }

    const SHELVED: &[(&str, &str, u32, Option<u64>)] = &[
        ("//depot/main/a.txt", "edit", 4, Some(120)),
        ("//depot/main/new.txt", "add", 1, Some(7)),
        ("//depot/main/old.txt", "delete", 7, None),
        ("//depot/main/src/moved.txt", "move/delete", 3, None),
        ("//depot/main/src/c.txt", "edit", 5, Some(9)),
        ("//depot/main/nosize.txt", "edit", 2, None),
        ("//depot/other/z.txt", "edit", 6, Some(3)),
    ];

    fn shelf_p4() -> FakeP4<impl Fn(&[&str]) -> Result<String, String>> {
        FakeP4::new(|args: &[&str]| match args[1] {
            "describe" => Ok(describe_output(SHELVED)),
            _ => panic!("unexpected {args:?}"),
        })
    }

    #[test]
    fn a_shelf_lists_only_its_shelved_files_under_the_folder() {
        let p4 = shelf_p4();
        let root = PathBuf::from("//depot/main@=1234");

        let output = scan_depot(&p4, &root).unwrap();
        let model = &output.model;

        FileSystemModel::assert_tree_consistency(model);
        assert_eq!(model.total_files(), 3);
        let a = node(model, &root.join("a.txt"));
        assert_eq!((a.size, &a.hash), (120, &digest(DIGEST_A)));
        assert!(model.find_path(root.join("new.txt")).is_some());
        assert!(model.find_path(root.join("src").join("c.txt")).is_some());
        for absent in ["old.txt", "nosize.txt"] {
            assert!(model.find_path(root.join(absent)).is_none(), "{absent}");
        }
        assert!(
            model
                .find_path(root.join("src").join("moved.txt"))
                .is_none()
        );
        assert_eq!(output.skipped.len(), 1);
        assert_eq!(output.skipped[0].path, root.join("nosize.txt"));
        assert_eq!(
            model.content(),
            &ContentSource::Depot(DepotContent {
                model_root: root.clone(),
                depot_folder: "//depot/main".into(),
                revisions: DepotRevisions::Shelf(1234),
            })
        );
        assert_eq!(
            p4.calls(),
            vec![args(&["-ztag", "describe", "-S", "-s", "1234"])]
        );
    }

    #[test]
    fn a_shelf_gives_the_base_revision_paths_for_the_other_side() {
        let base = shelf_base_paths(&shelf_p4(), Path::new("//depot/main@=1234")).unwrap();

        let rev = |path: &str, rev| UniversalPath::Depot(path.into(), Some(DepotRev::Rev(rev)));
        assert_eq!(
            base,
            vec![
                rev("//depot/main/a.txt", 4),
                rev("//depot/main/old.txt", 7),
                rev("//depot/main/src/moved.txt", 3),
                rev("//depot/main/src/c.txt", 5),
                rev("//depot/main/nosize.txt", 2),
            ]
        );
    }

    #[test]
    fn base_revision_paths_need_a_shelf_root() {
        let p4 = shelf_p4();

        for root in ["//depot/main", "//depot/main@12", r"D:\repo@git:main"] {
            assert!(shelf_base_paths(&p4, Path::new(root)).is_err(), "{root}");
        }
        assert!(p4.calls().is_empty());
    }

    fn depot_content(revisions: DepotRevisions) -> DepotContent {
        DepotContent {
            model_root: PathBuf::from("//depot/main"),
            depot_folder: "//depot/main".into(),
            revisions,
        }
    }

    #[test]
    fn a_depot_node_is_read_at_its_listed_revision() {
        let p4 = FakeP4::new(|_: &[&str]| Ok("bytes".to_string()));
        let content = depot_content(DepotRevisions::Files(
            [("src/run.sh".to_string(), 3)].into(),
        ));
        let path = content.model_root.join("src").join("run.sh");

        let bytes = read_depot_file(&p4, &content, &path).unwrap();

        assert_eq!(bytes, b"bytes");
        assert_eq!(
            p4.calls(),
            vec![args(&["print", "-q", "//depot/main/src/run.sh#3"])]
        );
    }

    #[test]
    fn a_shelf_node_is_read_from_the_shelf() {
        let p4 = FakeP4::new(|_: &[&str]| Ok("bytes".to_string()));
        let content = depot_content(DepotRevisions::Shelf(1234));
        let path = content.model_root.join("a.txt");

        read_depot_file(&p4, &content, &path).unwrap();

        assert_eq!(
            p4.calls(),
            vec![args(&["print", "-q", "//depot/main/a.txt@=1234"])]
        );
    }

    #[test]
    fn an_unlisted_or_outside_depot_path_is_not_read() {
        let p4 = FakeP4::new(|args: &[&str]| panic!("p4 must not run: {args:?}"));
        let content = depot_content(DepotRevisions::Files(HashMap::new()));

        for path in ["//depot/main/new.txt", "//depot/other/new.txt"] {
            let error = read_depot_file(&p4, &content, Path::new(path)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::NotFound, "{path}");
        }
    }
}
