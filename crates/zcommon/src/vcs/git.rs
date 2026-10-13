use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq)]
pub enum GitErrorKind {
    NotFound { exe: String },
    Spawn(String),
    Failed { status: String, stderr: String },
    Parse(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitError {
    pub command: String,
    pub kind: GitErrorKind,
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let command = &self.command;
        match &self.kind {
            GitErrorKind::NotFound { exe } => write!(
                f,
                "git not found: `{exe}` is not on PATH, install git ({command})"
            ),
            GitErrorKind::Spawn(e) => write!(f, "{command}: {e}"),
            GitErrorKind::Failed { status, stderr } if stderr.trim().is_empty() => {
                write!(f, "{command} failed ({status})")
            }
            GitErrorKind::Failed { status, stderr } => {
                write!(f, "{command} failed ({status}): {}", stderr.trim())
            }
            GitErrorKind::Parse(e) => write!(f, "{command}: unexpected output: {e}"),
        }
    }
}

impl std::error::Error for GitError {}

fn command_line(cwd: &Path, args: &[&str]) -> String {
    format!("git {} (in {})", args.join(" "), cwd.display())
}

/// Runs git in `cwd`: stdout on success.
pub trait GitRunner {
    fn run(&self, cwd: &Path, args: &[&str]) -> Result<Vec<u8>, GitError>;
}

pub struct GitCommand {
    pub exe: PathBuf,
}

impl Default for GitCommand {
    fn default() -> Self {
        Self {
            exe: PathBuf::from("git"),
        }
    }
}

impl GitRunner for GitCommand {
    fn run(&self, cwd: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
        let error = |kind| GitError {
            command: command_line(cwd, args),
            kind,
        };
        let mut cmd = Command::new(&self.exe);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console flashing up per spawn from the GUI apps.
            cmd.creation_flags(0x0800_0000);
        }
        let output = cmd.output().map_err(|e| match e.kind() {
            // A missing cwd also reads as NotFound on some platforms.
            io::ErrorKind::NotFound if cwd.is_dir() => error(GitErrorKind::NotFound {
                exe: self.exe.display().to_string(),
            }),
            _ => error(GitErrorKind::Spawn(format!("{}: {e}", self.exe.display()))),
        })?;
        if !output.status.success() {
            return Err(error(GitErrorKind::Failed {
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            }));
        }
        Ok(output.stdout)
    }
}

fn run_parsed<T>(
    git: &impl GitRunner,
    cwd: &Path,
    args: &[&str],
    parse: impl FnOnce(&str) -> Result<T, String>,
) -> Result<T, GitError> {
    let out = git.run(cwd, args)?;
    let parse_error = |e| GitError {
        command: command_line(cwd, args),
        kind: GitErrorKind::Parse(e),
    };
    let text = std::str::from_utf8(&out).map_err(|e| parse_error(format!("not UTF-8: {e}")))?;
    parse(text).map_err(parse_error)
}

fn is_object_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn parse_records<T>(
    out: &str,
    parse: impl Fn(&str) -> Result<T, String>,
) -> Result<Vec<T>, String> {
    out.split_terminator('\0')
        .enumerate()
        .map(|(i, record)| parse(record).map_err(|e| format!("record {}: {e}: {record:?}", i + 1)))
        .collect()
}

/// Where a path lives in its repository. `rel` is relative to `root`, `/`-separated, empty for
/// the root itself.
#[derive(Debug, Clone, PartialEq)]
pub struct GitLocation {
    pub root: PathBuf,
    pub rel: String,
}

/// `git rev-parse --show-toplevel --show-prefix`; the prefix line is empty at the root.
pub fn parse_location(out: &str) -> Result<(PathBuf, String), String> {
    let mut lines = out.lines();
    let root = lines
        .next()
        .filter(|line| !line.is_empty())
        .ok_or("no top-level folder line")?;
    let prefix = lines
        .next()
        .ok_or_else(|| format!("no prefix line after {root:?}"))?;
    if let Some(extra) = lines.next() {
        return Err(format!("line 3: unexpected {extra:?}"));
    }
    Ok((
        PathBuf::from(root),
        prefix.trim_end_matches('/').to_string(),
    ))
}

