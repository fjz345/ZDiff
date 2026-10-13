use std::{str::FromStr, sync::OnceLock};

use eframe::egui::{
    self, Color32, FontId, TextFormat, TextStyle,
    text::{LayoutJob, LayoutSection},
};
use syntect::{
    easy::HighlightLines,
    highlighting::{Color, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};
use zcommon::style::{Rgba8, SyntaxCategory, SyntaxColors};
use zdiff::{cached_file::CachedFile, lexer::RawToken, universal_path::UniversalPath};

use crate::viewer::path_extension;

// The more specific punctuation selectors keep quotes and comment markers out of Punctuation.
pub fn scopes(category: SyntaxCategory) -> Option<&'static str> {
    Some(match category {
        SyntaxCategory::Keyword => "keyword, storage",
        SyntaxCategory::Type => {
            "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, \
             entity.name.union, entity.name.trait, entity.name.impl, \
             entity.other.inherited-class, support.type, support.class"
        }
        SyntaxCategory::Function => "entity.name.function, support.function, variable.function",
        SyntaxCategory::String => "string, punctuation.definition.string",
        SyntaxCategory::Number => "constant.numeric, constant.language",
        SyntaxCategory::Comment => "comment, punctuation.definition.comment",
        SyntaxCategory::Punctuation => "punctuation, keyword.operator",
        SyntaxCategory::PlainText | SyntaxCategory::Background => return None,
    })
}

pub fn syntect_theme(colors: &SyntaxColors) -> Theme {
    let color = |category| {
        let Rgba8([r, g, b, _]) = colors.get(category);
        Color { r, g, b, a: 255 }
    };
    let scopes = SyntaxCategory::ALL
        .into_iter()
        .filter_map(|category| {
            let selectors = scopes(category)?;
            Some(ThemeItem {
                scope: ScopeSelectors::from_str(selectors)
                    .unwrap_or_else(|e| panic!("bad selectors for {category:?}: {e}")),
                style: StyleModifier {
                    foreground: Some(color(category)),
                    ..StyleModifier::default()
                },
            })
        })
        .collect();
    Theme {
        settings: ThemeSettings {
            foreground: Some(color(SyntaxCategory::PlainText)),
            background: Some(color(SyntaxCategory::Background)),
            ..ThemeSettings::default()
        },
        scopes,
        ..Theme::default()
    }
}

fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

pub fn detect_language(path: &UniversalPath, contents: &str) -> Option<&'static str> {
    let syntaxes = syntaxes();
    let plain = syntaxes.find_syntax_plain_text();
    let by_extension = path_extension(path)
        .and_then(|extension| match extension.as_str() {
            // C, C++ and Objective-C all claim `h`; syntect picks the last loaded (Objective-C).
            "h" => syntaxes.find_syntax_by_name("C++"),
            _ => syntaxes.find_syntax_by_extension(&extension),
        })
        .filter(|syntax| syntax.name != plain.name);
    let first_line = contents.trim_start_matches('\u{feff}').lines().next();
    by_extension
        .or_else(|| first_line.and_then(|line| syntaxes.find_syntax_by_first_line(line)))
        .map(|syntax| syntax.name.as_str())
}

#[derive(Debug, Default)]
pub struct DetectedLanguage {
    file: Option<(UniversalPath, String)>,
    language: Option<&'static str>,
}

impl DetectedLanguage {
    pub fn get(&mut self, file: Option<&CachedFile<RawToken>>) -> Option<&'static str> {
        let Some(file) = file else {
            *self = Self::default();
            return None;
        };
        if self
            .file
            .as_ref()
            .is_none_or(|(path, hash)| *path != file.path || *hash != file.hash)
        {
            self.file = Some((file.path.clone(), file.hash.clone()));
            self.language = detect_language(&file.path, &file.contents);
        }
        self.language
    }
}

pub fn layout_job(
    font_id: &FontId,
    theme: &Theme,
    colors: &SyntaxColors,
    code: &str,
    language: &str,
) -> LayoutJob {
    let plain = || {
        LayoutJob::simple(
            code.into(),
            font_id.clone(),
            color32(colors.get(SyntaxCategory::PlainText)),
            f32::INFINITY,
        )
    };
    let syntaxes = syntaxes();
    let Some(syntax) = syntaxes
        .find_syntax_by_name(language)
        .or_else(|| syntaxes.find_syntax_by_extension(language))
    else {
        return plain();
    };
    let mut lines = HighlightLines::new(syntax, theme);
    let mut job = LayoutJob {
        text: code.into(),
        ..Default::default()
    };
    let mut offset = 0;
    for line in LinesWithEndings::from(code) {
        let Ok(pieces) = lines.highlight_line(line, syntaxes) else {
            return plain();
        };
        for (style, piece) in pieces {
            let fg = style.foreground;
            job.sections.push(LayoutSection {
                leading_space: 0.0,
                byte_range: offset..offset + piece.len(),
                format: TextFormat {
                    font_id: font_id.clone(),
                    color: Color32::from_rgb(fg.r, fg.g, fg.b),
                    ..Default::default()
                },
            });
            offset += piece.len();
        }
    }
    job
}

