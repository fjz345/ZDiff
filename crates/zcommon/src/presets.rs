#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinPreset {
    pub name: &'static str,
    pub text: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Preset {
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(transparent)
)]
pub struct UserPresets(Vec<Preset>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetEntry<'a> {
    pub name: &'a str,
    pub text: &'a str,
    pub builtin: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Saved {
    Added,
    Overwritten,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameUse {
    Free,
    User,
    Builtin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetError {
    EmptyName,
    BuiltinName(String),
    NotFound(String),
}

impl std::fmt::Display for PresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PresetError::EmptyName => write!(f, "A preset needs a name"),
            PresetError::BuiltinName(name) => write!(f, "\"{name}\" is a built-in preset"),
            PresetError::NotFound(name) => write!(f, "No preset named \"{name}\""),
        }
    }
}

impl std::error::Error for PresetError {}

impl UserPresets {
    pub fn list<'a>(
        &'a self,
        builtins: &'a [BuiltinPreset],
    ) -> impl Iterator<Item = PresetEntry<'a>> + 'a {
        let builtins = builtins.iter().map(|p| PresetEntry {
            name: p.name,
            text: p.text,
            builtin: true,
        });
        let user = self.0.iter().map(|p| PresetEntry {
            name: &p.name,
            text: &p.text,
            builtin: false,
        });
        builtins.chain(user)
    }

    pub fn name_use(&self, builtins: &[BuiltinPreset], name: &str) -> NameUse {
        let name = name.trim();
        if builtins.iter().any(|p| p.name == name) {
            NameUse::Builtin
        } else if self.0.iter().any(|p| p.name == name) {
            NameUse::User
        } else {
            NameUse::Free
        }
    }

    pub fn save(
        &mut self,
        builtins: &[BuiltinPreset],
        name: &str,
        text: &str,
    ) -> Result<Saved, PresetError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(PresetError::EmptyName);
        }
        if builtins.iter().any(|p| p.name == name) {
            return Err(PresetError::BuiltinName(name.to_string()));
        }
        if let Some(existing) = self.0.iter_mut().find(|p| p.name == name) {
            existing.text = text.to_string();
            return Ok(Saved::Overwritten);
        }
        self.0.push(Preset {
            name: name.to_string(),
            text: text.to_string(),
        });
        Ok(Saved::Added)
    }

    pub fn delete(&mut self, builtins: &[BuiltinPreset], name: &str) -> Result<(), PresetError> {
        let name = name.trim();
        if builtins.iter().any(|p| p.name == name) {
            return Err(PresetError::BuiltinName(name.to_string()));
        }
        let idx = self
            .0
            .iter()
            .position(|p| p.name == name)
            .ok_or_else(|| PresetError::NotFound(name.to_string()))?;
        self.0.remove(idx);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILTINS: &[BuiltinPreset] = &[
        BuiltinPreset {
            name: "Times",
            text: r"\d\d:\d\d:\d\d",
        },
        BuiltinPreset {
            name: "GUIDs",
            text: r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}",
        },
    ];

    fn names(user: &UserPresets) -> Vec<(&str, bool)> {
        user.list(BUILTINS).map(|e| (e.name, e.builtin)).collect()
    }

    fn texts(user: &UserPresets) -> Vec<&str> {
        user.list(BUILTINS).map(|e| e.text).collect()
    }

    #[test]
    fn list_gives_builtins_then_user_presets_in_save_order() {
        let mut user = UserPresets::default();
        assert_eq!(names(&user), [("Times", true), ("GUIDs", true)]);

        user.save(BUILTINS, "Zeta", "z").unwrap();
        user.save(BUILTINS, "Alpha", "a").unwrap();
        assert_eq!(
            names(&user),
            [
                ("Times", true),
                ("GUIDs", true),
                ("Zeta", false),
                ("Alpha", false)
            ]
        );
        assert_eq!(
            texts(&user),
            [
                r"\d\d:\d\d:\d\d",
                r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}",
                "z",
                "a"
            ]
        );
    }

    #[test]
    fn save_new_name_appends() {
        let mut user = UserPresets::default();
        assert_eq!(user.name_use(BUILTINS, "Mine"), NameUse::Free);
        assert_eq!(user.save(BUILTINS, "Mine", "a\nb"), Ok(Saved::Added));
        assert_eq!(user.name_use(BUILTINS, "Mine"), NameUse::User);
        assert_eq!(names(&user).last(), Some(&("Mine", false)));
        assert_eq!(texts(&user).last(), Some(&"a\nb"));
    }

    #[test]
    fn save_existing_user_name_overwrites_in_place() {
        let mut user = UserPresets::default();
        user.save(BUILTINS, "First", "1").unwrap();
        user.save(BUILTINS, "Second", "2").unwrap();
        assert_eq!(user.save(BUILTINS, "First", "one"), Ok(Saved::Overwritten));
        assert_eq!(names(&user)[2..], [("First", false), ("Second", false)]);
        assert_eq!(texts(&user)[2..], ["one", "2"]);
    }

    #[test]
    fn save_trims_the_name_and_refuses_an_empty_one() {
        let mut user = UserPresets::default();
        assert_eq!(user.save(BUILTINS, "  Mine ", "a"), Ok(Saved::Added));
        assert_eq!(user.save(BUILTINS, "Mine", "b"), Ok(Saved::Overwritten));
        assert_eq!(user.save(BUILTINS, " \t", "c"), Err(PresetError::EmptyName));
        assert_eq!(names(&user)[2..], [("Mine", false)]);
        assert_eq!(texts(&user)[2..], ["b"]);
    }

    #[test]
    fn save_refuses_a_builtin_name() {
        let mut user = UserPresets::default();
        assert_eq!(user.name_use(BUILTINS, "Times"), NameUse::Builtin);
        let err = user.save(BUILTINS, "Times", "mine").unwrap_err();
        assert_eq!(err, PresetError::BuiltinName("Times".to_string()));
        assert_eq!(err.to_string(), "\"Times\" is a built-in preset");
        assert_eq!(user, UserPresets::default());
        assert_eq!(texts(&user)[0], r"\d\d:\d\d:\d\d");
    }

    #[test]
    fn delete_removes_own_preset() {
        let mut user = UserPresets::default();
        user.save(BUILTINS, "First", "1").unwrap();
        user.save(BUILTINS, "Second", "2").unwrap();
        assert_eq!(user.delete(BUILTINS, "First"), Ok(()));
        assert_eq!(names(&user)[2..], [("Second", false)]);
        assert_eq!(user.name_use(BUILTINS, "First"), NameUse::Free);
        assert_eq!(
            user.delete(BUILTINS, "First"),
            Err(PresetError::NotFound("First".to_string()))
        );
    }

    #[test]
    fn builtins_cannot_be_deleted() {
        let mut user = UserPresets::default();
        user.save(BUILTINS, "Mine", "m").unwrap();
        assert_eq!(
            user.delete(BUILTINS, "GUIDs"),
            Err(PresetError::BuiltinName("GUIDs".to_string()))
        );
        assert_eq!(
            names(&user),
            [("Times", true), ("GUIDs", true), ("Mine", false)]
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn user_presets_serde_round_trip() {
        let mut user = UserPresets::default();
        user.save(BUILTINS, "B", "x, y").unwrap();
        user.save(BUILTINS, "A", "line1\nline2").unwrap();
        let json = serde_json::to_string(&user).unwrap();
        assert_eq!(
            json,
            r#"[{"name":"B","text":"x, y"},{"name":"A","text":"line1\nline2"}]"#
        );
        assert_eq!(serde_json::from_str::<UserPresets>(&json).unwrap(), user);
        assert_eq!(
            serde_json::from_str::<UserPresets>("[]").unwrap(),
            UserPresets::default()
        );
    }
}
