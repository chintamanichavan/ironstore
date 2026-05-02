// Sorted String Table (SSTable) — immutable on-disk key-value file.
//
// File layout:
//   [data block:  N × (key_len:u32 | key | val_len:u32 | val | tombstone:u8)]
//   [bloom filter: m:u32 | bits:bytes]
//   [index block:  N × (key_len:u32 | key | offset:u64)]
//   [footer:       bloom_offset:u64 | index_offset:u64 | num_entries:u32 | magic:u32]  ← 24 bytes
//
// Bloom filter skips the file seek entirely for keys definitely not in the table.
// The in-memory index gives O(log n) seeks for keys that pass the bloom check.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::error::{Result, VektaError};
use crate::storage::bloom::BloomFilter;
use crate::storage::memtable::Value;

const MAGIC: u32 = 0x564B544C; // "VKTL"

pub struct SSTableWriter {
    writer: BufWriter<File>,
    index: Vec<(Vec<u8>, u64)>,
    bloom: BloomFilter,
    current_offset: u64,
}

impl SSTableWriter {
    /// `capacity` is the expected number of entries — used to pre-size the bloom filter.
    /// Pass `memtable.len()` when flushing, `merged.len()` when compacting.
    pub fn create(path: impl AsRef<Path>, capacity: usize) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            index: Vec::new(),
            bloom: BloomFilter::new(capacity.max(1)),
            current_offset: 0,
        })
    }

    pub fn write_entry(&mut self, key: &[u8], value: &Value) -> Result<()> {
        self.bloom.insert(key);
        self.index.push((key.to_vec(), self.current_offset));

        self.writer.write_all(&(key.len() as u32).to_le_bytes())?;
        self.writer.write_all(key)?;
        self.current_offset += 4 + key.len() as u64;

        match value {
            Value::Data(v) => {
                self.writer.write_all(&(v.len() as u32).to_le_bytes())?;
                self.writer.write_all(v)?;
                self.writer.write_all(&[0u8])?;
                self.current_offset += 4 + v.len() as u64 + 1;
            }
            Value::Tombstone => {
                self.writer.write_all(&0u32.to_le_bytes())?;
                self.writer.write_all(&[1u8])?;
                self.current_offset += 5;
            }
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        let bloom_offset = self.current_offset;
        let bloom_bytes = self.bloom.to_bytes();
        self.writer.write_all(&bloom_bytes)?;

        let index_offset = bloom_offset + bloom_bytes.len() as u64;

        for (key, offset) in &self.index {
            self.writer.write_all(&(key.len() as u32).to_le_bytes())?;
            self.writer.write_all(key)?;
            self.writer.write_all(&offset.to_le_bytes())?;
        }

        // Footer: bloom_offset(8) | index_offset(8) | num_entries(4) | magic(4) = 24 bytes
        self.writer.write_all(&bloom_offset.to_le_bytes())?;
        self.writer.write_all(&index_offset.to_le_bytes())?;
        self.writer.write_all(&(self.index.len() as u32).to_le_bytes())?;
        self.writer.write_all(&MAGIC.to_le_bytes())?;
        self.writer.flush()?;
        Ok(())
    }
}

pub struct SSTable {
    path: PathBuf,
    index: BTreeMap<Vec<u8>, u64>,
    bloom: BloomFilter,
}

impl SSTable {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;

        // Read footer (last 24 bytes).
        file.seek(SeekFrom::End(-24))?;
        let mut footer = [0u8; 24];
        file.read_exact(&mut footer)?;

        let bloom_offset = u64::from_le_bytes(footer[0..8].try_into().unwrap());
        let index_offset = u64::from_le_bytes(footer[8..16].try_into().unwrap());
        let num_entries = u32::from_le_bytes(footer[16..20].try_into().unwrap()) as usize;
        let magic = u32::from_le_bytes(footer[20..24].try_into().unwrap());

        if magic != MAGIC {
            return Err(VektaError::Corruption(format!(
                "Bad magic in SSTable {path:?}: expected {MAGIC:#010x}, got {magic:#010x}"
            )));
        }

        // Load bloom filter.
        let bloom_len = (index_offset - bloom_offset) as usize;
        file.seek(SeekFrom::Start(bloom_offset))?;
        let mut bloom_bytes = vec![0u8; bloom_len];
        file.read_exact(&mut bloom_bytes)?;
        let bloom = BloomFilter::from_bytes(&bloom_bytes)
            .ok_or_else(|| VektaError::Corruption("Invalid bloom filter in SSTable".into()))?;

