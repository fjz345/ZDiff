use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::SystemTime;

use eframe::egui;
use zcommon::job::Job;
use zcommon::lru::{DEFAULT_MAX_ENTRIES, LruCache};
use zcommon::vcs::git::{self, GitCommand, GitRunner};
use zdiff::cached_file::CachedFile;
use zdiff::lexer::RawToken;
use zdiff::universal_path::{DepotRev, GitRev, UniversalPath};

use crate::p4::{P4Command, P4Runner, print_file};
use crate::pdb_dump::{self, PdbDumpOptions};
use crate::viewer::audio::{RepaintContext, repaint_job};
use crate::viewer::{ViewerKind, path_extension, sniff_viewer_kind};

/// A file whose content was sniffed as not text, kept as raw bytes for the Hex viewer.
pub struct BinaryFile {
    pub path: UniversalPath,
    pub bytes: Vec<u8>,
}

// Input logging prints the loaded files; the bytes can be hundreds of MB.
impl std::fmt::Debug for BinaryFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BinaryFile")
            .field("path", &self.path)
            .field("len", &self.bytes.len())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub enum LoadedFile {
    Text(Arc<CachedFile<RawToken>>),
    Binary(Arc<BinaryFile>),
    /// A PDB shown as its text dump; `source` keeps the file's bytes for the byte viewers.
    Dump {
        text: Arc<CachedFile<RawToken>>,
        source: Arc<BinaryFile>,
    },
}

impl LoadedFile {
    pub fn path(&self) -> &UniversalPath {
        match self {
            LoadedFile::Text(file) | LoadedFile::Dump { text: file, .. } => &file.path,
            LoadedFile::Binary(file) => &file.path,
        }
    }

    /// The exact file bytes; text contents are read unmodified, so they are the file's bytes too.
    pub fn bytes(&self) -> &[u8] {
        match self {
            LoadedFile::Text(file) => file.contents.as_bytes(),
            LoadedFile::Binary(file) | LoadedFile::Dump { source: file, .. } => &file.bytes,
        }
    }

    pub fn viewer_kind(&self) -> ViewerKind {
        match self {
            LoadedFile::Text(_) | LoadedFile::Dump { .. } => ViewerKind::Text,
            LoadedFile::Binary(_) => ViewerKind::Hex,
        }
    }

    pub fn text(&self) -> Option<&Arc<CachedFile<RawToken>>> {
        match self {
            LoadedFile::Text(file) | LoadedFile::Dump { text: file, .. } => Some(file),
            LoadedFile::Binary(_) => None,
        }
    }

    pub fn is_dump(&self) -> bool {
        matches!(self, LoadedFile::Dump { .. })
    }

    fn approx_bytes(&self) -> usize {
        match self {
            LoadedFile::Text(file) => file.approx_bytes(),
            LoadedFile::Binary(file) => file.bytes.len(),
            LoadedFile::Dump { text, source } => text.approx_bytes() + source.bytes.len(),
        }
    }

    /// Same load, not just equal content: every load makes new `Arc`s.
    pub fn is_same_load(&self, other: &Self) -> bool {
        match (self, other) {
            (LoadedFile::Text(a), LoadedFile::Text(b)) => Arc::ptr_eq(a, b),
            (LoadedFile::Binary(a), LoadedFile::Binary(b)) => Arc::ptr_eq(a, b),
            (LoadedFile::Dump { text: a, .. }, LoadedFile::Dump { text: b, .. }) => {
                Arc::ptr_eq(a, b)
            }
            _ => false,
        }
    }
}

fn is_pdb_path(path: &UniversalPath) -> bool {
    path_extension(path).as_deref() == Some("pdb")
}

pub fn with_pdb_dump(file: LoadedFile, options: &PdbDumpOptions) -> LoadedFile {
    let LoadedFile::Binary(source) = &file else {
        return file;
    };
    if !is_pdb_path(&source.path) || !pdb_dump::is_msf(&source.bytes) {
        return file;
    }
    match pdb_dump::dump(&source.bytes, options) {
        Ok(text) => LoadedFile::Dump {
            text: Arc::new(CachedFile::from_contents(source.path.clone(), text)),
            source: source.clone(),
        },
        Err(e) => {
            log::warn!("{}: not parsed as a PDB, shown as bytes: {e}", source.path);
            file
        }
    }
}

pub fn load_file(display_path: UniversalPath, physical_path: &Path) -> io::Result<LoadedFile> {
    let bytes = std::fs::read(physical_path)?;
    match sniff_viewer_kind(&bytes) {
        ViewerKind::Text => String::from_utf8(bytes)
            .map(|contents| {
                LoadedFile::Text(Arc::new(CachedFile::from_contents(display_path, contents)))
            })
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        // Sniffing never picks Image; image content is binary, and the Image viewer reads bytes.
        ViewerKind::Hex | ViewerKind::Image | ViewerKind::Audio | ViewerKind::Video => {
            Ok(LoadedFile::Binary(Arc::new(BinaryFile {
                path: display_path,
                bytes,
            })))
        }
    }
}

