use super::state::AppStateCtx;

use zdiff::diff_builder::DiffBuilderOptions;

use crate::{
    cli::{self, LaunchRequest},
    diff_ctx::DiffProcessor,
    file::FileProcessor,
    keybindings::FileAction,
    toggle_all::{OptionSet, ToggleAll},
    ui_egui::merge_pane::{self, MergeNames, MergeRequest},
};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum LaunchOutcome {
    Opened,
    Merge(Box<MergeRequest>, MergeNames),
    Help,
    Error(String),
}

pub(super) fn apply_launch(
    file_1: &mut FileProcessor,
    file_2: &mut FileProcessor,
    launch: Result<cli::Launch, cli::LaunchError>,
) -> LaunchOutcome {
    let launch = match launch {
        Ok(launch) => launch,
        Err(err) => {
            log::error!("{err}");
            return LaunchOutcome::Error(err.to_string());
        }
    };
    for warning in &launch.warnings {
        log::warn!("{warning}");
    }
    let (left, right) = match launch.request {
        LaunchRequest::Diff { left, right } => (left, right),
        LaunchRequest::Merge {
            base,
            left,
            right,
            output,
        } => {
            if let Err(err) = merge_pane::check_output(&output.path) {
                log::error!("{err}");
                return LaunchOutcome::Error(err);
            }
            let request = MergeRequest {
                base: Some(base.path),
                theirs: left.path,
                yours: right.path,
                output: output.path,
            };
            let names = MergeNames {
                base: base.label,
                theirs: left.label,
                yours: right.label,
                output: output.label,
            };
            return LaunchOutcome::Merge(Box::new(request), names);
        }
        LaunchRequest::Help => return LaunchOutcome::Help,
    };
    // set_path clears the label, so the label goes second.
    for (file, side) in [(file_1, left), (file_2, right)] {
        file.set_path(side.path);
        file.set_label(side.label);
    }
    LaunchOutcome::Opened
}

pub(super) fn swap_sides(
    file_1: &mut FileProcessor,
    file_2: &mut FileProcessor,
    scroll_left: &mut f32,
    scroll_right: &mut f32,
    diff_processor: &mut DiffProcessor,
) {
    std::mem::swap(file_1, file_2);
    std::mem::swap(scroll_left, scroll_right);

    diff_processor.reset_ctx();
}

pub(super) fn toggle_all_options(
    toggle_all: &mut ToggleAll,
    options: &mut DiffBuilderOptions,
    h_scroll_linked: &mut bool,
    word_wrap: &mut bool,
) {
    let current = OptionSet::read(options, *h_scroll_linked, *word_wrap);
    toggle_all
        .press(current)
        .apply(options, h_scroll_linked, word_wrap);
}

// Mirrors the toolbar buttons in diff_pane.
pub(super) fn toggle_option(ctx: &mut AppStateCtx, action: FileAction) {
    let options = &mut ctx.diff_options;
    let flip = |b: &mut bool| *b = !*b;
    match action {
        FileAction::IgnoreWhitespace => flip(&mut options.ignore.whitespace),
        FileAction::IgnoreComments => flip(&mut options.ignore.comments),
        FileAction::IgnoreRegex => flip(&mut options.ignore.regex_enabled),
        FileAction::HighlightRows => flip(&mut options.highlight_rows),
        FileAction::GhostRows => flip(&mut options.ghost_rows),
        FileAction::LinkHScroll => flip(&mut ctx.h_scroll_linked),
        FileAction::WordWrap => flip(&mut ctx.word_wrap),
        FileAction::DiffOnly => {
            options.diff_only_with_extra_rows = match options.diff_only_with_extra_rows {
                Some(_) => None,
                None => Some(2),
            }
        }
        FileAction::FewerContextRows => {
            if let Some(rows) = &mut options.diff_only_with_extra_rows {
                *rows = rows.saturating_sub(1);
            }
        }
        FileAction::MoreContextRows => {
            if let Some(rows) = &mut options.diff_only_with_extra_rows {
                *rows = (*rows + 1).min(99);
            }
        }
        other => panic!("{other:?} is not an option toggle"),
    }
}

#[cfg(all(test, feature = "serde"))]
mod tests {
    use super::*;
    use zdiff::{ignore::IgnorePatterns, universal_path::UniversalPath};

    #[test]
    fn swap_sides_swaps_the_files_and_their_scroll() {
        let mut ctx = AppStateCtx::default();
        let (a, b) = (UniversalPath::from("a.txt"), UniversalPath::from("b.txt"));
        ctx.file_1.set_path(a.clone());
        ctx.file_2.set_path(b.clone());
        ctx.scroll_left = 1.0;
        ctx.scroll_right = 2.0;
        ctx.diff_processor.conflict_cursor.set(3);
        swap_sides(
            &mut ctx.file_1,
            &mut ctx.file_2,
            &mut ctx.scroll_left,
            &mut ctx.scroll_right,
            &mut ctx.diff_processor,
        );
        assert_eq!(ctx.file_1.get_full_path(), b);
        assert_eq!(ctx.file_2.get_full_path(), a);
        assert_eq!((ctx.scroll_left, ctx.scroll_right), (2.0, 1.0));
        assert_eq!(ctx.diff_processor.conflict_cursor.get(), 0);
    }

