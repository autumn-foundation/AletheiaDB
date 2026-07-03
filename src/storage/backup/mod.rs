//! Portable backup and restore for AletheiaDB (issue #3217).
//!
//! # Artifact Format
//!
//! ```text
//! [MAGIC: 4 bytes "ALBK"][FORMAT_VERSION: 2 bytes u16 LE][PAYLOAD: zstd-compressed bitcode]
//! ```
//!
//! The PAYLOAD is a `BackupPayload` struct encoded with [bitcode](https://github.com/llogiq/bitcode)
//! and compressed with zstd.  A single file is written atomically (temp + rename) so an
//! interrupted backup never leaves a partial file at the target path.
//!
//! # Restore
//!
//! Restore materialises the payload as a set of index-persistence files in a temp/target
//! directory, then constructs a fresh `AletheiaDB` via `with_unified_config` with
//! `load_on_startup = true`.  This reuses the existing, tested startup load path (including
//! ID-generator re-seeding, version-chain rebuilding, and interner restoration).

use std::io::Read;
use std::path::Path;

use bitcode::{Decode, Encode};
use thiserror::Error;

use crate::core::GLOBAL_INTERNER;
use crate::storage::index_persistence::formats::IndexManifest;
use crate::storage::index_persistence::formats::{
    GraphIndexData, StringInternerData, TemporalIndexData,
};
use crate::storage::index_persistence::graph::{persist_property_map, save_graph_index};
use crate::storage::index_persistence::strings::restore_string_interner;
use crate::storage::index_persistence::temporal::{
    convert_edge_version, convert_node_version, save_temporal_index,
};
use crate::storage::index_persistence::{
    INTERNER_MAGIC, IndexPersistenceManager, MANIFEST_VERSION, TEMPORAL_MAGIC,
};
use crate::storage::snapshot::{CurrentStorageSnapshot, HistoricalStorageSnapshot};

/// Magic bytes identifying an AletheiaDB backup artifact.
pub const BACKUP_MAGIC: [u8; 4] = *b"ALBK";

/// Current backup format version.
///
/// Bumped to 2 for write-time provenance on temporal versions (Issue #3224):
/// the embedded `TemporalIndexData`'s `NodeVersionEntry`/`EdgeVersionEntry`
/// gained an optional `provenance` field. Version 1 artifacts are still
/// restorable -- see [`BackupPayloadV1`] and `read_artifact`.
pub const BACKUP_FORMAT_VERSION: u16 = 2;

/// Maximum allowed decompressed payload size (5 GiB).
///
/// Enforced during restore to mitigate decompression-bomb denial-of-service:
/// a maliciously crafted small `.albk` file could otherwise decompress into
/// gigabytes of data and exhaust process memory.
const MAX_DECOMPRESSED_PAYLOAD_BYTES: u64 = 5 * 1024 * 1024 * 1024;

// ============================================================================
// Public error type
// ============================================================================

/// Errors that can occur during backup or restore operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BackupError {
    /// I/O error during reading or writing the backup artifact.
    #[error("I/O error: {0}")]
    Io(String),
    /// Serialization or deserialization failed.
    #[error("Serialization error: {0}")]
    Serialization(String),
    /// The file does not have the expected `ALBK` magic bytes.
    #[error("Invalid magic bytes: not an AletheiaDB backup file")]
    BadMagic,
    /// The backup was created with an incompatible format version.
    #[error(
        "Incompatible backup format version: found {found}, supported {supported}. \
         Please use a newer AletheiaDB version to restore this backup."
    )]
    IncompatibleVersion {
        /// The version found in the artifact.
        found: u16,
        /// The version supported by this build.
        supported: u16,
    },
    /// The target restore directory is non-empty; refusing to overwrite existing data.
    #[error(
        "Restore target directory is not empty (contains existing index files or an \
         in-progress restore sentinel). Use an empty directory or a fresh ephemeral database."
    )]
    TargetNotEmpty,
    /// The backup artifact is corrupt (bad checksum, truncated, or invalid data).
    #[error("Corrupt backup data: {0}")]
    Corrupt(String),
}