/// Fetches a depot file as bytes and loads it through a temp copy under `temp_root`. The bytes
/// are never decoded here, so binary content reaches sniffing intact.
pub fn load_depot_file(
    display_path: UniversalPath,
    temp_root: &Path,
    p4: &impl P4Runner,
) -> Result<LoadedFile, String> {
    let UniversalPath::Depot(depot_str, rev) = &display_path else {
        return Err(format!("{display_path:?} is not a depot path"));
    };
    let revision = rev.map(|r| r.to_string()).unwrap_or_default();
    let bytes = print_file(p4, depot_str, &revision).map_err(|e| {
        format!(
            "P4 command failed for {}: {}",
            display_path.to_p4_string(),
            e
        )
    })?;
    let name = depot_str.rsplit('/').next().unwrap_or_default();
    let mut temp = tempfile::Builder::new()
        .suffix(name)
        .tempfile_in(temp_root)
        .map_err(|e| format!("Failed to create a temp file for {display_path}: {e}"))?;
    temp.write_all(&bytes)
        .map_err(|e| format!("Failed to write P4 content to temp file: {e}"))?;
    let loaded = load_file(display_path.clone(), temp.path())
        .map_err(|e| format!("Cannot load file {}, Error: {e}", temp.path().display()));
    if let Err(e) = temp.close() {
        log::warn!("Removing the temp copy of {display_path}: {e}");
    }
    loaded
}

pub fn load_git_file(
    display_path: UniversalPath,
    temp_root: &Path,
    git: &impl GitRunner,
) -> Result<LoadedFile, String> {
    let UniversalPath::Git(path, rev) = &display_path else {
        return Err(format!("{display_path:?} is not a git revision path"));
    };
    let bytes = git::locate(git, path)
        .and_then(|location| match rev {
            GitRev::Index => git::read_index_file(git, &location.root, &location.rel),
            GitRev::Rev(rev) => git::read_file(git, &location.root, rev, &location.rel),
        })
        .map_err(|e| format!("Cannot load {display_path}: {e}"))?;

    let mut temp = tempfile::Builder::new()
        .suffix(path.file_name().unwrap_or_default())
        .tempfile_in(temp_root)
        .map_err(|e| format!("Failed to create a temp file for {display_path}: {e}"))?;
    temp.write_all(&bytes)
        .map_err(|e| format!("Failed to write git content to temp file: {e}"))?;
    load_file(display_path.clone(), temp.path())
        .map_err(|e| format!("Cannot load file {}, Error: {e}", temp.path().display()))
}

#[derive(Debug, Clone, PartialEq)]
struct LoadKey {
    path: UniversalPath,
    local_stamp: Option<(u64, SystemTime)>,
    pdb_options: PdbDumpOptions,
}

const LOAD_CACHE_BYTES: usize = 32 << 20;
type LoadCache = Arc<Mutex<LruCache<LoadKey, LoadedFile>>>;

fn default_load_cache() -> LoadCache {
    Arc::new(Mutex::new(LruCache::new(
        DEFAULT_MAX_ENTRIES,
        LOAD_CACHE_BYTES,
    )))
}

fn load_key(path: &UniversalPath, pdb_options: &PdbDumpOptions) -> Option<LoadKey> {
    let local_stamp = match path {
        UniversalPath::Local(p) => {
            let meta = std::fs::metadata(p).ok()?;
            Some((meta.len(), meta.modified().ok()?))
        }
        UniversalPath::Depot(_, Some(DepotRev::Rev(_) | DepotRev::Change(_))) => None,
        UniversalPath::Git(_, GitRev::Rev(rev)) if is_full_commit_sha(rev) => None,
        UniversalPath::Depot(..) | UniversalPath::Git(..) => return None,
    };
    Some(LoadKey {
        path: path.clone(),
        local_stamp,
        pdb_options: pdb_options.clone(),
    })
}

fn is_full_commit_sha(rev: &str) -> bool {
    matches!(rev.len(), 40 | 64) && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

fn lock_load_cache(cache: &LoadCache) -> std::sync::MutexGuard<'_, LruCache<LoadKey, LoadedFile>> {
    cache
        .lock()
        .expect("the load cache lock is only held for a lookup or insert")
}

fn default_channel() -> (mpsc::Sender<UniversalPath>, mpsc::Receiver<UniversalPath>) {
    mpsc::channel()
}
type LoadResult = Result<LoadedFile, String>;

fn default_file_path() -> UniversalPath {
    UniversalPath::new("")
}

#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FileProcessor {
    #[cfg_attr(feature = "serde", serde(skip, default = "default_channel"))]
    channel: (mpsc::Sender<UniversalPath>, mpsc::Receiver<UniversalPath>),
    #[cfg_attr(feature = "serde", serde(skip, default = "default_file_path"))]
    file_path: UniversalPath,

    #[cfg_attr(feature = "serde", serde(skip))]
    root_path: Option<UniversalPath>,

    #[cfg_attr(feature = "serde", serde(skip))]
    loaded_file: Option<LoadedFile>,

    #[cfg_attr(feature = "serde", serde(skip))]
    cached_file_path: Option<UniversalPath>, // only process once the path

    // Made on the first load: serde defaults each skipped field alone, so it couldn't share `repaint`.
    #[cfg_attr(feature = "serde", serde(skip))]
    load_job: Option<Job<LoadResult>>,
    #[cfg_attr(feature = "serde", serde(skip))]
    repaint: RepaintContext,
    #[cfg_attr(feature = "serde", serde(skip))]
    loading_path: Option<UniversalPath>,
    #[cfg_attr(feature = "serde", serde(skip))]
    label: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip))]
    load_error: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip))]
    pdb_options: PdbDumpOptions,
    #[cfg_attr(feature = "serde", serde(skip, default = "default_load_cache"))]
    load_cache: LoadCache,
}

