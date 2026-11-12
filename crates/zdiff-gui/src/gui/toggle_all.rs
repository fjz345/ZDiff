use zdiff::diff_builder::DiffBuilderOptions;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionSet {
    pub whitespace: bool,
    pub comments: bool,
    pub regex_enabled: bool,
    pub line_endings: bool,
    pub file_markers: bool,
    pub highlight_rows: bool,
    pub ghost_rows: bool,
    pub word_wrap: bool,
    pub diff_only_extra_rows: Option<usize>,
}

impl OptionSet {
    pub const ALL_OFF: Self = Self {
        whitespace: false,
        comments: false,
        regex_enabled: false,
        line_endings: false,
        file_markers: false,
        highlight_rows: false,
        ghost_rows: false,
        word_wrap: false,
        diff_only_extra_rows: None,
    };

    pub fn read(options: &DiffBuilderOptions, word_wrap: bool) -> Self {
        Self {
            whitespace: options.ignore.whitespace,
            comments: options.ignore.comments,
            regex_enabled: options.ignore.regex_enabled,
            line_endings: options.ignore.line_endings,
            file_markers: options.ignore.file_markers,
            highlight_rows: options.highlight_rows,
            ghost_rows: options.ghost_rows,
            word_wrap,
            diff_only_extra_rows: options.diff_only_with_extra_rows,
        }
    }

    pub fn apply(self, options: &mut DiffBuilderOptions, word_wrap: &mut bool) {
        options.ignore.whitespace = self.whitespace;
        options.ignore.comments = self.comments;
        options.ignore.regex_enabled = self.regex_enabled;
        options.ignore.line_endings = self.line_endings;
        options.ignore.file_markers = self.file_markers;
        options.highlight_rows = self.highlight_rows;
        options.ghost_rows = self.ghost_rows;
        *word_wrap = self.word_wrap;
        options.diff_only_with_extra_rows = self.diff_only_extra_rows;
    }
}

#[derive(Debug, Default)]
pub struct ToggleAll {
    snapshot: Option<OptionSet>,
}

impl ToggleAll {
    pub fn is_held(&self) -> bool {
        self.snapshot.is_some()
    }

    pub fn press(&mut self, current: OptionSet) -> OptionSet {
        match self.snapshot.take() {
            Some(snapshot) => snapshot,
            None => {
                self.snapshot = Some(current);
                OptionSet::ALL_OFF
            }
        }
    }

    pub fn observe(&mut self, current: OptionSet) {
        if current != OptionSet::ALL_OFF {
            self.snapshot = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zdiff::ignore::IgnorePatterns;

    fn customized() -> OptionSet {
        OptionSet {
            whitespace: true,
            comments: false,
            regex_enabled: true,
            line_endings: true,
            file_markers: false,
            highlight_rows: true,
            ghost_rows: false,
            word_wrap: true,
            diff_only_extra_rows: Some(5),
        }
    }

    #[test]
    fn pressing_twice_restores_the_exact_set() {
        let mut toggle = ToggleAll::default();
        let before = customized();

        assert_eq!(toggle.press(before), OptionSet::ALL_OFF);
        assert!(toggle.is_held());
        assert_eq!(toggle.press(OptionSet::ALL_OFF), before);
        assert!(!toggle.is_held());
    }

    #[test]
    fn a_manual_change_while_held_drops_the_snapshot() {
        let mut toggle = ToggleAll::default();
        toggle.press(customized());

        let changed = OptionSet {
            word_wrap: true,
            ..OptionSet::ALL_OFF
        };
        toggle.observe(changed);
        assert!(!toggle.is_held());

        assert_eq!(toggle.press(changed), OptionSet::ALL_OFF);
        assert_eq!(toggle.press(OptionSet::ALL_OFF), changed);
    }

    #[test]
    fn observing_the_applied_set_keeps_the_snapshot() {
        let mut toggle = ToggleAll::default();
        toggle.press(customized());
        toggle.observe(OptionSet::ALL_OFF);
        assert!(toggle.is_held());
    }

    #[test]
    fn observing_without_a_snapshot_changes_nothing() {
        let mut toggle = ToggleAll::default();
        toggle.observe(customized());
        assert!(!toggle.is_held());
    }

    #[test]
    fn all_off_with_nothing_to_switch_off_still_holds_a_snapshot() {
        let mut toggle = ToggleAll::default();
        assert_eq!(toggle.press(OptionSet::ALL_OFF), OptionSet::ALL_OFF);
        assert!(toggle.is_held());
    }

    #[test]
    fn read_and_apply_cover_every_option_and_leave_the_patterns() {
        let mut options = DiffBuilderOptions::default();
        options.ignore.whitespace = true;
        options.ignore.comments = true;
        options.ignore.regex_enabled = true;
        options.ignore.patterns = IgnorePatterns::new("foo\\d+");
        options.ignore.line_endings = true;
        options.ignore.file_markers = true;
        options.highlight_rows = true;
        options.ghost_rows = true;
        options.diff_only_with_extra_rows = Some(7);
        let mut wrap = true;

        let before = OptionSet::read(&options, wrap);
        assert_eq!(
            before,
            OptionSet {
                whitespace: true,
                comments: true,
                regex_enabled: true,
                line_endings: true,
                file_markers: true,
                highlight_rows: true,
                ghost_rows: true,
                word_wrap: true,
                diff_only_extra_rows: Some(7),
            }
        );

        let mut toggle = ToggleAll::default();
        toggle.press(before).apply(&mut options, &mut wrap);
        assert_eq!(OptionSet::read(&options, wrap), OptionSet::ALL_OFF);
        assert_eq!(options.ignore.patterns, IgnorePatterns::new("foo\\d+"));

        toggle
            .press(OptionSet::read(&options, wrap))
            .apply(&mut options, &mut wrap);
        assert_eq!(OptionSet::read(&options, wrap), before);
        assert_eq!(options.ignore.patterns, IgnorePatterns::new("foo\\d+"));
    }
}
