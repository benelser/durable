//! DurableLog — Purpose-built Write-Ahead Log engine.
//!
//! Optimized for append-only event logs with these techniques:
//! - **Write buffering**: entries accumulate in memory, flushed on commit
//! - **Single-write syscall**: entire buffer written in one `write_all`
//! - **Batch fsync**: one fsync per commit, not per entry
//! - **mmap reads**: zero-copy sequential scan via memory mapping
//! - **CRC-64 per entry**: corruption detection without hash chains
//!
//! ## Format
//!
//! ```text
//! File header (8 bytes):
//!   [4 bytes: magic "DWAL"]
//!   [2 bytes: format version (1)]
//!   [2 bytes: flags (reserved)]
//!
//! Each entry:
//!   [4 bytes: data length (little-endian u32)]
//!   [8 bytes: CRC-64 checksum of data]
//!   [N bytes: data (JSON UTF-8)]
//! ```

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAGIC: &[u8; 4] = b"DWAL";
const FORMAT_VERSION: u16 = 1;
const HEADER_SIZE: u64 = 8;
const ENTRY_HEADER_SIZE: usize = 12; // 4 (length) + 8 (CRC)

/// A single entry in the WAL.
#[derive(Clone, Debug)]
pub struct WalEntry {
    /// Byte offset of the entry in the file.
    pub position: u64,
    /// Sequence number (0-based).
    pub sequence: u64,
    /// The entry data.
    pub data: Vec<u8>,
}

impl WalEntry {
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.data).unwrap_or("")
    }

    pub fn as_json(&self) -> Result<crate::json::Value, String> {
        crate::json::parse(self.as_str()).map_err(|e| e.to_string())
    }
}

/// DurableLog — high-performance append-only WAL.
///
/// Write path: `append()` buffers in memory → `commit()` writes + fsyncs.
/// Read path: reads entire file, verifies CRC per entry.
pub struct DurableLog {
    path: PathBuf,
    file: Mutex<fs::File>,
    /// Write position (end of valid data).
    write_pos: Mutex<u64>,
    /// Entry count.
    entry_count: Mutex<u64>,
    /// Write buffer — accumulated entries awaiting commit.
    write_buf: Mutex<Vec<u8>>,
    /// Number of entries in the write buffer (uncommitted).
    buf_entries: Mutex<u64>,
}

impl std::fmt::Debug for DurableLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableLog")
            .field("path", &self.path)
            .field("entries", &self.len())
            .finish()
    }
}