#[derive(Default)]
struct Highlighter {
    theme: Option<(SyntaxColors, Theme)>,
}

impl<'a> egui::cache::ComputerMut<(&'a FontId, &'a SyntaxColors, &'a str, &'a str), LayoutJob>
    for Highlighter
{
    fn compute(
        &mut self,
        (font_id, colors, code, language): (&FontId, &SyntaxColors, &str, &str),
    ) -> LayoutJob {
        if self.theme.as_ref().is_none_or(|(built, _)| built != colors) {
            self.theme = Some((*colors, syntect_theme(colors)));
        }
        let (_, theme) = self.theme.as_ref().expect("theme was just built");
        layout_job(font_id, theme, colors, code, language)
    }
}

type HighlightCache = egui::cache::FrameCache<LayoutJob, Highlighter>;

// Memoized per frame like egui_extras' highlight; colors must be resolved before this locks ctx.
pub fn highlight(
    ctx: &egui::Context,
    style: &egui::Style,
    colors: &SyntaxColors,
    code: &str,
    language: &str,
) -> LayoutJob {
    let font_id = style
        .override_font_id
        .clone()
        .unwrap_or_else(|| TextStyle::Monospace.resolve(style));
    ctx.memory_mut(|mem| {
        mem.caches
            .cache::<HighlightCache>()
            .get((&font_id, colors, code, language))
    })
}

