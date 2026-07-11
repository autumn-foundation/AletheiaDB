//! WAL Segment Reader.
//!
//! This module provides standalone functions for reading WAL segments from disk
//! for recovery purposes. It does not require any WAL writer state.
//!
//! # Segment Format
//!
//! Each segment file on disk has the following binary layout:
//!
//! ```text
//!  Offset  Size  Field
//!  ──────  ────  ──────────────────────────────────────────────────
//!       0     4  Magic bytes: b"GWAL"  (WAL_MAGIC)
//!       4     1  Format version: 1 (plaintext) or 2 (encrypted)
//!       5     …  Entry frames (repeated until end of file)
//! ```
//!
//! **Entry frame (version 1 – plaintext)**:
//! ```text
//!  [bincode-serialised WalEntry bytes]
//! ```
//! Entries are length-prefixed by `bincode` (8-byte little-endian usize).
//!
//! **Entry frame (version 2 – encrypted)**:
//! ```text
//!  [4-byte LE u32 entry length][encrypted entry bytes]
//! ```
//! The header (magic + version) is always plaintext; only the entry frames
//! are encrypted in version 2.
//!
//! See the `WAL_MAGIC`, `WAL_VERSION`, `WAL_VERSION_ENCRYPTED`, and
//! `WAL_HEADER_SIZE` constants defined below for the associated values.
//!
//! # Memory Efficiency
//!
//! This module uses memory-mapped I/O (`memmap2`) to read WAL segments efficiently.
//! Instead of loading entire segment files (default 64MB) into memory, memory-mapped
//! files allow the OS to handle paging automatically. This provides several benefits:
//!
//! - **Lower memory usage**: OS pages in data as needed, not all at once
//! - **Better caching**: OS can cache frequently accessed pages
//! - **Automatic eviction**: OS evicts pages under memory pressure
//! - **Reduced recovery memory**: With 10 segments, peak memory drops from 640MB+ to O(working set)
//!
//! See issue #216 for details.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use crate::core::error::{Error, Result, StorageError};
use crate::core::hlc::HybridTimestamp;
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::property::PropertyMap;
use crate::core::provenance::Provenance;

use super::serialization::{
    OP_BEGIN_TX, OP_CHECKPOINT, OP_COMMIT_TX, OP_CREATE_EDGE, OP_CREATE_NODE,
    OP_DECLARE_UNIQUE_CONSTRAINT, OP_DELETE_EDGE, OP_DELETE_NODE, OP_DROP_UNIQUE_CONSTRAINT,
    OP_RETRACT_EDGE, OP_RETRACT_NODE, OP_UPDATE_EDGE, OP_UPDATE_NODE, TOMBSTONE_VERSION_ID_ABSENT,
};
use super::{LSN, WalEntry, WalOperation};

/// Magic bytes identifying a AletheiaDB WAL segment file.
pub(crate) const WAL_MAGIC: [u8; 4] = *b"GWAL";

/// Current WAL format version (plaintext entries).
pub(crate) const WAL_VERSION: u8 = 1;

/// WAL format version for encrypted segments.
///
/// Version 2 segments use length-prefixed encrypted entries:
/// `[4-byte LE entry length][encrypted entry bytes]`
/// The header (magic + version) remains plaintext.
pub(crate) const WAL_VERSION_ENCRYPTED: u8 = 2;

/// WAL format version for plaintext segments whose `CreateNode`/`CreateEdge`/
/// `UpdateNode`/`UpdateEdge` payloads carry an optional [`Provenance`] bundle
/// after `valid_from` (Issue #3224).
///
/// Segments below this version simply lack the provenance bytes; parsing
/// falls back to `provenance: None` for them (see `read_provenance`).
pub(crate) const WAL_VERSION_PROVENANCE: u8 = 3;

/// WAL format version for encrypted segments whose decrypted payload uses
/// the provenance-carrying entry format (i.e. `WAL_VERSION_PROVENANCE`).
///
/// Mirrors the `WAL_VERSION` / `WAL_VERSION_ENCRYPTED` relationship: this is
/// `WAL_VERSION_ENCRYPTED`'s counterpart once provenance was added.
pub(crate) const WAL_VERSION_ENCRYPTED_PROVENANCE: u8 = 4;

/// WAL format version for plaintext segments whose provenance bundle also
/// carries the authenticated-principal field (Issue #3350).
///
/// Identical to [`WAL_VERSION_PROVENANCE`] except the serialized provenance
/// bundle has a fifth optional string (`principal`) after `correlation_id`.
/// Segments below this version simply lack the extra bytes; parsing falls
/// back to `principal: None` for them (see `read_provenance`).
pub(crate) const WAL_VERSION_PROVENANCE_PRINCIPAL: u8 = 5;

/// WAL format version for encrypted segments whose decrypted payload uses
/// the principal-carrying provenance format (i.e.
/// [`WAL_VERSION_PROVENANCE_PRINCIPAL`]).
pub(crate) const WAL_VERSION_ENCRYPTED_PROVENANCE_PRINCIPAL: u8 = 6;

/// WAL format version for plaintext segments that may contain transaction
/// framing markers (`BeginTx`/`CommitTx`, Issue #3413).
///
/// Identical entry payload layout to [`WAL_VERSION_PROVENANCE_PRINCIPAL`] for
/// every pre-existing op; it additionally permits the two framing op tags
/// (`OP_BEGIN_TX` / `OP_COMMIT_TX`). Bumping the header version (rather than
/// silently emitting the new tags into a v5 segment) makes an older reader
/// reject the file cleanly with "Unsupported WAL version" instead of
/// misparsing an unknown op tag, following the #3224 / #3421 precedent.
///
/// NOTE: sibling Issue #3406 will also extend the delete/retract payloads and
/// must coordinate the WAL version-byte bump with this one — if both land in
/// the same release train they should share a single combined version; if they
/// land separately the second takes 9/10. The `framed` predicate here and any
/// #3406 payload gate are independent booleans on the same version byte and
/// compose without conflict.
pub(crate) const WAL_VERSION_TX_FRAMING: u8 = 7;

/// WAL format version for encrypted segments whose decrypted payload uses the
/// transaction-framing entry format (i.e. [`WAL_VERSION_TX_FRAMING`]).
pub(crate) const WAL_VERSION_ENCRYPTED_TX_FRAMING: u8 = 8;

/// WAL format version for plaintext segments whose delete/retract payloads
/// carry the tombstone/retraction `version_id` (Issue #3406).
///
/// A strict superset of [`WAL_VERSION_TX_FRAMING`]: it keeps the tx-framing
/// markers and additionally appends an 8-byte `version_id` to the
/// `DeleteNode`/`DeleteEdge`/`RetractNode`/`RetractEdge` payloads, so crash
/// recovery reproduces the live tombstone version chain bit-for-bit instead of
/// synthesizing a (possibly colliding) id. The `framed` predicate
/// ([`is_framed_version`]) and this delete-version-id gate
/// ([`carries_delete_version_id`]) are independent booleans on the same version
/// byte and compose without conflict (per the #3413 reservation note above).
/// Bumping the header version makes an older reader reject the file cleanly
/// rather than mis-length the extended payload, following the #3224/#3413
/// precedent.
pub(crate) const WAL_VERSION_DELETE_VERSION_ID: u8 = 9;

/// WAL format version for encrypted segments whose decrypted payload uses the
/// delete-version-id entry format (i.e. [`WAL_VERSION_DELETE_VERSION_ID`]).
pub(crate) const WAL_VERSION_ENCRYPTED_DELETE_VERSION_ID: u8 = 10;

/// Maximum supported WAL version (inclusive).
const WAL_VERSION_MAX: u8 = WAL_VERSION_ENCRYPTED_DELETE_VERSION_ID;

/// Returns `true` if `version` denotes an encrypted segment (the original
/// encrypted format or one of its provenance/framing-carrying successors).
#[inline]
fn is_encrypted_version(version: u8) -> bool {
    version == WAL_VERSION_ENCRYPTED
        || version == WAL_VERSION_ENCRYPTED_PROVENANCE
        || version == WAL_VERSION_ENCRYPTED_PROVENANCE_PRINCIPAL
        || version == WAL_VERSION_ENCRYPTED_TX_FRAMING
        || version == WAL_VERSION_ENCRYPTED_DELETE_VERSION_ID
}

/// Returns `true` if `version` (a plaintext/payload version) supports the
/// [`WalOperation::BeginTx`]/[`WalOperation::CommitTx`] transaction-framing
/// markers (Issue #3413). Drives the additive `WalEntry::framed` flag.
#[inline]
fn is_framed_version(version: u8) -> bool {
    version >= WAL_VERSION_TX_FRAMING
}

/// Returns `true` if `version` (a plaintext/payload version) carries the
/// tombstone/retraction `version_id` in delete/retract payloads (Issue #3406).
///
/// Independent of [`is_framed_version`]: both are monotonic `>=` predicates on
/// the same version byte, so v9/v10 segments are simultaneously framed AND
/// delete-version-id-carrying.
#[inline]
fn carries_delete_version_id(version: u8) -> bool {
    version >= WAL_VERSION_DELETE_VERSION_ID
}