// ============================================================================
// BackupSummary (public)
// ============================================================================

/// Summary returned by `AletheiaDB::backup`.
#[derive(Debug, Clone)]
pub struct BackupSummary {
    /// Number of node versions captured (hot + cold tiers).
    pub node_versions: u64,
    /// Number of edge versions captured (hot + cold tiers).
    pub edge_versions: u64,
    /// Number of current (live) nodes.
    pub current_node_count: u64,
    /// Number of current (live) edges.
    pub current_edge_count: u64,
    /// Total bytes written to the artifact file.
    pub bytes_written: usize,
    /// LSN at which the consistent snapshot was taken.
    pub source_lsn: u64,
}

// ============================================================================
// BackupPayload (internal, bitcode-encoded)
// ============================================================================

/// The complete serializable payload stored inside a backup artifact.
#[derive(Debug, Clone, Encode, Decode)]
pub(crate) struct BackupPayload {
    /// Unix timestamp (microseconds) when the backup was created.
    pub created_at_micros: i64,
    /// WAL LSN at which the consistent snapshot was taken.
    pub source_lsn: u64,
    /// Number of current nodes.
    pub current_node_count: u64,
    /// Number of current edges.
    pub current_edge_count: u64,
    /// Number of node versions (hot + cold).
    pub node_version_count: u64,
    /// Number of edge versions (hot + cold).
    pub edge_version_count: u64,
    /// String interner state.
    pub interner: StringInternerData,
    /// Current graph state (nodes and edges).
    pub graph: GraphIndexData,
    /// Complete temporal version history.
    pub temporal: TemporalIndexData,
}

/// Pre-provenance (Issue #3224) `BackupPayload` shape, i.e. `BACKUP_FORMAT_VERSION == 1`.
///
/// Identical to [`BackupPayload`] except `temporal` uses the frozen
/// [`crate::storage::index_persistence::formats::legacy_v1::TemporalIndexDataV1`]
/// shape. Kept only so `read_artifact` can restore version-1 artifacts.
#[derive(Debug, Clone, Encode, Decode)]
pub(crate) struct BackupPayloadV1 {
    /// Unix timestamp (microseconds) when the backup was created.
    pub created_at_micros: i64,
    /// WAL LSN at which the consistent snapshot was taken.
    pub source_lsn: u64,
    /// Number of current nodes.
    pub current_node_count: u64,
    /// Number of current edges.
    pub current_edge_count: u64,
    /// Number of node versions (hot + cold).
    pub node_version_count: u64,
    /// Number of edge versions (hot + cold).
    pub edge_version_count: u64,
    /// String interner state.
    pub interner: StringInternerData,
    /// Current graph state (nodes and edges).
    pub graph: GraphIndexData,
    /// Complete temporal version history (pre-provenance shape).
    pub temporal: crate::storage::index_persistence::formats::legacy_v1::TemporalIndexDataV1,
}

impl From<BackupPayloadV1> for BackupPayload {
    fn from(v1: BackupPayloadV1) -> Self {
        BackupPayload {
            created_at_micros: v1.created_at_micros,
            source_lsn: v1.source_lsn,
            current_node_count: v1.current_node_count,
            current_edge_count: v1.current_edge_count,
            node_version_count: v1.node_version_count,
            edge_version_count: v1.edge_version_count,
            interner: v1.interner,
            graph: v1.graph,
            temporal: v1.temporal.into(),
        }
    }
}

// ============================================================================
// Snapshot → payload builders
// ============================================================================

/// Extract `GraphIndexData` from a `CurrentStorageSnapshot`.
///
/// Delegates to the canonical implementation in
/// `crate::storage::index_persistence::graph::extract_graph_data_from_snapshot`.
fn build_graph_data(snapshot: &CurrentStorageSnapshot) -> Result<GraphIndexData, BackupError> {
    crate::storage::index_persistence::graph::extract_graph_data_from_snapshot(snapshot)
        .map_err(|e| BackupError::Serialization(e.to_string()))
}

