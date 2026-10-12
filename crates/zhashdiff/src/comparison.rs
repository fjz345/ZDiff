use crc32fast::Hasher as Crc32Hasher;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::SystemTime;

use zcommon::hash::hash_file_mmap;

use crate::scan::ContentHash;

pub enum PathComparissonMethod {
    Byte,
    Hash,
    CrC,
}

pub enum PathComparisonResult {
    Byte { likeness: f32 },
    Hash { identical: bool },
    Crc { identical: bool },
}

impl PathComparisonResult {
    pub fn identical(&self) -> bool {
        match self {
            PathComparisonResult::Byte { likeness } => *likeness == 1.0,
            PathComparisonResult::Hash { identical } => *identical,
            PathComparisonResult::Crc { identical } => *identical,
        }
    }

    pub fn likeness(&self) -> f32 {
        match self {
            PathComparisonResult::Byte { likeness } => *likeness,
            PathComparisonResult::Hash { identical } | PathComparisonResult::Crc { identical } => {
                if *identical {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }
}

pub fn compare_paths(
    path1: impl AsRef<Path>,
    path2: impl AsRef<Path>,
    method: &PathComparissonMethod,
) -> io::Result<PathComparisonResult> {
    match method {
        PathComparissonMethod::Byte => compare_bytes(path1, path2),
        PathComparissonMethod::Hash => compare_hash(path1, path2),
        PathComparissonMethod::CrC => compare_crc(path1, path2),
    }
}

pub fn compare_bytes(
    path1: impl AsRef<Path>,
    path2: impl AsRef<Path>,
) -> io::Result<PathComparisonResult> {
    let mut file1 = File::open(path1)?;
    let mut file2 = File::open(path2)?;

    let metadata1 = file1.metadata()?;
    let metadata2 = file2.metadata()?;

    let len1 = metadata1.len();
    let len2 = metadata2.len();

    let max_len = len1.max(len2);

    // both are empty
    if max_len == 0 {
        return Ok(PathComparisonResult::Byte { likeness: 1.0 });
    }

    // TODO: tweak
    const BUF_SIZE: usize = 64 * 1024;
    let mut buf1 = [0u8; BUF_SIZE];
    let mut buf2 = [0u8; BUF_SIZE];

    let mut total_matches: u64 = 0;
    let mut total_compared: u64 = 0;

    loop {
        let n1 = file1.read(&mut buf1)?;
        let n2 = file2.read(&mut buf2)?;

        if n1 == 0 && n2 == 0 {
            break;
        }

        let min_read = n1.min(n2);

        total_matches += buf1[..min_read]
            .iter()
            .zip(&buf2[..min_read])
            .filter(|(a, b)| a == b)
            .count() as u64;

        total_compared += min_read as u64;
    }

    let likeness = total_matches as f32 / total_compared as f32;

    Ok(PathComparisonResult::Byte { likeness })
}

pub fn compare_hash(
    path1: impl AsRef<Path>,
    path2: impl AsRef<Path>,
) -> io::Result<PathComparisonResult> {
    let hash1 = hash_file_mmap(path1)?;
    let hash2 = hash_file_mmap(path2)?;

    Ok(PathComparisonResult::Hash {
        identical: hash1 == hash2,
    })
}

pub fn compare_crc(
    path1: impl AsRef<Path>,
    path2: impl AsRef<Path>,
) -> io::Result<PathComparisonResult> {
    fn crc_of_file(path: impl AsRef<Path>) -> io::Result<u32> {
        let mut file = File::open(path)?;
        let mut hasher = Crc32Hasher::new();
        let mut buffer = [0u8; 8192];

        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }

        Ok(hasher.finalize())
    }

    let crc1 = crc_of_file(path1)?;
    let crc2 = crc_of_file(path2)?;

    Ok(PathComparisonResult::Crc {
        identical: crc1 == crc2,
    })
}

const PAIR_CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMeta {
    pub size: u64,
    pub modified: SystemTime,
}

pub struct PairSide<'a> {
    /// Relative to its root, with `/` separators.
    pub rel_path: &'a str,
    pub path: &'a Path,
    pub meta: FileMeta,
    pub hash: Option<&'a ContentHash>,
}

pub trait FileReader {
    type File: Read;
    fn open(&self, path: &Path) -> io::Result<Self::File>;
}

pub struct DiskReader;

impl FileReader for DiskReader {
    type File = File;
    fn open(&self, path: &Path) -> io::Result<File> {
        File::open(path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairState {
    Same,
    Different,
}

/// `error` is only set together with `PairState::Different`.
#[derive(Debug)]
pub struct PairCompare {
    pub state: PairState,
    pub error: Option<io::Error>,
}

pub fn compare_pair(reader: &impl FileReader, left: &PairSide, right: &PairSide) -> PairCompare {
    compare_pair_with(reader, reader, left, right)
}

/// Hashes of one kind decide without a read; otherwise the content is compared.
pub fn compare_pair_with(
    left_reader: &impl FileReader,
    right_reader: &impl FileReader,
    left: &PairSide,
    right: &PairSide,
) -> PairCompare {
    let (state, error) = match compare_pair_bytes(left_reader, right_reader, left, right) {
        Ok(true) => (PairState::Same, None),
        Ok(false) => (PairState::Different, None),
        Err(e) => (PairState::Different, Some(e)),
    };
    PairCompare { state, error }
}

fn compare_pair_bytes(
    left_reader: &impl FileReader,
    right_reader: &impl FileReader,
    left: &PairSide,
    right: &PairSide,
) -> io::Result<bool> {
    if let (Some(left_hash), Some(right_hash)) = (left.hash, right.hash)
        && left_hash.kind == right_hash.kind
    {
        return Ok(left_hash.value == right_hash.value);
    }
    if left.meta.size != right.meta.size {
        return Ok(false);
    }
    if left.meta.size == 0 {
        return Ok(true);
    }
    let mut file1 = left_reader.open(left.path)?;
    let mut file2 = right_reader.open(right.path)?;
    let mut buf1 = vec![0u8; PAIR_CHUNK_SIZE];
    let mut buf2 = vec![0u8; PAIR_CHUNK_SIZE];
    loop {
        let n1 = read_full(&mut file1, &mut buf1)?;
        let n2 = read_full(&mut file2, &mut buf2)?;
        if buf1[..n1] != buf2[..n2] {
            return Ok(false);
        }
        if n1 < PAIR_CHUNK_SIZE {
            return Ok(true);
        }
    }
}

fn read_full(file: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

#[derive(Debug, Default)]
pub struct PairCompareCache {
    entries: HashMap<(String, String), (FileMeta, FileMeta, PairState)>,
}

impl PairCompareCache {
    pub fn get(&self, left: &PairSide, right: &PairSide) -> Option<PairState> {
        let key = (left.rel_path.to_owned(), right.rel_path.to_owned());
        let (left_meta, right_meta, state) = self.entries.get(&key)?;
        (*left_meta == left.meta && *right_meta == right.meta).then_some(*state)
    }

    pub fn insert(&mut self, left: &PairSide, right: &PairSide, state: PairState) {
        let key = (left.rel_path.to_owned(), right.rel_path.to_owned());
        self.entries.insert(key, (left.meta, right.meta, state));
    }

    /// Read errors are not cached, so the next compare of the pair retries it.
    pub fn compare(
        &mut self,
        reader: &impl FileReader,
        left: &PairSide,
        right: &PairSide,
    ) -> PairCompare {
        if let Some(state) = self.get(left, right) {
            return PairCompare { state, error: None };
        }
        let result = compare_pair(reader, left, right);
        if result.error.is_none() {
            self.insert(left, right, result.state);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::HashKind;
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::time::Duration;

    #[derive(Default)]
    struct CountingReader {
        files: HashMap<PathBuf, Vec<u8>>,
        failing_reads: Vec<PathBuf>,
        opens: Cell<usize>,
        bytes_read: Rc<Cell<u64>>,
    }

    struct CountingFile {
        data: io::Cursor<Vec<u8>>,
        fail: bool,
        bytes_read: Rc<Cell<u64>>,
    }

    impl Read for CountingFile {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.fail {
                return Err(io::Error::other("read failed"));
            }
            let n = self.data.read(buf)?;
            self.bytes_read.set(self.bytes_read.get() + n as u64);
            Ok(n)
        }
    }

    impl FileReader for CountingReader {
        type File = CountingFile;
        fn open(&self, path: &Path) -> io::Result<CountingFile> {
            self.opens.set(self.opens.get() + 1);
            let data = self
                .files
                .get(path)
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            Ok(CountingFile {
                data: io::Cursor::new(data.clone()),
                fail: self.failing_reads.iter().any(|p| p == path),
                bytes_read: self.bytes_read.clone(),
            })
        }
    }

    impl CountingReader {
        fn with(files: &[(&str, Vec<u8>)]) -> Self {
            Self {
                files: files
                    .iter()
                    .map(|(p, d)| (PathBuf::from(p), d.clone()))
                    .collect(),
                ..Default::default()
            }
        }

        fn reads(&self) -> (usize, u64) {
            (self.opens.get(), self.bytes_read.get())
        }

        fn reset(&self) {
            self.opens.set(0);
            self.bytes_read.set(0);
        }
    }

    fn time(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn side(path: &str, size: usize, modified: SystemTime) -> PairSide<'_> {
        PairSide {
            rel_path: path,
            path: Path::new(path),
            meta: FileMeta {
                size: size as u64,
                modified,
            },
            hash: None,
        }
    }

    #[test]
    fn size_difference_is_different_without_reading() {
        let reader = CountingReader::with(&[("a", vec![1; 10]), ("b", vec![1; 11])]);
        let result = compare_pair(&reader, &side("a", 10, time(1)), &side("b", 11, time(1)));
        assert_eq!(result.state, PairState::Different);
        assert!(result.error.is_none());
        assert_eq!(reader.reads(), (0, 0));
    }

    #[test]
    fn first_chunk_difference_exits_early() {
        let len = 3 * PAIR_CHUNK_SIZE;
        let mut b = vec![7; len];
        b[0] = 8;
        let reader = CountingReader::with(&[("a", vec![7; len]), ("b", b)]);
        let result = compare_pair(&reader, &side("a", len, time(1)), &side("b", len, time(1)));
        assert_eq!(result.state, PairState::Different);
        assert!(reader.bytes_read.get() <= 2 * PAIR_CHUNK_SIZE as u64);
    }

    #[test]
    fn last_byte_difference_is_different() {
        let len = 2 * PAIR_CHUNK_SIZE + 5;
        let mut b = vec![7; len];
        b[len - 1] = 8;
        let reader = CountingReader::with(&[("a", vec![7; len]), ("b", b)]);
        let result = compare_pair(&reader, &side("a", len, time(1)), &side("b", len, time(1)));
        assert_eq!(result.state, PairState::Different);
    }

    #[test]
    fn equal_files_are_same() {
        let len = 2 * PAIR_CHUNK_SIZE + 5;
        let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
        let reader = CountingReader::with(&[("a", data.clone()), ("b", data)]);
        let result = compare_pair(&reader, &side("a", len, time(1)), &side("b", len, time(2)));
        assert_eq!(result.state, PairState::Same);
        assert!(result.error.is_none());
        assert_eq!(reader.bytes_read.get(), 2 * len as u64);
    }

    #[test]
    fn empty_files_are_same() {
        let reader = CountingReader::with(&[("a", vec![]), ("b", vec![])]);
        let result = compare_pair(&reader, &side("a", 0, time(1)), &side("b", 0, time(1)));
        assert_eq!(result.state, PairState::Same);
    }

    #[test]
    fn file_shorter_than_its_recorded_size_is_different() {
        let reader = CountingReader::with(&[("a", vec![1; 10]), ("b", vec![1; 8])]);
        let result = compare_pair(&reader, &side("a", 10, time(1)), &side("b", 10, time(1)));
        assert_eq!(result.state, PairState::Different);
    }

    #[test]
    fn read_error_is_different_with_error() {
        let mut reader = CountingReader::with(&[("a", vec![1; 10]), ("b", vec![1; 10])]);
        reader.failing_reads.push(PathBuf::from("b"));
        let result = compare_pair(&reader, &side("a", 10, time(1)), &side("b", 10, time(1)));
        assert_eq!(result.state, PairState::Different);
        assert_eq!(result.error.unwrap().to_string(), "read failed");
    }

    #[test]
    fn open_error_is_different_with_error() {
        let reader = CountingReader::with(&[("a", vec![1; 10])]);
        let result = compare_pair(
            &reader,
            &side("a", 10, time(1)),
            &side("missing", 10, time(1)),
        );
        assert_eq!(result.state, PairState::Different);
        assert_eq!(result.error.unwrap().kind(), io::ErrorKind::NotFound);
    }

    fn blob(value: &str) -> ContentHash {
        ContentHash {
            kind: HashKind::GitBlob,
            value: value.into(),
        }
    }

    fn hashed<'a>(path: &'a str, size: usize, hash: &'a ContentHash) -> PairSide<'a> {
        PairSide {
            hash: Some(hash),
            ..side(path, size, time(1))
        }
    }

    #[test]
    fn equal_hashes_of_one_kind_are_same_without_reading() {
        let reader = CountingReader::with(&[("a", vec![1; 10]), ("b", vec![2; 10])]);
        let (h1, h2) = (blob("aaaa"), blob("aaaa"));
        let result = compare_pair(&reader, &hashed("a", 10, &h1), &hashed("b", 10, &h2));
        assert_eq!(result.state, PairState::Same);
        assert!(result.error.is_none());
        assert_eq!(reader.reads(), (0, 0));
    }

    #[test]
    fn different_hashes_of_one_kind_are_different_without_reading() {
        let reader = CountingReader::with(&[("a", vec![1; 10]), ("b", vec![1; 10])]);
        let (h1, h2) = (blob("aaaa"), blob("bbbb"));
        let result = compare_pair(&reader, &hashed("a", 10, &h1), &hashed("b", 10, &h2));
        assert_eq!(result.state, PairState::Different);
        assert_eq!(reader.reads(), (0, 0));
    }

    #[test]
    fn hash_on_one_side_only_compares_content() {
        let left = CountingReader::with(&[("a", vec![1; 10])]);
        let right = CountingReader::with(&[("a", vec![1; 10])]);
        let hash = blob("aaaa");
        let result = compare_pair_with(
            &left,
            &right,
            &hashed("a", 10, &hash),
            &side("a", 10, time(1)),
        );
        assert_eq!(result.state, PairState::Same);
        assert_eq!(left.reads(), (1, 10));
        assert_eq!(right.reads(), (1, 10));
    }

    #[test]
    fn hashes_of_different_kinds_compare_content_even_when_the_values_are_equal() {
        let left = CountingReader::with(&[("a", vec![1; 10])]);
        let right = CountingReader::with(&[("a", vec![2; 10])]);
        let git = blob("aaaa");
        let p4 = ContentHash {
            kind: HashKind::P4Digest,
            value: "aaaa".into(),
        };
        let result =
            compare_pair_with(&left, &right, &hashed("a", 10, &git), &hashed("a", 10, &p4));
        assert_eq!(result.state, PairState::Different);
        assert_eq!(left.reads(), (1, 10));
        assert_eq!(right.reads(), (1, 10));
    }

    #[test]
    fn each_side_reads_through_its_own_reader() {
        let left = CountingReader::with(&[("a", vec![1; 10])]);
        let right = CountingReader::with(&[("a", vec![2; 10])]);
        let result = compare_pair_with(
            &left,
            &right,
            &side("a", 10, time(1)),
            &side("a", 10, time(1)),
        );
        assert_eq!(result.state, PairState::Different);
        assert_eq!(left.reads().0, 1);
        assert_eq!(right.reads().0, 1);
    }

    #[test]
    fn disk_reader_compares_files() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        std::fs::write(&a, b"same").unwrap();
        std::fs::write(&b, b"same").unwrap();
        std::fs::write(&c, b"diff").unwrap();
        fn side<'a>(rel_path: &'a str, path: &'a Path) -> PairSide<'a> {
            PairSide {
                rel_path,
                path,
                meta: FileMeta {
                    size: 4,
                    modified: std::fs::metadata(path).unwrap().modified().unwrap(),
                },
                hash: None,
            }
        }
        let same = compare_pair(&DiskReader, &side("a", &a), &side("b", &b));
        let diff = compare_pair(&DiskReader, &side("a", &a), &side("c", &c));
        assert_eq!(same.state, PairState::Same);
        assert_eq!(diff.state, PairState::Different);
    }

    #[test]
    fn cache_with_unchanged_metadata_does_no_reads() {
        let reader = CountingReader::with(&[("a", vec![1; 10]), ("b", vec![1; 10])]);
        let mut cache = PairCompareCache::default();
        let (left, right) = (side("a", 10, time(1)), side("b", 10, time(2)));
        assert_eq!(cache.compare(&reader, &left, &right).state, PairState::Same);
        reader.reset();
        let again = cache.compare(&reader, &side("a", 10, time(1)), &side("b", 10, time(2)));
        assert_eq!(again.state, PairState::Same);
        assert_eq!(reader.reads(), (0, 0));
        assert_eq!(cache.get(&left, &right), Some(PairState::Same));
    }

    #[test]
    fn cache_changed_size_forces_reread() {
        let mut reader = CountingReader::with(&[("a", vec![1; 10]), ("b", vec![1; 10])]);
        let mut cache = PairCompareCache::default();
        cache.compare(&reader, &side("a", 10, time(1)), &side("b", 10, time(1)));
        reader.files.insert(PathBuf::from("a"), vec![2; 12]);
        reader.files.insert(PathBuf::from("b"), vec![2; 12]);
        reader.reset();
        let result = cache.compare(&reader, &side("a", 12, time(1)), &side("b", 12, time(1)));
        assert_eq!(result.state, PairState::Same);
        assert_eq!(reader.reads(), (2, 24));
    }

    #[test]
    fn cache_changed_mtime_forces_reread() {
        let mut reader = CountingReader::with(&[("a", vec![1; 10]), ("b", vec![1; 10])]);
        let mut cache = PairCompareCache::default();
        cache.compare(&reader, &side("a", 10, time(1)), &side("b", 10, time(1)));
        reader.files.insert(PathBuf::from("b"), vec![2; 10]);
        reader.reset();
        let result = cache.compare(&reader, &side("a", 10, time(1)), &side("b", 10, time(5)));
        assert_eq!(result.state, PairState::Different);
        assert_eq!(reader.reads(), (2, 20));
        assert_eq!(
            cache.get(&side("a", 10, time(1)), &side("b", 10, time(1))),
            None
        );
    }

    #[test]
    fn cache_keeps_pairs_apart_by_relative_path() {
        let reader =
            CountingReader::with(&[("a", vec![1; 10]), ("b", vec![1; 10]), ("c", vec![2; 10])]);
        let mut cache = PairCompareCache::default();
        cache.compare(&reader, &side("a", 10, time(1)), &side("b", 10, time(1)));
        let result = cache.compare(&reader, &side("a", 10, time(1)), &side("c", 10, time(1)));
        assert_eq!(result.state, PairState::Different);
    }

    #[test]
    fn cache_does_not_keep_read_errors() {
        let mut reader = CountingReader::with(&[("a", vec![1; 10]), ("b", vec![1; 10])]);
        reader.failing_reads.push(PathBuf::from("b"));
        let mut cache = PairCompareCache::default();
        let (left, right) = (side("a", 10, time(1)), side("b", 10, time(1)));
        assert!(cache.compare(&reader, &left, &right).error.is_some());
        assert_eq!(cache.get(&left, &right), None);
        reader.failing_reads.clear();
        assert_eq!(cache.compare(&reader, &left, &right).state, PairState::Same);
    }
}
