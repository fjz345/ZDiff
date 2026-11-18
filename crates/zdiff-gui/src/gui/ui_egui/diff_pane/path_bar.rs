use super::*;

const DROP_ZONE_INSET: f32 = 8.0;
const DROP_ZONE_DASH: f32 = 6.0;
const DROP_ZONE_GAP: f32 = 4.0;
const MIN_STATE_WIDTH: f32 = 120.0;

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

    did_drop
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

pub(super) fn welcome_drop_rects(
    side_rects: [egui::Rect; 2],
    pane: egui::Rect,
    shown: [usize; 2],
) -> [egui::Rect; 2] {
    let [left, right] = shown.map(|side| side_rects[side]);
    let divider = (left.max.x + right.min.x) / 2.0;
    if !divider.is_finite() {
        return drop_rects([egui::Rect::NOTHING; 2], pane, shown);
    }
    let (left, right) = pane.split_left_right_at_x(divider);
    shown.map(|column| [left, right][column])
}

pub(super) fn paint_drop_zones(ui: &egui::Ui, side_rects: [egui::Rect; 2]) {
    let stroke = egui::Stroke::new(1.0, ui.visuals().weak_text_color());
    for zone in side_rects.map(|rect| rect.shrink(DROP_ZONE_INSET)) {
        if zone.is_positive() {
            let outline = [
                zone.left_top(),
                zone.right_top(),
                zone.right_bottom(),
                zone.left_bottom(),
                zone.left_top(),
            ];
            ui.painter().extend(egui::Shape::dashed_line(
                &outline,
                stroke,
                DROP_ZONE_DASH,
                DROP_ZONE_GAP,
            ));
        }
    }
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

#[derive(Debug, PartialEq)]
pub(super) enum NoDiffState {
    Error(String),
    Loading(&'static str),
    Welcome,
}

pub(super) fn no_diff_state(error: Option<String>, loading: bool, any_path: bool) -> NoDiffState {
    match error {
        Some(error) => NoDiffState::Error(error),
        None if loading => NoDiffState::Loading("Loading files..."),
        // A load can finish after this frame's diff request was decided; the diff starts next.
        None if any_path => NoDiffState::Loading("Comparing files..."),
        None => NoDiffState::Welcome,
    }
}

pub(super) fn uncovered_state_rect(
    area: egui::Rect,
    browsers: [Option<egui::Rangef>; 2],
) -> Option<egui::Rect> {
    let mut uncovered = vec![area.x_range()];
    for browser in browsers.into_iter().flatten() {
        uncovered = uncovered
            .into_iter()
            .flat_map(|span| {
                [
                    egui::Rangef::new(span.min, span.max.min(browser.min)),
                    egui::Rangef::new(span.min.max(browser.max), span.max),
                ]
            })
            .filter(|span| span.span() > 0.0)
            .collect();
    }
    uncovered
        .into_iter()
        .max_by(|a, b| a.span().total_cmp(&b.span()))
        .filter(|widest| widest.span() >= MIN_STATE_WIDTH)
        .map(|widest| egui::Rect::from_x_y_ranges(widest, area.y_range()))
}

pub(super) fn compared_names(paths: [&UniversalPath; 2], labels: [Option<&str>; 2]) -> String {
    let [left, right] = [0, 1].map(|side| {
        file_path_segments(paths[side], labels[side])
            .pop()
            .map(|file| file.name)
            .filter(|name| !name.is_empty())
    });
    match (left, right) {
        (Some(left), Some(right)) if left != right => format!("{left}  vs  {right}"),
        (left, right) => left.or(right).unwrap_or_default(),
    }
}

pub(super) fn pane_state_beside_browsers(
    ui: &mut egui::Ui,
    browsers: [Option<egui::Rangef>; 2],
    add_state: impl FnOnce(&mut egui::Ui),
) {
    let rest = ui.available_rect_before_wrap();
    match uncovered_state_rect(rest, browsers) {
        Some(rect) => {
            ui.scope_builder(UiBuilder::new().max_rect(rect), add_state);
        }
        // Still allocated: the browsers reach down to the bottom of the pane's content.
        None => {
            ui.allocate_rect(rest, egui::Sense::hover());
        }
    }
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

pub(super) fn folder_ancestor(side: &UniversalPath, segment: usize) -> UniversalPath {
    let ancestor = |folder: &str| {
        let segments = split_path(folder);
        let segment = segment.min(segments.len().saturating_sub(1));
        segments.get(segment).map_or(folder.to_owned(), |s| s.path.clone())
    };
    match side {
        UniversalPath::Local(folder) => {
            UniversalPath::Local(ancestor(&folder.to_string_lossy()).into())
        }
        UniversalPath::Git(folder, rev) => {
            UniversalPath::Git(ancestor(&folder.to_string_lossy()).into(), rev.clone())
        }
        UniversalPath::Depot(folder, rev) => UniversalPath::Depot(ancestor(folder), rev.clone()),
    }
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

#[derive(Debug, PartialEq, Eq)]
pub(super) enum BrowserChange {
    Open(String),
    Close,
}

pub(super) fn browser_change(
    clicked_folder: Option<String>,
    history_folder: Option<&str>,
    shown_folder: Option<&str>,
) -> Option<BrowserChange> {
    if let Some(folder) = clicked_folder {
        return Some(BrowserChange::Open(folder));
    }
    if history_folder == shown_folder {
        return None;
    }
    Some(history_folder.map_or(BrowserChange::Close, |folder| {
        BrowserChange::Open(folder.to_owned())
    }))
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
        fn a_browser_follows_a_click_then_the_history_and_else_stays() {
            use super::super::{BrowserChange, browser_change};

            let open = |folder: &str| Some(BrowserChange::Open(folder.to_owned()));
            assert_eq!(
                browser_change(Some(r"C:\ws".to_owned()), None, None),
                open(r"C:\ws")
            );
            assert_eq!(
                browser_change(Some(r"C:\ws".to_owned()), Some(r"C:\ws"), Some(r"C:\ws")),
                open(r"C:\ws"),
                "anew: the side may have switched local <-> depot"
            );
            assert_eq!(browser_change(None, None, None), None);
            assert_eq!(browser_change(None, Some(r"C:\ws"), Some(r"C:\ws")), None);
            assert_eq!(browser_change(None, Some(r"C:\ws"), None), open(r"C:\ws"));
            assert_eq!(
                browser_change(None, Some(r"C:\ws"), Some(r"C:\ws\src")),
                open(r"C:\ws")
            );
            assert_eq!(
                browser_change(None, None, Some(r"C:\ws")),
                Some(BrowserChange::Close)
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
        fn a_segment_click_on_a_folder_side_opens_the_ancestor_at_the_same_revision() {
            use super::super::{folder_ancestor, split_path};

            let ancestor = |side: &str, segment: usize| {
                folder_ancestor(&UniversalPath::from(side), segment).to_string()
            };
            assert_eq!(ancestor(r"C:\ws\src\gui", 2), r"C:\ws\src");
            assert_eq!(
                ancestor(r"C:\ws\src\gui@git:origin/main", 2),
                r"C:\ws\src@git:origin/main"
            );
            assert_eq!(
                ancestor(r"C:\ws\src\gui@git:origin/main", 4),
                r"C:\ws\src\gui@git:origin/main",
                "a segment of the revision is the folder itself"
            );
            assert_eq!(ancestor(r"C:\ws\src@git:index", 1), r"C:\ws@git:index");
            let depot_parent = split_path("//depot/main/src").len() - 2;
            assert_eq!(
                ancestor("//depot/main/src@1234", depot_parent),
                "//depot/main@1234"
            );
        }

        #[test]
        fn a_welcome_pane_takes_a_drop_anywhere_in_a_column_split_at_the_divider() {
            use super::super::{drop_rects, welcome_drop_rects};
            use crate::ui_egui::active_side::shown_sides;
            use eframe::egui::{Rect, pos2};

            let pane = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 50.0));
            let columns = [
                Rect::from_min_max(pos2(0.0, 10.0), pos2(20.0, 50.0)),
                Rect::from_min_max(pos2(30.0, 10.0), pos2(100.0, 50.0)),
            ];
            let left_points = [pos2(5.0, 2.0), pos2(24.0, 30.0)];
            let right_points = [pos2(26.0, 30.0), pos2(90.0, 2.0)];

            for swapped in [false, true] {
                let shown = shown_sides(swapped);
                let side_rects = shown.map(|column| columns[column]);
                let [file_1, file_2] = welcome_drop_rects(side_rects, pane, shown);
                let (on_left, on_right) = if swapped {
                    (file_2, file_1)
                } else {
                    (file_1, file_2)
                };
                for point in left_points {
                    assert!(on_left.contains(point) && !on_right.contains(point));
                }
                for point in right_points {
                    assert!(on_right.contains(point) && !on_left.contains(point));
                }
            }

            let no_rows = [Rect::NOTHING; 2];
            assert_eq!(
                welcome_drop_rects(no_rows, pane, shown_sides(false)),
                drop_rects(no_rows, pane, shown_sides(false))
            );
        }

        #[test]
        fn a_pane_with_a_file_but_no_diff_yet_waits_for_it_instead_of_welcoming() {
            use super::super::{NoDiffState, no_diff_state};

            assert_eq!(no_diff_state(None, false, false), NoDiffState::Welcome);
            assert_eq!(
                no_diff_state(None, true, true),
                NoDiffState::Loading("Loading files...")
            );
            assert_eq!(
                no_diff_state(None, false, true),
                NoDiffState::Loading("Comparing files...")
            );
            assert_eq!(
                no_diff_state(Some("Left: gone".to_owned()), true, true),
                NoDiffState::Error("Left: gone".to_owned())
            );
        }

        #[test]
        fn a_pane_state_is_laid_out_beside_an_open_folder_browser() {
            use super::super::uncovered_state_rect;
            use eframe::egui::{Rangef, Rect, pos2};

            let area = Rect::from_min_max(pos2(0.0, 40.0), pos2(1000.0, 600.0));
            let (left, right) = (Rangef::new(0.0, 490.0), Rangef::new(510.0, 1000.0));
            let beside = |x_min: f32, x_max: f32| {
                Some(Rect::from_min_max(pos2(x_min, 40.0), pos2(x_max, 600.0)))
            };

            assert_eq!(uncovered_state_rect(area, [None, None]), Some(area));
            assert_eq!(
                uncovered_state_rect(area, [Some(left), None]),
                beside(490.0, 1000.0)
            );
            assert_eq!(
                uncovered_state_rect(area, [None, Some(right)]),
                beside(0.0, 510.0)
            );
            assert_eq!(uncovered_state_rect(area, [Some(left), Some(right)]), None);
            assert_eq!(
                uncovered_state_rect(area, [Some(Rangef::new(0.0, 1000.0)), None]),
                None
            );
        }

        #[test]
        fn a_waiting_pane_names_the_files_it_compares() {
            use super::super::compared_names;

            let left = UniversalPath::from(r"C:\ws\old\a.rs");
            let right = UniversalPath::from("//depot/main/b.rs");
            let none = UniversalPath::from("");

            assert_eq!(
                compared_names([&left, &right], [None, None]),
                "a.rs  vs  b.rs"
            );
            assert_eq!(
                compared_names([&left, &right], [None, Some("b.rs#3")]),
                "a.rs  vs  b.rs#3"
            );
            assert_eq!(compared_names([&left, &left], [None, None]), "a.rs");
            assert_eq!(compared_names([&none, &right], [None, None]), "b.rs");
            assert_eq!(compared_names([&none, &none], [None, None]), "");
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
