use eframe::egui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageIcon {
    Emoji(&'static str),
    Badge(&'static str, egui::Color32),
}

const BADGE_CPP: egui::Color32 = egui::Color32::from_rgb(0x00, 0x59, 0x9c);
const BADGE_JS: egui::Color32 = egui::Color32::from_rgb(0xb5, 0x9a, 0x00);
const BADGE_JSON: egui::Color32 = egui::Color32::from_rgb(0x5a, 0x5a, 0x5a);
const BADGE_XML: egui::Color32 = egui::Color32::from_rgb(0x00, 0x7a, 0x5a);
const BADGE_MD: egui::Color32 = egui::Color32::from_rgb(0x3a, 0x7a, 0xb0);
const BADGE_CUSTOM: egui::Color32 = egui::Color32::from_rgb(0x7a, 0x4a, 0x9a);
const BADGE_RS: egui::Color32 = egui::Color32::from_rgb(0xb7, 0x41, 0x0e);
const BADGE_PY: egui::Color32 = egui::Color32::from_rgb(0x30, 0x70, 0xa8);

pub fn language_icon(syntax_name: &str, renders: impl Fn(&str) -> bool) -> LanguageIcon {
    let emoji_or = |emoji: &'static str, text: &'static str, color| {
        if renders(emoji) {
            LanguageIcon::Emoji(emoji)
        } else {
            LanguageIcon::Badge(text, color)
        }
    };
    match syntax_name {
        "Rust" => emoji_or("🦀", "RS", BADGE_RS),
        "Python" => emoji_or("🐍", "PY", BADGE_PY),
        "C++" => LanguageIcon::Badge("C++", BADGE_CPP),
        "JavaScript" => LanguageIcon::Badge("JS", BADGE_JS),
        "JSON" => LanguageIcon::Badge("{}", BADGE_JSON),
        "XML" => LanguageIcon::Badge("<>", BADGE_XML),
        "Markdown" => LanguageIcon::Badge("MD", BADGE_MD),
        _ => LanguageIcon::Badge("…", BADGE_CUSTOM),
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
            assert_eq!(text("Go"), "…");
            assert_eq!(text(""), "…");
        }
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
}
