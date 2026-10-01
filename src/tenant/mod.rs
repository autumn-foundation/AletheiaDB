//! Multi-tenant isolation runtime (Issue #3365).
//!
//! A [`TenantManager`] serves many isolated logical databases from **one
//! process**. Each tenant is a *fully separate* [`AletheiaDB`] instance: its own
//! nodes, edges, bi-temporal history, vector indexes, constraints, schema, WAL,
//! and (for a durable deployment) its own on-disk directory. Because the
//! instances share no storage, no locks, and no ID space, the core isolation
//! guarantees fall out **by construction** rather than from per-query filtering:
//!
//! - **Hard data isolation** — no query surface can return another tenant's data
//!   under any parameterization, because a tenant handle only ever touches its
//!   own [`AletheiaDB`]. Traversal, k-NN, `AS OF` reads, changefeed, and every
//!   error message are scoped to a single instance.
//! - **Temporal invariants compose** — each tenant's valid/tx-time semantics,
//!   WAL durability, and crash recovery are exactly a single-tenant database's,
//!   because that is literally what each tenant is. One tenant can be backed
//!   up / restored (`.albk`, Issue #3217) independently of the others.
//! - **Blast-radius containment** — one tenant hitting a quota, erroring, or
//!   being deleted touches only its own map entry and directory; every other
//!   tenant is a separate `Arc<AletheiaDB>` with a separate lock graph, so a
//!   saturating writer in one tenant does not stall another's reads.
//! - **ID / business-key collision is harmless** — two tenants may both have a
//!   node id `1` named "Alice" with no interference.
//!
//! ## v1 caveat: the string interner is process-global
//!
//! One thing is *not* per-tenant: the string interner
//! ([`GLOBAL_INTERNER`](crate::core::GLOBAL_INTERNER)) that maps labels,
//! edge-types, and property **keys** to ids is shared across every `AletheiaDB`
//! in the process. This never leaks **data** — property *values* are stored
//! inline per instance (not interned), and no read surface enumerates the
//! interner, so a tenant's storage only ever references ids it interned itself.
//! But the interner's capacity is a *shared* resource: a tenant writing an
//! unbounded number of *distinct* labels/keys could exhaust the process-wide cap
//! and make other tenants' schema-introducing writes fail, and interner ids are
//! not reclaimed when a tenant is deleted. Per-tenant interners (which would also
//! remove the shared cap) are a deliberate follow-up; size the cap for the
//! expected tenant count in the meantime. The count/byte quotas here bound
//! per-tenant node/edge/storage growth, not distinct-vocabulary cardinality.
//!
//! # Quotas
//!
//! Each tenant carries a [`TenantQuota`] (adjustable after creation). Writes that
//! would exceed a quota are rejected with a structured
//! [`TenantError::QuotaExceeded`] (→ `RESOURCE_EXHAUSTED`) **before** the write
//! is applied — never a partial write. Node- and edge-count limits are enforced
//! **precisely** via an atomic reservation taken before the write and released
//! on failure, so concurrent writers can never race past the cap. The two byte
//! dimensions (vector-index memory, total storage) are enforced **best-effort**
//! against O(1) estimators (see [`TenantHandle::usage`]); exact byte accounting
//! is a documented v1 follow-up.
//!
//! # The write path is the handle
//!
//! Quota enforcement and usage accounting live on [`TenantHandle`]'s
//! `create_*` / `delete_*` methods. [`TenantHandle::db`] exposes the underlying
//! [`AletheiaDB`] for reads and queries (always fully isolated). Writes issued
//! directly through `db()` bypass quota accounting — the sanctioned, enforced
//! write path is the handle. This mirrors the engine's other cooperative-scoping
//! layers while keeping isolation itself unconditional.
//!
//! # Single-tenant default
//!
//! `AletheiaDB::new()` / `open()` are untouched and carry zero tenant overhead. A
//! deployment that never constructs a [`TenantManager`] preserves 100% of current
//! behavior with zero configuration.

