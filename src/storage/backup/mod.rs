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
/// Version history:
/// - **1 -> 2** (Issue #3224): the embedded `TemporalIndexData`'s
///   `NodeVersionEntry`/`EdgeVersionEntry` gained an optional `provenance`
///   field.
/// - **2 -> 3** (Issue #3350): the embedded `PersistedProvenance` gained an
///   optional `principal` field (authenticated-principal provenance).
/// - **3 -> 4** (Issue #3387): the entries gained `tx_end`/`tx_end_logical`
///   (transaction-time closure) and `prev_version`/`next_version` (version
///   chain links).
///
/// - **4 -> 5** (Issue #3378): the payload gained `schema_constraints`
///   (declared property-type / required-key constraints).
///
/// - **5 -> 6** (Issue #3218): the payload gained `unique_constraints`
///   (declared uniqueness constraints; previously only WAL-persisted and thus
///   silently dropped by a fresh-WAL restore).
///
/// - **6 -> 7** (Issue #3665): the payload gained `keyring_sidecar` (the
///   crypto-shred subject-keyring / designation-registry sidecar bytes, so
///   designations, erased-state, `erased_at`, and attestations travel inside
///   the archive). This makes a v7 `.albk` **key-bearing** (wrapped DEKs
///   encrypted under the MEK); the field is empty when crypto-shred is unused
///   or the `audit-export` feature is off.
///
/// Older artifacts are still restorable -- see [`BackupPayloadV1`],
/// [`BackupPayloadV2`], [`BackupPayloadV3`], [`BackupPayloadV4`],
/// [`BackupPayloadV5`], [`BackupPayloadV6`] and `read_artifact`.
pub const BACKUP_FORMAT_VERSION: u16 = 7;

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
    /// A point-in-time restore (PITR, Issue #3374) target lies outside the
    /// achievable recovery window (base backup + archived WAL chain).
    ///
    /// The window is bounded below by the base backup's coordinate — PITR can
    /// only stop at-or-after the backup, never before it — and reported so the
    /// operator can pick a reachable target.
    #[error(
        "point-in-time restore target {requested} is outside the achievable window \
         [earliest={earliest}, latest={latest}]. PITR can only reach a coordinate \
         at-or-after the base backup and at-or-before the archived WAL tail."
    )]
    TargetOutsideWindow {
        /// The requested target coordinate, as a human-readable string.
        requested: String,
        /// The earliest reachable coordinate (the base backup).
        earliest: String,
        /// The latest reachable coordinate (the archived WAL tail).
        latest: String,
    },
    /// A point-in-time restore (PITR, Issue #3374) window crosses a **vocabulary
    /// change**: a transaction at-or-before the target references an interner id
    /// (a node/edge label or a property key) that the base backup's interner
    /// does not define, because that label/key was introduced *after* the base
    /// backup was taken.
    ///
    /// The WAL stores labels and property keys as raw `u32` interner ids, not
    /// strings, and the base `.albk` only carries the interner as of
    /// `source_lsn`. Replaying an out-of-range id would first dangle (resolve to
    /// nothing) and then, because the restored interner's `next_id` equals the
    /// base string count, silently **collide** with the first genuinely-new
    /// string a later write interns — mislabeling or dropping data. PITR refuses
    /// instead of corrupting: take a fresh base backup that includes the new
    /// vocabulary, or target a coordinate before the vocabulary change.
    #[error(
        "point-in-time restore window crosses a vocabulary change: a transaction \
         at-or-before the target references interner id {first_unresolved_id}, but \
         the base backup's interner only defines ids 0..{restored_interner_count} \
         (a label or property key introduced after the base backup). Replaying it \
         would silently mislabel or drop data. Take a fresh base backup that \
         includes the new vocabulary, or choose a target before the change."
    )]
    WindowCrossesVocabularyChange {
        /// The first out-of-range interner id encountered (references a string
        /// the base snapshot's interner does not contain).
        first_unresolved_id: u32,
        /// The number of strings the base backup's interner defines; valid ids
        /// are `0..restored_interner_count`.
        restored_interner_count: u32,
    },
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
    /// Declared property-type / required-key schema constraints (Issue #3378).
    /// Empty for a schemaless database. Restored through the normal startup
    /// sidecar-load path.
    pub schema_constraints: Vec<crate::core::constraint::SchemaConstraintDescriptor>,
    /// Declared uniqueness constraints (Issue #3218). Empty when none are
    /// declared. Uniqueness constraints are otherwise only WAL-persisted, so a
    /// fresh-WAL restore would drop them; they are captured here and
    /// re-declared on restore (which rebuilds the reservation index and writes
    /// a durable WAL record into the restored database).
    pub unique_constraints: Vec<crate::core::constraint::UniqueConstraintDescriptor>,
    /// CRC-wrapped `SubjectKeyringSidecar` bytes (crypto-shred keyring +
    /// designation registry, Issue #3665 fold). Empty when crypto-shred is
    /// unused or the `audit-export` feature is off. Opaque here (raw
    /// `encode_sidecar_with_crc` wire bytes) to keep `BackupPayload`
    /// feature-independent — the crypto-shred types live behind
    /// `#[cfg(feature = "audit-export")]` but this struct is always compiled.
    pub keyring_sidecar: Vec<u8>,
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
            schema_constraints: Vec::new(),
            unique_constraints: Vec::new(),
            keyring_sidecar: Vec::new(),
        }
    }
}

