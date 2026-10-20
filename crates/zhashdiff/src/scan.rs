use std::{
    collections::HashMap,
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::SystemTime,
};

use crate::fs::{FileSystemModel, FsNodeId, FsNodeKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum HashKind {
    GitBlob,
    P4Digest(P4Recipe),
}

impl HashKind {
    pub fn compares_with(self, other: HashKind) -> bool {
        matches!(
            (self, other),
            (HashKind::GitBlob, HashKind::GitBlob) | (HashKind::P4Digest(_), HashKind::P4Digest(_))
        )
    }
}

/// How a p4 digest (MD5) is reproduced from a local file's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum P4Recipe {
    Raw,
    /// Server content has LF line endings; the local copy may have CRLF.
    Text,
    /// Keyword expansion or charset translation: only a print reproduces the local bytes.
    ServerOnly,
}

/// Only comparable with a hash of a kind it `compares_with`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ContentHash {
    pub kind: HashKind,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListingEntry {
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub hash: Option<ContentHash>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SkippedEntry {
    pub path: PathBuf,
    pub reason: String,
}

pub trait ListingSource {
    /// Outer error: `dir` itself could not be listed. Inner error: one entry could not be read.
    fn children(&self, dir: &Path) -> io::Result<Vec<Result<ListingEntry, SkippedEntry>>>;
}

pub struct LocalDisk;

/// A file of a source that lists everything at once; `rel_path` is `/`-separated.
#[derive(Debug, Clone, PartialEq)]
pub struct FlatEntry {
    pub rel_path: String,
    pub size: u64,
    pub hash: Option<ContentHash>,
}

/// Folders are derived from the file paths, so it has no empty folders.
#[derive(Debug)]
pub struct FlatListing {
    dirs: HashMap<PathBuf, Vec<Result<ListingEntry, SkippedEntry>>>,
}

impl FlatListing {
    /// `skipped` is reported as entries of `root`.
    pub fn new(root: &Path, files: Vec<FlatEntry>, skipped: Vec<SkippedEntry>) -> Self {
        let mut dirs = HashMap::new();
        dirs.insert(
            root.to_path_buf(),
            skipped.into_iter().map(Err).collect::<Vec<_>>(),
        );
        for file in files {
            if !file.rel_path.split('/').all(is_plain_name) {
                dirs.get_mut(root).unwrap().push(Err(SkippedEntry {
                    path: root.to_path_buf(),
                    reason: format!("malformed path {:?}", file.rel_path),
                }));
                continue;
            }
            let (folders, name) = match file.rel_path.rsplit_once('/') {
                Some((folders, name)) => (Some(folders), name),
                None => (None, file.rel_path.as_str()),
            };
            // Segment by segment, so the paths have the platform separator like a disk scan's.
            let mut dir = root.to_path_buf();
            for segment in folders.into_iter().flat_map(|folders| folders.split('/')) {
                let sub = dir.join(segment);
                if !dirs.contains_key(&sub) {
                    dirs.insert(sub.clone(), Vec::new());
                    dirs.get_mut(&dir).unwrap().push(Ok(ListingEntry {
                        path: sub.clone(),
                        is_dir: true,
                        is_link: false,
                        size: 0,
                        modified: None,
                        hash: None,
                    }));
                }
                dir = sub;
            }
            let path = dir.join(name);
            dirs.get_mut(&dir).unwrap().push(Ok(ListingEntry {
                path,
                is_dir: false,
                is_link: false,
                size: file.size,
                modified: None,
                hash: file.hash,
            }));
        }
        Self { dirs }
    }
}

/// A file or folder name that, joined to a folder, stays a direct child of it. Windows drops
/// trailing dots and spaces, so `. .` is `..` there; `:` names a drive or a data stream.
pub(crate) fn is_plain_name(name: &str) -> bool {
    !name.contains(['/', '\\', ':']) && !name.trim_end_matches(['.', ' ']).is_empty()
}

impl ListingSource for FlatListing {
    fn children(&self, dir: &Path) -> io::Result<Vec<Result<ListingEntry, SkippedEntry>>> {
        self.dirs.get(dir).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} is not a listed folder", dir.display()),
            )
        })
    }
}

