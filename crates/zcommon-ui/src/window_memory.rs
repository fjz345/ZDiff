use eframe::egui;
use serde::Deserialize;

// Mirrors eframe's private STORAGE_WINDOW_KEY.
const WINDOW_KEY: &str = "window";
const MIN_RESTORABLE_POINTS: f32 = 64.0;

#[derive(Deserialize, Default)]
#[serde(default)]
struct SavedWindowSize {
    inner_size_points: Option<egui::Vec2>,
}

/// eframe saves a minimized window as 0x0 on Windows, which restores as a 64x64 window.
#[derive(Default)]
pub struct WindowMemory {
    restorable: Option<String>,
}

fn stored_window_is_restorable(storage: &dyn eframe::Storage) -> bool {
    eframe::get_value::<SavedWindowSize>(storage, WINDOW_KEY)
        .and_then(|window| window.inner_size_points)
        .is_none_or(|size| size.min_elem() >= MIN_RESTORABLE_POINTS)
}

impl WindowMemory {
    pub fn load(storage: Option<&dyn eframe::Storage>) -> Self {
        let restorable = storage
            .filter(|storage| stored_window_is_restorable(*storage))
            .and_then(|storage| storage.get_string(WINDOW_KEY));
        Self { restorable }
    }

    pub fn keep_restorable(&mut self, storage: &mut dyn eframe::Storage) {
        if stored_window_is_restorable(storage) {
            self.restorable = storage.get_string(WINDOW_KEY);
        } else if let Some(restorable) = &self.restorable {
            storage.set_string(WINDOW_KEY, restorable.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const RESTORED: &str = "(inner_position_pixels:Some((x:169.0,y:262.0)),outer_position_pixels:Some((x:160.0,y:224.0)),fullscreen:false,maximized:false,inner_size_points:Some((x:1508.8,y:704.8)))";
    const MINIMIZED: &str = "(inner_position_pixels:Some((x:-32000.0,y:-32000.0)),outer_position_pixels:Some((x:-32000.0,y:-32000.0)),fullscreen:false,maximized:false,inner_size_points:Some((x:0.0,y:0.0)))";

    #[derive(Default)]
    struct MemoryStorage(HashMap<String, String>);

    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }

        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }

        fn flush(&mut self) {}
    }

    fn storage_with_window(window: &str) -> MemoryStorage {
        let mut storage = MemoryStorage::default();
        eframe::Storage::set_string(&mut storage, WINDOW_KEY, window.to_owned());
        storage
    }

    fn stored_window(storage: &MemoryStorage) -> Option<&str> {
        storage.0.get(WINDOW_KEY).map(String::as_str)
    }

    #[test]
    fn a_minimized_save_is_replaced_by_the_last_restored_one() {
        let mut memory = WindowMemory::default();
        let mut storage = storage_with_window(RESTORED);
        memory.keep_restorable(&mut storage);

        eframe::Storage::set_string(&mut storage, WINDOW_KEY, MINIMIZED.to_owned());
        memory.keep_restorable(&mut storage);

        assert_eq!(stored_window(&storage), Some(RESTORED));
    }

    #[test]
    fn a_minimized_save_is_replaced_by_the_window_loaded_at_startup() {
        let mut memory = WindowMemory::load(Some(&storage_with_window(RESTORED)));
        let mut storage = storage_with_window(MINIMIZED);

        memory.keep_restorable(&mut storage);

        assert_eq!(stored_window(&storage), Some(RESTORED));
    }

    #[test]
    fn a_minimized_window_at_startup_is_not_remembered() {
        let mut memory = WindowMemory::load(Some(&storage_with_window(MINIMIZED)));
        let mut storage = storage_with_window(MINIMIZED);

        memory.keep_restorable(&mut storage);

        assert_eq!(stored_window(&storage), Some(MINIMIZED));
    }

    #[test]
    fn a_restored_save_is_kept() {
        let mut memory = WindowMemory::load(Some(&storage_with_window(MINIMIZED)));
        let mut storage = storage_with_window(RESTORED);

        memory.keep_restorable(&mut storage);

        assert_eq!(stored_window(&storage), Some(RESTORED));
    }
}