/// Map a segment/container format version to the logical *payload* version
/// used to gate field parsing (e.g. `version >= WAL_VERSION_PROVENANCE`).
///
/// Encrypted versions describe the on-disk *container* (length-prefixed +
/// encrypted), not the entry payload layout once decrypted; this maps them
/// back to the plaintext version whose entry layout they share.
#[inline]
fn payload_version(version: u8) -> u8 {
    match version {
        WAL_VERSION_ENCRYPTED => WAL_VERSION,
        WAL_VERSION_ENCRYPTED_PROVENANCE => WAL_VERSION_PROVENANCE,
        WAL_VERSION_ENCRYPTED_PROVENANCE_PRINCIPAL => WAL_VERSION_PROVENANCE_PRINCIPAL,
        WAL_VERSION_ENCRYPTED_TX_FRAMING => WAL_VERSION_TX_FRAMING,
        WAL_VERSION_ENCRYPTED_DELETE_VERSION_ID => WAL_VERSION_DELETE_VERSION_ID,
        v => v,
    }
}

/// The newest *plaintext* WAL entry payload version — the exact shape that
/// `serialization::serialize_operation_into` writes and that
/// `flush_coordinator` stamps on new plaintext segments
/// (currently [`WAL_VERSION_DELETE_VERSION_ID`], v9).
///
/// Derived from [`WAL_VERSION_MAX`] (the newest *encrypted* container version,
/// v10) via [`payload_version`], so it can NEVER lag a format bump: any change
/// that raises `WAL_VERSION_MAX` and extends `payload_version`'s mapping is
/// tracked automatically. Round-trip harnesses that serialize a fresh entry and
/// re-parse it (e.g. the `wal_entry_parsing` fuzz target via
/// `crate::fuzzing::wal::parse_current_entry`) MUST parse at this version:
/// parsing at a stale version skips the framing (#3413, v7) and delete/retract
/// `version_id` (#3406, v9) bytes the serializer wrote, misaligns the buffer,
/// and fails the entry checksum.
///
/// History of the newest plaintext version: #3224→3, #3421→5, #3413→7,
/// #3406→9. Bump on every WAL plaintext format increase.
#[inline]
#[cfg(any(fuzzing, feature = "fuzzing"))]
pub(crate) fn newest_plaintext_wal_version() -> u8 {
    payload_version(WAL_VERSION_MAX)
}

/// Size of the WAL segment header (magic + version).
pub(crate) const WAL_HEADER_SIZE: usize = 5;

/// Maximum reasonable segment size (configurable, but 1GB is a safe upper bound)
/// Default segments are 64MB, so 1GB allows for 16x growth
pub(crate) const MAX_SEGMENT_SIZE: u64 = 1024 * 1024 * 1024; // 1GB

/// Read all WAL entries from a directory, starting from the specified LSN.
///
/// This function scans the directory for segment files (*.log), reads them in order,
/// and returns all entries with LSN >= start_lsn.
///
/// # Arguments
///
/// * `wal_dir` - Path to the WAL directory containing segment files
/// * `start_lsn` - Only entries with LSN >= this value are returned
///
/// # Returns
///
/// A vector of WAL entries sorted by LSN.
///
/// # Examples
///
/// ```
/// use aletheiadb::storage::wal::LSN;
/// use aletheiadb::storage::wal::segment_reader::read_entries_from_dir;
/// use std::fs::File;
/// use std::io::Write;
/// use tempfile::tempdir;
///
/// let dir = tempdir().unwrap();
/// let wal_dir = dir.path();
///
/// // Create a dummy segment file
/// let segment_path = wal_dir.join("0.log");
/// let mut file = File::create(&segment_path).unwrap();
///
/// // Write WAL header: Magic bytes *b"GWAL" and version 1
/// file.write_all(b"GWAL").unwrap();
/// file.write_all(&[1]).unwrap();
/// file.sync_all().unwrap();
///
/// // Read entries (returns an empty vector since we wrote no entries)
/// let entries = read_entries_from_dir(wal_dir, LSN(1)).unwrap();
/// assert!(entries.is_empty());
/// ```
pub fn read_entries_from_dir(wal_dir: &Path, start_lsn: LSN) -> Result<Vec<WalEntry>> {
    read_entries_from_dir_with_cipher(wal_dir, start_lsn, None)
}

/// Read all WAL entries from a directory with optional decryption.
///
/// This function scans the directory for segment files (*.log), reads them in order,
/// and returns all entries with LSN >= start_lsn. If a cipher is provided, version 2
/// (encrypted) segments are decrypted transparently. Version 1 (plaintext) segments
/// are always read without decryption regardless of the cipher parameter.
///
/// # Arguments
///
/// * `wal_dir` - Path to the WAL directory containing segment files
/// * `start_lsn` - Only entries with LSN >= this value are returned
/// * `cipher` - Optional cipher for decrypting version 2 segments
///
/// # Returns
///
/// A vector of WAL entries sorted by LSN.
pub fn read_entries_from_dir_with_cipher(
    wal_dir: &Path,
    start_lsn: LSN,
    cipher: Option<&Arc<dyn crate::encryption::cipher::Cipher>>,
) -> Result<Vec<WalEntry>> {
    // Default recovery policy: tolerate a crash-torn trailing entry in the
    // FINAL segment (Issue #3433). Operators who prefer fail-stop recovery opt
    // out via the `tolerate_torn_tail = false` recovery config flag, which
    // reaches this reader through `read_entries_from_dir_with_options`.
    read_entries_from_dir_with_options(wal_dir, start_lsn, cipher, true)
}

/// Read all WAL entries from a directory with optional decryption and an
/// explicit crash-torn-tail recovery policy (Issue #3433).
///
/// When `tolerate_torn_tail` is `true` (the default), an undecodable trailing
/// entry in the **final** segment — the shape a crash during append leaves: a
/// zeroed/garbage op-type byte, a mid-field truncation, or a checksum mismatch
/// on a half-written payload — is treated as end-of-log: everything decoded
/// before it is applied, a WARNING is logged (segment path + byte offset +
/// underlying error), and the read stops. This generalizes #3413's
/// truncation-only tolerance to every crash-torn shape.
///
/// The tolerance is strictly **tail-scoped**:
/// * corruption in a NON-final segment (a newer segment exists past it) always
///   hard-errors — that is real damage, not a torn append;
/// * for encrypted (length-prefixed) segments, an undecodable frame that is
///   FOLLOWED BY a valid frame in the final segment also hard-errors (it is
///   resyncable mid-log corruption). Plaintext entries carry no per-entry
///   length prefix, so once an entry is undecodable a following entry cannot be
///   found — for plaintext, an undecodable entry in the final segment is
///   therefore unavoidably treated as the tail (documented asymmetry).
///
/// When `tolerate_torn_tail` is `false`, ANY parse failure hard-errors, exactly
/// as a strict per-segment [`read_segment`] does — fail-stop recovery.
pub fn read_entries_from_dir_with_options(
    wal_dir: &Path,
    start_lsn: LSN,
    cipher: Option<&Arc<dyn crate::encryption::cipher::Cipher>>,
    tolerate_torn_tail: bool,
) -> Result<Vec<WalEntry>> {
    let mut entries = Vec::new();

    // Only the FINAL segment is allowed to tolerate a torn trailing entry, and
    // only when the caller's recovery policy leaves tolerance on.
    let segment_paths = sorted_segment_paths(wal_dir);
    let last_idx = segment_paths.len().saturating_sub(1);
    for (i, (_, path)) in segment_paths.iter().enumerate() {
        let segment_tolerates = tolerate_torn_tail && i == last_idx;
        let segment_entries =
            read_segment_with_cipher_tolerant(path, start_lsn, cipher, segment_tolerates)?;
        entries.extend(segment_entries);
    }

    // Sort entries by LSN to ensure correct ordering across segments.
    // In a striped WAL architecture, entries can be flushed to different segments
    // in an order that differs from their LSN assignment order.
    entries.sort_by_key(|entry| entry.lsn);

    Ok(entries)
}

/// Enumerate `*.log` WAL segment files in `wal_dir`, sorted by segment ID.
///
/// Shared between the recovery read path ([`read_entries_from_dir_with_cipher`])
/// and the LSN seeding scan ([`max_lsn_in_dir`]). An unreadable directory
/// yields an empty list, matching the historical behavior of the read path.
fn sorted_segment_paths(wal_dir: &Path) -> Vec<(u64, std::path::PathBuf)> {
    // ⚡ Bolt Optimization: Pre-allocate space for WAL segment paths to prevent
    // small heap reallocations when reading directories.
    let mut segments = Vec::with_capacity(16);
    if let Ok(dir_entries) = std::fs::read_dir(wal_dir) {
        for entry in dir_entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && name.ends_with(".log")
                && let Some(seg_id) = name
                    .strip_suffix(".log")
                    .and_then(|s| s.parse::<u64>().ok())
            {
                segments.push((seg_id, entry.path()));
            }
        }
    }
    segments.sort_by_key(|(id, _)| *id);
    segments
}

/// Log a warning from the LSN seeding scan under both logging configurations,
/// matching this module's existing logging style.
fn log_scan_warning(message: &str) {
    #[cfg(feature = "observability")]
    tracing::warn!("{}", message);
    #[cfg(not(feature = "observability"))]
    eprintln!("WARNING: {}", message);
}

