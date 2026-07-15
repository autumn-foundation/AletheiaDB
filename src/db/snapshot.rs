//! Named snapshots for reproducible reads (Issue #3370).
//!
//! A *named snapshot* pins a human-readable name to a bi-temporal coordinate
//! `(valid_time, transaction_time)`. Reads issued through the resulting
//! [`Snapshot`] handle are evaluated at that coordinate via the deterministic
//! historical (`*_at_time`) read path, so the same handle returns
//! byte-for-byte identical results no matter how the database mutates
//! afterward.
//!
//! # A snapshot is a coordinate, not a held resource
//!
//! Creating a snapshot records two timestamps in a small sidecar registry. It
//! **pins no storage**, blocks no writers, and imposes **zero overhead on the
//! write path** (`create_node`/`create_edge` never touch the registry). This
//! mirrors the #3360 cursor, which likewise captures a `(vt, tt)` pair once
//! and re-reads deterministically. The tradeoff is that a snapshot enjoys no
//! retention guarantee: if the versions it observes are later evicted (cold
//! tier not configured, or history truncated) a read through the handle can
//! return "not found" for a fact that was visible at creation. This is the
//! same visibility caveat that governs `temporal_extent` (#3238) and every
//! other point-in-time read.
//!
//! # Defaulting the coordinate ("now")
//!
//! [`AletheiaDB::create_snapshot`] captures the database's authoritative
//! commit frontier — the value under `current_timestamp` — for **both**
//! dimensions. Because the commit path advances that frontier strictly
//! monotonically under the same lock, every already-committed transaction has
//! a transaction-time start `<=` the frontier (so it is visible) and every
//! future commit has a start strictly `>` the frontier (so it is invisible).
//! The result is a deterministic "as of the moment of creation" pin. Note the
//! same future-valid caveat point-in-time reads carry: a fact whose
//! `valid_from` lies in the future of the pin is not visible through it.
//!
//! # Scope (this wave is Rust-API-only)
//!
//! Surfacing named snapshots through MCP and an `AS OF SNAPSHOT <name>` query
//! DDL clause is a deliberately coordinated follow-up; this module adds only
//! the Rust API, registry, and durable persistence.

use crate::core::error::{Error, Result};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::property::PropertyValue;
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;
use crate::db::ops::NodesAtTime;
use crate::query::QueryBuilder;
use crate::query::builder::state::Initial;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Persisted-format version for the snapshot registry sidecar file. Bumped
/// only on an incompatible on-disk change (mirrors the auth key store).
const PERSIST_FORMAT_VERSION: u32 = 1;

/// serde adapter: (de)serialize a [`Timestamp`] as `i64` microseconds since
/// the Unix epoch, exactly as the #3360 cursor persists its coordinates. The
/// logical HLC counter is intentionally dropped — snapshot coordinates are
/// microsecond-granular on disk, which is sufficient for point-in-time reads.
mod ts_micros {
    use super::Timestamp;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(ts: &Timestamp, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(ts.wallclock())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Timestamp, D::Error> {
        Ok(Timestamp::from(i64::deserialize(d)?))
    }
}

/// A named, reproducible bi-temporal snapshot coordinate (Issue #3370).
///
/// This value is immutable and cheaply cloneable. It records the pin's name,
/// the `(valid_time, transaction_time)` coordinate reads resolve at, when the
/// snapshot was created, and an optional human description.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedSnapshot {
    name: String,
    #[serde(with = "ts_micros")]
    valid_time: Timestamp,
    #[serde(with = "ts_micros")]
    transaction_time: Timestamp,
    #[serde(with = "ts_micros")]
    created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

impl NamedSnapshot {
    /// The snapshot's unique name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The valid-time coordinate reads resolve at.
    #[must_use]
    pub fn valid_time(&self) -> Timestamp {
        self.valid_time
    }

    /// The transaction-time coordinate reads resolve at.
    #[must_use]
    pub fn transaction_time(&self) -> Timestamp {
        self.transaction_time
    }

    /// The `(valid_time, transaction_time)` coordinate pair.
    #[must_use]
    pub fn coordinate(&self) -> (Timestamp, Timestamp) {
        (self.valid_time, self.transaction_time)
    }

    /// When the snapshot was created (transaction-time of the create call).
    #[must_use]
    pub fn created_at(&self) -> Timestamp {
        self.created_at
    }

