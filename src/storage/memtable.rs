// Phase 1: In-Memory Core
// BTreeMap provides O(log n) reads/writes and keeps keys sorted — essential for SSTable flushes.
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Data(Vec<u8>),
    Tombstone,
}

pub struct MemTable {
    map: BTreeMap<Vec<u8>, Value>,
    size_bytes: usize,
    pub capacity_bytes: usize,
}

impl MemTable {
    pub fn new(capacity_bytes: usize) -> Self {
        Self {
            map: BTreeMap::new(),
            size_bytes: 0,
            capacity_bytes,
        }
    }

    pub fn set(&mut self, key: Vec<u8>, value: Vec<u8>) {
        let entry_size = key.len() + value.len();
        if let Some(old) = self.map.get(&key) {
            match old {
                Value::Data(v) => self.size_bytes -= key.len() + v.len(),
                Value::Tombstone => self.size_bytes -= key.len(),
            }
        }
        self.size_bytes += entry_size;
        self.map.insert(key, Value::Data(value));
    }

    pub fn delete(&mut self, key: &[u8]) {
        if let Some(old) = self.map.get(key) {
            match old {
                Value::Data(v) => self.size_bytes -= key.len() + v.len(),
                Value::Tombstone => {}
            }
        }
        self.size_bytes += key.len();
        self.map.insert(key.to_vec(), Value::Tombstone);
    }

    pub fn get(&self, key: &[u8]) -> Option<&Value> {
        self.map.get(key)
    }

    pub fn is_full(&self) -> bool {
        self.size_bytes >= self.capacity_bytes
    }

    pub fn size(&self) -> usize {
        self.size_bytes
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Returns a sorted iterator over all entries — used when flushing to SSTable.
    pub fn iter(&self) -> impl Iterator<Item = (&Vec<u8>, &Value)> {
        self.map.iter()
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.size_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get() {
        let mut m = MemTable::new(1024);
        m.set(b"k".to_vec(), b"v".to_vec());
        assert_eq!(m.get(b"k"), Some(&Value::Data(b"v".to_vec())));
    }

    #[test]
    fn delete_inserts_tombstone() {
        let mut m = MemTable::new(1024);
        m.set(b"k".to_vec(), b"v".to_vec());
        m.delete(b"k");
        assert_eq!(m.get(b"k"), Some(&Value::Tombstone));
    }

    #[test]
    fn overwrite_updates_size() {
        let mut m = MemTable::new(1024);
        m.set(b"k".to_vec(), b"long_value".to_vec()); // 1 + 10 = 11
        let size_before = m.size();
        m.set(b"k".to_vec(), b"x".to_vec()); // 1 + 1 = 2
        assert!(m.size() < size_before);
    }

    #[test]
    fn is_full_when_at_capacity() {
        let mut m = MemTable::new(5);
        m.set(b"kk".to_vec(), b"vvv".to_vec()); // 2 + 3 = 5
        assert!(m.is_full());
    }

    #[test]
    fn clear_resets_state() {
        let mut m = MemTable::new(1024);
        m.set(b"k".to_vec(), b"v".to_vec());
        m.clear();
        assert_eq!(m.len(), 0);
        assert_eq!(m.size(), 0);
    }
}