/// Scan a WAL directory for the maximum LSN present in any segment file.
///
/// Standalone, additive helper for Issue #3420: on startup the LSN allocator
/// must be seeded past every LSN already durable on disk, otherwise a
/// restarted process re-allocates LSNs that already exist in older segments
/// (breaking LSN total ordering) and new writes land *below* the manifest
/// LSN, causing the next startup's differential replay to skip them.
///
/// # Torn-tail tolerance
///
/// This scan runs inside database **constructors**, so unlike the recovery
/// read path it must tolerate the data shapes a crash (or another process
/// sharing the WAL directory) can leave behind: a truncated final entry, a
/// zeroed preallocated tail, or a torn entry whose header decoded but whose
/// payload is garbage (e.g. an unknown operation type byte). On encountering
/// an undecodable entry, the scan stops reading **that segment**, keeps the
/// maximum LSN decoded from its prefix, logs a warning, and continues with
/// the remaining segments. A segment that is unreadable from byte 0 (missing
/// `GWAL` magic, unsupported version, oversized, or encrypted without a
/// cipher) is skipped entirely with a warning. Only filesystem-level I/O
/// errors (open/metadata/mmap failures) propagate, exactly as they do from
/// [`read_segment_with_cipher`].
///
/// **Consistency with replay:** the recovery replay reader
/// (`parse_plaintext_entries`) stops at a truncated or zeroed tail and
/// hard-errors on any other undecodable entry — either way, replay never
/// applies anything at or beyond the first undecodable entry of a segment.
/// Seeding the allocator from each segment's decodable prefix (plus the
/// index-manifest floor applied by the caller) therefore cannot under-seed
/// relative to what replay actually applies; tolerating the tail here only
/// removes constructor failures on data replay would never have applied.
/// Replay's own error behavior is deliberately unchanged.
///
/// # Returns
///
/// `Ok(None)` when the directory contains no segments or no decodable
/// entries; otherwise the maximum LSN across all decodable entries.
pub fn max_lsn_in_dir(
    wal_dir: &Path,
    cipher: Option<&Arc<dyn crate::encryption::cipher::Cipher>>,
) -> Result<Option<LSN>> {
    let mut max: Option<LSN> = None;
    for (_, path) in sorted_segment_paths(wal_dir) {
        max = max.max(max_lsn_in_segment(&path, cipher)?);
    }
    Ok(max)
}

/// Best-effort maximum-LSN scan of a single segment (see [`max_lsn_in_dir`]).
///
/// Never fails on undecodable entry data — returns the maximum LSN of the
/// segment's decodable prefix instead. Only filesystem-level I/O errors
/// propagate.
fn max_lsn_in_segment(
    path: &Path,
    cipher: Option<&Arc<dyn crate::encryption::cipher::Cipher>>,
) -> Result<Option<LSN>> {
    // Filesystem-level failures propagate exactly like read_segment_with_cipher.
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(StorageError::IoError(format!(
                "Failed to open WAL segment {:?}: {}",
                path, e
            ))
            .into());
        }
    };

    let metadata = file
        .metadata()
        .map_err(|e| StorageError::IoError(format!("Failed to get file metadata: {}", e)))?;

    // A zero-byte file can occur if a crash happens immediately after segment
    // creation; it trivially contributes no LSNs.
    if metadata.len() == 0 {
        return Ok(None);
    }

    if metadata.len() > MAX_SEGMENT_SIZE {
        log_scan_warning(&format!(
            "Skipping oversized WAL segment {:?} ({} bytes, max {}) during LSN seeding scan",
            path,
            metadata.len(),
            MAX_SEGMENT_SIZE
        ));
        return Ok(None);
    }

    // Memory-map the file for efficient reading without loading it into memory.
    // SAFETY: We only read from the memory map, never write. The file is opened
    // read-only. The mapping is valid for the lifetime of this function and is
    // automatically unmapped when dropped. We have verified the file size above
    // to prevent out-of-bounds reads.
    let mmap = unsafe {
        memmap2::Mmap::map(&file).map_err(|e| {
            StorageError::IoError(format!("Failed to memory-map WAL segment: {}", e))
        })?
    };
    let buffer = &mmap[..];

    // Header validation: a segment that is undecodable from byte 0 (garbage
    // or partially written header, unsupported version) contributes no
    // decodable LSNs. Replay would apply nothing from it either, so skipping
    // it with a warning cannot under-seed relative to what replay applies —
    // and it keeps the constructor usable when e.g. another process is
    // mid-way through creating a segment in a shared WAL directory.
    let (version, offset) = if buffer.len() >= WAL_HEADER_SIZE && buffer[0..4] == WAL_MAGIC {
        let ver = buffer[4];
        if ver > WAL_VERSION_MAX {
            log_scan_warning(&format!(
                "Skipping WAL segment {:?} with unsupported version {} (max {}) during LSN seeding scan",
                path, ver, WAL_VERSION_MAX
            ));
            return Ok(None);
        }
        (ver, WAL_HEADER_SIZE)
    } else {
        log_scan_warning(&format!(
            "Skipping WAL segment {:?} without a valid GWAL header during LSN seeding scan",
            path
        ));
        return Ok(None);
    };

    if is_encrypted_version(version) {
        let Some(cipher) = cipher else {
            log_scan_warning(&format!(
                "Skipping encrypted WAL segment {:?} (version {}) during LSN seeding scan: no cipher configured",
                path, version
            ));
            return Ok(None);
        };
        Ok(scan_encrypted_max_lsn(
            buffer, offset, cipher, path, version,
        ))
    } else {
        Ok(scan_plaintext_max_lsn(buffer, offset, version, path))
    }
}

/// Walk plaintext entry frames, returning the maximum LSN of the decodable
/// prefix. Mirrors `parse_plaintext_entries`' benign-stop cases (truncated
/// tail, zeroed preallocated tail) and additionally *stops* — with a warning
/// instead of an error — at any other undecodable entry (torn tail).
fn scan_plaintext_max_lsn(
    buffer: &[u8],
    mut offset: usize,
    version: u8,
    path: &Path,
) -> Option<LSN> {
    let mut max: Option<LSN> = None;
    while offset < buffer.len() {
        match parse_entry_at(buffer, offset, version) {
            Ok((entry, bytes_consumed)) => {
                max = max.max(Some(entry.lsn));
                offset += bytes_consumed;
            }
            Err(e) => {
                // Same benign-stop cases as parse_plaintext_entries: a partial
                // entry at EOF or a zeroed preallocated tail.
                if offset + 24 > buffer.len() || buffer[offset..offset + 24].iter().all(|&b| b == 0)
                {
                    break;
                }
                // Torn entry (e.g. valid header, garbage payload): stop this
                // segment, keeping the decodable prefix's max. See the
                // consistency argument on max_lsn_in_dir.
                log_scan_warning(&format!(
                    "Undecodable WAL entry in segment {:?} at offset {} during LSN seeding scan ({}); \
                     seeding from this segment's decodable prefix",
                    path, offset, e
                ));
                break;
            }
        }
    }
    max
}

/// Walk encrypted (length-prefixed) entry frames, returning the maximum LSN
/// of the decodable prefix. Mirrors `parse_encrypted_entries`' benign-stop
/// cases (partial length prefix, zero length, truncated frame) and stops —
/// with a warning instead of an error — at a frame that fails to decrypt or
/// parse (torn tail).
fn scan_encrypted_max_lsn(
    buffer: &[u8],
    mut offset: usize,
    cipher: &Arc<dyn crate::encryption::cipher::Cipher>,
    path: &Path,
    container_version: u8,
) -> Option<LSN> {
    let entry_version = payload_version(container_version);
    let mut max: Option<LSN> = None;
    while offset < buffer.len() {
        // Partial length prefix at EOF, zero-length end marker, or truncated
        // frame: same benign stops as parse_encrypted_entries.
        if offset + 4 > buffer.len() {
            break;
        }
        let len_bytes: [u8; 4] = buffer[offset..offset + 4]
            .try_into()
            .expect("slice length verified above");
        let entry_len = u32::from_le_bytes(len_bytes) as usize;
        if entry_len == 0 {
            break;
        }
        offset += 4;
        if offset + entry_len > buffer.len() {
            break;
        }
        let encrypted_entry = &buffer[offset..offset + entry_len];
        offset += entry_len;

        let decrypted =
            match crate::encryption::wal_encryption::decrypt_wal_payload(encrypted_entry, cipher) {
                Ok(d) => d,
                Err(e) => {
                    log_scan_warning(&format!(
                        "Undecryptable WAL entry in segment {:?} during LSN seeding scan ({}); \
                         seeding from this segment's decodable prefix",
                        path, e
                    ));
                    break;
                }
            };

        match parse_entry_at(&decrypted, 0, entry_version) {
            Ok((entry, _bytes_consumed)) => max = max.max(Some(entry.lsn)),
            Err(e) => {
                log_scan_warning(&format!(
                    "Undecodable decrypted WAL entry in segment {:?} during LSN seeding scan ({}); \
                     seeding from this segment's decodable prefix",
                    path, e
                ));
                break;
            }
        }
    }
    max
}

