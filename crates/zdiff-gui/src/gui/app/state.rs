use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use zcommon::find::FindQuery;
use zcommon::presets::UserPresets;
use zcommon_ui::h_scroll::WheelModifier;
use zdiff::{diff_builder::DiffBuilderOptions, myers::MyersDiffAlgorithm};

use super::nav::NavHistory;
use crate::{
    diff_ctx::DiffProcessor,
    file::FileProcessor,
    keybindings::Keybindings,
    p4::P4Profiles,
    pdb_dump::PdbDumpOptions,
    revert::RevertHistory,
    toggle_all::ToggleAll,
    ui_egui::{diff_pane::TextMetric, syntax::DetectedLanguage, text_mode::TextMode},
    viewer::{
        FfmpegSetting, SharedKinds, ViewerKind, ViewerOverride, Viewers,
        audio::{AudioDiffProcessor, AudioSettings},
        audio_meta::AudioMetaProcessor,
        hex::HexDiffProcessor,
        hex_strings::{HexMode, HexSettings, HexStrings},
        image::{ImageDiffProcessor, ImageSettings},
        video_frames::VideoFrameProcessor,
        video_meta::VideoMetaProcessor,
    },
};

#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AppStateCtx {
    pub file_1: FileProcessor,
    pub file_2: FileProcessor,

    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub diff_processor: DiffProcessor,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub hex_processor: HexDiffProcessor,
    #[cfg_attr(feature = "serde", serde(default))]
    pub hex_settings: HexSettings,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub hex_strings: HexStrings,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub strings_active: bool,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub image_processor: ImageDiffProcessor,
    #[cfg_attr(feature = "serde", serde(default))]
    pub image_settings: ImageSettings,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub audio_processor: AudioDiffProcessor,
    #[cfg_attr(feature = "serde", serde(default))]
    pub audio_settings: AudioSettings,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub audio_meta: AudioMetaProcessor,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub video_frames: VideoFrameProcessor,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub video_meta: VideoMetaProcessor,
    /// Resolved each frame from the loaded files; `None` while neither side is loaded.
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub viewers: Option<Viewers>,
    /// Not persisted: paths aren't either, so a restart always opens a new pair.
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub viewer_override: ViewerOverride,
    /// The last logged reason the pair isn't in the viewer it asked for.
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub viewer_fallback: Option<String>,
    // Pre-shared-table viewer map, kept in the save until migrated into the kind file.
    #[cfg_attr(
        feature = "serde",
        serde(
            default,
            rename = "extension_map",
            skip_serializing_if = "Option::is_none"
        )
    )]
    pub legacy_extension_map: Option<BTreeMap<String, ViewerKind>>,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub kinds: SharedKinds,
    #[cfg_attr(feature = "serde", serde(default))]
    pub ffmpeg: FfmpegSetting,
    #[cfg_attr(feature = "serde", serde(default))]
    pub pdb_options: PdbDumpOptions,
    /// Undo and redo of reverts; in memory only.
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub revert_history: RevertHistory,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub nav_history: NavHistory,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub toggle_all: ToggleAll,

    pub diff_options: DiffBuilderOptions,

    pub myers_diff_algorithm: MyersDiffAlgorithm,
    // A new key, so saves pinned to the old "rs" default load as Auto.
    #[cfg_attr(feature = "serde", serde(default))]
    pub code_language_override: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub syntax_filter: String,
    #[cfg_attr(feature = "serde", serde(default = "default_search_syntax_variants"))]
    pub search_syntax_variants: bool,
    #[cfg_attr(feature = "serde", serde(skip), serde(default))]
    pub detected_languages: [DetectedLanguage; 2],

    // ### Keybindings
    pub keybindings: Keybindings,

    #[cfg_attr(feature = "serde", serde(default))]
    pub p4_profiles: P4Profiles,
    #[cfg_attr(feature = "serde", serde(default))]
    pub regex_ignore_presets: UserPresets,

    // ### UI TEMP
    pub scroll_left: f32,
    pub scroll_right: f32,
    // Saves from before the setting load linked, like a fresh state.
    #[cfg_attr(feature = "serde", serde(default = "default_h_scroll_linked"))]
    pub h_scroll_linked: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    pub h_scroll_modifier: WheelModifier,
    #[cfg_attr(feature = "serde", serde(default = "default_center_jumps"))]
    pub center_jumps: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    pub word_wrap: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    pub scope_breadcrumb: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    pub scope_block_headers: bool,
    #[cfg_attr(feature = "serde", serde(default = "default_revert_buttons"))]
    pub revert_buttons: bool,
    #[cfg_attr(feature = "serde", serde(default))]
    pub text_mode: TextMode,
    #[cfg_attr(feature = "serde", serde(default))]
    pub text_likeness: TextMetric,
    #[cfg_attr(feature = "serde", serde(default))]
    pub sides_swapped: bool,

    #[cfg_attr(feature = "serde", serde(skip))]
    pub goto_open: bool,
    #[cfg_attr(feature = "serde", serde(skip))]
    pub goto_input: String,
    #[cfg_attr(feature = "serde", serde(skip))]
    pub find_open: bool,
    #[cfg_attr(feature = "serde", serde(skip))]
    pub find_query: FindQuery,
    #[cfg_attr(feature = "serde", serde(default))]
    pub find_both_panes: bool,
    #[cfg_attr(feature = "serde", serde(skip))]
    pub find_error: Option<String>,
}

