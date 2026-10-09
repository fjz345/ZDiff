use zcommon::style::{Rgba8, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRole {
    Modified,
    GhostText,
    DimmedText,
    DeleteChangedChars,
    InsertChangedChars,
    GotoLineNumber,
    GotoRow,
    Find,
    FindCurrent,
    Occurrence,
    VoidRow,
    TableBackground,
    LineNumber,
}

impl Role for FileRole {
    fn all() -> &'static [Self] {
        &[
            FileRole::Modified,
            FileRole::GhostText,
            FileRole::DimmedText,
            FileRole::DeleteChangedChars,
            FileRole::InsertChangedChars,
            FileRole::GotoLineNumber,
            FileRole::GotoRow,
            FileRole::Find,
            FileRole::FindCurrent,
            FileRole::Occurrence,
            FileRole::VoidRow,
            FileRole::TableBackground,
            FileRole::LineNumber,
        ]
    }

    fn name(self) -> &'static str {
        match self {
            FileRole::Modified => "file_modified",
            FileRole::GhostText => "file_ghost_text",
            FileRole::DimmedText => "file_dimmed_text",
            FileRole::DeleteChangedChars => "file_delete_changed_chars",
            FileRole::InsertChangedChars => "file_insert_changed_chars",
            FileRole::GotoLineNumber => "file_goto_line_number",
            FileRole::GotoRow => "file_goto_row",
            FileRole::Find => "file_find",
            FileRole::FindCurrent => "file_find_current",
            FileRole::Occurrence => "file_occurrence",
            FileRole::VoidRow => "file_void_row",
            FileRole::TableBackground => "file_table_background",
            FileRole::LineNumber => "file_line_number",
        }
    }

    fn label(self) -> &'static str {
        match self {
            FileRole::Modified => "Modified marker",
            FileRole::GhostText => "Ghost text",
            FileRole::DimmedText => "Ignored text",
            FileRole::DeleteChangedChars => "Delete changed chars",
            FileRole::InsertChangedChars => "Insert changed chars",
            FileRole::GotoLineNumber => "Goto line number",
            FileRole::GotoRow => "Goto row",
            FileRole::Find => "Find match",
            FileRole::FindCurrent => "Current find match",
            FileRole::Occurrence => "Selection occurrence",
            FileRole::VoidRow => "Void row",
            FileRole::TableBackground => "Table background",
            FileRole::LineNumber => "Line numbers",
        }
    }

    fn dark(self) -> Rgba8 {
        match self {
            FileRole::Modified => Rgba8([255, 255, 100, 255]),
            FileRole::GhostText => Rgba8([150, 150, 150, 80]),
            FileRole::DimmedText => Rgba8([128, 128, 128, 110]),
            FileRole::DeleteChangedChars => Rgba8([255, 0, 0, 80]),
            FileRole::InsertChangedChars => Rgba8([0, 255, 0, 70]),
            FileRole::GotoLineNumber => Rgba8([255, 210, 0, 255]),
            FileRole::GotoRow => Rgba8([255, 255, 0, 40]),
            FileRole::Find => Rgba8([164, 120, 0, 140]),
            FileRole::FindCurrent => Rgba8([176, 104, 0, 255]),
            // Purple: apart from find amber, change red and green, and egui's blue selection.
            FileRole::Occurrence => Rgba8([153, 93, 223, 110]),
            FileRole::VoidRow => Rgba8([30, 30, 30, 255]),
            FileRole::TableBackground => Rgba8([15, 15, 15, 255]),
            FileRole::LineNumber => Rgba8([96, 96, 96, 255]),
        }
    }

    fn light(self) -> Rgba8 {
        match self {
            FileRole::Modified => Rgba8([170, 130, 0, 255]),
            FileRole::GhostText => Rgba8([90, 90, 90, 90]),
            FileRole::DimmedText => Rgba8([120, 120, 120, 130]),
            FileRole::DeleteChangedChars => Rgba8([255, 0, 0, 60]),
            FileRole::InsertChangedChars => Rgba8([0, 190, 0, 60]),
            FileRole::GotoLineNumber => Rgba8([190, 120, 0, 255]),
            FileRole::GotoRow => Rgba8([255, 200, 0, 60]),
            FileRole::Find => Rgba8([255, 190, 0, 110]),
            FileRole::FindCurrent => Rgba8([255, 150, 0, 255]),
            FileRole::Occurrence => Rgba8([170, 110, 240, 80]),
            FileRole::VoidRow => Rgba8([225, 225, 225, 255]),
            FileRole::TableBackground => Rgba8([248, 248, 248, 255]),
            FileRole::LineNumber => Rgba8([150, 150, 150, 255]),
        }
    }

    fn has_alpha(self) -> bool {
        matches!(
            self,
            FileRole::GhostText
                | FileRole::DimmedText
                | FileRole::DeleteChangedChars
                | FileRole::InsertChangedChars
                | FileRole::GotoRow
                | FileRole::Find
                | FileRole::Occurrence
        )
    }
}

#[cfg(test)]
mod tests {
    use zcommon::style::{Role, SharedRole};

    use super::FileRole;

    #[test]
    fn file_role_names_are_unique_against_the_shared_roles() {
        let mut names: Vec<&str> = SharedRole::all().iter().map(|r| r.name()).collect();
        names.extend(FileRole::all().iter().map(|r| r.name()));
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }
}
