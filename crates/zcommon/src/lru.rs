pub const DEFAULT_MAX_ENTRIES: usize = 6;
pub const DEFAULT_MAX_BYTES: usize = 256 << 20;

struct Entry<K, V> {
    key: K,
    value: V,
    bytes: usize,
}

/// Keys only need `PartialEq`: lookups scan, which is cheap for the few entries kept.
pub struct LruCache<K, V> {
    /// Least recently used first.
    entries: Vec<Entry<K, V>>,
    max_entries: usize,
    max_bytes: usize,
    bytes: usize,
}

impl<K: PartialEq, V> LruCache<K, V> {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        assert!(max_entries > 0, "an LRU cache needs room for an entry");
        Self {
            entries: Vec::new(),
            max_entries,
            max_bytes,
            bytes: 0,
        }
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        let index = self.entries.iter().position(|e| e.key == *key)?;
        let entry = self.entries.remove(index);
        self.entries.push(entry);
        self.entries.last().map(|e| &e.value)
    }

    pub fn insert(&mut self, key: K, value: V, bytes: usize) {
        self.retain(|k| *k != key);
        if bytes > self.max_bytes {
            return;
        }
        self.bytes += bytes;
        self.entries.push(Entry { key, value, bytes });
        while self.entries.len() > self.max_entries || self.bytes > self.max_bytes {
            let evicted = self.entries.remove(0);
            self.bytes -= evicted.bytes;
        }
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        self.entries.retain(|e| keep(&e.key));
        self.bytes = self.entries.iter().map(|e| e.bytes).sum();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

impl<K: PartialEq, V> Default for LruCache<K, V> {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_ENTRIES, DEFAULT_MAX_BYTES)
    }
}

impl<K, V> std::fmt::Debug for LruCache<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LruCache")
            .field("len", &self.entries.len())
            .field("bytes", &self.bytes)
            .field("max_entries", &self.max_entries)
            .field("max_bytes", &self.max_bytes)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(cache: &LruCache<u32, &str>) -> Vec<u32> {
        cache.entries.iter().map(|e| e.key).collect()
    }

    #[test]
    fn a_hit_returns_the_inserted_value_and_a_miss_none() {
        let mut cache = LruCache::new(3, 100);
        cache.insert(1, "one", 1);
        assert_eq!(cache.get(&1), Some(&"one"));
        assert_eq!(cache.get(&2), None);
    }

    #[test]
    fn the_least_recently_used_entry_is_evicted_past_the_entry_count() {
        let mut cache = LruCache::new(2, 100);
        cache.insert(1, "one", 1);
        cache.insert(2, "two", 1);
        cache.insert(3, "three", 1);
        assert_eq!(keys(&cache), [2, 3]);
        assert_eq!(cache.get(&1), None);
    }

    #[test]
    fn a_get_makes_the_entry_most_recently_used() {
        let mut cache = LruCache::new(2, 100);
        cache.insert(1, "one", 1);
        cache.insert(2, "two", 1);
        cache.get(&1);
        cache.insert(3, "three", 1);
        assert_eq!(keys(&cache), [1, 3]);
    }

    #[test]
    fn entries_are_evicted_oldest_first_until_within_the_byte_budget() {
        let mut cache = LruCache::new(10, 100);
        cache.insert(1, "one", 40);
        cache.insert(2, "two", 40);
        cache.insert(3, "three", 50);
        assert_eq!(keys(&cache), [2, 3]);
        assert_eq!(cache.bytes, 90);
        cache.insert(4, "four", 50);
        assert_eq!(keys(&cache), [3, 4]);
        assert_eq!(cache.bytes, 100);
    }

    #[test]
    fn an_entry_over_the_byte_budget_is_not_kept_and_evicts_nothing() {
        let mut cache = LruCache::new(10, 100);
        cache.insert(1, "one", 10);
        cache.insert(2, "huge", 101);
        assert_eq!(keys(&cache), [1]);
        assert_eq!(cache.bytes, 10);
    }

    #[test]
    fn an_entry_over_the_byte_budget_still_drops_the_value_it_replaces() {
        let mut cache = LruCache::new(10, 100);
        cache.insert(1, "one", 10);
        cache.insert(1, "huge", 101);
        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.bytes, 0);
    }

    #[test]
    fn inserting_an_equal_key_replaces_its_value_and_bytes() {
        let mut cache = LruCache::new(3, 100);
        cache.insert(1, "one", 30);
        cache.insert(2, "two", 30);
        cache.insert(1, "uno", 10);
        assert_eq!(keys(&cache), [2, 1]);
        assert_eq!(cache.get(&1), Some(&"uno"));
        assert_eq!(cache.bytes, 40);
    }

    #[test]
    fn retain_drops_the_rejected_entries_and_their_bytes() {
        let mut cache = LruCache::new(3, 100);
        cache.insert(1, "one", 10);
        cache.insert(2, "two", 20);
        cache.insert(3, "three", 30);
        cache.retain(|key| key % 2 == 1);
        assert_eq!(keys(&cache), [1, 3]);
        assert_eq!(cache.bytes, 40);
    }
}