/// The path need not exist: git runs in its nearest existing ancestor folder, and the missing
/// components are appended to the prefix.
pub fn locate(git: &impl GitRunner, path: &Path) -> Result<GitLocation, GitError> {
    let args = ["rev-parse", "--show-toplevel", "--show-prefix"];
    let mut dir = path;
    let mut missing = Vec::new();
    while !dir.is_dir() {
        let name = dir.file_name().and_then(|name| name.to_str());
        let (Some(name), Some(parent)) = (name, dir.parent()) else {
            return Err(GitError {
                command: command_line(path, &args),
                kind: GitErrorKind::Spawn(format!("{} has no existing folder", path.display())),
            });
        };
        missing.push(name);
        dir = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
    }
    let (root, prefix) = run_parsed(git, dir, &args, parse_location)?;
    let rel = std::iter::once(prefix.as_str())
        .filter(|prefix| !prefix.is_empty())
        .chain(missing.into_iter().rev())
        .collect::<Vec<_>>()
        .join("/");
    Ok(GitLocation { root, rel })
}

pub fn parse_commit_id(out: &str) -> Result<String, String> {
    let mut lines = out.lines();
    match (lines.next(), lines.next()) {
        (Some(id), None) if is_object_id(id) => Ok(id.to_string()),
        (Some(id), None) => Err(format!("not a commit id: {id:?}")),
        (None, _) => Err("no commit id".to_string()),
        (Some(_), Some(extra)) => Err(format!("line 2: unexpected {extra:?}")),
    }
}

