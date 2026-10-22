use zcommon::style::{Rgba8, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRole {
    Modified,
    GhostText,
    DimmedText,
    DeleteChangedChars,
    InsertChangedChars,
    DeleteChangedTokens,
    InsertChangedTokens,
    DeleteChangedScope,
    InsertChangedScope,
    GotoLineNumber,
    GotoRow,
    Find,
    FindCurrent,
    Occurrence,
    VoidRow,
    CollapsedRow,
    TableBackground,
    LineNumber,
    HexDifferent,
    HexOnlyHere,
    HexGoto,
    HexByteNull,
    HexBytePrintable,
    HexByteControl,
    HexByteHigh,
    HexByteFull,
    ImageDiff,
    RulerDeleted,
    RulerInserted,
    RulerChanged,
}

impl Role for FileRole {
    fn all() -> &'static [Self] {
        &[
            FileRole::Modified,
            FileRole::GhostText,
            FileRole::DimmedText,
            FileRole::DeleteChangedChars,
            FileRole::InsertChangedChars,
            FileRole::DeleteChangedTokens,
            FileRole::InsertChangedTokens,
            FileRole::DeleteChangedScope,
            FileRole::InsertChangedScope,
            FileRole::GotoLineNumber,
            FileRole::GotoRow,
            FileRole::Find,
            FileRole::FindCurrent,
            FileRole::Occurrence,
            FileRole::VoidRow,
            FileRole::CollapsedRow,
            FileRole::TableBackground,
            FileRole::LineNumber,
            FileRole::HexDifferent,
            FileRole::HexOnlyHere,
            FileRole::HexGoto,
            FileRole::HexByteNull,
            FileRole::HexBytePrintable,
            FileRole::HexByteControl,
            FileRole::HexByteHigh,
            FileRole::HexByteFull,
            FileRole::ImageDiff,
            FileRole::RulerDeleted,
            FileRole::RulerInserted,
            FileRole::RulerChanged,
        ]
    }

    fn name(self) -> &'static str {
        match self {
            FileRole::Modified => "file_modified",
            FileRole::GhostText => "file_ghost_text",
            FileRole::DimmedText => "file_dimmed_text",
            FileRole::DeleteChangedChars => "file_delete_changed_chars",
            FileRole::InsertChangedChars => "file_insert_changed_chars",
            FileRole::DeleteChangedTokens => "file_delete_changed_tokens",
            FileRole::InsertChangedTokens => "file_insert_changed_tokens",
            FileRole::DeleteChangedScope => "file_delete_changed_scope",
            FileRole::InsertChangedScope => "file_insert_changed_scope",
            FileRole::GotoLineNumber => "file_goto_line_number",
            FileRole::GotoRow => "file_goto_row",
            FileRole::Find => "file_find",
            FileRole::FindCurrent => "file_find_current",
            FileRole::Occurrence => "file_occurrence",
            FileRole::VoidRow => "file_void_row",
            FileRole::CollapsedRow => "file_collapsed_row",
            FileRole::TableBackground => "file_table_background",
            FileRole::LineNumber => "file_line_number",
            FileRole::HexDifferent => "file_hex_different",
            FileRole::HexOnlyHere => "file_hex_only_here",
            FileRole::HexGoto => "file_hex_goto",
            FileRole::HexByteNull => "file_hex_byte_null",
            FileRole::HexBytePrintable => "file_hex_byte_printable",
            FileRole::HexByteControl => "file_hex_byte_control",
            FileRole::HexByteHigh => "file_hex_byte_high",
            FileRole::HexByteFull => "file_hex_byte_full",
            FileRole::ImageDiff => "file_image_diff",
            FileRole::RulerDeleted => "file_ruler_deleted",
            FileRole::RulerInserted => "file_ruler_inserted",
            FileRole::RulerChanged => "file_ruler_changed",
        }
    }

    fn label(self) -> &'static str {
        match self {
            FileRole::Modified => "Modified marker",
            FileRole::GhostText => "Ghost text",
            FileRole::DimmedText => "Ignored text",
            FileRole::DeleteChangedChars => "Delete changed chars",
            FileRole::InsertChangedChars => "Insert changed chars",
            FileRole::DeleteChangedTokens => "Delete changed tokens",
            FileRole::InsertChangedTokens => "Insert changed tokens",
            FileRole::DeleteChangedScope => "Delete changed scope",
            FileRole::InsertChangedScope => "Insert changed scope",
            FileRole::GotoLineNumber => "Goto line number",
            FileRole::GotoRow => "Goto row",
            FileRole::Find => "Find match",
            FileRole::FindCurrent => "Current find match",
            FileRole::Occurrence => "Selection occurrence",
            FileRole::VoidRow => "Void row",
            FileRole::CollapsedRow => "Collapsed row",
            FileRole::TableBackground => "Table background",
            FileRole::LineNumber => "Line numbers",
            FileRole::HexDifferent => "Hex different bytes",
            FileRole::HexOnlyHere => "Hex bytes on one side only",
            FileRole::HexGoto => "Hex goto highlight",
            FileRole::HexByteNull => "Hex 00 bytes",
            FileRole::HexBytePrintable => "Hex printable ASCII bytes",
            FileRole::HexByteControl => "Hex ASCII control bytes",
            FileRole::HexByteHigh => "Hex non-ASCII bytes",
            FileRole::HexByteFull => "Hex FF bytes",
            FileRole::ImageDiff => "Image difference",
            FileRole::RulerDeleted => "Ruler deleted",
            FileRole::RulerInserted => "Ruler inserted",
            FileRole::RulerChanged => "Ruler changed",
        }
    }

    fn dark(self) -> Rgba8 {
        match self {
            FileRole::Modified => Rgba8([255, 255, 100, 255]),
            FileRole::GhostText => Rgba8([150, 150, 150, 80]),
            FileRole::DimmedText => Rgba8([128, 128, 128, 110]),
            FileRole::DeleteChangedChars => Rgba8([255, 0, 0, 80]),
            FileRole::InsertChangedChars => Rgba8([0, 255, 0, 70]),
            FileRole::DeleteChangedTokens => Rgba8([255, 0, 0, 45]),
            FileRole::InsertChangedTokens => Rgba8([0, 255, 0, 40]),
            FileRole::DeleteChangedScope => Rgba8([255, 0, 0, 10]),
            FileRole::InsertChangedScope => Rgba8([0, 255, 0, 10]),
            FileRole::GotoLineNumber => Rgba8([255, 210, 0, 255]),
            FileRole::GotoRow => Rgba8([255, 255, 0, 40]),
            FileRole::Find => Rgba8([164, 120, 0, 140]),
            FileRole::FindCurrent => Rgba8([176, 104, 0, 255]),
            // Purple: apart from find amber, change red and green, and egui's blue selection.
            FileRole::Occurrence => Rgba8([153, 93, 223, 110]),
            FileRole::VoidRow => Rgba8([30, 30, 30, 255]),
            FileRole::CollapsedRow => Rgba8([37, 43, 54, 255]),
            FileRole::TableBackground => Rgba8([15, 15, 15, 255]),
            FileRole::LineNumber => Rgba8([96, 96, 96, 255]),
            FileRole::HexDifferent => Rgba8([255, 70, 70, 110]),
            FileRole::HexOnlyHere => Rgba8([70, 209, 70, 110]),
            FileRole::HexGoto => Rgba8([255, 210, 60, 255]),
            FileRole::HexByteNull => Rgba8([140, 140, 140, 255]),
            FileRole::HexBytePrintable => Rgba8([125, 200, 235, 255]),
            FileRole::HexByteControl => Rgba8([160, 150, 245, 255]),
            FileRole::HexByteHigh => Rgba8([225, 130, 215, 255]),
            FileRole::HexByteFull => Rgba8([235, 235, 235, 255]),
            FileRole::ImageDiff => Rgba8([255, 0, 255, 255]),
            FileRole::RulerDeleted => Rgba8([230, 70, 70, 255]),
            FileRole::RulerInserted => Rgba8([70, 200, 70, 255]),
            FileRole::RulerChanged => Rgba8([230, 190, 60, 255]),
        }
    }

    fn light(self) -> Rgba8 {
        match self {
            FileRole::Modified => Rgba8([170, 130, 0, 255]),
            FileRole::GhostText => Rgba8([90, 90, 90, 90]),
            FileRole::DimmedText => Rgba8([120, 120, 120, 130]),
            FileRole::DeleteChangedChars => Rgba8([255, 0, 0, 60]),
            FileRole::InsertChangedChars => Rgba8([0, 190, 0, 60]),
            FileRole::DeleteChangedTokens => Rgba8([255, 0, 0, 35]),
            FileRole::InsertChangedTokens => Rgba8([0, 190, 0, 35]),
            FileRole::DeleteChangedScope => Rgba8([255, 0, 0, 12]),
            FileRole::InsertChangedScope => Rgba8([0, 190, 0, 12]),
            FileRole::GotoLineNumber => Rgba8([190, 120, 0, 255]),
            FileRole::GotoRow => Rgba8([255, 200, 0, 60]),
            FileRole::Find => Rgba8([255, 190, 0, 110]),
            FileRole::FindCurrent => Rgba8([255, 150, 0, 255]),
            FileRole::Occurrence => Rgba8([170, 110, 240, 80]),
            FileRole::VoidRow => Rgba8([225, 225, 225, 255]),
            FileRole::CollapsedRow => Rgba8([216, 222, 232, 255]),
            FileRole::TableBackground => Rgba8([248, 248, 248, 255]),
            FileRole::LineNumber => Rgba8([150, 150, 150, 255]),
            FileRole::HexDifferent => Rgba8([255, 80, 80, 80]),
            FileRole::HexOnlyHere => Rgba8([0, 190, 0, 70]),
            FileRole::HexGoto => Rgba8([190, 120, 0, 255]),
            FileRole::HexByteNull => Rgba8([130, 130, 130, 255]),
            FileRole::HexBytePrintable => Rgba8([0, 110, 160, 255]),
            FileRole::HexByteControl => Rgba8([80, 70, 200, 255]),
            FileRole::HexByteHigh => Rgba8([170, 40, 150, 255]),
            FileRole::HexByteFull => Rgba8([20, 20, 20, 255]),
            FileRole::ImageDiff => Rgba8([255, 0, 255, 255]),
            FileRole::RulerDeleted => Rgba8([210, 40, 40, 255]),
            FileRole::RulerInserted => Rgba8([30, 160, 30, 255]),
            FileRole::RulerChanged => Rgba8([200, 150, 0, 255]),
        }
    }

    fn has_alpha(self) -> bool {
        matches!(
            self,
            FileRole::GhostText
                | FileRole::DimmedText
                | FileRole::DeleteChangedChars
                | FileRole::InsertChangedChars
                | FileRole::DeleteChangedTokens
                | FileRole::InsertChangedTokens
                | FileRole::DeleteChangedScope
                | FileRole::InsertChangedScope
                | FileRole::GotoRow
                | FileRole::Find
                | FileRole::FindCurrent
                | FileRole::Occurrence
                | FileRole::HexDifferent
                | FileRole::HexOnlyHere
        )
    }
}

#[cfg(test)]
mod tests {
    use zcommon::style::{Role, SharedRole, SyntaxCategory};

    use super::FileRole;

    #[test]
    fn file_role_names_are_unique_against_the_shared_roles() {
        let mut names: Vec<&str> = SharedRole::all().iter().map(|r| r.name()).collect();
        names.extend(FileRole::all().iter().map(|r| r.name()));
        names.extend(SyntaxCategory::all().iter().map(|r| r.name()));
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }
}