use crate::core::error::{Error, Result};
use crate::core::id::{EdgeId, NodeId};
use crate::core::property::PropertyMap;
use crate::core::temporal::time;
use crate::core::tenant::{
    QuotaDimension, TenantError, TenantId, TenantInfo, TenantQuota, TenantUsage,
};
use crate::db::AletheiaDB;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "serde")]
mod persist;

/// Rough per-entity byte cost used by the best-effort `storage_bytes` estimator.
/// Deliberately coarse — the byte quotas are capacity guard-rails, not exact
/// accounting (see the module docs).
const EST_ENTITY_BYTES: u64 = 256;
/// Rough per-historical-version byte cost for the `storage_bytes` estimator.
const EST_VERSION_BYTES: u64 = 128;
/// Bytes per f32 vector component, for the `vector_index_bytes` estimator.
const BYTES_PER_VECTOR_COMPONENT: u64 = 4;

/// One tenant: an isolated [`AletheiaDB`] plus its quota and O(1) reservation
/// counters. Held behind an `Arc` so a [`TenantHandle`] can outlive a concurrent
/// `delete_tenant` of the *registry entry* without tearing the database out from
/// under an in-flight read.
pub(crate) struct Tenant {
    id: TenantId,
    db: Arc<AletheiaDB>,
    created_at_micros: i64,
    /// The on-disk data directory for a durable tenant, or `None` when ephemeral.
    data_dir: Option<PathBuf>,
    quota: RwLock<TenantQuota>,
    /// Exact current-state node count, seeded from storage at open and kept in
    /// lockstep by the handle's create/delete methods. Doubles as the precise
    /// node-quota reservation counter.
    nodes: AtomicU64,
    /// Exact current-state edge count; see [`Tenant::nodes`].
    edges: AtomicU64,
}

impl Tenant {
    /// Seed the reservation counters from the database's O(1) current-state
    /// counts (used at create and on durable reopen).
    fn seed_counters(&self) {
        let stats = self.db.stats();
        self.nodes
            .store(stats.current.node_count as u64, Ordering::Release);
        self.edges
            .store(stats.current.edge_count as u64, Ordering::Release);
    }

    /// Best-effort estimate of vector-index memory, in bytes.
    ///
    /// `Σ_index(dimensions) × vector_count × 4`. Exact-ish for the common
    /// single-index case; an over-estimate when multiple indexes hold different
    /// subsets (documented v1 limitation).
    fn estimate_vector_index_bytes(&self) -> u64 {
        let dims_sum: u64 = self
            .db
            .current
            .list_vector_indexes()
            .iter()
            .map(|i| i.dimensions as u64)
            .sum();
        let vectors = self.db.current.vector_count() as u64;
        dims_sum
            .saturating_mul(vectors)
            .saturating_mul(BYTES_PER_VECTOR_COMPONENT)
    }

    /// Best-effort estimate of total storage, in bytes: current entities plus
    /// retained historical versions, each weighted by a coarse per-item cost.
    fn estimate_storage_bytes(&self) -> u64 {
        let stats = self.db.stats();
        let entities = (stats.current.node_count as u64) + (stats.current.edge_count as u64);
        let versions = (stats.historical.total_node_versions as u64)
            + (stats.historical.total_edge_versions as u64);
        entities
            .saturating_mul(EST_ENTITY_BYTES)
            .saturating_add(versions.saturating_mul(EST_VERSION_BYTES))
    }

    /// A consistent-enough usage snapshot (O(1) counter reads; never a version
    /// scan). Node/edge counts are exact; the byte fields are best-effort.
    fn usage(&self) -> TenantUsage {
        TenantUsage {
            node_count: self.nodes.load(Ordering::Acquire),
            edge_count: self.edges.load(Ordering::Acquire),
            vector_index_bytes: self.estimate_vector_index_bytes(),
            storage_bytes: self.estimate_storage_bytes(),
        }
    }