/// Read WAL entries from a single segment file.
///
/// This function uses memory-mapped I/O for efficient reading without loading
/// the entire file into memory. The OS handles paging automatically, which is
/// especially important for large segment files (default 64MB).
///
/// # Arguments
///
/// * `path` - Path to the segment file
/// * `start_lsn` - Only entries with LSN >= this value are returned
///
/// # Returns
///
/// A vector of WAL entries from this segment.
///
/// # Memory Efficiency
///
/// Uses `memmap2` for memory-mapped I/O. Peak memory usage is O(working set)
/// rather than O(file size). See issue #216.
///
/// # Examples
///
/// ```
/// use aletheiadb::storage::wal::LSN;
/// use aletheiadb::storage::wal::segment_reader::read_segment;
/// use std::fs::File;
/// use std::io::Write;
/// use tempfile::tempdir;
///
/// let dir = tempdir().unwrap();
/// let segment_path = dir.path().join("segment.log");
///
/// // Create a segment file with a valid header: Magic bytes *b"GWAL" and version 1
/// let mut file = File::create(&segment_path).unwrap();
/// file.write_all(b"GWAL").unwrap();
/// file.write_all(&[1]).unwrap();
/// file.sync_all().unwrap();
///
/// // Read the empty segment
/// let entries = read_segment(&segment_path, LSN(1)).unwrap();
/// assert!(entries.is_empty());
/// ```
pub fn read_segment(path: &Path, start_lsn: LSN) -> Result<Vec<WalEntry>> {
    read_segment_with_cipher(path, start_lsn, None)
}

/// Read WAL entries from a single segment file with optional decryption.
///
/// This function uses memory-mapped I/O for efficient reading. It transparently
/// handles both version 1 (plaintext) and version 2 (encrypted) segments:
///
/// - **Version 1**: Entries are parsed directly (no cipher needed).
/// - **Version 2**: Each entry is length-prefixed (`[4-byte LE len][encrypted data]`).
///   A cipher must be provided to decrypt version 2 segments.
///
/// # Arguments
///
/// * `path` - Path to the segment file
/// * `start_lsn` - Only entries with LSN >= this value are returned
/// * `cipher` - Optional cipher for decrypting version 2 segments
///
/// # Returns
///
/// A vector of WAL entries from this segment.
///
/// # Errors
///
/// Returns an error if:
/// - The segment is version 2 but no cipher is provided
/// - Decryption fails (wrong key, corrupted data, tampered header)
/// - The segment format is invalid
pub fn read_segment_with_cipher(
    path: &Path,
    start_lsn: LSN,
    cipher: Option<&Arc<dyn crate::encryption::cipher::Cipher>>,
) -> Result<Vec<WalEntry>> {
    // Standalone single-segment reads keep the strict contract: a torn trailing
    // entry hard-errors, exactly as before. Only the recovery dir-reader
    // (`read_entries_from_dir_with_cipher`) opts the FINAL segment into
    // torn-tail tolerance (Issue #3413).
    read_segment_with_cipher_tolerant(path, start_lsn, cipher, false)
}

/// Read WAL entries from a single segment, optionally tolerating a torn
/// trailing entry (a crash during the final flush, Issue #3413).
///
/// When `tolerate_torn_tail` is `true` and the segment's LAST entry fails to
/// parse **because its declared payload runs past end-of-buffer** (a truncated
/// trailing entry — e.g. a half-written `CommitTx` marker), the decodable
/// prefix is kept and the read stops without error. Any other parse failure
/// (checksum mismatch, unknown op type, invalid UTF-8 — all of which mean the
/// entry's bytes were fully present but wrong) still hard-errors, and so does
/// every parse failure when `tolerate_torn_tail` is `false`.
fn read_segment_with_cipher_tolerant(
    path: &Path,
    start_lsn: LSN,
    cipher: Option<&Arc<dyn crate::encryption::cipher::Cipher>>,
    tolerate_torn_tail: bool,
) -> Result<Vec<WalEntry>> {
    // Open file, only treating NotFound as "empty" - all other errors are propagated
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(StorageError::IoError(format!(
                "Failed to open WAL segment {:?}: {}",
                path, e
            ))
            .into());
        }
    };

    // Validate file size before mapping to prevent DoS attacks with huge files
    let metadata = file
        .metadata()
        .map_err(|e| StorageError::IoError(format!("Failed to get file metadata: {}", e)))?;

    if metadata.len() > MAX_SEGMENT_SIZE {
        return Err(StorageError::CorruptedData(format!(
            "WAL segment too large: {} bytes (max: {} bytes)",
            metadata.len(),
            MAX_SEGMENT_SIZE
        ))
        .into());
    }

    // Handle empty files explicitly to avoid mmap failure on some platforms (e.g. macOS/Windows)
    // A zero-byte file can occur if a crash happens immediately after segment creation.
    if metadata.len() == 0 {
        return Ok(Vec::new());
    }

    // Memory-map the file for efficient reading without loading entire file into memory.
    // SAFETY: We only read from the memory map, never write. The file is opened read-only.
    // The mapping is valid for the lifetime of this function and is automatically unmapped
    // when dropped. We have verified the file size above to prevent out-of-bounds reads.
    let mmap = unsafe {
        memmap2::Mmap::map(&file).map_err(|e| {
            StorageError::IoError(format!("Failed to memory-map WAL segment: {}", e))
        })?
    };

    // Use the memory-mapped region as a byte slice
    let buffer = &mmap[..];

    // ⚡ Bolt Optimization: Pre-allocate vector based on buffer size.
    // Assuming an average WAL entry is ~128 bytes, this prevents numerous
    // heap reallocations when reading large segments.
    let capacity_hint = (buffer.len() / 128).max(1);
    let mut entries = Vec::with_capacity(capacity_hint);

    // Detect WAL format version
    let (version, mut offset) = if buffer.len() >= WAL_HEADER_SIZE && buffer[0..4] == WAL_MAGIC {
        // Version 1+ format: has magic header
        let ver = buffer[4];
        if ver > WAL_VERSION_MAX {
            return Err(StorageError::CorruptedData(format!(
                "Unsupported WAL version: {} (max supported: {})",
                ver, WAL_VERSION_MAX
            ))
            .into());
        }
        (ver, WAL_HEADER_SIZE)
    } else if !buffer.is_empty() {
        // Invalid format: no magic header
        return Err(StorageError::CorruptedData(
            "Invalid WAL segment: missing GWAL magic header".to_string(),
        )
        .into());
    } else {
        return Ok(Vec::new()); // Empty segment
    };

    // Encrypted segments (versions 2/4/6/8) require a cipher for decryption.
    if is_encrypted_version(version) && cipher.is_none() {
        return Err(StorageError::Encryption(format!(
            "Cannot read encrypted WAL segment (version {}) without a cipher",
            version
        ))
        .into());
    }

    // Dispatch to the appropriate parsing loop based on version.
    if is_encrypted_version(version) {
        // Encrypted (2/4/6/8): length-prefixed encrypted entries.
        let cipher = cipher.expect("cipher presence checked above");
        parse_encrypted_entries(
            buffer,
            &mut offset,
            start_lsn,
            cipher,
            path,
            &mut entries,
            version,
            tolerate_torn_tail,
        )?;
    } else {
        // Plaintext (1/3/5/7): plaintext entries.
        parse_plaintext_entries(
            buffer,
            &mut offset,
            version,
            start_lsn,
            path,
            &mut entries,
            tolerate_torn_tail,
        )?;
    }

    Ok(entries)
}

/// Log a WARNING that replay stopped at a crash-torn trailing entry, under both
/// logging configurations (Issue #3433). Carries the segment path, byte offset,
/// and the underlying parse/decrypt error so operators can audit exactly where
/// the log tail was truncated.
fn log_torn_tail_warning(path: &Path, offset: usize, err: &Error) {
    let msg = format!(
        "Crash-torn trailing WAL entry in final segment {:?} at byte offset {}: {}; \
         stopping replay here and keeping the decodable prefix (torn entries were never \
         acknowledged, so discarding them is correct)",
        path, offset, err
    );
    #[cfg(feature = "observability")]
    tracing::warn!("{}", msg);
    #[cfg(not(feature = "observability"))]
    eprintln!("WARNING: {}", msg);
}

