//! Named snapshots for reproducible reads (Issue #3370).
//!
//! A *named snapshot* pins a human-readable name to a bi-temporal coordinate
//! `(valid_time, transaction_time)`. Reads issued through the resulting
//! [`Snapshot`] handle are evaluated at that coordinate via the deterministic
//! historical (`*_at_time`) read path, so the same handle returns identical
//! results regardless of writes that land afterward.
//!
//! # A snapshot is a coordinate, not a held resource
//!
//! Creating a snapshot records two timestamps in a small sidecar registry. It
//! **pins no storage** and adds no lasting overhead to the write path
//! (`create_node`/`create_edge` never touch the registry). Creation itself
//! takes the commit-clock lock (`current_timestamp`) just long enough to copy a
//! single `Timestamp` — nanosecond-scale contention on the hot commit path, not
//! literally zero — so it is effectively free but not a true no-op against
//! concurrent committers. This mirrors the #3360 cursor, which likewise
//! captures a `(vt, tt)` pair once and re-reads deterministically. The tradeoff
//! is that a snapshot enjoys no retention guarantee: if the versions it observes
//! are later evicted (cold tier not configured, or history truncated) a read
//! through the handle can return "not found" for a fact that was visible at
//! creation. This is the same visibility caveat that governs `temporal_extent`
//! (#3238) and every other point-in-time read.
//!
//! A snapshot **created at an instant that races an in-flight commit** inherits
//! the engine's standard committed-but-not-yet-applied visibility window: the
//! same caveat that #3225 / #3236 point-in-time reads carry. Once created, the
//! coordinate is fixed and every read through it is deterministic.
//!
//! # Defaulting the coordinate ("now")
//!
//! [`AletheiaDB::create_snapshot`] defaults the two dimensions **differently**,
//! each to the correct notion of "now":
//!
//! - **transaction time** = the database's authoritative commit frontier (the
//!   value under `current_timestamp`). Because the commit path advances that
//!   frontier strictly monotonically under the same lock, every already-committed
//!   transaction has a transaction-time start `<=` the frontier (visible) and
//!   every future commit a start strictly `>` it (invisible) — a race-free
//!   monotonic pin. The frontier equals "now" for all committed data but is
//!   chosen over wallclock precisely for that race-safety.
//! - **valid time** = wallclock `time::now()`, matching the engine's "now"
//!   convention (`get_node_at_valid_time`, `find_nodes_at_time`). This ensures a
//!   default "now" snapshot sees every fact actually valid at creation, rather
//!   than silently excluding facts the (idle-lagging) frontier has not yet
//!   reached.
//!
//! So "as of the moment of creation" is precise: valid-time-now =
//! wallclock-now, transaction-time-now = the commit frontier. Note the same
//! future-valid caveat point-in-time reads carry: a fact whose `valid_from`
//! lies in the future of the pinned valid time is not visible through it.
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
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Persisted-format version for the snapshot registry sidecar file. Bumped
/// only on an incompatible on-disk change (mirrors the auth key store).
///
/// v1 stored each timestamp as a bare `i64` (wallclock micros only, HLC logical
/// counter dropped). v2 stores the **full** [`HybridTimestamp`] as
/// `{wallclock, logical}` so a pin round-trips losslessly across restart — a
/// coordinate with a nonzero logical counter (two commits in the same
/// microsecond) must not collapse onto its same-microsecond neighbor on reload.
/// The [`ts_hlc`] deserializer still accepts the legacy bare-integer form
/// (reconstructing it as logical 0), so v1 files load without a hard failure.
///
/// Only referenced by the serde-gated persistence path; the in-memory registry
/// needs no on-disk format version.
#[cfg(feature = "serde")]
const PERSIST_FORMAT_VERSION: u32 = 2;