    /// The optional human-readable description.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
}

/// The on-disk registry envelope (versioned, mirrors the auth key store).
#[derive(Serialize, Deserialize)]
struct PersistedRegistry {
    version: u32,
    snapshots: Vec<NamedSnapshot>,
}

/// In-process registry of named snapshots, optionally persisted to a sidecar
/// JSON file.
///
/// The registry is entirely off the data write path: mutating it never touches
/// current/historical storage, the WAL, or `current_timestamp`, so creating a
/// snapshot adds zero overhead to `create_node`/`create_edge`.
pub(crate) struct SnapshotRegistry {
    entries: RwLock<HashMap<String, NamedSnapshot>>,
    /// Sidecar file path when persistence is enabled; `None` for ephemeral,
    /// in-memory-only registries (`AletheiaDB::new()`).
    persist_path: Option<PathBuf>,
    /// Serializes concurrent disk saves (the in-memory map has its own
    /// `RwLock`; this guards the temp-file+rename dance).
    save_lock: parking_lot::Mutex<()>,
}

impl SnapshotRegistry {
    /// Create an empty, memory-only registry (no file is ever written).
    pub(crate) fn in_memory() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            persist_path: None,
            save_lock: parking_lot::Mutex::new(()),
        }
    }

    /// Open a registry, loading any existing sidecar at `path`.
    ///
    /// `path` is `None` for an in-memory-only registry. A missing file yields
    /// an empty registry (first run). A present-but-unreadable or corrupt file
    /// is surfaced as an error, matching how the auth key store treats its
    /// persisted file — a durable store that cannot be parsed is a
    /// configuration problem the operator should see, not silently discarded.
    pub(crate) fn open(path: Option<PathBuf>) -> Result<Self> {
        let registry = Self {
            entries: RwLock::new(HashMap::new()),
            persist_path: path.clone(),
            save_lock: parking_lot::Mutex::new(()),
        };
        if let Some(path) = path
            && path.exists()
        {
            let contents = std::fs::read_to_string(&path)?;
            let parsed: PersistedRegistry = serde_json::from_str(&contents).map_err(|e| {
                Error::Other(format!(
                    "failed to parse snapshot registry at {}: {e}",
                    path.display()
                ))
            })?;
            if parsed.version != PERSIST_FORMAT_VERSION {
                return Err(Error::Other(format!(
                    "unsupported snapshot registry version {} (expected {})",
                    parsed.version, PERSIST_FORMAT_VERSION
                )));
            }
            let mut entries = registry.entries.write();
            for snapshot in parsed.snapshots {
                entries.insert(snapshot.name.clone(), snapshot);
            }
        }
        Ok(registry)
    }

    /// Insert a snapshot if its name is free.
    ///
    /// Returns a `CONFLICT`-classed error (reusing
    /// [`StorageError::DuplicateId`](crate::core::error::StorageError::DuplicateId),
    /// which maps to the #3234 `CONFLICT` code) if the name is already taken.
    pub(crate) fn insert(&self, snapshot: NamedSnapshot) -> Result<()> {
        {
            let mut entries = self.entries.write();
            if entries.contains_key(&snapshot.name) {
                return Err(conflict(&snapshot.name));
            }
            entries.insert(snapshot.name.clone(), snapshot);
        }
        self.save()
    }

    /// Fetch a snapshot by name.
    pub(crate) fn get(&self, name: &str) -> Option<NamedSnapshot> {
        self.entries.read().get(name).cloned()
    }

    /// List all snapshots in a stable order (created_at, then name).
    pub(crate) fn list(&self) -> Vec<NamedSnapshot> {
        let mut all: Vec<NamedSnapshot> = self.entries.read().values().cloned().collect();
        all.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.name.cmp(&b.name))
        });
        all
    }

    /// Remove a snapshot by name.
    ///
    /// Returns a `NOT_FOUND`-classed error if the name is absent.
    pub(crate) fn remove(&self, name: &str) -> Result<()> {
        {
            let mut entries = self.entries.write();
            if entries.remove(name).is_none() {
                return Err(not_found(name));
            }
        }
        self.save()
    }

    /// Atomically persist the registry to its sidecar file (temp file +
    /// rename + parent fsync), mirroring the auth key store. A no-op for
    /// in-memory-only registries.
    fn save(&self) -> Result<()> {
        let Some(path) = &self.persist_path else {
            return Ok(());
        };
        let _guard = self.save_lock.lock();

        let mut snapshots: Vec<NamedSnapshot> = self.entries.read().values().cloned().collect();
        snapshots.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.name.cmp(&b.name))
        });

        let serialized = serde_json::to_vec_pretty(&PersistedRegistry {
            version: PERSIST_FORMAT_VERSION,
            snapshots,
        })
        .map_err(|e| Error::Other(format!("failed to serialize snapshot registry: {e}")))?;

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let tmp_path = path.with_extension("tmp");
        let _ = std::fs::remove_file(&tmp_path);
        {
            use std::io::Write as _;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            let mut file = options.open(&tmp_path)?;
            file.write_all(&serialized)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp_path, path)?;
        // Make the rename durable: fsync the parent directory (unix only).
        #[cfg(unix)]
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}