    /// Check the best-effort byte quotas against current usage, returning the
    /// structured error if either is already at/over its limit.
    fn check_byte_quotas(&self, quota: &TenantQuota) -> Result<()> {
        if let Some(limit) = quota.max_vector_index_bytes {
            let current = self.estimate_vector_index_bytes();
            if current >= limit {
                return Err(self.quota_error(QuotaDimension::VectorIndexBytes, current, limit));
            }
        }
        if let Some(limit) = quota.max_storage_bytes {
            let current = self.estimate_storage_bytes();
            if current >= limit {
                return Err(self.quota_error(QuotaDimension::StorageBytes, current, limit));
            }
        }
        Ok(())
    }

    /// Build a [`TenantError::QuotaExceeded`] for this tenant.
    fn quota_error(&self, dimension: QuotaDimension, current: u64, limit: u64) -> Error {
        Error::Tenant(TenantError::QuotaExceeded {
            tenant: self.id.as_str().to_string(),
            dimension,
            current,
            limit,
        })
    }

    /// Reserve capacity for one new node against the precise count quota (and the
    /// best-effort byte quotas). Returns an RAII [`CountReservation`] that
    /// decrements the counter on drop unless committed.
    fn reserve_node(&self) -> Result<CountReservation<'_>> {
        let quota = *self.quota.read();
        self.check_byte_quotas(&quota)?;
        reserve_count(
            &self.nodes,
            quota.max_nodes,
            QuotaDimension::Nodes,
            &self.id,
        )
    }

    /// Reserve capacity for one new edge; see [`Tenant::reserve_node`].
    fn reserve_edge(&self) -> Result<CountReservation<'_>> {
        let quota = *self.quota.read();
        self.check_byte_quotas(&quota)?;
        reserve_count(
            &self.edges,
            quota.max_edges,
            QuotaDimension::Edges,
            &self.id,
        )
    }

    fn info(&self) -> TenantInfo {
        TenantInfo {
            id: self.id.clone(),
            created_at_micros: self.created_at_micros,
            quota: *self.quota.read(),
        }
    }
}

/// Atomically reserve one unit against a counter bounded by `limit` (or
/// unbounded when `None`). Precise under concurrency: a CAS loop guarantees the
/// counter never crosses `limit`.
fn reserve_count<'a>(
    counter: &'a AtomicU64,
    limit: Option<u64>,
    dimension: QuotaDimension,
    tenant: &TenantId,
) -> Result<CountReservation<'a>> {
    match limit {
        None => {
            counter.fetch_add(1, Ordering::AcqRel);
        }
        Some(max) => loop {
            let cur = counter.load(Ordering::Acquire);
            if cur >= max {
                return Err(Error::Tenant(TenantError::QuotaExceeded {
                    tenant: tenant.as_str().to_string(),
                    dimension,
                    current: cur,
                    limit: max,
                }));
            }
            if counter
                .compare_exchange_weak(cur, cur + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        },
    }
    Ok(CountReservation {
        counter,
        committed: false,
    })
}

/// Decrement a usage counter by `n`, **saturating at zero** so it can never
/// underflow and wrap to `u64::MAX`.
///
/// A plain `fetch_sub` would wrap if the counter and reality ever drift — e.g. a
/// caller deletes (through the accounted handle) an entity it created directly
/// via the unaccounted [`TenantHandle::db`] escape hatch, leaving the counter at
/// 0 while the delete genuinely succeeds. A wrapped counter would then reject
/// every future create for the tenant's process lifetime; clamping keeps a drift
/// self-healing instead of catastrophic.
fn saturating_dec(counter: &AtomicU64, n: u64) {
    let mut cur = counter.load(Ordering::Acquire);
    loop {
        let next = cur.saturating_sub(n);
        match counter.compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => break,
            Err(observed) => cur = observed,
        }
    }
}