/// The commit a revision (ref, commit or expression) names; a tag is peeled to its commit.
pub fn resolve_commit(git: &impl GitRunner, root: &Path, rev: &str) -> Result<String, GitError> {
    let rev = format!("{rev}^{{commit}}");
    let args = ["rev-parse", "--verify", "--end-of-options", rev.as_str()];
    run_parsed(git, root, &args, parse_commit_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Branch,
    RemoteBranch,
    Tag,
}

/// `name` is the short name (`main`, `origin/main`, `v1.2`); a branch and a tag can share it.
#[derive(Debug, Clone, PartialEq)]
pub struct GitRef {
    pub kind: RefKind,
    pub name: String,
    pub full_name: String,
}

/// `git for-each-ref --format=%(refname) %(symref)`. Symbolic refs (`origin/HEAD`) are dropped.
pub fn parse_refs(out: &str) -> Result<Vec<GitRef>, String> {
    let mut refs = Vec::new();
    for (i, line) in out.lines().enumerate() {
        let parsed = parse_ref(line).map_err(|e| format!("line {}: {e}: {line:?}", i + 1))?;
        refs.extend(parsed);
    }
    Ok(refs)
}

fn parse_ref(line: &str) -> Result<Option<GitRef>, String> {
    let (full_name, symref) = line.split_once(' ').ok_or("expected `refname symref`")?;
    if !symref.is_empty() {
        return Ok(None);
    }
    let (kind, name) = [
        ("refs/heads/", RefKind::Branch),
        ("refs/remotes/", RefKind::RemoteBranch),
        ("refs/tags/", RefKind::Tag),
    ]
    .into_iter()
    .find_map(|(prefix, kind)| full_name.strip_prefix(prefix).map(|name| (kind, name)))
    .ok_or("not a branch, remote branch or tag")?;
    if name.is_empty() {
        return Err("empty name".to_string());
    }
    Ok(Some(GitRef {
        kind,
        name: name.to_string(),
        full_name: full_name.to_string(),
    }))
}

/// Local branches, remote-tracking branches and tags, in that order.
pub fn list_refs(git: &impl GitRunner, root: &Path) -> Result<Vec<GitRef>, GitError> {
    let args = [
        "for-each-ref",
        "--format=%(refname) %(symref)",
        "refs/heads",
        "refs/remotes",
        "refs/tags",
    ];
    run_parsed(git, root, &args, parse_refs)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Executable,
    Symlink,
    Submodule,
}

/// `id` is the blob id (the commit id for a submodule, which has no size).
#[derive(Debug, Clone, PartialEq)]
pub struct TreeEntry {
    pub path: String,
    pub kind: EntryKind,
    pub id: String,
    pub size: Option<u64>,
}

/// `git ls-tree -r -l -z`.
pub fn parse_tree(out: &str) -> Result<Vec<TreeEntry>, String> {
    parse_records(out, |record| {
        let (meta, path) = record.split_once('\t').ok_or("no tab before the path")?;
        let fields: Vec<&str> = meta.split_whitespace().collect();
        let [mode, object_type, id, size] = fields[..] else {
            return Err("expected `mode type id size`".to_string());
        };
        let kind = entry_kind(mode)?;
        let expected_type = if kind == EntryKind::Submodule {
            "commit"
        } else {
            "blob"
        };
        if object_type != expected_type {
            return Err(format!("mode {mode} with type {object_type}"));
        }
        entry(path, kind, id, size)
    })
}

fn entry_kind(mode: &str) -> Result<EntryKind, String> {
    match mode {
        "100644" => Ok(EntryKind::File),
        "100755" => Ok(EntryKind::Executable),
        "120000" => Ok(EntryKind::Symlink),
        "160000" => Ok(EntryKind::Submodule),
        _ => Err(format!("unexpected mode {mode}")),
    }
}

fn entry(path: &str, kind: EntryKind, id: &str, size: &str) -> Result<TreeEntry, String> {
    if !is_object_id(id) {
        return Err(format!("not an object id: {id:?}"));
    }
    let size = match (kind, size) {
        (EntryKind::Submodule, "-") => None,
        (EntryKind::Submodule, _) => return Err(format!("submodule with size {size:?}")),
        _ => Some(size.parse().map_err(|_| format!("not a size: {size:?}"))?),
    };
    Ok(TreeEntry {
        path: path.to_string(),
        kind,
        id: id.to_string(),
        size,
    })
}

/// Every file under `dir` (relative to `root`, `/`-separated) at `rev`, with paths relative to
/// `dir`.
pub fn list_tree(
    git: &impl GitRunner,
    root: &Path,
    rev: &str,
    dir: &str,
) -> Result<Vec<TreeEntry>, GitError> {
    let tree = format!("{rev}:{dir}");
    let args = [
        "ls-tree",
        "-r",
        "-l",
        "-z",
        "--end-of-options",
        tree.as_str(),
    ];
    run_parsed(git, root, &args, parse_tree)
}

/// Stage is 0 for a resolved entry; a conflicted path has one entry per stage 1..=3.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexEntry {
    pub entry: TreeEntry,
    pub stage: u8,
}

pub const INDEX_FORMAT: &str =
    "--format=%(objectmode) %(objectname) %(stage) %(objectsize)%x09%(path)";

/// `git ls-files -z` with [`INDEX_FORMAT`].
pub fn parse_index(out: &str) -> Result<Vec<IndexEntry>, String> {
    parse_records(out, |record| {
        let (meta, path) = record.split_once('\t').ok_or("no tab before the path")?;
        let fields: Vec<&str> = meta.split_whitespace().collect();
        let [mode, id, stage, size] = fields[..] else {
            return Err("expected `mode id stage size`".to_string());
        };
        let stage = match stage.parse() {
            Ok(stage @ 0..=3) => stage,
            _ => return Err(format!("not a stage: {stage:?}")),
        };
        Ok(IndexEntry {
            entry: entry(path, entry_kind(mode)?, id, size)?,
            stage,
        })
    })
}

