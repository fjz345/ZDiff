use std::{
    collections::{HashMap, HashSet},
    fmt, io,
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use crate::comparison::{DiskReader, FileMeta, FileReader};

pub const HEADER_BYTES: usize = 64 * 1024;

const PACKAGE_FILE_TAG: u32 = 0x9E2A_83C1;
// Enum values from ObjectVersion.h (UE 5.6).
const UE5_PACKAGE_SAVED_HASH: i32 = 1016;
const UE4_NAME_HASHES_SERIALIZED: i32 = 503;

/// Not `BlueprintGeneratedClass`: assets that merely reference a Blueprint class (data assets,
/// external actors) import it too, while every Blueprint also has its `...Blueprint` class name.
pub fn is_blueprint(header: &[u8]) -> bool {
    package_names(header).iter().any(|name| {
        name.ends_with("Blueprint") && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn read_header(reader: &impl FileReader, path: &Path) -> io::Result<Vec<u8>> {
    let mut header = Vec::new();
    reader
        .open(path)?
        .take(HEADER_BYTES as u64)
        .read_to_end(&mut header)?;
    Ok(header)
}

type DetectorResult = (PathBuf, FileMeta, io::Result<bool>);

/// Session cache of [`is_blueprint`] by path, size and mtime, filled by one worker thread.
pub struct BlueprintDetector {
    cache: HashMap<PathBuf, (FileMeta, bool)>,
    pending: HashSet<PathBuf>,
    requests: Sender<(PathBuf, FileMeta)>,
    results: Receiver<DetectorResult>,
}

impl BlueprintDetector {
    pub fn new(reader: impl FileReader + Send + 'static) -> Self {
        let (requests, request_rx) = mpsc::channel::<(PathBuf, FileMeta)>();
        let (result_tx, results) = mpsc::channel::<DetectorResult>();
        thread::Builder::new()
            .name("uasset blueprint detector".to_owned())
            .spawn(move || {
                for (path, meta) in request_rx {
                    let result = read_header(&reader, &path).map(|header| is_blueprint(&header));
                    if result_tx.send((path, meta, result)).is_err() {
                        break;
                    }
                }
            })
            .expect("failed to spawn the blueprint detector thread");
        Self {
            cache: HashMap::new(),
            pending: HashSet::new(),
            requests,
            results,
        }
    }

    /// The cached answer for this size and mtime, else None and the file is queued.
    pub fn check(&mut self, path: &Path, meta: FileMeta) -> Option<bool> {
        match self.cache.get(path) {
            Some((cached_meta, blueprint)) if *cached_meta == meta => return Some(*blueprint),
            _ => {}
        }
        if self.pending.insert(path.to_path_buf()) {
            self.requests
                .send((path.to_path_buf(), meta))
                .expect("the blueprint detector thread is gone");
        }
        None
    }

    /// Read errors are logged and not cached, so the next check retries the file.
    pub fn poll(&mut self) -> Vec<(PathBuf, bool)> {
        let mut done = Vec::new();
        while let Ok((path, meta, result)) = self.results.try_recv() {
            self.pending.remove(&path);
            match result {
                Ok(blueprint) => {
                    self.cache.insert(path.clone(), (meta, blueprint));
                    done.push((path, blueprint));
                }
                Err(e) => log::warn!("Blueprint check of {} failed: {e}", path.display()),
            }
        }
        done
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

impl Default for BlueprintDetector {
    fn default() -> Self {
        Self::new(DiskReader)
    }
}

impl fmt::Debug for BlueprintDetector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlueprintDetector")
            .field("cached", &self.cache.len())
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        let taken = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(taken)
    }

    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    /// FString: a positive length counts Latin-1 bytes, a negative one UTF-16 units, both
    /// including the terminating NUL.
    fn fstring(&mut self) -> Option<String> {
        let len = self.i32()?;
        let units = len.unsigned_abs() as usize;
        if len >= 0 {
            let bytes = self.take(units)?;
            let text = bytes.strip_suffix(&[0]).unwrap_or(bytes);
            Some(text.iter().map(|&b| b as char).collect())
        } else {
            let bytes = self.take(units.checked_mul(2)?)?;
            let wide: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            let text = wide.strip_suffix(&[0]).unwrap_or(&wide);
            Some(String::from_utf16_lossy(text))
        }
    }
}

/// Names of the name table within `header`, per PackageFileSummary.cpp (UE 5.6); empty if the
/// summary doesn't parse.
fn package_names(header: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let Some((name_count, name_offset, has_hashes)) = name_table(header) else {
        return names;
    };
    let mut cursor = Cursor {
        bytes: header,
        pos: name_offset,
    };
    for _ in 0..name_count {
        let Some(name) = cursor.fstring() else {
            break;
        };
        if has_hashes && cursor.take(4).is_none() {
            break;
        }
        names.push(name);
    }
    names
}

fn name_table(header: &[u8]) -> Option<(usize, usize, bool)> {
    let mut cursor = Cursor {
        bytes: header,
        pos: 0,
    };
    if cursor.u32()? != PACKAGE_FILE_TAG {
        return None;
    }
    let legacy_version = cursor.i32()?;
    if !(-9..=-3).contains(&legacy_version) {
        return None;
    }
    if legacy_version != -4 {
        cursor.i32()?;
    }
    let ue4_version = cursor.i32()?;
    let ue5_version = if legacy_version <= -8 {
        cursor.i32()?
    } else {
        0
    };
    let licensee_version = cursor.i32()?;
    if ue4_version == 0 && ue5_version == 0 && licensee_version == 0 {
        return None;
    }
    if ue5_version >= UE5_PACKAGE_SAVED_HASH {
        cursor.take(20 + 4)?;
    }
    let custom_versions = usize::try_from(cursor.i32()?).ok()?;
    if legacy_version < -5 {
        cursor.take(custom_versions.checked_mul(16 + 4)?)?;
    } else {
        for _ in 0..custom_versions {
            cursor.take(16 + 4)?;
            cursor.fstring()?;
        }
    }
    if ue5_version < UE5_PACKAGE_SAVED_HASH {
        cursor.i32()?;
    }
    cursor.fstring()?;
    cursor.u32()?;
    let name_count = usize::try_from(cursor.i32()?).ok()?;
    let name_offset = usize::try_from(cursor.i32()?).ok()?;
    Some((
        name_count,
        name_offset,
        ue4_version >= UE4_NAME_HASHES_SERIALIZED,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, UNIX_EPOCH};

    const BLUEPRINT: &[u8] = include_bytes!("../fixtures/uasset/BP_Castle.uasset");
    const WIDGET_BLUEPRINT: &[u8] = include_bytes!("../fixtures/uasset/TestHUD.uasset");
    const TEXTURE: &[u8] = include_bytes!("../fixtures/uasset/MiddleGradient.uasset");
    const DATA_ASSET_WITH_BLUEPRINT_CLASS: &[u8] =
        include_bytes!("../fixtures/uasset/DA_Config.uasset");

    #[test]
    fn blueprint_header_is_a_blueprint() {
        assert!(is_blueprint(BLUEPRINT));
    }

    #[test]
    fn widget_blueprint_header_is_a_blueprint() {
        assert!(is_blueprint(WIDGET_BLUEPRINT));
    }

    #[test]
    fn texture_header_is_not_a_blueprint() {
        assert!(!is_blueprint(TEXTURE));
    }

    #[test]
    fn data_asset_referencing_a_blueprint_class_is_not_a_blueprint() {
        assert!(!is_blueprint(DATA_ASSET_WITH_BLUEPRINT_CLASS));
    }

    #[test]
    fn truncated_blueprint_header_is_not_a_blueprint() {
        assert!(!is_blueprint(&BLUEPRINT[..40]));
        assert!(!is_blueprint(&BLUEPRINT[..BLUEPRINT.len() / 2]));
    }

    #[test]
    fn empty_or_garbage_is_not_a_blueprint() {
        assert!(!is_blueprint(&[]));
        assert!(!is_blueprint(&[0xFF; 256]));
        let mut bad_count = BLUEPRINT.to_vec();
        bad_count[0x30..0x34].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(!is_blueprint(&bad_count));
    }

    #[derive(Clone, Default)]
    struct SharedReader {
        files: Arc<Mutex<HashMap<PathBuf, Vec<u8>>>>,
        opens: Arc<Mutex<Vec<(PathBuf, thread::ThreadId)>>>,
    }

    impl FileReader for SharedReader {
        type File = io::Cursor<Vec<u8>>;
        fn open(&self, path: &Path) -> io::Result<Self::File> {
            self.opens
                .lock()
                .unwrap()
                .push((path.to_path_buf(), thread::current().id()));
            let files = self.files.lock().unwrap();
            let bytes = files.get(path).ok_or(io::ErrorKind::NotFound)?;
            Ok(io::Cursor::new(bytes.clone()))
        }
    }

    fn meta(size: u64, secs: u64) -> FileMeta {
        FileMeta {
            size,
            modified: UNIX_EPOCH + Duration::from_secs(secs),
        }
    }

    fn settle(detector: &mut BlueprintDetector) -> Vec<(PathBuf, bool)> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut done = Vec::new();
        while detector.pending() > 0 {
            assert!(Instant::now() < deadline, "detector did not finish");
            done.extend(detector.poll());
            thread::sleep(Duration::from_millis(1));
        }
        done
    }

    fn opens(reader: &SharedReader) -> usize {
        reader.opens.lock().unwrap().len()
    }

    #[test]
    fn detector_answers_after_the_worker_reads_the_header() {
        let reader = SharedReader::default();
        let bp = PathBuf::from("a/BP.uasset");
        let texture = PathBuf::from("a/T.uasset");
        reader
            .files
            .lock()
            .unwrap()
            .insert(bp.clone(), BLUEPRINT.to_vec());
        reader
            .files
            .lock()
            .unwrap()
            .insert(texture.clone(), TEXTURE.to_vec());
        let mut detector = BlueprintDetector::new(reader.clone());

        assert_eq!(detector.check(&bp, meta(1, 1)), None);
        assert_eq!(detector.check(&texture, meta(1, 1)), None);
        let mut done = settle(&mut detector);
        done.sort();
        assert_eq!(done, vec![(bp.clone(), true), (texture.clone(), false)]);
        assert_eq!(detector.check(&bp, meta(1, 1)), Some(true));
        assert_eq!(detector.check(&texture, meta(1, 1)), Some(false));
    }

    #[test]
    fn detector_reads_headers_off_the_calling_thread() {
        let reader = SharedReader::default();
        let bp = PathBuf::from("BP.uasset");
        reader
            .files
            .lock()
            .unwrap()
            .insert(bp.clone(), BLUEPRINT.to_vec());
        let mut detector = BlueprintDetector::new(reader.clone());

        detector.check(&bp, meta(1, 1));
        settle(&mut detector);
        let opens = reader.opens.lock().unwrap();
        assert_eq!(opens.len(), 1);
        assert_ne!(opens[0].1, thread::current().id());
    }

    #[test]
    fn unchanged_size_and_mtime_are_not_read_again() {
        let reader = SharedReader::default();
        let bp = PathBuf::from("BP.uasset");
        reader
            .files
            .lock()
            .unwrap()
            .insert(bp.clone(), BLUEPRINT.to_vec());
        let mut detector = BlueprintDetector::new(reader.clone());

        detector.check(&bp, meta(1, 1));
        detector.check(&bp, meta(1, 1));
        settle(&mut detector);
        assert_eq!(opens(&reader), 1);

        assert_eq!(detector.check(&bp, meta(1, 1)), Some(true));
        assert_eq!(detector.pending(), 0);
        assert_eq!(opens(&reader), 1);

        reader
            .files
            .lock()
            .unwrap()
            .insert(bp.clone(), TEXTURE.to_vec());
        assert_eq!(detector.check(&bp, meta(2, 1)), None);
        settle(&mut detector);
        assert_eq!(opens(&reader), 2);
        assert_eq!(detector.check(&bp, meta(2, 1)), Some(false));

        assert_eq!(detector.check(&bp, meta(2, 2)), None);
        settle(&mut detector);
        assert_eq!(opens(&reader), 3);
    }

    #[test]
    fn read_errors_are_not_cached() {
        let reader = SharedReader::default();
        let bp = PathBuf::from("BP.uasset");
        let mut detector = BlueprintDetector::new(reader.clone());

        assert_eq!(detector.check(&bp, meta(1, 1)), None);
        assert_eq!(settle(&mut detector), vec![]);

        reader
            .files
            .lock()
            .unwrap()
            .insert(bp.clone(), BLUEPRINT.to_vec());
        assert_eq!(detector.check(&bp, meta(1, 1)), None);
        assert_eq!(settle(&mut detector), vec![(bp.clone(), true)]);
        assert_eq!(opens(&reader), 2);
    }
}