/// RAII reservation of one count unit. Dropping without [`commit`](Self::commit)
/// releases the reservation (the write failed); committing keeps it (the write
/// succeeded, so the count is now real).
struct CountReservation<'a> {
    counter: &'a AtomicU64,
    committed: bool,
}

impl CountReservation<'_> {
    /// Keep the reservation permanently — the write committed.
    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for CountReservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.counter.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// A cheap, cloneable handle bound to exactly one tenant (Issue #3365).
///
/// All reads/queries go through [`db`](Self::db) (fully isolated). All
/// quota-enforced writes go through this handle's `create_*` / `delete_*`
/// methods. A connection or session binds to one tenant by holding one handle;
/// the tenant claim a #3350 identity carries maps 1:1 onto which handle it is
/// issued.
#[derive(Clone)]
pub struct TenantHandle {
    tenant: Arc<Tenant>,
}

impl TenantHandle {
    /// The tenant's identifier.
    #[must_use]
    pub fn id(&self) -> &TenantId {
        &self.tenant.id
    }

    /// The underlying isolated [`AletheiaDB`], for **reads, queries, and index
    /// management** — every read surface is fully tenant-scoped by construction.
    ///
    /// This is an **unaccounted** handle: writes issued directly through it
    /// (`db().create_node(...)`, an AQL/Cypher write, `apply_batch`, …) bypass
    /// **both** quota enforcement **and** the usage counters, so they also
    /// desynchronize [`usage`](Self::usage) and skew subsequent quota checks
    /// (which reserve against those counters). The sanctioned, accounted write
    /// path is this handle's `create_*` / `delete_*` methods — route writes
    /// through them. `db()` is for reads.
    #[must_use]
    pub fn db(&self) -> &Arc<AletheiaDB> {
        &self.tenant.db
    }

    /// Public metadata for this tenant (id, creation time, current quota).
    #[must_use]
    pub fn info(&self) -> TenantInfo {
        self.tenant.info()
    }

    /// This tenant's current resource quota.
    #[must_use]
    pub fn quota(&self) -> TenantQuota {
        *self.tenant.quota.read()
    }

    /// Replace this tenant's quota (takes effect on the next write). Lowering a
    /// limit below current usage never retroactively deletes data; it simply
    /// rejects further growth on that dimension until usage drops back under the
    /// new limit.
    pub fn set_quota(&self, quota: TenantQuota) {
        *self.tenant.quota.write() = quota;
    }

    /// A point-in-time usage snapshot (O(1) counters), suitable for metering.
    ///
    /// Node/edge counts are exact for writes made **through this handle**; the
    /// two byte fields are best-effort estimates. Writes made directly through
    /// the unaccounted [`db`](Self::db) handle are not reflected in the counts
    /// (see [`db`](Self::db)).
    #[must_use]
    pub fn usage(&self) -> TenantUsage {
        self.tenant.usage()
    }

