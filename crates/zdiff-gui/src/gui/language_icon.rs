use eframe::egui;

use crate::ui_egui::syntax::variant_base;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageIcon {
    Emoji(&'static str),
    Badge(&'static str, egui::Color32),
}

const BADGE_CUSTOM: egui::Color32 = egui::Color32::from_rgb(0x7a, 0x4a, 0x9a);
const BADGE_RS: egui::Color32 = egui::Color32::from_rgb(0xb7, 0x41, 0x0e);
const BADGE_PY: egui::Color32 = egui::Color32::from_rgb(0x30, 0x70, 0xa8);
const BADGE_SYSTEM: egui::Color32 = rgb(0x4a5560);
const BADGE_SSH: egui::Color32 = rgb(0x4a4a6a);
const BADGE_KEY: egui::Color32 = rgb(0x8a6d1a);
const BADGE_LOG: egui::Color32 = rgb(0x6a6a5a);

const fn rgb(hex: u32) -> egui::Color32 {
    egui::Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

// Plain ASCII so every badge renders in the monospace font; Git and SSH are variant families.
const BADGES: &[(&str, &str, egui::Color32)] = &[
    ("ActionScript", "AS3", rgb(0x882b0f)),
    ("Ada", "ADA", rgb(0x0a7a46)),
    ("Apache Conf", "APA", rgb(0xd12127)),
    ("AppleScript", "AS", rgb(0x2a3a3a)),
    ("AsciiDoc (Asciidoctor)", "AD", rgb(0xe40046)),
    ("ASP", "ASP", rgb(0x6a40fd)),
    ("Authorized Keys", "KEY", BADGE_KEY),
    ("AWK", "AWK", rgb(0xc30e9b)),
    ("Batch File", "C:\\", rgb(0x4d4d4d)),
    ("BibTeX", "BIB", rgb(0x556677)),
    ("Bourne Again Shell (bash)", ">_", rgb(0x3a6e2a)),
    ("C", "C", rgb(0x5c6bc0)),
    ("C#", "C#", rgb(0x68217a)),
    ("C++", "C++", rgb(0x00599c)),
    ("Cabal", "CBL", rgb(0x483465)),
    ("CFML", "CFM", rgb(0x2a5a9a)),
    ("Clojure", "CLJ", rgb(0x5881d8)),
    ("CMake", "CM", rgb(0xda3434)),
    ("CoffeeScript", "COF", rgb(0x244776)),
    ("Command Help", "-h", rgb(0x4a4a4a)),
    ("CpuInfo", "CPU", BADGE_SYSTEM),
    ("Crontab", "CRN", BADGE_SYSTEM),
    ("Crystal", "CR", rgb(0x2e2e2e)),
    ("CSS", "CSS", rgb(0x1572b6)),
    ("D", "D", rgb(0xba595e)),
    ("Dart", "DRT", rgb(0x0175c2)),
    ("debsources", "DEB", rgb(0xa80030)),
    ("Diff", "+-", rgb(0x4a7a4a)),
    ("Dockerfile", "DKR", rgb(0x1d63ed)),
    ("DotENV", "ENV", rgb(0x7a6e00)),
    ("Elixir", "EX", rgb(0x6e4a7e)),
    ("Elm", "ELM", rgb(0x1293d8)),
    ("Email", "@", rgb(0x4a6a8a)),
    ("Erlang", "ERL", rgb(0xa90533)),
    ("F#", "F#", rgb(0x378bba)),
    ("Fish", "><>", rgb(0x357a32)),
    ("Fortran (Modern)", "F90", rgb(0x734f96)),
    ("fstab", "FS", BADGE_SYSTEM),
    ("GDScript (Godot Engine)", "GD", rgb(0x478cbf)),
    ("Git", "GIT", rgb(0xf05032)),
    ("GLSL", "GL", rgb(0x5686a5)),
    ("gnuplot", "GP", rgb(0x8a3a8a)),
    ("Go", "GO", rgb(0x007d9c)),
    ("GraphQL", "GQL", rgb(0xe10098)),
    ("Graphviz (DOT)", "DOT", rgb(0x2596be)),
    ("Groff/troff", "ROF", rgb(0x7a6a3a)),
    ("Groovy", "GVY", rgb(0x4298b8)),
    ("group", "GRP", BADGE_SYSTEM),
    ("Haskell", ">>=", rgb(0x5e5086)),
    ("HTML", "</>", rgb(0xe34c26)),
    ("HTTP Request and Response", "://", rgb(0x005c9c)),
    ("Idris", "IDR", rgb(0xb30000)),
    ("INI", "INI", rgb(0x5a6a72)),
    ("Java", "JV", rgb(0xb07219)),
    ("JavaScript", "JS", rgb(0xb59a00)),
    ("Jinja2", "J2", rgb(0xa52a22)),
    ("JQ", "JQ", rgb(0x7a4a2a)),
    ("JSON", "{}", rgb(0x5a5a5a)),
    ("jsonnet", "JN", rgb(0x0064bd)),
    ("Julia", "JL", rgb(0x9558b2)),
    ("Known Hosts", "SSH", BADGE_SSH),
    ("Kotlin", "KT", rgb(0x7f52ff)),
    ("LaTeX", "TEX", rgb(0x3d6117)),
    ("Lean 4", "LN", rgb(0x494e8a)),
    ("Less", "LES", rgb(0x1d365d)),
    ("Lisp", "()", rgb(0x2a7a5c)),
    ("LiveScript", "LS", rgb(0x499886)),
    ("LLVM", "LL", rgb(0x185619)),
    ("log", "LOG", BADGE_LOG),
    ("Lua", "LUA", rgb(0x000080)),
    ("Makefile", "MK", rgb(0x427819)),
    ("Manpage", "MAN", rgb(0x4a4a4a)),
    ("Markdown", "MD", rgb(0x3a7ab0)),
    ("MATLAB", "MAT", rgb(0xc65a1e)),
    ("MediaWiki", "W", rgb(0x3366cc)),
    ("MemInfo", "MEM", BADGE_SYSTEM),
    ("NAnt Build File", "<>", rgb(0x512bd4)),
    ("nginx", "NGX", rgb(0x009639)),
    ("Nim", "NIM", rgb(0x9a7400)),
    ("Ninja", "NJ", rgb(0x3a3a3a)),
    ("Nix", "NIX", rgb(0x5277c3)),
    ("NSIS", "NSI", rgb(0x4a7a9a)),
    ("Objective-C", "OC", rgb(0x438eff)),
    ("Objective-C++", "O++", rgb(0x6866fb)),
    ("OCaml", "ML", rgb(0xd8600e)),
    ("Odin", "ODN", rgb(0x3882d2)),
    ("orgmode", "ORG", rgb(0x4a7a6a)),
    ("Pascal", "PAS", rgb(0x9b4a4a)),
    ("passwd", "PW", BADGE_SYSTEM),
    ("Perl", "PL", rgb(0x39457e)),
    ("PHP", "PHP", rgb(0x777bb4)),
    ("Plain Text", "TXT", rgb(0x6a6a6a)),
    ("Private Key", "KEY", BADGE_KEY),
    ("Protocol Buffer", "PB", rgb(0x4285f4)),
    ("Puppet", "PP", rgb(0x302b6d)),
    ("PureScript", "PUR", rgb(0x2d3440)),
    ("QML", "QML", rgb(0x2e8b3a)),
    ("R", "R", rgb(0x276dc3)),
    ("Racket", "RKT", rgb(0x3c5caa)),
    ("Rego", "REG", rgb(0x7d9199)),
    ("Regular Expression", ".*", rgb(0x2a7a2a)),
    ("Requirements.txt", "PIP", rgb(0x3776ab)),
    ("resolv", "DNS", BADGE_SYSTEM),
    ("reStructuredText", "RST", rgb(0x2a5a8a)),
    ("Robot Framework", "RF", rgb(0x00807a)),
    ("Ruby", "RB", rgb(0xcc342d)),
    ("Sass", "SAS", rgb(0xcc6699)),
    ("Scala", "SC", rgb(0xc22d40)),
    ("SCSS", "SCS", rgb(0xc6538c)),
    ("Separated Values", "CSV", rgb(0x237346)),
    ("SML", "SML", rgb(0xdc566d)),
    ("Solidity", "SOL", rgb(0xaa6746)),
    ("SQL", "SQL", rgb(0x336791)),
    ("SSH", "SSH", BADGE_SSH),
    ("Stylus", "STY", rgb(0xb03a2e)),
    ("Svelte", "SVT", rgb(0xe03a00)),
    ("Swift", "SW", rgb(0xf05138)),
    ("syslog", "LOG", BADGE_LOG),
    ("SystemVerilog", "SV", rgb(0x5a7a3a)),
    ("Tcl", "TCL", rgb(0x8a6a2a)),
    ("Terraform", "TF", rgb(0x7b42bc)),
    ("TeX", "TeX", rgb(0x2a4a6a)),
    ("Textile", "TXL", rgb(0x8a5a3a)),
    ("Todo.txt", "[x]", rgb(0x2a6a8a)),
    ("TOML", "TML", rgb(0x9c4221)),
    ("TypeScript", "TS", rgb(0x3178c6)),
    ("Typst", "TYP", rgb(0x239dad)),
    ("Verilog", "V", rgb(0x5a64c8)),
    ("VHDL", "VHD", rgb(0x543978)),
    ("VimL", "VIM", rgb(0x199f4b)),
    ("Vue Component", "VUE", rgb(0x2f855a)),
    ("Vyper", "VY", rgb(0x2980b9)),
    ("WGSL", "WG", rgb(0x1a5e9a)),
    ("x86_64 Assembly", "ASM", rgb(0x6e4c13)),
    ("XML", "<>", rgb(0x007a5a)),
    ("YAML", "YML", rgb(0xcb171e)),
    ("Zig", "ZIG", rgb(0xb8661e)),
];

pub fn language_icon(syntax_name: &str, renders: impl Fn(&str) -> bool) -> LanguageIcon {
    let emoji_or = |emoji: &'static str, text: &'static str, color| {
        if renders(emoji) {
            LanguageIcon::Emoji(emoji)
        } else {
            LanguageIcon::Badge(text, color)
        }
    };
    let name = variant_base(syntax_name).unwrap_or(syntax_name);
    match name {
        "Rust" => emoji_or("🦀", "RS", BADGE_RS),
        "Python" => emoji_or("🐍", "PY", BADGE_PY),
        _ => BADGES
            .iter()
            .find(|&&(badge_name, ..)| badge_name == name)
            .map_or(
                LanguageIcon::Badge("…", BADGE_CUSTOM),
                |&(_, text, color)| LanguageIcon::Badge(text, color),
            ),
    }
}

pub fn show_language_icon(ui: &mut egui::Ui, icon: LanguageIcon) {
    match icon {
        LanguageIcon::Emoji(emoji) => {
            ui.label(emoji);
        }
        LanguageIcon::Badge(text, color) => {
            ui.label(
                egui::RichText::new(text)
                    .monospace()
                    .small()
                    .color(egui::Color32::WHITE)
                    .background_color(color),
            );
        }
    }
}

pub fn emoji_renders(ctx: &egui::Context, text: &str) -> bool {
    ctx.fonts_mut(|fonts| fonts.has_glyphs(&egui::FontId::proportional(14.0), text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn badge_text(icon: LanguageIcon) -> &'static str {
        match icon {
            LanguageIcon::Badge(text, _) => text,
            LanguageIcon::Emoji(e) => panic!("expected a badge, got emoji {e}"),
        }
    }

    #[test]
    fn rust_and_python_use_emoji_when_they_render() {
        assert_eq!(language_icon("Rust", |_| true), LanguageIcon::Emoji("🦀"));
        assert_eq!(language_icon("Python", |_| true), LanguageIcon::Emoji("🐍"));
    }

    #[test]
    fn an_emoji_that_does_not_render_falls_back_to_a_badge() {
        assert_eq!(badge_text(language_icon("Rust", |_| false)), "RS");
        assert_eq!(badge_text(language_icon("Python", |_| false)), "PY");
        assert_eq!(badge_text(language_icon("Rust", |e| e != "🦀")), "RS");
        assert_eq!(
            language_icon("Python", |e| e != "🦀"),
            LanguageIcon::Emoji("🐍")
        );
    }

    #[test]
    fn other_languages_are_badges_whether_or_not_emoji_render() {
        for renders in [true, false] {
            let text = |name| badge_text(language_icon(name, |_| renders));
            assert_eq!(text("C++"), "C++");
            assert_eq!(text("JavaScript"), "JS");
            assert_eq!(text("JSON"), "{}");
            assert_eq!(text("XML"), "<>");
            assert_eq!(text("Markdown"), "MD");
            assert_eq!(text("Go"), "GO");
            assert_eq!(text("HTML"), "</>");
            assert_eq!(text(""), "…");
            assert_eq!(text("Not a syntax"), "…");
        }
    }

    #[test]
    fn variants_show_their_base_icon() {
        let icon = |name| language_icon(name, |_| false);
        assert_eq!(icon("HTML (Rails)"), icon("HTML"));
        assert_eq!(icon("Gomod"), icon("Go"));
        assert_eq!(icon("TypeScriptReact"), icon("TypeScript"));
        assert_eq!(badge_text(icon("Git Config")), "GIT");
        assert_eq!(icon("Git Rebase Todo"), icon("Git Config"));
        assert_eq!(badge_text(icon("SSH Config")), "SSH");
    }

    #[test]
    fn only_languages_without_a_sensible_icon_fall_back_to_the_ellipsis() {
        let fallback = language_icon("", |_| false);
        let without_icon: Vec<&str> = crate::ui_egui::syntax::languages()
            .iter()
            .map(|l| l.name)
            .filter(|name| language_icon(name, |_| false) == fallback)
            .collect();
        assert_eq!(
            without_icon,
            ["Highlight non-printables", "Strace", "varlink"]
        );
    }

    #[test]
    fn every_named_icon_is_a_listed_language() {
        let listed: Vec<&str> = crate::ui_egui::syntax::languages()
            .iter()
            .map(|l| l.name)
            .collect();
        for name in [
            "Rust",
            "Python",
            "C++",
            "JavaScript",
            "JSON",
            "XML",
            "Markdown",
        ] {
            assert!(listed.contains(&name), "{name}");
            assert_ne!(language_icon(name, |_| false), language_icon("", |_| false));
        }
    }

    #[test]
    fn every_badge_is_one_to_three_ascii_chars() {
        for &(name, text, _) in BADGES {
            assert!(
                (1..=3).contains(&text.len()) && text.is_ascii(),
                "{name}: {text:?}"
            );
        }
    }

    #[test]
    fn every_badge_names_one_main_language_or_variant_family() {
        let languages = crate::ui_egui::syntax::languages();
        for &(name, ..) in BADGES {
            let listed = languages.iter().any(|l| l.name == name);
            let family = languages.iter().any(|l| variant_base(l.name) == Some(name));
            assert!(listed || family, "{name}");
            assert_eq!(variant_base(name), None, "{name}");
            assert_eq!(
                BADGES.iter().filter(|(n, ..)| *n == name).count(),
                1,
                "{name}"
            );
        }
    }

    #[test]
    fn every_variant_shares_its_base_icon_and_the_base_has_one() {
        let fallback = language_icon("", |_| false);
        for language in crate::ui_egui::syntax::languages() {
            if let Some(base) = variant_base(language.name) {
                let icon = language_icon(language.name, |_| false);
                assert_eq!(icon, language_icon(base, |_| false), "{}", language.name);
                assert_ne!(icon, fallback, "{}", language.name);
            }
        }
    }
}