/// Build the sidecar path for a database's snapshot registry, or `None` when
/// the database is ephemeral (persistence disabled).
///
/// The file lives at `{data_dir}/snapshots.json`, alongside the auth store's
/// `{data_dir}/auth/`. The canonical durable config
/// ([`crate::config::durable_config_for_data_dir`]) sets
/// `persistence.data_dir = {data_dir}/indexes`, so the top-level data dir is
/// that path's parent.
pub(crate) fn registry_path_for(
    persistence: &crate::storage::index_persistence::PersistenceConfig,
) -> Option<PathBuf> {
    if !persistence.enabled {
        return None;
    }
    let indexes_dir = &persistence.data_dir;
    let root = indexes_dir
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| indexes_dir.clone());
    Some(root.join("snapshots.json"))
}

/// A read-only, borrowed handle that pins every read to a
/// [`NamedSnapshot`]'s bi-temporal coordinate (Issue #3370).
///
/// All reads delegate to the historical (`*_at_time`) path — never the
/// current-state hot path — so results are deterministic and reproducible
/// regardless of concurrent or subsequent writes.
///
/// # Adjacency and traversal
///
/// [`Snapshot::get_outgoing_edges`] / [`Snapshot::get_incoming_edges`] resolve
/// adjacency at the pin via the temporal adjacency index. Multi-hop traversal
/// is available through the pre-pinned [`Snapshot::query`] builder (its
/// `as_of` context is already set); there is no separate storage-layer
/// traversal for snapshots.
pub struct Snapshot<'a> {
    db: &'a AletheiaDB,
    snapshot: NamedSnapshot,
}

impl std::fmt::Debug for Snapshot<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("snapshot", &self.snapshot)
            .finish_non_exhaustive()
    }
}

impl<'a> Snapshot<'a> {
    fn new(db: &'a AletheiaDB, snapshot: NamedSnapshot) -> Self {
        Self { db, snapshot }
    }