fn default_h_scroll_linked() -> bool {
    true
}

fn default_center_jumps() -> bool {
    true
}

fn default_revert_buttons() -> bool {
    true
}

fn default_search_syntax_variants() -> bool {
    true
}

impl Default for AppStateCtx {
    fn default() -> Self {
        Self {
            file_1: Default::default(),
            file_2: Default::default(),
            diff_options: Default::default(),
            diff_processor: Default::default(),
            hex_processor: Default::default(),
            hex_settings: Default::default(),
            hex_strings: Default::default(),
            strings_active: false,
            image_processor: Default::default(),
            image_settings: Default::default(),
            audio_processor: Default::default(),
            audio_settings: Default::default(),
            audio_meta: Default::default(),
            video_frames: Default::default(),
            video_meta: Default::default(),
            viewers: None,
            viewer_override: Default::default(),
            viewer_fallback: None,
            legacy_extension_map: None,
            kinds: Default::default(),
            ffmpeg: Default::default(),
            pdb_options: Default::default(),
            revert_history: Default::default(),
            nav_history: Default::default(),
            toggle_all: Default::default(),
            scroll_left: Default::default(),
            scroll_right: Default::default(),
            h_scroll_linked: default_h_scroll_linked(),
            h_scroll_modifier: WheelModifier::Shift,
            center_jumps: default_center_jumps(),
            word_wrap: false,
            scope_breadcrumb: false,
            scope_block_headers: false,
            revert_buttons: default_revert_buttons(),
            text_mode: Default::default(),
            text_likeness: Default::default(),
            sides_swapped: false,
            goto_open: Default::default(),
            find_open: Default::default(),
            goto_input: Default::default(),
            find_query: Default::default(),
            find_both_panes: false,
            find_error: None,
            keybindings: Default::default(),
            p4_profiles: Default::default(),
            regex_ignore_presets: Default::default(),
            myers_diff_algorithm: Default::default(),
            code_language_override: None,
            syntax_filter: String::new(),
            search_syntax_variants: default_search_syntax_variants(),
            detected_languages: Default::default(),
        }
    }
}

impl AppStateCtx {
    /// The hex pair is shown as the text diff of its extracted strings.
    pub(super) fn strings_view(&self) -> bool {
        self.viewers == Some(Viewers::Pair(ViewerKind::Hex))
            && self.hex_settings.mode == HexMode::Strings
    }

    pub(super) fn shows_text_diff(&self) -> bool {
        self.strings_view()
            || self
                .viewers
                .is_none_or(|v| v == Viewers::Pair(ViewerKind::Text))
    }
}

#[derive(Debug, Serialize, Deserialize)]
// #[serde(bound(serialize = "", deserialize = "T: RawToken"))]
pub(super) enum AppState {
    Startup(AppStateCtx),
    Idle(AppStateCtx),
    Exit(AppStateCtx),
}

impl AppState {
    pub(super) fn variant_name(&self) -> &'static str {
        match self {
            Self::Startup(_) => "Startup",
            Self::Idle(_) => "Idle",
            Self::Exit(_) => "Exit",
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        AppState::Startup(AppStateCtx::default())
    }
}