impl Default for FileProcessor {
    fn default() -> Self {
        Self {
            channel: default_channel(),
            file_path: default_file_path(),
            loaded_file: None,
            cached_file_path: None,
            root_path: None,
            load_job: None,
            repaint: RepaintContext::default(),
            loading_path: None,
            label: None,
            load_error: None,
            pdb_options: PdbDumpOptions::default(),
            load_cache: default_load_cache(),
        }
    }
}

#[allow(dead_code)]
impl FileProcessor {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set_repaint(&mut self, ctx: &egui::Context) {
        let _ = self.repaint.set(ctx.clone());
    }
    pub fn get_tx(&self) -> mpsc::Sender<UniversalPath> {
        self.channel.0.clone()
    }
    pub fn get_rx(&self) -> &mpsc::Receiver<UniversalPath> {
        &self.channel.1
    }

    pub fn poll_path_channel(&mut self) {
        while let Ok(path) = self.channel.1.try_recv() {
            self.set_path(path);
        }
    }

    pub fn get_path(&mut self) -> UniversalPath {
        self.poll_path_channel();

        if let Some(root) = &self.root_path
            && let Some(stripped_path) = Self::strip_root_prefix(root, &self.file_path)
        {
            return stripped_path;
        }

        self.file_path.clone()
    }

    pub fn get_full_path(&mut self) -> UniversalPath {
        self.poll_path_channel();

        self.file_path.clone()
    }

    pub fn get_path_as_string(&self) -> String {
        match &self.file_path {
            UniversalPath::Git(_, rev) => format!("{}@git:{rev}", self.file_path.to_p4_string()),
            path => path.to_p4_string(),
        }
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    pub fn set_label(&mut self, label: Option<String>) {
        self.label = label;
    }

    pub fn display_name(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| self.get_path_as_string())
    }

    pub fn set_path(&mut self, path: UniversalPath) {
        let old_path = self.file_path.clone();
        log::debug!(
            "set_path {} (root: {})",
            path,
            self.root_path
                .as_ref()
                .map_or("none".to_string(), |root| root.to_string())
        );

        if let Some(root) = &self.root_path {
            if Self::is_root_valid(root, &path) {
                log::debug!("is_root_valid {:?}", true);
                log::debug!("is_root_depot {:?}", root.is_depot());
                let mut new_path = root.clone();
                new_path.append(path.clone());
                // append keeps the root's kind and revision; the path's own revision wins.
                if let (
                    UniversalPath::Local(joined) | UniversalPath::Git(joined, _),
                    UniversalPath::Git(_, rev),
                ) = (&new_path, &path)
                {
                    new_path = UniversalPath::Git(joined.clone(), rev.clone());
                }
                self.file_path = new_path
            } else {
                self.file_path = path;
            }
        } else {
            self.file_path = path;
        }

        if old_path != self.file_path {
            self.forget_loaded_file();
            self.label = None;
        }
    }

    // Not set_path: that joins the root, so a full depot path under a depot root would double.
    pub fn restore_path(&mut self, path: UniversalPath, label: Option<String>) {
        if self.file_path != path {
            self.file_path = path;
            self.forget_loaded_file();
        }
        self.label = label;
    }

    pub fn set_root(&mut self, root: UniversalPath) {
        self.root_path = Some(root);
    }
    pub fn clear_root(&mut self) {
        self.root_path = None;
    }
    pub fn get_root(&mut self) -> Option<UniversalPath> {
        self.root_path.clone()
    }

    pub fn strip_root_prefix(root: &UniversalPath, path: &UniversalPath) -> Option<UniversalPath> {
        match (root, path) {
            (UniversalPath::Local(root_local), UniversalPath::Local(path_local)) => {
                let norm_root = Self::normalize_path(root_local);
                let norm_path = Self::normalize_path(path_local);

                norm_path
                    .strip_prefix(&norm_root)
                    .ok()
                    .map(|p| UniversalPath::Local(p.to_path_buf()))
            }

            (UniversalPath::Depot(root_depot, _), UniversalPath::Depot(path_depot, rev)) => {
                let root = root_depot.trim_end_matches('/');

                if path_depot == root {
                    Some(UniversalPath::Depot(String::new(), *rev))
                } else {
                    path_depot
                        .strip_prefix(&(root.to_owned() + "/"))
                        .map(|s| UniversalPath::Depot(s.to_string(), *rev))
                }
            }

            _ => Some(path.clone()),
        }
    }

    pub fn is_root_valid(root: &UniversalPath, path: &UniversalPath) -> bool {
        match (root, path) {
            (
                UniversalPath::Local(root_local),
                UniversalPath::Local(path_local) | UniversalPath::Git(path_local, _),
            ) => {
                let norm_root = Self::normalize_path(root_local);
                let norm_path = Self::normalize_path(path_local);
                norm_path.starts_with(norm_root)
            }

            (UniversalPath::Depot(root_depot, _), UniversalPath::Depot(path_depot, _)) => {
                let root = root_depot.trim_end_matches('/');

                path_depot == root || path_depot.starts_with(&(root.to_owned() + "/"))
            }

            _ => true,
        }
    }

