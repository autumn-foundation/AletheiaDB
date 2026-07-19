//! Database configuration and initialization.
//!
//! Provides AletheiaDB initialization methods, including AletheiaDB::new() and AletheiaDB::with_unified_config().
use crate::api::transaction::TxVisibilityManager;
use crate::config::AletheiaDBConfig;
use crate::core::error::{Result, ResultExt, StorageError};
use crate::core::id::{IdGenerator, TxIdGenerator};
use crate::core::temporal::time;
use crate::core::version::AnchorConfig;
use crate::db::AletheiaDB;
use crate::index::temporal::TemporalIndexes;
use crate::query::planner::Statistics;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::index_persistence::tracker::PersistenceTracker;
use crate::storage::index_persistence::worker::spawn_background_persistence_thread;
use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
use crate::storage::tiered_storage::{TieredStorage, TieredStorageConfig};
use crate::storage::wal::DurabilityMode;
use crate::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
use parking_lot::RwLock;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Validate and clamp a configured `persistence.max_interned_strings` value
/// before it drives the process-global interner cap (configurable interner cap).
///
/// - `0` is **rejected** with a clear error: a zero cap would refuse every
///   string intern, bricking all writes and `open()` itself. It must never be
///   pushed into `GLOBAL_INTERNER.set_max_capacity`.
/// - Any value at or above `u32::MAX` is **clamped** to `u32::MAX`: interner ids
///   are `AtomicU32`, so a higher cap is unreachable and would only mislead.
fn validate_interner_cap(configured: usize) -> Result<usize> {
    if configured == 0 {
        return Err(crate::core::error::Error::FailedPrecondition(
            "persistence.max_interned_strings must be at least 1; a cap of 0 would reject all \
             string interning and make every write (and open) fail"
                .to_string(),
        ));
    }
    Ok(configured.min(u32::MAX as usize))
}

/// The outcome of reconciling a newly-opened database's requested interner cap
/// against the process-global cap already in effect (Issue #3724).
///
/// The string interner is a single process-wide static, so there is exactly one
/// effective cap shared by every `AletheiaDB` opened in the process. This enum is
/// the pure decision of the "max-of-all-opens high-water mark" policy; see
/// [`decide_interner_cap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InternerCapDecision {
    /// The first open in the process establishes the baseline cap (honoring an
    /// intentionally low DoS bound for the common single-DB deployment, even below
    /// the default seed).
    Established(usize),
    /// A later open requested MORE headroom than the current effective cap; the
    /// shared cap is raised to it. Legitimate and silent.
    Raised(usize),
    /// A later open requested a LOWER cap than the current effective cap. The lower
    /// value is NOT applied — lowering the shared cap could refuse writes on an
    /// already-open database that may already hold more strings than `requested`.
    /// The higher `effective` cap is kept and a loud warning is emitted.
    KeptHigher { requested: usize, effective: usize },
    /// A later open requested exactly the current effective cap; nothing changes.
    Unchanged(usize),
}

/// Decide how a newly-opened database's `requested` interner cap reconciles with
/// the process-global cap already in effect, under the "max-of-all-opens
/// high-water mark" policy (Issue #3724).
///
/// Pure and side-effect-free so it can be unit-tested exhaustively without
/// touching the process-global interner:
///
/// - The **first** open (`established == false`) establishes the baseline = its
///   requested cap, so a single DB can still set a deliberately low DoS bound.
/// - Every **subsequent** open may only ever ADD headroom: the effective cap
///   becomes `max(current, requested)` and is never lowered. A subsequently-opened
///   lower cap therefore can no longer refuse writes on an already-open database.
fn decide_interner_cap(established: bool, current: usize, requested: usize) -> InternerCapDecision {
    if !established {
        return InternerCapDecision::Established(requested);
    }
    match requested.cmp(&current) {
        std::cmp::Ordering::Greater => InternerCapDecision::Raised(requested),
        std::cmp::Ordering::Less => InternerCapDecision::KeptHigher {
            requested,
            effective: current,
        },
        std::cmp::Ordering::Equal => InternerCapDecision::Unchanged(current),
    }
}

/// Whether any database open in this process has already established the shared
/// interner cap. Guards the first-open-establishes vs subsequent-raise-only arms
/// of [`decide_interner_cap`].
static INTERNER_CAP_ESTABLISHED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Serializes concurrent `open()`s racing to establish/raise the process-global
/// interner cap, so the first-open-establishes / raise-only decision is atomic
/// with respect to the read-then-write of `GLOBAL_INTERNER.max_capacity()`.
static INTERNER_CAP_LOCK: Mutex<()> = Mutex::new(());

/// Apply a validated `requested` interner cap to the process-global interner under
/// the "max-of-all-opens high-water mark" policy (Issue #3724).
///
/// A later open may only ever raise the shared cap, never lower it, so a
/// subsequently-opened lower cap cannot break writes on an already-open database.
/// When a lower request is clamped up (not honored), a loud warning is emitted.
///
/// The high-water-mark invariant is enforced **only** on this open seam. The
/// `pub(crate)` [`StringInterner::set_max_capacity`] primitive still stores
/// exactly what it is given (a handful of in-crate/test callers rely on lowering
/// it directly); production databases all funnel their cap through here.
fn apply_interner_cap(requested: usize) {
    // Poisoning only happens if a prior holder panicked; the guarded region is a
    // couple of atomic ops with no panic path, so recover the guard either way.
    let _guard = INTERNER_CAP_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let interner = &*crate::core::GLOBAL_INTERNER;
    let established = INTERNER_CAP_ESTABLISHED.swap(true, std::sync::atomic::Ordering::AcqRel);
    let decision = decide_interner_cap(established, interner.max_capacity(), requested);
    match decision {
        InternerCapDecision::Established(cap) | InternerCapDecision::Raised(cap) => {
            interner.set_max_capacity(cap);
        }
        InternerCapDecision::Unchanged(_) => {}
        InternerCapDecision::KeptHigher {
            requested,
            effective,
        } => {
            warn_interner_cap_kept_higher(requested, effective);
        }
    }
}

/// Emit the loud, one-line warning for the case where a later open requested a
/// LOWER interner cap than the process-global cap already in effect and the lower
/// value was therefore not applied (Issue #3724).
///
/// Uses `tracing` when the `observability` feature is compiled and falls back to
/// `eprintln!` (the persistence layer's best-effort-stderr convention) otherwise,
/// so the warning is loud in every build configuration, including
/// `--no-default-features`.
fn warn_interner_cap_kept_higher(requested: usize, effective: usize) {
    #[cfg(feature = "observability")]
    tracing::warn!(
        requested,
        effective,
        "persistence.max_interned_strings is lower than the process-global \
         string-interner cap already in effect; the interner is shared across all \
         databases opened in this process, so the lower cap is NOT applied \
         (keeping the higher effective cap) to avoid refusing writes on an \
         already-open database. See Issue #3724."
    );
    #[cfg(not(feature = "observability"))]
    eprintln!(
        "WARNING: persistence.max_interned_strings={requested} is lower than the \
         process-global string-interner cap already in effect ({effective}); the \
         interner is shared across all databases opened in this process, so the \
         lower cap is NOT applied (keeping {effective}) to avoid refusing writes on \
         an already-open database. See Issue #3724."
    );
}

fn bootstrap_timestamp(
    current: &CurrentStorage,
    historical: &RwLock<HistoricalStorage>,
) -> crate::core::temporal::Timestamp {
    let mut max_timestamp = time::now();

    for node in current.iter_nodes() {
        if let Some(commit_ts) = node.metadata.commit_timestamp
            && commit_ts > max_timestamp
        {
            max_timestamp = commit_ts;
        }
    }

    for edge in current.iter_edges() {
        if let Some(commit_ts) = edge.metadata.commit_timestamp
            && commit_ts > max_timestamp
        {
            max_timestamp = commit_ts;
        }
    }

    let historical = historical.read();
    for node_version in historical.get_node_versions().values() {
        let tx_time = node_version.temporal.transaction_time();
        if tx_time.start() > max_timestamp {
            max_timestamp = tx_time.start();
        }
        // Issue #3387: restored versions carry CLOSED tx ends too. A closure
        // stamped by a superseding version that was since cold-migrated may
        // exceed every restored tx start; fold it in so the HLC seed stays
        // monotonic under clock skew.
        if !tx_time.is_current() && tx_time.end() > max_timestamp {
            max_timestamp = tx_time.end();
        }
    }

    for edge_version in historical.get_edge_versions().values() {
        let tx_time = edge_version.temporal.transaction_time();
        if tx_time.start() > max_timestamp {
            max_timestamp = tx_time.start();
        }
        if !tx_time.is_current() && tx_time.end() > max_timestamp {
            max_timestamp = tx_time.end();
        }
    }

    max_timestamp
}

pub(crate) fn seed_startup_current_timestamp(db: &AletheiaDB) -> Result<()> {
    let startup_timestamp = bootstrap_timestamp(&db.current, &db.historical);
    let mut current_timestamp = db.current_timestamp.lock().map_err(|_| {
        crate::core::error::Error::Storage(StorageError::LockPoisoned {
            resource: "current_timestamp".to_string(),
        })
    })?;
    *current_timestamp = startup_timestamp;
    Ok(())
}

/// Seed the WAL LSN allocator from durable state at startup (Issue #3420).
///
/// Scans the WAL directory for the maximum LSN present in existing segments
/// and moves the allocator to `max + 1` (never backwards). Must run after WAL
/// construction and **before any write is accepted**; otherwise a restarted
/// process starts allocating at LSN 1 again, producing duplicate LSNs across
/// segments and writes that land below the index manifest LSN — which the
/// next startup's differential replay then silently skips.
///
/// Seeding policy lives here (in the database startup path), not inside the
/// WAL constructor, so WAL-crate users keep full control over allocator state.
fn seed_lsn_allocator_from_segments(
    wal: &ConcurrentWalSystem,
    cipher: Option<&Arc<dyn crate::encryption::cipher::Cipher>>,
) -> Result<()> {
    let max_lsn = crate::storage::wal::segment_reader::max_lsn_in_dir(wal.wal_dir(), cipher)?;
    seed_lsn_allocator_from_max(wal, max_lsn);
    Ok(())
}

/// Move the allocator to `max_lsn + 1` (never backwards), given a max LSN that
/// the caller has already determined (Issue #3429).
///
/// Split out of [`seed_lsn_allocator_from_segments`] so the startup path can
/// seed from the max LSN of the single full WAL read it performs, instead of
/// scanning the segment directory a second time purely to compute it. See
/// [`seed_lsn_allocator_from_segments`] for the seeding-policy rationale.
fn seed_lsn_allocator_from_max(
    wal: &ConcurrentWalSystem,
    max_lsn: Option<crate::storage::wal::LSN>,
) {
    if let Some(max_lsn) = max_lsn {
        let next = crate::storage::wal::LSN(max_lsn.0.saturating_add(1));
        if next > wal.current_lsn() {
            wal.set_next_lsn(next);
        }
    }
}

/// Extract the effective flush interval from a durability mode, falling back to a default.
fn flush_interval_from_durability(mode: DurabilityMode, default_ms: u64) -> u64 {
    match mode {
        DurabilityMode::Async { flush_interval_ms } => flush_interval_ms,
        DurabilityMode::GroupCommit { max_delay_ms, .. } => max_delay_ms,
        DurabilityMode::AsyncBatched { max_delay_ms, .. } => max_delay_ms,
        _ => default_ms,
    }
}

/// Wire temporal indexes into historical storage in a single write-lock acquisition.
fn wire_temporal_indexes(db: &AletheiaDB) {
    let temporal_adjacency_index = Arc::new(
        crate::index::temporal_adjacency::TemporalAdjacencyIndex::new(
            crate::index::temporal_adjacency::TemporalAdjacencyConfig::default(),
        ),
    );

    let mut hist = db.historical.write();
    hist.set_temporal_indexes(Arc::clone(&db.temporal_indexes));
    hist.set_temporal_adjacency_index(temporal_adjacency_index);
    // Populate the temporal index from any versions already in historical storage
    // (loaded from index persistence + WAL replay). Without this, temporal
    // point-in-time queries would silently miss those versions.
    hist.rebuild_temporal_index_from_versions();
}

