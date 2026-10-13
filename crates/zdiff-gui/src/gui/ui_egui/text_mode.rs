//! How the text viewer lays out a diff.

use super::active_side::ActiveSide;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TextMode {
    #[default]
    SideBySide,
    Unified,
    SingleLeft,
    SingleRight,
}

impl TextMode {
    pub const ALL: [Self; 4] = [
        Self::SideBySide,
        Self::Unified,
        Self::SingleLeft,
        Self::SingleRight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::SideBySide => "Side by side",
            Self::Unified => "Unified",
            Self::SingleLeft => "Single file: left",
            Self::SingleRight => "Single file: right",
        }
    }

    pub fn single_side(self) -> Option<ActiveSide> {
        match self {
            Self::SingleLeft => Some(ActiveSide::Left),
            Self::SingleRight => Some(ActiveSide::Right),
            Self::SideBySide | Self::Unified => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_single_file_modes_have_a_side() {
        assert_eq!(TextMode::SideBySide.single_side(), None);
        assert_eq!(TextMode::Unified.single_side(), None);
        assert_eq!(TextMode::SingleLeft.single_side(), Some(ActiveSide::Left));
        assert_eq!(TextMode::SingleRight.single_side(), Some(ActiveSide::Right));
    }

    #[test]
    fn every_mode_is_selectable_with_its_own_label() {
        let labels: Vec<_> = TextMode::ALL.iter().map(|m| m.label()).collect();
        for (i, label) in labels.iter().enumerate() {
            assert!(!labels[..i].contains(label), "duplicate label {label}");
        }
        assert!(TextMode::ALL.contains(&TextMode::SingleLeft));
        assert!(TextMode::ALL.contains(&TextMode::SingleRight));
    }
}