/// Pre-principal (Issue #3350) `BackupPayload` shape, i.e.
/// `BACKUP_FORMAT_VERSION == 2`.
///
/// Identical to [`BackupPayload`] except `temporal` uses the frozen
/// [`crate::storage::index_persistence::formats::legacy_v2::TemporalIndexDataV2`]
/// shape (provenance without the `principal` field, no tx-end / chain-link
/// fields). Kept only so `read_artifact` can restore version-2 artifacts.
///
/// Note this shape reuses the LIVE `StringInternerData`/`GraphIndexData`
/// structs (FORMAT-FROZEN, see `index_persistence::formats`): they are
/// unchanged since v1, and any future layout change to them must freeze
/// copies here first.
#[derive(Debug, Clone, Encode, Decode)]
pub(crate) struct BackupPayloadV2 {
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
    /// Complete temporal version history (pre-principal shape).
    pub temporal: crate::storage::index_persistence::formats::legacy_v2::TemporalIndexDataV2,
}

impl From<BackupPayloadV2> for BackupPayload {
    fn from(v2: BackupPayloadV2) -> Self {
        BackupPayload {
            created_at_micros: v2.created_at_micros,
            source_lsn: v2.source_lsn,
            current_node_count: v2.current_node_count,
            current_edge_count: v2.current_edge_count,
            node_version_count: v2.node_version_count,
            edge_version_count: v2.edge_version_count,
            interner: v2.interner,
            graph: v2.graph,
            temporal: v2.temporal.into(),
            schema_constraints: Vec::new(),
            unique_constraints: Vec::new(),
            keyring_sidecar: Vec::new(),
        }
    }
}

/// Pre-bi-temporal-fidelity (Issue #3387) `BackupPayload` shape, i.e.
/// `BACKUP_FORMAT_VERSION == 3`.
///
/// Identical to [`BackupPayload`] except `temporal` uses the frozen
/// [`crate::storage::index_persistence::formats::legacy_v3::TemporalIndexDataV3`]
/// shape (principal-carrying provenance, no tx-end / chain-link fields).
/// Kept only so `read_artifact` can restore version-3 artifacts.
///
/// Frozen reference: the live `BackupPayload` as of trunk commit f2e02c6
/// (the #3421 merge, the last pre-#3387 trunk commit). Note this shape
/// reuses the LIVE `StringInternerData`/`GraphIndexData` structs
/// (FORMAT-FROZEN, see `index_persistence::formats`): they are unchanged
/// since v1, and any future layout change to them must freeze copies here
/// first.
#[derive(Debug, Clone, Encode, Decode)]
pub(crate) struct BackupPayloadV3 {
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
    /// Complete temporal version history (pre-fidelity shape).
    pub temporal: crate::storage::index_persistence::formats::legacy_v3::TemporalIndexDataV3,
}

impl From<BackupPayloadV3> for BackupPayload {
    fn from(v3: BackupPayloadV3) -> Self {
        BackupPayload {
            created_at_micros: v3.created_at_micros,
            source_lsn: v3.source_lsn,
            current_node_count: v3.current_node_count,
            current_edge_count: v3.current_edge_count,
            node_version_count: v3.node_version_count,
            edge_version_count: v3.edge_version_count,
            interner: v3.interner,
            graph: v3.graph,
            temporal: v3.temporal.into(),
            schema_constraints: Vec::new(),
            unique_constraints: Vec::new(),
            keyring_sidecar: Vec::new(),
        }
    }
}