/// serde adapter: (de)serialize a [`Timestamp`] (an HLC `HybridTimestamp`)
/// **losslessly** as a `{wallclock, logical}` object.
///
/// Dropping the logical counter (as the pre-#3370-review format did) is not
/// safe for snapshot coordinates: two commits within one wallclock microsecond
/// receive the same wallclock but distinct logical counters (`0`, `1`, …), so a
/// pin at `(W, 1)` that collapses to `(W, 0)` on reload lands on the *wrong*
/// side of the supersession and returns the superseded version. Persisting both
/// components mirrors the version store's on-disk 12-byte
/// `[wallclock:8][logical:4]` timestamp (`src/core/temporal.rs`).
///
/// The deserializer is tolerant of the **legacy** v1 shape — a bare integer is
/// accepted and reconstructed with logical `0` — so an old sidecar still loads.
///
/// Only compiled when the `serde` feature is enabled (persistence is a no-op
/// otherwise); the snapshot registry itself works in-memory regardless.
#[cfg(feature = "serde")]
mod ts_hlc {
    use super::Timestamp;
    use crate::core::hlc::HybridTimestamp;
    use serde::de::{self, MapAccess, Visitor};
    use serde::ser::SerializeStruct;
    use serde::{Deserializer, Serializer};
    use std::fmt;