    fn normalize_path(path: &std::path::Path) -> PathBuf {
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                std::path::Component::CurDir => {}
                _ => normalized.push(component),
            }
        }
        normalized
    }

    pub fn invalidate_cache_file(&mut self) {
        log::debug!("Invalidating cache file for path: {}", self.file_path);
        lock_load_cache(&self.load_cache).retain(|key| key.path != self.file_path);
        self.forget_loaded_file();
    }

    fn forget_loaded_file(&mut self) {
        self.loaded_file = None;
        self.cached_file_path = None;
        self.load_error = None;
    }

    pub fn set_pdb_options(&mut self, options: &PdbDumpOptions) {
        if self.pdb_options == *options {
            return;
        }
        self.pdb_options = options.clone();
        if is_pdb_path(&self.file_path) {
            self.forget_loaded_file();
        }
    }

    pub fn get_loading_path(&self) -> Option<&UniversalPath> {
        self.loading_path.as_ref()
    }

    pub fn root_valid(&mut self) -> bool {
        let root = self.root_path.clone().unwrap_or_default();
        self.loading_path.is_some() || Self::is_root_valid(&root, &self.get_full_path())
    }

    pub fn path_valid(&mut self) -> bool {
        self.get_full_path().is_empty()
            || self.loading_path.is_some()
            || self.get_loaded_file().is_some()
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    /// The loaded file only when it is text; a binary load gives `None`.
    pub fn get_cached_file(&mut self) -> Option<Arc<CachedFile<RawToken>>> {
        self.get_loaded_file().and_then(|file| file.text().cloned())
    }

    pub fn get_loaded_file(&mut self) -> Option<LoadedFile> {
        for loaded in self.load_job.as_mut().map(Job::poll).unwrap_or_default() {
            (self.loaded_file, self.load_error) = match loaded {
                Ok(file) => (Some(file), None),
                Err(e) => (None, Some(e)),
            };
            self.loading_path = None;
        }

        let path = &self.get_full_path();

        if !path.is_empty() && self.cached_file_path.as_ref() != Some(path) {
            self.cached_file_path = Some(path.clone());
            self.loaded_file = None;
            self.load_error = None;
            self.loading_path = Some(path.clone());

            log::debug!("Loading file asynchronously: {}", path);

            let path_clone = path.clone();
            let pdb_options = self.pdb_options.clone();
            let cache = self.load_cache.clone();

            let load_job = self
                .load_job
                .get_or_insert_with(|| repaint_job(&self.repaint));
            load_job.start(move |ctx| {
                let key = load_key(&path_clone, &pdb_options);
                let hit = key
                    .as_ref()
                    .and_then(|key| lock_load_cache(&cache).get(key).cloned());
                if let Some(hit) = hit {
                    log::debug!("Loaded {path_clone} from the load cache");
                    ctx.send(Ok(hit));
                    return;
                }
                let loaded = match &path_clone {
                    UniversalPath::Local(p) => load_file(path_clone.clone(), p)
                        .map_err(|e| format!("Cannot load file {}, Error: {e}", p.display())),
                    UniversalPath::Depot(..) => load_depot_file(
                        path_clone.clone(),
                        &std::env::temp_dir(),
                        &P4Command::new(false).for_file(path_clone.clone()),
                    ),
                    UniversalPath::Git(..) => load_git_file(
                        path_clone.clone(),
                        &std::env::temp_dir(),
                        &GitCommand::default(),
                    ),
                };
                let loaded = loaded
                    .map(|file| with_pdb_dump(file, &pdb_options))
                    .inspect_err(|e| log::error!("{e}"));
                // A write during the read leaves contents the first stamp doesn't describe.
                if let (Ok(file), Some(key)) = (&loaded, key)
                    && load_key(&path_clone, &pdb_options).as_ref() == Some(&key)
                {
                    lock_load_cache(&cache).insert(key, file.clone(), file.approx_bytes());
                }

                ctx.send(loaded);
            });
        }

        self.loaded_file.clone()
    }

    pub fn get_cached_file_hash(&mut self) -> Option<zcommon::hash::Blake3Hash> {
        self.get_cached_file().as_ref().map(|f| f.hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewer::ViewerKind;
    use std::path::PathBuf;
    use zcommon::vcs::git::{GitError, GitErrorKind};
    use zcommon::vcs::p4::FakeP4;
    #[test]
    fn test_is_root_valid_local() {
        let root = UniversalPath::Local(PathBuf::from(r"E:\Github\ZDiff\crates"));

        assert!(FileProcessor::is_root_valid(
            &root,
            &UniversalPath::Local(PathBuf::from(
                r"E:\Github\ZDiff\crates\zdiff-gui\src\main.rs",
            ))
        ));

        assert!(!FileProcessor::is_root_valid(
            &root,
            &UniversalPath::Local(PathBuf::from(r"C:\Other\Project\src\main.rs"))
        ));

        assert!(!FileProcessor::is_root_valid(
            &root,
            &UniversalPath::Local(PathBuf::from(
                r"E:\Github\ZDiff\crates\zdiff-gui\..\..\test\rust_files_diff_1\advanced_rust.rs",
            ))
        ));
    }

    #[test]
    fn test_is_root_valid_depot() {
        let cases = [
            (
                UniversalPath::Depot("//depot/folder".into(), None),
                UniversalPath::Depot("//depot/folder/file.rs".into(), None),
                true,
            ),
            (
                UniversalPath::Depot("//depot/folder".into(), None),
                UniversalPath::Depot("//depot/folder/file.rs".into(), Some(DepotRev::Rev(5))),
                true,
            ),
            (
                UniversalPath::Depot("//depot/folder".into(), None),
                UniversalPath::Depot("//depot/other/file.rs".into(), None),
                false,
            ),
            (
                UniversalPath::Depot("//depot_2/one_deeper".into(), None),
                UniversalPath::Depot("//depot_2/one_deeper/test_folder/test_2.txt".into(), None),
                true,
            ),
            (
                UniversalPath::Depot("//depot_2/one_deeper/".into(), None),
                UniversalPath::Depot("//depot_2/one_deeper/test_folder/test_2.txt".into(), None),
                true,
            ),
            (
                UniversalPath::Depot("//depot_2/one_deeper".into(), None),
                UniversalPath::Depot("//depot_2/one_deeper".into(), None),
                true,
            ),
            (
                UniversalPath::Depot("//depot_2/one_deeper/".into(), None),
                UniversalPath::Depot("//depot_2/one_deeper/file.rs".into(), None),
                true,
            ),
            (
                UniversalPath::Depot("//depot/folder".into(), None),
                UniversalPath::Depot("//depot/folder2/file.rs".into(), None),
                false,
            ),
        ];

        for (root, path, expected) in cases {
            assert_eq!(
                FileProcessor::is_root_valid(&root, &path),
                expected,
                "root={root:?}, path={path:?}"
            );
        }
    }

    #[test]
    fn a_root_that_does_not_hold_the_path_is_invalid() {
        let mut file = FileProcessor::new();
        file.set_root(UniversalPath::Local(PathBuf::from(r"E:\root")));

        file.restore_path(UniversalPath::Local(PathBuf::from(r"C:\other\a.txt")), None);
        assert!(!file.root_valid());

        file.restore_path(UniversalPath::Local(PathBuf::from(r"E:\root\a.txt")), None);
        assert!(file.root_valid());
    }

    #[test]
    fn an_unset_path_is_not_invalid() {
        assert!(FileProcessor::new().path_valid());
    }

    #[test]
    fn test_is_root_valid_mixed() {
        let local = UniversalPath::Local(PathBuf::from(r"E:\Github\ZDiff"));
        let depot = UniversalPath::Depot("//depot/folder/file.rs".into(), Some(DepotRev::Rev(2)));

        assert!(FileProcessor::is_root_valid(&local, &depot));
        assert!(FileProcessor::is_root_valid(&depot, &local));
    }

    fn load_bytes(bytes: &[u8]) -> LoadedFile {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.bin");
        std::fs::write(&path, bytes).unwrap();
        load_file(UniversalPath::from(path.clone()), &path).unwrap()
    }

    #[test]
    fn valid_utf8_loads_as_text() {
        let loaded = load_bytes("fn main() {}\n// ünïcode\n".as_bytes());
        assert_eq!(loaded.viewer_kind(), ViewerKind::Text);
        let LoadedFile::Text(file) = loaded else {
            panic!("expected text");
        };
        assert_eq!(file.contents, "fn main() {}\n// ünïcode\n");
    }

    #[test]
    fn empty_file_loads_as_text() {
        assert_eq!(load_bytes(b"").viewer_kind(), ViewerKind::Text);
    }

    #[test]
    fn nul_containing_content_loads_as_bytes_for_hex() {
        // Valid UTF-8 apart from the NUL rule, so only the NUL makes it binary.
        let bytes = b"PK\x03\x04\x00\x00abc";
        let loaded = load_bytes(bytes);
        assert_eq!(loaded.viewer_kind(), ViewerKind::Hex);
        assert_eq!(loaded.bytes(), bytes);
    }

    fn load_named(name: &str, bytes: &[u8]) -> LoadedFile {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let loaded = load_file(UniversalPath::from(path.clone()), &path).unwrap();
        with_pdb_dump(loaded, &PdbDumpOptions::default())
    }

    #[test]
    fn a_pdb_named_file_that_is_no_msf_pdb_loads_as_before() {
        let protein = load_named("protein.pdb", b"HEADER    PROTEIN\nATOM      1  N\n");
        assert!(matches!(protein, LoadedFile::Text(_)));

        for bytes in [
            &b"\x00\x01binary"[..],
            &b"Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0truncated"[..],
        ] {
            let loaded = load_named("broken.pdb", bytes);
            assert!(matches!(loaded, LoadedFile::Binary(_)));
            assert_eq!(loaded.bytes(), bytes);
        }
    }

    #[cfg(all(windows, target_env = "msvc"))]
    #[test]
    fn a_real_pdb_loads_as_its_dump_and_keeps_its_bytes() {
        let path = std::env::current_exe().unwrap().with_extension("pdb");
        let display = UniversalPath::from(path.clone());

        let loaded = with_pdb_dump(
            load_file(display.clone(), &path).unwrap(),
            &PdbDumpOptions::default(),
        );

        assert!(loaded.is_dump());
        assert_eq!(loaded.viewer_kind(), ViewerKind::Text);
        assert_eq!(loaded.path(), &display);
        assert_eq!(loaded.bytes(), std::fs::read(&path).unwrap());
        let text = &loaded.text().unwrap().contents;
        assert!(text.starts_with("== Header ==\n"), "{}", &text[..200]);
        assert!(!text.contains("\nGUID: "));
    }

    #[test]
    fn changing_the_pdb_options_reloads_only_a_pdb() {
        let dir = tempfile::tempdir().unwrap();
        let wait = |file: &mut FileProcessor| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while file.get_loaded_file().is_none() {
                assert!(std::time::Instant::now() < deadline, "load never finished");
                std::thread::yield_now();
            }
        };
        let changed = PdbDumpOptions {
            ignore_identity: false,
            ..Default::default()
        };
        for (name, reloads) in [("a.pdb", true), ("a.txt", false)] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"\x00bytes").unwrap();
            let mut file = FileProcessor::new();
            file.set_path(UniversalPath::from(path));
            wait(&mut file);

            file.set_pdb_options(&PdbDumpOptions::default());
            assert!(file.get_loading_path().is_none(), "{name}");
            file.set_pdb_options(&changed);
            assert_eq!(file.get_loaded_file().is_none(), reloads, "{name}");
            wait(&mut file);
        }
    }

    struct BytesP4<'a>(Result<&'a [u8], &'a str>);

    impl P4Runner for BytesP4<'_> {
        fn run(&self, _: &[&str]) -> Result<String, String> {
            unreachable!("depot loads read bytes")
        }

        fn run_bytes(&self, _: &[&str]) -> Result<Vec<u8>, String> {
            self.0.map(<[u8]>::to_vec).map_err(str::to_string)
        }
    }

    fn load_depot(
        temp_root: &Path,
        depot: &str,
        rev: Option<u32>,
        fetched: Result<&[u8], &str>,
    ) -> Result<LoadedFile, String> {
        let path = UniversalPath::Depot(depot.into(), rev.map(DepotRev::Rev));
        load_depot_file(path, temp_root, &BytesP4(fetched))
    }

    fn print_args(file: &str) -> Vec<Vec<String>> {
        vec![vec!["print".into(), "-q".into(), file.into()]]
    }

    #[test]
    fn depot_changelist_prints_the_file_at_the_change() {
        let temp = tempfile::tempdir().unwrap();
        let p4 = FakeP4::new(|_: &[&str]| Ok("at change\n".to_string()));
        let path = UniversalPath::Depot("//depot/src/a.rs".into(), Some(DepotRev::Change(1234)));

        let loaded = load_depot_file(path.clone(), temp.path(), &p4).unwrap();

        assert_eq!(p4.calls(), print_args("//depot/src/a.rs@1234"));
        assert_eq!(loaded.bytes(), b"at change\n");
        assert_eq!(loaded.path(), &path);
    }

    #[test]
    fn depot_shelf_prints_the_shelved_file() {
        let temp = tempfile::tempdir().unwrap();
        let p4 = FakeP4::new(|_: &[&str]| Ok("shelved\n".to_string()));
        let path = UniversalPath::Depot("//depot/src/a.rs".into(), Some(DepotRev::Shelf(77)));

        let loaded = load_depot_file(path.clone(), temp.path(), &p4).unwrap();

        assert_eq!(p4.calls(), print_args("//depot/src/a.rs@=77"));
        assert_eq!(loaded.bytes(), b"shelved\n");
        assert_eq!(loaded.path(), &path);
    }

    #[test]
    fn depot_head_and_revision_print_the_plain_path_and_rev() {
        let temp = tempfile::tempdir().unwrap();
        let p4 = FakeP4::new(|_: &[&str]| Ok("x".to_string()));
        for rev in [None, Some(DepotRev::Rev(3))] {
            let path = UniversalPath::Depot("//depot/src/a.rs".into(), rev);
            load_depot_file(path, temp.path(), &p4).unwrap();
        }
        let mut expected = print_args("//depot/src/a.rs");
        expected.extend(print_args("//depot/src/a.rs#3"));
        assert_eq!(p4.calls(), expected);
    }

    #[test]
    fn non_utf8_depot_content_loads_as_bytes_for_hex() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"\x89PNG\r\n\x1a\n\xff\xfe\x00caf\xe9";
        let loaded = load_depot(temp.path(), "//depot/art/logo.png", Some(3), Ok(bytes)).unwrap();
        assert_eq!(loaded.viewer_kind(), ViewerKind::Hex);
        assert_eq!(loaded.bytes(), bytes);
        assert_eq!(
            loaded.path(),
            &UniversalPath::Depot("//depot/art/logo.png".into(), Some(DepotRev::Rev(3)))
        );
    }

    #[test]
    fn utf8_depot_content_still_loads_as_text() {
        let temp = tempfile::tempdir().unwrap();
        let text = "fn main() {}\n// ünïcode\n";
        let loaded = load_depot(
            temp.path(),
            "//depot/src/main.rs",
            None,
            Ok(text.as_bytes()),
        )
        .unwrap();
        assert_eq!(loaded.viewer_kind(), ViewerKind::Text);
        assert_eq!(loaded.text().unwrap().contents, text);
        assert_eq!(
            loaded.path(),
            &UniversalPath::Depot("//depot/src/main.rs".into(), None)
        );
    }

    #[test]
    fn depot_fetch_failure_loads_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let result = load_depot(temp.path(), "//depot/a.bin", None, Err("no such file(s)"));
        assert!(result.unwrap_err().contains("no such file(s)"));
    }

    #[test]
    fn depot_temp_copy_is_removed_after_loading() {
        let temp = tempfile::tempdir().unwrap();
        load_depot(temp.path(), "//depot/dir/a.bin", Some(2), Ok(b"\x00\x01")).unwrap();
        load_depot(temp.path(), "//depot/dir/b.txt", None, Ok(b"text")).unwrap();
        let left: Vec<_> = walk_files(temp.path());
        assert!(left.is_empty(), "{left:?}");
    }

    fn walk_files(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(walk_files(&path));
            } else {
                files.push(path);
            }
        }
        files
    }

    #[test]
    fn invalid_utf8_loads_as_bytes_for_hex() {
        let bytes = b"caf\xe9 au lait";
        let loaded = load_bytes(bytes);
        assert_eq!(loaded.viewer_kind(), ViewerKind::Hex);
        assert_eq!(loaded.bytes(), bytes);
    }

    fn labelled(path: &str, label: &str) -> FileProcessor {
        let mut file = FileProcessor::new();
        file.set_path(UniversalPath::new(path));
        file.set_label(Some(label.to_string()));
        file
    }

    #[cfg(feature = "serde")]
    #[test]
    fn label_is_not_serialized() {
        let file = labelled("a.txt", "depot rev 3");
        let json = serde_json::to_string(&file).unwrap();
        assert!(!json.contains("depot rev 3"), "{json}");
        let restored: FileProcessor = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.label(), None);
    }

    #[test]
    fn label_is_kept_when_the_same_path_is_set() {
        let mut file = labelled("a.txt", "A");
        file.set_path(UniversalPath::new("a.txt"));
        assert_eq!(file.label(), Some("A"));
    }

    #[test]
    fn label_is_cleared_when_the_path_changes() {
        let mut file = labelled("a.txt", "A");
        file.set_path(UniversalPath::new("b.txt"));
        assert_eq!(file.label(), None);
    }

    #[test]
    fn label_is_cleared_when_a_new_path_arrives_on_the_channel() {
        let mut file = labelled("a.txt", "A");
        file.get_tx().send(UniversalPath::new("b.txt")).unwrap();
        file.get_path();
        assert_eq!(file.label(), None);
    }

    #[test]
    fn restore_path_keeps_a_full_depot_path_under_a_depot_root_and_its_label() {
        let mut file = FileProcessor::new();
        file.set_root(UniversalPath::new("//depot/main"));
        let full = UniversalPath::new("//depot/main/src/a.rs#3");
        file.restore_path(full.clone(), Some("Shelf base".to_owned()));
        assert_eq!(file.get_full_path(), full);
        assert_eq!(file.label(), Some("Shelf base"));

        file.restore_path(UniversalPath::new("//depot/main/b.rs"), None);
        assert_eq!(file.label(), None);
    }

    #[test]
    fn display_name_is_the_label_else_the_path() {
        let mut file = labelled("a.txt", "A");
        assert_eq!(file.display_name(), "A");
        file.set_label(None);
        assert_eq!(file.display_name(), file.get_path_as_string());
    }

    struct FakeGit<F: Fn(&[&str]) -> Result<Vec<u8>, GitError>>(F);

    impl<F: Fn(&[&str]) -> Result<Vec<u8>, GitError>> GitRunner for FakeGit<F> {
        fn run(&self, _cwd: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
            (self.0)(args)
        }
    }

    fn load_git(
        rev: GitRev,
        expected_object: &str,
        fetched: Result<&[u8], &str>,
    ) -> Result<LoadedFile, String> {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join("src")).unwrap();
        let root = repo.path().display().to_string();
        let git = FakeGit(|args: &[&str]| match args[0] {
            "rev-parse" => Ok(format!("{root}\nsrc/\n").into_bytes()),
            "cat-file" => {
                assert_eq!(args.last(), Some(&expected_object), "{args:?}");
                assert!(args.contains(&"--filters"), "{args:?}");
                fetched.map(<[u8]>::to_vec).map_err(|stderr| GitError {
                    command: args.join(" "),
                    kind: GitErrorKind::Failed {
                        status: "exit code: 128".into(),
                        stderr: stderr.into(),
                    },
                })
            }
            _ => panic!("unexpected git {args:?}"),
        });
        let temp = tempfile::tempdir().unwrap();
        let path = UniversalPath::Git(repo.path().join("src").join("a.rs"), rev);
        let loaded = load_git_file(path.clone(), temp.path(), &git);
        if let Ok(loaded) = &loaded {
            assert_eq!(loaded.path(), &path);
        }
        assert!(walk_files(temp.path()).is_empty());
        loaded
    }

    #[test]
    fn git_revision_loads_the_fetched_bytes() {
        let text = "fn main() {}\r\n";
        let loaded = load_git(
            GitRev::Rev("main".into()),
            "main:src/a.rs",
            Ok(text.as_bytes()),
        )
        .unwrap();
        assert_eq!(loaded.text().unwrap().contents, text);
    }

    #[test]
    fn git_index_loads_the_staged_bytes() {
        let bytes = b"\x00\x01staged";
        let loaded = load_git(GitRev::Index, ":src/a.rs", Ok(bytes)).unwrap();
        assert_eq!(loaded.viewer_kind(), ViewerKind::Hex);
        assert_eq!(loaded.bytes(), bytes);
    }

    #[test]
    fn git_error_is_the_side_error() {
        let stderr = "fatal: path 'src/a.rs' does not exist in 'main'";
        let err = load_git(GitRev::Rev("main".into()), "main:src/a.rs", Err(stderr)).unwrap_err();
        assert!(err.contains(stderr), "{err}");
    }

    #[test]
    fn git_path_keeps_its_revision_under_a_root() {
        let rev = GitRev::Rev("main".into());
        for (root, path, expected) in [
            ("", r"C:\repo\a.rs", r"C:\repo\a.rs"),
            (r"C:\repo", r"C:\repo\a.rs", r"C:\repo\a.rs"),
            (r"C:\repo@git:dev", r"C:\repo\a.rs", r"C:\repo\a.rs"),
        ] {
            let mut file = FileProcessor::new();
            file.set_root(UniversalPath::new(root));
            file.set_path(UniversalPath::Git(path.into(), rev.clone()));
            assert_eq!(
                file.get_full_path(),
                UniversalPath::Git(expected.into(), rev.clone()),
                "root={root:?}, path={path:?}"
            );
        }
    }

    #[test]
    fn display_name_of_a_git_path_shows_the_revision() {
        let mut file = FileProcessor::new();
        file.set_path(UniversalPath::new(r"C:\repo\a.rs@git:HEAD~3"));
        assert_eq!(file.display_name(), "C:/repo/a.rs@git:HEAD~3");
        file.set_path(UniversalPath::new(r"C:\repo\a.rs@git:index"));
        assert_eq!(file.display_name(), "C:/repo/a.rs@git:index");
    }

    #[test]
    fn load_error_is_kept_for_the_side() {
        let missing = tempfile::tempdir().unwrap().path().join("missing.rs");
        let mut file = FileProcessor::new();
        file.set_path(UniversalPath::from(missing.clone()));
        file.get_loaded_file();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while file.get_loading_path().is_some() {
            assert!(std::time::Instant::now() < deadline, "load never finished");
            std::thread::yield_now();
            file.get_loaded_file();
        }
        let error = file.load_error().expect("a load error");
        assert!(error.contains("missing.rs"), "{error}");

        file.set_path(UniversalPath::new("other.rs"));
        assert_eq!(file.load_error(), None);
    }

    fn loaded(file: &mut FileProcessor) -> LoadedFile {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let loaded = file.get_loaded_file();
            if file.get_loading_path().is_none() {
                return loaded.expect("the load failed");
            }
            assert!(std::time::Instant::now() < deadline, "load never finished");
            std::thread::yield_now();
        }
    }

    #[test]
    fn returning_to_an_unchanged_file_reuses_its_load_and_a_changed_one_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.rs"), dir.path().join("b.rs"));
        std::fs::write(&a, "fn a() {}\n").unwrap();
        std::fs::write(&b, "fn b() {}\n").unwrap();
        let mut file = FileProcessor::new();
        file.set_path(UniversalPath::from(a.clone()));
        let first = loaded(&mut file);
        file.set_path(UniversalPath::from(b.clone()));
        loaded(&mut file);

        file.set_path(UniversalPath::from(a.clone()));
        assert!(loaded(&mut file).is_same_load(&first));

        std::fs::write(&a, "fn a_changed() {}\n").unwrap();
        file.set_path(UniversalPath::from(b));
        loaded(&mut file);
        file.set_path(UniversalPath::from(a));
        let changed = loaded(&mut file);
        assert!(!changed.is_same_load(&first));
        assert_eq!(changed.bytes(), b"fn a_changed() {}\n");
    }

    #[test]
    fn a_refresh_reloads_an_unchanged_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "fn a() {}\n").unwrap();
        let mut file = FileProcessor::new();
        file.set_path(UniversalPath::from(path));
        let first = loaded(&mut file);

        file.invalidate_cache_file();
        assert!(!loaded(&mut file).is_same_load(&first));
    }

    #[test]
    fn an_undo_back_to_an_unchanged_file_reuses_its_load() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.rs"), dir.path().join("b.rs"));
        std::fs::write(&a, "fn a() {}\n").unwrap();
        std::fs::write(&b, "fn b() {}\n").unwrap();
        let mut file = FileProcessor::new();
        file.set_path(UniversalPath::from(a.clone()));
        let first = loaded(&mut file);
        file.set_path(UniversalPath::from(b));
        loaded(&mut file);

        file.restore_path(UniversalPath::from(a), None);
        assert!(loaded(&mut file).is_same_load(&first));
    }

    #[test]
    fn only_sides_that_cannot_change_under_their_path_are_cached() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("a.rs");
        std::fs::write(&local, "fn a() {}\n").unwrap();
        let sha = "0123456789abcdef0123456789abcdef01234567".to_owned();
        let depot = |rev| UniversalPath::Depot("//depot/a.rs".into(), rev);
        let git = |rev| UniversalPath::Git(local.clone(), rev);
        let cases = [
            (UniversalPath::from(local.clone()), true),
            (UniversalPath::from(dir.path().join("missing.rs")), false),
            (depot(None), false),
            (depot(Some(DepotRev::Rev(3))), true),
            (depot(Some(DepotRev::Change(12))), true),
            (depot(Some(DepotRev::Shelf(12))), false),
            (git(GitRev::Index), false),
            (git(GitRev::Rev("main".into())), false),
            (git(GitRev::Rev("HEAD".into())), false),
            (git(GitRev::Rev(sha[..12].into())), false),
            (git(GitRev::Rev(sha)), true),
        ];
        for (path, cached) in cases {
            let key = load_key(&path, &PdbDumpOptions::default());
            assert_eq!(key.is_some(), cached, "{path:?}");
        }
    }

    #[test]
    fn a_finished_load_wakes_the_ui() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "fn a() {}\n").unwrap();
        let repaint = egui::Context::default();
        for _ in 0..3 {
            let _ = repaint.run(egui::RawInput::default(), |_| {});
        }
        let mut file = FileProcessor::new();
        file.set_repaint(&repaint);
        file.set_path(UniversalPath::from(path));
        assert!(!repaint.has_requested_repaint());
        assert!(file.get_loaded_file().is_none());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !repaint.has_requested_repaint() {
            assert!(std::time::Instant::now() < deadline, "no repaint requested");
            std::thread::yield_now();
        }
        assert!(file.get_cached_file().is_some());
    }
}
