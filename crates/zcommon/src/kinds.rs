use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FileKind {
    Text,
    Image,
    Video,
    Audio,
    Model,
    Archive,
    Executable,
    UnrealAsset,
    UnrealMap,
    Binary,
}

impl FileKind {
    pub const ALL: [FileKind; 10] = [
        FileKind::Text,
        FileKind::Image,
        FileKind::Video,
        FileKind::Audio,
        FileKind::Model,
        FileKind::Archive,
        FileKind::Executable,
        FileKind::UnrealAsset,
        FileKind::UnrealMap,
        FileKind::Binary,
    ];

    pub fn tag(self) -> &'static str {
        match self {
            FileKind::Text => "txt",
            FileKind::Image => "img",
            FileKind::Video => "vid",
            FileKind::Audio => "aud",
            FileKind::Model => "3d",
            FileKind::Archive => "arc",
            FileKind::Executable => "exe",
            FileKind::UnrealAsset => "UASS",
            FileKind::UnrealMap => "UMAP",
            FileKind::Binary => "bin",
        }
    }

    pub fn default_extensions(self) -> &'static [&'static str] {
        match self {
            FileKind::Text => &[
                "txt", "md", "log", "json", "xml", "ini", "cfg", "yaml", "toml", "csv", "rs", "c",
                "h", "cpp", "hpp", "cs", "py", "js", "ts", "hlsl", "usf", "ush",
            ],
            FileKind::Image => &[
                "png", "tga", "jpg", "jpeg", "bmp", "dds", "exr", "hdr", "psd", "tif", "gif",
                "webp",
            ],
            FileKind::Video => &["mp4", "mov", "mkv", "avi", "webm", "wmv", "m4v"],
            FileKind::Audio => &[
                "wav", "flac", "mp3", "ogg", "opus", "m4a", "aac", "wma", "aif", "aiff",
            ],
            FileKind::Model => &["fbx", "gltf", "glb", "blend", "abc", "usd"],
            FileKind::Archive => &["zip", "7z", "rar", "pak", "utoc", "ucas"],
            FileKind::Executable => &["exe", "dll", "pdb", "lib", "so"],
            FileKind::UnrealAsset => &["uasset"],
            FileKind::UnrealMap => &["umap"],
            FileKind::Binary => &[],
        }
    }
}

pub fn normalize_extension(extension: &str) -> String {
    extension.trim().trim_start_matches('.').to_lowercase()
}

fn default_kind(extension: &str) -> Option<FileKind> {
    FileKind::ALL
        .into_iter()
        .find(|kind| kind.default_extensions().contains(&extension))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(
        from = "BTreeMap<String, FileKind>",
        into = "BTreeMap<String, FileKind>"
    )
)]
pub struct KindTable {
    overrides: BTreeMap<String, FileKind>,
}

impl From<BTreeMap<String, FileKind>> for KindTable {
    fn from(entries: BTreeMap<String, FileKind>) -> Self {
        let mut table = Self::default();
        for (extension, kind) in entries {
            table.set_override(&extension, kind);
        }
        table
    }
}

impl From<KindTable> for BTreeMap<String, FileKind> {
    fn from(table: KindTable) -> Self {
        table.overrides
    }
}

impl KindTable {
    pub fn kind(&self, extension: &str) -> FileKind {
        self.mapped_kind(extension).unwrap_or(FileKind::Binary)
    }

    // None for an extension neither the defaults nor an override list, unlike an explicit Binary.
    pub fn mapped_kind(&self, extension: &str) -> Option<FileKind> {
        let extension = normalize_extension(extension);
        self.overrides
            .get(&extension)
            .copied()
            .or_else(|| default_kind(&extension))
    }

    pub fn overrides(&self) -> impl Iterator<Item = (&str, FileKind)> {
        self.overrides
            .iter()
            .map(|(ext, kind)| (ext.as_str(), *kind))
    }

    pub fn set_override(&mut self, extension: &str, kind: FileKind) -> bool {
        let extension = normalize_extension(extension);
        if extension.is_empty() {
            return false;
        }
        self.overrides.insert(extension, kind);
        true
    }

    pub fn remove_override(&mut self, extension: &str) {
        self.overrides.remove(&normalize_extension(extension));
    }
}

pub fn kinds_file_path() -> Option<PathBuf> {
    crate::style::settings_dir().map(|dir| dir.join("kinds.json"))
}

#[cfg(feature = "serde")]
#[derive(Debug)]
pub enum KindFileError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

#[cfg(feature = "serde")]
impl std::fmt::Display for KindFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KindFileError::Io(e) => write!(f, "{e}"),
            KindFileError::Json(e) => write!(f, "invalid kind table JSON: {e}"),
        }
    }
}

#[cfg(feature = "serde")]
impl std::error::Error for KindFileError {}