/// Extract `TemporalIndexData` from a `HistoricalStorageSnapshot` and optional
/// cold-tier versions.  Deduplicates by `version_id` so cold and hot tiers can
/// be merged without double-counting.
fn build_temporal_data(
    snapshot: &HistoricalStorageSnapshot,
    cold_node_versions: Vec<crate::core::version::NodeVersion>,
    cold_edge_versions: Vec<crate::core::version::EdgeVersion>,
) -> Result<TemporalIndexData, BackupError> {
    use std::collections::HashSet;

    let mut node_version_ids: HashSet<u64> = HashSet::new();
    let mut edge_version_ids: HashSet<u64> = HashSet::new();

    let mut node_versions = Vec::new();
    let mut node_anchors = Vec::new();
    let mut edge_versions = Vec::new();
    let mut edge_anchors = Vec::new();

    // Hot-tier node versions.
    for v_arc in snapshot.iter_node_versions() {
        let v = &*v_arc;
        if node_version_ids.insert(v.id.as_u64()) {
            let entry =
                convert_node_version(v).map_err(|e| BackupError::Serialization(e.to_string()))?;
            if matches!(
                entry.version_type,
                crate::storage::index_persistence::formats::PersistedVersionType::Anchor
            ) {
                node_anchors.push(build_node_anchor_entry(v)?);
            }
            node_versions.push(entry);
        }
    }

    // Cold-tier node versions (fold in, deduplicated by version_id).
    for v in cold_node_versions {
        if node_version_ids.insert(v.id.as_u64()) {
            let entry =
                convert_node_version(&v).map_err(|e| BackupError::Serialization(e.to_string()))?;
            if matches!(
                entry.version_type,
                crate::storage::index_persistence::formats::PersistedVersionType::Anchor
            ) {
                node_anchors.push(build_node_anchor_entry(&v)?);
            }
            node_versions.push(entry);
        }
    }

    // Hot-tier edge versions.
    for v_arc in snapshot.iter_edge_versions() {
        let v = &*v_arc;
        if edge_version_ids.insert(v.id.as_u64()) {
            let entry =
                convert_edge_version(v).map_err(|e| BackupError::Serialization(e.to_string()))?;
            if matches!(
                entry.version_type,
                crate::storage::index_persistence::formats::PersistedVersionType::Anchor
            ) {
                edge_anchors.push(build_edge_anchor_entry(v)?);
            }
            edge_versions.push(entry);
        }
    }

    // Cold-tier edge versions.
    for v in cold_edge_versions {
        if edge_version_ids.insert(v.id.as_u64()) {
            let entry =
                convert_edge_version(&v).map_err(|e| BackupError::Serialization(e.to_string()))?;
            if matches!(
                entry.version_type,
                crate::storage::index_persistence::formats::PersistedVersionType::Anchor
            ) {
                edge_anchors.push(build_edge_anchor_entry(&v)?);
            }
            edge_versions.push(entry);
        }
    }

    Ok(TemporalIndexData {
        magic: TEMPORAL_MAGIC,
        version: MANIFEST_VERSION,
        node_versions,
        node_anchors,
        edge_versions,
        edge_anchors,
    })
}

fn build_node_anchor_entry(
    v: &crate::core::version::NodeVersion,
) -> Result<crate::storage::index_persistence::formats::NodeAnchorEntry, BackupError> {
    use crate::core::version::VersionData;
    let full_state = match &v.data {
        VersionData::Anchor { properties, .. } => persist_property_map(properties)
            .map_err(|e| BackupError::Serialization(e.to_string()))?,
        VersionData::Delta { .. } => {
            return Err(BackupError::Serialization(format!(
                "Expected Anchor version data for anchor entry, node version id={}",
                v.id.as_u64()
            )));
        }
    };
    Ok(
        crate::storage::index_persistence::formats::NodeAnchorEntry {
            node_id: v.node_id.as_u64(),
            anchor_tx_time: v.temporal.transaction_time().start().wallclock(),
            full_state,
            vector_snapshot_id: None,
        },
    )
}

