use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::SystemTime;

use md5::{Digest, Md5};

use crate::scan::{ContentHash, HashKind, P4Recipe};

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
        && left_hash.kind.compares_with(right_hash.kind)
    {
        return Ok(left_hash.value == right_hash.value);
    }
    let p4_match = match (left.hash, right.hash) {
        (Some(digest), None) => matches_p4_digest(right_reader, right, digest, left.meta.size)?,
        (None, Some(digest)) => matches_p4_digest(left_reader, left, digest, right.meta.size)?,
        _ => None,
    };
    if let Some(same) = p4_match {
        return Ok(same);
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

/// `None` when the digest can't be reproduced from `local`'s bytes.
fn matches_p4_digest(
    reader: &impl FileReader,
    local: &PairSide,
    digest: &ContentHash,
    digest_size: u64,
) -> io::Result<Option<bool>> {
    let HashKind::P4Digest(recipe) = digest.kind else {
        return Ok(None);
    };
    let local_digests = match recipe {
        P4Recipe::ServerOnly => return Ok(None),
        P4Recipe::Raw if local.meta.size != digest_size => return Ok(Some(false)),
        P4Recipe::Raw => local_md5s(reader.open(local.path)?, false)?,
        P4Recipe::Text => local_md5s(reader.open(local.path)?, true)?,
    };
    Ok(Some(
        local_digests
            .iter()
            .any(|local| local.eq_ignore_ascii_case(&digest.value)),
    ))
}

/// MD5 of the bytes and, with `crlf_as_lf`, also of the bytes with each CRLF read as LF.
fn local_md5s(mut file: impl Read, crlf_as_lf: bool) -> io::Result<Vec<String>> {
    let mut raw = Md5::new();
    let mut lf = crlf_as_lf.then(Md5::new);
    let mut buf = vec![0u8; PAIR_CHUNK_SIZE];
    let mut normalized = Vec::with_capacity(PAIR_CHUNK_SIZE);
    // A CR at a chunk end may pair with an LF at the next chunk's start.
    let mut held_cr = false;
    loop {
        let n = read_full(&mut file, &mut buf)?;
        let chunk = &buf[..n];
        raw.update(chunk);
        if let Some(lf) = &mut lf {
            normalized.clear();
            for &byte in chunk {
                if held_cr && byte != b'\n' {
                    normalized.push(b'\r');
                }
                held_cr = byte == b'\r';
                if !held_cr {
                    normalized.push(byte);
                }
            }
            lf.update(&normalized);
        }
        if n < PAIR_CHUNK_SIZE {
            break;
        }
    }
    let hex = |hasher: Md5| -> String {
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect()
    };
    let mut digests = vec![hex(raw)];
    if let Some(mut lf) = lf {
        if held_cr {
            lf.update(b"\r");
        }
        digests.push(hex(lf));
    }
    Ok(digests)
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

type SideVersion = (FileMeta, Option<ContentHash>);

fn side_version(side: &PairSide) -> SideVersion {
    (side.meta, side.hash.cloned())
}

fn is_version_of(version: &SideVersion, side: &PairSide) -> bool {
    version.0 == side.meta && version.1.as_ref() == side.hash
}

#[derive(Debug, Default)]
pub struct PairCompareCache {
    entries: HashMap<String, HashMap<String, (SideVersion, SideVersion, PairState)>>,
}

impl PairCompareCache {
    pub fn get(&self, left: &PairSide, right: &PairSide) -> Option<PairState> {
        let (left_version, right_version, state) =
            self.entries.get(left.rel_path)?.get(right.rel_path)?;
        (is_version_of(left_version, left) && is_version_of(right_version, right)).then_some(*state)
    }

    pub fn insert(&mut self, left: &PairSide, right: &PairSide, state: PairState) {
        self.entries
            .entry(left.rel_path.to_owned())
            .or_default()
            .insert(
                right.rel_path.to_owned(),
                (side_version(left), side_version(right), state),
            );
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
            kind: HashKind::P4Digest(P4Recipe::Raw),
            value: "aaaa".into(),
        };
        let result =
            compare_pair_with(&left, &right, &hashed("a", 10, &git), &hashed("a", 10, &p4));
        assert_eq!(result.state, PairState::Different);
        assert_eq!(left.reads(), (1, 10));
        assert_eq!(right.reads(), (1, 10));
    }

    fn p4(recipe: P4Recipe, content: &[u8]) -> ContentHash {
        ContentHash {
            kind: HashKind::P4Digest(recipe),
            value: Md5::digest(content)
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect(),
        }
    }

    #[test]
    fn a_local_file_matching_a_raw_p4_digest_is_same_without_reading_the_depot_side() {
        let depot = CountingReader::default();
        let local = CountingReader::with(&[("a", vec![3; 10])]);
        let digest = p4(P4Recipe::Raw, &[3; 10]);
        let result = compare_pair_with(
            &depot,
            &local,
            &hashed("a", 10, &digest),
            &side("a", 10, time(1)),
        );
        assert_eq!(result.state, PairState::Same);
        assert!(result.error.is_none());
        assert_eq!(depot.reads(), (0, 0));
        assert_eq!(local.reads(), (1, 10));
    }

    #[test]
    fn a_local_file_differing_from_a_raw_p4_digest_is_different_without_reading_the_depot_side() {
        let local = CountingReader::with(&[("a", vec![3; 10])]);
        let depot = CountingReader::default();
        let digest = p4(P4Recipe::Raw, &[4; 10]);
        let result = compare_pair_with(
            &local,
            &depot,
            &side("a", 10, time(1)),
            &hashed("a", 10, &digest),
        );
        assert_eq!(result.state, PairState::Different);
        assert!(result.error.is_none());
        assert_eq!(depot.reads(), (0, 0));
    }

    #[test]
    fn a_raw_p4_digest_of_another_size_is_different_without_reading() {
        let local = CountingReader::with(&[("a", vec![3; 10])]);
        let depot = CountingReader::default();
        let digest = p4(P4Recipe::Raw, &[3; 10]);
        let result = compare_pair_with(
            &local,
            &depot,
            &side("a", 10, time(1)),
            &hashed("a", 11, &digest),
        );
        assert_eq!(result.state, PairState::Different);
        assert_eq!((local.reads(), depot.reads()), ((0, 0), (0, 0)));
    }

    #[test]
    fn a_crlf_local_file_matches_the_lf_text_digest_of_another_size() {
        let local = CountingReader::with(&[("a", b"a\r\nb\r\n".to_vec())]);
        let depot = CountingReader::default();
        let digest = p4(P4Recipe::Text, b"a\nb\n");
        let result = compare_pair_with(
            &local,
            &depot,
            &side("a", 6, time(1)),
            &hashed("a", 4, &digest),
        );
        assert_eq!(result.state, PairState::Same);
        assert_eq!(depot.reads(), (0, 0));
    }

    #[test]
    fn a_text_digest_of_crlf_content_matches_the_local_file_as_is() {
        let local = CountingReader::with(&[("a", b"a\r\n".to_vec())]);
        let depot = CountingReader::default();
        let digest = p4(P4Recipe::Text, b"a\r\n");
        let result = compare_pair_with(
            &local,
            &depot,
            &side("a", 3, time(1)),
            &hashed("a", 3, &digest),
        );
        assert_eq!(result.state, PairState::Same);
    }

    #[test]
    fn a_crlf_split_across_read_chunks_is_still_one_line_ending() {
        let mut crlf = vec![b'x'; PAIR_CHUNK_SIZE - 1];
        let mut lf = crlf.clone();
        crlf.extend(b"\r\ny");
        lf.extend(b"\ny");
        let local = CountingReader::with(&[("a", crlf.clone())]);
        let depot = CountingReader::default();
        let digest = p4(P4Recipe::Text, &lf);
        let result = compare_pair_with(
            &local,
            &depot,
            &side("a", crlf.len(), time(1)),
            &hashed("a", lf.len(), &digest),
        );
        assert_eq!(result.state, PairState::Same);
    }

    #[test]
    fn a_lone_cr_is_not_a_line_ending_for_a_text_digest() {
        let local = CountingReader::with(&[("a", b"a\rb\r".to_vec())]);
        let depot = CountingReader::default();
        let (dropped, kept) = (p4(P4Recipe::Text, b"ab"), p4(P4Recipe::Text, b"a\rb\r"));
        let compare = |digest| {
            compare_pair_with(
                &local,
                &depot,
                &side("a", 4, time(1)),
                &hashed("a", 4, digest),
            )
            .state
        };
        assert_eq!(compare(&dropped), PairState::Different);
        assert_eq!(compare(&kept), PairState::Same);
    }

    #[test]
    fn a_server_only_p4_digest_compares_content() {
        let local = CountingReader::with(&[("a", vec![3; 10])]);
        let depot = CountingReader::with(&[("a", vec![3; 10])]);
        let digest = p4(P4Recipe::ServerOnly, &[3; 10]);
        let result = compare_pair_with(
            &local,
            &depot,
            &side("a", 10, time(1)),
            &hashed("a", 10, &digest),
        );
        assert_eq!(result.state, PairState::Same);
        assert_eq!((local.reads(), depot.reads()), ((1, 10), (1, 10)));
    }

    #[test]
    fn p4_digests_of_different_recipes_compare_by_value() {
        let reader = CountingReader::default();
        let (raw, text) = (p4(P4Recipe::Raw, b"a"), p4(P4Recipe::Text, b"a"));
        let result = compare_pair(&reader, &hashed("a", 1, &raw), &hashed("b", 1, &text));
        assert_eq!(result.state, PairState::Same);
        assert_eq!(reader.reads(), (0, 0));
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
    fn cache_tells_hashed_sides_apart_by_hash() {
        let reader = CountingReader::with(&[("a", vec![1; 10]), ("b", vec![1; 10])]);
        let mut cache = PairCompareCache::default();
        let (old, new) = (p4(P4Recipe::Raw, &[1; 10]), p4(P4Recipe::Raw, &[2; 10]));
        let local = side("a", 10, time(1));
        cache.insert(&local, &hashed("b", 10, &old), PairState::Same);
        assert_eq!(
            cache.get(&local, &hashed("b", 10, &old)),
            Some(PairState::Same)
        );
        assert_eq!(cache.get(&local, &hashed("b", 10, &new)), None);
        assert_eq!(reader.reads(), (0, 0));
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