#[cfg(feature = "serde")]
impl KindTable {
    pub fn load(path: &std::path::Path) -> Result<KindTable, KindFileError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log::info!("No kind table at {}, using defaults", path.display());
                return Ok(KindTable::default());
            }
            Err(e) => return Err(KindFileError::Io(e)),
        };
        serde_json::from_str(&text).map_err(KindFileError::Json)
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), KindFileError> {
        let dir = path.parent().unwrap_or(std::path::Path::new("."));
        std::fs::create_dir_all(dir).map_err(KindFileError::Io)?;
        let text = serde_json::to_string_pretty(self).map_err(KindFileError::Json)?;
        let mut file = tempfile::NamedTempFile::new_in(dir).map_err(KindFileError::Io)?;
        std::io::Write::write_all(&mut file, text.as_bytes()).map_err(KindFileError::Io)?;
        file.persist(path).map_err(|e| KindFileError::Io(e.error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_DEFAULTS: &[(FileKind, &[&str])] = &[
        (
            FileKind::Text,
            &[
                "txt", "md", "log", "json", "xml", "ini", "cfg", "yaml", "toml", "csv", "rs", "c",
                "h", "cpp", "hpp", "cs", "py", "js", "ts", "hlsl", "usf", "ush",
            ],
        ),
        (
            FileKind::Image,
            &[
                "png", "tga", "jpg", "jpeg", "bmp", "dds", "exr", "hdr", "psd", "tif", "gif",
                "webp",
            ],
        ),
        (
            FileKind::Video,
            &["mp4", "mov", "mkv", "avi", "webm", "wmv", "m4v"],
        ),
        (
            FileKind::Audio,
            &[
                "wav", "flac", "mp3", "ogg", "opus", "m4a", "aac", "wma", "aif", "aiff",
            ],
        ),
        (
            FileKind::Model,
            &["fbx", "gltf", "glb", "blend", "abc", "usd"],
        ),
        (
            FileKind::Archive,
            &["zip", "7z", "rar", "pak", "utoc", "ucas"],
        ),
        (FileKind::Executable, &["exe", "dll", "pdb", "lib", "so"]),
        (FileKind::UnrealAsset, &["uasset"]),
        (FileKind::UnrealMap, &["umap"]),
    ];

    #[test]
    fn every_default_extension_maps_to_its_kind() {
        let table = KindTable::default();
        for &(kind, extensions) in EXPECTED_DEFAULTS {
            for extension in extensions {
                assert_eq!(table.kind(extension), kind, "{extension}");
            }
        }
    }

    #[test]
    fn unknown_extension_is_binary() {
        let table = KindTable::default();
        for extension in ["", "xyz", "uasset2", "tx", "obj"] {
            assert_eq!(table.kind(extension), FileKind::Binary, "{extension}");
        }
    }

    #[test]
    fn lookup_is_case_insensitive_and_ignores_leading_dot() {
        let table = KindTable::default();
        for extension in ["PNG", "Png", ".png", ".PNG", " png "] {
            assert_eq!(table.kind(extension), FileKind::Image, "{extension}");
        }
        assert_eq!(table.kind("UAsset"), FileKind::UnrealAsset);
    }

    #[test]
    fn override_wins_over_default() {
        let mut table = KindTable::default();
        assert!(table.set_override(".PNG", FileKind::Binary));
        assert!(table.set_override("xyz", FileKind::Text));
        assert_eq!(table.kind("png"), FileKind::Binary);
        assert_eq!(table.kind("XYZ"), FileKind::Text);
        assert_eq!(table.kind("jpg"), FileKind::Image);

        table.remove_override("Png");
        assert_eq!(table.kind("png"), FileKind::Image);
    }

    #[test]
    fn override_with_empty_extension_is_rejected() {
        let mut table = KindTable::default();
        assert!(!table.set_override(" . ", FileKind::Text));
        assert_eq!(table, KindTable::default());
    }

    #[test]
    fn each_kind_has_its_tag() {
        let tags = [
            (FileKind::Text, "txt"),
            (FileKind::Image, "img"),
            (FileKind::Video, "vid"),
            (FileKind::Audio, "aud"),
            (FileKind::Model, "3d"),
            (FileKind::Archive, "arc"),
            (FileKind::Executable, "exe"),
            (FileKind::UnrealAsset, "UASS"),
            (FileKind::UnrealMap, "UMAP"),
            (FileKind::Binary, "bin"),
        ];
        for (kind, tag) in tags {
            assert_eq!(kind.tag(), tag, "{kind:?}");
        }
    }

    #[test]
    fn mapped_kind_is_none_only_for_unlisted_extensions() {
        let mut table = KindTable::default();
        assert_eq!(table.mapped_kind(".PNG"), Some(FileKind::Image));
        assert_eq!(table.mapped_kind("exe"), Some(FileKind::Executable));
        assert_eq!(table.mapped_kind("zbin"), None);
        assert_eq!(table.mapped_kind(""), None);

        assert!(table.set_override("zbin", FileKind::Binary));
        assert_eq!(table.mapped_kind("ZBIN"), Some(FileKind::Binary));
        assert_eq!(table.kind("zbin"), FileKind::Binary);
    }

    #[cfg(feature = "serde")]
    mod file {
        use super::*;

        #[test]
        fn kind_file_round_trips() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("nested").join("kinds.json");
            let mut table = KindTable::default();
            table.set_override("zbin", FileKind::Binary);
            table.set_override("png", FileKind::Text);
            table.save(&path).unwrap();
            assert_eq!(KindTable::load(&path).unwrap(), table);
        }

        #[test]
        fn missing_file_gives_defaults() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("kinds.json");
            assert_eq!(KindTable::load(&path).unwrap(), KindTable::default());
        }

        #[test]
        fn invalid_json_is_an_error() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("kinds.json");
            std::fs::write(&path, "{ not json").unwrap();
            assert!(matches!(
                KindTable::load(&path),
                Err(KindFileError::Json(_))
            ));
        }

        #[test]
        fn hand_edited_file_loads_normalized() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("kinds.json");
            std::fs::write(
                &path,
                r#"{ ".ZBIN": "Binary", "Dat": "Text", ".": "Text" }"#,
            )
            .unwrap();
            let table = KindTable::load(&path).unwrap();
            assert_eq!(
                table.overrides().collect::<Vec<_>>(),
                vec![("dat", FileKind::Text), ("zbin", FileKind::Binary)]
            );
        }
    }
}