/// Parse plaintext (versions 1/3/5/7) entries from a WAL segment buffer.
///
/// See [`read_segment_with_cipher_tolerant`] for the meaning of
/// `tolerate_torn_tail`.
fn parse_plaintext_entries(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    start_lsn: LSN,
    path: &Path,
    entries: &mut Vec<WalEntry>,
    tolerate_torn_tail: bool,
) -> Result<()> {
    // LSN of the last entry we successfully decoded (regardless of the
    // `start_lsn` push filter). A genuine continuation entry after an
    // undecodable one has a strictly higher LSN; this guards the forward probe
    // (below) against a ~2^-32 CRC false positive from mis-aligned bytes.
    let mut last_parsed_lsn: Option<LSN> = None;

    while *offset < buffer.len() {
        match parse_entry_at(buffer, *offset, version) {
            Ok((entry, bytes_consumed)) => {
                last_parsed_lsn = Some(entry.lsn);
                if entry.lsn >= start_lsn {
                    entries.push(entry);
                }
                *offset += bytes_consumed;
            }
            Err(e) => {
                // Distinguish between expected EOF truncation vs. unexpected corruption
                if *offset + 24 > buffer.len() {
                    // Partial header (torn write, < 24 bytes left): the entry
                    // cannot even form a header, so nothing valid can follow —
                    // this is a genuine torn tail. An all-zero remainder is
                    // benign pre-allocation padding and is ALWAYS treated as
                    // end-of-log (hard-erroring on it would brick normal
                    // startup). A NONZERO partial header is a torn append:
                    // tolerated under the flag, fail-stop when opted out
                    // (Issue #3433 / PR #3461 config fix — this branch used to
                    // `break` unconditionally, ignoring `tolerate_torn_tail`).
                    let remainder = &buffer[*offset..];
                    if remainder.iter().all(|&b| b == 0) {
                        #[cfg(feature = "observability")]
                        tracing::debug!(
                            "Zeroed partial region at end of WAL segment {:?} (offset {}/{}), stopping read",
                            path,
                            offset,
                            buffer.len()
                        );
                        break;
                    }
                    if tolerate_torn_tail {
                        log_torn_tail_warning(path, *offset, &e);
                        break;
                    }
                    return Err(e);
                } else {
                    let header_slice = &buffer[*offset..*offset + 24];
                    if header_slice.iter().all(|&b| b == 0) {
                        #[cfg(feature = "observability")]
                        tracing::debug!(
                            "Zeroed region at end of WAL segment {:?} (offset {}/{}), stopping read",
                            path,
                            offset,
                            buffer.len()
                        );
                        break;
                    }

                    // An undecodable full entry (Issue #3433 / PR #3461
                    // corruption-safety fix). Plaintext entries carry NO
                    // per-entry length prefix, so we cannot resync by a length
                    // field — but we CAN still tell a crash-torn tail apart
                    // from mid-log corruption by scanning forward for a real
                    // continuation entry (`plaintext_valid_entry_follows`):
                    //
                    // * A valid entry with a HIGHER LSN survives after this one
                    //   => acknowledged committed data lies past the corruption.
                    //   This is mid-log damage, NOT a torn tail, and MUST
                    //   hard-error even with `tolerate_torn_tail = true` — the
                    //   tolerant default must never silently drop the bytes of a
                    //   real transaction (which, plaintext being length-prefix
                    //   free, could be up to a whole 64 MB segment).
                    // * Nothing valid follows (scan reaches EOF) => a genuine
                    //   crash-torn tail (zeroed/garbage op-type, mid-field
                    //   truncation, or a checksum mismatch on a half-written
                    //   payload). The torn entry was never acknowledged, so we
                    //   keep the decodable prefix and stop — but only under the
                    //   flag (a non-final segment or `tolerate_torn_tail = false`
                    //   still hard-errors).
                    //
                    // This mirrors the encrypted path's `encrypted_valid_frame_follows`
                    // lookahead, closing the plaintext data-loss asymmetry.
                    if !plaintext_valid_entry_follows(buffer, *offset, version, last_parsed_lsn)
                        && tolerate_torn_tail
                    {
                        log_torn_tail_warning(path, *offset, &e);
                        break;
                    }

                    #[cfg(feature = "observability")]
                    tracing::error!(
                        "Failed to parse WAL entry in segment {:?} at offset {}: {}",
                        path,
                        offset,
                        e
                    );
                    #[cfg(not(feature = "observability"))]
                    {
                        eprintln!(
                            "CRITICAL: Failed to parse WAL entry in segment {:?} at offset {}: {}",
                            path, offset, e
                        );
                        eprintln!("Header slice: {:?}", header_slice);
                    }
                    return Err(e);
                }
            }
        }
    }
    Ok(())
}

/// Scanning forward from `start` (byte by byte, because plaintext WAL entries
/// carry no per-entry length prefix), does any position decode via
/// [`parse_entry_at`] into a valid, CRC-checked entry whose LSN is strictly
/// greater than `last_parsed_lsn`? (Issue #3433 / PR #3461 corruption-safety.)
///
/// This is the plaintext analogue of [`encrypted_valid_frame_follows`]. A
/// `true` answer means a real committed entry survives PAST an undecodable one
/// — i.e. mid-log corruption, not a crash-torn tail — so replay must hard-error
/// regardless of `tolerate_torn_tail`. A `false` answer (the scan reaches
/// end-of-buffer with nothing valid after) means a genuine torn tail, and the
/// caller applies the flag.
///
/// The `> last_parsed_lsn` guard rejects a ~2^-32 CRC false positive from
/// mis-aligned bytes: a genuine continuation always has a strictly higher LSN
/// than the last entry we decoded (WAL LSNs increase monotonically). When
/// nothing has been decoded yet (`None`), any valid following entry counts —
/// a corrupt first entry with a valid entry after it is still mid-log damage.
///
/// The scan runs only on the error path during recovery (rare) and is bounded
/// by the remaining segment bytes.
fn plaintext_valid_entry_follows(
    buffer: &[u8],
    start: usize,
    version: u8,
    last_parsed_lsn: Option<LSN>,
) -> bool {
    // `start` itself already failed to parse; begin at the next byte.
    let mut scan = start + 1;
    while scan + 24 <= buffer.len() {
        if let Ok((entry, _)) = parse_entry_at(buffer, scan, version) {
            match last_parsed_lsn {
                Some(last) if entry.lsn > last => return true,
                None => return true,
                _ => {}
            }
        }
        scan += 1;
    }
    false
}

