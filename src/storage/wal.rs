// Write-Ahead Log.
// Every mutation is appended before touching the MemTable.
// On crash recovery, WAL is replayed to rebuild the MemTable.
//
// Record format (all fields little-endian):
//   [op: u8][key_len: u32][key]([val_len: u32][val])[crc32: u32]
//   op: 0x01 = Set, 0x02 = Delete
//
// CRC covers every byte in the record except the trailing CRC field itself.
// Set VEKTALITE_SYNC=1 to call fdatasync after every write (durable but slower).

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use crc32fast::Hasher as Crc32Hasher;

use crate::error::{Result, VektaError};

#[derive(Debug)]
pub enum WalRecord {
    Set { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

pub struct Wal {
    #[allow(dead_code)]
    path: PathBuf,
    writer: BufWriter<File>,
    sync_writes: bool,
}

impl Wal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_ref())?;
        let sync_writes = std::env::var("VEKTALITE_SYNC").map_or(false, |v| v == "1");
        Ok(Self {
            path: path.as_ref().to_path_buf(),
            writer: BufWriter::new(file),
            sync_writes,
        })
    }

    pub fn append_set(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        let mut crc = Crc32Hasher::new();
        crc.update(&[0x01]);
        crc.update(&(key.len() as u32).to_le_bytes());
        crc.update(key);
        crc.update(&(value.len() as u32).to_le_bytes());
        crc.update(value);

        self.writer.write_all(&[0x01])?;
        self.writer.write_all(&(key.len() as u32).to_le_bytes())?;
        self.writer.write_all(key)?;
        self.writer.write_all(&(value.len() as u32).to_le_bytes())?;
        self.writer.write_all(value)?;
        self.writer.write_all(&crc.finalize().to_le_bytes())?;
        self.flush_to_disk()
    }

    pub fn append_delete(&mut self, key: &[u8]) -> Result<()> {
        let mut crc = Crc32Hasher::new();
        crc.update(&[0x02]);
        crc.update(&(key.len() as u32).to_le_bytes());
        crc.update(key);

        self.writer.write_all(&[0x02])?;
        self.writer.write_all(&(key.len() as u32).to_le_bytes())?;
        self.writer.write_all(key)?;
        self.writer.write_all(&crc.finalize().to_le_bytes())?;
        self.flush_to_disk()
    }

    fn flush_to_disk(&mut self) -> Result<()> {
        self.writer.flush()?;
        if self.sync_writes {
            self.writer.get_ref().sync_data()?;
        }
        Ok(())
    }

    pub fn replay(path: impl AsRef<Path>) -> Result<Vec<WalRecord>> {
        let mut file = match File::open(path.as_ref()) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;

        let mut records = Vec::new();
        let mut cursor = 0usize;

        while cursor < data.len() {
            let record_start = cursor;

            if cursor + 1 > data.len() {
                return Err(VektaError::Corruption("WAL truncated at op byte".into()));
            }
            let op = data[cursor];
            cursor += 1;

            let key_len = read_u32(&data, cursor)? as usize;
            cursor += 4;
            if cursor + key_len > data.len() {
                return Err(VektaError::Corruption("WAL truncated reading key".into()));
            }
            let key = data[cursor..cursor + key_len].to_vec();
            cursor += key_len;

            let record = match op {
                0x01 => {
                    let val_len = read_u32(&data, cursor)? as usize;
                    cursor += 4;
                    if cursor + val_len > data.len() {
                        return Err(VektaError::Corruption("WAL truncated reading value".into()));
                    }
                    let value = data[cursor..cursor + val_len].to_vec();
                    cursor += val_len;
                    WalRecord::Set { key, value }
                }
                0x02 => WalRecord::Delete { key },
                _ => return Err(VektaError::Corruption(format!("Unknown WAL op: {op}"))),
            };

            // Verify CRC covers everything up to (not including) the CRC field.
            if cursor + 4 > data.len() {
                return Err(VektaError::Corruption("WAL truncated reading CRC".into()));
            }
            let stored_crc = u32::from_le_bytes(data[cursor..cursor + 4].try_into().unwrap());
            cursor += 4;

            let mut crc = Crc32Hasher::new();
            crc.update(&data[record_start..cursor - 4]);
            let computed_crc = crc.finalize();
            if computed_crc != stored_crc {
                return Err(VektaError::Corruption(format!(
                    "WAL CRC mismatch at byte {record_start}: expected {stored_crc:#010x}, got {computed_crc:#010x}"
                )));
            }

            records.push(record);
        }
        Ok(records)
    }
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    if offset + 4 > data.len() {
        return Err(VektaError::Corruption("WAL truncated reading u32".into()));
    }
    Ok(u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn round_trip_set_and_delete() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wal.log");

        let mut wal = Wal::open(&path).unwrap();
        wal.append_set(b"hello", b"world").unwrap();
        wal.append_delete(b"foo").unwrap();
        wal.append_set(b"k", b"v").unwrap();
        drop(wal);

        let records = Wal::replay(&path).unwrap();
        assert_eq!(records.len(), 3);
        assert!(matches!(&records[0], WalRecord::Set { key, value } if key == b"hello" && value == b"world"));
        assert!(matches!(&records[1], WalRecord::Delete { key } if key == b"foo"));
        assert!(matches!(&records[2], WalRecord::Set { key, .. } if key == b"k"));
    }

    #[test]
    fn detects_crc_corruption() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wal.log");

        let mut wal = Wal::open(&path).unwrap();
        wal.append_set(b"key", b"val").unwrap();
        drop(wal);

        // Flip a bit in the middle of the record.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[3] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        assert!(matches!(
            Wal::replay(&path),
            Err(VektaError::Corruption(_))
        ));
    }
}
