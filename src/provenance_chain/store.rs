//! Append-only persistence for the provenance hash chain (Issue #3351).
//!
//! Layout under the chain directory:
//! - `chain.log` — a header (`ALCHAIN1` magic + format-version `u32`) followed by
//!   length-prefixed [`ChainTxRecord`]s (`u32` big-endian frame length, then the
//!   record's binary payload).
//! - `head` — an atomically-rewritten [`ChainHead`] checkpoint (`ALCHEAD1` magic +
//!   version + payload), enabling fast startup without replaying the whole log.
//!
//! Torn-tail tolerance: [`ChainStore::load`] returns the longest valid prefix of
//! records. A partially-written trailing frame (crash mid-append) is dropped
//! rather than failing the load, mirroring WAL recovery.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::config::ChainFsyncMode;
use super::error::{ChainError, ChainResult};
use super::record::{ChainHead, ChainTxRecord};

const LOG_MAGIC: &[u8; 8] = b"ALCHAIN1";
const HEAD_MAGIC: &[u8; 8] = b"ALCHEAD1";
const FORMAT_VERSION: u32 = 1;
const HEADER_LEN: usize = 12; // magic(8) + version(4)

/// Append-only store for the sealed portion of a provenance hash chain.
pub struct ChainStore {
    log_path: PathBuf,
    head_path: PathBuf,
    fsync: ChainFsyncMode,
    /// Append handle guarded for cross-thread appends.
    append: Mutex<File>,
}

impl ChainStore {
    /// Open (creating if necessary) the chain store under `dir`.
    ///
    /// Creates the directory and writes a fresh log header if the log does not
    /// yet exist. If the log exists, its header is validated.
    pub fn open(dir: impl AsRef<Path>, fsync: ChainFsyncMode) -> ChainResult<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let log_path = dir.join("chain.log");
        let head_path = dir.join("head");

        if !log_path.exists() {
            // Create with header.
            let mut f = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&log_path)?;
            let mut header = Vec::with_capacity(HEADER_LEN);
            header.extend_from_slice(LOG_MAGIC);
            header.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
            f.write_all(&header)?;
            f.sync_all()?;
        } else {
            // Validate the existing header eagerly.
            let mut f = File::open(&log_path)?;
            let mut header = [0u8; HEADER_LEN];
            f.read_exact(&mut header)
                .map_err(|_| ChainError::HeaderMismatch("chain log too short for header".into()))?;
            validate_header(&header)?;
        }

        let append = OpenOptions::new().append(true).open(&log_path)?;
        Ok(ChainStore {
            log_path,
            head_path,
            fsync,
            append: Mutex::new(append),
        })
    }

    /// Append one sealed transaction record, flushing per the configured mode.
    pub fn append(&self, record: &ChainTxRecord) -> ChainResult<()> {
        let payload = record.encode();
        let len = u32::try_from(payload.len())
            .map_err(|_| ChainError::BadFraming("record too large to frame".into()))?;

        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&len.to_be_bytes());
        framed.extend_from_slice(&payload);

        let mut guard = self
            .append
            .lock()
            .map_err(|_| ChainError::Io("chain append lock poisoned".into()))?;
        guard.write_all(&framed)?;
        match self.fsync {
            ChainFsyncMode::PerTransaction => guard.sync_all()?,
            ChainFsyncMode::Batched | ChainFsyncMode::Never => {}
        }
        Ok(())
    }

    /// Force any buffered/os-cached writes to durable storage. Useful for
    /// [`ChainFsyncMode::Batched`] callers that batch appends and flush
    /// explicitly.
    pub fn flush(&self) -> ChainResult<()> {
        let guard = self
            .append
            .lock()
            .map_err(|_| ChainError::Io("chain append lock poisoned".into()))?;
        guard.sync_all()?;
        Ok(())
    }

    /// Load all valid records from the log, tolerating a torn trailing frame.
    pub fn load(&self) -> ChainResult<Vec<ChainTxRecord>> {
        let bytes = std::fs::read(&self.log_path)?;
        if bytes.len() < HEADER_LEN {
            return Err(ChainError::HeaderMismatch(
                "chain log too short for header".into(),
            ));
        }
        validate_header(&bytes[..HEADER_LEN])?;

        let mut records = Vec::new();
        let mut pos = HEADER_LEN;
        while pos < bytes.len() {
            // Need at least 4 bytes for the frame length.
            if pos + 4 > bytes.len() {
                // Torn length prefix at the tail — drop it.
                break;
            }
            let len =
                u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
                    as usize;
            let body_start = pos + 4;
            let body_end = match body_start.checked_add(len) {
                Some(e) => e,
                None => break, // absurd length; stop
            };
            if body_end > bytes.len() {
                // Torn record body at the tail — drop it and keep the prefix.
                break;
            }
            match ChainTxRecord::decode(&bytes[body_start..body_end]) {
                Ok(rec) => records.push(rec),
                // A corrupt (non-tail) record is a hard error, not a torn tail.
                Err(e) => return Err(e),
            }
            pos = body_end;
        }
        Ok(records)
    }

    /// Atomically write the head checkpoint (temp file + rename), fsyncing the
    /// temp file first.
    pub fn write_head_checkpoint(&self, head: &ChainHead) -> ChainResult<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(HEAD_MAGIC);
        payload.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
        payload.extend_from_slice(&head.encode());

        let tmp_path = self.head_path.with_extension("tmp");
        {
            let mut f = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp_path)?;
            f.write_all(&payload)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp_path, &self.head_path)?;
        Ok(())
    }

    /// Read the head checkpoint, returning `None` if none has been written.
    pub fn read_head_checkpoint(&self) -> ChainResult<Option<ChainHead>> {
        let bytes = match std::fs::read(&self.head_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if bytes.len() < HEADER_LEN {
            return Err(ChainError::Corrupt("head checkpoint too short".into()));
        }
        if &bytes[..8] != HEAD_MAGIC {
            return Err(ChainError::HeaderMismatch("bad head magic".into()));
        }
        let version = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        if version != FORMAT_VERSION {
            return Err(ChainError::HeaderMismatch(format!(
                "unsupported head version {version}"
            )));
        }
        let head = ChainHead::decode(&bytes[HEADER_LEN..])?;
        Ok(Some(head))
    }
}