fn build_edge_anchor_entry(
    v: &crate::core::version::EdgeVersion,
) -> Result<crate::storage::index_persistence::formats::EdgeAnchorEntry, BackupError> {
    use crate::core::version::VersionData;
    let full_state = match &v.data {
        VersionData::Anchor { properties, .. } => persist_property_map(properties)
            .map_err(|e| BackupError::Serialization(e.to_string()))?,
        VersionData::Delta { .. } => {
            return Err(BackupError::Serialization(format!(
                "Expected Anchor version data for anchor entry, edge version id={}",
                v.id.as_u64()
            )));
        }
    };
    Ok(
        crate::storage::index_persistence::formats::EdgeAnchorEntry {
            edge_id: v.edge_id.as_u64(),
            anchor_tx_time: v.temporal.transaction_time().start().wallclock(),
            full_state,
        },
    )
}

/// Capture current string interner state as `StringInternerData`.
fn capture_interner_data() -> StringInternerData {
    let strings = GLOBAL_INTERNER.get_all_strings();
    StringInternerData {
        magic: INTERNER_MAGIC,
        version: MANIFEST_VERSION,
        string_count: strings.len() as u64,
        strings,
    }
}

// ============================================================================
// Artifact I/O
// ============================================================================

/// Serialize a `BackupPayload` into a compressed backup artifact.
///
/// Format: `[ALBK magic 4B][version 2B u16 LE][zstd-compressed bitcode]`
pub(crate) fn encode_artifact(payload: &BackupPayload) -> Result<Vec<u8>, BackupError> {
    let encoded = bitcode::encode(payload);

    let compressed = zstd::encode_all(encoded.as_slice(), 3)
        .map_err(|e| BackupError::Io(format!("zstd compression failed: {e}")))?;

    let mut out = Vec::with_capacity(6 + compressed.len());
    out.extend_from_slice(&BACKUP_MAGIC);
    out.extend_from_slice(&BACKUP_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&compressed);
    Ok(out)
}

/// Write a backup artifact to `path` using an atomic temp-write-then-rename.
///
/// Returns the number of bytes written.
pub(crate) fn write_artifact(payload: &BackupPayload, path: &Path) -> Result<usize, BackupError> {
    let data = encode_artifact(payload)?;
    let bytes_written = data.len();
    crate::storage::index_persistence::atomic_write(path, &data)
        .map_err(|e| BackupError::Io(e.to_string()))?;
    Ok(bytes_written)
}