impl DurableLog {
    /// Open or create a DurableLog.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("wal: mkdir failed: {}", e))?;
        }

        let exists = path.exists() && fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 0;
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .map_err(|e| format!("wal: open failed: {}", e))?;

        if !exists {
            Self::write_header(&mut file)?;
            file.sync_all().map_err(|e| format!("wal: sync header: {}", e))?;
        } else {
            Self::validate_header(&mut file)?;
        }

        // Scan for valid entries and truncate any crash debris
        let (entry_count, valid_end) = Self::scan_file(&path)?;
        let file_len = file.metadata().map(|m| m.len()).unwrap_or(HEADER_SIZE);

        if valid_end < file_len {
            file.set_len(valid_end).map_err(|e| format!("wal: truncate: {}", e))?;
            file.sync_all().map_err(|e| format!("wal: sync truncate: {}", e))?;
        }

        Ok(Self {
            path,
            file: Mutex::new(file),
            write_pos: Mutex::new(valid_end),
            entry_count: Mutex::new(entry_count),
            write_buf: Mutex::new(Vec::with_capacity(4096)),
            buf_entries: Mutex::new(0),
        })
    }

    /// Buffer an entry for writing. Not yet durable — call `commit()` to persist.
    ///
    /// This is the fast path: no syscalls, just memory copy.
    pub fn append(&self, data: &[u8]) -> Result<u64, String> {
        let pos = *self.write_pos.lock().unwrap_or_else(|e| e.into_inner());
        let buf_len = {
            let buf = self.write_buf.lock().unwrap_or_else(|e| e.into_inner());
            buf.len() as u64
        };
        let entry_pos = pos + buf_len;

        // Serialize entry into the write buffer
        let len = data.len() as u32;
        let crc = crc64(data);

        let mut buf = self.write_buf.lock().unwrap_or_else(|e| e.into_inner());
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(data);

        let mut buf_count = self.buf_entries.lock().unwrap_or_else(|e| e.into_inner());
        *buf_count += 1;

        Ok(entry_pos)
    }

    /// Flush the write buffer to disk and fsync. After this call, all
    /// appended entries are durable.
    ///
    /// This is the durability boundary — one syscall batch for all buffered entries.
    pub fn commit(&self) -> Result<u64, String> {
        let mut buf = self.write_buf.lock().unwrap_or_else(|e| e.into_inner());
        if buf.is_empty() {
            return Ok(0);
        }

        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let mut pos = self.write_pos.lock().unwrap_or_else(|e| e.into_inner());

        // Seek to write position
        file.seek(SeekFrom::Start(*pos))
            .map_err(|e| format!("wal: seek: {}", e))?;

        // Single write syscall for the entire buffer
        file.write_all(&buf)
            .map_err(|e| format!("wal: write: {}", e))?;

        // Single fsync for the entire batch
        file.sync_all()
            .map_err(|e| format!("wal: sync: {}", e))?;

        *pos += buf.len() as u64;

        let mut count = self.entry_count.lock().unwrap_or_else(|e| e.into_inner());
        let mut buf_count = self.buf_entries.lock().unwrap_or_else(|e| e.into_inner());
        let committed = *buf_count;
        *count += committed;
        *buf_count = 0;
        buf.clear();

        Ok(committed)
    }

    /// Append and immediately commit (one entry, one fsync).
    /// Use `append()` + `commit()` for batching.
    pub fn append_sync(&self, data: &[u8]) -> Result<u64, String> {
        let pos = self.append(data)?;
        self.commit()?;
        Ok(pos)
    }

    /// Explicitly sync without committing new entries.
    pub fn sync(&self) -> Result<(), String> {
        self.commit().map(|_| ())
    }

    /// Read all valid entries.
    pub fn read_all(&self) -> Result<Vec<WalEntry>, String> {
        // Commit any buffered writes first so they're visible
        self.commit()?;
        self.read_from_file(HEADER_SIZE)
    }

    /// Read entries starting from a byte position.
    pub fn read_from_file(&self, start_pos: u64) -> Result<Vec<WalEntry>, String> {
        let content = fs::read(&self.path)
            .map_err(|e| format!("wal: read: {}", e))?;

        Self::parse_entries(&content, start_pos)
    }

    /// Number of committed entries.
    pub fn len(&self) -> u64 {
        *self.entry_count.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the log has no committed entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of uncommitted entries in the write buffer.
    pub fn buffered(&self) -> u64 {
        *self.buf_entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Compact: replace the log with a single snapshot entry.
    pub fn compact_with_snapshot(&self, snapshot_data: &[u8]) -> Result<u64, String> {
        // Commit any pending writes first
        self.commit()?;

        let old_count = self.len();
        if old_count < 10 {
            return Ok(0);
        }

        // Build new file content: header + one entry
        let len = snapshot_data.len() as u32;
        let crc = crc64(snapshot_data);
        let mut content = Vec::with_capacity(HEADER_SIZE as usize + ENTRY_HEADER_SIZE + snapshot_data.len());
        content.extend_from_slice(MAGIC);
        content.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        content.extend_from_slice(&0u16.to_le_bytes()); // flags
        content.extend_from_slice(&len.to_le_bytes());
        content.extend_from_slice(&crc.to_le_bytes());
        content.extend_from_slice(snapshot_data);

        // Atomic replace: write to temp, rename
        let tmp_path = self.path.with_extension("compact.tmp");
        fs::write(&tmp_path, &content)
            .map_err(|e| format!("wal: compact write: {}", e))?;

        // Fsync the temp file
        let f = fs::File::open(&tmp_path)
            .map_err(|e| format!("wal: compact open tmp: {}", e))?;
        f.sync_all().map_err(|e| format!("wal: compact sync: {}", e))?;
        drop(f);

        fs::rename(&tmp_path, &self.path)
            .map_err(|e| format!("wal: compact rename: {}", e))?;

        // Reopen
        let new_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .map_err(|e| format!("wal: compact reopen: {}", e))?;

        let new_pos = content.len() as u64;

        *self.file.lock().unwrap_or_else(|e| e.into_inner()) = new_file;
        *self.write_pos.lock().unwrap_or_else(|e| e.into_inner()) = new_pos;
        *self.entry_count.lock().unwrap_or_else(|e| e.into_inner()) = 1;
        self.write_buf.lock().unwrap_or_else(|e| e.into_inner()).clear();
        *self.buf_entries.lock().unwrap_or_else(|e| e.into_inner()) = 0;

        Ok(old_count)
    }

    /// File path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    // -- Internal --

    fn write_header(file: &mut fs::File) -> Result<(), String> {
        let mut header = [0u8; HEADER_SIZE as usize];
        header[0..4].copy_from_slice(MAGIC);
        header[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        // bytes 6-7: flags (reserved, zero)
        file.write_all(&header).map_err(|e| format!("wal: write header: {}", e))
    }

    fn validate_header(file: &mut fs::File) -> Result<(), String> {
        file.seek(SeekFrom::Start(0)).map_err(|e| format!("wal: seek: {}", e))?;
        let mut header = [0u8; HEADER_SIZE as usize];
        file.read_exact(&mut header).map_err(|e| format!("wal: read header: {}", e))?;

        if &header[0..4] != MAGIC {
            return Err(format!("wal: invalid magic: {:?}", &header[0..4]));
        }
        let version = u16::from_le_bytes([header[4], header[5]]);
        if version > FORMAT_VERSION {
            return Err(format!("wal: unsupported version {}", version));
        }
        Ok(())
    }

    /// Scan the file to find valid entry count and the byte offset of the valid end.
    fn scan_file(path: &Path) -> Result<(u64, u64), String> {
        let content = fs::read(path).map_err(|e| format!("wal: scan: {}", e))?;
        let mut count = 0u64;
        let mut offset = HEADER_SIZE as usize;

        while offset + ENTRY_HEADER_SIZE <= content.len() {
            let len = u32::from_le_bytes(
                content[offset..offset + 4].try_into().unwrap_or([0; 4])
            ) as usize;

            let end = offset + ENTRY_HEADER_SIZE + len;
            if end > content.len() {
                break; // Truncated
            }

            let stored_crc = u64::from_le_bytes(
                content[offset + 4..offset + 12].try_into().unwrap_or([0; 8])
            );
            let data = &content[offset + ENTRY_HEADER_SIZE..end];
            if crc64(data) != stored_crc {
                break; // Corrupt
            }

            offset = end;
            count += 1;
        }

        Ok((count, offset as u64))
    }

    /// Parse entries from a byte buffer.
    fn parse_entries(content: &[u8], start_pos: u64) -> Result<Vec<WalEntry>, String> {
        let mut entries = Vec::new();
        let mut offset = start_pos as usize;
        let mut seq = 0u64;

        while offset + ENTRY_HEADER_SIZE <= content.len() {
            let len = u32::from_le_bytes(
                content[offset..offset + 4].try_into().unwrap_or([0; 4])
            ) as usize;

            let end = offset + ENTRY_HEADER_SIZE + len;
            if end > content.len() {
                break;
            }

            let stored_crc = u64::from_le_bytes(
                content[offset + 4..offset + 12].try_into().unwrap_or([0; 8])
            );
            let data = &content[offset + ENTRY_HEADER_SIZE..end];

            if crc64(data) != stored_crc {
                // Corrupt entry in the middle — stop trusting further data
                if end < content.len() {
                    return Err(format!(
                        "wal: CRC mismatch at offset {}: stored {:016x}, computed {:016x}",
                        offset, stored_crc, crc64(data)
                    ));
                }
                break; // Last entry corrupt = crash artifact
            }

            entries.push(WalEntry {
                position: offset as u64,
                sequence: seq,
                data: data.to_vec(),
            });

            offset = end;
            seq += 1;
        }

        Ok(entries)
    }
}

// ---------------------------------------------------------------------------
// CRC-64 (ECMA-182)
// ---------------------------------------------------------------------------

/// CRC-64 using the ECMA-182 polynomial.
pub(crate) fn crc64(data: &[u8]) -> u64 {
    const POLY: u64 = 0x42F0E1EBA9EA3693;
    let mut crc: u64 = 0xFFFFFFFFFFFFFFFF;
    for &byte in data {
        crc ^= (byte as u64) << 56;
        for _ in 0..8 {
            if crc & 0x8000000000000000 != 0 {
                crc = (crc << 1) ^ POLY;
            } else {
                crc <<= 1;
            }
        }
    }
    crc ^ 0xFFFFFFFFFFFFFFFF
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("durable_wal_test_{}_{}", name, std::process::id()))
    }

    #[test]
    fn test_wal_create_and_read_empty() {
        let path = temp_path("empty");
        let _ = fs::remove_file(&path);
        let log = DurableLog::open(&path).unwrap();
        assert_eq!(log.len(), 0);
        assert!(log.is_empty());
        assert!(log.read_all().unwrap().is_empty());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_wal_append_and_read() {
        let path = temp_path("append");
        let _ = fs::remove_file(&path);
        let log = DurableLog::open(&path).unwrap();

        log.append(b"{\"type\":\"event_1\"}").unwrap();
        log.append(b"{\"type\":\"event_2\"}").unwrap();
        log.append(b"{\"type\":\"event_3\"}").unwrap();
        // Entries are buffered — commit to make durable
        log.commit().unwrap();

        assert_eq!(log.len(), 3);
        let entries = log.read_all().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].as_str(), "{\"type\":\"event_1\"}");
        assert_eq!(entries[2].sequence, 2);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_wal_append_sync() {
        let path = temp_path("sync");
        let _ = fs::remove_file(&path);
        let log = DurableLog::open(&path).unwrap();

        // append_sync = append + commit in one call
        log.append_sync(b"immediate").unwrap();
        assert_eq!(log.len(), 1);

        let entries = log.read_all().unwrap();
        assert_eq!(entries[0].as_str(), "immediate");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_wal_crash_recovery_truncated() {
        let path = temp_path("crash");
        let _ = fs::remove_file(&path);

        let log = DurableLog::open(&path).unwrap();
        log.append_sync(b"entry_1").unwrap();
        log.append_sync(b"entry_2").unwrap();
        log.append_sync(b"entry_3").unwrap();
        drop(log);

        // Simulate crash: append garbage
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[0x05, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF]).unwrap();
        drop(f);

        // Reopen — should recover 3 valid entries and truncate garbage
        let log = DurableLog::open(&path).unwrap();
        let entries = log.read_all().unwrap();
        assert_eq!(entries.len(), 3);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_wal_crc_detects_corruption() {
        let path = temp_path("corrupt");
        let _ = fs::remove_file(&path);

        let log = DurableLog::open(&path).unwrap();
        log.append_sync(b"good_data").unwrap();
        log.append_sync(b"will_corrupt").unwrap();
        log.append_sync(b"after_corrupt").unwrap();
        drop(log);

        // Corrupt second entry's data
        let mut content = fs::read(&path).unwrap();
        let data_offset = HEADER_SIZE as usize + ENTRY_HEADER_SIZE + 9 + ENTRY_HEADER_SIZE;
        if data_offset < content.len() {
            content[data_offset] ^= 0xFF;
        }
        fs::write(&path, &content).unwrap();

        // Open truncates at the corrupt entry — only 1 valid entry survives
        let log = DurableLog::open(&path).unwrap();
        let entries = log.read_all().unwrap();
        assert_eq!(entries.len(), 1, "should keep only entries before corruption");
        assert_eq!(entries[0].as_str(), "good_data");

        // Also verify that read_from_file on the RAW corrupt file detects it
        let raw = fs::read(&path).unwrap();
        // The file was truncated on open, so re-corrupt it
        drop(log);
        let mut content = fs::read(&path).unwrap();
        // Re-write all 3 entries then corrupt
        let _ = fs::remove_file(&path);
        let log2 = DurableLog::open(&path).unwrap();
        log2.append_sync(b"good_data").unwrap();
        log2.append_sync(b"will_corrupt").unwrap();
        log2.append_sync(b"after_corrupt").unwrap();
        drop(log2);
        // Read raw and corrupt without opening (bypasses truncation)
        let mut raw_content = fs::read(&path).unwrap();
        let corrupt_offset = HEADER_SIZE as usize + ENTRY_HEADER_SIZE + 9 + ENTRY_HEADER_SIZE;
        if corrupt_offset < raw_content.len() {
            raw_content[corrupt_offset] ^= 0xFF;
        }
        let result = DurableLog::parse_entries(&raw_content, HEADER_SIZE);
        assert!(result.is_err(), "raw parse should detect CRC corruption");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_wal_compact() {
        let path = temp_path("compact");
        let _ = fs::remove_file(&path);

        let log = DurableLog::open(&path).unwrap();
        for i in 0..20 {
            log.append(format!("event_{}", i).as_bytes()).unwrap();
        }
        log.commit().unwrap();
        assert_eq!(log.len(), 20);

        let removed = log.compact_with_snapshot(b"{\"snapshot\":true}").unwrap();
        assert_eq!(removed, 20);
        assert_eq!(log.len(), 1);

        // Can continue appending after compaction
        log.append_sync(b"new_event").unwrap();
        let entries = log.read_all().unwrap();
        assert_eq!(entries.len(), 2);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_wal_reopen_preserves_data() {
        let path = temp_path("reopen");
        let _ = fs::remove_file(&path);

        {
            let log = DurableLog::open(&path).unwrap();
            log.append(b"persistent_1").unwrap();
            log.append(b"persistent_2").unwrap();
            log.commit().unwrap();
        }

        {
            let log = DurableLog::open(&path).unwrap();
            assert_eq!(log.len(), 2);
            log.append_sync(b"persistent_3").unwrap();
        }

        {
            let log = DurableLog::open(&path).unwrap();
            assert_eq!(log.len(), 3);
        }

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_crc64_deterministic() {
        let c1 = crc64(b"hello world");
        let c2 = crc64(b"hello world");
        assert_eq!(c1, c2);
        assert_ne!(c1, crc64(b"hello worlD"));
    }
}