/// Reconcile the namespace registry with the entities actually present after a
/// load (Issue #3349, PR2). Every namespace that holds at least one node or edge
/// in the rebuilt membership index is ensured-registered, so a scoped read
/// naming it validates (rather than returning `NOT_FOUND`) after a load path
/// that populates entities without the `namespaces.json` sidecar — notably
/// `.albk` restore, whose payload carries the ride-along namespace on each
/// entity but not the registry sidecar.
///
/// Idempotent and off the hot path: on a durable reopen (sidecar already loaded)
/// every name is already registered, so this is a cheap no-op scan. A per-name
/// persist failure never bricks startup — the entity data is already loaded and
/// the membership index is authoritative for isolation.
///
/// v1 caveat: an *empty* explicitly-created namespace (registered but holding no
/// entities) is not carried across `.albk` restore, because the registry sidecar
/// is not part of the backup payload; folding the registry into the payload is a
/// deliberate follow-up (`// TODO(#3349)`).
fn reconcile_namespace_registry(db: &AletheiaDB) {
    for ns in db.current.populated_namespaces() {
        if let Err(_e) = db.namespaces.ensure_registered(&ns) {
            #[cfg(feature = "observability")]
            tracing::warn!(
                namespace = %ns,
                error = %_e,
                "failed to auto-register namespace during load reconciliation"
            );
        }
    }
}

