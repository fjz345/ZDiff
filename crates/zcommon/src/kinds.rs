use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
            ],
            FileKind::Video => &["mp4", "mov", "mkv", "avi", "webm", "wmv"],
            FileKind::Audio => &["wav", "flac", "mp3", "ogg", "opus", "m4a"],
            FileKind::Model => &["fbx", "obj", "gltf", "glb", "blend", "abc", "usd"],
            FileKind::Archive => &["zip", "7z", "rar", "pak", "utoc", "ucas"],
            FileKind::Executable => &["exe", "dll", "pdb", "lib", "so"],
            FileKind::UnrealAsset => &["uasset"],
            FileKind::UnrealMap => &["umap"],
            FileKind::Binary => &[],
        }
    }
}

/// Case-insensitive, leading dot ignored: `PNG`, `.png` and `png` are one entry.
pub fn normalize_extension(extension: &str) -> String {
    extension.trim().trim_start_matches('.').to_lowercase()
}

fn default_kind(extension: &str) -> FileKind {
    let extension = normalize_extension(extension);
    FileKind::ALL
        .into_iter()
        .find(|kind| kind.default_extensions().contains(&extension.as_str()))
        .unwrap_or(FileKind::Binary)
}

/// The default extension map with user overrides on top. Override keys are always normalized.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KindTable {
    overrides: BTreeMap<String, FileKind>,
}

impl KindTable {
    pub fn kind(&self, extension: &str) -> FileKind {
        let extension = normalize_extension(extension);
        self.overrides
            .get(&extension)
            .copied()
            .unwrap_or_else(|| default_kind(&extension))
    }

    /// Returns false, and changes nothing, for an extension that is empty once normalized.
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
            ],
        ),
        (
            FileKind::Video,
            &["mp4", "mov", "mkv", "avi", "webm", "wmv"],
        ),
        (
            FileKind::Audio,
            &["wav", "flac", "mp3", "ogg", "opus", "m4a"],
        ),
        (
            FileKind::Model,
            &["fbx", "obj", "gltf", "glb", "blend", "abc", "usd"],
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
        for extension in ["", "xyz", "uasset2", "tx"] {
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
}
