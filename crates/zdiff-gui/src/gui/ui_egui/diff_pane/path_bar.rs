use super::*;

pub(super) fn handle_drops(
    ui: &egui::Ui,
    load_file_1_request: &mut Option<UniversalPath>,
    load_file_2_request: &mut Option<UniversalPath>,
    rect1: egui::Rect,
    rect2: egui::Rect,
) -> bool {
    // assert!(
    //     load_file_1_request.is_none() && load_file_2_request.is_none(),
    //     "File load requests should be None when handling drops"
    // );

    // let mut should_draw = false;
    let did_drop = match zcommon_ui::common::dropped_file(ui.ctx()) {
        Some((path, drop_pos)) if rect1.contains(drop_pos) => {
            log::info!("File dropped on left pane: {}", path.display());
            *load_file_1_request = Some(UniversalPath::from(path));
            true
        }
        Some((path, drop_pos)) if rect2.contains(drop_pos) => {
            log::info!("File dropped on right pane: {}", path.display());
            *load_file_2_request = Some(UniversalPath::from(path));
            true
        }
        _ => false,
    };
    // if should_draw {
    //     ui.painter()
    //         .debug_rect(rect1, egui::Color32::RED, "Left Drop Zone");
    //     ui.painter()
    //         .debug_rect(rect2, egui::Color32::BLUE, "Right Drop Zone");
    // }

    return did_drop;
}

pub(super) fn drop_rects(
    side_rects: [egui::Rect; 2],
    pane: egui::Rect,
    shown: [usize; 2],
) -> [egui::Rect; 2] {
    if side_rects.iter().any(egui::Rect::is_positive) {
        return side_rects;
    }
    let (left, right) = pane.split_left_right_at_fraction(0.5);
    shown.map(|column| [left, right][column])
}

pub(super) fn file_path_segments(path: &UniversalPath, label: Option<&str>) -> Vec<PathSegment> {
    match label {
        Some(label) => vec![PathSegment {
            name: label.to_owned(),
            path: path.to_string(),
        }],
        None => split_path(&path.to_string()),
    }
}

pub(super) fn bar_segments(
    path: &UniversalPath,
    label: Option<&str>,
    placeholder: &str,
) -> Vec<PathSegment> {
    if path.is_empty() && label.is_none() {
        return vec![PathSegment {
            name: placeholder.to_owned(),
            path: String::new(),
        }];
    }
    file_path_segments(path, label)
}