    /// The pinned snapshot's name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.snapshot.name()
    }

    /// The pinned valid-time coordinate.
    #[must_use]
    pub fn valid_time(&self) -> Timestamp {
        self.snapshot.valid_time()
    }

    /// The pinned transaction-time coordinate.
    #[must_use]
    pub fn transaction_time(&self) -> Timestamp {
        self.snapshot.transaction_time()
    }

    /// The pinned `(valid_time, transaction_time)` coordinate pair.
    #[must_use]
    pub fn coordinate(&self) -> (Timestamp, Timestamp) {
        self.snapshot.coordinate()
    }

    /// The pinned snapshot's optional description.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.snapshot.description()
    }

    /// The underlying [`NamedSnapshot`] value.
    #[must_use]
    pub fn named(&self) -> &NamedSnapshot {
        &self.snapshot
    }

    /// Read a node as it existed at the pinned coordinate.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_node(&self, node_id: NodeId) -> Result<Node> {
        let (vt, tt) = self.snapshot.coordinate();
        self.db.get_node_at_time(node_id, vt, tt)
    }

    /// Read an edge as it existed at the pinned coordinate.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edge(&self, edge_id: EdgeId) -> Result<Edge> {
        let (vt, tt) = self.snapshot.coordinate();
        self.db.get_edge_at_time(edge_id, vt, tt)
    }

    /// Find nodes by label as they existed at the pinned coordinate.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn find_nodes(&self, label: &str) -> Result<NodesAtTime> {
        let (vt, tt) = self.snapshot.coordinate();
        self.db.find_nodes_at_time(label, vt, tt)
    }

    /// Find nodes by label and exact property value at the pinned coordinate.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn find_nodes_by_property(
        &self,
        label: &str,
        property_key: &str,
        property_value: &PropertyValue,
    ) -> Result<NodesAtTime> {
        let (vt, tt) = self.snapshot.coordinate();
        self.db
            .find_nodes_by_property_at(label, property_key, property_value, vt, tt)
    }

    /// Outgoing edges from `source` valid at the pinned coordinate.
    #[must_use]
    pub fn get_outgoing_edges(&self, source: NodeId) -> Vec<EdgeId> {
        let (vt, tt) = self.snapshot.coordinate();
        self.db.get_outgoing_edges_at_time(source, vt, tt)
    }

    /// Incoming edges to `target` valid at the pinned coordinate.
    #[must_use]
    pub fn get_incoming_edges(&self, target: NodeId) -> Vec<EdgeId> {
        let (vt, tt) = self.snapshot.coordinate();
        self.db.get_incoming_edges_at_time(target, vt, tt)
    }

    /// A query builder pre-pinned to the snapshot's coordinate.
    ///
    /// The returned builder already has its `as_of(valid_time,
    /// transaction_time)` context set, so multi-hop traversal, filtering, and
    /// vector ranking all execute at the pin. Executing it reproduces the same
    /// world every time.
    #[must_use]
    pub fn query(&self) -> QueryBuilder<Initial> {
        let (vt, tt) = self.snapshot.coordinate();
        self.db.query().as_of(vt, tt)
    }
}

/// Reuse [`StorageError::DuplicateId`] for a duplicate snapshot name so the
/// error maps to the #3234 `CONFLICT` code (and is non-retriable) when the
/// MCP surface is added later. The name is carried in the `id` field.
fn conflict(name: &str) -> Error {
    Error::Storage(crate::core::error::StorageError::DuplicateId {
        id: name.to_string(),
        kind: "snapshot".to_string(),
    })
}

/// Reuse [`StorageError::PropertyNotFound`] (a string-carrying storage
/// not-found) for a missing snapshot name so the error maps to the #3234
/// `NOT_FOUND` code. The name is embedded in the message ("the details").
///
/// There is no dedicated string-based not-found variant on the top-level
/// `Error` enum that maps to `NOT_FOUND`, and adding one would require editing
/// the (exhaustive) MCP error classifier — out of scope for this Rust-only
/// wave — so we reuse the closest existing variant.
fn not_found(name: &str) -> Error {
    Error::Storage(crate::core::error::StorageError::PropertyNotFound(format!(
        "snapshot '{name}'"
    )))
}