/// Pre-schema-constraint (Issue #3378) `BackupPayload` shape, i.e.
/// `BACKUP_FORMAT_VERSION == 4`.
///
/// Identical to [`BackupPayload`] but without the `schema_constraints` field
/// (it uses the LIVE `TemporalIndexData`, unchanged since v4). Kept only so
/// `read_artifact` can restore version-4 artifacts; the `From` impl defaults
/// the schema constraints to empty.
#[derive(Debug, Clone, Encode, Decode)]
pub(crate) struct BackupPayloadV4 {
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

impl From<BackupPayloadV4> for BackupPayload {
    fn from(v4: BackupPayloadV4) -> Self {
        BackupPayload {
            created_at_micros: v4.created_at_micros,
            source_lsn: v4.source_lsn,
            current_node_count: v4.current_node_count,
            current_edge_count: v4.current_edge_count,
            node_version_count: v4.node_version_count,
            edge_version_count: v4.edge_version_count,
            interner: v4.interner,
            graph: v4.graph,
            temporal: v4.temporal,
            schema_constraints: Vec::new(),
            unique_constraints: Vec::new(),
            keyring_sidecar: Vec::new(),
        }
    }
}

/// Pre-unique-constraint-backup (Issue #3218) `BackupPayload` shape, i.e.
/// `BACKUP_FORMAT_VERSION == 5`.
///
/// Identical to [`BackupPayload`] but without the `unique_constraints` field
/// (it uses the LIVE `TemporalIndexData`, unchanged since v4). Kept only so
/// `read_artifact` can restore version-5 artifacts; the `From` impl defaults
/// the uniqueness constraints to empty.
#[derive(Debug, Clone, Encode, Decode)]
pub(crate) struct BackupPayloadV5 {
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
    /// Declared property-type / required-key schema constraints (Issue #3378).
    pub schema_constraints: Vec<crate::core::constraint::SchemaConstraintDescriptor>,
}

impl From<BackupPayloadV5> for BackupPayload {
    fn from(v5: BackupPayloadV5) -> Self {
        BackupPayload {
            created_at_micros: v5.created_at_micros,
            source_lsn: v5.source_lsn,
            current_node_count: v5.current_node_count,
            current_edge_count: v5.current_edge_count,
            node_version_count: v5.node_version_count,
            edge_version_count: v5.edge_version_count,
            interner: v5.interner,
            graph: v5.graph,
            temporal: v5.temporal,
            schema_constraints: v5.schema_constraints,
            unique_constraints: Vec::new(),
            keyring_sidecar: Vec::new(),
        }
    }
}

/// Pre-keyring-fold (Issue #3665) `BackupPayload` shape, i.e.
/// `BACKUP_FORMAT_VERSION == 6`.
///
/// Identical to [`BackupPayload`] but without the `keyring_sidecar` field
/// (it uses the LIVE `TemporalIndexData`, unchanged since v4). Kept only so
/// `read_artifact` can restore version-6 artifacts; the `From` impl defaults
/// the keyring sidecar to empty — a v5/v6 archive predates the crypto-shred
/// keyring fold, so any designated properties it holds restore
/// sealed-unreadable (see the restore-path backward-compat warning).
#[derive(Debug, Clone, Encode, Decode)]
pub(crate) struct BackupPayloadV6 {
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
    /// Declared property-type / required-key schema constraints (Issue #3378).
    pub schema_constraints: Vec<crate::core::constraint::SchemaConstraintDescriptor>,
    /// Declared uniqueness constraints (Issue #3218).
    pub unique_constraints: Vec<crate::core::constraint::UniqueConstraintDescriptor>,
}

impl From<BackupPayloadV6> for BackupPayload {
    fn from(v6: BackupPayloadV6) -> Self {
        BackupPayload {
            created_at_micros: v6.created_at_micros,
            source_lsn: v6.source_lsn,
            current_node_count: v6.current_node_count,
            current_edge_count: v6.current_edge_count,
            node_version_count: v6.node_version_count,
            edge_version_count: v6.edge_version_count,
            interner: v6.interner,
            graph: v6.graph,
            temporal: v6.temporal,
            schema_constraints: v6.schema_constraints,
            unique_constraints: v6.unique_constraints,
            keyring_sidecar: Vec::new(),
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
    //
    // This matches exhaustively on every version this build actually knows
    // how to decode, rather than treating "not the legacy version" as
    // synonymous with "current version": a corrupted or hand-crafted header
    // claiming an unassigned value (e.g. 0, or a version skipped by a future
    // release) is rejected with a clear `IncompatibleVersion` error instead
    // of being silently routed through the current-shape decoder.
    match found_version {
        1 => {
            let legacy: BackupPayloadV1 = bitcode::decode(&decoded_bytes).map_err(|e| {
                BackupError::Serialization(format!("bitcode deserialization failed: {e}"))
            })?;
            Ok(legacy.into())
        }
        2 => {
            let legacy: BackupPayloadV2 = bitcode::decode(&decoded_bytes).map_err(|e| {
                BackupError::Serialization(format!("bitcode deserialization failed: {e}"))
            })?;
            Ok(legacy.into())
        }
        3 => {
            let legacy: BackupPayloadV3 = bitcode::decode(&decoded_bytes).map_err(|e| {
                BackupError::Serialization(format!("bitcode deserialization failed: {e}"))
            })?;
            Ok(legacy.into())
        }
        4 => {
            let legacy: BackupPayloadV4 = bitcode::decode(&decoded_bytes).map_err(|e| {
                BackupError::Serialization(format!("bitcode deserialization failed: {e}"))
            })?;
            Ok(legacy.into())
        }
        5 => {
            let legacy: BackupPayloadV5 = bitcode::decode(&decoded_bytes).map_err(|e| {
                BackupError::Serialization(format!("bitcode deserialization failed: {e}"))
            })?;
            Ok(legacy.into())
        }
        6 => {
            let legacy: BackupPayloadV6 = bitcode::decode(&decoded_bytes).map_err(|e| {
                BackupError::Serialization(format!("bitcode deserialization failed: {e}"))
            })?;
            Ok(legacy.into())
        }
        v if v == BACKUP_FORMAT_VERSION => {
            let payload: BackupPayload = bitcode::decode(&decoded_bytes).map_err(|e| {
                BackupError::Serialization(format!("bitcode deserialization failed: {e}"))
            })?;
            Ok(payload)
        }
        other => Err(BackupError::IncompatibleVersion {
            found: other,
            supported: BACKUP_FORMAT_VERSION,
        }),
    }
}

/// Test-only helper: read an `.albk` artifact, validate its magic + version,
/// strip the 6-byte header, and zstd-decompress the body to the **raw bitcode
/// payload bytes**.
///
/// The `.albk` body is zstd-compressed, so a raw byte scan of the on-disk file
/// can pass **vacuously** — a plaintext/DEK needle would not match the
/// compressed bytes even if it were logically present in the payload. This
/// helper lets a sentinel/absence test scan the *decompressed* payload so the
/// scan actually proves absence (Issue #3665 hardening, T4/T5).
///
/// Gated on `audit-export`: its only callers are the crypto-shred integration
/// tests, and `crate::db::crypto_shred` itself is `#[cfg(feature = "audit-export")]`.
#[cfg(all(test, feature = "audit-export"))]
pub(crate) fn decompress_artifact_payload(path: &Path) -> Result<Vec<u8>, BackupError> {
    let bytes = std::fs::read(path).map_err(|e| BackupError::Io(e.to_string()))?;
    if bytes.len() < 6 {
        return Err(BackupError::Corrupt(
            "Artifact too short to contain header".to_string(),
        ));
    }
    if bytes[..4] != BACKUP_MAGIC {
        return Err(BackupError::BadMagic);
    }
    let found_version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if found_version > BACKUP_FORMAT_VERSION {
        return Err(BackupError::IncompatibleVersion {
            found: found_version,
            supported: BACKUP_FORMAT_VERSION,
        });
    }
    zstd::decode_all(&bytes[6..])
        .map_err(|e| BackupError::Corrupt(format!("zstd decompression failed: {e}")))
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

    // 1. String interner: NO process-global mutation.
    //
    // We deliberately DO NOT touch the process-global `GLOBAL_INTERNER` here.
    // A previous version cleared it and re-interned the backup's strings from
    // id 0 so that this process's interner matched the backup's file-space id
    // layout. That clear was a PROCESS-GLOBAL side effect: any other
    // `AletheiaDB` live in the same process instantly held dangling label /
    // property-key ids, producing "not found in interner - data corruption
    // detected" WAL-serialize errors or silent wrong-label reads on the
    // concurrent DB (the concurrent-restore corruption regression).
    //
    // It is unnecessary: `save_graph_index` / `save_temporal_index` serialize
    // `payload.graph` / `payload.temporal` verbatim in the backup's file-space
    // ids (they never resolve against `GLOBAL_INTERNER`), and the reopen path
    // (`load_manifest_and_strings_with_remap`, Issue #3490) re-derives a
    // file-id -> live-id `InternerRemap` from the interner file written below
    // and applies it to the loaded graph/temporal data. So a restored data dir
    // reads correct labels purely through the remap-aware startup path, with no
    // global mutation from `materialize_to_dir`.

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
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_payload(
    current_snapshot: CurrentStorageSnapshot,
    historical_snapshot: HistoricalStorageSnapshot,
    cold_node_versions: Vec<crate::core::version::NodeVersion>,
    cold_edge_versions: Vec<crate::core::version::EdgeVersion>,
    source_lsn: u64,
    created_at_micros: i64,
    schema_constraints: Vec<crate::core::constraint::SchemaConstraintDescriptor>,
    unique_constraints: Vec<crate::core::constraint::UniqueConstraintDescriptor>,
    keyring_sidecar: Vec<u8>,
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
        schema_constraints,
        unique_constraints,
        keyring_sidecar,
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
            schema_constraints: vec![],
            unique_constraints: vec![],
            keyring_sidecar: vec![],
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

    /// A version-2 (Issue #3224 era, pre-#3350) artifact -- provenance
    /// present but without the `principal` field -- must restore with the
    /// caller-supplied provenance intact and `principal: None`.
    #[test]
    fn read_artifact_accepts_legacy_v2_format() {
        use crate::storage::index_persistence::formats::legacy_v2::{
            NodeVersionEntryV2, PersistedProvenanceV2, TemporalIndexDataV2,
        };
        use crate::storage::index_persistence::formats::{
            GraphIndexData, PersistedPropertyMap, PersistedVersionType, StringInternerData,
        };
        use crate::storage::index_persistence::{
            GRAPH_MAGIC, INTERNER_MAGIC, MANIFEST_VERSION, TEMPORAL_MAGIC,
        };

        let payload_v2 = BackupPayloadV2 {
            created_at_micros: 43,
            source_lsn: 9,
            current_node_count: 1,
            current_edge_count: 0,
            node_version_count: 1,
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
            temporal: TemporalIndexDataV2 {
                magic: TEMPORAL_MAGIC,
                version: 2,
                node_versions: vec![NodeVersionEntryV2 {
                    version_id: 1,
                    node_id: 1,
                    label_idx: 0,
                    valid_from: 1000,
                    valid_to: None,
                    valid_from_logical: 0,
                    valid_to_logical: None,
                    tx_time: 1000,
                    tx_time_logical: 0,
                    version_type: PersistedVersionType::Anchor,
                    properties: PersistedPropertyMap { entries: vec![] },
                    vector_snapshot_id: None,
                    provenance: Some(PersistedProvenanceV2 {
                        source: Some("hr-system".to_string()),
                        confidence: Some(0.9),
                        note: None,
                        correlation_id: None,
                    }),
                }],
                node_anchors: vec![],
                edge_versions: vec![],
                edge_anchors: vec![],
            },
        };

        // Encode byte-for-byte the way an Issue-#3224-era binary would have:
        // `[MAGIC][version=2][zstd(bitcode(BackupPayloadV2))]`.
        let encoded = bitcode::encode(&payload_v2);
        let compressed = zstd::encode_all(encoded.as_slice(), 3).unwrap();
        let mut bytes = Vec::with_capacity(6 + compressed.len());
        bytes.extend_from_slice(&BACKUP_MAGIC);
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&compressed);

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("legacy_v2.albk");
        std::fs::write(&path, &bytes).unwrap();

        let restored = read_artifact(&path).unwrap();

        assert_eq!(restored.source_lsn, 9);
        assert_eq!(restored.temporal.version, MANIFEST_VERSION);
        assert_eq!(restored.temporal.node_versions.len(), 1);
        let provenance = restored.temporal.node_versions[0]
            .provenance
            .as_ref()
            .unwrap();
        assert_eq!(provenance.source.as_deref(), Some("hr-system"));
        assert!(provenance.principal.is_none());
        // The Issue #3387 fidelity fields also default to None on the
        // upgraded v2 entries (open tx interval, no chain links).
        let entry = &restored.temporal.node_versions[0];
        assert_eq!(entry.tx_end, None);
        assert_eq!(entry.tx_end_logical, None);
        assert_eq!(entry.prev_version, None);
        assert_eq!(entry.next_version, None);
    }

    /// Regression test for a bug where restoring a legacy (`BACKUP_FORMAT_VERSION
    /// == 1`) artifact with actual version data produced a `TemporalIndexData`
    /// whose entries were upgraded to the current (provenance-carrying) shape
    /// but whose `version` field still said `1` (copied verbatim from the V1
    /// struct). Persisting that mismatched struct and reloading it would then
    /// misdetect it as legacy and misdecode every entry. See Issue #3224.
    #[test]
    fn restoring_legacy_v1_backup_with_versions_stamps_current_manifest_version() {
        use crate::storage::index_persistence::formats::legacy_v1::{
            NodeVersionEntryV1, TemporalIndexDataV1,
        };
        use crate::storage::index_persistence::formats::{
            PersistedPropertyMap, PersistedVersionType,
        };
        use crate::storage::index_persistence::temporal::{
            load_temporal_index, save_temporal_index,
        };
        use crate::storage::index_persistence::{MANIFEST_VERSION, TEMPORAL_MAGIC};

        let dir = TempDir::new().unwrap();
        let artifact_path = dir.path().join("legacy_with_data.albk");

        let mut payload = empty_payload_v1();
        payload.node_version_count = 1;
        payload.temporal = TemporalIndexDataV1 {
            magic: TEMPORAL_MAGIC,
            version: 1,
            node_versions: vec![NodeVersionEntryV1 {
                version_id: 1,
                node_id: 1,
                label_idx: 0,
                valid_from: 1000,
                valid_to: None,
                valid_from_logical: 0,
                valid_to_logical: None,
                tx_time: 1000,
                tx_time_logical: 0,
                version_type: PersistedVersionType::Anchor,
                properties: PersistedPropertyMap { entries: vec![] },
                vector_snapshot_id: None,
            }],
            node_anchors: vec![],
            edge_versions: vec![],
            edge_anchors: vec![],
        };

        std::fs::write(&artifact_path, encode_artifact_v1(&payload)).unwrap();

        let restored = read_artifact(&artifact_path).unwrap();

        // The upgraded struct must claim the *current* format version, not
        // the legacy one it was originally decoded as, since its entries are
        // already current-shape.
        assert_eq!(restored.temporal.version, MANIFEST_VERSION);
        assert_eq!(restored.temporal.node_versions.len(), 1);
        assert!(restored.temporal.node_versions[0].provenance.is_none());

        // Persist the restored (upgraded) temporal index and reload it, as
        // `restore_to_data_dir`/normal operation would. Before the fix, this
        // would silently take the legacy-decode fallback path and either
        // error or corrupt the entry.
        let temporal_path = dir.path().join("temporal.idx");
        save_temporal_index(&restored.temporal, &temporal_path).unwrap();
        let reloaded = load_temporal_index(&temporal_path).unwrap();

        assert_eq!(reloaded.version, MANIFEST_VERSION);
        assert_eq!(reloaded.node_versions.len(), 1);
        assert_eq!(reloaded.node_versions[0].node_id, 1);
        assert!(reloaded.node_versions[0].provenance.is_none());
    }

    // -----------------------------------------------------------------------
    // Version-3 (pre-fidelity, Issue #3387) backup compatibility
    // -----------------------------------------------------------------------

    /// Encode a version-3 artifact byte-for-byte the way a #3350-era
    /// (pre-#3387) AletheiaDB would have:
    /// `[MAGIC][version=3][zstd(bitcode(BackupPayloadV3))]`.
    fn encode_artifact_v3(payload: &BackupPayloadV3) -> Vec<u8> {
        let encoded = bitcode::encode(payload);
        let compressed = zstd::encode_all(encoded.as_slice(), 3).unwrap();
        let mut out = Vec::with_capacity(6 + compressed.len());
        out.extend_from_slice(&BACKUP_MAGIC);
        out.extend_from_slice(&3u16.to_le_bytes());
        out.extend_from_slice(&compressed);
        out
    }

    /// A version-3 (Issue #3350 era, pre-#3387) artifact -- provenance WITH
    /// the `principal` field, but no tx-end / chain-link fields -- must
    /// restore with the principal preserved and the Issue #3387 fidelity
    /// fields defaulting to `None`.
    #[test]
    fn read_artifact_accepts_legacy_v3_format() {
        use crate::storage::index_persistence::formats::legacy_v3::{
            NodeVersionEntryV3, TemporalIndexDataV3,
        };
        use crate::storage::index_persistence::formats::{
            PersistedPropertyMap, PersistedProvenance, PersistedVersionType,
        };
        use crate::storage::index_persistence::{MANIFEST_VERSION, TEMPORAL_MAGIC};

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("legacy_v3.albk");

        let empty = empty_payload();
        let payload = BackupPayloadV3 {
            created_at_micros: 42,
            source_lsn: 7,
            current_node_count: 0,
            current_edge_count: 0,
            node_version_count: 1,
            edge_version_count: 0,
            interner: empty.interner,
            graph: empty.graph,
            temporal: TemporalIndexDataV3 {
                magic: TEMPORAL_MAGIC,
                version: 3,
                node_versions: vec![NodeVersionEntryV3 {
                    version_id: 1,
                    node_id: 1,
                    label_idx: 0,
                    valid_from: 1000,
                    valid_to: None,
                    valid_from_logical: 0,
                    valid_to_logical: None,
                    tx_time: 1000,
                    tx_time_logical: 0,
                    version_type: PersistedVersionType::Anchor,
                    properties: PersistedPropertyMap { entries: vec![] },
                    vector_snapshot_id: None,
                    provenance: Some(PersistedProvenance {
                        source: Some("hr-system".to_string()),
                        confidence: None,
                        note: None,
                        correlation_id: None,
                        principal: Some("alice@example.com".to_string()),
                    }),
                }],
                node_anchors: vec![],
                edge_versions: vec![],
                edge_anchors: vec![],
            },
        };

        std::fs::write(&path, encode_artifact_v3(&payload)).unwrap();

        let restored = read_artifact(&path).unwrap();

        assert_eq!(restored.source_lsn, 7);
        // The upgraded struct claims the current format version (same
        // contract as the v1/v2 upgrades above).
        assert_eq!(restored.temporal.version, MANIFEST_VERSION);
        assert_eq!(restored.temporal.node_versions.len(), 1);
        let entry = &restored.temporal.node_versions[0];
        // The #3350 principal is preserved; the Issue #3387 fidelity fields
        // default to None (open tx interval, no chain links).
        let provenance = entry.provenance.as_ref().unwrap();
        assert_eq!(provenance.principal.as_deref(), Some("alice@example.com"));
        assert_eq!(entry.tx_end, None);
        assert_eq!(entry.tx_end_logical, None);
        assert_eq!(entry.prev_version, None);
        assert_eq!(entry.next_version, None);
    }

    // -----------------------------------------------------------------------
    // Version-4 (pre-schema-constraint, Issue #3378) backup compatibility
    // -----------------------------------------------------------------------

    /// Encode a version-4 artifact byte-for-byte the way a pre-#3378 AletheiaDB
    /// would have: `[MAGIC][version=4][zstd(bitcode(BackupPayloadV4))]`.
    fn encode_artifact_v4(payload: &BackupPayloadV4) -> Vec<u8> {
        let encoded = bitcode::encode(payload);
        let compressed = zstd::encode_all(encoded.as_slice(), 3).unwrap();
        let mut out = Vec::with_capacity(6 + compressed.len());
        out.extend_from_slice(&BACKUP_MAGIC);
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&compressed);
        out
    }

    /// A version-4 (Issue #3387 era, pre-#3378) artifact -- the immediately
    /// prior backup format this PR bumps -- must restore with
    /// `schema_constraints` defaulting to empty (the field the v4 shape lacks).
    #[test]
    fn read_artifact_accepts_legacy_v4_format() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("legacy_v4.albk");

        // The v4 shape uses the LIVE temporal/interner/graph structs (unchanged
        // since v4), so reuse the current empty payload's members.
        let empty = empty_payload();
        let payload = BackupPayloadV4 {
            created_at_micros: 55,
            source_lsn: 11,
            current_node_count: 0,
            current_edge_count: 0,
            node_version_count: 0,
            edge_version_count: 0,
            interner: empty.interner,
            graph: empty.graph,
            temporal: empty.temporal,
        };

        std::fs::write(&path, encode_artifact_v4(&payload)).unwrap();

        let restored = read_artifact(&path).unwrap();

        assert_eq!(restored.source_lsn, 11);
        assert_eq!(restored.created_at_micros, 55);
        // The pre-#3378 artifact carries no schema constraints; the upgrade
        // From impl defaults them to empty.
        assert!(
            restored.schema_constraints.is_empty(),
            "v4 artifact must restore with empty schema constraints"
        );
    }