/// Every index entry under `dir`, with paths relative to `dir`. Needs git 2.38 (`--format`).
pub fn list_index(
    git: &impl GitRunner,
    root: &Path,
    dir: &str,
) -> Result<Vec<IndexEntry>, GitError> {
    let pathspec = format!(":(literal){dir}");
    let mut args = vec!["ls-files", "-z", INDEX_FORMAT];
    if !dir.is_empty() {
        args.extend(["--", pathspec.as_str()]);
    }
    run_parsed(git, root, &args, |out| {
        let mut entries = parse_index(out)?;
        if !dir.is_empty() {
            for index_entry in &mut entries {
                let path = &mut index_entry.entry.path;
                let rel = path
                    .strip_prefix(dir)
                    .and_then(|rest| rest.strip_prefix('/'))
                    .ok_or_else(|| format!("{path:?} is not under {dir:?}"))?;
                *path = rel.to_string();
            }
        }
        Ok(entries)
    })
}

/// The file as a checkout would write it: checkout filters (line endings, LFS) applied.
pub fn read_file(
    git: &impl GitRunner,
    root: &Path,
    rev: &str,
    path: &str,
) -> Result<Vec<u8>, GitError> {
    let object = format!("{rev}:{path}");
    git.run(
        root,
        &["cat-file", "--filters", "--end-of-options", object.as_str()],
    )
}

/// The stage 0 file, as [`read_file`].
pub fn read_index_file(git: &impl GitRunner, root: &Path, path: &str) -> Result<Vec<u8>, GitError> {
    let object = format!(":{path}");
    git.run(
        root,
        &["cat-file", "--filters", "--end-of-options", object.as_str()],
    )
}

#[cfg(test)]
pub(crate) struct FakeGit<F: Fn(&Path, &[&str]) -> Result<Vec<u8>, GitError>> {
    respond: F,
    calls: std::cell::RefCell<Vec<(PathBuf, Vec<String>)>>,
}

#[cfg(test)]
impl<F: Fn(&Path, &[&str]) -> Result<Vec<u8>, GitError>> FakeGit<F> {
    pub(crate) fn new(respond: F) -> Self {
        Self {
            respond,
            calls: Default::default(),
        }
    }

    pub(crate) fn calls(&self) -> Vec<(PathBuf, Vec<String>)> {
        self.calls.borrow().clone()
    }
}

#[cfg(test)]
impl<F: Fn(&Path, &[&str]) -> Result<Vec<u8>, GitError>> GitRunner for FakeGit<F> {
    fn run(&self, cwd: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
        self.calls.borrow_mut().push((
            cwd.to_path_buf(),
            args.iter().map(|a| a.to_string()).collect(),
        ));
        (self.respond)(cwd, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_A: &str = "45b983be36b73c0788dc9cbcb76cbb80fc7bb057";
    const ID_B: &str = "e0c213bc3202185a5890a137fb2563f3d408f0c1";
    const ID_SUB: &str = "ce013625030ba8dba906f756967f9e9ca394464a";

    fn ok(out: &'static str) -> impl Fn(&Path, &[&str]) -> Result<Vec<u8>, GitError> {
        move |_: &Path, _: &[&str]| Ok(out.as_bytes().to_vec())
    }

    #[test]
    fn location_in_a_subfolder() {
        assert_eq!(
            parse_location("E:/Github/repo\ncrates/zcommon/\n"),
            Ok((
                PathBuf::from("E:/Github/repo"),
                "crates/zcommon".to_string()
            ))
        );
    }

    #[test]
    fn location_at_the_root_has_an_empty_prefix() {
        assert_eq!(
            parse_location("E:/Github/repo\n\n"),
            Ok((PathBuf::from("E:/Github/repo"), String::new()))
        );
    }

    #[test]
    fn location_without_a_prefix_line_is_reported() {
        let err = parse_location("E:/Github/repo\n").unwrap_err();
        assert!(err.contains("prefix"), "{err}");
        assert!(parse_location("").is_err());
        assert!(parse_location("a\nb/\nc\n").is_err());
    }

    #[test]
    fn locate_appends_missing_components_to_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let git = FakeGit::new(ok("D:/repo\nsrc/\n"));

        let location = locate(&git, &sub.join("gone").join("a.rs")).unwrap();

        assert_eq!(
            location,
            GitLocation {
                root: PathBuf::from("D:/repo"),
                rel: "src/gone/a.rs".to_string()
            }
        );
        let calls = git.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, sub);
        assert_eq!(
            calls[0].1,
            ["rev-parse", "--show-toplevel", "--show-prefix"]
        );
    }

