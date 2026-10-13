//! Hex Strings mode: each side's printable strings as text, one string per line, so the text
//! engine diffs them and the gutter can show each string's byte offset.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

use eframe::egui;
use zdiff::{cached_file::CachedFile, lexer::RawToken};

use super::hex::is_same_side;
use crate::file::LoadedFile;

pub const DEFAULT_MIN_STRING_LEN: usize = 4;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum HexMode {
    #[default]
    Bytes,
    Strings,
}

impl HexMode {
    pub const ALL: [Self; 2] = [Self::Bytes, Self::Strings];

    pub fn label(self) -> &'static str {
        match self {
            Self::Bytes => "Bytes",
            Self::Strings => "Strings",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct HexSettings {
    pub mode: HexMode,
    pub min_string_len: usize,
}

impl Default for HexSettings {
    fn default() -> Self {
        Self {
            mode: HexMode::default(),
            min_string_len: DEFAULT_MIN_STRING_LEN,
        }
    }
}

pub fn hex_mode_ui(ui: &mut egui::Ui, settings: &mut HexSettings) {
    egui::ComboBox::from_id_salt("hex_mode")
        .selected_text(settings.mode.label())
        .show_ui(ui, |ui| {
            for mode in HexMode::ALL {
                ui.selectable_value(&mut settings.mode, mode, mode.label());
            }
        });
    if settings.mode == HexMode::Strings {
        ui.add(
            egui::DragValue::new(&mut settings.min_string_len)
                .range(1..=64)
                .prefix("min "),
        )
        .on_hover_text("Minimum string length in characters");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractedString<'a> {
    pub offset: usize,
    pub text: &'a str,
}

fn is_printable(c: char) -> bool {
    c == '\t' || !c.is_control()
}

const CANCEL_CHECK_CHARS: usize = 1 << 16;

/// Runs of printable characters (ASCII and valid UTF-8) of at least `min_len` characters, with the
/// byte offset each starts at. Bytes that aren't valid UTF-8 end a run. `None` once `cancel` is set.
pub fn extract_strings<'a>(
    bytes: &'a [u8],
    min_len: usize,
    cancel: &AtomicBool,
) -> Option<Vec<ExtractedString<'a>>> {
    let mut found = Vec::new();
    let mut chunk_start = 0;
    let mut scanned = 0usize;
    for chunk in bytes.utf8_chunks() {
        if cancel.load(Ordering::Acquire) {
            return None;
        }
        let valid = chunk.valid();
        let mut run: Option<(usize, usize)> = None;
        let mut end_run = |run: &mut Option<(usize, usize)>, end: usize| {
            if let Some((start, chars)) = run.take()
                && chars >= min_len
            {
                found.push(ExtractedString {
                    offset: chunk_start + start,
                    text: &valid[start..end],
                });
            }
        };
        for (i, c) in valid.char_indices() {
            scanned += 1;
            if scanned % CANCEL_CHECK_CHARS == 0 && cancel.load(Ordering::Acquire) {
                return None;
            }
            if is_printable(c) {
                run.get_or_insert((i, 0)).1 += 1;
            } else {
                end_run(&mut run, i);
            }
        }
        end_run(&mut run, valid.len());
        chunk_start += valid.len() + chunk.invalid().len();
    }
    Some(found)
}

/// One string per line, no trailing newline, so line `n` is string `n - 1`.
pub fn strings_text(strings: &[ExtractedString]) -> String {
    let texts: Vec<_> = strings.iter().map(|s| s.text).collect();
    texts.join("\n")
}

/// The byte offset of the string on 1-based `line_num`; `None` for no line (0).
pub fn line_offset(offsets: &[usize], line_num: usize) -> Option<usize> {
    offsets.get(line_num.checked_sub(1)?).copied()
}

#[derive(Debug, Clone)]
pub struct StringsSide {
    pub file: Arc<CachedFile<RawToken>>,
    pub offsets: Arc<[usize]>,
}

fn strings_side(
    file: &LoadedFile,
    min_len: usize,
    lexer_mode: u8,
    cancel: &AtomicBool,
) -> Option<StringsSide> {
    let strings = extract_strings(file.bytes(), min_len, cancel)?;
    let text = strings_text(&strings);
    if cancel.load(Ordering::Acquire) {
        return None;
    }
    Some(StringsSide {
        file: Arc::new(CachedFile::from_contents(
            file.path().clone(),
            text,
            lexer_mode,
        )),
        offsets: strings.iter().map(|s| s.offset).collect(),
    })
}

type StringsPair = [Option<StringsSide>; 2];

struct StringsRequest {
    files: [Option<LoadedFile>; 2],
    min_len: usize,
    lexer_mode: u8,
}

/// Extracts the strings of the current pair on a worker thread. A new request cancels the running
/// one and drops the result of an older one.
pub struct HexStrings {
    request: Option<StringsRequest>,
    sides: Option<StringsPair>,
    generation: u64,
    cancel_flag: Arc<AtomicBool>,
    channel: (
        mpsc::Sender<(u64, StringsPair)>,
        mpsc::Receiver<(u64, StringsPair)>,
    ),
}

impl Default for HexStrings {
    fn default() -> Self {
        Self {
            request: None,
            sides: None,
            generation: 0,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            channel: mpsc::channel(),
        }
    }
}

// The sides hold the strings text; input logging prints the app state.
impl std::fmt::Debug for HexStrings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HexStrings")
            .field("generation", &self.generation)
            .field("ready", &self.sides.is_some())
            .finish()
    }
}