    // -----------------------------------------------------------------------
    // Version-5 (pre-unique-constraint-backup, Issue #3218) compatibility
    // -----------------------------------------------------------------------

    /// Encode a version-5 artifact byte-for-byte the way a pre-#3218-backup
    /// AletheiaDB would have: `[MAGIC][version=5][zstd(bitcode(BackupPayloadV5))]`.
    fn encode_artifact_v5(payload: &BackupPayloadV5) -> Vec<u8> {
        let encoded = bitcode::encode(payload);
        let compressed = zstd::encode_all(encoded.as_slice(), 3).unwrap();
        let mut out = Vec::with_capacity(6 + compressed.len());
        out.extend_from_slice(&BACKUP_MAGIC);
        out.extend_from_slice(&5u16.to_le_bytes());
        out.extend_from_slice(&compressed);
        out
    }

    /// A version-5 (Issue #3378 era, pre-#3218-backup) artifact -- the
    /// immediately prior backup format this PR bumps -- must restore with
    /// `unique_constraints` defaulting to empty (the field the v5 shape lacks),
    /// while its `schema_constraints` are preserved verbatim.
    #[test]
    fn read_artifact_accepts_legacy_v5_format() {
        use crate::core::constraint::{PropertyConstraintDescriptor, SchemaConstraintDescriptor};

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("legacy_v5.albk");

        // The v5 shape uses the LIVE temporal/interner/graph structs (unchanged
        // since v4), so reuse the current empty payload's members.
        let empty = empty_payload();
        let payload = BackupPayloadV5 {
            created_at_micros: 66,
            source_lsn: 13,
            current_node_count: 0,
            current_edge_count: 0,
            node_version_count: 0,
            edge_version_count: 0,
            interner: empty.interner,
            graph: empty.graph,
            temporal: empty.temporal,
            schema_constraints: vec![SchemaConstraintDescriptor {
                entity_kind: "node".to_string(),
                label: "Person".to_string(),
                properties: vec![PropertyConstraintDescriptor {
                    property: "name".to_string(),
                    declared_type: None,
                    required: true,
                    nullable: true,
                }],
            }],
        };

        std::fs::write(&path, encode_artifact_v5(&payload)).unwrap();

        let restored = read_artifact(&path).unwrap();

        assert_eq!(restored.source_lsn, 13);
        assert_eq!(restored.created_at_micros, 66);
        // Schema constraints survive the v5 -> v6 upgrade verbatim.
        assert_eq!(restored.schema_constraints.len(), 1);
        assert_eq!(restored.schema_constraints[0].label, "Person");
        // The pre-#3218-backup artifact carries no uniqueness constraints; the
        // upgrade From impl defaults them to empty.
        assert!(
            restored.unique_constraints.is_empty(),
            "v5 artifact must restore with empty unique constraints"
        );
    }