impl AletheiaDB {
    /// Create a new **ephemeral** in-memory database.
    ///
    /// The WAL is backed by a freshly created temporary directory that is
    /// removed automatically when the returned [`AletheiaDB`] is dropped. No
    /// state survives the process; nothing is loaded from prior runs.
    ///
    /// This is the right constructor for tests, scratch sessions, and quick
    /// experiments. For durable storage that persists across restarts, use
    /// [`Self::open`] — the one-line durable counterpart to this
    /// constructor. Power users needing full control can call
    /// [`Self::with_unified_config`] with a config built from
    /// [`crate::config::durable_config_for_data_dir`], or
    /// [`Self::open_from_env`] to honor the `ALETHEIADB_DATA_DIR`
    /// environment variable.
    ///
    /// # Errors
    ///
    /// Returns an error if the temporary directory cannot be created or WAL
    /// initialization fails inside it.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn new() -> Result<Self> {
        let tempdir = tempfile::Builder::new()
            .prefix("aletheiadb-")
            .tempdir()
            .map_err(crate::core::error::Error::Io)?;
        let wal_config = crate::config::WalConfigBuilder::new()
            .wal_dir(tempdir.path().join("wal"))
            .build();
        let mut db = Self::with_full_config(AnchorConfig::default(), wal_config)?;
        db._tempdir = Some(tempdir);
        Ok(db)
    }

    /// Open a database honouring the `ALETHEIADB_CONFIG` and `ALETHEIADB_DATA_DIR`
    /// environment variables.
    ///
    /// Precedence (first match wins):
    ///
    /// 1. `ALETHEIADB_CONFIG=/path/to/config.toml` — load the full
    ///    [`AletheiaDBConfig`] from TOML. Requires the `config-toml` feature
    ///    (enabled by default); without that feature this returns an error
    ///    when the variable is set.
    /// 2. `ALETHEIADB_DATA_DIR=/path` — open a durable database rooted at
    ///    that path via [`Self::open`].
    /// 3. Neither set — fall back to [`Self::new`] (ephemeral, tempdir-backed).
    ///
    /// This is the entry point every exposed binary (HTTP server, MCP server,
    /// CLI, Python SDK) calls so the persistence story is consistent.
    ///
    /// # Errors
    ///
    /// Returns an error if the TOML file cannot be read or parsed, if WAL
    /// initialization fails, or if index loading fails.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn open_from_env() -> Result<Self> {
        if let Some(path) = crate::config::config_path_from_env() {
            return Self::open_from_toml_path(&path);
        }
        if let Some(path) = crate::config::data_dir_from_env() {
            return Self::open(path);
        }
        Self::new()
    }

    /// Open (or create) a **durable** database rooted at `path`.
    ///
    /// This is the one-line entry point for embedding a durable AletheiaDB:
    /// it creates the directory tree at `path` if absent, and opens an
    /// existing one otherwise, replaying any prior state so calls are
    /// idempotent across process restarts. Internally it is exactly
    /// [`Self::with_unified_config`] with a config built by
    /// [`crate::config::durable_config_for_data_dir`] — WAL + index
    /// persistence with `load_on_startup`, group-commit durability — so
    /// behavior stays in one canonical place and does not fork config
    /// defaults. For an ephemeral, tempdir-backed database, use
    /// [`Self::new`] instead.
    ///
    /// # Errors
    ///
    /// Returns an error if `path` is not writable, WAL initialization
    /// fails, or index loading fails. Never falls back to an ephemeral
    /// database on failure.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::{AletheiaDB, PropertyMapBuilder};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let dir = tempfile::tempdir()?;
    ///
    /// let node_id = {
    ///     let db = AletheiaDB::open(dir.path())?;
    ///     db.create_node(
    ///         "Person",
    ///         PropertyMapBuilder::new().insert("name", "Alice").build(),
    ///     )?
    ///     // `db` drops here, persisting final state.
    /// };
    ///
    /// // Reopening the same path replays the prior state.
    /// let db = AletheiaDB::open(dir.path())?;
    /// let node = db.get_node(node_id)?;
    /// assert_eq!(
    ///     node.properties.get("name").and_then(|v| v.as_str()),
    ///     Some("Alice")
    /// );
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        Self::with_unified_config(crate::config::durable_config_for_data_dir(
            path.as_ref().to_path_buf(),
        ))
    }

    /// Load a TOML config and open the database with it. Used by
    /// [`Self::open_from_env`] when `ALETHEIADB_CONFIG` is set.
    #[cfg(feature = "config-toml")]
    fn open_from_toml_path(path: &std::path::Path) -> Result<Self> {
        let config = AletheiaDBConfig::from_toml_file(path).map_err(|e| {
            crate::core::error::Error::Other(format!(
                "failed to load TOML config from {}: {e}",
                path.display()
            ))
        })?;
        Self::with_unified_config(config)
    }

    #[cfg(not(feature = "config-toml"))]
    fn open_from_toml_path(_path: &std::path::Path) -> Result<Self> {
        Err(crate::core::error::Error::Other(format!(
            "{} is set but the `config-toml` feature is not enabled in this build",
            crate::config::CONFIG_ENV,
        )))
    }

    /// Create a new database with custom anchor configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if WAL initialization fails (e.g., cannot create WAL directory).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn with_config(config: AnchorConfig) -> Result<Self> {
        Self::with_full_config(config, crate::config::WalConfig::default())
    }

    /// Create a new database with custom WAL configuration.
    ///
    /// This allows configuring the durability mode and other WAL settings.
    ///
    /// # Errors
    ///
    /// Returns an error if WAL initialization fails (e.g., cannot create WAL directory).
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::{AletheiaDB, WalConfigBuilder, DurabilityMode};
    ///
    /// // High-throughput ACID mode with group commit
    /// let wal_config = WalConfigBuilder::new()
    ///     .durability_mode(DurabilityMode::group_commit(10, 200))
    ///     .build();
    /// let db = AletheiaDB::with_wal_config(wal_config)?;
    ///
    /// // Bulk loading mode with async durability
    /// let wal_config = WalConfigBuilder::new()
    ///     .durability_mode(DurabilityMode::async_mode(100))
    ///     .build();
    /// let db = AletheiaDB::with_wal_config(wal_config)?;
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn with_wal_config(wal_config: crate::config::WalConfig) -> Result<Self> {
        Self::with_full_config(AnchorConfig::default(), wal_config)
    }

    /// Create a new database with unified configuration.
    ///
    /// This method accepts a [`AletheiaDBConfig`] which consolidates all configuration
    /// settings for the database, including WAL, historical storage, and vector indexes.
    ///
    /// # Errors
    ///
    /// Returns an error if WAL initialization fails (e.g., cannot create WAL directory).
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::{AletheiaDB, config::AletheiaDBConfig, config::WalConfigBuilder};
    ///
    /// let config = AletheiaDBConfig::builder()
    ///     .wal(WalConfigBuilder::new()
    ///         .with_validated(32, 2048, 64 * 1024, 64 * 1024 * 1024, 10, 10).unwrap()
    ///         .build())
    ///     .build();
    ///
    /// let db = AletheiaDB::with_unified_config(config)?;
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn with_unified_config(config: AletheiaDBConfig) -> Result<Self> {
        let result = (|| {
            // Rebound mutable so the durable encryption-state authority
            // (Issue #3616) can override `config.encryption` below.
            let mut config = config;
            let durability_mode = config.wal.durability_mode;

            // Reconfigure the process-global string-interner cap from
            // `persistence.max_interned_strings` BEFORE any WAL bootstrap
            // deserialization (which interns property keys) or index load (which
            // re-interns persisted strings and validates the persisted count
            // against this cap). Doing it here guarantees both the runtime
            // intern cap and the load-validation cap reflect the configured
            // value for the rest of open(). Configurable interner cap fix.
            //
            // A cap of 0 would reject ALL interning and brick every write and
            // open(); reject it with a clear error instead of silently bricking
            // the DB. A cap above `u32::MAX` is unreachable (ids are `AtomicU32`),
            // so it is clamped so the effective cap is honest.
            //
            // The interner is process-global (Issue #3724): rather than
            // last-open-wins (which would let a later, LOWER cap refuse writes on
            // an already-open database), `apply_interner_cap` applies a
            // max-of-all-opens high-water mark — the first open establishes the
            // baseline and later opens may only RAISE the shared cap, never lower
            // it, warning loudly when a lower request is clamped up.
            let effective_interner_cap =
                validate_interner_cap(config.persistence.max_interned_strings)?;
            apply_interner_cap(effective_interner_cap);

            // Capture provenance-chain settings before `config.wal.wal_dir` is
            // moved into the WAL system config. The chain lives under the data
            // dir (the parent of the WAL dir) unless an explicit override is set.
            let chain_config = config.chain.clone();
            let chain_data_dir: std::path::PathBuf = config
                .wal
                .wal_dir
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| std::path::PathBuf::from("."));

            // Issue #3616: the durable encryption-state authority
            // (`{persistence.data_dir}/encryption.state`) is the AUTHORITY for
            // the encryption on/off decision — it WINS over the TOML/env-derived
            // `EncryptionConfig`. This is what lets an `encryption enable`
            // migration flip a database to encrypted-at-rest so the NEXT open()
            // uses the cipher even though the operator's plaintext config is
            // unchanged. Precedence & fail-closed contract (see
            // `db::encryption_state`):
            //   * absent           -> no override; today's TOML/env behavior.
            //   * enabled + source -> force encryption ON under that key source.
            //   * disabled         -> force encryption OFF.
            //   * corrupt / enabled-without-source -> read returns Err -> open
            //     aborts loudly (fail-closed; NEVER a silent plaintext fallback).
            // On DIVERGENCE a LOUD warning naming BOTH sources is logged so an
            // operator is never silently overridden.
            if config.persistence.enabled
                && let Some(authority) = crate::db::encryption_state::read_encryption_state(
                    &config.persistence.data_dir,
                )?
            {
                if authority.enabled != config.encryption.enabled {
                    crate::db::encryption_state::log_authority_warning(&format!(
                        "encryption-state authority at {} records enabled={}, OVERRIDING the \
                         loaded config (encryption.enabled={}). The durable authority wins; \
                         opening with enabled={}.",
                        crate::db::encryption_state::encryption_state_path(
                            &config.persistence.data_dir
                        )
                        .display(),
                        authority.enabled,
                        config.encryption.enabled,
                        authority.enabled,
                    ));
                }
                if authority.enabled {
                    // The reader guarantees an enabled authority carries a key
                    // source. Compose that key SOURCE with the config's
                    // algorithm/audit; a missing/unreadable key then fails
                    // `EncryptionManager::from_config` below, so open() refuses
                    // rather than falling back to plaintext (fail-closed).
                    let key_source =
                        authority
                            .key_source
                            .ok_or_else(|| -> crate::core::error::Error {
                                crate::core::error::StorageError::InconsistentState {
                                    reason:
                                        "encryption-state authority is enabled but resolved no \
                                         key source"
                                            .to_string(),
                                }
                                .into()
                            })?;
                    config.encryption.enabled = true;
                    config.encryption.key_provider = key_source;
                    // Issue #3616 PR3: the authority also PINS the concrete AEAD
                    // algorithm the enable migration wrote under. Override the
                    // operator's `config.encryption.algorithm` with it so the DB
                    // reopens under the EXACT cipher it was written with — immune to
                    // a later TOML edit (e.g. a concrete `chacha20` that Auto did not
                    // resolve to) and to a cross-CPU host migration (`Auto` resolving
                    // AES on one host, ChaCha on another). A legacy `version=1`
                    // authority pins nothing (`None`) → the config algorithm stands,
                    // exactly the prior behavior.
                    if let Some(pinned) = authority.algorithm {
                        config.encryption.algorithm = pinned;
                    }
                } else {
                    config.encryption.enabled = false;
                }
            }

            // Issue #3616 PR4: an INTERRUPTED encrypted → plaintext disable leaves
            // the durable authority STILL `enabled` (the flip to `disabled` is the
            // commit point, done LAST), so `config.encryption.enabled` is `true`
            // here even though the on-disk bytes are being unwrapped to plaintext.
            // But the disable END STATE is plaintext, and a live encrypted index
            // manager / cold store would RE-ENCRYPT over the freshly-unwrapped bytes
            // on the next background persist / cold migration. So on a pending-disable
            // startup we build every at-rest layer PLAINTEXT (the end state) by
            // FORCING `enabled=false`, and resolve the DECRYPT ciphers from the
            // pending disable ledger for the transient resume read passes: the
            // pre-read WAL hook installs the decrypt keyring so the replay decrypts
            // the still-encrypted segments, the index unwrap decrypts each `AEIX`
            // file explicitly, and the cold store is built under the decrypt cold DEK
            // so its `ACV1` → bare unwrap can decrypt. `None` on the common path (no
            // pending disable) reproduces prior behavior exactly. A ledger is either
            // an enable OR a disable ledger, never both, so this never collides with
            // the enable-resume path below.
            let disable_resume = if config.persistence.enabled && config.encryption.enabled {
                crate::db::rotation::disable_resume_ciphers(
                    &config.persistence.data_dir,
                    config.encryption.algorithm,
                )?
            } else {
                None
            };
            if disable_resume.is_some() {
                // Force the plaintext (end-state) build; the resume read passes use
                // the decrypt ciphers resolved above, and the authority is flipped to
                // `disabled` only once every layer is unwrapped.
                config.encryption.enabled = false;
            }

            // Issue #3616 PR3: an INTERRUPTED plaintext → encrypted enable leaves
            // the durable authority NOT yet flipped, so `config.encryption.enabled`
            // is still `false` here even though the on-disk WAL/index/cold bytes may
            // already have been (partially) migrated. On that branch, resolve the
            // enable DEKs from the pending ENABLE ledger so the at-rest layers below
            // (index manager, cold store) are built UNDER those DEKs — otherwise
            // they would be built plaintext and then fail to read (or over-write
            // with plaintext) the freshly-wrapped `AEIX`/`ACV1` bytes the resume
            // passes produce. The WAL is handled separately by the pre-read hook.
            // `None` on the common path (no pending enable) reproduces prior
            // behavior exactly.
            let enable_resume = if config.persistence.enabled && !config.encryption.enabled {
                crate::db::rotation::enable_resume_ciphers(
                    &config.persistence.data_dir,
                    config.encryption.algorithm,
                )?
            } else {
                None
            };

            // Create encryption manager if encryption is enabled
            let encryption_manager = if config.encryption.enabled {
                let manager = crate::encryption::EncryptionManager::from_config(&config.encryption)
                    .map_err(|e| -> crate::core::error::Error {
                        crate::core::error::StorageError::KeyProvider(e.to_string()).into()
                    })?;
                Some(Arc::new(manager))
            } else {
                None
            };

            // Retain the encryption config for key rotation (Issue #488): the
            // current MEK must be re-sourceable to guard against rotating to the
            // same key. Only stored when encryption is enabled; never the key.
            let encryption_config_stored = if config.encryption.enabled {
                Some(config.encryption.clone())
            } else {
                None
            };

            // Extract WAL cipher from encryption manager (if enabled)
            let wal_cipher = encryption_manager
                .as_ref()
                .map(|mgr| Arc::clone(mgr.wal_cipher()));

            // Issue #488 version-provisioning: derive the keyring `current_version`
            // from durable on-disk state (index-file AEIX headers + WAL segment
            // headers, with a pending rotation ledger as authority) so a
            // rotate-then-cold-reopen reports the real version instead of a
            // hard-coded 1. Only when encryption AND persistence are enabled;
            // otherwise `None` reproduces prior behavior exactly. Computed here,
            // before the WAL is built, so both keyrings are provisioned in
            // lockstep. See `resolve_provisioned_key_version` for precedence.
            let provisioned_key_version = if config.encryption.enabled && config.persistence.enabled
            {
                Some(crate::db::rotation::resolve_provisioned_key_version(
                    &config.persistence.data_dir.join("indexes"),
                    &config.wal.wal_dir,
                    &config.persistence.data_dir,
                )?)
            } else {
                None
            };

            let wal_system_config = ConcurrentWalSystemConfig {
                wal_dir: config.wal.wal_dir,
                num_stripes: config.wal.num_stripes,
                stripe_capacity: config.wal.stripe_capacity,
                segment_size: config.wal.segment_size,
                segments_to_retain: config.wal.segments_to_retain,
                flush_interval_ms: flush_interval_from_durability(
                    durability_mode,
                    config.wal.flush_interval_ms,
                ),
                durability_mode,
                write_buffer_size: config.wal.write_buffer_size,
                wal_cipher: wal_cipher.clone(),
                wal_key_version: provisioned_key_version,
                tolerate_torn_tail: config.wal.tolerate_torn_tail,
            };

            let wal = Arc::new(ConcurrentWalSystem::new(wal_system_config)?);

            // Issue #3429: decode the WAL exactly once here and reuse that
            // single decode for all three startup passes that historically each
            // re-read every segment: (1) the LSN-allocator seed below, (2) the
            // constraint declaration net-state, and (3) the differential
            // node/edge replay. `read_from` mirrors the recovery reader's cipher
            // behavior (plaintext today), so for an encrypted WAL its max LSN
            // would omit encrypted entries; the cipher-aware directory scan is
            // used as a targeted fallback below only when a cipher is
            // configured, preserving the #3420/#3430 encrypted seed exactly.
            //
            // Issue #3617 PR2 (crash-consistency): if a WAL rotation ledger is
            // PENDING, install BOTH the old and new WAL key generations into the
            // keyring BEFORE the replay read, so a half-rotated WAL (old-DEK +
            // new-DEK segments both on disk) decrypts regardless of generation.
            // Without this the read below fails on new-DEK frames before
            // `resume_pending_rotation` (further down) can install the new
            // generation. Additive: a no-op when no rotation is pending.
            if config.encryption.enabled && config.persistence.enabled {
                crate::db::rotation::install_pending_wal_generations(
                    &wal,
                    &config.encryption,
                    &config.persistence.data_dir,
                )?;
            } else if config.persistence.enabled {
                // Issue #3616 PR3: an INTERRUPTED plaintext→encrypted enable leaves
                // the authority NOT yet flipped, so `config.encryption.enabled` is
                // false here even though the WAL may already have been rolled to
                // encrypted. Install the WAL keyring from the pending enable ledger
                // BEFORE the replay read so those encrypted segments decrypt
                // (otherwise the read below mis-reads ciphertext as undecodable
                // plaintext and fails). `resume_pending_enable` (further down)
                // finishes the migration. A no-op when no enable ledger is pending
                // or the WAL is not yet encrypted.
                crate::db::rotation::install_pending_enable_wal_keyring(
                    &wal,
                    &config.encryption,
                    &config.persistence.data_dir,
                )?;
                // Issue #3616 PR4: mirror for an INTERRUPTED disable — the WAL was
                // built plaintext (end state) above, but the on-disk segments may
                // still be encrypted (uninstall/retire not finished). Install the
                // DECRYPT keyring from the pending disable ledger BEFORE the replay
                // read so those encrypted segments decrypt; `resume_pending_disable`
                // (further down) then uninstalls it back to plaintext and retires the
                // encrypted tail. A no-op when no disable ledger is pending or the
                // encrypted WAL tail is already retired.
                crate::db::rotation::install_pending_disable_wal_keyring(
                    &wal,
                    &config.encryption,
                    &config.persistence.data_dir,
                )?;
            }

            // Held (as `mut`, consumed by whichever replay branch runs) across
            // index load so no later pass re-reads the segment directory.
            let mut startup_wal_entries = wal.read_from(crate::storage::wal::LSN::initial())?;

            // Issue #3420: seed the LSN allocator past every LSN already durable
            // in existing WAL segments, BEFORE any write is accepted. Without
            // this, a restarted process re-allocates LSNs starting at 1,
            // breaking LSN total ordering across segments and placing new
            // writes below the index manifest LSN (so the next startup's
            // differential replay silently skips them).
            let seed_max_lsn = if wal_cipher.is_some() {
                // Encrypted segments are invisible to the cipher-less read
                // above; fall back to the cipher-aware directory scan so the
                // seed still accounts for encrypted entries (unchanged #3420
                // behavior for encrypted WALs; the constraint/replay passes
                // stay cipher-less exactly as before, per #3430).
                crate::storage::wal::segment_reader::max_lsn_in_dir(
                    wal.wal_dir(),
                    wal_cipher.as_ref(),
                )?
            } else {
                // `read_from` returns entries globally sorted by LSN
                // (segment_reader: `entries.sort_by_key(|e| e.lsn)`), so the
                // last element carries the max LSN in O(1). Empty WAL -> `None`
                // -> the seed is a no-op.
                startup_wal_entries.last().map(|e| e.lsn)
            };
            seed_lsn_allocator_from_max(&wal, seed_max_lsn);

            // Create persistence manager if enabled. When encryption is
            // enabled, thread the index cipher through so index files are
            // encrypted at rest (Issue #481); mirrors the WAL cipher wiring
            // above. Encryption disabled => None => plaintext, unchanged.
            let persistence_manager = if config.persistence.enabled {
                // Issue #3616 PR3: on an interrupted-enable resume the manager must
                // read/write under the enable index DEK (checkpoints ride it), so a
                // half-wrapped index dir loads and the resume wrap pass publishes
                // `AEIX` the same open() then reads back. Otherwise the index cipher
                // comes from the (enabled) encryption manager as before.
                let index_cipher = enable_resume
                    .as_ref()
                    .map(|e| Arc::clone(&e.index))
                    .or_else(|| {
                        encryption_manager
                            .as_ref()
                            .map(|mgr| Arc::clone(mgr.index_cipher()))
                    });
                // Issue #488 version-provisioning: build the index keyring at the
                // resolved on-disk version so `index_rotation_status` /
                // `encryption verify` classify rotated files correctly after a
                // cold reopen and the next rotation increments from the real
                // version. Reads stay `match_any` (identical to `with_cipher`);
                // `None` falls back to `with_cipher` (unchanged behavior).
                let manager = match provisioned_key_version {
                    Some(v) => crate::storage::index_persistence::IndexPersistenceManager::with_cipher_versioned(
                        &config.persistence.data_dir,
                        index_cipher,
                        v,
                    ),
                    None => crate::storage::index_persistence::IndexPersistenceManager::with_cipher(
                        &config.persistence.data_dir,
                        index_cipher,
                    ),
                };
                Some(Arc::new(manager))
            } else {
                None
            };

            // Create persistence tracker if persistence is enabled
            let persistence_tracker = if config.persistence.enabled {
                Some(Arc::new(PersistenceTracker::new()))
            } else {
                None
            };

            // Extract cold storage configuration before config.historical is moved
            let enable_cold_storage = config.historical.enable_cold_storage;
            let cold_storage_path = config.historical.cold_storage_path.clone();

            // Capture the changefeed caps (incl. the Issue #3678 per-principal
            // quota) before `config` fields are moved, so the broadcaster is
            // constructed from the unified config rather than defaults.
            let changefeed_config = config.changefeed.clone();

            // Named-snapshot registry (Issue #3370): durable sidecar at
            // `{data_dir}/snapshots.json` when index persistence is enabled,
            // in-memory-only otherwise. Loaded here so pins survive restart.
            let snapshots = Arc::new(crate::db::snapshot::SnapshotRegistry::open(
                crate::db::snapshot::registry_path_for(&config.persistence),
            )?);

            // Namespace registry (Issue #3349): durable sidecar at
            // `{data_dir}/namespaces.json` when index persistence is enabled,
            // in-memory-only otherwise. Loaded here so registrations survive restart.
            let namespaces = Arc::new(crate::db::namespace::NamespaceRegistry::open(
                crate::db::namespace::registry_path_for(&config.persistence),
            )?);

            // GDPR crypto-shred (Issue #3359): load the durable per-subject
            // keyring fail-closed and run breadcrumb crash-recovery. Co-located
            // with the schema-constraint sidecar in the data root `{D}`;
            // in-memory-only when index persistence is disabled.
            #[cfg(feature = "audit-export")]
            let crypto_shred = {
                let crypto_shred_path = if config.persistence.enabled {
                    Some(chain_data_dir.join(crate::db::crypto_shred::keyring::KEYRING_FILENAME))
                } else {
                    None
                };
                Arc::new(crate::db::crypto_shred::CryptoShredState::open(
                    crypto_shred_path,
                )?)
            };

            let mut db = AletheiaDB {
                current: Arc::new(CurrentStorage::new()),
                historical: Arc::new(RwLock::new(HistoricalStorage::from_unified_config(
                    config.historical,
                ))),
                temporal_indexes: Arc::new(TemporalIndexes::new()),
                wal,
                in_flight: Arc::new(crate::api::transaction::write::InFlightLsns::new()),
                current_timestamp: Arc::new(Mutex::new(time::now())),
                commit_clock_observed_at: Arc::new(Mutex::new(Instant::now())),
                tx_id_gen: Arc::new(TxIdGenerator::new()),
                visibility_manager: Arc::new(TxVisibilityManager::new()),
                node_id_gen: Arc::new(IdGenerator::new()),
                edge_id_gen: Arc::new(IdGenerator::new()),
                version_id_gen: Arc::new(IdGenerator::new()),
                default_durability: durability_mode,
                stats: Arc::new(Statistics::new()),
                persistence_config: config.persistence.clone(),
                persistence_manager: persistence_manager.clone(),
                persistence_tracker: persistence_tracker.clone(),
                persistence_thread_stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                persistence_thread_handle: None,
                encryption_manager: encryption_manager.clone(),
                encryption_config: encryption_config_stored,
                // Retained even when encryption is disabled (a plaintext DB that may
                // later be enabled) so `enable_encryption` writes + pins the operator's
                // configured algorithm (Issue #3616 PR3). On the reopen path this is the
                // authority-overridden concrete algorithm; on a plaintext DB it is the
                // raw config value (possibly `Auto`).
                configured_encryption_algorithm: config.encryption.algorithm,
                constraint_registry: Arc::new(crate::core::constraint::ConstraintRegistry::new()),
                schema_constraint_path: if config.persistence.enabled {
                    Some(crate::db::schema_constraint::sidecar_path(&chain_data_dir))
                } else {
                    None
                },
                #[cfg(feature = "audit-export")]
                crypto_shred,
                lineage: Arc::new(crate::core::lineage::LineageStore::new()),
                changefeed: Arc::new(
                    crate::core::changefeed_subscription::ChangefeedBroadcaster::with_config(
                        changefeed_config,
                    ),
                ),
                snapshots,
                namespaces,
                chain: None,
                _tempdir: None,
            };

            // Issue #488: resume an interrupted index key rotation BEFORE
            // loading indexes, so the index directory is a single, uniform key
            // generation by the time it is read back (a crash mid-rotation left
            // a mix of old/new key files plus a `rotation.state` breadcrumb).
            //
            // Issue #3617 PR2 (resume-path double-crash data-loss fix): pass
            // `wal_persist = None` — the startup path DEFERS retiring the
            // old-generation WAL segments (indexes are not loaded yet, so a
            // persist here would snapshot empty state). Retirement + ledger clear
            // happen in `finalize_resumed_wal_rotation` below, AFTER the snapshot
            // is loaded and the WAL is replayed into current state and captured by
            // a synchronous `persist_indexes()`. This pass still installs both WAL
            // generations so the replay read decrypts every segment.
            if let Some(ref manager) = persistence_manager
                && config.encryption.enabled
            {
                crate::db::rotation::resume_pending_rotation(
                    manager,
                    &config.encryption,
                    Some(&db.wal),
                    None,
                )?;
            }

            // Issue #3616 PR3: resume an interrupted plaintext → encrypted enable
            // migration BEFORE loading indexes, so any half-wrapped index/cold
            // bytes are completed before the index directory is read back. This is
            // DISTINCT from `resume_pending_rotation` above: an interrupted enable
            // has NOT yet flipped the encryption.state authority, so
            // `config.encryption.enabled` is still false and that gated path never
            // fires. A guarded no-op when no pending-enable ledger exists.
            db.resume_pending_enable()?;

            // Issue #3616 PR4: resume an interrupted encrypted → plaintext disable
            // migration BEFORE loading indexes, so any half-unwrapped index bytes are
            // completed (and the WAL finished uninstalling + retiring its encrypted
            // tail) before the index directory is read back by the PLAINTEXT manager.
            // Distinct from the rotation/enable paths: a pending disable ledger forced
            // `config.encryption.enabled=false` above, so those gated paths never
            // fire. A guarded no-op when no pending-disable ledger exists.
            db.resume_pending_disable()?;

            // Load indexes on startup if enabled
            if let Some(ref manager) = persistence_manager
                && config.persistence.load_on_startup
            {
                let loaded_lsn =
                    crate::storage::index_persistence::operations::load_indexes_startup(
                        manager,
                        &db.current,
                        &db.historical,
                        &db.node_id_gen,
                        &db.edge_id_gen,
                        &db.version_id_gen,
                    );

                // Issue #3420: the manifest LSN is a second durability floor for
                // the allocator. Normally the segment scan above already seeded
                // the allocator higher, but if the WAL was truncated below the
                // manifest LSN (e.g. LSN-based truncation after cold-storage
                // migration), the segments alone under-seed it. The manifest
                // stores the next-to-allocate LSN captured at snapshot time
                // (see `IndexManifest::lsn`), so it is itself a valid "next".
                if let Some(lsn) = loaded_lsn {
                    let manifest_floor = crate::storage::wal::LSN(lsn);
                    if manifest_floor > db.wal.current_lsn() {
                        db.wal.set_next_lsn(manifest_floor);
                    }
                }

                // Initialize tracker LSNs from the loaded manifest
                if let Some(ref tracker) = persistence_tracker
                    && let Some(lsn) = loaded_lsn
                {
                    tracker.set_start_lsn(lsn);
                    tracker.update_last_persisted_counts(
                        db.current.node_count() as u64,
                        db.current.edge_count() as u64,
                    );
                    // Also initialize string count from current state
                    tracker.update_last_persisted_string_count(
                        crate::core::GLOBAL_INTERNER.len() as u64
                    );
                }

                // Replay constraint declarations from the full WAL history before the
                // differential node/edge replay.  Constraint WAL entries may predate
                // the snapshot LSN and would be skipped by the differential replay.
                //
                // Issue #3429: fold the constraint net-state out of the single
                // full read taken at startup (`startup_wal_entries` spans the
                // whole history from LSN 0), so this no longer re-reads the
                // segment directory. Below-manifest declarations are still
                // applied because the borrowed slice includes them.
                crate::storage::recovery::apply_constraint_declarations(
                    &startup_wal_entries,
                    &db.constraint_registry,
                );

                // Replay WAL entries that occurred after the persisted snapshot
                // This ensures no data loss if the WAL is ahead of the indexes (e.g. crash before persist)
                //
                // Issue #3419: the manifest LSN is the NEXT-to-allocate LSN
                // captured *before* the snapshot was taken (see
                // `IndexManifest::lsn`). Entries with LSN < manifest.lsn are
                // guaranteed to be in the snapshot; entries with LSN >=
                // manifest.lsn may or may not be. Replay therefore starts AT
                // the manifest LSN (inclusive) — the previous `.next()` here
                // skipped the first post-persist write entirely — and the
                // replay itself is idempotent for already-applied entries
                // (see the re-application guards in
                // `replay_wal_into_storage_with_constraints`).
                let start_lsn = match loaded_lsn {
                    Some(lsn) => crate::storage::wal::LSN(lsn),
                    None => {
                        // Safety check: if we have data but no LSN, replaying from initial is dangerous
                        // as it might overwrite existing data with old WAL entries or duplicate IDs.
                        if db.current.node_count() > 0 {
                            #[cfg(feature = "observability")]
                            tracing::error!(
                                "Database contains data ({} nodes) but no persistence LSN found. \
                             Skipping WAL replay to prevent potential data corruption. \
                             This may indicate a missing or corrupted manifest file.",
                                db.current.node_count()
                            );
                            #[cfg(not(feature = "observability"))]
                            eprintln!(
                                "ERROR: Database contains data ({} nodes) but no persistence LSN found. \
                             Skipping WAL replay to prevent potential data corruption.",
                                db.current.node_count()
                            );
                            // Skip replay by setting start_lsn to current WAL LSN (effectively no-op)
                            db.wal.current_lsn()
                        } else {
                            crate::storage::wal::LSN::initial()
                        }
                    }
                };

                // Capture initial version ID before replay
                let initial_version_id = db.version_id_gen.current();

                // Issue #3429: feed the differential replay the suffix of the
                // single startup read (entries with LSN >= start_lsn), rather
                // than re-reading the segment directory a third time.
                //
                // CORRECTNESS: `read_from` returns entries globally sorted by
                // LSN (segment_reader: `entries.sort_by_key(|e| e.lsn)`), so
                // `partition_point` finds the first index with LSN >= start_lsn
                // and `drain(..idx)` discards exactly the entries below it — a
                // contiguous, in-place split with no extra allocation. This is
                // byte-identical to what `read_from(start_lsn)` would have
                // produced (the same LSN-ordered tail the framing resolver
                // expects). If the sort is ever removed upstream, this
                // partition (and the O(1) seed above) breaks — keep them
                // together.
                let split = startup_wal_entries.partition_point(|entry| entry.lsn < start_lsn);
                startup_wal_entries.drain(..split);
                let replay_entries = std::mem::take(&mut startup_wal_entries);

                let mut historical_guard = db.historical.write();
                let (_final_lsn, max_node_id, max_edge_id, next_version_id) =
                    crate::storage::recovery::replay_entries_into_storage_with_constraints(
                        &db.wal,
                        replay_entries,
                        &db.current,
                        &mut historical_guard,
                        initial_version_id,
                        Some(&db.constraint_registry),
                    )?;
                drop(historical_guard);

                // Rebuild reservation index from currently-valid nodes for each declared constraint.
                for (label_str, property_str) in db.constraint_registry.list() {
                    if let (Some(label_id), Some(property_id)) = (
                        crate::core::interning::GLOBAL_INTERNER.get_id(&label_str),
                        crate::core::interning::GLOBAL_INTERNER.get_id(&property_str),
                    ) {
                        let nodes = db.current.get_nodes_by_label(&label_str);
                        db.constraint_registry
                            .rebuild_from_nodes(&nodes, label_id, property_id);
                    }
                }

                // Update ID generators to account for replayed entities.
                // This ensures that subsequent writes use IDs that don't collide with replayed data.
                if let Some(max_nid) = max_node_id {
                    db.node_id_gen.ensure_at_least(max_nid + 1);
                }
                if let Some(max_eid) = max_edge_id {
                    db.edge_id_gen.ensure_at_least(max_eid + 1);
                }
                db.version_id_gen.ensure_at_least(next_version_id);
            }

            // When only the WAL is configured (no index persistence), replay the full
            // WAL from the beginning to restore node/edge data AND constraint declarations.
            if persistence_manager.is_none() {
                let initial_version_id = db.version_id_gen.current();

                // Issue #3429: replay directly from the single startup read
                // instead of re-decoding the segment directory. This branch
                // replays from LSN 0, so it consumes every entry; the inline
                // constraint arms in the replay loop recover constraint state
                // (no separate constraint pass is needed here). `std::mem::take`
                // moves the entries out of the (persistence-branch-shared)
                // binding; only one of the two branches runs.
                let replay_entries = std::mem::take(&mut startup_wal_entries);

                let mut historical_guard = db.historical.write();
                let (_final_lsn, max_node_id, max_edge_id, next_version_id) =
                    crate::storage::recovery::replay_entries_into_storage_with_constraints(
                        &db.wal,
                        replay_entries,
                        &db.current,
                        &mut historical_guard,
                        initial_version_id,
                        Some(&db.constraint_registry),
                    )?;
                drop(historical_guard);

                // Rebuild reservation index.
                for (label_str, property_str) in db.constraint_registry.list() {
                    if let (Some(label_id), Some(property_id)) = (
                        crate::core::interning::GLOBAL_INTERNER.get_id(&label_str),
                        crate::core::interning::GLOBAL_INTERNER.get_id(&property_str),
                    ) {
                        let nodes = db.current.get_nodes_by_label(&label_str);
                        db.constraint_registry
                            .rebuild_from_nodes(&nodes, label_id, property_id);
                    }
                }

                if let Some(max_nid) = max_node_id {
                    db.node_id_gen.ensure_at_least(max_nid + 1);
                }
                if let Some(max_eid) = max_edge_id {
                    db.edge_id_gen.ensure_at_least(max_eid + 1);
                }
                db.version_id_gen.ensure_at_least(next_version_id);
            }

            // Issue #3617 PR2 (resume-path double-crash data-loss fix): PHASE 2 of
            // a startup-resumed WAL key rotation. `resume_pending_rotation` above
            // installed both WAL generations but DEFERRED retiring the
            // old-generation segments (indexes were not loaded yet, so a persist
            // then would have snapshotted empty state). Now that the snapshot is
            // loaded and the WAL has been replayed into current state, retire the
            // old-generation segments — but only AFTER a SYNCHRONOUS
            // `persist_indexes()` captures the replayed `[checkpoint, seal)` writes
            // (run inside `finalize_resumed_wal_rotation`, after the seal and
            // before the retire). So no old-generation WAL segment is deleted until
            // its entries are durable in the index snapshot, closing the window
            // where a second crash before the async checkpoint lost them. A no-op
            // when no WAL rotation is pending. MUST run BEFORE the background
            // persistence thread starts, so the synchronous persist is the one
            // that closes the window (not a racy async checkpoint).
            if let Some(ref manager) = persistence_manager
                && config.encryption.enabled
                && config.persistence.load_on_startup
            {
                crate::db::rotation::finalize_resumed_wal_rotation(
                    manager,
                    &config.encryption,
                    &db.wal,
                    || db.persist_indexes(),
                )?;
            }

            // Start background persistence thread if enabled.
            //
            // Issue #3616 PR3 (NIT-2) load-bearing invariant: on an interrupted-enable
            // resume this worker is spawned HERE, BEFORE `resume_pending_enable_cold`
            // runs the cold wrap (below). That is safe ONLY because both at-rest layers
            // were built under the enable-resume ciphers on this path — the index
            // manager under `enable_resume.index` (so a persist writes `AEIX`, never
            // plaintext over the freshly-wrapped files) and the cold store under
            // `enable_resume.cold` (so a hot→cold migration writes `ACV1`, and the wrap
            // pass is idempotent). A future refactor that built either layer PLAINTEXT
            // on the resume path would reintroduce a plaintext/bare-over-encrypted race
            // with this running worker: the worker must NEVER run with a plaintext
            // manager while an enable ledger is pending. The `index_cipher`
            // (config.rs) and cold `with_cipher_versioned` (below) wiring enforce this.
            if let Some(ref tracker) = persistence_tracker
                && let Some(ref manager) = persistence_manager
            {
                debug_assert!(
                    enable_resume.is_none() || manager.keyring().is_some(),
                    "enable-resume must build the index manager under the enable index \
                     DEK; a plaintext manager with a pending enable ledger would let this \
                     worker persist plaintext over encrypted index files (Issue #3616 PR3)"
                );
                // Issue #3616 PR4: the inverse invariant — a pending disable ledger
                // MUST build the index manager PLAINTEXT (the end state). An encrypted
                // manager with a pending disable ledger would let this worker persist
                // `AEIX` over the freshly-unwrapped plaintext index files. The
                // `disable_resume` force-`enabled=false` wiring above enforces this.
                debug_assert!(
                    disable_resume.is_none() || manager.keyring().is_none(),
                    "disable-resume must build the index manager PLAINTEXT; an encrypted \
                     manager with a pending disable ledger would let this worker persist \
                     encrypted index files over the unwrapped plaintext snapshot (Issue #3616 PR4)"
                );
                let handle = spawn_background_persistence_thread(
                    Arc::clone(&db.current),
                    Arc::clone(&db.historical),
                    Arc::clone(&db.temporal_indexes),
                    Arc::clone(&db.wal),
                    Arc::clone(manager),
                    Arc::clone(tracker),
                    config.persistence.policies.clone(),
                    Arc::clone(&db.persistence_thread_stopped),
                );
                db.persistence_thread_handle = Some(handle);
            }

            wire_temporal_indexes(&db);
            reconcile_namespace_registry(&db);

            // Initialize cold storage if enabled
            if enable_cold_storage && let Some(cold_storage_path) = cold_storage_path {
                // Create Redb cold storage backend, with optional encryption cipher
                let mut cold_storage = RedbColdStorage::new(&cold_storage_path, RedbConfig::new())?;

                if let Some(ref enc_mgr) = encryption_manager {
                    // Issue #3617 PR3 version-provisioning: build the cold keyring
                    // at the provisioned key version (the max on-disk key version,
                    // or `ledger.new_version - 1` mid-rotation) so freshly written
                    // cold values stamp the real version and stay in lockstep with
                    // index/WAL after a rotate-then-reopen. Reads stay `match_any`
                    // (identical to `with_cipher`); `None` falls back to
                    // `with_cipher` (unchanged behavior for a never-rotated DB).
                    cold_storage = match provisioned_key_version {
                        Some(v) => {
                            cold_storage.with_cipher_versioned(Arc::clone(enc_mgr.cold_cipher()), v)
                        }
                        None => cold_storage.with_cipher(Arc::clone(enc_mgr.cold_cipher())),
                    };
                } else if let Some(ref e) = enable_resume {
                    // Issue #3616 PR3: on an interrupted-enable resume the authority
                    // is not yet flipped (no encryption manager), but the cold store
                    // must be built under the enable cold DEK so `ACV1` values
                    // written by the resume wrap pass (below) are read back this same
                    // open(). Provisioned at the enable version.
                    cold_storage = cold_storage.with_cipher_versioned(
                        Arc::clone(&e.cold),
                        crate::db::rotation::ENABLE_KEY_VERSION,
                    );
                } else if let Some(ref d) = disable_resume {
                    // Issue #3616 PR4: on an interrupted-disable resume the authority
                    // is not yet flipped (no encryption manager), but the cold store
                    // must be built under the DECRYPT cold DEK — at the recorded
                    // (to-be-retired) version — so `resume_pending_disable_cold`'s
                    // `ACV1` → bare unwrap pass (below) can decrypt each wrapped value.
                    cold_storage =
                        cold_storage.with_cipher_versioned(Arc::clone(&d.cold), d.version);
                }

                let cold_storage = Arc::new(cold_storage);

                // Create tiered storage with warm cache configuration
                let tiered_config = TieredStorageConfig::default();
                let tiered_storage = TieredStorage::new(tiered_config, cold_storage);

                // Issue #3617 PR3: finish a startup-resumed COLD key rotation. The
                // cold store is built here — AFTER index/WAL resume — so it is the
                // last layer to settle; this completes the bulk re-encrypt from the
                // durable redb cursor (idempotent, skips already-wrapped values),
                // retires the old cold generation, and clears the rotation ledger.
                // A no-op when no rotation is pending or cold is not in scope.
                if let Some(ref manager) = persistence_manager
                    && config.encryption.enabled
                {
                    crate::db::rotation::finalize_resumed_cold_rotation(
                        manager,
                        &config.encryption,
                        tiered_storage.cold_storage(),
                    )?;
                }

                // Merge the cold tier's persisted extent bounds into the
                // temporal index so `temporal_extent` spans history migrated to
                // cold before this restart (Issue #3389). `wire_temporal_indexes`
                // above rebuilt the extent aggregate from the hot tier only;
                // absent/empty cold metadata leaves it untouched, and merging
                // only ever widens (never narrows) the reported extent.
                if let Some(bounds) = tiered_storage.cold_storage().get_temporal_extent_bounds()? {
                    db.temporal_indexes.merge_extent_bounds(
                        bounds.valid_earliest,
                        bounds.valid_latest,
                        bounds.tx_earliest,
                        bounds.tx_latest,
                    );
                }

                // Wire tiered storage to historical storage
                // Note: migration_age_threshold and max_hot_versions from config.historical
                // are used by HistoricalStorage's migration logic, not by TieredStorage
                db.historical
                    .write()
                    .set_tiered_storage(Arc::new(tiered_storage));

                // Issue #3616 PR3: finish an interrupted plaintext → encrypted
                // enable migration's COLD layer, now that the cold store is wired.
                // `resume_pending_enable` (run before index load) deferred the cold
                // wrap + the authority flip + the ledger clear to here — this wraps
                // the bare cold values to `ACV1`, then (all layers settled) flips
                // the authority and clears the ledger, preserving the binding order.
                // A guarded no-op when no enable migration with a cold layer is
                // pending.
                db.resume_pending_enable_cold()?;

                // Issue #3616 PR4: finish an interrupted encrypted → plaintext
                // disable migration's COLD layer, now that the cold store is wired
                // under the decrypt cold DEK. `resume_pending_disable` (run before
                // index load) deferred the cold unwrap + the authority flip + the
                // ledger clear to here — this unwraps each `ACV1` value back to bare,
                // then (all layers settled) flips the authority to `disabled` and
                // clears the ledger, preserving the binding order. A guarded no-op
                // when no disable migration with a cold layer is pending.
                db.resume_pending_disable_cold()?;
            } else {
                // Issue #3616 PR3: this open() did NOT build a cold tier
                // (enable_cold_storage disabled or no cold path). If an interrupted
                // enable recorded a cold layer as Pending, `resume_pending_enable_cold`
                // (with its fail-closed guard) never runs — so the ledger would wedge
                // forever (authority never flips, no diagnostic). Fail LOUDLY here
                // with the same actionable "reopen with the cold tier enabled" error
                // instead of silently stranding the migration.
                db.fail_if_pending_enable_cold_without_tier()?;
                // Issue #3616 PR4: the same guard for an interrupted disable whose
                // ledger records a cold layer but this open() built no cold tier.
                db.fail_if_pending_disable_cold_without_tier()?;
            }

            // Crypto-shred subject-keyring re-wrap (Slice 5): the LAST layer of a
            // full-MEK rotation to settle. Runs AFTER the index/WAL/cold resume
            // passes above, unconditionally (whether or not a cold tier was built),
            // so a rotation that crashed after the ledger recorded
            // `layer.subject_keyring=pending` finishes here — re-wrapping every
            // per-subject DEK under the new MEK and clearing the ledger. Idempotent
            // and a guarded no-op when the layer is not in flight; requires the OLD
            // MEK to still be the configured key source (reopen-under-old-key
            // contract). Gated on `audit-export` (the crypto-shred feature).
            #[cfg(feature = "audit-export")]
            if let Some(manager) = persistence_manager
                .as_ref()
                .filter(|_| config.encryption.enabled)
            {
                crate::db::rotation::finalize_resumed_subject_keyring_rotation(
                    manager,
                    &config.encryption,
                    db.crypto_shred.as_ref(),
                )?;
            }

            // Fail-closed counterpart (Slice 5 review 4b): when THIS binary was
            // built WITHOUT the crypto-shred (`audit-export`) feature there is no
            // finalizer for a subject-keyring re-wrap left `Pending` by a crash.
            // Rather than wedge the rotation ledger `Pending` forever with no
            // diagnostic, surface an actionable `FailedPrecondition` telling the
            // operator to reopen with the crypto-shred feature enabled. A guarded
            // no-op when no such pending re-wrap exists.
            #[cfg(not(feature = "audit-export"))]
            if let Some(manager) = persistence_manager
                .as_ref()
                .filter(|_| config.encryption.enabled)
            {
                crate::db::rotation::fail_if_pending_subject_keyring_without_crypto_shred(manager)?;
            }

            seed_startup_current_timestamp(&db)?;

            // Wire the opt-in provenance hash chain (Issue #3351). Constructed
            // after WAL replay + state restore so the genesis anchors the
            // post-recovery LSN and the tail rebuild can fold every restored
            // transaction. Nothing here runs (and no chain dir is created) when
            // the chain is disabled — the default — preserving byte-identical
            // behavior.
            if chain_config.enabled {
                let source: Arc<dyn crate::provenance_chain::VersionSource + Send + Sync> =
                    Arc::new(crate::db::chain_source::DbVersionSource::new(Arc::clone(
                        &db.historical,
                    )));
                let genesis_lsn = db.wal.current_lsn().0;
                let genesis_ts = time::now().wallclock();
                let chain = crate::provenance_chain::ProvenanceChain::open(
                    &chain_config,
                    &chain_data_dir,
                    genesis_lsn,
                    genesis_ts,
                    source,
                )
                .map_err(|e| {
                    crate::core::error::Error::Other(format!(
                        "provenance hash chain open failed: {e}"
                    ))
                })?;
                // Rebuild the unsealed tail from replayed history, then start the
                // background sealer for live commits.
                db.rebuild_chain_tail(&chain);
                chain.start();
                db.chain = Some(chain);
            }

            // Schema constraints (Issue #3378): load the durable sidecar LAST —
            // after WAL replay + index load have fully restored the string
            // interner — so the label/property strings re-intern to the same
            // ids that live writes use (loading earlier would key constraints by
            // pre-restore interner ids that no longer match). Tolerant load
            // (missing = empty; corrupt = warn + quarantine + empty) so a bad
            // sidecar never bricks startup. Ephemeral databases have no path.
            if let Some(ref path) = db.schema_constraint_path {
                crate::db::schema_constraint::load_sidecar(path, &db.constraint_registry);
            }

            Ok(db)
        })();
        result.record_error_metric()
    }

    /// Create a new database with both anchor and WAL configuration.
    ///
    /// This maintains backward compatibility with the old API.
    /// For new code, prefer using [`with_unified_config`](Self::with_unified_config).
    ///
    /// # Errors
    ///
    /// Returns an error if WAL initialization fails (e.g., cannot create WAL directory).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn with_full_config(
        anchor_config: AnchorConfig,
        wal_config: crate::config::WalConfig,
    ) -> Result<Self> {
        let result = (|| {
            let durability_mode = wal_config.durability_mode;

            let wal_system_config = ConcurrentWalSystemConfig {
                wal_dir: wal_config.wal_dir,
                num_stripes: wal_config.num_stripes,
                stripe_capacity: wal_config.stripe_capacity,
                segment_size: wal_config.segment_size,
                segments_to_retain: wal_config.segments_to_retain,
                flush_interval_ms: flush_interval_from_durability(
                    durability_mode,
                    wal_config.flush_interval_ms,
                ),
                durability_mode,
                write_buffer_size: wal_config.write_buffer_size,
                wal_cipher: None,
                wal_key_version: None,
                tolerate_torn_tail: wal_config.tolerate_torn_tail,
            };

            let wal = Arc::new(ConcurrentWalSystem::new(wal_system_config)?);

            // Issue #3420: seed the LSN allocator from existing WAL segments
            // before any write is accepted (see with_unified_config for details).
            // This construction path never configures a WAL cipher, matching
            // its (pre-existing) cipher-less read path.
            seed_lsn_allocator_from_segments(&wal, None)?;

            let db = AletheiaDB {
                current: Arc::new(CurrentStorage::new()),
                historical: Arc::new(RwLock::new(HistoricalStorage::with_config(anchor_config))),
                temporal_indexes: Arc::new(TemporalIndexes::new()),
                wal,
                in_flight: Arc::new(crate::api::transaction::write::InFlightLsns::new()),
                current_timestamp: Arc::new(Mutex::new(time::now())),
                commit_clock_observed_at: Arc::new(Mutex::new(Instant::now())),
                tx_id_gen: Arc::new(TxIdGenerator::new()),
                visibility_manager: Arc::new(TxVisibilityManager::new()),
                node_id_gen: Arc::new(IdGenerator::new()),
                edge_id_gen: Arc::new(IdGenerator::new()),
                version_id_gen: Arc::new(IdGenerator::new()),
                default_durability: durability_mode,
                stats: Arc::new(Statistics::new()),
                persistence_config: crate::storage::index_persistence::PersistenceConfig::default(),
                persistence_manager: None,
                persistence_tracker: None,
                persistence_thread_stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                persistence_thread_handle: None,
                encryption_manager: None,
                encryption_config: None,
                // Ephemeral (in-memory) DB: no encryption config and cannot be
                // enabled; default algorithm (Issue #3616 PR3).
                configured_encryption_algorithm: crate::encryption::factory::Algorithm::default(),
                constraint_registry: Arc::new(crate::core::constraint::ConstraintRegistry::new()),
                // `with_full_config` has no index-persistence data dir; schema
                // constraints are in-memory only (ephemeral `new()` path).
                schema_constraint_path: None,
                // Ephemeral: crypto-shred keyring is in-memory only (no path).
                #[cfg(feature = "audit-export")]
                crypto_shred: Arc::new(crate::db::crypto_shred::CryptoShredState::open(None)?),
                lineage: Arc::new(crate::core::lineage::LineageStore::new()),
                changefeed: Arc::new(
                    crate::core::changefeed_subscription::ChangefeedBroadcaster::new(),
                ),
                // Ephemeral (no index persistence): in-memory-only registry.
                snapshots: Arc::new(crate::db::snapshot::SnapshotRegistry::in_memory()),
                // Ephemeral namespace registry (Issue #3349): in-memory only.
                namespaces: Arc::new(crate::db::namespace::NamespaceRegistry::in_memory()),
                chain: None,
                _tempdir: None,
            };
            seed_startup_current_timestamp(&db)?;
            wire_temporal_indexes(&db);
            reconcile_namespace_registry(&db);

            Ok(db)
        })();
        result.record_error_metric()
    }

    /// Get the default durability mode for this database.
    pub fn default_durability(&self) -> DurabilityMode {
        self.default_durability
    }

    /// Returns `true` if encryption at rest is enabled.
    pub fn is_encryption_enabled(&self) -> bool {
        self.encryption_manager.is_some()
    }

    /// Get a reference to the encryption manager, if encryption is enabled.
    pub fn encryption_manager(&self) -> Option<&Arc<crate::encryption::EncryptionManager>> {
        self.encryption_manager.as_ref()
    }
}