/// Parse encrypted (versions 2/4/6/8) entries from a WAL segment buffer.
///
/// Each entry is stored as `[4-byte LE length][encrypted entry bytes]`.
/// The encrypted entry bytes are decrypted using the provided cipher,
/// then parsed as a normal WAL entry using the payload version implied by
/// `container_version` (see [`payload_version`]).
///
/// See [`read_segment_with_cipher_tolerant`] for the meaning of
/// `tolerate_torn_tail`.
#[allow(clippy::too_many_arguments)]
fn parse_encrypted_entries(
    buffer: &[u8],
    offset: &mut usize,
    start_lsn: LSN,
    cipher: &Arc<dyn crate::encryption::cipher::Cipher>,
    path: &Path,
    entries: &mut Vec<WalEntry>,
    container_version: u8,
    tolerate_torn_tail: bool,
) -> Result<()> {
    let entry_version = payload_version(container_version);
    while *offset < buffer.len() {
        // Byte offset of this frame's length prefix, for torn-tail warnings.
        let frame_start = *offset;
        // Need at least 4 bytes for the length prefix
        if *offset + 4 > buffer.len() {
            // Partial length prefix at EOF -- a torn write. An all-zero
            // remainder is benign pre-allocation padding (always end-of-log);
            // a nonzero partial prefix is a torn tail: tolerated under the flag,
            // fail-stop when opted out (Issue #3433 / PR #3461 config fix —
            // this used to `break` unconditionally, ignoring the flag).
            let remainder = &buffer[*offset..];
            if remainder.iter().all(|&b| b == 0) {
                break;
            }
            let err = Error::Storage(StorageError::CorruptedData(format!(
                "Partial encrypted length prefix at end of WAL segment {:?} (offset {}/{})",
                path,
                *offset,
                buffer.len()
            )));
            if tolerate_torn_tail {
                log_torn_tail_warning(path, frame_start, &err);
                break;
            }
            return Err(err);
        }

        // Check for zeroed length prefix (indicates end of data in pre-allocated files)
        let len_bytes: [u8; 4] = buffer[*offset..*offset + 4]
            .try_into()
            .expect("slice length verified above");
        let entry_len = u32::from_le_bytes(len_bytes) as usize;

        if entry_len == 0 {
            // Zero-length entry marks end of valid data (pre-allocation
            // padding). Always benign — never gated on the flag.
            break;
        }

        *offset += 4;

        // Validate entry length
        if *offset + entry_len > buffer.len() {
            // Truncated encrypted entry at EOF -- a torn write. A length-prefix
            // that points past the end of the buffer means the frame body was
            // never fully written, so nothing valid can follow: a genuine torn
            // tail. Tolerated under the flag, fail-stop when opted out
            // (Issue #3433 / PR #3461 config fix — used to `break`
            // unconditionally, ignoring the flag).
            let err = Error::Storage(StorageError::CorruptedData(format!(
                "Truncated encrypted entry at end of WAL segment {:?} (offset {}, entry_len {}, buf_len {})",
                path,
                *offset,
                entry_len,
                buffer.len()
            )));
            if tolerate_torn_tail {
                log_torn_tail_warning(path, frame_start, &err);
                break;
            }
            return Err(err);
        }

        let encrypted_entry = &buffer[*offset..*offset + entry_len];
        *offset += entry_len;

        // Decrypt the entry. A crash-torn tail can leave a length-complete but
        // undecryptable final frame (Issue #3433). Encrypted frames ARE
        // length-prefixed, so unlike plaintext we can — and MUST — distinguish a
        // torn tail from mid-log corruption: tolerate only when NO valid frame
        // follows this one; if a later frame still decrypts and parses, this is
        // resyncable mid-log damage and must hard-error even in the final
        // segment.
        let decrypted =
            match crate::encryption::wal_encryption::decrypt_wal_payload(encrypted_entry, cipher) {
                Ok(d) => d,
                Err(e) => {
                    let err = Error::Storage(StorageError::Encryption(format!(
                        "Failed to decrypt WAL entry in segment {:?}: {}",
                        path, e
                    )));
                    if tolerate_torn_tail
                        && !encrypted_valid_frame_follows(buffer, *offset, cipher, entry_version)
                    {
                        log_torn_tail_warning(path, frame_start, &err);
                        break;
                    }
                    return Err(err);
                }
            };

        // Parse the decrypted bytes as a normal entry, using the payload
        // version implied by the container version (plaintext-equivalent).
        match parse_entry_at(&decrypted, 0, entry_version) {
            Ok((entry, _bytes_consumed)) => {
                if entry.lsn >= start_lsn {
                    entries.push(entry);
                }
            }
            Err(e) => {
                // Same crash-torn-tail policy as the decrypt branch above:
                // tolerate a length-complete-but-undecodable final frame only
                // when no valid frame follows it.
                if tolerate_torn_tail
                    && !encrypted_valid_frame_follows(buffer, *offset, cipher, entry_version)
                {
                    log_torn_tail_warning(path, frame_start, &e);
                    break;
                }
                #[cfg(feature = "observability")]
                tracing::error!(
                    "Failed to parse decrypted WAL entry in segment {:?}: {}",
                    path,
                    e
                );
                #[cfg(not(feature = "observability"))]
                eprintln!(
                    "CRITICAL: Failed to parse decrypted WAL entry in segment {:?}: {}",
                    path, e
                );
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Does any length-prefixed encrypted frame at or after `offset` still decrypt
/// AND parse into a valid WAL entry? (Issue #3433)
///
/// Used by [`parse_encrypted_entries`] to distinguish a crash-torn tail from
/// resyncable mid-log corruption: because encrypted frames are length-prefixed,
/// a failed frame that is FOLLOWED BY a recoverable frame is real damage (and
/// must hard-error), whereas a failed frame with nothing decodable after it is
/// the torn tail (and may be dropped). Partial/zero-length trailing frames end
/// the scan without counting as "valid following".
///
/// KNOWN LIMITATION (defensive follow-up, not fixed here): the 4-byte little-
/// endian length prefix that frames each ciphertext lives OUTSIDE the AEAD
/// envelope — it is neither encrypted nor authenticated. A mid-stream corrupted
/// or tampered length prefix can therefore point the scan at the wrong offset
/// and desync this lookahead (skip a real following frame, or realign onto
/// garbage), weakening the torn-tail-vs-mid-log-corruption discrimination for
/// encrypted segments specifically. Hardening this needs a WAL format change
/// (bind the length into the frame's AAD, or length-prefix-inside-envelope) and
/// is tracked separately; do NOT rely on this probe as an integrity guarantee
/// against an adversary who can edit length prefixes.
fn encrypted_valid_frame_follows(
    buffer: &[u8],
    mut offset: usize,
    cipher: &Arc<dyn crate::encryption::cipher::Cipher>,
    entry_version: u8,
) -> bool {
    while offset < buffer.len() {
        if offset + 4 > buffer.len() {
            return false;
        }
        let entry_len = u32::from_le_bytes(
            buffer[offset..offset + 4]
                .try_into()
                .expect("slice length verified above"),
        ) as usize;
        if entry_len == 0 {
            return false;
        }
        offset += 4;
        if offset + entry_len > buffer.len() {
            return false;
        }
        let frame = &buffer[offset..offset + entry_len];
        offset += entry_len;
        if let Ok(decrypted) = crate::encryption::wal_encryption::decrypt_wal_payload(frame, cipher)
            && parse_entry_at(&decrypted, 0, entry_version).is_ok()
        {
            return true;
        }
        // Otherwise keep scanning: a single bad frame followed by more bad
        // frames is still a tail unless SOMETHING valid appears later.
    }
    false
}

/// Advance `offset` by `n` bytes with overflow protection.
#[inline]
fn advance(offset: &mut usize, n: usize) -> Result<()> {
    *offset = offset.checked_add(n).ok_or_else(|| {
        Error::Storage(StorageError::CorruptedData(
            "WAL offset overflow".to_string(),
        ))
    })?;
    Ok(())
}

/// Verify at least `n` bytes are available from `offset` in `buffer`.
///
/// Returns an overflow error if `offset + n` would overflow, or a `CorruptedData`
/// error with `context` in the message if the buffer is too short.
#[inline]
fn require_bytes(buffer: &[u8], offset: usize, n: usize, context: &str) -> Result<()> {
    let end = offset.checked_add(n).ok_or_else(|| {
        Error::Storage(StorageError::CorruptedData(
            "WAL offset overflow".to_string(),
        ))
    })?;
    if end > buffer.len() {
        return Err(StorageError::CorruptedData(format!(
            "Insufficient buffer size for {}",
            context
        ))
        .into());
    }
    Ok(())
}

/// Read an optional `String` field: `[1-byte presence][4-byte LE len][UTF-8 bytes]`.
///
/// The length and bytes are only present when the presence byte is nonzero.
fn read_opt_string(buffer: &[u8], offset: &mut usize, context: &str) -> Result<Option<String>> {
    require_bytes(buffer, *offset, 1, context)?;
    let present = buffer[*offset];
    advance(offset, 1)?;
    if present == 0 {
        return Ok(None);
    }
    require_bytes(buffer, *offset, 4, context)?;
    let len = u32::from_le_bytes(buffer[*offset..*offset + 4].try_into().unwrap()) as usize;
    advance(offset, 4)?;
    require_bytes(buffer, *offset, len, context)?;
    let s = std::str::from_utf8(&buffer[*offset..*offset + len])
        .map_err(|e| StorageError::CorruptedData(format!("Invalid UTF-8 in {}: {}", context, e)))?
        .to_string();
    advance(offset, len)?;
    Ok(Some(s))
}

/// Read an optional `f64` field: `[1-byte presence][8-byte LE value]`.
fn read_opt_f64(buffer: &[u8], offset: &mut usize, context: &str) -> Result<Option<f64>> {
    require_bytes(buffer, *offset, 1, context)?;
    let present = buffer[*offset];
    advance(offset, 1)?;
    if present == 0 {
        return Ok(None);
    }
    require_bytes(buffer, *offset, 8, context)?;
    let v = f64::from_le_bytes(buffer[*offset..*offset + 8].try_into().unwrap());
    advance(offset, 8)?;
    Ok(Some(v))
}

/// Read an optional [`Provenance`] bundle written by `serialize_provenance_into`.
///
/// Segments below [`WAL_VERSION_PROVENANCE`] never contain these bytes at
/// all; for those, this returns `Ok(None)` without touching `offset`.
fn read_provenance(buffer: &[u8], offset: &mut usize, version: u8) -> Result<Option<Provenance>> {
    if version < WAL_VERSION_PROVENANCE {
        return Ok(None);
    }
    require_bytes(buffer, *offset, 1, "provenance presence")?;
    let present = buffer[*offset];
    advance(offset, 1)?;
    if present == 0 {
        return Ok(None);
    }

    let source = read_opt_string(buffer, offset, "provenance.source")?;
    let confidence = read_opt_f64(buffer, offset, "provenance.confidence")?;
    let note = read_opt_string(buffer, offset, "provenance.note")?;
    let correlation_id = read_opt_string(buffer, offset, "provenance.correlation_id")?;
    // The authenticated-principal field (Issue #3350) only exists on
    // segments at or above WAL_VERSION_PROVENANCE_PRINCIPAL; older
    // provenance-carrying segments end the bundle at correlation_id.
    let principal = if version >= WAL_VERSION_PROVENANCE_PRINCIPAL {
        read_opt_string(buffer, offset, "provenance.principal")?
    } else {
        None
    };

    let mut builder = Provenance::builder();
    if let Some(source) = source {
        builder = builder.source(source);
    }
    if let Some(confidence) = confidence {
        builder = builder.confidence(confidence);
    }
    if let Some(note) = note {
        builder = builder.note(note);
    }
    if let Some(correlation_id) = correlation_id {
        builder = builder.correlation_id(correlation_id);
    }
    if let Some(principal) = principal {
        builder = builder.principal(principal);
    }
    let provenance = builder.build().map_err(|e| {
        StorageError::CorruptedData(format!("Invalid provenance in WAL entry: {}", e))
    })?;
    Ok(Some(provenance))
}

/// Read a 4-byte InternedString label ID from `buffer` at `offset`, advancing `offset` by 4.
#[inline]
fn read_label(
    buffer: &[u8],
    offset: &mut usize,
    context: &str,
) -> Result<crate::core::interning::InternedString> {
    require_bytes(buffer, *offset, 4, context)?;
    let label_id = u32::from_le_bytes(buffer[*offset..*offset + 4].try_into().unwrap());
    advance(offset, 4)?;
    Ok(crate::core::interning::InternedString::from_raw(label_id))
}

/// Read a PropertyMap and valid_from HybridTimestamp for version 1+ entries.
///
/// For version 0 (legacy format), returns an empty property map and the entry's
/// transaction timestamp as the valid_from time.
fn read_props_and_valid_from(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    tx_timestamp: HybridTimestamp,
) -> Result<(PropertyMap, HybridTimestamp)> {
    if version >= WAL_VERSION {
        let (props, props_len) = PropertyMap::deserialize(&buffer[*offset..])?;
        advance(offset, props_len)?;
        let (valid_from, ts_len) = HybridTimestamp::deserialize(&buffer[*offset..])?;
        advance(offset, ts_len)?;
        Ok((props, valid_from))
    } else {
        Ok((PropertyMap::new(), tx_timestamp))
    }
}

fn parse_create_node_op(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    tx_timestamp: HybridTimestamp,
) -> Result<WalOperation> {
    let node_id = deserialize_node_id(buffer, *offset, "CreateNode")?;
    advance(offset, 8)?;
    let label = read_label(buffer, offset, "CreateNode label")?;
    let (properties, valid_from) =
        read_props_and_valid_from(buffer, offset, version, tx_timestamp)?;
    let provenance = read_provenance(buffer, offset, version)?;
    Ok(WalOperation::CreateNode {
        node_id,
        label,
        properties,
        valid_from,
        provenance,
    })
}

fn parse_create_edge_op(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    tx_timestamp: HybridTimestamp,
) -> Result<WalOperation> {
    let edge_id = deserialize_edge_id(buffer, *offset, "CreateEdge")?;
    advance(offset, 8)?;
    let source = deserialize_node_id(buffer, *offset, "CreateEdge source")?;
    advance(offset, 8)?;
    let target = deserialize_node_id(buffer, *offset, "CreateEdge target")?;
    advance(offset, 8)?;
    let label = read_label(buffer, offset, "CreateEdge label")?;
    let (properties, valid_from) =
        read_props_and_valid_from(buffer, offset, version, tx_timestamp)?;
    let provenance = read_provenance(buffer, offset, version)?;
    Ok(WalOperation::CreateEdge {
        edge_id,
        source,
        target,
        label,
        properties,
        valid_from,
        provenance,
    })
}

fn parse_update_node_op(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    tx_timestamp: HybridTimestamp,
) -> Result<WalOperation> {
    let node_id = deserialize_node_id(buffer, *offset, "UpdateNode")?;
    advance(offset, 8)?;
    let version_id = deserialize_version_id(buffer, *offset, "UpdateNode")?;
    advance(offset, 8)?;
    let (label, properties, valid_from) = if version >= WAL_VERSION {
        let label = read_label(buffer, offset, "UpdateNode label")?;
        let (props, valid_from) = read_props_and_valid_from(buffer, offset, version, tx_timestamp)?;
        (label, props, valid_from)
    } else {
        (
            crate::core::interning::InternedString::from_raw(0),
            PropertyMap::new(),
            tx_timestamp,
        )
    };
    let provenance = read_provenance(buffer, offset, version)?;
    Ok(WalOperation::UpdateNode {
        node_id,
        version_id,
        label,
        properties,
        valid_from,
        provenance,
    })
}

fn parse_update_edge_op(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    tx_timestamp: HybridTimestamp,
) -> Result<WalOperation> {
    // Upfront check is required: for V1 it pre-validates EdgeId+VersionId+LabelId (20 bytes)
    // as a unit, producing the "UpdateEdge" error message that tests assert on.
    // Removing it would shift the failure to read_label with a different message.
    let required = if version >= WAL_VERSION { 20 } else { 16 };
    require_bytes(buffer, *offset, required, "UpdateEdge")?;
    let edge_id = deserialize_edge_id(buffer, *offset, "UpdateEdge")?;
    advance(offset, 8)?;
    let version_id = deserialize_version_id(buffer, *offset, "UpdateEdge")?;
    advance(offset, 8)?;
    let (label, properties, valid_from) = if version >= WAL_VERSION {
        let label = read_label(buffer, offset, "UpdateEdge label")?;
        let (props, valid_from) = read_props_and_valid_from(buffer, offset, version, tx_timestamp)?;
        (label, props, valid_from)
    } else {
        (
            crate::core::interning::InternedString::from_raw(0),
            PropertyMap::new(),
            tx_timestamp,
        )
    };
    let provenance = read_provenance(buffer, offset, version)?;
    Ok(WalOperation::UpdateEdge {
        edge_id,
        version_id,
        label,
        properties,
        valid_from,
        provenance,
    })
}

/// Parse an optional tombstone/retraction `version_id` trailing a delete/retract
/// payload (Issue #3406).
///
/// For segments below [`WAL_VERSION_DELETE_VERSION_ID`] the field is absent, so
/// this returns `None` and replay falls back to synthesizing the id. For v9+
/// segments it reads a fixed 8-byte LE u64; the
/// [`TOMBSTONE_VERSION_ID_ABSENT`] sentinel maps back to `None`.
fn parse_opt_tombstone_version_id(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    context: &str,
) -> Result<Option<VersionId>> {
    if !carries_delete_version_id(version) {
        return Ok(None);
    }
    require_bytes(buffer, *offset, 8, context)?;
    let raw = u64::from_le_bytes(buffer[*offset..*offset + 8].try_into().unwrap());
    advance(offset, 8)?;
    if raw == TOMBSTONE_VERSION_ID_ABSENT {
        return Ok(None);
    }
    let vid = VersionId::new(raw).map_err(|e| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Invalid tombstone version ID in WAL {}: {}",
            context, e
        )))
    })?;
    Ok(Some(vid))
}