    // -----------------------------------------------------------------------
    // Version-6 (pre-keyring-fold, Issue #3665) backup compatibility
    // -----------------------------------------------------------------------

    /// Encode a version-6 artifact byte-for-byte the way a pre-#3665 AletheiaDB
    /// would have: `[MAGIC][version=6][zstd(bitcode(BackupPayloadV6))]`.
    fn encode_artifact_v6(payload: &BackupPayloadV6) -> Vec<u8> {
        let encoded = bitcode::encode(payload);
        let compressed = zstd::encode_all(encoded.as_slice(), 3).unwrap();
        let mut out = Vec::with_capacity(6 + compressed.len());
        out.extend_from_slice(&BACKUP_MAGIC);
        out.extend_from_slice(&6u16.to_le_bytes());
        out.extend_from_slice(&compressed);
        out
    }

    /// T6: a version-6 (Issue #3218 era, pre-#3665) artifact -- the immediately
    /// prior backup format this PR bumps -- must restore with `keyring_sidecar`
    /// defaulting to empty (the field the v6 shape lacks), while its
    /// `schema_constraints` and `unique_constraints` are preserved verbatim.
    #[test]
    fn read_artifact_accepts_legacy_v6_format() {
        use crate::core::constraint::{
            PropertyConstraintDescriptor, SchemaConstraintDescriptor, UniqueConstraintDescriptor,
        };

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("legacy_v6.albk");

        // The v6 shape uses the LIVE temporal/interner/graph structs (unchanged
        // since v4), so reuse the current empty payload's members.
        let empty = empty_payload();
        let payload = BackupPayloadV6 {
            created_at_micros: 77,
            source_lsn: 17,
            current_node_count: 0,
            current_edge_count: 0,
            node_version_count: 0,
            edge_version_count: 0,
            interner: empty.interner,
            graph: empty.graph,
            temporal: empty.temporal,
            schema_constraints: vec![SchemaConstraintDescriptor {
                entity_kind: "node".to_string(),
                label: "Person".to_string(),
                properties: vec![PropertyConstraintDescriptor {
                    property: "name".to_string(),
                    declared_type: None,
                    required: true,
                    nullable: true,
                }],
            }],
            unique_constraints: vec![UniqueConstraintDescriptor {
                label: "Person".to_string(),
                property: "email".to_string(),
            }],
        };

        std::fs::write(&path, encode_artifact_v6(&payload)).unwrap();

        let restored = read_artifact(&path).unwrap();

        assert_eq!(restored.source_lsn, 17);
        assert_eq!(restored.created_at_micros, 77);
        // Schema + uniqueness constraints survive the v6 -> v7 upgrade verbatim.
        assert_eq!(restored.schema_constraints.len(), 1);
        assert_eq!(restored.schema_constraints[0].label, "Person");
        assert_eq!(restored.unique_constraints.len(), 1);
        assert_eq!(restored.unique_constraints[0].property, "email");
        // The pre-#3665 artifact carries no keyring; the upgrade From impl
        // defaults it to empty (designated properties, if any, restore
        // sealed-unreadable).
        assert!(
            restored.keyring_sidecar.is_empty(),
            "v6 artifact must restore with an empty keyring sidecar"
        );
    }