fn color32(Rgba8([r, g, b, _]): Rgba8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

#[cfg(test)]
mod tests {

    use syntect::{highlighting::Highlighter as SyntectHighlighter, parsing::Scope};
    use zdiff::universal_path::DepotRev;

    use super::*;

    fn distinct_colors() -> SyntaxColors {
        SyntaxColors(std::array::from_fn(|i| {
            Rgba8([10 + i as u8 * 25, 200 - i as u8 * 20, 3 * i as u8, 255])
        }))
    }

    fn syntect_color(color: Rgba8) -> Color {
        let Rgba8([r, g, b, a]) = color;
        Color { r, g, b, a }
    }

    fn foreground(theme: &Theme, stack: &[&str]) -> Color {
        let stack: Vec<Scope> = stack.iter().map(|s| Scope::from_str(s).unwrap()).collect();
        SyntectHighlighter::new(theme)
            .style_for_stack(&stack)
            .foreground
    }

    #[test]
    fn the_theme_maps_each_category_to_its_scopes() {
        let colors = distinct_colors();
        let theme = syntect_theme(&colors);
        let cases: &[(SyntaxCategory, &[&str])] = &[
            (
                SyntaxCategory::Keyword,
                &["source.rust", "keyword.control.rust"],
            ),
            (
                SyntaxCategory::Keyword,
                &["source.rust", "storage.type.function.rust"],
            ),
            (
                SyntaxCategory::Type,
                &["source.rust", "entity.name.type.struct.rust"],
            ),
            (
                SyntaxCategory::Type,
                &["source.c++", "entity.name.class.c++"],
            ),
            (SyntaxCategory::Type, &["source.rust", "support.type.rust"]),
            (
                SyntaxCategory::Function,
                &[
                    "source.rust",
                    "meta.function.rust",
                    "entity.name.function.rust",
                ],
            ),
            (
                SyntaxCategory::Function,
                &["source.python", "support.function.builtin.python"],
            ),
            (
                SyntaxCategory::Function,
                &[
                    "source.rust",
                    "meta.function-call.rust",
                    "variable.function.rust",
                ],
            ),
            (
                SyntaxCategory::String,
                &["source.rust", "string.quoted.double.rust"],
            ),
            (
                SyntaxCategory::String,
                &[
                    "source.rust",
                    "string.quoted.double.rust",
                    "punctuation.definition.string.begin.rust",
                ],
            ),
            (
                SyntaxCategory::Number,
                &["source.rust", "constant.numeric.integer.decimal.rust"],
            ),
            (
                SyntaxCategory::Number,
                &["source.rust", "constant.language.rust"],
            ),
            (
                SyntaxCategory::Comment,
                &["source.rust", "comment.line.double-slash.rust"],
            ),
            (
                SyntaxCategory::Comment,
                &[
                    "source.rust",
                    "comment.line.double-slash.rust",
                    "punctuation.definition.comment.rust",
                ],
            ),
            (
                SyntaxCategory::Punctuation,
                &["source.rust", "punctuation.separator.rust"],
            ),
            (
                SyntaxCategory::Punctuation,
                &["source.rust", "keyword.operator.arithmetic.rust"],
            ),
            (SyntaxCategory::PlainText, &["source.rust"]),
            (
                SyntaxCategory::PlainText,
                &["source.rust", "variable.other.rust"],
            ),
        ];
        for (category, stack) in cases {
            assert_eq!(
                foreground(&theme, stack),
                syntect_color(colors.get(*category)),
                "{category:?} {stack:?}"
            );
        }
        assert_eq!(
            theme.settings.foreground,
            Some(syntect_color(colors.get(SyntaxCategory::PlainText)))
        );
        assert_eq!(
            theme.settings.background,
            Some(syntect_color(colors.get(SyntaxCategory::Background)))
        );
    }

    #[test]
    fn every_text_category_has_scopes() {
        for category in SyntaxCategory::ALL {
            let expected = !matches!(
                category,
                SyntaxCategory::PlainText | SyntaxCategory::Background
            );
            assert_eq!(scopes(category).is_some(), expected, "{category:?}");
        }
    }

    #[test]
    fn rust_code_is_colored_by_category() {
        let colors = distinct_colors();
        let theme = syntect_theme(&colors);
        let font_id = FontId::monospace(12.0);
        let code = "fn main() { let x = 42; }";
        let job = layout_job(&font_id, &theme, &colors, code, "rs");
        let color_at = |needle: &str| {
            let start = code.find(needle).unwrap();
            job.sections
                .iter()
                .find(|s| s.byte_range.contains(&start))
                .unwrap()
                .format
                .color
        };
        assert_eq!(color_at("fn"), color32(colors.get(SyntaxCategory::Keyword)));
        assert_eq!(
            color_at("main"),
            color32(colors.get(SyntaxCategory::Function))
        );
        assert_eq!(color_at("42"), color32(colors.get(SyntaxCategory::Number)));
        assert_eq!(job.text, code);
        assert!(job.sections.iter().all(|s| s.format.font_id == font_id));
        let covered: usize = job.sections.iter().map(|s| s.byte_range.len()).sum();
        assert_eq!(covered, code.len());
    }

    fn detect(path: &str, contents: &str) -> Option<&'static str> {
        detect_language(&UniversalPath::from(path), contents)
    }

    #[test]
    fn the_extension_picks_the_language() {
        assert_eq!(detect(r"C:\src\main.rs", ""), Some("Rust"));
        assert_eq!(detect(r"C:\src\MAIN.RS", ""), Some("Rust"));
        assert_eq!(detect(r"C:\src\a.xml", ""), Some("XML"));
        assert_eq!(detect(r"C:\src\a.py", ""), Some("Python"));
        assert_eq!(detect(r"C:\src\a.h", ""), Some("C++"));
        assert_eq!(detect(r"C:\src\a.hpp", ""), Some("C++"));
        assert_eq!(detect(r"C:\src\a.cpp", ""), Some("C++"));
        assert_eq!(
            detect_language(
                &UniversalPath::Depot("//depot/a.b/lib.rs".into(), Some(DepotRev::Rev(5))),
                ""
            ),
            Some("Rust")
        );
    }

    #[test]
    fn the_extension_beats_the_contents() {
        assert_eq!(
            detect("a.rs", "<?xml version=\"1.0\"?>\n<a/>"),
            Some("Rust")
        );
    }

    #[test]
    fn without_a_known_extension_the_contents_are_sniffed() {
        let xml = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<root/>\n";
        assert_eq!(detect("data", xml), Some("XML"));
        assert_eq!(detect("data.zzz", xml), Some("XML"));
        assert_eq!(detect("data.txt", xml), Some("XML"));
        assert_eq!(detect("data", &format!("\u{feff}{xml}")), Some("XML"));
        assert_eq!(
            detect("tool", "#!/usr/bin/env python\nprint(1)\n"),
            Some("Python")
        );
    }

    #[test]
    fn nothing_recognizable_is_no_language() {
        assert_eq!(detect("notes.zzz", "just some words\n"), None);
        assert_eq!(detect("notes.txt", "just some words\n"), None);
        assert_eq!(detect("", ""), None);
    }

    #[test]
    fn xml_code_is_colored() {
        let colors = distinct_colors();
        let theme = syntect_theme(&colors);
        let code = "<a href=\"x\"><!-- c --></a>";
        let job = layout_job(&FontId::monospace(12.0), &theme, &colors, code, "xml");
        let color_at = |needle: &str| {
            let start = code.find(needle).unwrap();
            job.sections
                .iter()
                .find(|s| s.byte_range.contains(&start))
                .unwrap()
                .format
                .color
        };
        assert_eq!(
            color_at("\"x\""),
            color32(colors.get(SyntaxCategory::String))
        );
        assert_eq!(
            color_at("<!--"),
            color32(colors.get(SyntaxCategory::Comment))
        );
    }

    #[test]
    fn an_unknown_language_is_plain_text() {
        let colors = distinct_colors();
        let theme = syntect_theme(&colors);
        let job = layout_job(
            &FontId::monospace(12.0),
            &theme,
            &colors,
            "a b",
            "no-such-lang",
        );
        assert_eq!(job.text, "a b");
        assert!(
            job.sections
                .iter()
                .all(|s| s.format.color == color32(colors.get(SyntaxCategory::PlainText)))
        );
    }
}