fn validate_header(header: &[u8]) -> ChainResult<()> {
    if header.len() < HEADER_LEN {
        return Err(ChainError::HeaderMismatch("header too short".into()));
    }
    if &header[..8] != LOG_MAGIC {
        return Err(ChainError::HeaderMismatch("bad chain log magic".into()));
    }
    let version = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
    if version != FORMAT_VERSION {
        return Err(ChainError::HeaderMismatch(format!(
            "unsupported chain log version {version}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance_chain::canonical::EntityKind;

    fn rec(seq: u64) -> ChainTxRecord {
        ChainTxRecord {
            seq,
            commit_ts: 1000 + seq as i64,
            tx_id: seq,
            anchor_lsn: seq,
            leaves: vec![[seq as u8; 32]],
            entity_refs: vec![(EntityKind::Node, seq, seq)],
            digest: [(seq + 100) as u8; 32],
        }
    }

    #[test]
    fn append_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChainStore::open(dir.path(), ChainFsyncMode::PerTransaction).unwrap();
        let records: Vec<_> = (1..=5).map(rec).collect();
        for r in &records {
            store.append(r).unwrap();
        }
        let loaded = store.load().unwrap();
        assert_eq!(loaded, records);
    }

    #[test]
    fn reopen_appends_after_existing() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = ChainStore::open(dir.path(), ChainFsyncMode::Batched).unwrap();
            store.append(&rec(1)).unwrap();
            store.flush().unwrap();
        }
        let store = ChainStore::open(dir.path(), ChainFsyncMode::Batched).unwrap();
        store.append(&rec(2)).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded, vec![rec(1), rec(2)]);
    }

    #[test]
    fn torn_trailing_record_yields_valid_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChainStore::open(dir.path(), ChainFsyncMode::Never).unwrap();
        store.append(&rec(1)).unwrap();
        store.append(&rec(2)).unwrap();
        store.flush().unwrap();

        // Corrupt the tail: append a partial frame (a length prefix promising more
        // bytes than are present).
        let log_path = dir.path().join("chain.log");
        let mut f = OpenOptions::new().append(true).open(&log_path).unwrap();
        f.write_all(&999u32.to_be_bytes()).unwrap(); // frame len = 999
        f.write_all(&[0xAB, 0xCD]).unwrap(); // only 2 bytes of body
        f.sync_all().unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded, vec![rec(1), rec(2)], "torn tail must be dropped");
    }

    #[test]
    fn head_checkpoint_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChainStore::open(dir.path(), ChainFsyncMode::Batched).unwrap();
        assert!(store.read_head_checkpoint().unwrap().is_none());

        let head = ChainHead::genesis(3, 4242);
        store.write_head_checkpoint(&head).unwrap();
        let read = store.read_head_checkpoint().unwrap().unwrap();
        assert_eq!(read, head);

        // Overwrite is atomic and readable.
        let head2 = ChainHead {
            seq: 9,
            digest: [7u8; 32],
            commit_ts: 5000,
            anchor_lsn: 88,
            genesis_digest: head.genesis_digest,
        };
        store.write_head_checkpoint(&head2).unwrap();
        assert_eq!(store.read_head_checkpoint().unwrap().unwrap(), head2);
    }

    #[test]
    fn open_rejects_bad_magic() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("chain.log");
        std::fs::write(&log_path, b"XXXXXXXX\x00\x00\x00\x01").unwrap();
        assert!(matches!(
            ChainStore::open(dir.path(), ChainFsyncMode::Batched),
            Err(ChainError::HeaderMismatch(_))
        ));
    }

    #[test]
    fn empty_chain_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChainStore::open(dir.path(), ChainFsyncMode::Batched).unwrap();
        assert!(store.load().unwrap().is_empty());
    }
}
