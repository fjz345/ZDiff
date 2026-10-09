//! Which viewer shows a diff. One module per viewer kind; Text is the existing diff pane.
//!
//! Selection order: the per-diff override, then the shared kind table, then sniffing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eframe::egui;
use zcommon::ffmpeg::{FfmpegNotFound, FfmpegPaths, find_ffmpeg};
use zcommon::kinds::{FileKind, KindTable, normalize_extension};
use zdiff::universal_path::UniversalPath;

pub mod audio;
pub mod audio_diff;
pub mod audio_meta;
pub mod audio_spectrum;
pub mod hex;
pub mod image;
pub mod video_frames;
pub mod video_meta;
pub mod video_timeline;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ViewerKind {
    Text,
    Hex,
    Image,
    Audio,
    Video,
}

impl ViewerKind {
    pub const ALL: [ViewerKind; 5] = [
        ViewerKind::Text,
        ViewerKind::Hex,
        ViewerKind::Image,
        ViewerKind::Audio,
        ViewerKind::Video,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ViewerKind::Text => "Text",
            ViewerKind::Hex => "Hex",
            ViewerKind::Image => "Image",
            ViewerKind::Audio => "Audio",
            ViewerKind::Video => "Video",
        }
    }
}

/// Interim rule until the text-encoding-eol decoder lands: a NUL byte or invalid UTF-8 means
/// Hex. An empty file is Text.
pub fn sniff_viewer_kind(bytes: &[u8]) -> ViewerKind {
    if bytes.contains(&0) || std::str::from_utf8(bytes).is_err() {
        ViewerKind::Hex
    } else {
        ViewerKind::Text
    }
}

/// The extension of the path's file name, normalized. Depot paths carry the revision outside
/// the string, so `//depot/a.png#3` still gives `png`.
pub fn path_extension(path: &UniversalPath) -> Option<String> {
    let name = match path {
        UniversalPath::Local(path) | UniversalPath::Git(path, _) => path.file_name()?.to_str()?,
        UniversalPath::Depot(path, _) => path.rsplit('/').next()?,
    };
    let (stem, extension) = name.rsplit_once('.')?;
    (!stem.is_empty() && !extension.is_empty()).then(|| normalize_extension(extension))
}

pub fn viewer_for_kind(kind: FileKind) -> ViewerKind {
    match kind {
        FileKind::Text => ViewerKind::Text,
        FileKind::Image => ViewerKind::Image,
        FileKind::Audio => ViewerKind::Audio,
        FileKind::Video => ViewerKind::Video,
        FileKind::Model
        | FileKind::Archive
        | FileKind::Executable
        | FileKind::UnrealAsset
        | FileKind::UnrealMap
        | FileKind::Binary => ViewerKind::Hex,
    }
}