/// Read and validate a backup artifact from `path`.
///
/// Validates magic bytes and format version before decompressing and decoding.
/// Uses streaming decompression so peak memory is the decompressed payload only,
/// not compressed + decompressed simultaneously.
pub(crate) fn read_artifact(path: &Path) -> Result<BackupPayload, BackupError> {
    let mut file = std::fs::File::open(path).map_err(|e| BackupError::Io(e.to_string()))?;

    // Read the 6-byte header with read_exact — avoids buffering the full file.
    let mut header = [0u8; 6];
    file.read_exact(&mut header).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            BackupError::Corrupt("Artifact too short to contain header".to_string())
        } else {
            BackupError::Io(e.to_string())
        }
    })?;

    // Validate magic.
    if header[..4] != BACKUP_MAGIC {
        return Err(BackupError::BadMagic);
    }

    // Validate format version. Older (but still-decodable) versions are
    // accepted -- see `BackupPayloadV1` below for the version-1 fallback
    // (Issue #3224); only a version newer than this build understands is
    // rejected.
    let found_version = u16::from_le_bytes([header[4], header[5]]);
    if found_version > BACKUP_FORMAT_VERSION {
        return Err(BackupError::IncompatibleVersion {
            found: found_version,
            supported: BACKUP_FORMAT_VERSION,
        });
    }

    // Stream-decompress the payload — peak memory = decompressed size only.
    let mut decoder = zstd::stream::Decoder::new(file)
        .map_err(|e| BackupError::Corrupt(format!("zstd stream init failed: {e}")))?;
    let mut decoded_bytes = Vec::new();
    decoder
        .by_ref()
        .take(MAX_DECOMPRESSED_PAYLOAD_BYTES)
        .read_to_end(&mut decoded_bytes)
        .map_err(|e| BackupError::Corrupt(format!("zstd decompression failed: {e}")))?;

    // If we read exactly the take limit the payload was truncated — the real
    // decompressed size may be larger (decompression-bomb guard).
    if decoded_bytes.len() as u64 >= MAX_DECOMPRESSED_PAYLOAD_BYTES {
        return Err(BackupError::Corrupt(
            "Decompressed payload exceeds size limit (possible decompression bomb)".to_string(),
        ));
    }

    // Decode bitcode. The header's `found_version` (already validated above,
    // and not itself part of the bitcode blob) tells us unambiguously which
    // payload shape to expect -- no try-decode-and-fallback needed here,
    // unlike the temporal index file's embedded version (Issue #3224).
    if found_version == 1 {
        let legacy: BackupPayloadV1 = bitcode::decode(&decoded_bytes).map_err(|e| {
            BackupError::Serialization(format!("bitcode deserialization failed: {e}"))
        })?;
        return Ok(legacy.into());
    }

    let payload: BackupPayload = bitcode::decode(&decoded_bytes)
        .map_err(|e| BackupError::Serialization(format!("bitcode deserialization failed: {e}")))?;

    Ok(payload)
}

// ============================================================================
// Materialise payload → index-persistence directory
// ============================================================================

/// Write a `BackupPayload` as a complete set of index-persistence files under
/// `data_dir`, suitable for loading via `load_indexes_startup`.
///
/// A `.restore-in-progress` sentinel is written first and removed last (after
/// the manifest is committed).  Any crash between those two points leaves the
/// sentinel in place, which `check_target_empty` treats as a non-empty target —
/// preventing a future restore from silently overwriting partially-written data.
pub(crate) fn materialize_to_dir(
    payload: &BackupPayload,
    data_dir: &Path,
) -> Result<(), BackupError> {
    let manager = IndexPersistenceManager::new(data_dir);
    manager
        .ensure_directories()
        .map_err(|e| BackupError::Io(e.to_string()))?;

    // Write sentinel FIRST — any crash before the final remove leaves this file,
    // so check_target_empty will refuse a subsequent restore on the same dir.
    let sentinel = manager.indexes_path().join(".restore-in-progress");
    std::fs::write(&sentinel, b"")
        .map_err(|e| BackupError::Io(format!("failed to write restore sentinel: {e}")))?;

    // 1. String interner (must come first; graph + temporal resolve string indices).
    // Clear before restoring so this process's interner matches the backup's ID layout.
    GLOBAL_INTERNER.clear();
    restore_string_interner(&payload.interner)
        .map_err(|e| BackupError::Serialization(e.to_string()))?;

    // 2. Write interner file.
    let interner_path = manager.interner_path();
    crate::storage::index_persistence::common::save_encoded_with_crc(
        &payload.interner,
        &interner_path,
    )
    .map_err(|e| BackupError::Io(e.to_string()))?;

    // 3. Graph index.
    let graph_path = manager.graph_path().join("adjacency.idx");
    save_graph_index(&payload.graph, &graph_path).map_err(|e| BackupError::Io(e.to_string()))?;

    // 4. Temporal index.
    let temporal_path = manager.temporal_path().join("versions.idx");
    save_temporal_index(&payload.temporal, &temporal_path)
        .map_err(|e| BackupError::Io(e.to_string()))?;

    // 5. Manifest (written last — acts as the committed marker).
    let manifest = IndexManifest::new(payload.source_lsn);
    manager
        .save_manifest(&manifest)
        .map_err(|e| BackupError::Io(e.to_string()))?;

    // Remove sentinel LAST — after manifest is on disk.
    // Failure is non-fatal (data is intact) but should be logged so operators
    // know why a subsequent restore on the same dir would see TargetNotEmpty.
    if let Err(_e) = std::fs::remove_file(&sentinel) {
        #[cfg(feature = "observability")]
        tracing::warn!(
            "Failed to remove restore sentinel at {}: {_e}",
            sentinel.display()
        );
    }

    Ok(())
}