pub(super) fn load_error_text(errors: [Option<&str>; 2]) -> Option<String> {
    let lines: Vec<String> = ["Left", "Right"]
        .into_iter()
        .zip(errors)
        .filter_map(|(side, error)| error.map(|error| format!("{side}: {error}")))
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

pub(super) fn file_path_dim([a, b]: [&UniversalPath; 2], labelled: bool) -> usize {
    if labelled {
        return 0;
    }
    common_parent(&a.to_string(), true, &b.to_string(), true)
}

pub(super) fn file_path_request(
    text: &str,
    current: &UniversalPath,
    root: Option<&UniversalPath>,
) -> Option<UniversalPath> {
    let text = text.trim().trim_matches('"');
    if text == current.to_string() {
        return None;
    }
    let path = UniversalPath::from(text);
    // set_path appends a depot path to a depot root, so send it root-relative like the old field.
    let relative = root
        .filter(|root| root.is_depot() && path.is_depot())
        .and_then(|root| FileProcessor::strip_root_prefix(root, &path));
    Some(relative.map_or(path, |relative| UniversalPath::from(relative.to_string())))
}

pub(super) fn root_path_request(
    action: PathBarAction,
    root: &UniversalPath,
) -> Option<UniversalPath> {
    let text = match action {
        PathBarAction::Segment(i) => file_path_segments(root, None)[i].path.clone(),
        PathBarAction::Commit(text) => text.trim().trim_matches('"').to_owned(),
    };
    (text != root.to_string()).then(|| UniversalPath::from(text.as_str()))
}

pub(super) fn browser_pick(
    side: ActiveSide,
    picked: &str,
    paths: [&UniversalPath; 2],
    roots: [Option<&UniversalPath>; 2],
) -> [Option<UniversalPath>; 2] {
    let i = h_scroll::side_index(side);
    let mut requests = [None, None];
    requests[i] = file_path_request(picked, paths[i], roots[i]);
    requests
}

#[cfg(test)]
mod tests {
    mod path_bar {
        use zcommon::path::PathSegment;
        use zdiff::universal_path::UniversalPath;

        use zcommon_ui::pathbar::PathBarAction;

        use super::super::{
            browser_pick, file_path_dim, file_path_request, file_path_segments, root_path_request,
        };
        use crate::file::FileProcessor;
        use crate::ui_egui::active_side::ActiveSide;

        fn committed(root: Option<&str>, current: &str, text: &str) -> UniversalPath {
            let mut file = FileProcessor::new();
            let root = root.map(UniversalPath::from);
            if let Some(root) = &root {
                file.set_root(root.clone());
            }
            file.set_path(UniversalPath::from(current));
            let current = file.get_full_path();
            let request = file_path_request(text, &current, root.as_ref()).unwrap();
            file.set_path(request);
            file.get_full_path()
        }

        #[test]
        fn segments_cover_the_full_path() {
            let names: Vec<String> =
                file_path_segments(&UniversalPath::from(r"C:\ws\src\main.rs"), None)
                    .into_iter()
                    .map(|s| s.name)
                    .collect();
            assert_eq!(names, [r"C:\", "ws", "src", "main.rs"]);
        }

        #[test]
        fn a_label_is_one_segment_standing_for_the_full_path() {
            let path = UniversalPath::from(r"C:\Temp\p4v\main.rs#3");
            assert_eq!(
                file_path_segments(&path, Some("main.rs#3 (depot)")),
                [PathSegment {
                    name: "main.rs#3 (depot)".to_owned(),
                    path: path.to_string(),
                }]
            );
        }

        #[test]
        fn dim_follows_either_sides_path() {
            let a = UniversalPath::from(r"C:\ws\src\main.rs");
            let b = UniversalPath::from(r"C:\ws\src\lib.rs");
            let c = UniversalPath::from(r"C:\ws\docs\readme.md");
            let d = UniversalPath::from(r"D:\ws\src\main.rs");
            assert_eq!(file_path_dim([&a, &b], false), 3);
            assert_eq!(file_path_dim([&a, &c], false), 2);
            assert_eq!(file_path_dim([&c, &b], false), 2);
            assert_eq!(file_path_dim([&d, &b], false), 0);
            assert_eq!(
                file_path_dim(
                    [
                        &UniversalPath::from("//depot/main/a.rs#3"),
                        &UniversalPath::from("//depot/main/b.rs")
                    ],
                    false
                ),
                2
            );
        }

        #[test]
        fn nothing_is_dimmed_with_a_label() {
            let a = UniversalPath::from(r"C:\ws\src\main.rs");
            assert_eq!(file_path_dim([&a, &a], true), 0);
        }

        #[test]
        fn unchanged_text_is_not_a_request() {
            let path = UniversalPath::from(r"C:\ws\src\main.rs");
            assert_eq!(file_path_request(r"C:\ws\src\main.rs", &path, None), None);
            let depot = UniversalPath::from("//depot/main/a.rs#3");
            assert_eq!(
                file_path_request(" //depot/main/a.rs#3 ", &depot, None),
                None
            );
        }

        #[test]
        fn an_unchanged_root_is_not_a_request() {
            let root = UniversalPath::from(r"C:\ws\src");
            let commit = PathBarAction::Commit(r#" "C:\ws\src" "#.to_owned());
            assert_eq!(root_path_request(commit, &root), None);
            let last = file_path_segments(&root, None).len() - 1;
            assert_eq!(root_path_request(PathBarAction::Segment(last), &root), None);
        }

        #[test]
        fn a_committed_root_is_trimmed_and_unquoted() {
            let root = UniversalPath::from(r"C:\ws\src");
            let commit = PathBarAction::Commit(r#" "C:\other dir" "#.to_owned());
            assert_eq!(
                root_path_request(commit, &root),
                Some(UniversalPath::from(r"C:\other dir"))
            );
        }

        #[test]
        fn a_clicked_root_segment_sets_the_root_to_that_folder() {
            let root = UniversalPath::from("//depot/main/src");
            assert_eq!(
                root_path_request(PathBarAction::Segment(1), &root),
                Some(UniversalPath::from("//depot/main"))
            );
        }

        #[test]
        fn committed_full_path_without_a_root_loads_it() {
            assert_eq!(
                committed(None, r"C:\ws\a.rs", r#""C:\other dir\b.rs" "#),
                UniversalPath::from(r"C:\other dir\b.rs")
            );
            assert_eq!(
                committed(None, r"C:\ws\a.rs", "//depot/main/b.rs#2"),
                UniversalPath::from("//depot/main/b.rs#2")
            );
        }

        #[test]
        fn committed_full_path_under_a_local_root_loads_it() {
            assert_eq!(
                committed(Some(r"C:\ws"), r"C:\ws\src\a.rs", r"C:\ws\src\b.rs"),
                UniversalPath::from(r"C:\ws\src\b.rs")
            );
            assert_eq!(
                committed(Some(r"C:\ws"), r"C:\ws\src\a.rs", r"C:\other\b.rs"),
                UniversalPath::from(r"C:\other\b.rs")
            );
        }

        #[test]
        fn committed_full_path_under_a_depot_root_loads_it() {
            assert_eq!(
                committed(
                    Some("//depot/main"),
                    "//depot/main/src/a.rs",
                    "//depot/main/src/b.rs"
                ),
                UniversalPath::from("//depot/main/src/b.rs")
            );
            assert_eq!(
                committed(
                    Some("//depot/main"),
                    "//depot/main/src/a.rs",
                    "//depot/main/src/b.rs#4"
                )
                .to_string(),
                "//depot/main/src/b.rs#4"
            );
            assert_eq!(
                committed(
                    Some("//depot/main"),
                    "//depot/main/src/a.rs",
                    "//depot/dev/src/a.rs"
                ),
                UniversalPath::from("//depot/dev/src/a.rs")
            );
        }

        #[test]
        fn a_picked_file_loads_into_that_side_only() {
            let left = UniversalPath::from(r"C:\ws\src\a.rs");
            let right = UniversalPath::from(r"C:\ws\src\b.rs");
            assert_eq!(
                browser_pick(
                    ActiveSide::Left,
                    r"C:\ws\c.rs",
                    [&left, &right],
                    [None, None]
                ),
                [Some(UniversalPath::from(r"C:\ws\c.rs")), None]
            );
            assert_eq!(
                browser_pick(
                    ActiveSide::Right,
                    r"C:\ws\c.rs",
                    [&left, &right],
                    [None, None]
                ),
                [None, Some(UniversalPath::from(r"C:\ws\c.rs"))]
            );
        }

        #[test]
        fn picking_the_loaded_file_is_no_request() {
            let left = UniversalPath::from(r"C:\ws\src\a.rs");
            let right = UniversalPath::from(r"C:\ws\src\b.rs");
            assert_eq!(
                browser_pick(
                    ActiveSide::Right,
                    r"C:\ws\src\b.rs",
                    [&left, &right],
                    [None, None]
                ),
                [None, None]
            );
        }

        #[test]
        fn a_picked_file_is_the_same_request_as_typing_it() {
            let root = UniversalPath::from("//depot/main");
            let left = UniversalPath::from("//depot/main/src/a.rs");
            let right = UniversalPath::from(r"C:\ws\b.rs");
            let picked = "//depot/main/src/c.rs";
            assert_eq!(
                browser_pick(
                    ActiveSide::Left,
                    picked,
                    [&left, &right],
                    [Some(&root), None]
                ),
                [file_path_request(picked, &left, Some(&root)), None]
            );
        }

        #[test]
        fn a_picked_depot_file_opens_at_head() {
            let left = UniversalPath::from("//depot/main/a.rs#3");
            let right = UniversalPath::from(r"C:\ws\b.rs");
            for picked in ["//depot/main/a.rs", "//depot/main/c.rs"] {
                assert_eq!(
                    browser_pick(ActiveSide::Left, picked, [&left, &right], [None, None]),
                    [Some(UniversalPath::Depot(picked.into(), None)), None]
                );
            }
        }

        #[test]
        fn a_pane_without_rows_takes_a_drop_for_the_file_its_half_shows() {
            use super::super::drop_rects;
            use crate::ui_egui::active_side::shown_sides;
            use eframe::egui::{Rect, pos2};

            let pane = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 50.0));
            let (left_half, right_half) = (pos2(25.0, 25.0), pos2(75.0, 25.0));
            let no_rows = [Rect::NOTHING; 2];

            let [file_1, file_2] = drop_rects(no_rows, pane, shown_sides(false));
            assert!(file_1.contains(left_half) && file_2.contains(right_half));
            let [file_1, file_2] = drop_rects(no_rows, pane, shown_sides(true));
            assert!(file_2.contains(left_half) && file_1.contains(right_half));
            assert!(!file_1.contains(left_half) && !file_2.contains(right_half));

            let rows = [
                Rect::from_min_max(pos2(60.0, 0.0), pos2(100.0, 50.0)),
                Rect::NOTHING,
            ];
            assert_eq!(drop_rects(rows, pane, shown_sides(true)), rows);
        }

        #[test]
        fn load_errors_are_named_by_their_file_like_the_open_buttons() {
            use super::super::load_error_text;

            assert_eq!(load_error_text([None, None]), None);
            assert_eq!(
                load_error_text([Some("gone"), Some("denied")]).as_deref(),
                Some("Left: gone\nRight: denied")
            );
            assert_eq!(
                load_error_text([None, Some("denied")]).as_deref(),
                Some("Right: denied")
            );
        }
    }

}