        // Load index block.
        file.seek(SeekFrom::Start(index_offset))?;
        let mut index = BTreeMap::new();
        for _ in 0..num_entries {
            let key_len = read_u32_file(&mut file)? as usize;
            let mut key = vec![0u8; key_len];
            file.read_exact(&mut key)?;
            let mut off_buf = [0u8; 8];
            file.read_exact(&mut off_buf)?;
            index.insert(key, u64::from_le_bytes(off_buf));
        }

        Ok(Self { path, index, bloom })
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Value>> {
        // Bloom filter: definite miss — skip all I/O.
        if !self.bloom.might_contain(key) {
            return Ok(None);
        }
        let Some(&offset) = self.index.get(key) else {
            return Ok(None);
        };
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(offset))?;

        let key_len = read_u32_file(&mut file)? as usize;
        file.seek(SeekFrom::Current(key_len as i64))?;

        let val_len = read_u32_file(&mut file)? as usize;
        let mut tombstone_flag = [0u8; 1];

        if val_len == 0 {
            file.read_exact(&mut tombstone_flag)?;
            return Ok(Some(if tombstone_flag[0] == 1 {
                Value::Tombstone
            } else {
                Value::Data(vec![])
            }));
        }

        let mut value = vec![0u8; val_len];
        file.read_exact(&mut value)?;
        file.read_exact(&mut tombstone_flag)?;
        Ok(Some(Value::Data(value)))
    }

    pub fn scan_all(&self) -> Result<Vec<(Vec<u8>, Value)>> {
        let mut file = File::open(&self.path)?;
        let mut entries = Vec::new();

        for (key, &offset) in &self.index {
            file.seek(SeekFrom::Start(offset))?;
            let key_len = read_u32_file(&mut file)? as usize;
            file.seek(SeekFrom::Current(key_len as i64))?;

            let val_len = read_u32_file(&mut file)? as usize;
            let mut tombstone_flag = [0u8; 1];

            if val_len == 0 {
                file.read_exact(&mut tombstone_flag)?;
                entries.push((
                    key.clone(),
                    if tombstone_flag[0] == 1 {
                        Value::Tombstone
                    } else {
                        Value::Data(vec![])
                    },
                ));
                continue;
            }

            let mut value = vec![0u8; val_len];
            file.read_exact(&mut value)?;
            file.read_exact(&mut tombstone_flag)?;
            entries.push((key.clone(), Value::Data(value)));
        }
        Ok(entries)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn num_keys(&self) -> usize {
        self.index.len()
    }
}

fn read_u32_file(file: &mut File) -> Result<u32> {
    let mut buf = [0u8; 4];
    file.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_table(path: &Path, entries: &[(&[u8], Value)]) {
        let mut w = SSTableWriter::create(path, entries.len()).unwrap();
        for (k, v) in entries {
            w.write_entry(k, v).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn get_existing_key() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("t.sst");
        write_table(&p, &[(b"hello", Value::Data(b"world".to_vec()))]);
        let t = SSTable::open(&p).unwrap();
        assert_eq!(t.get(b"hello").unwrap(), Some(Value::Data(b"world".to_vec())));
    }

    #[test]
    fn get_missing_key_bloom_skips_io() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("t.sst");
        write_table(&p, &[(b"a", Value::Data(b"1".to_vec()))]);
        let t = SSTable::open(&p).unwrap();
        assert_eq!(t.get(b"definitely_not_there").unwrap(), None);
    }

    #[test]
    fn tombstone_round_trips() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("t.sst");
        write_table(&p, &[(b"gone", Value::Tombstone)]);
        let t = SSTable::open(&p).unwrap();
        assert_eq!(t.get(b"gone").unwrap(), Some(Value::Tombstone));
    }

    #[test]
    fn scan_all_returns_sorted() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("t.sst");
        write_table(
            &p,
            &[
                (b"a", Value::Data(b"1".to_vec())),
                (b"b", Value::Data(b"2".to_vec())),
                (b"c", Value::Tombstone),
            ],
        );
        let t = SSTable::open(&p).unwrap();
        let entries = t.scan_all().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].0, b"a");
    }
}
