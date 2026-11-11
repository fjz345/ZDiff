use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use eframe::egui::{Key, Modifiers, PointerButton};

use super::keymap::{Action, Keymap, Shortcut};
use zcommon::style::settings_dir;

const CTRL: Modifiers = Modifiers {
    ctrl: !cfg!(target_os = "macos"),
    mac_cmd: cfg!(target_os = "macos"),
    command: true,
    ..Modifiers::NONE
};
const CTRL_SHIFT: Modifiers = Modifiers {
    shift: true,
    ..CTRL
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SharedAction {
    Open1,
    Open2,
    NextDiff,
    PrevDiff,
    Refresh,
    Find,
    OpenKeybindings,
    SwapSides,
    ClearPaths,
    OpenStyle,
    Export,
    Undo,
    Redo,
}

impl SharedAction {
    pub const ALL: &'static [Self] = &[
        Self::Open1,
        Self::Open2,
        Self::NextDiff,
        Self::PrevDiff,
        Self::Refresh,
        Self::Find,
        Self::OpenKeybindings,
        Self::SwapSides,
        Self::ClearPaths,
        Self::OpenStyle,
        Self::Export,
        Self::Undo,
        Self::Redo,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Open1 => "open_1",
            Self::Open2 => "open_2",
            Self::NextDiff => "next_diff",
            Self::PrevDiff => "prev_diff",
            Self::Refresh => "refresh",
            Self::Find => "find",
            Self::OpenKeybindings => "open_keybindings",
            Self::SwapSides => "swap_sides",
            Self::ClearPaths => "clear_paths",
            Self::OpenStyle => "open_style",
            Self::Export => "export",
            Self::Undo => "undo",
            Self::Redo => "redo",
        }
    }

    pub fn defaults(self) -> &'static [Shortcut] {
        match self {
            Self::Open1 => &[Shortcut::Key {
                key: Key::F1,
                modifiers: CTRL,
            }],
            Self::Open2 => &[Shortcut::Key {
                key: Key::F2,
                modifiers: CTRL,
            }],
            Self::NextDiff => &[
                Shortcut::Key {
                    key: Key::Num2,
                    modifiers: CTRL,
                },
                Shortcut::Key {
                    key: Key::ArrowDown,
                    modifiers: Modifiers::ALT,
                },
            ],
            Self::PrevDiff => &[
                Shortcut::Key {
                    key: Key::Num1,
                    modifiers: CTRL,
                },
                Shortcut::Key {
                    key: Key::ArrowUp,
                    modifiers: Modifiers::ALT,
                },
            ],
            Self::Refresh => &[Shortcut::Key {
                key: Key::R,
                modifiers: CTRL,
            }],
            Self::Find => &[Shortcut::Key {
                key: Key::F,
                modifiers: CTRL,
            }],
            Self::OpenKeybindings => &[Shortcut::Key {
                key: Key::O,
                modifiers: CTRL,
            }],
            Self::SwapSides => &[Shortcut::Key {
                key: Key::S,
                modifiers: CTRL_SHIFT,
            }],
            Self::ClearPaths => &[Shortcut::Key {
                key: Key::W,
                modifiers: CTRL,
            }],
            Self::OpenStyle => &[Shortcut::Key {
                key: Key::Comma,
                modifiers: CTRL,
            }],
            Self::Export => &[Shortcut::Key {
                key: Key::E,
                modifiers: CTRL,
            }],
            Self::Undo => &[
                Shortcut::Key {
                    key: Key::Z,
                    modifiers: CTRL,
                },
                Shortcut::Pointer {
                    button: PointerButton::Extra1,
                    modifiers: Modifiers::NONE,
                },
            ],
            Self::Redo => &[
                Shortcut::Key {
                    key: Key::Y,
                    modifiers: CTRL,
                },
                Shortcut::Key {
                    key: Key::Z,
                    modifiers: CTRL_SHIFT,
                },
                Shortcut::Pointer {
                    button: PointerButton::Extra2,
                    modifiers: Modifiers::NONE,
                },
            ],
        }
    }
}

type SharedFile = BTreeMap<String, Vec<Shortcut>>;

pub fn keymap_file_path() -> Option<PathBuf> {
    settings_dir().map(|dir| dir.join("keymap.json"))
}

#[derive(Debug)]
pub enum KeymapFileError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for KeymapFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeymapFileError::Io(e) => write!(f, "{e}"),
            KeymapFileError::Json(e) => write!(f, "invalid keymap JSON: {e}"),
        }
    }
}

impl std::error::Error for KeymapFileError {}

fn read_file(path: &Path) -> Result<SharedFile, KeymapFileError> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(KeymapFileError::Json),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SharedFile::new()),
        Err(e) => Err(KeymapFileError::Io(e)),
    }
}

