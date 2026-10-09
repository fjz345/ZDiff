use std::collections::BTreeMap;
use std::path::PathBuf;

// Unmultiplied sRGB with alpha, as egui's from_rgba_unmultiplied and its pickers take it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(transparent)
)]
pub struct Rgba8(pub [u8; 4]);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Mode {
    #[default]
    Dark,
    Light,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RowLook {
    #[default]
    Solid,
    Stripes,
    Empty,
}

pub trait Role: Copy + Eq + 'static {
    fn all() -> &'static [Self];
    fn name(self) -> &'static str;
    fn label(self) -> &'static str;
    fn dark(self) -> Rgba8;
    fn light(self) -> Rgba8;

    fn has_alpha(self) -> bool {
        false
    }

    fn default_color(self, tone: Tone) -> Rgba8 {
        match tone {
            Tone::Dark => self.dark(),
            Tone::Light => self.light(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedRole {
    DeleteText,
    DeleteBackground,
    InsertText,
    InsertBackground,
}

impl Role for SharedRole {
    fn all() -> &'static [Self] {
        &[
            SharedRole::DeleteText,
            SharedRole::DeleteBackground,
            SharedRole::InsertText,
            SharedRole::InsertBackground,
        ]
    }

    fn name(self) -> &'static str {
        match self {
            SharedRole::DeleteText => "delete_text",
            SharedRole::DeleteBackground => "delete_background",
            SharedRole::InsertText => "insert_text",
            SharedRole::InsertBackground => "insert_background",
        }
    }

    fn label(self) -> &'static str {
        match self {
            SharedRole::DeleteText => "Delete text",
            SharedRole::DeleteBackground => "Delete background",
            SharedRole::InsertText => "Insert text",
            SharedRole::InsertBackground => "Insert background",
        }
    }

    fn dark(self) -> Rgba8 {
        match self {
            SharedRole::DeleteText => Rgba8([255, 100, 100, 255]),
            SharedRole::DeleteBackground => Rgba8([255, 0, 0, 20]),
            SharedRole::InsertText => Rgba8([100, 255, 100, 255]),
            SharedRole::InsertBackground => Rgba8([0, 255, 0, 20]),
        }
    }

    fn light(self) -> Rgba8 {
        match self {
            SharedRole::DeleteText => Rgba8([180, 30, 30, 255]),
            SharedRole::DeleteBackground => Rgba8([255, 0, 0, 30]),
            SharedRole::InsertText => Rgba8([20, 130, 20, 255]),
            SharedRole::InsertBackground => Rgba8([0, 200, 0, 30]),
        }
    }

    fn has_alpha(self) -> bool {
        matches!(
            self,
            SharedRole::DeleteBackground | SharedRole::InsertBackground
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RoleColors {
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub dark: Option<Rgba8>,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub light: Option<Rgba8>,
}

impl RoleColors {
    fn slot(&mut self, tone: Tone) -> &mut Option<Rgba8> {
        match tone {
            Tone::Dark => &mut self.dark,
            Tone::Light => &mut self.light,
        }
    }
}

// Keyed by role name, not by enum, so roles of the other app survive a load and save.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(transparent)
)]
pub struct Palette(BTreeMap<String, RoleColors>);

impl Palette {
    pub fn get<R: Role>(&self, role: R, tone: Tone) -> Rgba8 {
        let colors = self.0.get(role.name());
        let set = colors.and_then(|c| match tone {
            Tone::Dark => c.dark,
            Tone::Light => c.light,
        });
        set.unwrap_or_else(|| role.default_color(tone))
    }

    pub fn set<R: Role>(&mut self, role: R, tone: Tone, color: Rgba8) {
        *self.0.entry(role.name().to_owned()).or_default().slot(tone) = Some(color);
    }

    pub fn reset<R: Role>(&mut self, role: R, tone: Tone) {
        let Some(colors) = self.0.get_mut(role.name()) else {
            return;
        };
        *colors.slot(tone) = None;
        if *colors == RoleColors::default() {
            self.0.remove(role.name());
        }
    }

    pub fn reset_all<R: Role>(&mut self) {
        for role in R::all() {
            self.0.remove(role.name());
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(default)
)]
pub struct Style {
    pub mode: Mode,
    pub palette: Palette,
    pub void_look: RowLook,
    pub collapsed_look: RowLook,
}

pub type Segment = [[f32; 2]; 2];

// 45 degree "/" lines at content x + y = k * spacing, clipped to the screen rect.
// origin is the screen position of content (0, 0), so the lines move with the content.
pub fn stripe_segments(
    min: [f32; 2],
    max: [f32; 2],
    origin: [f32; 2],
    spacing: f32,
) -> impl Iterator<Item = Segment> {
    assert!(
        spacing > 0.0,
        "stripe spacing must be positive, got {spacing}"
    );
    let [x0, y0] = [min[0] - origin[0], min[1] - origin[1]];
    let [x1, y1] = [max[0] - origin[0], max[1] - origin[1]];
    let first = ((x0 + y0) / spacing).ceil() as i64;
    let last = ((x1 + y1) / spacing).floor() as i64;
    (first..=last).filter_map(move |k| {
        let c = k as f32 * spacing;
        let xa = x0.max(c - y1);
        let xb = x1.min(c - y0);
        (xb > xa).then(|| {
            [
                [xa + origin[0], c - xa + origin[1]],
                [xb + origin[0], c - xb + origin[1]],
            ]
        })
    })
}

// %APPDATA%\ZHashDiff on Windows; shared by both apps, unlike eframe's per-app storage.
pub fn settings_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.map(|dir| dir.join("ZHashDiff"))
}

pub fn style_file_path() -> Option<PathBuf> {
    settings_dir().map(|dir| dir.join("style.json"))
}

#[cfg(feature = "serde")]
#[derive(Debug)]
pub enum StyleFileError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

#[cfg(feature = "serde")]
impl std::fmt::Display for StyleFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StyleFileError::Io(e) => write!(f, "{e}"),
            StyleFileError::Json(e) => write!(f, "invalid style JSON: {e}"),
        }
    }
}