    /// Create a node, enforcing the tenant's node-count and byte quotas.
    ///
    /// # Errors
    ///
    /// [`TenantError::QuotaExceeded`] (→ `RESOURCE_EXHAUSTED`) if the write would
    /// exceed a quota — rejected atomically, with no partial write. Otherwise any
    /// error the underlying [`AletheiaDB::create_node`] returns.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_node(&self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        let reservation = self.tenant.reserve_node()?;
        let id = self.tenant.db.create_node(label, properties)?;
        reservation.commit();
        Ok(id)
    }

    /// Create a node in an intra-tenant namespace (Issue #3349), enforcing the
    /// tenant's quotas. Namespaces scope data *within* one tenant; they are not a
    /// tenant boundary.
    ///
    /// # Errors
    ///
    /// See [`create_node`](Self::create_node).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_node_in_namespace(
        &self,
        label: &str,
        properties: PropertyMap,
        namespace: impl AsRef<str>,
    ) -> Result<NodeId> {
        let reservation = self.tenant.reserve_node()?;
        let id = self
            .tenant
            .db
            .create_node_in_namespace(label, properties, namespace)?;
        reservation.commit();
        Ok(id)
    }

    /// Create an edge, enforcing the tenant's edge-count and byte quotas.
    ///
    /// # Errors
    ///
    /// See [`create_node`](Self::create_node).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_edge(
        &self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<EdgeId> {
        let reservation = self.tenant.reserve_edge()?;
        let id = self
            .tenant
            .db
            .create_edge(source, target, label, properties)?;
        reservation.commit();
        Ok(id)
    }

    /// Create an edge in an intra-tenant namespace (Issue #3349). See
    /// [`create_edge`](Self::create_edge) and
    /// [`create_node_in_namespace`](Self::create_node_in_namespace).
    ///
    /// # Errors
    ///
    /// See [`create_node`](Self::create_node).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_edge_in_namespace(
        &self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
        namespace: impl AsRef<str>,
    ) -> Result<EdgeId> {
        let reservation = self.tenant.reserve_edge()?;
        let id = self
            .tenant
            .db
            .create_edge_in_namespace(source, target, label, properties, namespace)?;
        reservation.commit();
        Ok(id)
    }

    /// Delete a node (without deleting connected edges), keeping the usage
    /// counter in sync. See [`AletheiaDB::delete_node_with_valid_time`].
    ///
    /// # Errors
    ///
    /// Any error the underlying delete returns; on error the usage counter is
    /// left unchanged.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn delete_node(&self, node_id: NodeId) -> Result<()> {
        self.tenant.db.delete_node_with_valid_time(node_id, None)?;
        saturating_dec(&self.tenant.nodes, 1);
        Ok(())
    }

    /// Delete a node and all connected edges (cascade), keeping both usage
    /// counters in sync. See [`AletheiaDB::delete_node_cascade_with_options`].
    ///
    /// # Errors
    ///
    /// Any error the underlying cascade delete returns; on error the usage
    /// counters are left unchanged.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn delete_node_cascade(&self, node_id: NodeId) -> Result<()> {
        // Count connected edges before the delete so the edge counter can be
        // decremented by the right amount (DISTINCT edges; a self-loop counts
        // once — matches `count_connected_edges`, Issue #3416).
        let connected = self.tenant.db.count_connected_edges(node_id)? as u64;
        self.tenant
            .db
            .delete_node_cascade_with_options(node_id, Default::default())?;
        // Both decrements saturate at zero (never wrap) — see `saturating_dec`.
        saturating_dec(&self.tenant.nodes, 1);
        if connected > 0 {
            saturating_dec(&self.tenant.edges, connected);
        }
        Ok(())
    }

    /// Delete an edge, keeping the usage counter in sync. See
    /// [`AletheiaDB::delete_edge_with_valid_time`].
    ///
    /// # Errors
    ///
    /// Any error the underlying delete returns; on error the usage counter is
    /// left unchanged.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn delete_edge(&self, edge_id: EdgeId) -> Result<()> {
        self.tenant.db.delete_edge_with_valid_time(edge_id, None)?;
        saturating_dec(&self.tenant.edges, 1);
        Ok(())
    }

    /// Back up this tenant's complete bi-temporal state to a portable `.albk`
    /// artifact (Issue #3217), independently of every other tenant.
    ///
    /// # Errors
    ///
    /// Any error [`AletheiaDB::backup`] returns.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn backup(&self, path: &std::path::Path) -> Result<crate::db::BackupSummary> {
        self.tenant.db.backup(path)
    }

    /// Build an MCP server bound to **this tenant only** (Issue #3365).
    ///
    /// Because a tenant is a fully separate [`AletheiaDB`], the returned server
    /// exposes exactly the same tool surface as a single-tenant deployment, but
    /// every tool operates only on this tenant's data — existing agent tooling
    /// works unchanged against a tenant-scoped endpoint. The MCP server is
    /// anonymous by default; layer [`crate::mcp::AletheiaMcpServer::with_auth`]
    /// on the same `db()` handle to bind a #3350 identity (whose tenant claim
    /// selects which handle it is issued).
    #[cfg(feature = "mcp-server")]
    #[must_use]
    pub fn mcp_server(&self) -> crate::mcp::AletheiaMcpServer {
        crate::mcp::AletheiaMcpServer::new(Arc::clone(self.db()))
    }
}

