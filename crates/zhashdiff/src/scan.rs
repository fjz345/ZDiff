use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::SystemTime,
};

use zcommon::hash::HashRepresentation;

use crate::fs::{FileSystemModel, FsNodeId, FsNodeKind};

#[derive(Debug, Clone, PartialEq)]
pub struct ListingEntry {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub hash: Option<HashRepresentation>,
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

impl ListingSource for LocalDisk {
    fn children(&self, dir: &Path) -> io::Result<Vec<Result<ListingEntry, SkippedEntry>>> {
        let entries = std::fs::read_dir(dir)?
            .map(|entry| {
                let entry = entry.map_err(|e| SkippedEntry {
                    path: dir.to_path_buf(),
                    reason: e.to_string(),
                })?;
                let path = entry.path();
                // Follows symlinks and junctions; DirEntry::metadata does not on Windows.
                match std::fs::metadata(&path) {
                    Ok(meta) => Ok(ListingEntry {
                        is_dir: meta.is_dir(),
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
        model: FileSystemModel::empty(root),
        skipped: Vec::new(),
    };
    let root_id = scanner.push(None, FsNodeKind::new_empty_dir(root), 0, None);
    assert_eq!(root_id, 0);
    scanner.add_children(root_id, listing)?;

    Some(Ok(ScanOutput {
        model: scanner.model,
        skipped: scanner.skipped,
    }))
}

struct Scanner<'a> {
    source: &'a dyn ListingSource,
    cancel: &'a AtomicBool,
    found: &'a AtomicUsize,
    model: FileSystemModel,
    skipped: Vec<SkippedEntry>,
}

impl Scanner<'_> {
    fn push(
        &mut self,
        parent: Option<FsNodeId>,
        kind: FsNodeKind,
        size: u64,
        modified: Option<SystemTime>,
    ) -> FsNodeId {
        self.found.fetch_add(1, Ordering::Relaxed);
        self.model.push_node(parent, kind, size, modified)
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
                    FsNodeKind::new_empty_dir(&entry.path),
                    entry.size,
                    entry.modified,
                );
                self.add_children(id, sub_listing)?;
                id
            } else {
                self.push(
                    Some(dir_id),
                    FsNodeKind::new_file(&entry.path),
                    entry.size,
                    entry.modified,
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
            .map(|&id| model.get_node(id).unwrap().as_path().as_ref().to_path_buf())
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