impl AppState {
    pub(super) fn into_ctx(self) -> AppStateCtx {
        match self {
            AppState::Startup(ctx) | AppState::Idle(ctx) | AppState::Exit(ctx) => ctx,
        }
    }
    pub(super) fn ctx_mut(&mut self) -> &mut AppStateCtx {
        match self {
            AppState::Startup(ctx) | AppState::Idle(ctx) | AppState::Exit(ctx) => ctx,
        }
    }
}

#[cfg(all(test, feature = "serde"))]
mod tests {
    use super::super::ZApp;
    use super::*;
    use crate::viewer::audio::AudioMode;
    use crate::viewer::image::ImageSampling;
    use crate::{
        keybindings::FileAction,
        ui_egui::merge_pane::{MergeRequest, MergeView},
    };
    use zcommon_ui::keymap::Action;
    use zdiff::ignore::IgnorePatterns;
    use zdiff::{myers::DiffPhases, universal_path::UniversalPath};

    #[test]
    fn swapped_sides_survive_a_restart() {
        let state = AppStateCtx {
            sides_swapped: true,
            ..Default::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();

        assert!(restored.sides_swapped);
    }

    #[test]
    fn the_syntax_variant_search_survives_a_restart_and_an_old_save_searches_them() {
        assert!(AppStateCtx::default().search_syntax_variants);
        let state = AppStateCtx {
            search_syntax_variants: false,
            ..Default::default()
        };
        let mut json = serde_json::to_value(&state).unwrap();
        let restored: AppStateCtx = serde_json::from_value(json.clone()).unwrap();
        assert!(!restored.search_syntax_variants);

        json.as_object_mut()
            .unwrap()
            .remove("search_syntax_variants")
            .unwrap();
        let old: AppStateCtx = serde_json::from_value(json).unwrap();
        assert!(old.search_syntax_variants);
    }

    #[test]
    fn a_save_with_an_old_custom_language_still_highlights_it() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        let object = json.as_object_mut().unwrap();
        object.insert("code_language_override".into(), "go".into());
        object.insert("code_language_custom".into(), "go".into());

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();

        let language = restored.code_language_override.as_deref().unwrap();
        assert_eq!(crate::ui_egui::syntax::language_name(language), Some("Go"));
        let resaved = serde_json::to_value(&restored).unwrap();
        assert!(resaved.get("code_language_custom").is_none());
    }

    #[test]
    fn pdb_options_survive_a_restart_and_an_old_save_loads_the_defaults() {
        let state = AppStateCtx {
            pdb_options: PdbDumpOptions {
                public_only: true,
                ignore_addresses: true,
                ignore_identity: false,
            },
            ..Default::default()
        };
        let mut json = serde_json::to_value(&state).unwrap();
        let restored: AppStateCtx = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(restored.pdb_options, state.pdb_options);

        json["pdb_options"].as_object_mut().unwrap().remove("ignore_identity");
        let partial: AppStateCtx = serde_json::from_value(json.clone()).unwrap();
        assert!(partial.pdb_options.public_only && partial.pdb_options.ignore_identity);

        json.as_object_mut().unwrap().remove("pdb_options");
        let old: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(old.pdb_options, PdbDumpOptions::default());
    }

    #[test]
    fn a_state_saved_before_the_swap_existed_loads_unswapped() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut().unwrap().remove("sides_swapped");

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();