fn write_file(path: &Path, file: &SharedFile) -> Result<(), KeymapFileError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(KeymapFileError::Io)?;
    let text = serde_json::to_string_pretty(file).map_err(KeymapFileError::Json)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(KeymapFileError::Io)?;
    std::io::Write::write_all(&mut temp, text.as_bytes()).map_err(KeymapFileError::Io)?;
    temp.persist(path)
        .map_err(|e| KeymapFileError::Io(e.error))?;
    Ok(())
}

// Seeds instead of overwriting: a binding customized before the file existed carries over.
pub fn sync_from_file<A: Action>(
    keymap: &mut Keymap<A>,
    path: &Path,
) -> Result<(), KeymapFileError> {
    let mut file = read_file(path)?;
    let mut seeded = false;
    for &action in A::all() {
        let Some(shared) = action.shared() else {
            continue;
        };
        match file.get(shared.name()) {
            Some(shortcuts) => *keymap.shortcuts_mut(action) = shortcuts.clone(),
            None if keymap.shortcuts(action) != shared.defaults() => {
                file.insert(shared.name().to_owned(), keymap.shortcuts(action).to_vec());
                seeded = true;
            }
            None => {}
        }
    }
    if seeded {
        write_file(path, &file)?;
    }
    Ok(())
}

// Re-reads and writes only the changed entries, so another running app's edits survive.
pub fn save_changes_to_file<A: Action>(
    before: &Keymap<A>,
    after: &Keymap<A>,
    path: &Path,
) -> Result<(), KeymapFileError> {
    let changed: Vec<_> = A::all()
        .iter()
        .filter(|&&action| before.shortcuts(action) != after.shortcuts(action))
        .filter_map(|&action| Some((action.shared()?, after.shortcuts(action))))
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    let mut file = read_file(path)?;
    for (shared, shortcuts) in changed {
        file.insert(shared.name().to_owned(), shortcuts.to_vec());
    }
    write_file(path, &file)
}

pub fn load_shared<A: Action>(keymap: &mut Keymap<A>) {
    let Some(path) = keymap_file_path() else {
        log::warn!("No settings folder found; keybindings are not shared");
        return;
    };
    if let Err(e) = sync_from_file(keymap, &path) {
        log::error!("Failed to load shared keymap {}: {e}", path.display());
    }
}