impl AletheiaDB {
    /// Create a named snapshot pinned to the database's current commit frontier
    /// (Issue #3370).
    ///
    /// Both temporal dimensions default to the value under `current_timestamp`
    /// (the authoritative, monotonically advancing commit clock), so the
    /// snapshot sees exactly the state committed at creation and nothing
    /// after: every already-committed transaction is visible and every future
    /// commit is not. See the [module docs](crate::db::snapshot) for the
    /// determinism argument and the future-valid caveat.
    ///
    /// # Errors
    ///
    /// Returns a `CONFLICT`-classed error if `name` is already registered.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_snapshot(
        &self,
        name: impl Into<String>,
        description: Option<String>,
    ) -> Result<NamedSnapshot> {
        // Capture the commit frontier under its own lock (lock class 1) and
        // release immediately — we hold no later-class lock, so the ordering
        // contract is preserved. The commit path advances this value strictly
        // monotonically under the same lock, which is what makes the pin
        // deterministic (future commits are strictly greater, hence invisible).
        let frontier = {
            let ts = self.current_timestamp.lock().map_err(|_| {
                Error::from(crate::core::error::TransactionError::LockPoisoned {
                    resource: "current_timestamp".to_string(),
                })
            })?;
            *ts
        };
        self.create_snapshot_at(name, frontier, frontier, description)
    }

    /// Create a named snapshot pinned to an explicit bi-temporal coordinate
    /// (Issue #3370).
    ///
    /// Unlike [`create_snapshot`](Self::create_snapshot), the caller supplies
    /// the `(valid_time, transaction_time)` coordinate directly. Backdated
    /// coordinates are permitted and are **not** rejected for falling outside
    /// the current temporal extent — the coordinate is stored as given, and a
    /// coordinate before any recorded history simply resolves to an empty
    /// world.
    ///
    /// # Errors
    ///
    /// Returns a `CONFLICT`-classed error if `name` is already registered.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_snapshot_at(
        &self,
        name: impl Into<String>,
        valid_time: Timestamp,
        transaction_time: Timestamp,
        description: Option<String>,
    ) -> Result<NamedSnapshot> {
        let snapshot = NamedSnapshot {
            name: name.into(),
            valid_time,
            transaction_time,
            created_at: crate::core::temporal::time::now(),
            description,
        };
        self.snapshots.insert(snapshot.clone())?;
        Ok(snapshot)
    }

    /// Resolve a named snapshot to a borrowed, read-only [`Snapshot`] handle.
    ///
    /// # Errors
    ///
    /// Returns a `NOT_FOUND`-classed error (with `name` in the message) if no
    /// snapshot with that name is registered.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn snapshot(&self, name: &str) -> Result<Snapshot<'_>> {
        let snapshot = self.snapshots.get(name).ok_or_else(|| not_found(name))?;
        Ok(Snapshot::new(self, snapshot))
    }

    /// Fetch a named snapshot's coordinate/metadata without a borrowed handle.
    ///
    /// # Errors
    ///
    /// Returns a `NOT_FOUND`-classed error (with `name` in the message) if
    /// absent.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_snapshot(&self, name: &str) -> Result<NamedSnapshot> {
        self.snapshots.get(name).ok_or_else(|| not_found(name))
    }

    /// List all registered snapshots in a stable order (creation time, then
    /// name).
    #[must_use]
    pub fn list_snapshots(&self) -> Vec<NamedSnapshot> {
        self.snapshots.list()
    }

    /// Delete a named snapshot.
    ///
    /// # Errors
    ///
    /// Returns a `NOT_FOUND`-classed error (with `name` in the message) if no
    /// snapshot with that name is registered.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn delete_snapshot(&self, name: &str) -> Result<()> {
        self.snapshots.remove(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_path_is_none_when_persistence_disabled() {
        let cfg = crate::storage::index_persistence::PersistenceConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(registry_path_for(&cfg), None);
    }

    #[test]
    fn registry_path_is_data_dir_sibling_of_indexes() {
        let cfg = crate::storage::index_persistence::PersistenceConfig {
            enabled: true,
            data_dir: PathBuf::from("/var/lib/aletheia/indexes"),
            ..Default::default()
        };
        assert_eq!(
            registry_path_for(&cfg),
            Some(PathBuf::from("/var/lib/aletheia/snapshots.json"))
        );
    }

    #[test]
    fn in_memory_registry_insert_get_conflict_remove() {
        let reg = SnapshotRegistry::in_memory();
        let snap = NamedSnapshot {
            name: "run1".to_string(),
            valid_time: Timestamp::from(1000),
            transaction_time: Timestamp::from(1000),
            created_at: Timestamp::from(1000),
            description: Some("first".to_string()),
        };
        reg.insert(snap.clone()).unwrap();
        assert_eq!(reg.get("run1"), Some(snap.clone()));

        // Duplicate -> CONFLICT (DuplicateId).
        let err = reg.insert(snap).unwrap_err();
        assert!(matches!(
            err,
            Error::Storage(crate::core::error::StorageError::DuplicateId { .. })
        ));

        // Remove -> gone; second remove -> NOT_FOUND.
        reg.remove("run1").unwrap();
        assert_eq!(reg.get("run1"), None);
        let err = reg.remove("run1").unwrap_err();
        assert!(matches!(
            err,
            Error::Storage(crate::core::error::StorageError::PropertyNotFound(_))
        ));
    }

    #[test]
    fn named_snapshot_serde_round_trips_as_micros() {
        let snap = NamedSnapshot {
            name: "s".to_string(),
            valid_time: Timestamp::from(111),
            transaction_time: Timestamp::from(222),
            created_at: Timestamp::from(333),
            description: None,
        };
        let json = serde_json::to_string(&snap).unwrap();
        // Timestamps serialize as bare integer micros, not nested objects.
        assert!(json.contains("\"valid_time\":111"), "json was {json}");
        assert!(json.contains("\"transaction_time\":222"), "json was {json}");
        let back: NamedSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snap);
    }
}
