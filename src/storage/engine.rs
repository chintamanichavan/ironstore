use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::compaction::Compactor;
use crate::error::{Result, VektaError};
use crate::index::VectorIndex;
use crate::storage::memtable::{MemTable, Value};
use crate::storage::sstable::{SSTable, SSTableWriter};
use crate::storage::wal::{Wal, WalRecord};

const MEMTABLE_CAPACITY: usize = 4 * 1024 * 1024; // 4 MB
const COMPACTION_THRESHOLD: usize = 4;

const VEC_PREFIX: &[u8] = b"\x00v:";

pub struct Engine {
    data_dir: PathBuf,
    memtable: MemTable,
    wal: Wal,
    sstables: Vec<SSTable>,
    seq: Arc<AtomicU64>,
    vector_index: VectorIndex,
    pending_compaction: bool,
    max_memory_bytes: usize,
}

impl Engine {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&data_dir)?;

        let wal_path = data_dir.join("wal.log");
        let records = Wal::replay(&wal_path)?;

        let mut memtable = MemTable::new(MEMTABLE_CAPACITY);
        for record in records {
            match record {
                WalRecord::Set { key, value } => memtable.set(key, value),
                WalRecord::Delete { key } => memtable.delete(&key),
            }
        }

        let wal = Wal::open(&wal_path)?;
        let sstables = Self::load_sstables(&data_dir)?;
        let seq = Arc::new(AtomicU64::new(sstables.len() as u64));

        let vector_index = rebuild_vector_index(&memtable, &sstables)?;

        tracing::info!(
            "Engine opened: {} WAL records replayed, {} SSTables, {} vectors indexed",
            memtable.len(),
            sstables.len(),
            vector_index.len(),
        );

        let max_memory_bytes = std::env::var("VEKTALITE_MAX_MEMORY_MB")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|mb| mb * 1024 * 1024)
            .unwrap_or(usize::MAX);

        Ok(Self {
            data_dir,
            memtable,
            wal,
            sstables,
            seq,
            vector_index,
            pending_compaction: false,
            max_memory_bytes,
        })
    }

    pub fn set(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        self.check_memory()?;
        self.wal.append_set(&key, &value)?;
        self.memtable.set(key, value);
        if self.memtable.is_full() {
            self.flush()?;
        }
        Ok(())
    }

    pub fn set_vec(&mut self, key: String, vector: Vec<f32>) -> Result<()> {
        self.check_memory()?;
        if let Some(expected) = self.vector_index.dim() {
            if vector.len() != expected {
                return Err(VektaError::DimensionMismatch {
                    expected,
                    got: vector.len(),
                });
            }
        }
        let storage_key = vec_storage_key(key.as_bytes());
        let bytes = serialize_vec(&vector);
        self.wal.append_set(&storage_key, &bytes)?;
        self.memtable.set(storage_key, bytes);
        self.vector_index.insert(key, vector);
        if self.memtable.is_full() {
            self.flush()?;
        }
        Ok(())
    }

    pub fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.wal.append_delete(key)?;
        self.memtable.delete(key);
        // Also remove from vector index and KV if a vec entry exists under this key.
        let storage_key = vec_storage_key(key);
        if self.memtable.get(&storage_key).is_some() || self.kv_exists_in_sstables(&storage_key) {
            self.wal.append_delete(&storage_key)?;
            self.memtable.delete(&storage_key);
        }
        self.vector_index.remove(&String::from_utf8_lossy(key));
        Ok(())
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        if let Some(v) = self.memtable.get(key) {
            return match v {
                Value::Data(d) => Ok(Some(d.clone())),
                Value::Tombstone => Ok(None),
            };
        }
        for table in self.sstables.iter().rev() {
            if let Some(v) = table.get(key)? {
                return match v {
                    Value::Data(d) => Ok(Some(d)),
                    Value::Tombstone => Ok(None),
                };
            }
        }
        Ok(None)
    }

    /// Return top-k keys by cosine similarity to `query`.
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<(String, f32)>> {
        self.vector_index.search(query, k)
    }

    pub fn flush(&mut self) -> Result<()> {
        if self.memtable.len() == 0 {
            return Ok(());
        }

        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let sst_path = self.data_dir.join(format!("L0-{seq:012}.sst"));

        let mut writer = SSTableWriter::create(&sst_path, self.memtable.len())?;
        for (key, value) in self.memtable.iter() {
            writer.write_entry(key, value)?;
        }
        writer.finish()?;

        let new_table = SSTable::open(&sst_path)?;
        tracing::info!(
            "Flushed MemTable → {} ({} keys)",
            sst_path.display(),
            new_table.num_keys()
        );

        self.sstables.push(new_table);
        self.memtable.clear();

        let wal_path = self.data_dir.join("wal.log");
        self.wal = {
            std::fs::remove_file(&wal_path).ok();
            Wal::open(&wal_path)?
        };

        if self.sstables.len() >= COMPACTION_THRESHOLD {
            self.pending_compaction = true;
        }

        Ok(())
    }

    /// Run compaction if one is pending. Called by the background compaction task.
    pub fn maybe_compact(&mut self) -> Result<bool> {
        if !self.pending_compaction {
            return Ok(false);
        }
        self.compact()?;
        self.pending_compaction = false;
        Ok(true)
    }

    fn compact(&mut self) -> Result<()> {
        tracing::info!("Starting compaction of {} SSTables…", self.sstables.len());

        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let out_path = self.data_dir.join(format!("L1-{seq:012}.sst"));

        let compacted = Compactor::compact(&self.sstables, &out_path)?;
        tracing::info!(
            "Compaction complete → {} ({} keys)",
            out_path.display(),
            compacted.num_keys()
        );

        for table in &self.sstables {
            std::fs::remove_file(table.path()).ok();
        }
        self.sstables = vec![compacted];

        Ok(())
    }

    fn load_sstables(data_dir: &Path) -> Result<Vec<SSTable>> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(data_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "sst").unwrap_or(false))
            .collect();
        paths.sort();
        paths.iter().map(|p| SSTable::open(p)).collect()
    }

    fn kv_exists_in_sstables(&self, key: &[u8]) -> bool {
        for table in self.sstables.iter().rev() {
            if let Ok(Some(Value::Data(_))) = table.get(key) {
                return true;
            }
        }
        false
    }

    pub fn stats(&self) -> EngineStats {
        let vec_dim = self.vector_index.dim().unwrap_or(0);
        let memory_bytes = self.memtable.size() + self.vector_index.len() * vec_dim * 4;
        EngineStats {
            memtable_size_bytes: self.memtable.size(),
            memtable_entries: self.memtable.len(),
            sstable_count: self.sstables.len(),
            sstable_entries: self.sstables.iter().map(|t| t.num_keys()).sum(),
            vector_count: self.vector_index.len(),
            memory_bytes,
            max_memory_bytes: self.max_memory_bytes,
        }
    }

    fn check_memory(&self) -> Result<()> {
        if self.max_memory_bytes == usize::MAX {
            return Ok(());
        }
        let vec_dim = self.vector_index.dim().unwrap_or(0);
        let used = self.memtable.size() + self.vector_index.len() * vec_dim * 4;
        if used >= self.max_memory_bytes {
            Err(VektaError::StorageFull)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct EngineStats {
    pub memtable_size_bytes: usize,
    pub memtable_entries: usize,
    pub sstable_count: usize,
    pub sstable_entries: usize,
    pub vector_count: usize,
    pub memory_bytes: usize,
    pub max_memory_bytes: usize,
}

fn vec_storage_key(user_key: &[u8]) -> Vec<u8> {
    let mut k = VEC_PREFIX.to_vec();
    k.extend_from_slice(user_key);
    k
}

pub fn serialize_vec(vec: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vec.len() * 4);
    for &f in vec {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    bytes
}

pub fn deserialize_vec(bytes: &[u8]) -> Result<Vec<f32>> {
    if bytes.len() % 4 != 0 {
        return Err(VektaError::Corruption("invalid vector byte length".into()));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn open(dir: &std::path::Path) -> Engine {
        Engine::open(dir).unwrap()
    }

    #[test]
    fn set_get_delete() {
        let dir = tempdir().unwrap();
        let mut e = open(dir.path());
        e.set(b"k".to_vec(), b"v".to_vec()).unwrap();
        assert_eq!(e.get(b"k").unwrap(), Some(b"v".to_vec()));
        e.delete(b"k").unwrap();
        assert_eq!(e.get(b"k").unwrap(), None);
    }

    #[test]
    fn missing_key_returns_none() {
        let dir = tempdir().unwrap();
        let e = open(dir.path());
        assert_eq!(e.get(b"nope").unwrap(), None);
    }

    #[test]
    fn set_vec_and_search() {
        let dir = tempdir().unwrap();
        let mut e = open(dir.path());
        e.set_vec("a".into(), vec![1.0, 0.0]).unwrap();
        e.set_vec("b".into(), vec![0.0, 1.0]).unwrap();
        let results = e.search(&[1.0, 0.0], 1).unwrap();
        assert_eq!(results[0].0, "a");
    }

    #[test]
    fn dimension_mismatch_is_rejected() {
        let dir = tempdir().unwrap();
        let mut e = open(dir.path());
        e.set_vec("a".into(), vec![1.0, 0.0]).unwrap();
        assert!(matches!(
            e.set_vec("b".into(), vec![1.0]),
            Err(VektaError::DimensionMismatch { .. })
        ));
    }

    #[test]
    fn delete_also_removes_vec_from_index() {
        let dir = tempdir().unwrap();
        let mut e = open(dir.path());
        e.set_vec("a".into(), vec![1.0, 0.0]).unwrap();
        e.delete(b"a").unwrap();
        assert!(e.search(&[1.0, 0.0], 5).unwrap().is_empty());
    }

    #[test]
    fn data_survives_flush_and_reopen() {
        let dir = tempdir().unwrap();
        {
            let mut e = open(dir.path());
            e.set(b"hello".to_vec(), b"world".to_vec()).unwrap();
            e.set_vec("doc".into(), vec![0.5, 0.5]).unwrap();
            e.flush().unwrap();
        }
        let e = open(dir.path());
        assert_eq!(e.get(b"hello").unwrap(), Some(b"world".to_vec()));
        let results = e.search(&[0.5, 0.5], 1).unwrap();
        assert_eq!(results[0].0, "doc");
    }

    #[test]
    fn stats_reflect_state() {
        let dir = tempdir().unwrap();
        let mut e = open(dir.path());
        e.set(b"k".to_vec(), b"v".to_vec()).unwrap();
        e.set_vec("v".into(), vec![1.0]).unwrap();
        let s = e.stats();
        assert_eq!(s.memtable_entries, 2); // regular key + vec namespace key
        assert_eq!(s.vector_count, 1);
    }
}

fn rebuild_vector_index(memtable: &MemTable, sstables: &[SSTable]) -> Result<VectorIndex> {
    let mut index = VectorIndex::new();

    // Apply SSTables oldest→newest, then memtable on top.
    for table in sstables {
        for (key, value) in table.scan_all()? {
            if !key.starts_with(VEC_PREFIX) {
                continue;
            }
            let user_key = String::from_utf8_lossy(&key[VEC_PREFIX.len()..]).into_owned();
            match value {
                Value::Data(bytes) => {
                    if let Ok(vec) = deserialize_vec(&bytes) {
                        index.insert(user_key, vec);
                    }
                }
                Value::Tombstone => {
                    index.remove(&user_key);
                }
            }
        }
    }

    for (key, value) in memtable.iter() {
        if !key.starts_with(VEC_PREFIX) {
            continue;
        }
        let user_key = String::from_utf8_lossy(&key[VEC_PREFIX.len()..]).into_owned();
        match value {
            Value::Data(bytes) => {
                if let Ok(vec) = deserialize_vec(bytes) {
                    index.insert(user_key, vec);
                }
            }
            Value::Tombstone => {
                index.remove(&user_key);
            }
        }
    }

    Ok(index)
}