impl ListingSource for LocalDisk {
    fn children(&self, dir: &Path) -> io::Result<Vec<Result<ListingEntry, SkippedEntry>>> {
        let entries = std::fs::read_dir(dir)?
            .map(|entry| {
                let entry = entry.map_err(|e| SkippedEntry {
                    path: dir.to_path_buf(),
                    reason: e.to_string(),
                })?;
                let path = entry.path();
                // DirEntry::metadata is free on Windows but doesn't follow symlinks and junctions,
                // which is_symlink covers there.
                let is_link =
                    !matches!(entry.file_type(), Ok(file_type) if !file_type.is_symlink());
                let meta = if is_link {
                    std::fs::metadata(&path)
                } else {
                    entry.metadata()
                };
                match meta {
                    Ok(meta) => Ok(ListingEntry {
                        is_dir: meta.is_dir(),
                        is_link,
                        size: meta.len(),
                        modified: meta.modified().ok(),
                        hash: None,
                        path,
                    }),
                    Err(e) => Err(SkippedEntry {
                        path,
                        reason: e.to_string(),
                    }),
                }
            })
            .collect();
        Ok(entries)
    }
}

#[derive(Debug)]
pub struct ScanOutput {
    pub model: FileSystemModel,
    pub skipped: Vec<SkippedEntry>,
}

/// `None` when cancelled. `found` counts nodes added so far.
pub fn scan(
    source: &dyn ListingSource,
    root: &Path,
    cancel: &AtomicBool,
    found: &AtomicUsize,
) -> Option<io::Result<ScanOutput>> {
    let listing = match source.children(root) {
        Ok(listing) => listing,
        Err(e) => return Some(Err(e)),
    };
    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    let mut scanner = Scanner {
        source,
        cancel,
        found,
        model: FileSystemModel::empty(),
        skipped: Vec::new(),
        ancestors: vec![root.to_path_buf()],
    };
    scanner.model.set_root_path(root.to_path_buf());
    let root_name = root.file_name().unwrap_or_default();
    let root_id = scanner.push(None, root_name, FsNodeKind::new_empty_dir(), 0, None, None);
    assert_eq!(root_id, 0);
    scanner.add_children(root_id, listing)?;

    Some(Ok(ScanOutput {
        model: scanner.model,
        skipped: scanner.skipped,
    }))
}

fn entry_name(path: &Path) -> &OsStr {
    // A depot root at the server level ("//depot") is a UNC prefix on Windows: its children have no file_name.
    path.file_name().unwrap_or_else(|| {
        let text = path
            .as_os_str()
            .to_str()
            .expect("listing entry path is not UTF-8");
        OsStr::new(text.rsplit(['/', '\\']).next().unwrap_or(text))
    })
}

struct Scanner<'a> {
    source: &'a dyn ListingSource,
    cancel: &'a AtomicBool,
    found: &'a AtomicUsize,
    model: FileSystemModel,
    skipped: Vec<SkippedEntry>,
    ancestors: Vec<PathBuf>,
}