    #[test]
    fn locate_a_folder_runs_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let git = FakeGit::new(ok("D:/repo\n\n"));

        let location = locate(&git, dir.path()).unwrap();

        assert_eq!(location.rel, "");
        assert_eq!(git.calls()[0].0, dir.path());
    }

    #[test]
    fn commit_id_is_one_hex_line() {
        assert_eq!(parse_commit_id(&format!("{ID_A}\n")), Ok(ID_A.to_string()));
        let sha256 = "a".repeat(64);
        assert_eq!(parse_commit_id(&format!("{sha256}\n")), Ok(sha256));
    }

    #[test]
    fn malformed_commit_ids_are_reported() {
        let err = parse_commit_id("not-a-commit\n").unwrap_err();
        assert!(err.contains("not-a-commit"), "{err}");
        assert!(parse_commit_id("").is_err());
        assert!(parse_commit_id(&format!("{ID_A}\n{ID_B}\n")).is_err());
        assert!(parse_commit_id(&ID_A[..39]).is_err());
    }

    #[test]
    fn resolve_commit_peels_and_ends_options() {
        let git = FakeGit::new(ok("45b983be36b73c0788dc9cbcb76cbb80fc7bb057\n"));

        assert_eq!(
            resolve_commit(&git, Path::new("D:/repo"), "-v1").unwrap(),
            ID_A
        );

        assert_eq!(
            git.calls(),
            [(
                PathBuf::from("D:/repo"),
                vec![
                    "rev-parse".to_string(),
                    "--verify".to_string(),
                    "--end-of-options".to_string(),
                    "-v1^{commit}".to_string()
                ]
            )]
        );
    }

    const REFS: &str = "\
refs/heads/feature/x \n\
refs/heads/main \n\
refs/remotes/origin/HEAD refs/remotes/origin/main\n\
refs/remotes/origin/main \n\
refs/tags/main \n\
refs/tags/v1.2 \n";

    #[test]
    fn refs_are_classified_by_prefix_and_symrefs_dropped() {
        let refs = parse_refs(REFS).unwrap();
        let short: Vec<_> = refs.iter().map(|r| (r.kind, r.name.as_str())).collect();
        assert_eq!(
            short,
            [
                (RefKind::Branch, "feature/x"),
                (RefKind::Branch, "main"),
                (RefKind::RemoteBranch, "origin/main"),
                (RefKind::Tag, "main"),
                (RefKind::Tag, "v1.2"),
            ]
        );
        assert_eq!(refs[4].full_name, "refs/tags/v1.2");
    }

    #[test]
    fn no_refs_is_empty() {
        assert_eq!(parse_refs(""), Ok(vec![]));
    }

    #[test]
    fn malformed_ref_lines_are_reported() {
        let err = parse_refs("refs/heads/main \nrefs/notes/commits \n").unwrap_err();
        assert!(err.contains("line 2"), "{err}");
        assert!(err.contains("refs/notes/commits"), "{err}");
        assert!(parse_refs("refs/heads/main\n").is_err());
        assert!(parse_refs("refs/heads/ \n").is_err());
    }

    #[test]
    fn list_refs_asks_for_the_three_namespaces() {
        let git = FakeGit::new(ok(REFS));

        assert_eq!(list_refs(&git, Path::new("D:/repo")).unwrap().len(), 5);

        let args = &git.calls()[0].1;
        assert_eq!(args[0], "for-each-ref");
        assert_eq!(args[1], "--format=%(refname) %(symref)");
        assert_eq!(args[2..], ["refs/heads", "refs/remotes", "refs/tags"]);
    }

    const TREE: &str = "100644 blob 45b983be36b73c0788dc9cbcb76cbb80fc7bb057       3\ta\0\
100755 blob e0c213bc3202185a5890a137fb2563f3d408f0c1    1741\tdir/run me.sh\0\
120000 blob 45b983be36b73c0788dc9cbcb76cbb80fc7bb057       3\tlink\0\
160000 commit ce013625030ba8dba906f756967f9e9ca394464a       -\tsub\0";

    #[test]
    fn tree_listing_with_padded_sizes_and_a_submodule() {
        assert_eq!(
            parse_tree(TREE).unwrap(),
            [
                TreeEntry {
                    path: "a".to_string(),
                    kind: EntryKind::File,
                    id: ID_A.to_string(),
                    size: Some(3),
                },
                TreeEntry {
                    path: "dir/run me.sh".to_string(),
                    kind: EntryKind::Executable,
                    id: ID_B.to_string(),
                    size: Some(1741),
                },
                TreeEntry {
                    path: "link".to_string(),
                    kind: EntryKind::Symlink,
                    id: ID_A.to_string(),
                    size: Some(3),
                },
                TreeEntry {
                    path: "sub".to_string(),
                    kind: EntryKind::Submodule,
                    id: ID_SUB.to_string(),
                    size: None,
                },
            ]
        );
    }

    #[test]
    fn empty_tree_is_empty() {
        assert_eq!(parse_tree(""), Ok(vec![]));
    }

    #[test]
    fn malformed_tree_records_are_reported() {
        let err = parse_tree(&format!(
            "100644 blob {ID_A} 3\ta\0100644 blob {ID_A} x\tb\0"
        ))
        .unwrap_err();
        assert!(err.contains("record 2"), "{err}");
        assert!(err.contains(&format!("{ID_A} x")), "{err}");
        assert!(parse_tree(&format!("100644 blob {ID_A} 3 a\0")).is_err());
        assert!(parse_tree(&format!("040000 tree {ID_A} -\tdir\0")).is_err());
        assert!(parse_tree(&format!("100644 blob {ID_A} -\ta\0")).is_err());
        assert!(parse_tree(&format!("100644 commit {ID_A} 3\ta\0")).is_err());
        assert!(parse_tree("100644 blob abc 3\ta\0").is_err());
    }

    #[test]
    fn list_tree_lists_the_folders_tree() {
        let git = FakeGit::new(ok(TREE));

        list_tree(&git, Path::new("D:/repo"), "main~1", "src/sub").unwrap();
        list_tree(&git, Path::new("D:/repo"), "HEAD", "").unwrap();

        let calls = git.calls();
        assert_eq!(
            calls[0].1,
            [
                "ls-tree",
                "-r",
                "-l",
                "-z",
                "--end-of-options",
                "main~1:src/sub"
            ]
        );
        assert_eq!(calls[1].1[5], "HEAD:");
    }

    #[test]
    fn a_parse_error_names_the_command() {
        let git = FakeGit::new(ok("garbage\0"));

        let err = list_tree(&git, Path::new("D:/repo"), "HEAD", "").unwrap_err();

        assert!(matches!(err.kind, GitErrorKind::Parse(_)), "{err:?}");
        assert!(err.command.contains("ls-tree -r -l -z"), "{}", err.command);
        assert!(err.to_string().contains("garbage"), "{err}");
    }

    #[test]
    fn a_runner_error_passes_through() {
        let failed = GitError {
            command: "git ls-tree".to_string(),
            kind: GitErrorKind::Failed {
                status: "exit code: 128".to_string(),
                stderr: "fatal: not a tree object\n".to_string(),
            },
        };
        let expected = failed.clone();
        let git = FakeGit::new(move |_: &Path, _: &[&str]| Err(failed.clone()));

        let err = list_tree(&git, Path::new("D:/repo"), "nope", "").unwrap_err();

        assert_eq!(err, expected);
        assert_eq!(
            err.to_string(),
            "git ls-tree failed (exit code: 128): fatal: not a tree object"
        );
    }

    const INDEX: &str = "100644 45b983be36b73c0788dc9cbcb76cbb80fc7bb057 0 3\tsrc/a\0\
100644 45b983be36b73c0788dc9cbcb76cbb80fc7bb057 1 3\tsrc/c\0\
100644 e0c213bc3202185a5890a137fb2563f3d408f0c1 2 1741\tsrc/c\0\
160000 ce013625030ba8dba906f756967f9e9ca394464a 0 -\tsrc/sub\0";

    #[test]
    fn index_listing_with_sizes_and_stages() {
        let entries = parse_index(INDEX).unwrap();
        assert_eq!(
            entries[0],
            IndexEntry {
                entry: TreeEntry {
                    path: "src/a".to_string(),
                    kind: EntryKind::File,
                    id: ID_A.to_string(),
                    size: Some(3),
                },
                stage: 0,
            }
        );
        let stages: Vec<_> = entries
            .iter()
            .map(|e| (e.entry.path.as_str(), e.stage, e.entry.size))
            .collect();
        assert_eq!(
            stages,
            [
                ("src/a", 0, Some(3)),
                ("src/c", 1, Some(3)),
                ("src/c", 2, Some(1741)),
                ("src/sub", 0, None),
            ]
        );
        assert_eq!(entries[3].entry.kind, EntryKind::Submodule);
    }

    #[test]
    fn malformed_index_records_are_reported() {
        let err =
            parse_index(&format!("100644 {ID_A} 0 3\ta\0100644 {ID_A} 4 3\tb\0")).unwrap_err();
        assert!(err.contains("record 2"), "{err}");
        assert!(parse_index(&format!("100644 {ID_A} 0\ta\0")).is_err());
        assert!(parse_index(&format!("100644 {ID_A} 0 3 a\0")).is_err());
        assert!(parse_index(&format!("100644 {ID_A} 0 -\ta\0")).is_err());
    }

    #[test]
    fn list_index_strips_the_folder() {
        let git = FakeGit::new(ok(INDEX));

        let entries = list_index(&git, Path::new("D:/repo"), "src").unwrap();

        let paths: Vec<_> = entries.iter().map(|e| e.entry.path.as_str()).collect();
        assert_eq!(paths, ["a", "c", "c", "sub"]);
        let args = &git.calls()[0].1;
        assert_eq!(args[..3], ["ls-files", "-z", INDEX_FORMAT]);
        assert_eq!(args[3..], ["--", ":(literal)src"]);
    }

    #[test]
    fn list_index_of_the_root_has_no_pathspec() {
        let git = FakeGit::new(ok(INDEX));

        let entries = list_index(&git, Path::new("D:/repo"), "").unwrap();

        assert_eq!(entries[0].entry.path, "src/a");
        assert_eq!(git.calls()[0].1, ["ls-files", "-z", INDEX_FORMAT]);
    }

    #[test]
    fn read_file_applies_checkout_filters() {
        let git = FakeGit::new(|_: &Path, _: &[&str]| Ok(vec![0xff, 0, b'\r', b'\n']));

        let bytes = read_file(&git, Path::new("D:/repo"), "v1.2", "src/a b.rs").unwrap();
        read_index_file(&git, Path::new("D:/repo"), "src/a b.rs").unwrap();

        assert_eq!(bytes, [0xff, 0, b'\r', b'\n']);
        let calls = git.calls();
        assert_eq!(
            calls[0].1,
            [
                "cat-file",
                "--filters",
                "--end-of-options",
                "v1.2:src/a b.rs"
            ]
        );
        assert_eq!(
            calls[1].1,
            ["cat-file", "--filters", "--end-of-options", ":src/a b.rs"]
        );
    }

    #[test]
    fn the_default_executable_is_git() {
        assert_eq!(GitCommand::default().exe, PathBuf::from("git"));
    }

    #[test]
    fn a_missing_git_is_named() {
        let git = GitCommand {
            exe: PathBuf::from("zhashdiff-no-such-git"),
        };

        let err = git.run(Path::new("."), &["--version"]).unwrap_err();

        assert_eq!(
            err.kind,
            GitErrorKind::NotFound {
                exe: "zhashdiff-no-such-git".to_string()
            }
        );
        let message = err.to_string();
        assert!(message.contains("git not found"), "{message}");
        assert!(message.contains("zhashdiff-no-such-git"), "{message}");
    }

    fn setup(dir: &Path, args: &[&str]) {
        let config = dir.join("empty.gitconfig");
        std::fs::write(&config, "").unwrap();
        let output = Command::new("git")
            .args(args)
            .current_dir(dir.join("repo"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &config)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .unwrap_or_else(|e| panic!("git not found on PATH, install git to run this test: {e}"));
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore = "needs git on PATH"]
    fn tree_listing_of_a_real_repository() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        std::fs::write(repo.join("top.txt"), "hello\n").unwrap();
        std::fs::write(repo.join("src/a.rs"), "fn main() {}\n").unwrap();
        std::fs::write(repo.join("src/deep/b.bin"), [0u8, 1, 2, 3, 4]).unwrap();
        setup(dir.path(), &["init", "-q"]);
        std::fs::write(repo.join(".git/info/attributes"), "*.rs text eol=crlf\n").unwrap();
        setup(dir.path(), &["add", "."]);
        setup(dir.path(), &["commit", "-q", "-m", "first"]);
        setup(dir.path(), &["tag", "v1"]);
        std::fs::write(repo.join("src/a.rs"), "changed\n").unwrap();
        setup(dir.path(), &["add", "src/a.rs"]);
        let git = GitCommand::default();

        let location =
            locate(&git, &repo.join("src").join("a.rs")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(location.rel, "src/a.rs");
        assert_eq!(
            location.root.canonicalize().unwrap(),
            repo.canonicalize().unwrap()
        );
        let root = location.root;

        let tree = list_tree(&git, &root, "v1", "").unwrap_or_else(|e| panic!("{e}"));
        let listed: Vec<_> = tree
            .iter()
            .map(|e| (e.path.as_str(), e.kind, e.size))
            .collect();
        assert_eq!(
            listed,
            [
                ("src/a.rs", EntryKind::File, Some(13)),
                ("src/deep/b.bin", EntryKind::File, Some(5)),
                ("top.txt", EntryKind::File, Some(6)),
            ]
        );
        assert_eq!(tree[2].id, "ce013625030ba8dba906f756967f9e9ca394464a");

        let sub = list_tree(&git, &root, "HEAD", "src").unwrap_or_else(|e| panic!("{e}"));
        let paths: Vec<_> = sub.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["a.rs", "deep/b.bin"]);

        let index = list_index(&git, &root, "src").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(index[0].entry.path, "a.rs");
        assert_eq!(index[0].entry.size, Some(8));
        assert_eq!(index[0].stage, 0);

        assert_eq!(
            read_file(&git, &root, "v1", "src/a.rs").unwrap(),
            b"fn main() {}\r\n"
        );
        assert_eq!(
            read_index_file(&git, &root, "src/a.rs").unwrap(),
            b"changed\r\n"
        );

        let head = resolve_commit(&git, &root, "HEAD").unwrap();
        assert_eq!(resolve_commit(&git, &root, "v1").unwrap(), head);
        let refs = list_refs(&git, &root).unwrap();
        assert!(refs.iter().any(|r| r.kind == RefKind::Branch));
        assert!(
            refs.iter()
                .any(|r| r.kind == RefKind::Tag && r.name == "v1")
        );

        let err = read_file(&git, &root, "v1", "missing").unwrap_err();
        assert!(matches!(err.kind, GitErrorKind::Failed { .. }), "{err:?}");
        assert!(err.to_string().contains("missing"), "{err}");
    }
}