        assert!(!restored.sides_swapped);
    }

    #[test]
    fn an_old_viewer_map_is_read_and_saved_again_until_migrated() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut().unwrap().insert(
            "extension_map".into(),
            serde_json::json!({ ".ZBIN": "Hex", "png": "Image" }),
        );
        let mut restored: AppStateCtx = serde_json::from_value(json).unwrap();
        let old = BTreeMap::from([
            (".ZBIN".to_string(), ViewerKind::Hex),
            ("png".to_string(), ViewerKind::Image),
        ]);
        assert_eq!(restored.legacy_extension_map, Some(old.clone()));

        let resaved: AppStateCtx =
            serde_json::from_str(&serde_json::to_string(&restored).unwrap()).unwrap();
        assert_eq!(resaved.legacy_extension_map, Some(old));

        restored.legacy_extension_map = None;
        let json = serde_json::to_value(&restored).unwrap();
        assert!(json.get("extension_map").is_none());
    }

    #[test]
    fn a_save_without_a_viewer_map_has_nothing_to_migrate() {
        let mut ctx = AppStateCtx::default();
        ctx.viewer_override.kind = Some(ViewerKind::Text);
        let json = serde_json::to_value(&ctx).unwrap();
        assert!(json.get("extension_map").is_none());
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.legacy_extension_map, None);
        // Paths aren't saved, so a restart opens a new pair and the override is gone.
        assert_eq!(restored.viewer_override.kind, None);
    }

    #[test]
    fn the_ffmpeg_path_is_saved_and_older_saves_have_none() {
        let mut ctx = AppStateCtx::default();
        ctx.ffmpeg.path = r"C:\tools\ffmpeg\bin".to_string();
        let mut json = serde_json::to_value(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(restored.ffmpeg.path, r"C:\tools\ffmpeg\bin");

        json.as_object_mut().unwrap().remove("ffmpeg");
        let older: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(older.ffmpeg.path, "");
    }

    #[test]
    fn the_image_sampling_is_saved_and_older_saves_have_nearest() {
        let mut ctx = AppStateCtx::default();
        ctx.image_settings.sampling = ImageSampling::Linear;
        let mut json = serde_json::to_value(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(restored.image_settings.sampling, ImageSampling::Linear);

        json.as_object_mut().unwrap().remove("image_settings");
        let older: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(older.image_settings.sampling, ImageSampling::Nearest);
    }

    #[test]
    fn the_audio_view_mode_is_saved_and_older_saves_have_lanes() {
        let mut ctx = AppStateCtx::default();
        ctx.audio_settings.mode = AudioMode::Overlay;
        let mut json = serde_json::to_value(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(restored.audio_settings.mode, AudioMode::Overlay);

        json.as_object_mut().unwrap().remove("audio_settings");
        let older: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(older.audio_settings.mode, AudioMode::Lanes);
    }

    #[test]
    fn pins_are_never_saved() {
        let mut ctx = AppStateCtx::default();
        ctx.diff_processor.set_pins(vec![zdiff::myers::Pin {
            source: 1,
            target: 2,
        }]);

        let json = serde_json::to_string(&ctx).unwrap();
        assert!(!json.contains("pin"), "{json}");
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert!(restored.diff_processor.pins().is_empty());
    }

    #[test]
    fn a_pivot_saved_by_an_older_version_is_discarded() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json["diff_options"].as_object_mut().unwrap().insert(
            "pivot_lines".into(),
            serde_json::json!({"left": 2, "right": 3}),
        );
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.diff_options, DiffBuilderOptions::default());
        assert!(restored.diff_processor.pins().is_empty());
        let saved_again = serde_json::to_string(&restored).unwrap();
        assert!(!saved_again.contains("pivot"), "{saved_again}");
    }

    #[test]
    fn horizontal_scroll_link_defaults_on_and_survives_a_restart() {
        assert!(AppStateCtx::default().h_scroll_linked);

        let ctx = AppStateCtx {
            h_scroll_linked: false,
            ..Default::default()
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert!(!restored.h_scroll_linked);
    }

    #[test]
    fn horizontal_scroll_wheel_modifier_defaults_to_shift_and_survives_a_restart() {
        assert_eq!(
            AppStateCtx::default().h_scroll_modifier,
            WheelModifier::Shift
        );

        let ctx = AppStateCtx {
            h_scroll_modifier: WheelModifier::Ctrl,
            ..Default::default()
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.h_scroll_modifier, WheelModifier::Ctrl);
    }

    #[test]
    fn a_save_without_the_horizontal_scroll_wheel_modifier_loads_shift() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("h_scroll_modifier")
            .unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.h_scroll_modifier, WheelModifier::Shift);
    }

    #[test]
    fn regex_ignore_presets_survive_a_restart() {
        use crate::ui_egui::diff_pane::REGEX_IGNORE_PRESETS;
        let mut ctx = AppStateCtx::default();
        ctx.regex_ignore_presets
            .save(REGEX_IGNORE_PRESETS, "Mine", r"\d+\nfoo")
            .unwrap();

        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.regex_ignore_presets, ctx.regex_ignore_presets);
        let user: Vec<_> = restored
            .regex_ignore_presets
            .list(REGEX_IGNORE_PRESETS)
            .filter(|e| !e.builtin)
            .map(|e| (e.name, e.text))
            .collect();
        assert_eq!(user, [("Mine", r"\d+\nfoo")]);
    }

    #[test]
    fn a_save_without_regex_ignore_presets_loads_none() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("regex_ignore_presets")
            .unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.regex_ignore_presets, UserPresets::default());
    }

    #[test]
    fn a_save_without_the_horizontal_scroll_link_loads_linked() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("h_scroll_linked")
            .unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert!(restored.h_scroll_linked);
    }

    #[test]
    fn centered_jumps_default_on_and_survive_a_restart() {
        assert!(AppStateCtx::default().center_jumps);

        let ctx = AppStateCtx {
            center_jumps: false,
            ..Default::default()
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert!(!restored.center_jumps);
    }

    #[test]
    fn a_save_without_centered_jumps_loads_centered() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("center_jumps")
            .unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert!(restored.center_jumps);
    }

    #[test]
    fn scope_block_headers_default_off_and_survive_a_restart() {
        assert!(!AppStateCtx::default().scope_block_headers);

        let ctx = AppStateCtx {
            scope_block_headers: true,
            ..Default::default()
        };
        let mut json = serde_json::to_value(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_value(json.clone()).unwrap();
        assert!(restored.scope_block_headers);

        json.as_object_mut()
            .unwrap()
            .remove("scope_block_headers")
            .unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert!(!restored.scope_block_headers);
    }

    #[test]
    fn revert_buttons_default_shown_and_survive_a_restart() {
        assert!(AppStateCtx::default().revert_buttons);

        let ctx = AppStateCtx {
            revert_buttons: false,
            ..Default::default()
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert!(!restored.revert_buttons);
    }

    #[test]
    fn a_save_without_the_revert_buttons_setting_shows_them() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("revert_buttons")
            .unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert!(restored.revert_buttons);
    }

    #[test]
    fn zoom_survives_a_restart() {
        let mut app = ZApp::new();
        app.zoom = 1.7;
        let json = serde_json::to_string(&app).unwrap();
        let restored: ZApp = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.zoom, 1.7);
    }

    #[test]
    fn a_merge_is_never_saved() {
        let mut app = ZApp::new();
        app.merge = Some(MergeView::new(
            MergeRequest {
                base: None,
                theirs: UniversalPath::from("merge_theirs.txt"),
                yours: UniversalPath::from("merge_yours.txt"),
                output: UniversalPath::from("merge_output.txt"),
            },
            MyersDiffAlgorithm::Linear,
        ));
        let json = serde_json::to_string(&app).unwrap();
        for path in ["merge_theirs.txt", "merge_yours.txt", "merge_output.txt"] {
            assert!(!json.contains(path), "{json}");
        }
        let restored: ZApp = serde_json::from_str(&json).unwrap();
        assert!(restored.merge.is_none());
    }

    #[test]
    fn a_saved_zoom_out_of_range_loads_clamped() {
        let mut json = serde_json::to_value(ZApp::new()).unwrap();
        json["zoom"] = 10.0.into();
        assert_eq!(
            serde_json::from_value::<ZApp>(json.clone()).unwrap().zoom,
            3.0
        );
        json["zoom"] = 0.1.into();
        assert_eq!(serde_json::from_value::<ZApp>(json).unwrap().zoom, 0.5);
    }

    #[test]
    fn a_save_without_a_zoom_loads_at_100_percent() {
        let mut json = serde_json::to_value(ZApp::new()).unwrap();
        json.as_object_mut().unwrap().remove("zoom").unwrap();
        let restored: ZApp = serde_json::from_value(json).unwrap();
        assert_eq!(restored.zoom, 1.0);
    }

    #[test]
    fn diff_phases_survive_a_restart() {
        let mut ctx = AppStateCtx::default();
        ctx.diff_options.phases = DiffPhases {
            scope: true,
            line: false,
            token: true,
            character: true,
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.diff_options.phases, ctx.diff_options.phases);
    }

    #[test]
    fn a_save_without_diff_phases_loads_the_defaults() {
        let mut ctx = AppStateCtx::default();
        ctx.diff_options.ignore.whitespace = true;
        let mut json = serde_json::to_value(&ctx).unwrap();
        json["diff_options"]
            .as_object_mut()
            .unwrap()
            .remove("phases")
            .unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(
            restored.diff_options.phases,
            DiffBuilderOptions::default().phases
        );
        assert!(restored.diff_options.ignore.whitespace);
    }

    #[test]
    fn a_save_without_diff_marks_loads_only_the_line_mark() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        let options = json["diff_options"].as_object_mut().unwrap();
        for field in ["mark_scopes", "mark_lines", "mark_tokens", "mark_chars"] {
            options.remove(field).unwrap();
        }
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.diff_options, DiffBuilderOptions::default());
        assert!(restored.diff_options.highlight_rows && restored.diff_options.mark_lines);
    }

    #[test]
    fn regex_off_survives_a_restart_with_its_patterns() {
        let mut ctx = AppStateCtx::default();
        ctx.diff_options.ignore.patterns = IgnorePatterns::new(r"\d+");
        ctx.diff_options.ignore.regex_enabled = false;
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.diff_options.ignore, ctx.diff_options.ignore);
    }

    #[test]
    fn a_save_without_the_regex_flag_loads_with_regex_enabled() {
        let mut ctx = AppStateCtx::default();
        ctx.diff_options.ignore.patterns = IgnorePatterns::new(r"\d+");
        ctx.diff_options.ignore.regex_enabled = false;
        let mut json = serde_json::to_value(&ctx).unwrap();
        json["diff_options"]
            .as_object_mut()
            .unwrap()
            .remove("ignore_regex_enabled")
            .unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert!(restored.diff_options.ignore.regex_enabled);
        assert_eq!(restored.diff_options.ignore.patterns.text(), r"\d+");
    }

    #[test]
    fn word_wrap_defaults_off_and_survives_a_restart() {
        assert!(!AppStateCtx::default().word_wrap);

        let ctx = AppStateCtx {
            word_wrap: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert!(restored.word_wrap);
    }

    #[test]
    fn text_mode_defaults_to_side_by_side_and_survives_a_restart() {
        assert_eq!(AppStateCtx::default().text_mode, TextMode::SideBySide);

        let ctx = AppStateCtx {
            text_mode: TextMode::Unified,
            ..Default::default()
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.text_mode, TextMode::Unified);
    }

    #[test]
    fn the_chosen_single_file_side_survives_a_restart() {
        for mode in [TextMode::SingleLeft, TextMode::SingleRight] {
            let ctx = AppStateCtx {
                text_mode: mode,
                ..Default::default()
            };
            let json = serde_json::to_string(&ctx).unwrap();
            let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
            assert_eq!(restored.text_mode, mode);
        }
    }

    #[test]
    fn a_save_without_a_text_mode_loads_side_by_side() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut().unwrap().remove("text_mode").unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.text_mode, TextMode::SideBySide);
    }

    #[test]
    fn the_text_likeness_metric_survives_a_restart_and_older_saves_load_lines() {
        let ctx = AppStateCtx {
            text_likeness: TextMetric::Tokens,
            ..AppStateCtx::default()
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.text_likeness, TextMetric::Tokens);

        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut().unwrap().remove("text_likeness").unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.text_likeness, TextMetric::Lines);
    }

    #[test]
    fn a_save_without_word_wrap_loads_unwrapped() {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        json.as_object_mut().unwrap().remove("word_wrap").unwrap();
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert!(!restored.word_wrap);
    }

    fn old_format_save(missing: &[&str]) -> serde_json::Value {
        let mut json = serde_json::to_value(AppStateCtx::default()).unwrap();
        let quick_diffs = json["keybindings"]["user_quick_diffs"].take();
        let mut old = serde_json::json!({ "user_quick_diffs": quick_diffs });
        for field in [
            "find",
            "undo_revert",
            "redo_revert",
            "redo_revert_alt",
            "diff_local_file",
        ] {
            if !missing.contains(&field) {
                old[field] = serde_json::Value::Null;
            }
        }
        json["keybindings"] = old;
        json
    }

    #[test]
    fn an_old_save_without_the_undo_keys_loads_the_defaults() {
        let json = old_format_save(&["undo_revert", "redo_revert", "redo_revert_alt"]);
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        let keymap = &restored.keybindings.keymap;
        assert!(!FileAction::Undo.defaults().is_empty());
        assert_eq!(
            keymap.shortcuts(FileAction::Undo),
            FileAction::Undo.defaults()
        );
        assert_eq!(keymap.shortcuts(FileAction::Redo).len(), 3);
        assert_eq!(
            keymap.shortcuts(FileAction::Redo),
            FileAction::Redo.defaults()
        );
        assert_eq!(keymap.shortcuts(FileAction::DiffLocalFile), []);
    }

    #[test]
    fn undo_keys_survive_a_restart() {
        let mut ctx = AppStateCtx::default();
        let find = ctx.keybindings.keymap.shortcuts(FileAction::Find)[0];
        let keymap = &mut ctx.keybindings.keymap;
        keymap.shortcuts_mut(FileAction::Undo).clear();
        keymap.shortcuts_mut(FileAction::Redo).insert(0, find);
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.keybindings.keymap, ctx.keybindings.keymap);
    }

    #[test]
    fn an_old_save_without_the_local_file_key_loads_the_default() {
        let json = old_format_save(&["diff_local_file"]);
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        let keymap = &restored.keybindings.keymap;
        assert!(!FileAction::DiffLocalFile.defaults().is_empty());
        assert_eq!(
            keymap.shortcuts(FileAction::DiffLocalFile),
            FileAction::DiffLocalFile.defaults()
        );
        assert_eq!(keymap.shortcuts(FileAction::Undo), []);
    }

    #[test]
    fn the_local_file_key_survives_a_restart() {
        let mut ctx = AppStateCtx::default();
        let find = ctx.keybindings.keymap.shortcuts(FileAction::Find)[0];
        *ctx.keybindings
            .keymap
            .shortcuts_mut(FileAction::DiffLocalFile) = vec![find];
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored
                .keybindings
                .keymap
                .shortcuts(FileAction::DiffLocalFile),
            [find]
        );

        ctx.keybindings
            .keymap
            .shortcuts_mut(FileAction::DiffLocalFile)
            .clear();
        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored
                .keybindings
                .keymap
                .shortcuts(FileAction::DiffLocalFile),
            []
        );
    }

    #[test]
    fn p4_profiles_and_slot_profiles_survive_a_restart() {
        let mut ctx = AppStateCtx::default();
        let id = ctx.p4_profiles.add("work".into());
        ctx.p4_profiles.profiles[0].config.port = "ssl:p4:1666".into();
        ctx.p4_profiles.default = Some(id);
        ctx.keybindings.user_quick_diffs[1].1.p4_profile = Some(id);

        let json = serde_json::to_string(&ctx).unwrap();
        let restored: AppStateCtx = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.p4_profiles, ctx.p4_profiles);
        let slot_profiles: Vec<_> = restored
            .keybindings
            .user_quick_diffs
            .iter()
            .map(|(_, slot)| slot.p4_profile)
            .collect();
        assert_eq!(slot_profiles, [None, Some(id), None, None]);
    }

    #[test]
    fn a_save_with_a_p4_password_loads_and_drops_it_on_the_next_save() {
        let mut ctx = AppStateCtx::default();
        ctx.p4_profiles.add("work".into());
        let mut json = serde_json::to_value(&ctx).unwrap();
        json["p4_profiles"]["profiles"][0]["config"]["password"] = "secret".into();

        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.p4_profiles, ctx.p4_profiles);
        assert!(
            !serde_json::to_string(&restored)
                .unwrap()
                .contains("password")
        );
    }

    /// The single p4 config before profiles was never saved, so such a save has no profiles.
    #[test]
    fn a_save_without_p4_profiles_loads_with_none_and_auto_slots() {
        let mut ctx = AppStateCtx::default();
        ctx.p4_profiles.add("work".into());
        let mut json = serde_json::to_value(&ctx).unwrap();
        json.as_object_mut().unwrap().remove("p4_profiles").unwrap();
        for slot in json["keybindings"]["user_quick_diffs"]
            .as_array_mut()
            .unwrap()
        {
            slot[1]
                .as_object_mut()
                .unwrap()
                .remove("p4_profile")
                .unwrap();
        }
        let restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_eq!(restored.p4_profiles, P4Profiles::default());
        assert_eq!(restored.keybindings.user_quick_diffs.len(), 4);
        assert!(
            restored
                .keybindings
                .user_quick_diffs
                .iter()
                .all(|(_, slot)| slot.p4_profile.is_none())
        );
    }

    #[test]
    fn a_save_with_p4_profiles_but_no_next_id_adds_unused_ids() {
        let mut ctx = AppStateCtx::default();
        let id = ctx.p4_profiles.add("work".into());
        let mut json = serde_json::to_value(&ctx).unwrap();
        json["p4_profiles"]
            .as_object_mut()
            .unwrap()
            .remove("next_id")
            .unwrap();
        let mut restored: AppStateCtx = serde_json::from_value(json).unwrap();
        assert_ne!(restored.p4_profiles.add("new".into()), id);
    }
}