    #[test]
    fn each_toggle_action_flips_its_option_and_back() {
        type Get = fn(&AppStateCtx) -> bool;
        let toggles: [(FileAction, Get); 8] = [
            (FileAction::IgnoreWhitespace, |c| {
                c.diff_options.ignore.whitespace
            }),
            (FileAction::IgnoreComments, |c| {
                c.diff_options.ignore.comments
            }),
            (FileAction::IgnoreRegex, |c| {
                c.diff_options.ignore.regex_enabled
            }),
            (FileAction::HighlightRows, |c| c.diff_options.highlight_rows),
            (FileAction::GhostRows, |c| c.diff_options.ghost_rows),
            (FileAction::LinkHScroll, |c| c.h_scroll_linked),
            (FileAction::WordWrap, |c| c.word_wrap),
            (FileAction::DiffOnly, |c| {
                c.diff_options.diff_only_with_extra_rows.is_some()
            }),
        ];
        for (action, get) in toggles {
            let mut ctx = AppStateCtx::default();
            let before = AppStateCtx::default();
            toggle_option(&mut ctx, action);
            assert_eq!(get(&ctx), !get(&before), "{action:?}");
            toggle_option(&mut ctx, action);
            assert_eq!(get(&ctx), get(&before), "{action:?}");
            assert_eq!(ctx.diff_options, before.diff_options, "{action:?}");
        }
    }

    #[test]
    fn regex_off_keeps_the_patterns() {
        let mut ctx = AppStateCtx::default();
        ctx.diff_options.ignore.patterns = IgnorePatterns::new(r"\d+");
        toggle_option(&mut ctx, FileAction::IgnoreRegex);
        assert!(!ctx.diff_options.ignore.regex_enabled);
        assert_eq!(ctx.diff_options.ignore.patterns.text(), r"\d+");
    }

    #[test]
    fn diff_only_turns_on_with_two_context_rows() {
        let mut ctx = AppStateCtx::default();
        toggle_option(&mut ctx, FileAction::DiffOnly);
        assert_eq!(ctx.diff_options.diff_only_with_extra_rows, Some(2));
        ctx.diff_options.diff_only_with_extra_rows = Some(5);
        toggle_option(&mut ctx, FileAction::DiffOnly);
        assert_eq!(ctx.diff_options.diff_only_with_extra_rows, None);
    }

    #[test]
    fn context_rows_step_between_0_and_99_in_diff_only() {
        let rows = |start: Option<usize>, action| {
            let mut ctx = AppStateCtx::default();
            ctx.diff_options.diff_only_with_extra_rows = start;
            toggle_option(&mut ctx, action);
            ctx.diff_options.diff_only_with_extra_rows
        };
        assert_eq!(rows(Some(2), FileAction::FewerContextRows), Some(1));
        assert_eq!(rows(Some(0), FileAction::FewerContextRows), Some(0));
        assert_eq!(rows(Some(2), FileAction::MoreContextRows), Some(3));
        assert_eq!(rows(Some(99), FileAction::MoreContextRows), Some(99));
        assert_eq!(rows(None, FileAction::FewerContextRows), None);
        assert_eq!(rows(None, FileAction::MoreContextRows), None);
    }

    mod launch {
        use super::*;

        fn launch(args: &[&str]) -> (FileProcessor, FileProcessor, LaunchOutcome) {
            let mut left = FileProcessor::new();
            let mut right = FileProcessor::new();
            let outcome = apply_launch(
                &mut left,
                &mut right,
                cli::parse_launch_args(args.iter().copied()),
            );
            (left, right, outcome)
        }

        #[test]
        fn diff_sets_paths_and_labels() {
            let (mut left, mut right, outcome) = launch(&["-nl", "A", "-nr", "B", "x", "y"]);
            assert_eq!(outcome, LaunchOutcome::Opened);
            assert_eq!(left.get_full_path(), UniversalPath::new("x"));
            assert_eq!(right.get_full_path(), UniversalPath::new("y"));
            assert_eq!(left.label(), Some("A"));
            assert_eq!(right.label(), Some("B"));
        }

        #[test]
        fn help_opens_nothing() {
            let (mut left, _, outcome) = launch(&["--help"]);
            assert_eq!(outcome, LaunchOutcome::Help);
            assert_eq!(left.get_full_path(), UniversalPath::default());
        }

        #[test]
        fn error_is_returned_and_opens_nothing() {
            let (mut left, _, outcome) = launch(&["a", "b", "c"]);
            assert_eq!(
                outcome,
                LaunchOutcome::Error(cli::LaunchError::PositionalCount(3).to_string())
            );
            assert_eq!(left.get_full_path(), UniversalPath::default());
        }

        #[test]
        fn merge_opens_a_merge_with_labels_and_leaves_the_diff_sides() {
            let (mut left, mut right, outcome) = launch(&[
                "-nb", "B", "-nl", "T", "-nr", "Y", "-nm", "R", "b", "t", "y", "o",
            ]);
            assert_eq!(
                outcome,
                LaunchOutcome::Merge(
                    Box::new(MergeRequest {
                        base: Some(UniversalPath::new("b")),
                        theirs: UniversalPath::new("t"),
                        yours: UniversalPath::new("y"),
                        output: UniversalPath::new("o"),
                    }),
                    MergeNames {
                        base: Some("B".into()),
                        theirs: Some("T".into()),
                        yours: Some("Y".into()),
                        output: Some("R".into()),
                    }
                )
            );
            assert_eq!(left.get_full_path(), UniversalPath::default());
            assert_eq!(right.get_full_path(), UniversalPath::default());
        }

        #[test]
        fn merge_into_a_depot_output_is_an_error() {
            let (_, _, outcome) = launch(&["b", "t", "y", "//depot/o.txt"]);
            assert_eq!(
                outcome,
                LaunchOutcome::Error("Output must be a local path".to_string())
            );
        }

        #[test]
        fn merge_into_a_git_revision_output_is_an_error() {
            let (_, _, outcome) = launch(&["b", "t", "y", r"C:\repo\o.txt@git:main"]);
            assert_eq!(
                outcome,
                LaunchOutcome::Error("Output must be a local path".to_string())
            );
        }
    }
}
