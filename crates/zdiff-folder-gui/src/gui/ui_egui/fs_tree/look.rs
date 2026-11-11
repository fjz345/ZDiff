use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderRole {
    Same,
    Different,
    Similar,
    OnlyInFirst,
    OnlyInSecond,
    PendingDeletion,
    RowTint,
    DimmedText,
    GuideLine,
    TableBackground,
    Tag(FileKind),
    BlueprintTag,
}

impl Role for FolderRole {
    fn all() -> &'static [Self] {
        &[
            FolderRole::Same,
            FolderRole::Different,
            FolderRole::Similar,
            FolderRole::OnlyInFirst,
            FolderRole::OnlyInSecond,
            FolderRole::PendingDeletion,
            FolderRole::RowTint,
            FolderRole::DimmedText,
            FolderRole::GuideLine,
            FolderRole::TableBackground,
            FolderRole::Tag(FileKind::Text),
            FolderRole::Tag(FileKind::Image),
            FolderRole::Tag(FileKind::Video),
            FolderRole::Tag(FileKind::Audio),
            FolderRole::Tag(FileKind::Model),
            FolderRole::Tag(FileKind::Archive),
            FolderRole::Tag(FileKind::Executable),
            FolderRole::Tag(FileKind::DebugSymbols),
            FolderRole::Tag(FileKind::Intermediate),
            FolderRole::Tag(FileKind::UnrealAsset),
            FolderRole::Tag(FileKind::UnrealMap),
            FolderRole::Tag(FileKind::Binary),
            FolderRole::BlueprintTag,
        ]
    }

    fn name(self) -> &'static str {
        match self {
            FolderRole::Same => "folder_same",
            FolderRole::Different => "folder_different",
            FolderRole::Similar => "folder_similar",
            FolderRole::OnlyInFirst => "folder_only_in_first",
            FolderRole::OnlyInSecond => "folder_only_in_second",
            FolderRole::PendingDeletion => "folder_pending_deletion",
            FolderRole::RowTint => "folder_row_tint",
            FolderRole::DimmedText => "folder_dimmed_text",
            FolderRole::GuideLine => "folder_guide_line",
            FolderRole::TableBackground => "folder_table_background",
            FolderRole::Tag(kind) => match kind {
                FileKind::Text => "folder_tag_text",
                FileKind::Image => "folder_tag_image",
                FileKind::Video => "folder_tag_video",
                FileKind::Audio => "folder_tag_audio",
                FileKind::Model => "folder_tag_model",
                FileKind::Archive => "folder_tag_archive",
                FileKind::Executable => "folder_tag_executable",
                FileKind::DebugSymbols => "folder_tag_debug_symbols",
                FileKind::Intermediate => "folder_tag_intermediate",
                FileKind::UnrealAsset => "folder_tag_unreal_asset",
                FileKind::UnrealMap => "folder_tag_unreal_map",
                FileKind::Binary => "folder_tag_binary",
            },
            FolderRole::BlueprintTag => "folder_tag_blueprint",
        }
    }

    fn label(self) -> &'static str {
        match self {
            FolderRole::Same => "Same",
            FolderRole::Different => "Different",
            FolderRole::Similar => "Similar",
            FolderRole::OnlyInFirst => "Only in first",
            FolderRole::OnlyInSecond => "Only in second",
            FolderRole::PendingDeletion => "Pending deletion",
            FolderRole::RowTint => "Folder row tint",
            FolderRole::DimmedText => "Dimmed text",
            FolderRole::GuideLine => "Guide lines",
            FolderRole::TableBackground => "Table background",
            FolderRole::Tag(kind) => match kind {
                FileKind::Text => "Tag txt",
                FileKind::Image => "Tag img",
                FileKind::Video => "Tag vid",
                FileKind::Audio => "Tag aud",
                FileKind::Model => "Tag 3d",
                FileKind::Archive => "Tag arc",
                FileKind::Executable => "Tag exe",
                FileKind::DebugSymbols => "Tag dbg",
                FileKind::Intermediate => "Tag int",
                FileKind::UnrealAsset => "Tag UASS",
                FileKind::UnrealMap => "Tag UMAP",
                FileKind::Binary => "Tag bin",
            },
            FolderRole::BlueprintTag => "Tag BP (Blueprint)",
        }
    }

    fn dark(self) -> Rgba8 {
        match self {
            FolderRole::Same => Rgba8([0x7E, 0xD3, 0x21, 255]),
            FolderRole::Different => Rgba8([0xD0, 0x02, 0x1B, 255]),
            FolderRole::Similar => Rgba8([0xF8, 0xE7, 0x1C, 255]),
            FolderRole::OnlyInFirst | FolderRole::OnlyInSecond => Rgba8([0x4A, 0x90, 0xE2, 255]),
            FolderRole::PendingDeletion => Rgba8([255, 128, 128, 255]),
            FolderRole::RowTint => Rgba8([120, 150, 255, 14]),
            FolderRole::DimmedText => Rgba8([105, 105, 105, 255]),
            FolderRole::GuideLine => Rgba8([95, 95, 95, 255]),
            FolderRole::TableBackground => Rgba8([20, 20, 20, 255]),
            FolderRole::Tag(kind) => match kind {
                FileKind::Text => Rgba8([170, 190, 215, 255]),
                FileKind::Image => Rgba8([215, 130, 220, 255]),
                FileKind::Video => Rgba8([235, 150, 80, 255]),
                FileKind::Audio => Rgba8([90, 200, 190, 255]),
                FileKind::Model => Rgba8([140, 210, 110, 255]),
                FileKind::Archive => Rgba8([210, 180, 110, 255]),
                FileKind::Executable => Rgba8([230, 105, 105, 255]),
                FileKind::DebugSymbols => Rgba8([225, 140, 175, 255]),
                FileKind::Intermediate => Rgba8([195, 150, 150, 255]),
                FileKind::UnrealAsset => Rgba8([100, 170, 255, 255]),
                FileKind::UnrealMap => Rgba8([150, 140, 255, 255]),
                FileKind::Binary => Rgba8([130, 130, 130, 255]),
            },
            FolderRole::BlueprintTag => Rgba8([70, 120, 255, 255]),
        }
    }

    fn light(self) -> Rgba8 {
        match self {
            FolderRole::Same => Rgba8([40, 140, 20, 255]),
            FolderRole::Different => Rgba8([200, 0, 20, 255]),
            FolderRole::Similar => Rgba8([160, 120, 0, 255]),
            FolderRole::OnlyInFirst | FolderRole::OnlyInSecond => Rgba8([30, 100, 200, 255]),
            FolderRole::PendingDeletion => Rgba8([200, 40, 40, 255]),
            FolderRole::RowTint => Rgba8([60, 100, 230, 22]),
            FolderRole::DimmedText => Rgba8([160, 160, 160, 255]),
            FolderRole::GuideLine => Rgba8([185, 185, 185, 255]),
            FolderRole::TableBackground => Rgba8([248, 248, 248, 255]),
            FolderRole::Tag(kind) => match kind {
                FileKind::Text => Rgba8([70, 95, 130, 255]),
                FileKind::Image => Rgba8([150, 50, 160, 255]),
                FileKind::Video => Rgba8([190, 95, 20, 255]),
                FileKind::Audio => Rgba8([20, 130, 120, 255]),
                FileKind::Model => Rgba8([60, 140, 30, 255]),
                FileKind::Archive => Rgba8([140, 105, 30, 255]),
                FileKind::Executable => Rgba8([190, 40, 40, 255]),
                FileKind::DebugSymbols => Rgba8([170, 55, 105, 255]),
                FileKind::Intermediate => Rgba8([140, 80, 80, 255]),
                FileKind::UnrealAsset => Rgba8([30, 100, 200, 255]),
                FileKind::UnrealMap => Rgba8([90, 70, 210, 255]),
                FileKind::Binary => Rgba8([110, 110, 110, 255]),
            },
            FolderRole::BlueprintTag => Rgba8([30, 80, 220, 255]),
        }
    }

    fn has_alpha(self) -> bool {
        self == FolderRole::RowTint
    }
}
pub(super) const FIND_MATCH_BG: egui::Color32 =
    egui::Color32::from_rgba_premultiplied(90, 66, 0, 140);
pub(super) const FIND_CHIP_MAX_CHARS: usize = 24;
pub(super) const TREE_INDENT: f32 = 16.0;
pub(super) const EXPANDER_WIDTH: f32 = 12.0;