    pub fn serialize<S: Serializer>(ts: &Timestamp, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("HybridTimestamp", 2)?;
        st.serialize_field("wallclock", &ts.wallclock())?;
        st.serialize_field("logical", &ts.logical())?;
        st.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Timestamp, D::Error> {
        struct TsVisitor;

        impl<'de> Visitor<'de> for TsVisitor {
            type Value = Timestamp;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a {wallclock, logical} object or a legacy integer (micros)")
            }

            // Legacy v1 form: bare integer micros -> logical 0.
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Timestamp, E> {
                Ok(HybridTimestamp::new_unchecked(v, 0))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Timestamp, E> {
                Ok(HybridTimestamp::new_unchecked(v as i64, 0))
            }

            // Current v2 form: {wallclock, logical}.
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Timestamp, A::Error> {
                let mut wallclock: Option<i64> = None;
                let mut logical: Option<u32> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "wallclock" => wallclock = Some(map.next_value()?),
                        "logical" => logical = Some(map.next_value()?),
                        _ => {
                            let _: de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                let wallclock = wallclock.ok_or_else(|| de::Error::missing_field("wallclock"))?;
                Ok(HybridTimestamp::new_unchecked(
                    wallclock,
                    logical.unwrap_or(0),
                ))
            }
        }

        d.deserialize_any(TsVisitor)
    }
}

/// A named, reproducible bi-temporal snapshot coordinate (Issue #3370).
///
/// This value is immutable and cheaply cloneable. It records the pin's name,
/// the `(valid_time, transaction_time)` coordinate reads resolve at, when the
/// snapshot was created, and an optional human description.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct NamedSnapshot {
    name: String,
    #[cfg_attr(feature = "serde", serde(with = "ts_hlc"))]
    valid_time: Timestamp,
    #[cfg_attr(feature = "serde", serde(with = "ts_hlc"))]
    transaction_time: Timestamp,
    #[cfg_attr(feature = "serde", serde(with = "ts_hlc"))]
    created_at: Timestamp,
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
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
///
/// Only compiled with the `serde` feature; without it the registry is
/// in-memory-only and never (de)serialized.
#[cfg(feature = "serde")]
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
    /// an empty registry (first run).
    ///
    /// # Tolerant load (deliberate divergence from the auth key store)
    ///
    /// Snapshots are non-critical *bookmarks*: they pin no data and losing them
    /// costs at most a re-created pin. So — unlike the **security-critical** auth
    /// key store (`src/auth/store.rs`), which correctly hard-fails on a corrupt
    /// file rather than silently start with no credentials — a corrupt,
    /// unparseable, or unknown-future-version `snapshots.json` must **not brick
    /// database startup**. On such a file we log a warning, quarantine it aside
    /// (rename to `*.corrupt`, preserving the bytes for inspection), and start
    /// with an empty registry. Legacy v1 files (bare-integer timestamps) still
    /// load normally via the [`ts_hlc`] tolerant deserializer.
    ///
    /// The load is driven off a single `read_to_string` (no `exists()`
    /// pre-check, so no TOCTOU gap): a `NotFound` error is the normal first-run
    /// case and silently yields an empty registry, while **any other** read I/O
    /// error (permissions, a sharing violation, on-disk corruption surfacing as
    /// an I/O error) is treated exactly like a parse failure — warn, quarantine,
    /// start empty — so an operator problem still cannot brick startup for a
    /// non-critical bookmark file.
    pub(crate) fn open(path: Option<PathBuf>) -> Result<Self> {
        let registry = Self {
            entries: RwLock::new(HashMap::new()),
            persist_path: path.clone(),
            save_lock: parking_lot::Mutex::new(()),
        };
        // Loading the sidecar requires serde; without it the registry is
        // in-memory-only (persistence is compiled out) and `path` is retained
        // solely so a later save is a clean no-op.
        #[cfg(feature = "serde")]
        if let Some(path) = path {
            match std::fs::read_to_string(&path) {
                Ok(contents) => match serde_json::from_str::<PersistedRegistry>(&contents) {
                    Ok(parsed) if parsed.version <= PERSIST_FORMAT_VERSION => {
                        let mut entries = registry.entries.write();
                        for snapshot in parsed.snapshots {
                            entries.insert(snapshot.name.clone(), snapshot);
                        }
                    }
                    Ok(parsed) => {
                        log_registry_warning(&format!(
                            "snapshot registry at {} has unsupported future version {} (this \
                             build understands up to {}); quarantining it and starting with an \
                             empty registry",
                            path.display(),
                            parsed.version,
                            PERSIST_FORMAT_VERSION
                        ));
                        quarantine_corrupt_registry(&path);
                    }
                    Err(e) => {
                        log_registry_warning(&format!(
                            "failed to parse snapshot registry at {} ({e}); quarantining it and \
                             starting with an empty registry",
                            path.display()
                        ));
                        quarantine_corrupt_registry(&path);
                    }
                },
                // Missing file is the normal first-run case: start empty, no warning.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                // Any other read I/O error (permissions, sharing violation,
                // corruption) must not brick startup: warn, quarantine, empty.
                Err(e) => {
                    log_registry_warning(&format!(
                        "failed to read snapshot registry at {} ({e}); quarantining it and \
                         starting with an empty registry",
                        path.display()
                    ));
                    quarantine_corrupt_registry(&path);
                }
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
        // Serialize the whole mutate+save under `save_lock` (outermost). Two
        // concurrent inserts must not interleave such that the later save wins
        // on disk while the earlier one rolls back its in-memory entry, leaving
        // an on-disk entry absent from RAM. Holding `save_lock` across the
        // conflict-check, insert, and save closes that observable divergence
        // window. No deadlock: the `entries` write guard is dropped before
        // `save_locked()` (which only takes `entries.read()`), and the rollback
        // re-takes `entries.write()` only after `save_locked()` returns.
        let _guard = self.save_lock.lock();
        let name = {
            let mut entries = self.entries.write();
            if entries.contains_key(&snapshot.name) {
                return Err(conflict(&snapshot.name));
            }
            let name = snapshot.name.clone();
            entries.insert(name.clone(), snapshot);
            name
        };
        // Roll back the in-memory insert if the disk save fails, so RAM and disk
        // never diverge (the caller sees Err *and* the entry is not registered).
        if let Err(e) = self.save_locked() {
            self.entries.write().remove(&name);
            return Err(e);
        }
        Ok(())
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
        // Serialize the whole mutate+save under `save_lock` (outermost),
        // symmetric to `insert`, so a concurrent remove/insert cannot leave RAM
        // and disk diverged. Same no-deadlock argument: the `entries` write
        // guard is dropped before `save_locked()`, and the re-insert on failure
        // re-takes `entries.write()` only after it returns.
        let _guard = self.save_lock.lock();
        let removed = {
            let mut entries = self.entries.write();
            match entries.remove(name) {
                Some(removed) => removed,
                None => return Err(not_found(name)),
            }
        };
        // Re-insert the removed entry if the disk save fails, so RAM and disk
        // never diverge (the caller sees Err *and* the entry is still present).
        if let Err(e) = self.save_locked() {
            self.entries.write().insert(removed.name.clone(), removed);
            return Err(e);
        }
        Ok(())
    }

    /// Atomically persist the registry to its sidecar file (temp file +
    /// rename + parent fsync), mirroring the auth key store. A no-op for
    /// in-memory-only registries.
    ///
    /// Acquires `save_lock` and delegates to [`save_locked`](Self::save_locked).
    /// Callers already holding `save_lock` (the `insert`/`remove` mutate+save
    /// critical sections) must call `save_locked` directly instead — this
    /// `Mutex` is not reentrant. Retained as the standalone lock-acquiring save
    /// entry point (both current callers hold `save_lock` and use `save_locked`,
    /// so this is currently unused).
    #[allow(dead_code)]
    fn save(&self) -> Result<()> {
        let _guard = self.save_lock.lock();
        self.save_locked()
    }

    /// The durable-write body of [`save`](Self::save), **assuming `save_lock`
    /// is already held** by the caller. A no-op for in-memory-only registries.
    ///
    /// Only ever takes `entries.read()` (to snapshot the map for serialization),
    /// never `save_lock`, so it composes safely inside the `insert`/`remove`
    /// critical sections that hold `save_lock` across the whole mutate+save.
    fn save_locked(&self) -> Result<()> {
        let Some(path) = &self.persist_path else {
            return Ok(());
        };

        // Durable persistence requires serde. Without it the registry is
        // in-memory-only: nothing is written (the sidecar path is still tracked
        // so behavior is otherwise identical).
        #[cfg(not(feature = "serde"))]
        {
            let _ = path;
            Ok(())
        }

        #[cfg(feature = "serde")]
        {
            self.save_serialized(path)
        }
    }

    /// Serialize the registry to `path` (temp file + rename + parent fsync).
    /// Split out so the serde-gated body lives in one place.
    #[cfg(feature = "serde")]
    fn save_serialized(&self, path: &std::path::Path) -> Result<()> {
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

/// Emit a warning about the snapshot registry under either logging
/// configuration, matching the crate's `observability`-gated logging style
/// (mirrors `log_scan_warning` in `src/storage/wal/segment_reader.rs`).
///
/// Only used by the serde-gated sidecar load path.
#[cfg(feature = "serde")]
fn log_registry_warning(message: &str) {
    #[cfg(feature = "observability")]
    tracing::warn!("{}", message);
    #[cfg(not(feature = "observability"))]
    eprintln!("WARNING: {}", message);
}

/// Move a corrupt/unreadable sidecar aside so startup can proceed with an empty
/// registry while preserving the bad bytes for inspection. Renames `path` to
/// `path.corrupt` (overwriting a prior quarantine, which is fine — the current
/// bad file is the interesting one). Failure to quarantine is itself logged but
/// never fatal: the in-memory registry is already empty, and a later `save()`
/// overwrites the file atomically.
///
/// Only used by the serde-gated sidecar load path.
#[cfg(feature = "serde")]
fn quarantine_corrupt_registry(path: &std::path::Path) {
    let mut corrupt = path.as_os_str().to_owned();
    corrupt.push(".corrupt");
    let corrupt = PathBuf::from(corrupt);
    if let Err(e) = std::fs::rename(path, &corrupt) {
        log_registry_warning(&format!(
            "could not quarantine corrupt snapshot registry {} -> {}: {e}",
            path.display(),
            corrupt.display()
        ));
    }
}

/// Build the sidecar path for a database's snapshot registry, or `None` when
/// the database is ephemeral (persistence disabled).
///
/// The file lives **inside** the configured persistence directory, at
/// `{persistence.data_dir}/snapshots.json`. Under the canonical durable config
/// ([`crate::config::durable_config_for_data_dir`], which sets
/// `persistence.data_dir = {data_dir}/indexes`) that is
/// `{data_dir}/indexes/snapshots.json`. Keeping it inside `data_dir` — rather
/// than stripping a parent path component — means a power user who points
/// `data_dir` straight at their data root gets the sidecar contained within
/// that root, never a sibling written outside it that could collide.
pub(crate) fn registry_path_for(
    persistence: &crate::storage::index_persistence::PersistenceConfig,
) -> Option<PathBuf> {
    if !persistence.enabled {
        return None;
    }
    Some(persistence.data_dir.join("snapshots.json"))
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
    /// Create a named snapshot pinned to "the moment of creation" (Issue #3370).
    ///
    /// The two dimensions are defaulted **differently**, each to the correct
    /// notion of "now":
    ///
    /// - **transaction time** = the commit frontier under `current_timestamp`
    ///   (the authoritative, monotonically advancing commit clock). Because the
    ///   commit path advances that frontier strictly monotonically under the
    ///   same lock, every already-committed transaction has a transaction-time
    ///   start `<=` the frontier (visible) and every future commit a start
    ///   strictly `>` it (invisible) — a race-free monotonic pin.
    /// - **valid time** = wallclock `time::now()`, matching the engine's "now"
    ///   convention for `get_node_at_valid_time` / `find_nodes_at_time`. Using
    ///   the (possibly lagging) commit frontier for valid time would silently
    ///   *exclude* facts that are genuinely valid at creation — the frontier
    ///   trails wallclock during write-idle periods, and a #3221 future-dated
    ///   fact already valid by now would be dropped.
    ///
    /// Both coordinates are captured **once** here and stored, so reads through
    /// the resulting handle remain fully deterministic. See the
    /// [module docs](crate::db::snapshot) for the determinism argument and the
    /// future-valid caveat.
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
        // Valid-time "now" is wallclock now (the engine's convention). Captured
        // outside the lock (class-1 lock discipline holds nothing else).
        let valid_now = crate::core::temporal::time::now();
        // Transaction-time "now" is the commit frontier: captured under its own
        // lock (lock class 1) and released immediately — we hold no later-class
        // lock, so the ordering contract is preserved. The frontier advances
        // strictly monotonically under the same lock, which is what makes the
        // transaction-time pin deterministic (future commits are strictly
        // greater, hence invisible).
        let frontier = {
            let ts = self.current_timestamp.lock().map_err(|_| {
                Error::from(crate::core::error::TransactionError::LockPoisoned {
                    resource: "current_timestamp".to_string(),
                })
            })?;
            *ts
        };
        self.create_snapshot_at(name, valid_now, frontier, description)
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
    fn registry_path_is_inside_data_dir() {
        let cfg = crate::storage::index_persistence::PersistenceConfig {
            enabled: true,
            data_dir: PathBuf::from("/var/lib/aletheia/indexes"),
            ..Default::default()
        };
        // The sidecar lives INSIDE data_dir, never a sibling written outside it.
        assert_eq!(
            registry_path_for(&cfg),
            Some(PathBuf::from("/var/lib/aletheia/indexes/snapshots.json"))
        );
    }

    #[test]
    fn registry_path_is_inside_data_dir_root() {
        // A power user who points data_dir at their data root gets the sidecar
        // contained within that root (not a sibling that could collide).
        let cfg = crate::storage::index_persistence::PersistenceConfig {
            enabled: true,
            data_dir: PathBuf::from("/var/lib/aletheia"),
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

    #[cfg(feature = "serde")]
    #[test]
    fn named_snapshot_serde_round_trips_full_hlc() {
        use crate::core::hlc::HybridTimestamp;
        // Coordinates with NONZERO logical counters (two commits in the same
        // wallclock microsecond) must round-trip losslessly — dropping logical
        // would collapse a pin onto its same-microsecond neighbor.
        let snap = NamedSnapshot {
            name: "s".to_string(),
            valid_time: HybridTimestamp::new_unchecked(111, 7),
            transaction_time: HybridTimestamp::new_unchecked(222, 3),
            created_at: HybridTimestamp::new_unchecked(333, 0),
            description: None,
        };
        let json = serde_json::to_string(&snap).unwrap();
        // Timestamps serialize as {wallclock, logical} objects (lossless).
        assert!(json.contains("\"wallclock\":111"), "json was {json}");
        assert!(json.contains("\"logical\":7"), "json was {json}");
        assert!(json.contains("\"wallclock\":222"), "json was {json}");
        assert!(json.contains("\"logical\":3"), "json was {json}");
        let back: NamedSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snap);
        // The logical counter specifically survives (the regression guard).
        assert_eq!(back.valid_time().logical(), 7);
        assert_eq!(back.transaction_time().logical(), 3);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn ts_hlc_reconstructs_exact_wallclock_and_logical() {
        use crate::core::hlc::HybridTimestamp;
        // Direct serde round-trip of a single timestamp with logical > 0.
        #[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
        struct Wrap(#[serde(with = "ts_hlc")] Timestamp);
        let ts = HybridTimestamp::new_unchecked(1_700_000_000_000_000, 42);
        let json = serde_json::to_string(&Wrap(ts)).unwrap();
        let back: Wrap = serde_json::from_str(&json).unwrap();
        assert_eq!(back.0, ts);
        assert_eq!(back.0.wallclock(), 1_700_000_000_000_000);
        assert_eq!(back.0.logical(), 42);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn ts_hlc_accepts_legacy_bare_integer_as_logical_zero() {
        // A legacy v1 sidecar stored bare-integer micros. The tolerant
        // deserializer must still load it (reconstructed with logical 0), so an
        // old file loads instead of hard-failing.
        let legacy = r#"{
            "version": 1,
            "snapshots": [
                { "name": "old", "valid_time": 111, "transaction_time": 222, "created_at": 333 }
            ]
        }"#;
        let parsed: PersistedRegistry = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.version, 1);
        let s = &parsed.snapshots[0];
        assert_eq!(s.valid_time.wallclock(), 111);
        assert_eq!(s.valid_time.logical(), 0);
        assert_eq!(s.transaction_time.wallclock(), 222);
        assert_eq!(s.transaction_time.logical(), 0);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn open_quarantines_corrupt_sidecar_and_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshots.json");
        std::fs::write(&path, b"this is not valid json {{{").unwrap();

        // A corrupt sidecar must NOT brick startup: it quarantines and yields an
        // empty registry (unlike the security-critical auth store).
        let reg = SnapshotRegistry::open(Some(path.clone())).unwrap();
        assert!(
            reg.list().is_empty(),
            "registry starts empty after corruption"
        );

        // The bad bytes are preserved aside for inspection.
        assert!(!path.exists(), "corrupt file was moved aside");
        assert!(
            dir.path().join("snapshots.json.corrupt").exists(),
            "quarantined file exists"
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn open_quarantines_future_version_and_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshots.json");
        // A well-formed sidecar whose version is NEWER than this build
        // understands must be treated exactly like corruption: quarantine and
        // start empty, never brick startup or misread newer-format entries.
        let future = format!(
            r#"{{ "version": {}, "snapshots": [] }}"#,
            PERSIST_FORMAT_VERSION + 1
        );
        std::fs::write(&path, future.as_bytes()).unwrap();

        let reg = SnapshotRegistry::open(Some(path.clone())).unwrap();
        assert!(
            reg.list().is_empty(),
            "future-version registry starts empty"
        );
        // The unsupported file is preserved aside for inspection.
        assert!(!path.exists(), "future-version file was moved aside");
        assert!(
            dir.path().join("snapshots.json.corrupt").exists(),
            "quarantined file exists"
        );
    }

    // NOTE: an unreadable-file (non-NotFound I/O error) `open` case — e.g. a
    // `chmod 000` sidecar exercising the generic `Err(e)` quarantine arm — is
    // deliberately NOT tested here: the suite frequently runs as root (in CI
    // containers), and root bypasses Unix permission bits, so `read_to_string`
    // would succeed and the test could not reliably provoke the error. The arm
    // shares the parse-failure quarantine path, which the corrupt-sidecar and
    // future-version tests already cover.

    #[cfg(feature = "serde")]
    #[test]
    fn insert_rolls_back_on_save_failure() {
        // Point the sidecar at a path whose parent is a *file*, so directory
        // creation / the atomic write cannot succeed and save() returns Err.
        let dir = tempfile::tempdir().unwrap();
        let blocking_file = dir.path().join("not_a_dir");
        std::fs::write(&blocking_file, b"x").unwrap();
        let bad_path = blocking_file.join("nested").join("snapshots.json");

        let reg = SnapshotRegistry::open(Some(bad_path)).unwrap();
        let snap = NamedSnapshot {
            name: "run1".to_string(),
            valid_time: Timestamp::from(1000),
            transaction_time: Timestamp::from(1000),
            created_at: Timestamp::from(1000),
            description: None,
        };
        let err = reg.insert(snap);
        assert!(err.is_err(), "save failure surfaces as Err");
        // RAM did not diverge from disk: the failed insert left no entry.
        assert!(
            reg.get("run1").is_none(),
            "in-memory insert rolled back on save failure"
        );
        assert!(reg.list().is_empty());
    }
}