#[cfg(feature = "serde")]
impl std::error::Error for StyleFileError {}

#[cfg(feature = "serde")]
impl Style {
    pub fn load(path: &std::path::Path) -> Result<Style, StyleFileError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log::info!("No style file at {}, using defaults", path.display());
                return Ok(Style::default());
            }
            Err(e) => return Err(StyleFileError::Io(e)),
        };
        serde_json::from_str(&text).map_err(StyleFileError::Json)
    }

    pub fn load_or_default(path: &std::path::Path) -> Style {
        Style::load(path).unwrap_or_else(|e| {
            log::error!(
                "Failed to load style {}: {e}; using defaults",
                path.display()
            );
            Style::default()
        })
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), StyleFileError> {
        let dir = path.parent().unwrap_or(std::path::Path::new("."));
        std::fs::create_dir_all(dir).map_err(StyleFileError::Io)?;
        let text = serde_json::to_string_pretty(self).map_err(StyleFileError::Json)?;
        let mut file = tempfile::NamedTempFile::new_in(dir).map_err(StyleFileError::Io)?;
        std::io::Write::write_all(&mut file, text.as_bytes()).map_err(StyleFileError::Io)?;
        file.persist(path)
            .map_err(|e| StyleFileError::Io(e.error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestRole {
        A,
        B,
    }

    impl Role for TestRole {
        fn all() -> &'static [Self] {
            &[TestRole::A, TestRole::B]
        }

        fn name(self) -> &'static str {
            match self {
                TestRole::A => "test_a",
                TestRole::B => "test_b",
            }
        }

        fn label(self) -> &'static str {
            match self {
                TestRole::A => "A",
                TestRole::B => "B",
            }
        }

        fn dark(self) -> Rgba8 {
            match self {
                TestRole::A => Rgba8([1, 1, 1, 255]),
                TestRole::B => Rgba8([2, 2, 2, 255]),
            }
        }

        fn light(self) -> Rgba8 {
            match self {
                TestRole::A => Rgba8([201, 201, 201, 255]),
                TestRole::B => Rgba8([202, 202, 202, 255]),
            }
        }
    }

    const RED: Rgba8 = Rgba8([255, 0, 0, 255]);
    const BLUE: Rgba8 = Rgba8([0, 0, 255, 128]);

    #[test]
    fn role_names_are_unique() {
        let mut names: Vec<&str> = SharedRole::all().iter().map(|r| r.name()).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn lookup_gives_the_set_color_per_tone() {
        let mut palette = Palette::default();
        palette.set(TestRole::A, Tone::Dark, RED);
        palette.set(TestRole::A, Tone::Light, BLUE);
        assert_eq!(palette.get(TestRole::A, Tone::Dark), RED);
        assert_eq!(palette.get(TestRole::A, Tone::Light), BLUE);
    }

    #[test]
    fn lookup_falls_back_to_the_default_when_missing() {
        let mut palette = Palette::default();
        assert_eq!(palette.get(TestRole::A, Tone::Dark), TestRole::A.dark());
        assert_eq!(palette.get(TestRole::A, Tone::Light), TestRole::A.light());
        palette.set(TestRole::A, Tone::Dark, RED);
        assert_eq!(palette.get(TestRole::A, Tone::Light), TestRole::A.light());
        assert_eq!(palette.get(TestRole::B, Tone::Dark), TestRole::B.dark());
    }

    #[test]
    fn reset_one_role_restores_its_default_for_that_tone_only() {
        let mut palette = Palette::default();
        palette.set(TestRole::A, Tone::Dark, RED);
        palette.set(TestRole::A, Tone::Light, BLUE);
        palette.set(TestRole::B, Tone::Dark, RED);
        palette.reset(TestRole::A, Tone::Dark);
        assert_eq!(palette.get(TestRole::A, Tone::Dark), TestRole::A.dark());
        assert_eq!(palette.get(TestRole::A, Tone::Light), BLUE);
        assert_eq!(palette.get(TestRole::B, Tone::Dark), RED);
        palette.reset(TestRole::A, Tone::Light);
        palette.reset(TestRole::B, Tone::Dark);
        assert_eq!(palette, Palette::default());
    }

    #[test]
    fn reset_all_restores_every_role_of_the_set_and_keeps_others() {
        let mut palette = Palette::default();
        palette.set(TestRole::A, Tone::Dark, RED);
        palette.set(TestRole::B, Tone::Light, BLUE);
        palette.set(SharedRole::DeleteText, Tone::Dark, RED);
        palette.reset_all::<TestRole>();
        assert_eq!(palette.get(TestRole::A, Tone::Dark), TestRole::A.dark());
        assert_eq!(palette.get(TestRole::B, Tone::Light), TestRole::B.light());
        assert_eq!(palette.get(SharedRole::DeleteText, Tone::Dark), RED);
    }

    fn content_segments(min: [f32; 2], max: [f32; 2], origin: [f32; 2]) -> Vec<Segment> {
        stripe_segments(min, max, origin, 8.0)
            .map(|[a, b]| {
                [
                    [a[0] - origin[0], a[1] - origin[1]],
                    [b[0] - origin[0], b[1] - origin[1]],
                ]
            })
            .collect()
    }

    fn assert_segments_eq(a: &[Segment], b: &[Segment]) {
        assert_eq!(a.len(), b.len(), "{a:?} vs {b:?}");
        for (sa, sb) in a.iter().zip(b) {
            for (pa, pb) in sa.iter().zip(sb) {
                assert!(
                    (pa[0] - pb[0]).abs() < 1e-3 && (pa[1] - pb[1]).abs() < 1e-3,
                    "{a:?} vs {b:?}"
                );
            }
        }
    }

    #[test]
    fn stripes_stay_put_in_content_space_when_scrolled() {
        let row = ([100.0, 40.0], [140.0, 60.0]);
        let origin = [100.0, 20.0];
        let still = content_segments(row.0, row.1, origin);

        let scroll = 37.5;
        let scrolled_origin = [origin[0], origin[1] - scroll];
        let scrolled = content_segments(
            [row.0[0], row.0[1] - scroll],
            [row.1[0], row.1[1] - scroll],
            scrolled_origin,
        );
        assert_segments_eq(&still, &scrolled);

        let clipped = content_segments(
            [row.0[0], row.0[1] - scroll + 5.0],
            [row.1[0], row.1[1] - scroll],
            scrolled_origin,
        );
        let still_sums: Vec<f32> = still.iter().map(|[a, _]| a[0] + a[1]).collect();
        assert!(!clipped.is_empty());
        for [a, _] in &clipped {
            assert!(still_sums.iter().any(|s| (s - (a[0] + a[1])).abs() < 1e-3));
        }

        assert_eq!(still.len(), 7);
        assert_segments_eq(&still[..1], &[[[0.0, 24.0], [4.0, 20.0]]]);
        for [a, b] in &still {
            assert!(((a[0] + a[1]) - (b[0] + b[1])).abs() < 1e-3);
            assert!(((a[0] + a[1]) / 8.0).fract().abs() < 1e-3);
        }
    }

    #[test]
    fn stripes_handle_negative_content_coordinates() {
        let segments = content_segments([0.0, 0.0], [10.0, 10.0], [20.0, 20.0]);
        let sums: Vec<f32> = segments.iter().map(|[a, _]| a[0] + a[1]).collect();
        assert_eq!(sums, vec![-32.0, -24.0]);
    }

    #[cfg(feature = "serde")]
    mod file {
        use super::*;

        fn custom_style() -> Style {
            let mut style = Style {
                mode: Mode::System,
                void_look: RowLook::Stripes,
                collapsed_look: RowLook::Empty,
                ..Style::default()
            };
            style.palette.set(TestRole::A, Tone::Dark, RED);
            style.palette.set(SharedRole::InsertText, Tone::Light, BLUE);
            style
        }

        #[test]
        fn style_file_round_trips() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("nested").join("style.json");
            let style = custom_style();
            style.save(&path).unwrap();
            assert_eq!(Style::load(&path).unwrap(), style);
        }

        #[test]
        fn missing_file_gives_defaults() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("style.json");
            assert_eq!(Style::load(&path).unwrap(), Style::default());
            assert_eq!(Style::load_or_default(&path), Style::default());
        }

        #[test]
        fn invalid_json_gives_defaults_and_an_error() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("style.json");
            std::fs::write(&path, "{ not json").unwrap();
            assert!(matches!(Style::load(&path), Err(StyleFileError::Json(_))));
            assert_eq!(Style::load_or_default(&path), Style::default());
        }

        #[test]
        fn older_file_missing_roles_and_fields_gets_defaults() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("style.json");
            std::fs::write(
                &path,
                r#"{ "mode": "Light", "palette": { "test_a": { "dark": [255, 0, 0, 255] } } }"#,
            )
            .unwrap();
            let style = Style::load(&path).unwrap();
            assert_eq!(style.mode, Mode::Light);
            assert_eq!(style.void_look, RowLook::default());
            assert_eq!(style.palette.get(TestRole::A, Tone::Dark), RED);
            assert_eq!(
                style.palette.get(TestRole::A, Tone::Light),
                TestRole::A.light()
            );
            assert_eq!(
                style.palette.get(TestRole::B, Tone::Dark),
                TestRole::B.dark()
            );
            assert_eq!(
                style.palette.get(SharedRole::DeleteText, Tone::Dark),
                SharedRole::DeleteText.dark()
            );
        }

        #[test]
        fn unknown_role_names_survive_a_round_trip() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("style.json");
            std::fs::write(
                &path,
                r#"{ "palette": { "other_app_role": { "light": [1, 2, 3, 4] } } }"#,
            )
            .unwrap();
            let mut style = Style::load(&path).unwrap();
            style.palette.set(TestRole::A, Tone::Dark, RED);
            style.palette.reset_all::<TestRole>();
            style.palette.reset_all::<SharedRole>();
            style.save(&path).unwrap();

            let text = std::fs::read_to_string(&path).unwrap();
            let json: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                json["palette"],
                serde_json::json!({ "other_app_role": { "light": [1, 2, 3, 4] } })
            );
        }
    }
}