// The kind an entry of the old per-app viewer map becomes in the shared table.
pub fn viewer_map_kind(viewer: ViewerKind) -> FileKind {
    match viewer {
        ViewerKind::Text => FileKind::Text,
        ViewerKind::Image => FileKind::Image,
        ViewerKind::Hex => FileKind::Binary,
        ViewerKind::Audio => FileKind::Audio,
        ViewerKind::Video => FileKind::Video,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KindEdit {
    Set(String, FileKind),
    Remove(String),
    Reset,
}

pub fn apply_kind_edits(table: &mut KindTable, edits: Vec<KindEdit>) -> bool {
    let before = table.clone();
    for edit in edits {
        match edit {
            KindEdit::Set(extension, kind) => {
                table.set_override(&extension, kind);
            }
            KindEdit::Remove(extension) => table.remove_override(&extension),
            KindEdit::Reset => *table = KindTable::default(),
        }
    }
    *table != before
}

// Entries the table already gives the same viewer are skipped, so an old default like png stays
// on the table's default and exe keeps its Executable tag; an override already in the table wins.
fn migrate_viewer_map(table: &mut KindTable, legacy: &BTreeMap<String, ViewerKind>) -> bool {
    let mut changed = false;
    for (extension, &viewer) in legacy {
        let extension = normalize_extension(extension);
        let overridden = table.overrides().any(|(ext, _)| ext == extension);
        let same_viewer = table.mapped_kind(&extension).map(viewer_for_kind) == Some(viewer);
        if !overridden && !same_viewer {
            changed |= table.set_override(&extension, viewer_map_kind(viewer));
        }
    }
    changed
}

/// The shared kind table and the file it was loaded from; `path` is `None` without a settings
/// folder, and edits then live for the session only.
#[derive(Debug, Default)]
pub struct SharedKinds {
    pub table: KindTable,
    path: Option<PathBuf>,
}

#[cfg(feature = "serde")]
impl SharedKinds {
    /// Migrates the old viewer map into the table and clears it once the table is saved, so a
    /// failed save retries on the next start.
    pub fn load(path: Option<PathBuf>, legacy: &mut Option<BTreeMap<String, ViewerKind>>) -> Self {
        let Some(path) = path else {
            log::warn!("No settings folder; the kind table is the defaults, edits aren't saved");
            let mut table = KindTable::default();
            if let Some(old) = legacy {
                migrate_viewer_map(&mut table, old);
            }
            return Self { table, path: None };
        };
        let (mut table, loaded) = match KindTable::load(&path) {
            Ok(table) => (table, true),
            Err(e) => {
                log::error!(
                    "Failed to load the kind table {}: {e}; using defaults",
                    path.display()
                );
                (KindTable::default(), false)
            }
        };
        if let Some(old) = legacy {
            let migrated = migrate_viewer_map(&mut table, old);
            // A broken file is left for the user to fix rather than overwritten.
            let saved = loaded && (!migrated || Self::save(&table, &path));
            if saved {
                log::info!("Viewer map migrated into {}", path.display());
                *legacy = None;
            }
        }
        Self {
            table,
            path: Some(path),
        }
    }

    pub fn apply(&mut self, edits: Vec<KindEdit>) {
        if apply_kind_edits(&mut self.table, edits)
            && let Some(path) = &self.path
        {
            Self::save(&self.table, path);
        }
    }

    fn save(table: &KindTable, path: &std::path::Path) -> bool {
        table
            .save(path)
            .inspect_err(|e| log::error!("Failed to save the kind table {}: {e}", path.display()))
            .is_ok()
    }
}

/// The user's viewer choice for the shown pair; `None` is Auto.
#[derive(Debug, Default)]
pub struct ViewerOverride {
    pub kind: Option<ViewerKind>,
    pair: Option<(UniversalPath, UniversalPath)>,
}

impl ViewerOverride {
    /// Call before resolving. Any pair other than the one the override was made for resets it to
    /// Auto; the same pair swapped or reloaded keeps it.
    pub fn observe_pair(&mut self, path_1: &UniversalPath, path_2: &UniversalPath) {
        let same_pair = self
            .pair
            .as_ref()
            .is_some_and(|(a, b)| (a == path_1 && b == path_2) || (a == path_2 && b == path_1));
        if !same_pair {
            self.kind = None;
            self.pair = Some((path_1.clone(), path_2.clone()));
        }
    }
}

/// One loaded side: its display path (the extension comes from it, never from a temp copy) and
/// what its content sniffed as. Hex means binary, which has no text load.
#[derive(Debug, Clone, Copy)]
pub struct LoadedSide<'a> {
    pub path: &'a UniversalPath,
    pub sniffed: ViewerKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Viewers {
    Pair(ViewerKind),
    /// The sides resolve to different viewers: each side is shown on its own, not diffed.
    Sides([ViewerKind; 2]),
}

impl Viewers {
    fn from_sides([first, second]: [ViewerKind; 2]) -> Self {
        if first == second {
            Viewers::Pair(first)
        } else {
            Viewers::Sides([first, second])
        }
    }

    pub fn pair(self) -> Option<ViewerKind> {
        match self {
            Viewers::Pair(kind) => Some(kind),
            Viewers::Sides(_) => None,
        }
    }

    pub fn shows(self, kind: ViewerKind) -> bool {
        match self {
            Viewers::Pair(shown) => shown == kind,
            Viewers::Sides(sides) => sides.contains(&kind),
        }
    }

    fn map(self, f: impl Fn(ViewerKind) -> ViewerKind) -> Self {
        match self {
            Viewers::Pair(kind) => Viewers::Pair(f(kind)),
            Viewers::Sides(sides) => Viewers::from_sides(sides.map(f)),
        }
    }

    /// What a viewer's processor is given: the pair, or a mismatched pair's side of that kind in
    /// the first slot.
    pub fn files_for<T>(self, kind: ViewerKind, files: [Option<T>; 2]) -> [Option<T>; 2] {
        let [first, second] = files;
        match self {
            Viewers::Pair(shown) if shown == kind => [first, second],
            Viewers::Sides([shown, _]) if shown == kind => [first, None],
            Viewers::Sides([_, shown]) if shown == kind => [second, None],
            _ => [None, None],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerResolution {
    pub viewers: Viewers,
    /// Why the pair isn't shown in the viewer it asked for; logged once per change.
    pub fallback: Option<String>,
}

/// The viewer for a pair, `None` while neither side is loaded.
pub fn resolve_viewer(
    override_kind: Option<ViewerKind>,
    kinds: &KindTable,
    side_1: Option<LoadedSide>,
    side_2: Option<LoadedSide>,
) -> Option<ViewerResolution> {
    let resolve_side = |side: LoadedSide| {
        let wanted = override_kind
            .or_else(|| {
                path_extension(side.path)
                    .and_then(|ext| kinds.mapped_kind(&ext))
                    .map(viewer_for_kind)
            })
            .unwrap_or(side.sniffed);
        // A binary load has no text to show.
        if wanted == ViewerKind::Text && side.sniffed == ViewerKind::Hex {
            let reason = format!("{} is binary and can't be shown as Text", side.path);
            (wanted, ViewerKind::Hex, Some(reason))
        } else {
            (wanted, wanted, None)
        }
    };

    match (side_1.map(resolve_side), side_2.map(resolve_side)) {
        (None, None) => None,
        (Some((_, kind, fallback)), None) | (None, Some((_, kind, fallback))) => {
            Some(ViewerResolution {
                viewers: Viewers::Pair(kind),
                fallback,
            })
        }
        (Some((wanted_1, first, fallback_1)), Some((wanted_2, second, fallback_2))) => {
            let fallback = match (fallback_1, fallback_2) {
                (Some(a), Some(b)) => Some(format!("{}; {}", a, b)),
                (a, b) => a.or(b),
            };
            // Sides that asked for the same viewer stay one diff even when one is binary.
            let viewers = if wanted_1 == wanted_2 && first != second {
                Viewers::Pair(ViewerKind::Hex)
            } else {
                Viewers::from_sides([first, second])
            };
            Some(ViewerResolution { viewers, fallback })
        }
    }
}

/// An Image resolution with a side that failed to decode (`failures`, one reason per side) is
/// shown as Hex. Decoding happens after resolving, so this is a second step.
pub fn image_decode_fallback(
    resolution: ViewerResolution,
    failures: &[String],
) -> ViewerResolution {
    decode_fallback(resolution, ViewerKind::Image, failures)
}

/// `failures` are from the decoder of viewer `decoded`; they apply to the pair or the side
/// resolved to it.
pub fn decode_fallback(
    resolution: ViewerResolution,
    decoded: ViewerKind,
    failures: &[String],
) -> ViewerResolution {
    if !resolution.viewers.shows(decoded) || failures.is_empty() {
        return resolution;
    }
    hex_fallback(resolution, |kind| kind == decoded, failures.to_vec())
}

pub fn ffmpeg_fallback(
    resolution: ViewerResolution,
    ffmpeg: Result<&FfmpegPaths, &FfmpegNotFound>,
) -> ViewerResolution {
    let Err(not_found) = ffmpeg else {
        return resolution;
    };
    let media: Vec<&str> = [ViewerKind::Audio, ViewerKind::Video]
        .into_iter()
        .filter(|&kind| resolution.viewers.shows(kind))
        .map(ViewerKind::name)
        .collect();
    if media.is_empty() {
        return resolution;
    }
    let needs = if media.len() == 1 {
        "viewer needs"
    } else {
        "viewers need"
    };
    let reason = format!(
        "The {} {needs} ffmpeg. {not_found}. Put ffmpeg and ffprobe on PATH or next to \
         the app, or set their folder in Option - File Viewers",
        media.join(" and ")
    );
    hex_fallback(
        resolution,
        |kind| matches!(kind, ViewerKind::Audio | ViewerKind::Video),
        vec![reason],
    )
}

// Sides left the same by the fallback become one hex diff.
fn hex_fallback(
    resolution: ViewerResolution,
    falls_back: impl Fn(ViewerKind) -> bool,
    reasons: Vec<String>,
) -> ViewerResolution {
    let mut all: Vec<String> = resolution.fallback.into_iter().collect();
    all.extend(reasons);
    all.push("showing Hex".to_string());
    ViewerResolution {
        viewers: resolution.viewers.map(|kind| {
            if falls_back(kind) {
                ViewerKind::Hex
            } else {
                kind
            }
        }),
        fallback: Some(all.join("; ")),
    }
}

/// The ffmpeg folder (or executable) set in Options; empty searches next to the app and PATH.
#[derive(Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FfmpegSetting {
    pub path: String,
    // Looked up on first use and again after the path changes; it stats files and walks PATH.
    #[cfg_attr(feature = "serde", serde(skip))]
    found: Option<Result<FfmpegPaths, FfmpegNotFound>>,
}

impl FfmpegSetting {
    pub fn lookup(&mut self) -> Result<&FfmpegPaths, &FfmpegNotFound> {
        let configured = self.path.trim();
        self.found
            .get_or_insert_with(|| {
                find_ffmpeg((!configured.is_empty()).then(|| Path::new(configured)))
            })
            .as_ref()
    }
}

pub fn ui_ffmpeg_setting(ui: &mut egui::Ui, setting: &mut FfmpegSetting) {
    ui.horizontal(|ui| {
        ui.label("ffmpeg folder");
        let edit = egui::TextEdit::singleline(&mut setting.path)
            .desired_width(300.0)
            .hint_text("next to the app, then PATH");
        if ui.add(edit).changed() {
            setting.found = None;
        }
    });
    match setting.lookup() {
        Ok(paths) => ui.label(format!("Using {}", paths.ffmpeg.display())),
        Err(not_found) => ui.colored_label(ui.visuals().warn_fg_color, not_found.to_string()),
    };
}

/// Stands in for the Audio and Video viewers until they exist.
pub fn media_placeholder(
    ui: &mut egui::Ui,
    kind: ViewerKind,
    left: &UniversalPath,
    right: &UniversalPath,
) {
    ui.vertical_centered(|ui| {
        ui.add_space(20.0);
        ui.label(format!("{} viewer", kind.name()));
        ui.label(left.to_string());
        ui.label(right.to_string());
    });
}

fn kind_label(kind: FileKind) -> String {
    format!("{kind:?} ({})", viewer_for_kind(kind).name())
}

/// Settings editor for the shared kind table's overrides. The extension being added and its kind
/// live in egui's temp memory.
pub fn ui_extension_map(ui: &mut egui::Ui, table: &KindTable) -> Vec<KindEdit> {
    let mut edits = Vec::new();
    if ui.button("Reset to defaults").clicked() {
        edits.push(KindEdit::Reset);
    }
    ui.label("Shared with ZHashDiff. Unlisted extensions are sniffed: text opens as Text, binary as Hex.");
    ui.collapsing("Defaults", |ui| {
        for kind in FileKind::ALL {
            let extensions = kind.default_extensions();
            if !extensions.is_empty() {
                ui.label(format!("{}: {}", kind_label(kind), extensions.join(" ")));
            }
        }
    });
    ui.separator();

    let new_id = ui.id().with("new_extension");
    let (mut new_extension, mut new_kind) = ui.data_mut(|d| {
        d.get_temp::<(String, FileKind)>(new_id)
            .unwrap_or((String::new(), FileKind::Text))
    });
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut new_extension)
                .desired_width(80.0)
                .hint_text("zbin"),
        );
        egui::ComboBox::from_id_salt("new_extension_kind")
            .selected_text(kind_label(new_kind))
            .show_ui(ui, |ui| {
                for option in FileKind::ALL {
                    ui.selectable_value(&mut new_kind, option, kind_label(option));
                }
            });
        if ui.button("Add").clicked() && !normalize_extension(&new_extension).is_empty() {
            edits.push(KindEdit::Set(std::mem::take(&mut new_extension), new_kind));
        }
    });
    ui.data_mut(|d| d.insert_temp(new_id, (new_extension, new_kind)));
    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("extension_map_grid")
            .num_columns(3)
            .spacing([20.0, 8.0])
            .show(ui, |ui| {
                for (extension, kind) in table.overrides() {
                    ui.label(format!(".{}", extension));
                    let mut picked = kind;
                    egui::ComboBox::from_id_salt(("extension_map_kind", extension))
                        .selected_text(kind_label(kind))
                        .show_ui(ui, |ui| {
                            for option in FileKind::ALL {
                                ui.selectable_value(&mut picked, option, kind_label(option));
                            }
                        });
                    if picked != kind {
                        edits.push(KindEdit::Set(extension.to_owned(), picked));
                    }
                    if ui.button("Remove").clicked() {
                        edits.push(KindEdit::Remove(extension.to_owned()));
                    }
                    ui.end_row();
                }
            });
    });
    edits
}