impl Scanner<'_> {
    /// The folder being scanned above `link` that `link` resolves to. Following it would list the
    /// same files again until the path gets too long.
    fn looped_ancestor(&self, link: &Path) -> Option<&PathBuf> {
        let real = link.canonicalize().ok()?;
        self.ancestors
            .iter()
            .find(|ancestor| ancestor.canonicalize().is_ok_and(|a| a == real))
    }

    fn push(
        &mut self,
        parent: Option<FsNodeId>,
        name: &OsStr,
        kind: FsNodeKind,
        size: u64,
        modified: Option<SystemTime>,
        hash: Option<ContentHash>,
    ) -> FsNodeId {
        self.found.fetch_add(1, Ordering::Relaxed);
        self.model
            .push_node(parent, name, kind, size, modified, hash)
    }

    fn add_children(
        &mut self,
        dir_id: FsNodeId,
        listing: Vec<Result<ListingEntry, SkippedEntry>>,
    ) -> Option<()> {
        let mut children = Vec::with_capacity(listing.len());
        for entry in listing {
            let entry = match entry {
                Ok(entry) => entry,
                Err(skipped) => {
                    self.skipped.push(skipped);
                    continue;
                }
            };
            let child_id = if entry.is_dir {
                if entry.is_link
                    && let Some(ancestor) = self.looped_ancestor(&entry.path)
                {
                    let reason = format!("links back to {}", ancestor.display());
                    self.skipped.push(SkippedEntry {
                        path: entry.path,
                        reason,
                    });
                    continue;
                }
                // An unlistable folder is omitted rather than shown empty, so it can't compare as Same.
                let sub_listing = match self.source.children(&entry.path) {
                    Ok(listing) => listing,
                    Err(e) => {
                        self.skipped.push(SkippedEntry {
                            path: entry.path,
                            reason: e.to_string(),
                        });
                        continue;
                    }
                };
                if self.cancel.load(Ordering::Relaxed) {
                    return None;
                }
                let id = self.push(
                    Some(dir_id),
                    entry_name(&entry.path),
                    FsNodeKind::new_empty_dir(),
                    entry.size,
                    entry.modified,
                    None,
                );
                self.ancestors.push(entry.path);
                self.add_children(id, sub_listing)?;
                self.ancestors.pop();
                id
            } else {
                self.push(
                    Some(dir_id),
                    entry_name(&entry.path),
                    FsNodeKind::new_file(),
                    entry.size,
                    entry.modified,
                    entry.hash,
                )
            };
            children.push(child_id);
        }
        self.model.set_children(dir_id, children);
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::FsNode;
    use std::{
        collections::HashMap,
        fs::{self, File},
        io::Write,
        time::Duration,
    };
    use tempfile::tempdir;

    fn node_at<'a>(model: &'a FileSystemModel, path: &Path) -> &'a FsNode {
        let id = model.find_path(path).expect("path not in model");
        model.get_node(id).unwrap()
    }

    fn child_paths(model: &FileSystemModel, node: &FsNode) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = node
            .children()
            .expect("not a dir")
            .iter()
            .map(|&id| model.path(id))
            .collect();
        paths.sort();
        paths
    }

    fn scan_local(root: &Path) -> ScanOutput {
        scan(
            &LocalDisk,
            root,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )
        .expect("not cancelled")
        .expect("root readable")
    }

    #[test]
    fn local_scan_builds_nested_and_empty_folders() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("a").join("b")).unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        File::create(root.join("a").join("b").join("deep.txt")).unwrap();
        File::create(root.join("top.txt")).unwrap();

        let output = scan_local(root);
        let model = &output.model;

        assert!(output.skipped.is_empty());
        assert_eq!(model.total_folders(), 4);
        assert_eq!(model.total_files(), 2);
        FileSystemModel::assert_tree_consistency(model);
        assert_eq!(
            child_paths(model, model.get_root()),
            vec![root.join("a"), root.join("empty"), root.join("top.txt")]
        );
        assert!(
            node_at(model, &root.join("empty"))
                .children()
                .unwrap()
                .is_empty()
        );
        let b = node_at(model, &root.join("a").join("b"));
        assert_eq!(
            child_paths(model, b),
            vec![root.join("a").join("b").join("deep.txt")]
        );
        assert_eq!(b.parent, model.find_path(root.join("a")));
    }

    #[test]
    fn local_scan_records_size_and_mtime() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("five.bin");
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        let mut file = File::create(&path).unwrap();
        file.write_all(b"12345").unwrap();
        file.set_modified(mtime).unwrap();
        drop(file);

        let output = scan_local(dir.path());
        let node = node_at(&output.model, &path);

        assert_eq!(node.size, 5);
        assert_eq!(node.modified, Some(mtime));
    }

    #[cfg(windows)]
    #[test]
    fn local_scan_follows_a_junction_into_its_target() {
        let dir = tempdir().unwrap();
        let (target, root) = (dir.path().join("target"), dir.path().join("root"));
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::write(target.join("inside.txt"), "12345").unwrap();
        let link = root.join("link");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap()
            .status;
        assert!(status.success());

        let output = scan_local(&root);
        let model = &output.model;

        let link_node = node_at(model, &link);
        assert!(link_node.is_dir());
        assert_eq!(child_paths(model, link_node), vec![link.join("inside.txt")]);
        assert_eq!(node_at(model, &link.join("inside.txt")).size, 5);
    }

    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap()
            .status;
        assert!(status.success());
    }

    #[cfg(windows)]
    #[test]
    fn local_scan_skips_a_junction_to_an_ancestor() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub").join("a.txt"), "a").unwrap();
        let link = root.join("sub").join("up");
        junction(&link, &root);

        let output = scan_local(&root);

        assert_eq!(output.model.total_files(), 1);
        assert!(output.model.find_path(&link).is_none());
        assert_eq!(output.skipped.len(), 1);
        assert_eq!(output.skipped[0].path, link);
    }

    #[cfg(windows)]
    #[test]
    fn local_scan_skips_junctions_that_loop_through_each_other() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("root");
        let (a, b) = (root.join("a"), root.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("a.txt"), "a").unwrap();
        junction(&a.join("to_b"), &b);
        junction(&b.join("to_a"), &a);

        let output = scan_local(&root);

        assert_eq!(output.model.total_files(), 2);
        assert!(
            output
                .model
                .find_path(b.join("to_a").join("a.txt"))
                .is_some()
        );
        let mut skipped: Vec<PathBuf> = output.skipped.into_iter().map(|s| s.path).collect();
        skipped.sort();
        assert_eq!(
            skipped,
            vec![a.join("to_b").join("to_a"), b.join("to_a").join("to_b")]
        );
    }

    #[test]
    fn local_scan_with_cancel_set_returns_none() {
        let dir = tempdir().unwrap();
        File::create(dir.path().join("a.txt")).unwrap();

        let result = scan(
            &LocalDisk,
            dir.path(),
            &AtomicBool::new(true),
            &AtomicUsize::new(0),
        );

        assert!(result.is_none());
    }

    #[test]
    fn unreadable_root_is_an_error() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("missing");

        let result = scan(
            &LocalDisk,
            &missing,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        );

        assert!(matches!(result, Some(Err(_))));
    }

    #[derive(Default)]
    struct FakeSource<'a> {
        dirs: HashMap<PathBuf, Vec<Result<ListingEntry, SkippedEntry>>>,
        cancel_on: Option<(PathBuf, &'a AtomicBool)>,
    }

    impl ListingSource for FakeSource<'_> {
        fn children(&self, dir: &Path) -> io::Result<Vec<Result<ListingEntry, SkippedEntry>>> {
            if let Some((cancel_dir, flag)) = &self.cancel_on
                && cancel_dir == dir
            {
                flag.store(true, Ordering::Relaxed);
            }
            self.dirs
                .get(dir)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
        }
    }

    fn entry(path: &str, is_dir: bool) -> Result<ListingEntry, SkippedEntry> {
        Ok(ListingEntry {
            path: path.into(),
            is_dir,
            is_link: false,
            size: 0,
            modified: None,
            hash: None,
        })
    }

    #[test]
    fn per_entry_errors_are_skipped_and_listed() {
        let bad = SkippedEntry {
            path: "root/bad.txt".into(),
            reason: "unreadable".into(),
        };
        let mut source = FakeSource::default();
        source.dirs.insert(
            "root".into(),
            vec![
                entry("root/a.txt", false),
                Err(bad.clone()),
                entry("root/locked", true),
                entry("root/sub", true),
            ],
        );
        source
            .dirs
            .insert("root/sub".into(), vec![entry("root/sub/c.txt", false)]);
        let found = AtomicUsize::new(0);

        let output = scan(&source, Path::new("root"), &AtomicBool::new(false), &found)
            .unwrap()
            .unwrap();
        let model = &output.model;

        assert_eq!(output.skipped.len(), 2);
        assert_eq!(output.skipped[0], bad);
        assert_eq!(output.skipped[1].path, PathBuf::from("root/locked"));
        assert!(output.skipped[1].reason.contains("denied"));
        assert_eq!(
            child_paths(model, model.get_root()),
            vec![PathBuf::from("root/a.txt"), PathBuf::from("root/sub")]
        );
        assert!(matches!(
            node_at(model, Path::new("root/sub/c.txt")).kind,
            FsNodeKind::File { .. }
        ));
        assert_eq!(
            found.load(Ordering::Relaxed),
            model.total_files_and_folders()
        );
        FileSystemModel::assert_tree_consistency(model);
    }

    fn flat(rel_path: &str, size: u64, hash: Option<&str>) -> FlatEntry {
        FlatEntry {
            rel_path: rel_path.into(),
            size,
            hash: hash.map(|value| ContentHash {
                kind: HashKind::GitBlob,
                value: value.into(),
            }),
        }
    }

    fn scan_flat(root: &Path, listing: &FlatListing) -> ScanOutput {
        scan(listing, root, &AtomicBool::new(false), &AtomicUsize::new(0))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn flat_listing_derives_nested_folders() {
        let root = Path::new(r"D:\repo\src@git:main");
        let skipped = SkippedEntry {
            path: root.join("sub"),
            reason: "submodule".into(),
        };
        let listing = FlatListing::new(
            root,
            vec![
                flat("deep/x/y.rs", 3, Some("1111")),
                flat("a.txt", 5, Some("2222")),
                flat("lib/lib.rs", 7, None),
                flat("deep/z.rs", 9, Some("3333")),
            ],
            vec![skipped.clone()],
        );

        let output = scan_flat(root, &listing);
        let model = &output.model;

        assert_eq!(output.skipped, vec![skipped]);
        assert_eq!(model.total_folders(), 4);
        assert_eq!(model.total_files(), 4);
        FileSystemModel::assert_tree_consistency(model);
        let (deep, x, lib) = (
            root.join("deep"),
            root.join("deep").join("x"),
            root.join("lib"),
        );
        assert_eq!(
            child_paths(model, model.get_root()),
            vec![root.join("a.txt"), deep.clone(), lib.clone()]
        );
        assert_eq!(
            child_paths(model, node_at(model, &deep)),
            vec![x.clone(), deep.join("z.rs")]
        );
        assert_eq!(
            child_paths(model, node_at(model, &lib)),
            vec![lib.join("lib.rs")]
        );
        assert_eq!(node_at(model, &x).parent, model.find_path(&deep));
        let y = node_at(model, &x.join("y.rs"));
        assert!(matches!(y.kind, FsNodeKind::File { .. }));
        assert_eq!(y.parent, model.find_path(&x));
        assert_eq!(y.size, 3);
        assert_eq!(
            y.hash,
            Some(ContentHash {
                kind: HashKind::GitBlob,
                value: "1111".into()
            })
        );
        assert_eq!(node_at(model, &lib.join("lib.rs")).hash, None);
        assert_eq!(node_at(model, &deep).hash, None);
    }

    #[test]
    fn flat_listing_without_files_is_an_empty_root() {
        let root = Path::new(r"D:\repo@git:main");
        let output = scan_flat(root, &FlatListing::new(root, Vec::new(), Vec::new()));
        assert_eq!(output.model.total_files_and_folders(), 1);
        assert!(output.model.get_root().children().unwrap().is_empty());
    }

    #[test]
    fn flat_listing_skips_paths_with_empty_segments() {
        let root = Path::new("root");
        let listing = FlatListing::new(
            root,
            vec![flat("a//b.txt", 1, None), flat("c.txt", 1, None)],
            Vec::new(),
        );
        let output = scan_flat(root, &listing);
        assert_eq!(output.skipped.len(), 1);
        assert!(output.skipped[0].reason.contains("a//b.txt"));
        assert_eq!(
            child_paths(&output.model, output.model.get_root()),
            vec![root.join("c.txt")]
        );
    }

    #[test]
    fn flat_listing_skips_paths_that_could_leave_the_root() {
        let root = Path::new("root");
        let unsafe_paths = [
            r"a\..\..\x.txt",
            "a/../../x.txt",
            "./x.txt",
            "a/. . /x.txt",
            "C:x.txt",
            "x.txt:stream",
        ];
        let files = unsafe_paths
            .iter()
            .map(|path| flat(path, 1, None))
            .chain([flat("dir/ok.txt", 1, None)])
            .collect();

        let output = scan_flat(root, &FlatListing::new(root, files, Vec::new()));

        assert_eq!(output.skipped.len(), unsafe_paths.len());
        for (skipped, path) in output.skipped.iter().zip(unsafe_paths) {
            assert!(skipped.reason.contains(&format!("{path:?}")), "{path}");
        }
        assert_eq!(output.model.total_files(), 1);
        assert!(
            output
                .model
                .find_path(root.join("dir").join("ok.txt"))
                .is_some()
        );
    }

    #[test]
    fn cancel_during_scan_returns_none() {
        let cancel = AtomicBool::new(false);
        let mut source = FakeSource {
            cancel_on: Some(("root/sub".into(), &cancel)),
            ..Default::default()
        };
        source.dirs.insert(
            "root".into(),
            vec![entry("root/sub", true), entry("root/z.txt", false)],
        );
        source
            .dirs
            .insert("root/sub".into(), vec![entry("root/sub/c.txt", false)]);

        let result = scan(&source, Path::new("root"), &cancel, &AtomicUsize::new(0));

        assert!(result.is_none());
    }
}
