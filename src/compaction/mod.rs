// Phase 2/3: Compaction
// Merges multiple SSTables into one, dropping stale/deleted keys.
// Runs in a background thread sharing state via Arc<RwLock<...>>.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::storage::memtable::Value;
use crate::storage::sstable::{SSTable, SSTableWriter};

pub struct Compactor;

impl Compactor {
    /// Merge a list of SSTables (oldest first) into a single new SSTable.
    /// Newer tables win on key conflicts; tombstones are dropped (GC'd).
    pub fn compact(
        tables: &[SSTable],
        output_path: impl AsRef<Path>,
    ) -> Result<SSTable> {
        use std::collections::BTreeMap;

        // Merge: iterate oldest to newest so newer values overwrite older ones.
        let mut merged: BTreeMap<Vec<u8>, Value> = BTreeMap::new();

        for table in tables {
            for (key, value) in table.scan_all()? {
                merged.insert(key, value);
            }
        }

        let mut writer = SSTableWriter::create(&output_path, merged.len())?;
        for (key, value) in &merged {
            // Drop tombstones during full compaction — the key is gone for good.
            if matches!(value, Value::Tombstone) {
                continue;
            }
            writer.write_entry(key, value)?;
        }
        writer.finish()?;

        SSTable::open(output_path)
    }

    pub fn generate_path(data_dir: &Path, level: u32, seq: u64) -> PathBuf {
        data_dir.join(format!("L{level}-{seq:012}.sst"))
    }
}