// ============================================================================
// High-level build / restore entrypoints (called by src/db/backup.rs)
// ============================================================================

/// Build a `BackupPayload` from a consistent snapshot.
///
/// The caller must hold a read lock on `HistoricalStorage` long enough to call
/// `create_snapshot`, then release it before calling this function (cold I/O
/// should not be done while holding the historical lock).
pub(crate) fn build_payload(
    current_snapshot: CurrentStorageSnapshot,
    historical_snapshot: HistoricalStorageSnapshot,
    cold_node_versions: Vec<crate::core::version::NodeVersion>,
    cold_edge_versions: Vec<crate::core::version::EdgeVersion>,
    source_lsn: u64,
    created_at_micros: i64,
) -> Result<BackupPayload, BackupError> {
    let current_node_count = current_snapshot.node_count() as u64;
    let current_edge_count = current_snapshot.edge_count() as u64;

    let graph = build_graph_data(&current_snapshot)?;
    let temporal =
        build_temporal_data(&historical_snapshot, cold_node_versions, cold_edge_versions)?;

    let node_version_count = temporal.node_versions.len() as u64;
    let edge_version_count = temporal.edge_versions.len() as u64;
    let interner = capture_interner_data();

    Ok(BackupPayload {
        created_at_micros,
        source_lsn,
        current_node_count,
        current_edge_count,
        node_version_count,
        edge_version_count,
        interner,
        graph,
        temporal,
    })
}

