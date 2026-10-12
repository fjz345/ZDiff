//! How the text viewer lays out a diff.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TextMode {
    #[default]
    SideBySide,
    Unified,
}

impl TextMode {
    pub const ALL: [Self; 2] = [Self::SideBySide, Self::Unified];

    pub fn label(self) -> &'static str {
        match self {
            Self::SideBySide => "Side by side",
            Self::Unified => "Unified",
        }
    }
}