/// Stops for the shared conflict cursor: the shown viewer's. The other viewer's state is stale.
pub fn conflict_count(viewers: Option<Viewers>, text: usize, hex: usize) -> usize {
    match viewers {
        Some(Viewers::Pair(ViewerKind::Hex)) => hex,
        Some(
            Viewers::Pair(ViewerKind::Image | ViewerKind::Audio | ViewerKind::Video)
            | Viewers::Sides(_),
        ) => 0,
        Some(Viewers::Pair(ViewerKind::Text)) | None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcommon::ffmpeg::resolve_ffmpeg;
    use zdiff::universal_path::DepotRev;

    #[test]
    fn conflict_navigation_counts_the_shown_viewers_stops() {
        // The conflict shortcuts drive one shared cursor; its max must come from the viewer on
        // screen, because the other viewer's state is stale.
        let pair = |kind| Some(Viewers::Pair(kind));
        assert_eq!(conflict_count(pair(ViewerKind::Text), 7, 3), 7);
        assert_eq!(conflict_count(pair(ViewerKind::Hex), 7, 3), 3);
        assert_eq!(conflict_count(None, 7, 3), 7);
        assert_eq!(conflict_count(pair(ViewerKind::Image), 7, 3), 0);
    }

    #[test]
    fn a_mismatched_pair_has_no_conflict_stops() {
        for sides in [
            [ViewerKind::Text, ViewerKind::Hex],
            [ViewerKind::Hex, ViewerKind::Image],
            [ViewerKind::Image, ViewerKind::Text],
        ] {
            assert_eq!(conflict_count(Some(Viewers::Sides(sides)), 7, 3), 0);
        }
    }

    fn side(path: &UniversalPath, sniffed: ViewerKind) -> Option<LoadedSide<'_>> {
        Some(LoadedSide { path, sniffed })
    }

    fn kind(resolution: Option<ViewerResolution>) -> Option<ViewerKind> {
        resolution.and_then(|r| r.viewers.pair())
    }

    fn viewers(resolution: Option<ViewerResolution>) -> Viewers {
        resolution.unwrap().viewers
    }

    #[test]
    fn same_kinds_give_one_viewer_for_the_pair() {
        let kinds = KindTable::default();
        let resolve = |a: &str, sniffed_a, b: &str, sniffed_b| {
            let (a, b) = (UniversalPath::from(a), UniversalPath::from(b));
            viewers(resolve_viewer(
                None,
                &kinds,
                side(&a, sniffed_a),
                side(&b, sniffed_b),
            ))
        };
        let (text, hex) = (ViewerKind::Text, ViewerKind::Hex);
        assert_eq!(
            resolve("a.png", hex, "b.jpg", hex),
            Viewers::Pair(ViewerKind::Image)
        );
        assert_eq!(resolve("a.txt", text, "b.rs", text), Viewers::Pair(text));
        assert_eq!(resolve("a.exe", hex, "b.zip", hex), Viewers::Pair(hex));
    }

    #[test]
    fn image_against_text_gives_one_viewer_per_side() {
        let png = UniversalPath::from("a.png");
        let txt = UniversalPath::from("b.txt");
        let kinds = KindTable::default();
        let resolution = resolve_viewer(
            None,
            &kinds,
            side(&png, ViewerKind::Hex),
            side(&txt, ViewerKind::Text),
        )
        .unwrap();
        assert_eq!(
            resolution,
            ViewerResolution {
                viewers: Viewers::Sides([ViewerKind::Image, ViewerKind::Text]),
                fallback: None,
            }
        );
        assert_eq!(
            viewers(resolve_viewer(
                None,
                &kinds,
                side(&txt, ViewerKind::Text),
                side(&png, ViewerKind::Hex),
            )),
            Viewers::Sides([ViewerKind::Text, ViewerKind::Image])
        );
    }

    #[test]
    fn image_against_an_unknown_binary_gives_image_and_single_side_hex() {
        let png = UniversalPath::from("a.png");
        let kinds = KindTable::default();
        for binary in ["b.unlisted", "b.exe", "b.uasset"] {
            let binary = UniversalPath::from(binary);
            assert_eq!(
                viewers(resolve_viewer(
                    None,
                    &kinds,
                    side(&png, ViewerKind::Hex),
                    side(&binary, ViewerKind::Hex),
                )),
                Viewers::Sides([ViewerKind::Image, ViewerKind::Hex]),
                "{binary}"
            );
        }
    }

    #[test]
    fn a_hex_override_gives_a_hex_diff_of_a_mismatched_pair() {
        let png = UniversalPath::from("a.png");
        let txt = UniversalPath::from("b.txt");
        assert_eq!(
            resolve_viewer(
                Some(ViewerKind::Hex),
                &KindTable::default(),
                side(&png, ViewerKind::Hex),
                side(&txt, ViewerKind::Text),
            ),
            Some(ViewerResolution {
                viewers: Viewers::Pair(ViewerKind::Hex),
                fallback: None,
            })
        );
    }

    #[test]
    fn a_binary_side_that_wanted_text_is_single_side_hex_against_another_kind() {
        let txt = UniversalPath::from("a.txt");
        let png = UniversalPath::from("b.png");
        let bin = UniversalPath::from("b.unlisted");
        let kinds = KindTable::default();
        let resolution = resolve_viewer(
            None,
            &kinds,
            side(&txt, ViewerKind::Hex),
            side(&png, ViewerKind::Hex),
        )
        .unwrap();
        assert_eq!(
            resolution.viewers,
            Viewers::Sides([ViewerKind::Hex, ViewerKind::Image])
        );
        assert!(resolution.fallback.is_some());

        let resolution = resolve_viewer(
            None,
            &kinds,
            side(&txt, ViewerKind::Hex),
            side(&bin, ViewerKind::Hex),
        )
        .unwrap();
        assert_eq!(resolution.viewers, Viewers::Pair(ViewerKind::Hex));
        assert!(resolution.fallback.is_some());
    }

    #[test]
    fn each_viewer_gets_the_pair_or_its_own_side_first() {
        let pair = Viewers::Pair(ViewerKind::Image);
        assert_eq!(
            pair.files_for(ViewerKind::Image, [Some(1), Some(2)]),
            [Some(1), Some(2)]
        );
        assert_eq!(
            pair.files_for(ViewerKind::Hex, [Some(1), Some(2)]),
            [None, None]
        );

        let sides = Viewers::Sides([ViewerKind::Text, ViewerKind::Image]);
        assert_eq!(
            sides.files_for(ViewerKind::Text, [Some(1), Some(2)]),
            [Some(1), None]
        );
        assert_eq!(
            sides.files_for(ViewerKind::Image, [Some(1), Some(2)]),
            [Some(2), None]
        );
        assert_eq!(
            sides.files_for(ViewerKind::Hex, [Some(1), Some(2)]),
            [None, None]
        );
    }

    #[test]
    fn a_failed_decode_on_one_side_of_a_mismatched_pair_turns_that_side_to_hex() {
        let resolution = |sides| ViewerResolution {
            viewers: Viewers::Sides(sides),
            fallback: None,
        };
        let failure = ["a.png can't be decoded: bad".to_string()];

        let shown =
            image_decode_fallback(resolution([ViewerKind::Image, ViewerKind::Text]), &failure);
        assert_eq!(
            shown.viewers,
            Viewers::Sides([ViewerKind::Hex, ViewerKind::Text])
        );
        assert!(shown.fallback.unwrap().contains(&failure[0]));

        let shown =
            image_decode_fallback(resolution([ViewerKind::Hex, ViewerKind::Image]), &failure);
        assert_eq!(shown.viewers, Viewers::Pair(ViewerKind::Hex));

        let text_and_hex = resolution([ViewerKind::Text, ViewerKind::Hex]);
        assert_eq!(
            image_decode_fallback(text_and_hex.clone(), &failure),
            text_and_hex
        );
    }

    #[test]
    fn a_media_side_of_a_mismatched_pair_without_ffmpeg_turns_to_hex() {
        let not_found = resolve_ffmpeg(None, None, |_| false, |_| None).unwrap_err();
        let shown = ffmpeg_fallback(
            ViewerResolution {
                viewers: Viewers::Sides([ViewerKind::Text, ViewerKind::Audio]),
                fallback: None,
            },
            Err(&not_found),
        );
        assert_eq!(
            shown.viewers,
            Viewers::Sides([ViewerKind::Text, ViewerKind::Hex])
        );
        assert!(
            shown
                .fallback
                .unwrap()
                .contains("The Audio viewer needs ffmpeg")
        );
    }

    fn zbin_hex_map() -> KindTable {
        let mut map = KindTable::default();
        assert!(map.set_override("zbin", FileKind::Binary));
        map
    }

    #[test]
    fn each_kind_gives_its_viewer() {
        let expected = [
            (FileKind::Text, ViewerKind::Text),
            (FileKind::Image, ViewerKind::Image),
            (FileKind::Video, ViewerKind::Video),
            (FileKind::Audio, ViewerKind::Audio),
            (FileKind::Model, ViewerKind::Hex),
            (FileKind::Archive, ViewerKind::Hex),
            (FileKind::Executable, ViewerKind::Hex),
            (FileKind::UnrealAsset, ViewerKind::Hex),
            (FileKind::UnrealMap, ViewerKind::Hex),
            (FileKind::Binary, ViewerKind::Hex),
        ];
        assert_eq!(expected.map(|(kind, _)| kind), FileKind::ALL);
        for (kind, viewer) in expected {
            assert_eq!(viewer_for_kind(kind), viewer, "{kind:?}");
        }
    }

    #[test]
    fn old_viewer_map_entries_convert_to_kinds() {
        assert_eq!(viewer_map_kind(ViewerKind::Text), FileKind::Text);
        assert_eq!(viewer_map_kind(ViewerKind::Image), FileKind::Image);
        assert_eq!(viewer_map_kind(ViewerKind::Hex), FileKind::Binary);
        assert_eq!(viewer_map_kind(ViewerKind::Audio), FileKind::Audio);
        assert_eq!(viewer_map_kind(ViewerKind::Video), FileKind::Video);
    }

    #[test]
    fn a_kind_from_the_table_picks_the_viewer_over_the_sniff() {
        let (text, hex) = (ViewerKind::Text, ViewerKind::Hex);
        let kinds = KindTable::default();
        let resolve = |name: &str, sniffed| {
            let path = UniversalPath::from(name);
            kind(resolve_viewer(None, &kinds, side(&path, sniffed), None))
        };
        assert_eq!(resolve("a.exe", text), Some(hex));
        assert_eq!(resolve("a.mp4", hex), Some(ViewerKind::Video));
        assert_eq!(resolve("a.uasset", text), Some(hex));
        assert_eq!(resolve("a.rs", text), Some(text));
        assert_eq!(resolve("a.webp", hex), Some(ViewerKind::Image));
        assert_eq!(resolve("a.unlisted", text), Some(text));
        assert_eq!(resolve("a.unlisted", hex), Some(hex));
    }

    #[test]
    fn override_beats_map_beats_sniff() {
        let a = UniversalPath::from("a.zbin");
        let b = UniversalPath::from("b.zbin");
        let (text, hex) = (ViewerKind::Text, ViewerKind::Hex);
        let map = zbin_hex_map();
        let none = KindTable::default();

        // Sniff alone.
        let sniffed = |s1, s2| kind(resolve_viewer(None, &none, side(&a, s1), side(&b, s2)));
        assert_eq!(sniffed(text, text), Some(text));
        assert_eq!(sniffed(hex, hex), Some(hex));

        // The map beats the sniff: text content mapped to Hex opens in Hex.
        assert_eq!(
            kind(resolve_viewer(None, &map, side(&a, text), side(&b, text))),
            Some(hex)
        );

        // The override beats the map, in both directions.
        assert_eq!(
            kind(resolve_viewer(
                Some(text),
                &map,
                side(&a, text),
                side(&b, text)
            )),
            Some(text)
        );
        assert_eq!(
            kind(resolve_viewer(
                Some(hex),
                &none,
                side(&a, text),
                side(&b, text)
            )),
            Some(hex)
        );

        // Nothing asked for differs from what was shown: no fallback message.
        assert_eq!(
            resolve_viewer(Some(text), &map, side(&a, text), side(&b, text))
                .unwrap()
                .fallback,
            None
        );
    }

    #[test]
    fn nothing_loaded_resolves_to_no_viewer_and_one_side_resolves_alone() {
        let a = UniversalPath::from("a.zbin");
        let map = zbin_hex_map();
        assert_eq!(resolve_viewer(None, &map, None, None), None);
        assert_eq!(
            kind(resolve_viewer(None, &map, None, side(&a, ViewerKind::Text))),
            Some(ViewerKind::Hex)
        );
        let txt = UniversalPath::from("a.txt");
        assert_eq!(
            kind(resolve_viewer(
                None,
                &map,
                side(&txt, ViewerKind::Text),
                None
            )),
            Some(ViewerKind::Text)
        );
    }

    #[test]
    fn extensions_normalize_case_and_leading_dot() {
        for spelling in ["PNG", ".png", "png", ".PnG"] {
            assert_eq!(normalize_extension(spelling), "png", "{spelling}");
        }

        let mut map = KindTable::default();
        assert!(apply_kind_edits(
            &mut map,
            vec![
                KindEdit::Set("PNG".into(), FileKind::Binary),
                KindEdit::Set(".png".into(), FileKind::Binary),
            ]
        ));
        assert_eq!(map.overrides().count(), 1, "one entry for all spellings");
        for spelling in ["PNG", ".png", "png"] {
            assert_eq!(
                map.mapped_kind(spelling),
                Some(FileKind::Binary),
                "{spelling}"
            );
        }
        assert!(apply_kind_edits(
            &mut map,
            vec![KindEdit::Remove(".Png".into())]
        ));
        assert_eq!(map.mapped_kind("png"), Some(FileKind::Image));

        assert!(
            !apply_kind_edits(&mut map, vec![KindEdit::Set(".".into(), FileKind::Binary)]),
            "empty extension"
        );
        assert_eq!(map.overrides().count(), 0);
    }

    #[test]
    fn path_extension_comes_from_the_file_name_of_local_and_depot_paths() {
        assert_eq!(
            path_extension(&UniversalPath::from(r"C:\dir.d\Image.PNG")),
            Some("png".into())
        );
        assert_eq!(
            path_extension(&UniversalPath::Depot(
                "//depot/a.b/Image.Png".into(),
                Some(DepotRev::Rev(5))
            )),
            Some("png".into())
        );
        assert_eq!(
            path_extension(&UniversalPath::from(r"C:\dir.d\Makefile")),
            None
        );
        assert_eq!(
            path_extension(&UniversalPath::from(r"C:\dir\.gitignore")),
            None
        );
        assert_eq!(
            path_extension(&UniversalPath::Depot("//depot/dir.d/Makefile".into(), None)),
            None
        );
    }

    #[test]
    fn map_lookup_uses_the_paths_extension_in_any_case() {
        let map = zbin_hex_map();
        let upper = UniversalPath::from(r"C:\x\DATA.ZBIN");
        let depot = UniversalPath::Depot("//depot/x/data.Zbin".into(), Some(DepotRev::Rev(3)));
        assert_eq!(
            kind(resolve_viewer(
                None,
                &map,
                side(&upper, ViewerKind::Text),
                side(&depot, ViewerKind::Text)
            )),
            Some(ViewerKind::Hex)
        );
    }

    #[test]
    fn sides_resolving_to_different_viewers_are_shown_per_side_without_a_fallback() {
        let a = UniversalPath::from("a.zbin");
        let b = UniversalPath::from("b.txt");
        let resolution = resolve_viewer(
            None,
            &zbin_hex_map(),
            side(&a, ViewerKind::Text),
            side(&b, ViewerKind::Text),
        )
        .unwrap();
        assert_eq!(
            resolution.viewers,
            Viewers::Sides([ViewerKind::Hex, ViewerKind::Text])
        );
        assert_eq!(resolution.fallback, None);

        let resolution = resolve_viewer(
            None,
            &KindTable::default(),
            side(&b, ViewerKind::Text),
            side(&a, ViewerKind::Hex),
        )
        .unwrap();
        assert_eq!(
            resolution.viewers,
            Viewers::Sides([ViewerKind::Text, ViewerKind::Hex])
        );
        assert_eq!(resolution.fallback, None);
    }

    #[test]
    fn forced_text_on_binary_content_falls_back_to_hex_with_a_reason() {
        // NUL bytes without a BOM: the loader's binary verdict.
        assert_eq!(sniff_viewer_kind(b"ab\0cd"), ViewerKind::Hex);

        let a = UniversalPath::from("a.dat");
        let b = UniversalPath::from("b.dat");
        let (text, hex) = (ViewerKind::Text, ViewerKind::Hex);

        // By the override.
        let resolution = resolve_viewer(
            Some(text),
            &KindTable::default(),
            side(&a, hex),
            side(&b, hex),
        )
        .unwrap();
        assert_eq!(resolution.viewers, Viewers::Pair(hex));
        assert!(resolution.fallback.is_some());

        // By the map, with only one side binary.
        let mut map = KindTable::default();
        map.set_override("dat", FileKind::Text);
        let resolution = resolve_viewer(None, &map, side(&a, text), side(&b, hex)).unwrap();
        assert_eq!(resolution.viewers, Viewers::Pair(hex));
        assert!(resolution.fallback.is_some());

        // A one-sided diff falls back too.
        let resolution = resolve_viewer(Some(text), &map, None, side(&b, hex)).unwrap();
        assert_eq!(resolution.viewers, Viewers::Pair(hex));
        assert!(resolution.fallback.is_some());
    }

    #[test]
    fn default_map_sends_common_image_extensions_to_image() {
        let map = KindTable::default();
        for extension in ["png", "jpg", "jpeg", "bmp", "gif", "webp", "tga"] {
            assert_eq!(
                map.mapped_kind(extension).map(viewer_for_kind),
                Some(ViewerKind::Image),
                "{extension}"
            );
        }

        // Image content sniffs as binary; the map still picks Image, in any case.
        let png = UniversalPath::from(r"C:\x\a.png");
        let jpg = UniversalPath::from(r"C:\x\b.JPG");
        assert_eq!(
            kind(resolve_viewer(
                None,
                &map,
                side(&png, ViewerKind::Hex),
                side(&jpg, ViewerKind::Hex)
            )),
            Some(ViewerKind::Image)
        );
    }

    #[test]
    fn a_user_mapped_extension_opens_as_image() {
        let a = UniversalPath::from("a.zpng");
        let b = UniversalPath::from("b.zpng");
        let mut map = KindTable::default();
        let resolve = |map: &KindTable| {
            kind(resolve_viewer(
                None,
                map,
                side(&a, ViewerKind::Hex),
                side(&b, ViewerKind::Hex),
            ))
        };
        assert_eq!(resolve(&map), Some(ViewerKind::Hex), "unmapped: sniffed");
        assert!(map.set_override(".ZPNG", FileKind::Image));
        assert_eq!(resolve(&map), Some(ViewerKind::Image));
    }

    #[test]
    fn a_failed_image_decode_falls_back_to_hex_with_a_reason() {
        let image = ViewerResolution {
            viewers: Viewers::Pair(ViewerKind::Image),
            fallback: None,
        };
        assert_eq!(image_decode_fallback(image.clone(), &[]), image);

        let resolution = image_decode_fallback(image, &["a.png can't be decoded: bad".to_string()]);
        assert_eq!(resolution.viewers, Viewers::Pair(ViewerKind::Hex));
        assert!(
            resolution
                .fallback
                .unwrap()
                .contains("a.png can't be decoded: bad")
        );

        // Only an Image resolution looks at decode results.
        let hex = ViewerResolution {
            viewers: Viewers::Pair(ViewerKind::Hex),
            fallback: None,
        };
        assert_eq!(
            image_decode_fallback(hex.clone(), &["stale".to_string()]),
            hex
        );
    }

    #[test]
    fn a_failed_audio_decode_falls_back_to_hex_with_a_reason() {
        let audio = ViewerResolution {
            viewers: Viewers::Pair(ViewerKind::Audio),
            fallback: None,
        };
        let failure = ["a.wav can't be decoded as audio: bad".to_string()];
        let resolution = decode_fallback(audio.clone(), ViewerKind::Audio, &failure);
        assert_eq!(resolution.viewers, Viewers::Pair(ViewerKind::Hex));
        assert!(resolution.fallback.unwrap().contains(&failure[0]));

        assert_eq!(
            decode_fallback(audio.clone(), ViewerKind::Image, &failure),
            audio
        );
    }

    fn resolve_pair(extension: &str) -> ViewerResolution {
        let a = UniversalPath::from(format!("a.{extension}"));
        let b = UniversalPath::from(format!("b.{extension}"));
        resolve_viewer(
            None,
            &KindTable::default(),
            side(&a, ViewerKind::Hex),
            side(&b, ViewerKind::Hex),
        )
        .unwrap()
    }

    #[test]
    fn audio_and_video_without_ffmpeg_fall_back_to_hex_with_the_not_found_reason() {
        let not_found = resolve_ffmpeg(
            Some(Path::new(r"C:\tools\ffmpeg")),
            Some(Path::new(r"C:\app")),
            |_| false,
            |_| None,
        )
        .unwrap_err();
        for extension in ["wav", "mp4"] {
            let resolution = ffmpeg_fallback(resolve_pair(extension), Err(&not_found));
            assert_eq!(
                resolution.viewers,
                Viewers::Pair(ViewerKind::Hex),
                "{extension}"
            );
            let reason = resolution.fallback.unwrap();
            assert!(reason.contains(&not_found.to_string()), "{reason}");
            assert!(reason.contains("not found"), "{reason}");
        }

        let image = resolve_pair("png");
        assert_eq!(image.viewers, Viewers::Pair(ViewerKind::Image));
        assert_eq!(ffmpeg_fallback(image.clone(), Err(&not_found)), image);
    }

    #[test]
    fn a_video_that_cant_be_probed_falls_back_to_hex_with_the_reason() {
        let failures = ["b.mp4: no video stream".to_string()];
        let resolution = decode_fallback(resolve_pair("mp4"), ViewerKind::Video, &failures);
        assert_eq!(resolution.viewers, Viewers::Pair(ViewerKind::Hex));
        let reason = resolution.fallback.unwrap();
        assert!(reason.contains("b.mp4: no video stream"), "{reason}");

        let video = resolve_pair("mp4");
        assert_eq!(
            decode_fallback(video.clone(), ViewerKind::Video, &[]),
            video
        );
        let image = resolve_pair("png");
        assert_eq!(
            decode_fallback(image.clone(), ViewerKind::Video, &failures),
            image
        );
    }

    #[test]
    fn audio_and_video_extensions_open_their_viewers_when_ffmpeg_is_found() {
        let found = resolve_ffmpeg(
            Some(Path::new(r"C:\tools\ffmpeg")),
            None,
            |_| true,
            |_| None,
        )
        .unwrap();
        for (kind, viewer) in [
            (FileKind::Audio, ViewerKind::Audio),
            (FileKind::Video, ViewerKind::Video),
        ] {
            for extension in kind.default_extensions() {
                let resolution = ffmpeg_fallback(resolve_pair(extension), Ok(&found));
                let expected = ViewerResolution {
                    viewers: Viewers::Pair(viewer),
                    fallback: None,
                };
                assert_eq!(resolution, expected, "{extension}");
            }
        }
    }

    #[test]
    fn reset_restores_the_defaults() {
        let mut map = KindTable::default();
        assert!(apply_kind_edits(
            &mut map,
            vec![
                KindEdit::Set("zbin".into(), FileKind::Binary),
                KindEdit::Set("txt".into(), FileKind::Binary),
            ]
        ));
        assert_ne!(map, KindTable::default());
        assert!(apply_kind_edits(&mut map, vec![KindEdit::Reset]));
        assert_eq!(map, KindTable::default());
        assert!(!apply_kind_edits(&mut map, vec![KindEdit::Reset]));
    }

    #[cfg(feature = "serde")]
    mod shared_file {
        use super::*;

        fn legacy(entries: &[(&str, ViewerKind)]) -> Option<BTreeMap<String, ViewerKind>> {
            Some(
                entries
                    .iter()
                    .map(|&(extension, viewer)| (extension.to_string(), viewer))
                    .collect(),
            )
        }

        #[test]
        fn an_edit_in_the_settings_page_is_written_to_the_shared_file() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("ZHashDiff").join("kinds.json");
            let mut kinds = SharedKinds::load(Some(path.clone()), &mut None);
            assert!(!path.exists(), "loading alone writes nothing");

            kinds.apply(vec![KindEdit::Set(".ZBIN".into(), FileKind::Binary)]);
            assert_eq!(KindTable::load(&path).unwrap(), zbin_hex_map());
            assert_eq!(kinds.table, zbin_hex_map());

            kinds.apply(vec![KindEdit::Remove("zbin".into())]);
            assert_eq!(KindTable::load(&path).unwrap(), KindTable::default());

            kinds.apply(vec![KindEdit::Set("dat".into(), FileKind::Text)]);
            kinds.apply(vec![KindEdit::Reset]);
            assert_eq!(KindTable::load(&path).unwrap(), KindTable::default());

            let reloaded = SharedKinds::load(Some(path), &mut None);
            assert_eq!(reloaded.table, KindTable::default());
        }

        #[test]
        fn old_viewer_map_entries_migrate_into_the_shared_file_once() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("kinds.json");
            let mut old = legacy(&[
                ("png", ViewerKind::Image),
                ("webp", ViewerKind::Image),
                (".ZBIN", ViewerKind::Hex),
                ("dat", ViewerKind::Text),
                ("jpg", ViewerKind::Hex),
                ("exe", ViewerKind::Hex),
                ("zpng", ViewerKind::Image),
            ]);
            let kinds = SharedKinds::load(Some(path.clone()), &mut old);
            assert_eq!(old, None);

            let migrated = KindTable::load(&path).unwrap();
            assert_eq!(migrated, kinds.table);
            assert_eq!(
                migrated.overrides().collect::<Vec<_>>(),
                vec![
                    ("dat", FileKind::Text),
                    ("jpg", FileKind::Binary),
                    ("zbin", FileKind::Binary),
                    ("zpng", FileKind::Image),
                ]
            );
            assert_eq!(migrated.kind("exe"), FileKind::Executable);

            let again = SharedKinds::load(Some(path.clone()), &mut None);
            assert_eq!(again.table, migrated);
        }

        #[test]
        fn an_override_already_in_the_shared_file_wins_over_the_old_viewer_map() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("kinds.json");
            let mut table = KindTable::default();
            table.set_override("dat", FileKind::Archive);
            table.save(&path).unwrap();

            let mut old = legacy(&[("dat", ViewerKind::Text), ("zbin", ViewerKind::Hex)]);
            let kinds = SharedKinds::load(Some(path.clone()), &mut old);
            assert_eq!(old, None);
            assert_eq!(kinds.table.kind("dat"), FileKind::Archive);
            assert_eq!(
                KindTable::load(&path).unwrap().kind("zbin"),
                FileKind::Binary
            );
        }

        #[test]
        fn a_broken_shared_file_is_not_overwritten_and_the_old_map_is_kept() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("kinds.json");
            std::fs::write(&path, "{ not json").unwrap();

            let mut old = legacy(&[("zbin", ViewerKind::Hex)]);
            let kinds = SharedKinds::load(Some(path.clone()), &mut old);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
            assert_eq!(old, legacy(&[("zbin", ViewerKind::Hex)]));
            assert_eq!(kinds.table.mapped_kind("zbin"), Some(FileKind::Binary));
        }

        #[test]
        fn without_a_settings_folder_the_old_map_applies_and_is_kept() {
            let mut old = legacy(&[("zbin", ViewerKind::Hex)]);
            let mut kinds = SharedKinds::load(None, &mut old);
            assert!(old.is_some());
            assert_eq!(kinds.table, zbin_hex_map());
            kinds.apply(vec![KindEdit::Remove("zbin".into())]);
            assert_eq!(kinds.table, KindTable::default());
        }
    }

    #[test]
    fn override_resets_when_a_new_pair_is_opened() {
        let a = UniversalPath::from("a.txt");
        let b = UniversalPath::from("b.txt");
        let c = UniversalPath::from("c.txt");

        let mut viewer_override = ViewerOverride::default();
        viewer_override.observe_pair(&a, &b);
        viewer_override.kind = Some(ViewerKind::Hex);

        // The same pair, every frame or after a reload, keeps it; swapping sides too.
        viewer_override.observe_pair(&a, &b);
        assert_eq!(viewer_override.kind, Some(ViewerKind::Hex));
        viewer_override.observe_pair(&b, &a);
        assert_eq!(viewer_override.kind, Some(ViewerKind::Hex));

        // Another file on either side is a new pair.
        viewer_override.observe_pair(&a, &c);
        assert_eq!(viewer_override.kind, None);

        // And the new pair keeps its own override.
        viewer_override.kind = Some(ViewerKind::Text);
        viewer_override.observe_pair(&a, &c);
        assert_eq!(viewer_override.kind, Some(ViewerKind::Text));
    }
}
