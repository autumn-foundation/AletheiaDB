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

use std::io::{Read, Write};
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
    GRAPH_MAGIC, INTERNER_MAGIC, IndexPersistenceManager, MANIFEST_VERSION, TEMPORAL_MAGIC,
};
use crate::storage::snapshot::{CurrentStorageSnapshot, HistoricalStorageSnapshot};

/// Magic bytes identifying an AletheiaDB backup artifact.
pub const BACKUP_MAGIC: [u8; 4] = *b"ALBK";

/// Current backup format version.
pub const BACKUP_FORMAT_VERSION: u16 = 1;

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
        "Restore target directory is not empty (contains a manifest.idx). \
         Use an empty directory or a fresh ephemeral database."
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

// ============================================================================
// Snapshot → payload builders
// ============================================================================

/// Extract `GraphIndexData` from a `CurrentStorageSnapshot`.
///
/// Mirrors `CheckpointManager::extract_graph_data_from_snapshot` but is kept
/// independent so the backup module does not depend on checkpoint internals.
fn build_graph_data(snapshot: &CurrentStorageSnapshot) -> Result<GraphIndexData, BackupError> {
    let mut nodes = Vec::with_capacity(snapshot.node_count());
    let mut edges = Vec::with_capacity(snapshot.edge_count());

    for node in snapshot.iter_nodes() {
        let properties = persist_property_map(&node.properties)
            .map_err(|e| BackupError::Serialization(e.to_string()))?;
        nodes.push(crate::storage::index_persistence::formats::PersistedNode {
            id: node.id.as_u64(),
            label_idx: node.label.as_u32(),
            version_id: node.current_version.as_u64(),
            properties,
        });
    }

    for edge in snapshot.iter_edges() {
        let properties = persist_property_map(&edge.properties)
            .map_err(|e| BackupError::Serialization(e.to_string()))?;
        edges.push(crate::storage::index_persistence::formats::PersistedEdge {
            id: edge.id.as_u64(),
            source_id: edge.source.as_u64(),
            target_id: edge.target.as_u64(),
            label_idx: edge.label.as_u32(),
            version_id: edge.current_version.as_u64(),
            properties,
        });
    }

    Ok(GraphIndexData {
        magic: GRAPH_MAGIC,
        version: MANIFEST_VERSION,
        node_count: nodes.len() as u64,
        edge_count: edges.len() as u64,
        nodes,
        edges,
        outgoing_node_ids: Vec::new(),
        outgoing_offsets: Vec::new(),
        outgoing_neighbors: Vec::new(),
        incoming_node_ids: Vec::new(),
        incoming_offsets: Vec::new(),
        incoming_neighbors: Vec::new(),
    })
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
    use rand::Rng;
    let data = encode_artifact(payload)?;
    let bytes_written = data.len();

    let suffix: u32 = rand::thread_rng().r#gen();
    let temp_path = path.with_extension(format!("albk.{}.tmp", suffix));

    {
        let mut file =
            std::fs::File::create(&temp_path).map_err(|e| BackupError::Io(e.to_string()))?;
        file.write_all(&data)
            .map_err(|e| BackupError::Io(e.to_string()))?;
        file.sync_all()
            .map_err(|e| BackupError::Io(e.to_string()))?;
    }

    std::fs::rename(&temp_path, path).map_err(|e| BackupError::Io(e.to_string()))?;
    Ok(bytes_written)
}

/// Read and validate a backup artifact from `path`.
///
/// Validates magic bytes and format version before decompressing and decoding.
pub(crate) fn read_artifact(path: &Path) -> Result<BackupPayload, BackupError> {
    let mut file = std::fs::File::open(path).map_err(|e| BackupError::Io(e.to_string()))?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw)
        .map_err(|e| BackupError::Io(e.to_string()))?;

    if raw.len() < 6 {
        return Err(BackupError::Corrupt(
            "Artifact too short to contain header".to_string(),
        ));
    }

    // Validate magic.
    let magic: [u8; 4] = raw[..4].try_into().unwrap();
    if magic != BACKUP_MAGIC {
        return Err(BackupError::BadMagic);
    }

    // Validate format version.
    let found_version = u16::from_le_bytes([raw[4], raw[5]]);
    if found_version != BACKUP_FORMAT_VERSION {
        return Err(BackupError::IncompatibleVersion {
            found: found_version,
            supported: BACKUP_FORMAT_VERSION,
        });
    }

    // Decompress payload.
    let compressed = &raw[6..];
    let decoded_bytes = zstd::decode_all(compressed)
        .map_err(|e| BackupError::Corrupt(format!("zstd decompression failed: {e}")))?;

    // Decode bitcode.
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
/// Writes the manifest **last** so that `indexes_exist()` (which checks for
/// `manifest.idx`) only returns `true` after all data is on disk — giving
/// callers a reliable atomicity signal.
pub(crate) fn materialize_to_dir(
    payload: &BackupPayload,
    data_dir: &Path,
) -> Result<(), BackupError> {
    let manager = IndexPersistenceManager::new(data_dir.to_str().unwrap_or(""));
    manager
        .ensure_directories()
        .map_err(|e| BackupError::Io(e.to_string()))?;

    // 1. String interner (must come first; graph + temporal resolve string indices).
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

    // 5. Manifest (written last — acts as a committed marker).
    let manifest = IndexManifest::new(payload.source_lsn);
    manager
        .save_manifest(&manifest)
        .map_err(|e| BackupError::Io(e.to_string()))?;

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

/// Check whether a target data directory is non-empty (has a `manifest.idx`).
///
/// Returns `Err(BackupError::TargetNotEmpty)` if the target is occupied.
pub(crate) fn check_target_empty(data_dir: &Path) -> Result<(), BackupError> {
    let manager = IndexPersistenceManager::new(data_dir.to_str().unwrap_or(""));
    if manager.indexes_exist() {
        return Err(BackupError::TargetNotEmpty);
    }
    Ok(())
}