#[cfg(test)]
mod ephemeral_tests {
    use super::*;

    /// Configurable interner cap bounds (finding 2): `0` is rejected (it would
    /// brick all writes/open), and any value at/above `u32::MAX` is clamped to
    /// `u32::MAX` (ids are `AtomicU32`, so a higher cap is unreachable). Pure,
    /// deterministic — does NOT touch the process-global interner, so it cannot
    /// race concurrent tests.
    #[test]
    fn validate_interner_cap_rejects_zero_and_clamps_above_u32_max() {
        // Zero is refused with a clear, actionable error.
        let err = validate_interner_cap(0).expect_err("cap of 0 must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("max_interned_strings"),
            "error must name the config field, got: {msg}"
        );

        // Ordinary values pass through unchanged.
        assert_eq!(validate_interner_cap(1).unwrap(), 1);
        assert_eq!(validate_interner_cap(10_000_000).unwrap(), 10_000_000);
        assert_eq!(
            validate_interner_cap(u32::MAX as usize).unwrap(),
            u32::MAX as usize
        );

        // Anything above u32::MAX is clamped down (unreachable otherwise).
        assert_eq!(
            validate_interner_cap(u32::MAX as usize + 1).unwrap(),
            u32::MAX as usize
        );
        assert_eq!(
            validate_interner_cap(usize::MAX).unwrap(),
            u32::MAX as usize
        );
    }