impl HexStrings {
    /// No-op for the request already made.
    pub fn request(
        &mut self,
        ctx: &egui::Context,
        files: [Option<LoadedFile>; 2],
        min_len: usize,
        lexer_mode: u8,
    ) {
        if let Some(current) = &self.request
            && current.min_len == min_len
            && current.lexer_mode == lexer_mode
            && is_same_side(&current.files[0], &files[0])
            && is_same_side(&current.files[1], &files[1])
        {
            return;
        }
        self.cancel();
        self.generation += 1;
        self.sides = None;
        let work = files.clone();
        self.request = Some(StringsRequest {
            files,
            min_len,
            lexer_mode,
        });

        let tx = self.channel.0.clone();
        let generation = self.generation;
        let cancel_flag = self.cancel_flag.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut pair = StringsPair::default();
            for (slot, file) in pair.iter_mut().zip(&work) {
                if let Some(file) = file {
                    let Some(side) = strings_side(file, min_len, lexer_mode, &cancel_flag) else {
                        return;
                    };
                    *slot = Some(side);
                }
            }
            let _ = tx.send((generation, pair));
            ctx.request_repaint();
        });
    }

    fn cancel(&mut self) {
        self.cancel_flag.store(true, Ordering::Release);
        self.cancel_flag = Arc::new(AtomicBool::new(false));
    }

    pub fn poll(&mut self) {
        while let Ok((generation, pair)) = self.channel.1.try_recv() {
            if generation == self.generation {
                self.sides = Some(pair);
            }
        }
    }

    /// `None` while extracting.
    pub fn sides(&self) -> Option<&StringsPair> {
        self.sides.as_ref()
    }

    pub fn clear(&mut self) {
        if self.request.is_some() {
            self.cancel();
            self.generation += 1;
            self.request = None;
            self.sides = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use zdiff::diff_builder::LineContent;
    use zdiff::{diff_builder::DiffBuilderOptions, universal_path::UniversalPath};

    use super::*;
    use crate::{file::BinaryFile, ui_egui::copy_harness::CopyHarness};

    fn extract(bytes: &[u8], min_len: usize) -> Vec<ExtractedString<'_>> {
        extract_strings(bytes, min_len, &AtomicBool::new(false)).unwrap()
    }

    fn found(bytes: &[u8], min_len: usize) -> Vec<(usize, &str)> {
        extract(bytes, min_len)
            .iter()
            .map(|s| (s.offset, s.text))
            .collect()
    }

    #[test]
    fn the_default_is_bytes_with_a_minimum_of_four() {
        let settings = HexSettings::default();
        assert_eq!(settings.mode, HexMode::Bytes);
        assert_eq!(settings.min_string_len, 4);
    }

    #[test]
    fn runs_shorter_than_the_minimum_are_dropped() {
        let bytes = b"abc\0abcd\0ab\0abcde";
        assert_eq!(found(bytes, 4), [(4, "abcd"), (12, "abcde")]);
        assert_eq!(found(bytes, 3), [(0, "abc"), (4, "abcd"), (12, "abcde")]);
        assert_eq!(found(bytes, 6), []);
    }

    #[test]
    fn offsets_are_the_byte_offset_of_each_run_start() {
        let bytes = b"\0\0alpha\0\0\0beta12\xFF\xFFgamma";
        assert_eq!(
            found(bytes, 4),
            [(2, "alpha"), (10, "beta12"), (18, "gamma")]
        );
    }

    #[test]
    fn ascii_and_utf8_mix_in_one_run() {
        let bytes = "hi \u{e9}t\u{e9} ok".as_bytes();
        assert_eq!(found(bytes, 4), [(0, "hi \u{e9}t\u{e9} ok")]);
    }

    #[test]
    fn the_minimum_counts_characters_not_bytes() {
        let bytes = "\u{e9}\u{e9}\u{e9}\u{e9}\0\u{e9}\u{e9}\u{e9}".as_bytes();
        assert_eq!(found(bytes, 4), [(0, "\u{e9}\u{e9}\u{e9}\u{e9}")]);
        assert_eq!(found("\u{4e2d}\u{6587}\u{5b57}".as_bytes(), 4), []);
    }

    #[test]
    fn utf8_after_an_offset_keeps_byte_offsets() {
        let bytes = "\u{e9}\u{e9}\0abcd".as_bytes();
        assert_eq!(found(bytes, 4), [(5, "abcd")]);
    }

    #[test]
    fn invalid_utf8_splits_a_run() {
        assert_eq!(found(b"abcd\xFFefgh", 4), [(0, "abcd"), (5, "efgh")]);
        assert_eq!(found(b"abcd\x80efgh", 4), [(0, "abcd"), (5, "efgh")]);
        assert_eq!(found(b"abcd\xC3", 4), [(0, "abcd")]);
        assert_eq!(found(b"ab\xC3\xA9\xC3(cd", 4), []);
    }

    #[test]
    fn control_characters_end_a_run_but_tab_does_not() {
        assert_eq!(found(b"abcd\nefgh\r\nijkl\x7Fmnop", 4).len(), 4);
        assert_eq!(found(b"ab\tcd", 4), [(0, "ab\tcd")]);
        assert_eq!(found(b"ab\xC2\x85cd", 2), [(0, "ab"), (4, "cd")]);
    }

    #[test]
    fn no_bytes_or_no_runs_find_nothing() {
        assert_eq!(found(b"", 4), []);
        assert_eq!(found(&[0, 1, 2, 3, 0xFF], 1), []);
    }

    #[test]
    fn text_is_one_string_per_line_without_a_trailing_newline() {
        let strings = extract(b"abcd\0efgh", 4);
        assert_eq!(strings_text(&strings), "abcd\nefgh");
        assert_eq!(strings_text(&[]), "");
    }

    #[test]
    fn a_line_maps_to_its_strings_offset() {
        let offsets = [2, 10, 18];
        assert_eq!(line_offset(&offsets, 0), None);
        assert_eq!(line_offset(&offsets, 1), Some(2));
        assert_eq!(line_offset(&offsets, 3), Some(18));
        assert_eq!(line_offset(&offsets, 4), None);
    }

    #[test]
    fn diff_rows_map_back_to_each_strings_offset() {
        let left = b"\0\0alpha\0beta12\0gamma";
        let right = b"alpha\0\0\0beta12\0delta";
        let side_text = |bytes| strings_text(&extract(bytes, 4));
        let offsets =
            |bytes| -> Vec<usize> { extract(bytes, 4).iter().map(|s| s.offset).collect() };
        let harness = CopyHarness::new(
            &side_text(left),
            &side_text(right),
            &DiffBuilderOptions::default(),
        );
        let line_of = |content: &LineContent| match content {
            LineContent::Code { line_num, .. } => usize::try_from(*line_num).ok(),
            _ => None,
        };
        let shown: Vec<_> = harness
            .rows()
            .iter()
            .map(|row| {
                (
                    line_of(&row.left).and_then(|n| line_offset(&offsets(left), n)),
                    line_of(&row.right).and_then(|n| line_offset(&offsets(right), n)),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [(Some(2), Some(0)), (Some(8), Some(8)), (Some(15), Some(15))]
        );
    }

    fn binary(bytes: &[u8]) -> LoadedFile {
        LoadedFile::Binary(Arc::new(BinaryFile {
            path: UniversalPath::default(),
            bytes: bytes.to_vec(),
        }))
    }

    fn wait_for(strings: &mut HexStrings) {
        let start = Instant::now();
        while strings.sides().is_none() {
            assert!(start.elapsed() < Duration::from_secs(10), "never finished");
            std::thread::sleep(Duration::from_millis(2));
            strings.poll();
        }
    }

    #[test]
    fn a_request_extracts_each_side_off_thread() {
        let mut strings = HexStrings::default();
        let files = [Some(binary(b"\0abcd\0efgh")), None];
        strings.request(
            &egui::Context::default(),
            files.clone(),
            4,
            zdiff::lexer::LEXER_MODE_DEFAULT,
        );
        wait_for(&mut strings);

        let [left, right] = strings.sides().unwrap();
        let left = left.as_ref().unwrap();
        assert_eq!(left.file.contents, "abcd\nefgh");
        assert_eq!(&*left.offsets, [1, 6]);
        assert!(right.is_none());
    }

    #[test]
    fn a_cancelled_extraction_yields_nothing() {
        let cancel = AtomicBool::new(true);
        assert!(extract_strings(b"abcd\0efgh", 4, &cancel).is_none());
        let file = binary(b"abcd\0efgh");
        assert!(strings_side(&file, 4, zdiff::lexer::LEXER_MODE_DEFAULT, &cancel).is_none());
    }

    #[test]
    fn a_new_request_cancels_the_running_one() {
        let mut strings = HexStrings::default();
        let ctx = egui::Context::default();
        let files = [Some(binary(b"\0abcd\0efghij")), None];
        strings.request(&ctx, files.clone(), 4, zdiff::lexer::LEXER_MODE_DEFAULT);
        let first = strings.cancel_flag.clone();
        assert!(!first.load(Ordering::Acquire));
        strings.request(&ctx, files.clone(), 5, zdiff::lexer::LEXER_MODE_DEFAULT);
        assert!(first.load(Ordering::Acquire));
        assert!(!strings.cancel_flag.load(Ordering::Acquire));
        let second = strings.cancel_flag.clone();
        strings.clear();
        assert!(second.load(Ordering::Acquire));
    }

    #[test]
    fn a_changed_minimum_re_extracts_and_the_same_request_does_not() {
        let mut strings = HexStrings::default();
        let ctx = egui::Context::default();
        let files = [Some(binary(b"\0abcd\0efghij")), None];
        strings.request(&ctx, files.clone(), 4, zdiff::lexer::LEXER_MODE_DEFAULT);
        wait_for(&mut strings);

        strings.request(&ctx, files.clone(), 4, zdiff::lexer::LEXER_MODE_DEFAULT);
        assert!(strings.sides().is_some());

        strings.request(&ctx, files.clone(), 5, zdiff::lexer::LEXER_MODE_DEFAULT);
        assert!(strings.sides().is_none());
        wait_for(&mut strings);
        let [left, _] = strings.sides().unwrap();
        assert_eq!(left.as_ref().unwrap().file.contents, "efghij");

        strings.clear();
        assert!(strings.sides().is_none());
    }
}