fn parse_delete_node_op(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    tx_timestamp: HybridTimestamp,
) -> Result<WalOperation> {
    let node_id = deserialize_node_id(buffer, *offset, "DeleteNode")?;
    advance(offset, 8)?;
    let valid_from = if version >= WAL_VERSION {
        let (ts, ts_len) = HybridTimestamp::deserialize(&buffer[*offset..])?;
        advance(offset, ts_len)?;
        ts
    } else {
        tx_timestamp
    };
    let version_id = parse_opt_tombstone_version_id(buffer, offset, version, "DeleteNode")?;
    Ok(WalOperation::DeleteNode {
        node_id,
        valid_from,
        version_id,
    })
}

fn parse_delete_edge_op(
    buffer: &[u8],
    offset: &mut usize,
    version: u8,
    tx_timestamp: HybridTimestamp,
) -> Result<WalOperation> {
    let edge_id = deserialize_edge_id(buffer, *offset, "DeleteEdge")?;
    advance(offset, 8)?;
    let valid_from = if version >= WAL_VERSION {
        let (ts, ts_len) = HybridTimestamp::deserialize(&buffer[*offset..])?;
        advance(offset, ts_len)?;
        ts
    } else {
        tx_timestamp
    };
    let version_id = parse_opt_tombstone_version_id(buffer, offset, version, "DeleteEdge")?;
    Ok(WalOperation::DeleteEdge {
        edge_id,
        valid_from,
        version_id,
    })
}

/// Parse a `RetractNode` payload: `[node_id: 8][valid_to: 12]` plus, for v9+
/// segments, an 8-byte tombstone `version_id` (Issue #3406).
///
/// The `valid_to` needs no version gating: the `OP_RETRACT_NODE` tag (Issue
/// #3230) only ever appears in segments written by versions that serialize
/// `valid_to`. The trailing `version_id` is gated on
/// [`WAL_VERSION_DELETE_VERSION_ID`].
fn parse_retract_node_op(buffer: &[u8], offset: &mut usize, version: u8) -> Result<WalOperation> {
    let node_id = deserialize_node_id(buffer, *offset, "RetractNode")?;
    advance(offset, 8)?;
    let (valid_to, ts_len) = HybridTimestamp::deserialize(&buffer[*offset..])?;
    advance(offset, ts_len)?;
    let version_id = parse_opt_tombstone_version_id(buffer, offset, version, "RetractNode")?;
    Ok(WalOperation::RetractNode {
        node_id,
        valid_to,
        version_id,
    })
}

/// Parse a `RetractEdge` payload: `[edge_id: 8][valid_to: 12]` plus, for v9+
/// segments, an 8-byte tombstone `version_id` (Issue #3406).
///
/// See [`parse_retract_node_op`] for version-gating details.
fn parse_retract_edge_op(buffer: &[u8], offset: &mut usize, version: u8) -> Result<WalOperation> {
    let edge_id = deserialize_edge_id(buffer, *offset, "RetractEdge")?;
    advance(offset, 8)?;
    let (valid_to, ts_len) = HybridTimestamp::deserialize(&buffer[*offset..])?;
    advance(offset, ts_len)?;
    let version_id = parse_opt_tombstone_version_id(buffer, offset, version, "RetractEdge")?;
    Ok(WalOperation::RetractEdge {
        edge_id,
        valid_to,
        version_id,
    })
}