pub fn save_shared<A: Action>(before: &Keymap<A>, after: &Keymap<A>) {
    let Some(path) = keymap_file_path() else {
        return;
    };
    if let Err(e) = save_changes_to_file(before, after, &path) {
        log::error!("Failed to save shared keymap {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum TestAction {
        Open1,
        Find,
        Refresh,
        Local,
    }

    impl Action for TestAction {
        fn all() -> &'static [Self] {
            &[Self::Open1, Self::Find, Self::Refresh, Self::Local]
        }

        fn name(self) -> &'static str {
            match self {
                Self::Open1 => "open_source",
                Self::Find => "find",
                Self::Refresh => "refresh_diff",
                Self::Local => "local",
            }
        }

        fn label(self) -> &'static str {
            self.name()
        }

        fn defaults(self) -> &'static [Shortcut] {
            match self.shared() {
                Some(shared) => shared.defaults(),
                None => &[Shortcut::Key {
                    key: Key::L,
                    modifiers: Modifiers::ALT,
                }],
            }
        }

        fn shared(self) -> Option<SharedAction> {
            match self {
                Self::Open1 => Some(SharedAction::Open1),
                Self::Find => Some(SharedAction::Find),
                Self::Refresh => Some(SharedAction::Refresh),
                Self::Local => None,
            }
        }
    }

    fn alt(key: Key) -> Shortcut {
        Shortcut::Key {
            key,
            modifiers: Modifiers::ALT,
        }
    }

    fn write_json(path: &Path, value: serde_json::Value) {
        std::fs::write(path, value.to_string()).unwrap();
    }

    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn shortcuts_json(shortcuts: &[Shortcut]) -> serde_json::Value {
        serde_json::to_value(shortcuts).unwrap()
    }

    #[test]
    fn shared_names_are_unique() {
        let names: std::collections::HashSet<_> =
            SharedAction::ALL.iter().map(|a| a.name()).collect();
        assert_eq!(names.len(), SharedAction::ALL.len());
    }

    #[test]
    fn shared_defaults_are_pinned() {
        let ctrl = |key| Shortcut::Key {
            key,
            modifiers: CTRL,
        };
        let ctrl_shift = |key| Shortcut::Key {
            key,
            modifiers: CTRL_SHIFT,
        };
        let mouse = |button| Shortcut::Pointer {
            button,
            modifiers: Modifiers::NONE,
        };
        let expected = [
            (SharedAction::Open1, vec![ctrl(Key::F1)]),
            (SharedAction::Open2, vec![ctrl(Key::F2)]),
            (
                SharedAction::NextDiff,
                vec![ctrl(Key::Num2), alt(Key::ArrowDown)],
            ),
            (
                SharedAction::PrevDiff,
                vec![ctrl(Key::Num1), alt(Key::ArrowUp)],
            ),
            (SharedAction::Refresh, vec![ctrl(Key::R)]),
            (SharedAction::Find, vec![ctrl(Key::F)]),
            (SharedAction::OpenKeybindings, vec![ctrl(Key::O)]),
            (SharedAction::SwapSides, vec![ctrl_shift(Key::S)]),
            (SharedAction::ClearPaths, vec![ctrl(Key::W)]),
            (SharedAction::OpenStyle, vec![ctrl(Key::Comma)]),
            (SharedAction::Export, vec![ctrl(Key::E)]),
            (
                SharedAction::Undo,
                vec![ctrl(Key::Z), mouse(PointerButton::Extra1)],
            ),
            (
                SharedAction::Redo,
                vec![
                    ctrl(Key::Y),
                    ctrl_shift(Key::Z),
                    mouse(PointerButton::Extra2),
                ],
            ),
        ];
        assert_eq!(expected.len(), SharedAction::ALL.len());
        for (action, shortcuts) in expected {
            assert_eq!(action.defaults(), shortcuts, "{action:?}");
        }
    }

    #[test]
    fn file_entries_override_mapped_actions_and_absent_ones_keep_the_app_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keymap.json");
        write_json(
            &path,
            json!({ "open_1": shortcuts_json(&[alt(Key::A)]), "refresh": [] }),
        );
        let mut keymap = Keymap::<TestAction>::default();
        keymap.shortcuts_mut(TestAction::Local).push(alt(Key::B));

        sync_from_file(&mut keymap, &path).unwrap();

        assert_eq!(keymap.shortcuts(TestAction::Open1), [alt(Key::A)]);
        assert_eq!(keymap.shortcuts(TestAction::Refresh), []);
        assert_eq!(
            keymap.shortcuts(TestAction::Find),
            SharedAction::Find.defaults()
        );
        assert_eq!(
            keymap.shortcuts(TestAction::Local),
            [alt(Key::L), alt(Key::B)]
        );
    }

    #[test]
    fn a_missing_file_leaves_default_bindings_alone_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keymap.json");
        let mut keymap = Keymap::<TestAction>::default();

        sync_from_file(&mut keymap, &path).unwrap();

        assert_eq!(keymap, Keymap::default());
        assert!(!path.exists());
    }

    #[test]
    fn customized_bindings_absent_from_the_file_are_seeded_into_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keymap.json");
        write_json(&path, json!({ "open_1": shortcuts_json(&[alt(Key::A)]) }));
        let mut keymap = Keymap::<TestAction>::default();
        *keymap.shortcuts_mut(TestAction::Open1) = vec![alt(Key::X)];
        *keymap.shortcuts_mut(TestAction::Find) = vec![alt(Key::F)];
        keymap.shortcuts_mut(TestAction::Local).clear();

        sync_from_file(&mut keymap, &path).unwrap();

        assert_eq!(keymap.shortcuts(TestAction::Open1), [alt(Key::A)]);
        assert_eq!(keymap.shortcuts(TestAction::Find), [alt(Key::F)]);
        assert_eq!(
            read_json(&path),
            json!({
                "open_1": shortcuts_json(&[alt(Key::A)]),
                "find": shortcuts_json(&[alt(Key::F)]),
            })
        );
    }

    #[test]
    fn saving_writes_only_changed_shared_entries_and_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keymap.json");
        write_json(
            &path,
            json!({
                "open_1": shortcuts_json(&[alt(Key::A)]),
                "find": shortcuts_json(&[alt(Key::G)]),
                "from_a_newer_build": shortcuts_json(&[alt(Key::N)]),
            }),
        );
        let before = Keymap::<TestAction>::default();
        let mut after = before.clone();
        *after.shortcuts_mut(TestAction::Refresh) = vec![alt(Key::R)];
        *after.shortcuts_mut(TestAction::Local) = vec![alt(Key::Z)];

        save_changes_to_file(&before, &after, &path).unwrap();

        assert_eq!(
            read_json(&path),
            json!({
                "open_1": shortcuts_json(&[alt(Key::A)]),
                "find": shortcuts_json(&[alt(Key::G)]),
                "refresh": shortcuts_json(&[alt(Key::R)]),
                "from_a_newer_build": shortcuts_json(&[alt(Key::N)]),
            })
        );
    }

    #[test]
    fn saving_without_shared_changes_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keymap.json");
        let before = Keymap::<TestAction>::default();
        let mut after = before.clone();
        *after.shortcuts_mut(TestAction::Local) = vec![alt(Key::Z)];

        save_changes_to_file(&before, &after, &path).unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn saving_over_an_unreadable_file_fails_and_leaves_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keymap.json");
        std::fs::write(&path, "not json").unwrap();
        let before = Keymap::<TestAction>::default();
        let mut after = before.clone();
        after.shortcuts_mut(TestAction::Find).clear();

        assert!(matches!(
            save_changes_to_file(&before, &after, &path),
            Err(KeymapFileError::Json(_))
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
    }
}