    /// `with_unified_config` rejects a zero interner cap up front (before it
    /// touches the process-global interner) rather than bricking the DB. This is
    /// race-free: the error is returned before any `set_max_capacity`.
    #[test]
    fn with_unified_config_rejects_zero_interner_cap() {
        use crate::config::AletheiaDBConfig;
        use crate::storage::index_persistence::PersistenceConfig;

        let config = AletheiaDBConfig::builder()
            .persistence(PersistenceConfig {
                max_interned_strings: 0,
                ..Default::default()
            })
            .build();
        let err = match AletheiaDB::with_unified_config(config) {
            Ok(_) => panic!("a zero interner cap must fail open(), not brick the DB"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("max_interned_strings"),
            "error must name the config field, got: {err}"
        );
    }

    /// Precedence (finding 10): on the `open()`/`with_unified_config` path the
    /// config field authoritatively sets the interner cap — it OVERRIDES the
    /// `ALETHEIADB_MAX_INTERNED_STRINGS` env seed (which only ever seeds the
    /// `GLOBAL_INTERNER` LazyLock for embedded users who never open a DB).
    ///
    /// Runs in a SUBPROCESS with the env var injected by the parent, so the seed
    /// is present from process start and the process-global mutation is isolated.
    #[test]
    fn config_interner_cap_overrides_env_on_open_via_subprocess() {
        use std::process::Command;
        use std::time::{Duration, Instant};

        let exe = std::env::current_exe().expect("failed to locate current test binary");
        let mut child = Command::new(exe)
            .env("ALETHEIADB_MAX_INTERNED_STRINGS", "12345")
            .args([
                "--ignored",
                "--exact",
                "db::config::ephemeral_tests::config_interner_cap_overrides_env_helper",
            ])
            .spawn()
            .expect("failed to spawn subprocess for config-vs-env precedence test");

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(status.success(), "config-vs-env precedence helper failed");
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("config-vs-env precedence helper did not complete");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("failed while polling subprocess: {e}"),
            }
        }
    }

    #[test]
    #[ignore]
    fn config_interner_cap_overrides_env_helper() {
        use crate::config::{AletheiaDBConfig, WalConfigBuilder};
        use crate::storage::index_persistence::PersistenceConfig;
        use tempfile::tempdir;

        // First access initializes the LazyLock interner from the injected env
        // seed (12345). This is the embedded/`new()`-only precedence.
        assert_eq!(
            crate::core::GLOBAL_INTERNER.max_capacity(),
            12345,
            "the env var must seed the interner before any DB is opened"
        );

        // Open a DB with an explicit, DIFFERENT config cap: the config field wins.
        // Use an isolated tempdir WAL so the fully-default WAL dir (a shared,
        // cwd-relative path) can't collide with stale data from another run.
        let scratch = tempdir().unwrap();
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(scratch.path().join("wal"))
                    .build(),
            )
            .persistence(PersistenceConfig {
                max_interned_strings: 777_000,
                ..Default::default()
            })
            .build();
        let _db = AletheiaDB::with_unified_config(config).expect("open must succeed");
        assert_eq!(
            crate::core::GLOBAL_INTERNER.max_capacity(),
            777_000,
            "the config field must override the env seed on the open() path"
        );
    }

    #[test]
    fn new_uses_a_unique_tempdir_per_call() {
        let db1 = AletheiaDB::new().expect("new should succeed");
        let db2 = AletheiaDB::new().expect("new should succeed");
        let dir1 = db1
            ._tempdir
            .as_ref()
            .expect("new should attach a tempdir")
            .path()
            .to_path_buf();
        let dir2 = db2
            ._tempdir
            .as_ref()
            .expect("new should attach a tempdir")
            .path()
            .to_path_buf();
        assert_ne!(dir1, dir2, "each new() must own a distinct tempdir");
        assert!(
            dir1.exists(),
            "tempdir must exist while the database is alive"
        );
        assert!(dir2.exists());
    }

    #[test]
    fn new_does_not_write_into_cwd() {
        let cwd = std::env::current_dir().expect("cwd");
        let db = AletheiaDB::new().expect("new should succeed");
        let wal_dir = db._tempdir.as_ref().expect("tempdir attached").path();
        assert!(
            !wal_dir.starts_with(&cwd) || wal_dir.starts_with(std::env::temp_dir()),
            "tempdir should live under the system temp root, not the cwd"
        );
    }

    #[test]
    fn tempdir_is_removed_when_db_drops() {
        let path = {
            let db = AletheiaDB::new().expect("new should succeed");
            db._tempdir.as_ref().unwrap().path().to_path_buf()
        };
        assert!(!path.exists(), "tempdir should be cleaned up on drop");
    }

    #[test]
    #[serial_test::serial]
    fn open_from_env_with_unset_var_is_ephemeral() {
        // SAFETY: env access is single-threaded inside this serial test.
        // remove_var is unsafe under Rust edition 2024.
        unsafe {
            std::env::remove_var(crate::config::DATA_DIR_ENV);
            std::env::remove_var(crate::config::CONFIG_ENV);
        }
        let db = AletheiaDB::open_from_env().expect("open_from_env should fall back to new()");
        assert!(
            db._tempdir.is_some(),
            "unset env must yield a tempdir-backed db"
        );
    }

    #[cfg(feature = "config-toml")]
    #[test]
    #[serial_test::serial]
    fn open_from_env_prefers_config_over_data_dir() {
        let scratch = tempfile::tempdir().expect("scratch tempdir");
        let config_path = scratch.path().join("config.toml");
        let toml_data_dir = scratch.path().join("toml-data");
        let env_data_dir = scratch.path().join("env-data");

        // Minimal valid TOML pointing the WAL inside `toml-data/wal`.
        // Use a TOML literal string (single quotes) so Windows backslashes
        // in the path don't trigger basic-string escape processing.
        std::fs::write(
            &config_path,
            format!(
                "[wal]\nwal_dir = '{}'\n",
                toml_data_dir.join("wal").display()
            ),
        )
        .unwrap();

        // SAFETY: serial test, single-threaded env access; required by edition 2024.
        unsafe {
            std::env::set_var(crate::config::CONFIG_ENV, &config_path);
            std::env::set_var(crate::config::DATA_DIR_ENV, &env_data_dir);
        }

        let db = AletheiaDB::open_from_env().expect("config TOML should load");
        assert!(
            db._tempdir.is_none(),
            "TOML-backed db is durable, not tempdir"
        );
        assert!(
            toml_data_dir.join("wal").exists(),
            "TOML's wal_dir must win over DATA_DIR"
        );
        assert!(
            !env_data_dir.exists(),
            "DATA_DIR should be ignored when CONFIG is set"
        );

        // SAFETY: see above.
        unsafe {
            std::env::remove_var(crate::config::CONFIG_ENV);
            std::env::remove_var(crate::config::DATA_DIR_ENV);
        }
    }

    /// Covers config.rs line 435: the WAL+persistence incremental-replay path that
    /// runs `db.edge_id_gen.ensure_at_least(max_eid + 1)` when the incremental WAL
    /// (between the last snapshot LSN and the current WAL head) contains an edge.
    ///
    /// Achieving this requires a snapshot taken *before* the edge is written, while
    /// ensuring the normal background thread does not re-snapshot on shutdown (which
    /// would swallow the edge into the snapshot and leave the incremental WAL empty).
    /// We do this by manually calling `persist_all_indexes`, then stopping the
    /// background thread before creating the edge.
    #[test]
    fn wal_persistence_incremental_wal_edge_covers_edge_id_gen() {
        use crate::config::{AletheiaDBConfig, WalConfigBuilder};
        use crate::storage::index_persistence::PersistenceConfig;
        use crate::storage::index_persistence::operations::persist_all_indexes;
        use crate::storage::wal::DurabilityMode;
        use crate::{PropertyMapBuilder, WriteOps};
        use tempfile::tempdir;

        let scratch = tempdir().unwrap();
        let db_path = scratch.path().to_path_buf();

        let make_config = || {
            AletheiaDBConfig::builder()
                .wal(
                    WalConfigBuilder::new()
                        .wal_dir(db_path.join("wal"))
                        .durability_mode(DurabilityMode::Synchronous)
                        .build(),
                )
                .persistence(PersistenceConfig {
                    enabled: true,
                    data_dir: db_path.join("indexes"),
                    load_on_startup: true,
                    ..Default::default()
                })
                .build()
        };

        let (n1, n2) = {
            let mut db = AletheiaDB::with_unified_config(make_config()).unwrap();
            db.unique_constraint("P", "k").enable().unwrap();
            let n1 = db
                .create_node("P", PropertyMapBuilder::new().insert("k", "a").build())
                .unwrap();
            let n2 = db
                .create_node("P", PropertyMapBuilder::new().insert("k", "b").build())
                .unwrap();

            // Snapshot now so startup sees safe_lsn = current WAL head.
            // Use .expect() rather than if-let so LLVM does not generate uncovered
            // else-branches that inflate the Codecov patch-missing count.
            let tracker = db
                .persistence_tracker
                .as_ref()
                .expect("persistence configured");
            let manager = db
                .persistence_manager
                .as_ref()
                .expect("persistence configured");
            let _ = persist_all_indexes(
                &db.current,
                &db.historical,
                &db.temporal_indexes,
                &db.wal,
                manager,
                tracker,
            );

            // Stop the background thread before it can re-snapshot on its own.
            tracker.signal_shutdown();
            let handle = db
                .persistence_thread_handle
                .take()
                .expect("background thread running");
            let _ = handle.join();

            // persist_all_indexes records safe_lsn = wal.current_lsn(), the
            // NEXT-to-allocate LSN (call it L).  The edge below receives LSN=L
            // — the exact Issue #3419 boundary.  Startup replays from the
            // manifest LSN INCLUSIVE, so the very first post-persist write is
            // recovered without burning a throwaway LSN (the old workaround
            // that this test now guards against regressing).
            db.write(|tx| tx.create_edge(n1, n2, "R", PropertyMapBuilder::new().build()))
                .unwrap();
            (n1, n2)
        };
        let _ = (n1, n2);

        // Session 2: startup loads snapshot (n1+n2 only, no edge) then replays
        // incremental WAL from LSN L INCLUSIVE (the edge, Issue #3419) →
        // max_edge_id = Some(_) → the edge_id_gen bump fires.
        {
            let db = AletheiaDB::with_unified_config(make_config()).unwrap();
            assert_eq!(
                db.edge_count(),
                1,
                "edge must be recovered from incremental WAL"
            );
            db.create_node("P", PropertyMapBuilder::new().insert("k", "a").build())
                .expect_err("constraint must survive WAL+persistence restart");
        }
    }

    /// Prod-upgrade regression (configurable interner cap): a durable database
    /// opened under a LOW `max_interned_strings`, populated, and then REOPENED
    /// under a HIGHER cap must (a) load its existing data cleanly FROM DISK, (b)
    /// intern past the old cap, and (c) hit no migration/format error.
    ///
    /// The string interner is a process-global singleton and this test lowers
    /// its cap, so it runs in a SUBPROCESS to isolate the mutation from other
    /// tests running concurrently (mirrors the interning.rs mutant-kill pattern).
    ///
    /// Cold-load fidelity (finding 7): sessions 1 and 2 share one process, so the
    /// helper CLEARS + re-warms the process-global interner between them —
    /// reproducing a fresh-process cold start where the interner holds only the
    /// warmed common strings and every other string MUST be repopulated from the
    /// on-disk interner file. Without the clear, session 1's residual in-memory
    /// strings would mask a broken disk→interner load. The helper asserts the
    /// interner is genuinely empty of session-1 data BEFORE reopen and repopulated
    /// FROM DISK after, and crosses ~1000 strings so the load path is real.
    #[test]
    fn interner_cap_reopen_with_higher_cap_via_subprocess() {
        use std::process::Command;
        use std::time::{Duration, Instant};

        let exe = std::env::current_exe().expect("failed to locate current test binary");
        let mut child = Command::new(exe)
            .args([
                "--ignored",
                "--exact",
                "db::config::ephemeral_tests::interner_cap_reopen_with_higher_cap_helper",
            ])
            .spawn()
            .expect("failed to spawn subprocess for interner-cap reopen test");

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(status.success(), "interner-cap reopen helper failed");
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("interner-cap reopen helper did not complete");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("failed while polling subprocess: {e}"),
            }
        }
    }

    #[test]
    #[ignore]
    fn interner_cap_reopen_with_higher_cap_helper() {
        use crate::PropertyMapBuilder;
        use crate::config::{AletheiaDBConfig, WalConfigBuilder};
        use crate::storage::index_persistence::PersistenceConfig;
        use crate::storage::wal::DurabilityMode;
        use tempfile::tempdir;

        let scratch = tempdir().unwrap();
        let db_path = scratch.path().to_path_buf();

        let make_config = |cap: usize| {
            AletheiaDBConfig::builder()
                .wal(
                    WalConfigBuilder::new()
                        .wal_dir(db_path.join("wal"))
                        .durability_mode(DurabilityMode::Synchronous)
                        .build(),
                )
                .persistence(PersistenceConfig {
                    enabled: true,
                    data_dir: db_path.join("indexes"),
                    load_on_startup: true,
                    max_interned_strings: cap,
                    ..Default::default()
                })
                .build()
        };

        // Session 1: LOW cap (3000). Intern ~1000 distinct labels + ~1000 distinct
        // property VALUES (interned at persist time) — a real load-path workload,
        // still comfortably under the cap — then persist and close.
        const N: usize = 1000;
        let probe_label = "LowCapLabel_500";
        let created = {
            let db = AletheiaDB::with_unified_config(make_config(3000)).unwrap();
            assert_eq!(crate::core::GLOBAL_INTERNER.max_capacity(), 3000);

            let mut ids = Vec::new();
            for i in 0..N {
                let label = format!("LowCapLabel_{i}");
                let id = db
                    .create_node(
                        &label,
                        PropertyMapBuilder::new()
                            .insert("k", format!("v{i}"))
                            .build(),
                    )
                    .unwrap();
                ids.push(id);
            }
            db.persist_indexes().unwrap();
            ids.len()
        };
        assert_eq!(created, N);

        // Between sessions: reproduce a FRESH-PROCESS cold start. Dropping the
        // session-1 db does not clear the process-global interner, so clear it and
        // re-warm only the common strings — exactly the state a brand-new process's
        // LazyLock interner would hold. Now session-1's labels/values live ONLY on
        // disk; if the disk→interner load is broken, session 2 cannot resolve them.
        crate::core::GLOBAL_INTERNER.clear();
        crate::core::GLOBAL_INTERNER.warm_common_strings();
        assert!(
            crate::core::GLOBAL_INTERNER.get_id(probe_label).is_none(),
            "precondition: after clear+warm the interner must NOT hold session-1 data \
             (proving the reopen genuinely cold-loads from disk)"
        );

        // Session 2: HIGHER cap (10_000). Existing data must load clean FROM DISK
        // and interning must proceed past the old cap without any format error.
        {
            let db = AletheiaDB::with_unified_config(make_config(10_000)).unwrap();
            assert_eq!(crate::core::GLOBAL_INTERNER.max_capacity(), 10_000);
            assert_eq!(
                db.node_count(),
                created,
                "existing nodes must reload cleanly from disk after cap raise"
            );
            // The disk interner file repopulated the (previously cleared) interner:
            // a session-1 label we did NOT recreate now resolves again.
            assert!(
                crate::core::GLOBAL_INTERNER.get_id(probe_label).is_some(),
                "the on-disk interner file must repopulate the cold interner on reopen"
            );

            // Intern well past the old cap: 500 more distinct labels succeed.
            for i in 0..500 {
                let label = format!("HighCapLabel_{i}");
                db.create_node(
                    &label,
                    PropertyMapBuilder::new()
                        .insert("k", format!("w{i}"))
                        .build(),
                )
                .expect("interning must proceed past the old cap under the raised limit");
            }
            assert_eq!(db.node_count(), created + 500);
        }
    }

    /// Pure-decision coverage for the process-global interner-cap policy
    /// (Issue #3724). Exhaustively pins every arm of `decide_interner_cap` without
    /// touching the process-global interner, so these run in-process safely.
    #[test]
    fn decide_interner_cap_covers_all_arms() {
        // First open in the process establishes the baseline, even LOWER than the
        // (default 10M) seed — preserves the single-DB "low DoS bound" case.
        assert_eq!(
            decide_interner_cap(false, 10_000_000, 3_000),
            InternerCapDecision::Established(3_000),
        );
        assert_eq!(
            decide_interner_cap(false, 10_000_000, 50_000_000),
            InternerCapDecision::Established(50_000_000),
        );

        // A later open requesting MORE headroom raises the shared cap.
        assert_eq!(
            decide_interner_cap(true, 5_000, 8_000),
            InternerCapDecision::Raised(8_000),
        );

        // A later open requesting a LOWER cap keeps the higher effective cap (the
        // footgun fix) — this is the arm that emits the loud warning.
        assert_eq!(
            decide_interner_cap(true, 5_000, 2_000),
            InternerCapDecision::KeptHigher {
                requested: 2_000,
                effective: 5_000,
            },
        );

        // A later open requesting exactly the effective cap changes nothing.
        assert_eq!(
            decide_interner_cap(true, 5_000, 5_000),
            InternerCapDecision::Unchanged(5_000),
        );
    }

    /// Multi-DB-in-one-process regression (Issue #3724): opening a SECOND database
    /// with a LOWER `max_interned_strings` must NOT lower the process-global
    /// interner cap and break writes on the already-open first database. Under the
    /// pre-fix last-open-wins behavior the second open would drop the shared cap to
    /// its lower value; the max-of-all-opens high-water mark keeps the higher cap.
    ///
    /// The string interner is a process-global singleton and this test drives its
    /// cap through multiple opens, so it runs in a SUBPROCESS to isolate the
    /// mutation from other tests running concurrently (mirrors the interning.rs
    /// mutant-kill / reopen-cap subprocess pattern).
    #[test]
    fn interner_cap_multidb_high_water_mark_via_subprocess() {
        use std::process::Command;
        use std::time::{Duration, Instant};

        let exe = std::env::current_exe().expect("failed to locate current test binary");
        let mut child = Command::new(exe)
            .args([
                "--ignored",
                "--exact",
                "db::config::ephemeral_tests::interner_cap_multidb_high_water_mark_helper",
            ])
            .spawn()
            .expect("failed to spawn subprocess for multi-DB interner-cap test");

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(status.success(), "multi-DB interner-cap helper failed");
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("multi-DB interner-cap helper did not complete");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("failed while polling subprocess: {e}"),
            }
        }
    }

    #[test]
    #[ignore]
    fn interner_cap_multidb_high_water_mark_helper() {
        use crate::config::{AletheiaDBConfig, WalConfigBuilder};
        use crate::storage::index_persistence::PersistenceConfig;
        use tempfile::tempdir;

        // Each database gets its own isolated tempdir WAL so the fully-default WAL
        // dir can't collide across the four opens in this one process.
        let make_config = |cap: usize| {
            let scratch = tempdir().expect("scratch tempdir");
            let dir = scratch.path().to_path_buf();
            let config = AletheiaDBConfig::builder()
                .wal(WalConfigBuilder::new().wal_dir(dir.join("wal")).build())
                .persistence(PersistenceConfig {
                    max_interned_strings: cap,
                    ..Default::default()
                })
                .build();
            // Return the tempdir guard too so it outlives the open.
            (config, scratch)
        };

        let interner = &*crate::core::GLOBAL_INTERNER;

        // DB-A: the FIRST open establishes the baseline at exactly its cap (5000),
        // regardless of the 10M LazyLock seed.
        let (cfg_a, _g_a) = make_config(5_000);
        let _db_a = AletheiaDB::with_unified_config(cfg_a).expect("open A");
        assert_eq!(
            interner.max_capacity(),
            5_000,
            "the first open must establish the baseline cap exactly"
        );

        // DB-B: a SECOND open with a LOWER cap (2000) must NOT lower the shared cap.
        // Pre-fix (last-open-wins) this assertion fails — the cap would drop to 2000
        // and DB-A could start refusing writes.
        let (cfg_b, _g_b) = make_config(2_000);
        let _db_b = AletheiaDB::with_unified_config(cfg_b).expect("open B");
        assert_eq!(
            interner.max_capacity(),
            5_000,
            "a later, LOWER cap must NOT shrink the process-global interner cap \
             (Issue #3724): DB-A's write headroom must be preserved"
        );

        // DB-C: a later open with a HIGHER cap (8000) legitimately raises the cap.
        let (cfg_c, _g_c) = make_config(8_000);
        let _db_c = AletheiaDB::with_unified_config(cfg_c).expect("open C");
        assert_eq!(
            interner.max_capacity(),
            8_000,
            "a later, HIGHER cap must raise the shared cap (add headroom)"
        );

        // DB-D: a later open with the SAME cap as the current effective one is a
        // no-op and must not perturb the cap.
        let (cfg_d, _g_d) = make_config(8_000);
        let _db_d = AletheiaDB::with_unified_config(cfg_d).expect("open D");
        assert_eq!(
            interner.max_capacity(),
            8_000,
            "an equal later cap must leave the shared cap unchanged"
        );
    }

    /// Concurrent-opens regression (Issue #3724): many databases opened
    /// simultaneously, each with a different `max_interned_strings`, must
    /// deterministically converge the process-global cap to the MAX of all
    /// requested caps. This exercises `INTERNER_CAP_LOCK` serializing the
    /// establish/raise read-modify-write across the two atomics; without the lock
    /// a lost update could leave the effective cap below the true maximum.
    ///
    /// Process-global mutation → run in a SUBPROCESS for isolation.
    #[test]
    fn interner_cap_concurrent_opens_converge_to_max_via_subprocess() {
        use std::process::Command;
        use std::time::{Duration, Instant};

        let exe = std::env::current_exe().expect("failed to locate current test binary");
        let mut child = Command::new(exe)
            .args([
                "--ignored",
                "--exact",
                "db::config::ephemeral_tests::interner_cap_concurrent_opens_converge_to_max_helper",
            ])
            .spawn()
            .expect("failed to spawn subprocess for concurrent-opens interner-cap test");

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(
                        status.success(),
                        "concurrent-opens interner-cap helper failed"
                    );
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("concurrent-opens interner-cap helper did not complete");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("failed while polling subprocess: {e}"),
            }
        }
    }

    #[test]
    #[ignore]
    fn interner_cap_concurrent_opens_converge_to_max_helper() {
        use crate::config::{AletheiaDBConfig, WalConfigBuilder};
        use crate::storage::index_persistence::PersistenceConfig;
        use std::sync::{Arc, Barrier, Mutex};
        use tempfile::tempdir;

        // Mixed caps; the max (9_000) appears twice so a lost update on any raise
        // is observable. Whichever thread wins the establish race, the high-water
        // mark must still climb to the maximum.
        let caps = [4_000usize, 9_000, 2_000, 7_000, 5_000, 9_000, 1_000, 6_000];
        let expected_max = *caps.iter().max().unwrap();

        // Barrier maximizes the chance of a real race on the establish/raise RMW.
        let barrier = Arc::new(Barrier::new(caps.len()));
        // Hold every opened DB + its tempdir alive until after the assertion, so
        // all databases are genuinely open simultaneously.
        let kept: Mutex<Vec<(AletheiaDB, tempfile::TempDir)>> = Mutex::new(Vec::new());

        std::thread::scope(|s| {
            for &cap in &caps {
                let barrier = Arc::clone(&barrier);
                let kept = &kept;
                s.spawn(move || {
                    let scratch = tempdir().expect("scratch tempdir");
                    let config = AletheiaDBConfig::builder()
                        .wal(
                            WalConfigBuilder::new()
                                .wal_dir(scratch.path().join("wal"))
                                .build(),
                        )
                        .persistence(PersistenceConfig {
                            max_interned_strings: cap,
                            ..Default::default()
                        })
                        .build();
                    barrier.wait();
                    let db = AletheiaDB::with_unified_config(config).expect("concurrent open");
                    kept.lock().expect("kept mutex").push((db, scratch));
                });
            }
        });

        assert_eq!(
            crate::core::GLOBAL_INTERNER.max_capacity(),
            expected_max,
            "concurrent opens must converge the shared interner cap to the MAX of \
             all requested caps ({expected_max})"
        );
        drop(kept);
    }

    /// WAL-only recovery (no index persistence) bumps edge_id_gen past the
    /// highest edge ID seen in the WAL.  This exercises the
    /// `if persistence_manager.is_none()` branch at config.rs lines ~468-472:
    ///
    /// ```text
    /// if let Some(max_eid) = max_edge_id {
    ///     db.edge_id_gen.ensure_at_least(max_eid + 1);
    /// }
    /// ```
    #[test]
    fn wal_only_recovery_bumps_edge_id_gen() {
        use crate::WriteOps;
        use crate::config::{AletheiaDBConfig, WalConfigBuilder};
        use crate::storage::index_persistence::PersistenceConfig;
        use crate::storage::wal::DurabilityMode;
        use tempfile::tempdir;

        let scratch = tempdir().unwrap();
        let wal_dir = scratch.path().join("wal");

        let make_config = || {
            AletheiaDBConfig::builder()
                .wal(
                    WalConfigBuilder::new()
                        .wal_dir(wal_dir.clone())
                        .durability_mode(DurabilityMode::Synchronous)
                        .build(),
                )
                .persistence(PersistenceConfig {
                    enabled: false,
                    ..PersistenceConfig::default()
                })
                .build()
        };

        // Session 1: create two nodes and an edge; remember the edge ID.
        let recorded_edge_id = {
            let db = AletheiaDB::with_unified_config(make_config()).unwrap();
            let n1 = db
                .create_node("N", crate::PropertyMapBuilder::new().build())
                .unwrap();
            let n2 = db
                .create_node("N", crate::PropertyMapBuilder::new().build())
                .unwrap();
            let eid = db
                .write(|tx| tx.create_edge(n1, n2, "E", crate::PropertyMapBuilder::new().build()))
                .unwrap();
            eid.as_u64()
        };

        // Session 2: WAL-only replay — no persistence manager → enters the
        // `persistence_manager.is_none()` branch → max_edge_id is Some(_) →
        // edge_id_gen.ensure_at_least fires → new edge ID must be > recorded one.
        {
            let db = AletheiaDB::with_unified_config(make_config()).unwrap();
            assert_eq!(db.edge_count(), 1, "edge must be recovered from WAL replay");

            let n1 = db
                .create_node("N", crate::PropertyMapBuilder::new().build())
                .unwrap();
            let n2 = db
                .create_node("N", crate::PropertyMapBuilder::new().build())
                .unwrap();
            let new_eid = db
                .write(|tx| tx.create_edge(n1, n2, "E2", crate::PropertyMapBuilder::new().build()))
                .unwrap();
            assert!(
                new_eid.as_u64() > recorded_edge_id,
                "edge_id_gen must be bumped past the WAL-replayed max edge ID \
                 (got {}, expected > {})",
                new_eid.as_u64(),
                recorded_edge_id,
            );
        }
    }

    /// Issue #3387: the startup HLC seed must fold restored CLOSED
    /// transaction-time ends into the max, not just tx starts -- a closure
    /// stamped by a since-cold-migrated superseding version can exceed
    /// every restored tx start.
    #[test]
    fn bootstrap_timestamp_folds_closed_tx_ends() {
        use crate::core::GLOBAL_INTERNER;
        use crate::core::hlc::HybridTimestamp;
        use crate::core::id::{EdgeId, NodeId, VersionId};
        use crate::core::property::PropertyMapBuilder;
        use crate::storage::historical::HistoricalStorage;
        use parking_lot::RwLock;

        let current = CurrentStorage::new();
        let historical = RwLock::new(HistoricalStorage::new());

        let now = crate::core::temporal::time::now().wallclock();
        let node_start = HybridTimestamp::new(now + 3_600_000_000, 0).unwrap(); // now + 1h
        let node_end = HybridTimestamp::new(now + 7_200_000_000, 3).unwrap(); // now + 2h
        let edge_start = HybridTimestamp::new(now + 1_800_000_000, 0).unwrap();
        let edge_end = HybridTimestamp::new(now + 10_800_000_000, 5).unwrap(); // now + 3h (max)

        {
            let mut hist = historical.write();
            let label = GLOBAL_INTERNER.intern("BootstrapFold").unwrap();
            let node_id = NodeId::new(1).unwrap();
            let node_vid = VersionId::new(1).unwrap();
            hist.add_node_version(
                node_id,
                node_vid,
                node_start,
                node_start,
                label,
                PropertyMapBuilder::new().build(),
                false,
            )
            .unwrap();
            hist.close_node_version_transaction_time(node_vid, node_end)
                .unwrap();

            let edge_label = GLOBAL_INTERNER.intern("BOOTSTRAP_FOLD").unwrap();
            let edge_id = EdgeId::new(1).unwrap();
            let edge_vid = VersionId::new(2).unwrap();
            hist.add_edge_version(
                edge_id,
                edge_vid,
                edge_start,
                edge_start,
                edge_label,
                NodeId::new(1).unwrap(),
                NodeId::new(2).unwrap(),
                PropertyMapBuilder::new().build(),
                false,
            )
            .unwrap();
            hist.close_edge_version_transaction_time(edge_vid, edge_end)
                .unwrap();
        }

        let seed = bootstrap_timestamp(&current, &historical);
        assert_eq!(
            seed, edge_end,
            "seed must be the max over restored tx starts AND closed tx ends \
             (here the edge's closed end, incl. its logical component)"
        );
    }
}