fn parse_checkpoint_op(buffer: &[u8], offset: &mut usize) -> Result<WalOperation> {
    // LSN (8 bytes) + HybridTimestamp (12 bytes) = 20 bytes
    require_bytes(buffer, *offset, 20, "Checkpoint")?;
    let cp_lsn = LSN(u64::from_le_bytes(
        buffer[*offset..*offset + 8].try_into().unwrap(),
    ));
    advance(offset, 8)?;
    let (cp_timestamp, consumed) = HybridTimestamp::deserialize(&buffer[*offset..])?;
    advance(offset, consumed)?;
    Ok(WalOperation::Checkpoint {
        lsn: cp_lsn,
        timestamp: cp_timestamp,
    })
}

/// Parse a `BeginTx` payload: `[tx_id: 8]` (Issue #3413).
///
/// No version gating is needed: the `OP_BEGIN_TX` tag only ever appears in
/// segments at or above [`WAL_VERSION_TX_FRAMING`].
fn parse_begin_tx_op(buffer: &[u8], offset: &mut usize) -> Result<WalOperation> {
    require_bytes(buffer, *offset, 8, "BeginTx")?;
    let tx_id = u64::from_le_bytes(buffer[*offset..*offset + 8].try_into().unwrap());
    advance(offset, 8)?;
    Ok(WalOperation::BeginTx { tx_id })
}

/// Parse a `CommitTx` payload: `[tx_id: 8][entry_count: 4][commit_timestamp: 12]`
/// (Issue #3413). See [`parse_begin_tx_op`] for why no version gating is needed.
fn parse_commit_tx_op(buffer: &[u8], offset: &mut usize) -> Result<WalOperation> {
    require_bytes(buffer, *offset, 12, "CommitTx header")?;
    let tx_id = u64::from_le_bytes(buffer[*offset..*offset + 8].try_into().unwrap());
    advance(offset, 8)?;
    let entry_count = u32::from_le_bytes(buffer[*offset..*offset + 4].try_into().unwrap());
    advance(offset, 4)?;
    let (commit_timestamp, ts_len) = HybridTimestamp::deserialize(&buffer[*offset..])?;
    advance(offset, ts_len)?;
    Ok(WalOperation::CommitTx {
        tx_id,
        entry_count,
        commit_timestamp,
    })
}

/// Parse a single WAL entry from a buffer at the specified offset.
///
/// This function extracts the parsing logic that was previously duplicated
/// in multiple places (issue #218). It handles all WAL operation types and
/// returns both the parsed entry and the number of bytes consumed.
///
/// # Arguments
///
/// * `buffer` - The buffer containing serialized WAL data
/// * `offset` - The offset in the buffer to start parsing from
/// * `version` - The WAL format version
///
/// # Returns
///
/// A tuple of (WalEntry, bytes_consumed) on success, or an error if:
/// - The buffer is too small to contain a valid entry
/// - The operation type is unknown
/// - The data is corrupted or truncated
///
/// # Example
///
/// ```ignore
/// let (entry, bytes_consumed) = parse_entry_at(&buffer, 0, WAL_VERSION)?;
/// let next_offset = offset + bytes_consumed;
/// ```
pub(crate) fn parse_entry_at(
    buffer: &[u8],
    offset: usize,
    version: u8,
) -> Result<(WalEntry, usize)> {
    let start_offset = offset;
    let mut cur = offset;

    // Need at least 24 bytes for LSN (8) + HybridTimestamp (12) + checksum (4)
    require_bytes(buffer, cur, 24, "WAL entry header")?;

    let lsn = LSN(u64::from_le_bytes(
        buffer[cur..cur + 8].try_into().unwrap(), // Safe: require_bytes verified 24 bytes
    ));
    advance(&mut cur, 8)?;

    let (timestamp, _) = HybridTimestamp::deserialize(&buffer[cur..]).map_err(|e| {
        StorageError::CorruptedData(format!("Failed to deserialize timestamp: {}", e))
    })?;
    advance(&mut cur, 12)?;

    let checksum = u32::from_le_bytes(
        buffer[cur..cur + 4].try_into().unwrap(), // Safe: require_bytes verified 24 bytes
    );
    advance(&mut cur, 4)?;

    if cur >= buffer.len() {
        return Err(StorageError::CorruptedData(
            "Insufficient buffer size for operation type".to_string(),
        )
        .into());
    }
    let op_type = buffer[cur];
    advance(&mut cur, 1)?;

    let operation = match op_type {
        OP_CREATE_NODE => parse_create_node_op(buffer, &mut cur, version, timestamp)?,
        OP_CREATE_EDGE => parse_create_edge_op(buffer, &mut cur, version, timestamp)?,
        OP_UPDATE_NODE => parse_update_node_op(buffer, &mut cur, version, timestamp)?,
        OP_UPDATE_EDGE => parse_update_edge_op(buffer, &mut cur, version, timestamp)?,
        OP_DELETE_NODE => parse_delete_node_op(buffer, &mut cur, version, timestamp)?,
        OP_DELETE_EDGE => parse_delete_edge_op(buffer, &mut cur, version, timestamp)?,
        OP_RETRACT_NODE => parse_retract_node_op(buffer, &mut cur, version)?,
        OP_RETRACT_EDGE => parse_retract_edge_op(buffer, &mut cur, version)?,
        OP_CHECKPOINT => parse_checkpoint_op(buffer, &mut cur)?,
        OP_DECLARE_UNIQUE_CONSTRAINT => {
            let label = read_label(buffer, &mut cur, "DeclareUniqueConstraint.label")?;
            let property = read_label(buffer, &mut cur, "DeclareUniqueConstraint.property")?;
            WalOperation::DeclareUniqueConstraint { label, property }
        }
        OP_DROP_UNIQUE_CONSTRAINT => {
            let label = read_label(buffer, &mut cur, "DropUniqueConstraint.label")?;
            let property = read_label(buffer, &mut cur, "DropUniqueConstraint.property")?;
            WalOperation::DropUniqueConstraint { label, property }
        }
        OP_BEGIN_TX => parse_begin_tx_op(buffer, &mut cur)?,
        OP_COMMIT_TX => parse_commit_tx_op(buffer, &mut cur)?,
        _ => {
            return Err(StorageError::CorruptedData(format!(
                "Unknown WAL operation type: {}",
                op_type
            ))
            .into());
        }
    };

    // Verify checksum to ensure data integrity (critical for WAL correctness)
    let mut hasher = crc32fast::Hasher::new();
    // Hash LSN (8 bytes) + timestamp (12 bytes) = bytes 0..20
    hasher.update(&buffer[start_offset..start_offset + 20]);
    // Hash operation data (from after checksum field to end of entry)
    hasher.update(&buffer[start_offset + 24..cur]);
    let computed_checksum = hasher.finalize();

    if checksum != computed_checksum {
        return Err(StorageError::CorruptedData(format!(
            "WAL entry checksum mismatch for LSN {}: expected {:#x}, got {:#x}. Entry is corrupted.",
            lsn.0, checksum, computed_checksum
        ))
        .into());
    }

    let entry = WalEntry {
        lsn,
        timestamp,
        operation,
        checksum,
        // Segments at or above WAL_VERSION_TX_FRAMING carry transaction
        // framing markers; `version` here is the plaintext/payload version
        // (encrypted container versions are mapped via `payload_version`
        // before reaching this function), so the comparison is uniform.
        framed: is_framed_version(version),
    };
    let bytes_consumed = cur - start_offset;
    Ok((entry, bytes_consumed))
}

/// Helper to deserialize and validate a NodeId from WAL buffer
#[inline]
fn deserialize_node_id(buffer: &[u8], offset: usize, context: &str) -> Result<NodeId> {
    let bytes = buffer.get(offset..offset + 8).ok_or_else(|| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Insufficient buffer size for NodeId in {}",
            context
        )))
    })?;
    let raw_id = u64::from_le_bytes(bytes.try_into().unwrap());
    NodeId::new(raw_id).map_err(|e| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Invalid node ID in WAL {}: {}",
            context, e
        )))
    })
}

/// Helper to deserialize and validate an EdgeId from WAL buffer
#[inline]
fn deserialize_edge_id(buffer: &[u8], offset: usize, context: &str) -> Result<EdgeId> {
    let bytes = buffer.get(offset..offset + 8).ok_or_else(|| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Insufficient buffer size for EdgeId in {}",
            context
        )))
    })?;
    let raw_id = u64::from_le_bytes(bytes.try_into().unwrap());
    EdgeId::new(raw_id).map_err(|e| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Invalid edge ID in WAL {}: {}",
            context, e
        )))
    })
}

/// Helper to deserialize and validate a VersionId from WAL buffer
#[inline]
fn deserialize_version_id(buffer: &[u8], offset: usize, context: &str) -> Result<VersionId> {
    let bytes = buffer.get(offset..offset + 8).ok_or_else(|| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Insufficient buffer size for VersionId in {}",
            context
        )))
    })?;
    let raw_id = u64::from_le_bytes(bytes.try_into().unwrap());
    VersionId::new(raw_id).map_err(|e| {
        Error::Storage(StorageError::CorruptedData(format!(
            "Invalid version ID in WAL {}: {}",
            context, e
        )))
    })
}

#[cfg(test)]
mod fuzz_tests;
#[cfg(test)]
mod regression_tests;
#[cfg(test)]
mod sentry_tests;
#[cfg(test)]
mod tests;