/// Check whether a target data directory is non-empty (has a `manifest.idx` or
/// a `.restore-in-progress` sentinel from a previous interrupted restore).
///
/// Returns `Err(BackupError::TargetNotEmpty)` if the target is occupied.
pub(crate) fn check_target_empty(data_dir: &Path) -> Result<(), BackupError> {
    let manager = IndexPersistenceManager::new(data_dir);
    let sentinel = manager.indexes_path().join(".restore-in-progress");
    if manager.indexes_exist() || sentinel.exists() {
        return Err(BackupError::TargetNotEmpty);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // read_artifact error paths
    // -----------------------------------------------------------------------

    #[test]
    fn read_artifact_empty_file_yields_corrupt() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.albk");
        std::fs::write(&path, b"").unwrap();
        let err = read_artifact(&path).unwrap_err();
        assert!(
            matches!(err, BackupError::Corrupt(_)),
            "expected Corrupt, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("too short") || msg.contains("Corrupt"),
            "{msg}"
        );
    }

    #[test]
    fn read_artifact_partial_header_yields_corrupt() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("partial.albk");
        // 3 bytes — too short for the 6-byte header
        std::fs::write(&path, b"ALB").unwrap();
        let err = read_artifact(&path).unwrap_err();
        assert!(
            matches!(err, BackupError::Corrupt(_)),
            "expected Corrupt for partial header, got: {err:?}"
        );
    }

    #[test]
    fn read_artifact_bad_zstd_yields_corrupt() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("badzstd.albk");
        // Valid magic + version, then garbage that is not valid zstd.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&BACKUP_MAGIC);
        bytes.extend_from_slice(&BACKUP_FORMAT_VERSION.to_le_bytes());
        bytes.extend_from_slice(b"NOT_VALID_ZSTD_DATA_AT_ALL");
        std::fs::write(&path, &bytes).unwrap();
        let err = read_artifact(&path).unwrap_err();
        assert!(
            matches!(err, BackupError::Corrupt(_)),
            "expected Corrupt for bad zstd, got: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // check_target_empty sentinel detection
    // -----------------------------------------------------------------------

    #[test]
    fn check_target_empty_detects_sentinel() {
        let dir = TempDir::new().unwrap();
        let manager = IndexPersistenceManager::new(dir.path());
        manager.ensure_directories().unwrap();
        let sentinel = manager.indexes_path().join(".restore-in-progress");
        std::fs::write(&sentinel, b"").unwrap();

        let err = check_target_empty(dir.path()).unwrap_err();
        assert!(matches!(err, BackupError::TargetNotEmpty));
    }

    #[test]
    fn check_target_empty_succeeds_on_fresh_dir() {
        let dir = TempDir::new().unwrap();
        assert!(check_target_empty(dir.path()).is_ok());
    }

    // -----------------------------------------------------------------------
    // build_*_anchor_entry rejects Delta versions
    // -----------------------------------------------------------------------

    fn make_temporal() -> crate::core::temporal::BiTemporalInterval {
        use crate::core::temporal::{BiTemporalInterval, TimeRange};
        BiTemporalInterval::new(TimeRange::from(0.into()), TimeRange::from(0.into()))
    }

    #[test]
    fn build_node_anchor_rejects_delta_version() {
        use crate::core::id::{NodeId, VersionId};
        use crate::core::version::NodeVersion;
        use crate::core::{InternedString, PropertyMap};

        let id = VersionId::new_unchecked(1);
        let node_id = NodeId::new_unchecked(1);
        let prev = VersionId::new_unchecked(0);
        let v = NodeVersion::new_delta(
            id,
            node_id,
            make_temporal(),
            InternedString::from_raw(0),
            &PropertyMap::new(),
            &PropertyMap::new(),
            prev,
        );

        let err = build_node_anchor_entry(&v).unwrap_err();
        assert!(matches!(err, BackupError::Serialization(_)));
    }

    #[test]
    fn build_edge_anchor_rejects_delta_version() {
        use crate::core::id::{EdgeId, NodeId, VersionId};
        use crate::core::version::EdgeVersion;
        use crate::core::{InternedString, PropertyMap};

        let id = VersionId::new_unchecked(1);
        let edge_id = EdgeId::new_unchecked(1);
        let source = NodeId::new_unchecked(1);
        let target = NodeId::new_unchecked(2);
        let prev = VersionId::new_unchecked(0);
        let v = EdgeVersion::new_delta(
            id,
            edge_id,
            make_temporal(),
            InternedString::from_raw(0),
            source,
            target,
            &PropertyMap::new(),
            &PropertyMap::new(),
            prev,
        );

        let err = build_edge_anchor_entry(&v).unwrap_err();
        assert!(matches!(err, BackupError::Serialization(_)));
    }

    // -----------------------------------------------------------------------
    // encode_artifact / encode+decode roundtrip
    // -----------------------------------------------------------------------

    fn empty_payload() -> BackupPayload {
        use crate::storage::index_persistence::formats::{GraphIndexData, TemporalIndexData};
        use crate::storage::index_persistence::{
            GRAPH_MAGIC, INTERNER_MAGIC, MANIFEST_VERSION, TEMPORAL_MAGIC,
        };
        BackupPayload {
            created_at_micros: 0,
            source_lsn: 0,
            current_node_count: 0,
            current_edge_count: 0,
            node_version_count: 0,
            edge_version_count: 0,
            interner: crate::storage::index_persistence::formats::StringInternerData {
                magic: INTERNER_MAGIC,
                version: MANIFEST_VERSION,
                string_count: 0,
                strings: vec![],
            },
            graph: GraphIndexData {
                magic: GRAPH_MAGIC,
                version: MANIFEST_VERSION,
                node_count: 0,
                edge_count: 0,
                nodes: vec![],
                edges: vec![],
                outgoing_node_ids: vec![],
                outgoing_offsets: vec![],
                outgoing_neighbors: vec![],
                incoming_node_ids: vec![],
                incoming_offsets: vec![],
                incoming_neighbors: vec![],
            },
            temporal: TemporalIndexData {
                magic: TEMPORAL_MAGIC,
                version: MANIFEST_VERSION,
                node_versions: vec![],
                node_anchors: vec![],
                edge_versions: vec![],
                edge_anchors: vec![],
            },
        }
    }

    #[test]
    fn encode_artifact_produces_valid_header() {
        let payload = empty_payload();
        let bytes = encode_artifact(&payload).unwrap();
        assert!(bytes.len() >= 6);
        assert_eq!(&bytes[..4], &BACKUP_MAGIC);
        assert_eq!(
            u16::from_le_bytes([bytes[4], bytes[5]]),
            BACKUP_FORMAT_VERSION
        );
    }

    #[test]
    fn encode_then_decode_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rt.albk");
        let payload = empty_payload();
        write_artifact(&payload, &path).unwrap();
        let restored = read_artifact(&path).unwrap();
        assert_eq!(restored.source_lsn, 0);
        assert_eq!(restored.current_node_count, 0);
    }

    // -----------------------------------------------------------------------
    // Version-1 (pre-provenance, Issue #3224) backup compatibility
    // -----------------------------------------------------------------------

    fn empty_payload_v1() -> BackupPayloadV1 {
        use crate::storage::index_persistence::formats::legacy_v1::TemporalIndexDataV1;
        use crate::storage::index_persistence::formats::{GraphIndexData, StringInternerData};
        use crate::storage::index_persistence::{
            GRAPH_MAGIC, INTERNER_MAGIC, MANIFEST_VERSION, TEMPORAL_MAGIC,
        };
        BackupPayloadV1 {
            created_at_micros: 42,
            source_lsn: 7,
            current_node_count: 0,
            current_edge_count: 0,
            node_version_count: 0,
            edge_version_count: 0,
            interner: StringInternerData {
                magic: INTERNER_MAGIC,
                version: MANIFEST_VERSION,
                string_count: 0,
                strings: vec![],
            },
            graph: GraphIndexData {
                magic: GRAPH_MAGIC,
                version: MANIFEST_VERSION,
                node_count: 0,
                edge_count: 0,
                nodes: vec![],
                edges: vec![],
                outgoing_node_ids: vec![],
                outgoing_offsets: vec![],
                outgoing_neighbors: vec![],
                incoming_node_ids: vec![],
                incoming_offsets: vec![],
                incoming_neighbors: vec![],
            },
            temporal: TemporalIndexDataV1 {
                magic: TEMPORAL_MAGIC,
                version: 1,
                node_versions: vec![],
                node_anchors: vec![],
                edge_versions: vec![],
                edge_anchors: vec![],
            },
        }
    }

    /// Encode a version-1 artifact byte-for-byte the way pre-#3224 AletheiaDB
    /// would have: `[MAGIC][version=1][zstd(bitcode(BackupPayloadV1))]`.
    fn encode_artifact_v1(payload: &BackupPayloadV1) -> Vec<u8> {
        let encoded = bitcode::encode(payload);
        let compressed = zstd::encode_all(encoded.as_slice(), 3).unwrap();
        let mut out = Vec::with_capacity(6 + compressed.len());
        out.extend_from_slice(&BACKUP_MAGIC);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&compressed);
        out
    }

    #[test]
    fn read_artifact_accepts_legacy_v1_format() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("legacy.albk");
        let bytes = encode_artifact_v1(&empty_payload_v1());
        std::fs::write(&path, &bytes).unwrap();

        let restored = read_artifact(&path).unwrap();

        assert_eq!(restored.source_lsn, 7);
        assert_eq!(restored.created_at_micros, 42);
        assert!(restored.temporal.node_versions.is_empty());
    }

    #[test]
    fn read_artifact_rejects_future_version() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("future.albk");
        let mut bytes = encode_artifact(&empty_payload()).unwrap();
        // Corrupt the header to claim a version newer than this build supports.
        bytes[4..6].copy_from_slice(&(BACKUP_FORMAT_VERSION + 1).to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();

        let err = read_artifact(&path).unwrap_err();
        assert!(matches!(err, BackupError::IncompatibleVersion { .. }));
    }
}
