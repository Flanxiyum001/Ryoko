use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Shared blackboard for cross-agent knowledge (decisions, discovered symbols,
/// conventions). Cloning is cheap and all clones share the same map.
///
/// Writes are short critical sections; never hold a guard across `.await`.
#[derive(Debug, Clone, Default)]
pub struct SharedMemory {
    inner: Arc<RwLock<HashMap<String, String>>>,
}

impl SharedMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, key: impl Into<String>, value: impl Into<String>) -> Option<String> {
        self.inner
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.into(), value.into())
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
    }

    pub fn remove(&self, key: &str) -> Option<String> {
        self.inner
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key)
    }

    pub fn snapshot(&self) -> HashMap<String, String> {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn extend(&self, values: impl IntoIterator<Item = (String, String)>) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.extend(values);
    }

    pub fn len(&self) -> usize {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_state_across_clones_and_threads() {
        let memory = SharedMemory::new();
        let handle = memory.clone();
        std::thread::spawn(move || {
            for index in 0..100 {
                handle.insert(format!("key-{index}"), index.to_string());
            }
        })
        .join()
        .unwrap();

        assert_eq!(memory.len(), 100);
        assert_eq!(memory.get("key-42").as_deref(), Some("42"));
        assert_eq!(memory.snapshot().len(), 100);
    }
}
