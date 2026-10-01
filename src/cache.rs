use std::{collections::HashMap, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
/// Everything that can change the rendered PNG for one page.
/// `version` changes when the PDF is reloaded; width/height/zoom change on resize.
pub struct Key {
    pub version: u64,
    pub page: u32,
    pub width: u16,
    pub height: u16,
    pub zoom: u16,
}

/// Owns temporary rendered PNGs, removing replaced and discarded files.
pub struct Cache(HashMap<Key, PathBuf>);

impl Cache {
    pub fn new() -> Self {
        Self(HashMap::new())
    }
    pub fn get(&mut self, key: &Key) -> Option<PathBuf> {
        self.0.get(key).cloned()
    }
    pub fn put(&mut self, key: Key, path: PathBuf) {
        if let Some(old) = self.0.insert(key, path.clone()) {
            if old != path {
                let _ = std::fs::remove_file(old);
            }
        }
    }
    pub fn contains(&self, key: &Key) -> bool {
        self.0.contains_key(key)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn clear(&mut self) {
        for (_, path) in self.0.drain() {
            let _ = std::fs::remove_file(path);
        }
    }
}