/// The multi-tenant runtime: create, list, look up, and drop isolated tenants,
/// each a fully separate [`AletheiaDB`] (Issue #3365).
pub struct TenantManager {
    /// The live tenant registry. `Arc<Tenant>` so a handle can outlive a
    /// concurrent registry drop.
    tenants: RwLock<HashMap<TenantId, Arc<Tenant>>>,
    /// The durable root, or `None` for an all-ephemeral manager. When `Some`,
    /// tenant `t` lives at `{root}/tenants/{t}` and the registry sidecar is
    /// `{root}/tenants.json`.
    root: Option<PathBuf>,
    /// Serializes registry mutations + sidecar saves.
    save_lock: parking_lot::Mutex<()>,
}

impl TenantManager {
    /// An all-ephemeral manager: every tenant is an in-memory
    /// [`AletheiaDB::new`], nothing is persisted, and nothing survives the
    /// process. The right choice for tests and scratch multi-tenant sessions.
    #[must_use]
    pub fn new_ephemeral() -> Self {
        Self {
            tenants: RwLock::new(HashMap::new()),
            root: None,
            save_lock: parking_lot::Mutex::new(()),
        }
    }

    /// Open (or create) a **durable** multi-tenant deployment rooted at `root`.
    ///
    /// Each tenant's data lives under `{root}/tenants/{id}`; the tenant registry
    /// (ids + quotas) is persisted to `{root}/tenants.json`. On open, every
    /// previously-registered tenant's database is reopened from its directory, so
    /// a restart restores every tenant to its last consistent state.
    ///
    /// # Errors
    ///
    /// Returns an error if the root is not writable, the registry sidecar is
    /// present but unreadable in a way that cannot be safely quarantined, or a
    /// tenant database fails to reopen.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("tenants")).map_err(Error::Io)?;
        let manager = Self {
            tenants: RwLock::new(HashMap::new()),
            root: Some(root.clone()),
            save_lock: parking_lot::Mutex::new(()),
        };
        #[cfg(feature = "serde")]
        manager.load_registry()?;
        Ok(manager)
    }

    /// Whether this manager persists tenants durably.
    #[must_use]
    pub fn is_durable(&self) -> bool {
        self.root.is_some()
    }

    /// The number of currently-registered tenants.
    #[must_use]
    pub fn tenant_count(&self) -> usize {
        self.tenants.read().len()
    }

    /// Construct a new tenant's isolated database at the appropriate backing.
    fn build_tenant_db(&self, id: &TenantId) -> Result<(Arc<AletheiaDB>, Option<PathBuf>)> {
        match &self.root {
            None => Ok((Arc::new(AletheiaDB::new()?), None)),
            #[cfg(not(target_arch = "wasm32"))]
            Some(root) => {
                let dir = root.join("tenants").join(id.as_str());
                let db = AletheiaDB::with_unified_config(
                    crate::config::durable_config_for_data_dir(dir.clone()),
                )?;
                Ok((Arc::new(db), Some(dir)))
            }
            // A durable root cannot exist on wasm32 (no `open`), but the match
            // must be total.
            #[cfg(target_arch = "wasm32")]
            Some(_) => Ok((Arc::new(AletheiaDB::new()?), None)),
        }
    }

    /// Create a new tenant with the given quota, returning a bound handle.
    ///
    /// Provisioning is process-restart-free and fast (a single database open).
    ///
    /// # Errors
    ///
    /// - [`TenantError::InvalidId`] (→ `INVALID_ARGUMENT`) if `id` is malformed.
    /// - [`TenantError::AlreadyExists`] (→ `CONFLICT`) if a tenant with that id
    ///   already exists.
    /// - A storage error if the tenant database cannot be created.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn create_tenant(&self, id: impl AsRef<str>, quota: TenantQuota) -> Result<TenantHandle> {
        let id = TenantId::new(id.as_ref())?;
        let _guard = self.save_lock.lock();
        if self.tenants.read().contains_key(&id) {
            return Err(Error::Tenant(TenantError::AlreadyExists {
                id: id.into_string(),
            }));
        }
        let (db, data_dir) = self.build_tenant_db(&id)?;
        let tenant = Arc::new(Tenant {
            id: id.clone(),
            db,
            created_at_micros: time::now().wallclock(),
            data_dir,
            quota: RwLock::new(quota),
            nodes: AtomicU64::new(0),
            edges: AtomicU64::new(0),
        });
        tenant.seed_counters();
        self.tenants.write().insert(id, Arc::clone(&tenant));
        #[cfg(feature = "serde")]
        if let Err(e) = self.save_registry_locked() {
            // Roll back the in-memory insert so the registry and the sidecar stay
            // consistent (mirrors the namespace registry's create rollback).
            self.tenants.write().remove(&tenant.id);
            return Err(e);
        }
        Ok(TenantHandle { tenant })
    }

    /// Look up a bound handle for an existing tenant.
    ///
    /// # Errors
    ///
    /// [`TenantError::NotFound`] (→ `NOT_FOUND`) if no such tenant exists.
    /// [`TenantError::InvalidId`] (→ `INVALID_ARGUMENT`) if `id` is malformed.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_tenant(&self, id: impl AsRef<str>) -> Result<TenantHandle> {
        let id = TenantId::new(id.as_ref())?;
        self.tenants
            .read()
            .get(&id)
            .map(|t| TenantHandle {
                tenant: Arc::clone(t),
            })
            .ok_or_else(|| {
                Error::Tenant(TenantError::NotFound {
                    id: id.into_string(),
                })
            })
    }

    /// List all tenants, sorted by id for stable output.
    #[must_use]
    pub fn list_tenants(&self) -> Vec<TenantInfo> {
        let mut infos: Vec<TenantInfo> = self.tenants.read().values().map(|t| t.info()).collect();
        infos.sort_by(|a, b| a.id.cmp(&b.id));
        infos
    }

    /// Public metadata for one tenant, mirroring [`get_namespace`]-style
    /// single-entity lookup.
    ///
    /// [`get_namespace`]: crate::db::AletheiaDB::get_namespace
    ///
    /// # Errors
    ///
    /// [`TenantError::NotFound`] / [`TenantError::InvalidId`] as for
    /// [`get_tenant`](Self::get_tenant).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_tenant_info(&self, id: impl AsRef<str>) -> Result<TenantInfo> {
        Ok(self.get_tenant(id)?.info())
    }

    /// Report one tenant's usage.
    ///
    /// # Errors
    ///
    /// [`TenantError::NotFound`] / [`TenantError::InvalidId`] as for
    /// [`get_tenant`](Self::get_tenant).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn tenant_usage(&self, id: impl AsRef<str>) -> Result<TenantUsage> {
        Ok(self.get_tenant(id)?.usage())
    }

    /// Adjust an existing tenant's quota.
    ///
    /// # Errors
    ///
    /// [`TenantError::NotFound`] / [`TenantError::InvalidId`] as for
    /// [`get_tenant`](Self::get_tenant); or a persistence error if the durable
    /// registry cannot be updated.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn set_tenant_quota(&self, id: impl AsRef<str>, quota: TenantQuota) -> Result<()> {
        let handle = self.get_tenant(id)?;
        handle.set_quota(quota);
        #[cfg(feature = "serde")]
        {
            let _guard = self.save_lock.lock();
            self.save_registry_locked()?;
        }
        Ok(())
    }

    /// Delete a tenant: deregister it (so no further access resolves) and, for a
    /// durable deployment, reclaim its on-disk directory best-effort.
    ///
    /// Deleting tenant A has **no effect** on any other tenant — each is a
    /// separate database with a separate directory. A caller still holding a
    /// [`TenantHandle`] keeps its `AletheiaDB` alive until that handle drops
    /// (so an in-flight read never tears); the *registry* entry is gone
    /// immediately, which is what isolation requires.
    ///
    /// # Errors
    ///
    /// [`TenantError::NotFound`] (→ `NOT_FOUND`) if no such tenant exists;
    /// [`TenantError::InvalidId`] if `id` is malformed; or a persistence error
    /// if the durable registry cannot be updated.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn delete_tenant(&self, id: impl AsRef<str>) -> Result<()> {
        let id = TenantId::new(id.as_ref())?;
        let _guard = self.save_lock.lock();
        let removed = {
            let mut tenants = self.tenants.write();
            match tenants.remove(&id) {
                Some(t) => t,
                None => {
                    return Err(Error::Tenant(TenantError::NotFound {
                        id: id.into_string(),
                    }));
                }
            }
        };
        #[cfg(feature = "serde")]
        if let Err(e) = self.save_registry_locked() {
            // Re-insert to keep the registry and sidecar consistent on a save
            // failure.
            self.tenants.write().insert(removed.id.clone(), removed);
            return Err(e);
        }
        // Best-effort disk reclamation. On unix an open database's files are
        // unlinked here and freed when the last handle drops; the directory
        // itself is removed once nothing holds it open. Ignore errors — the
        // tenant is already logically gone and every other tenant is untouched.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(dir) = &removed.data_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
        Ok(())
    }

    /// Restore a previously backed-up tenant from an `.albk` artifact into a new
    /// tenant `id` with the given quota (Issue #3217), independently of every
    /// other tenant.
    ///
    /// # Errors
    ///
    /// [`TenantError::AlreadyExists`] if `id` is taken; [`TenantError::InvalidId`]
    /// if malformed; or any error the backup restore returns.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn restore_tenant(
        &self,
        id: impl AsRef<str>,
        artifact: &std::path::Path,
        quota: TenantQuota,
    ) -> Result<TenantHandle> {
        let id = TenantId::new(id.as_ref())?;
        let _guard = self.save_lock.lock();
        if self.tenants.read().contains_key(&id) {
            return Err(Error::Tenant(TenantError::AlreadyExists {
                id: id.into_string(),
            }));
        }
        let (db, data_dir) = match &self.root {
            // Durable: restore into the tenant's canonical data dir, then reopen
            // via the normal durable path so it persists across restarts.
            Some(root) => {
                let dir = root.join("tenants").join(id.as_str());
                let restored = AletheiaDB::restore_to_data_dir(artifact, &dir)?;
                drop(restored);
                let db = AletheiaDB::with_unified_config(
                    crate::config::durable_config_for_data_dir(dir.clone()),
                )?;
                (Arc::new(db), Some(dir))
            }
            // Ephemeral: restore into a fresh in-memory database.
            None => (Arc::new(AletheiaDB::restore(artifact)?), None),
        };
        let tenant = Arc::new(Tenant {
            id: id.clone(),
            db,
            created_at_micros: time::now().wallclock(),
            data_dir,
            quota: RwLock::new(quota),
            nodes: AtomicU64::new(0),
            edges: AtomicU64::new(0),
        });
        tenant.seed_counters();
        self.tenants.write().insert(id, Arc::clone(&tenant));
        #[cfg(feature = "serde")]
        if let Err(e) = self.save_registry_locked() {
            self.tenants.write().remove(&tenant.id);
            return Err(e);
        }
        Ok(TenantHandle { tenant })
    }
}

#[cfg(test)]
mod tests;