    /// A version-2 header whose payload does not decode as the frozen
    /// `BackupPayloadV2` shape is a decode error, not a silent fallback.
    #[test]
    fn read_artifact_rejects_corrupt_v2_payload() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("corrupt_v2.albk");

        // Valid header claiming version 2, but the zstd payload holds bytes
        // that are not a bitcode-encoded BackupPayloadV2.
        let garbage = zstd::encode_all(&b"not a backup payload"[..], 3).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&BACKUP_MAGIC);
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&garbage);
        std::fs::write(&path, &bytes).unwrap();

        let err = read_artifact(&path).unwrap_err();
        assert!(
            matches!(err, BackupError::Serialization(_)),
            "unexpected error: {err:?}"
        );
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

    /// Regression test: the version dispatch must explicitly reject any
    /// `found_version` it doesn't recognize (e.g. 0, or any value skipped by
    /// a future release) rather than silently treating "not the legacy
    /// version" as synonymous with "current version".
    #[test]
    fn read_artifact_rejects_unassigned_version_zero() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("zero.albk");
        let mut bytes = encode_artifact(&empty_payload()).unwrap();
        // Corrupt the header to claim an unassigned version (below the
        // legacy version-1 floor, not a valid value in either scheme).
        bytes[4..6].copy_from_slice(&0u16.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();

        let err = read_artifact(&path).unwrap_err();
        assert!(matches!(
            err,
            BackupError::IncompatibleVersion { found: 0, .. }
        ));
    }
}
