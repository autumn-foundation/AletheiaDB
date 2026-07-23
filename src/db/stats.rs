//! Database-wide statistics snapshot (Issue #3222).
//!
//! Aggregates the already-maintained internal counters — [`CurrentStats`],
//! [`HistoricalStats`], [`TieredStorageMetrics`]/[`ColdStorageStats`], and the
//! WAL system's atomic counters — into a single serializable
//! [`DatabaseStats`] value so callers (including the MCP `database_stats`
//! tool) can orient themselves in one call: how much graph is here, how much
//! history can be time-traveled through, where that history lives
//! (hot / warm-cache / cold), and what durability guarantees are active.
//!
//! # Performance: O(1) by construction — no version scans
//!
//! Every numeric field is read from a counter that the storage engines
//! already maintain incrementally:
//!
//! - Current node/edge counts: index length reads
//!   (`CurrentStorage::node_count`/`edge_count`).
//! - Historical version/anchor/delta/unique counts:
//!   `HistoricalStorage::stats()`, which per Issue #212 "returns cached
//!   counters in O(1) time instead of iterating through all versions"
//!   (see `src/storage/historical/mod.rs`).
//! - Cold-tier counters and hot/warm/cold access distribution: atomic
//!   counter snapshots from `TieredStorage::metrics()` and
//!   `RedbColdStorage::stats()`. The cold version counts are seeded from the
//!   persisted tables when the database is opened (an O(1) B-tree metadata
//!   read), so they survive restarts; byte and access counters are
//!   process-lifetime.
//! - WAL state: a field copy (`durability_mode`) plus atomic loads
//!   (`current_lsn`, `total_appends`, `consecutive_flush_errors`).
//!
//! Calling [`AletheiaDB::stats`] therefore never triggers a full version
//! scan, regardless of database size.
//!
//! # Disabled subsystems are tagged, never zero-reported
//!
//! A disabled tier is reported as `{"enabled": false}` with **no** count
//! fields, so a consumer (human or LLM) can never mistake "cold storage is
//! not configured" for "cold storage holds 0 versions".
//!
//! [`CurrentStats`]: crate::storage::current::CurrentStats
//! [`HistoricalStats`]: crate::storage::historical::HistoricalStats
//! [`TieredStorageMetrics`]: crate::storage::tiered_storage::TieredStorageMetrics
//! [`ColdStorageStats`]: crate::storage::redb_cold_storage::ColdStorageStats

use crate::db::AletheiaDB;
use crate::storage::wal::DurabilityMode;

/// Point-in-time snapshot of database size, temporal depth, storage-tier
/// distribution, and WAL state.
///
/// Produced by [`AletheiaDB::stats`]. All counters are O(1)/cached reads
/// (see the module docs); this is a consistent-enough operational snapshot,
/// not a transactionally frozen view — concurrent writers may advance
/// individual counters between reads.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct DatabaseStats {
    /// Current-state graph size (the hot, in-RAM tier queried by
    /// current-state operations).
    pub current: CurrentStateStats,
    /// Bi-temporal depth of the in-RAM historical store, including
    /// anchor/delta compression effectiveness.
    pub historical: HistoricalDepthStats,
    /// Cold-tier (disk) presence and, when enabled, counters plus the
    /// hot/warm/cold read distribution. `enabled: false` means the tier is
    /// not configured — it does NOT mean zero cold versions.
    pub cold_storage: ColdStorageTierStats,
    /// Write-ahead-log durability state.
    pub wal: WalStateStats,
    /// Tamper-evident provenance hash chain status (Issue #3351). `enabled:
    /// false` with all-`None` fields when the chain is not configured.
    pub chain: ProvenanceChainStats,
    /// Per-namespace current node/edge counts (Issue #3349). One entry per
    /// registered or populated namespace, sorted by name; O(1) membership-index
    /// reads. Empty on a database that only uses the implicit `default`
    /// namespace with no explicit registrations (the `default` entry is still
    /// present when it holds entities).
    ///
    /// Serialized on the MCP/HTTP `database_stats` JSON since PR3a (the
    /// coordinated surface slice); each entry is `{name, node_count,
    /// edge_count}`.
    pub namespaces: Vec<crate::db::namespace_query::NamespaceCount>,
    /// Push-changefeed subscription state, including the per-principal quota
    /// breakdown (Issue #3678). This is the authenticated surface for the
    /// per-principal detail deliberately kept OFF the bounded `/metrics`
    /// exposition (which stays bounded-label-only).
    pub changefeed: ChangefeedStats,
    /// Replication role + (for a replica) progress/lag observability (Issue
    /// #3355). `role` is an O(1) atomic read; `replica` is `None` on a
    /// primary and populated on a streaming replica via the applier's shared
    /// progress handle (applied LSN, entries behind, lag, state).
    pub replication: ReplicationStats,
}

// Note (Issue #3368): engine-lane per-query resource-limit termination counters
// are deliberately NOT part of the storage-layer `DatabaseStats`. They are a
// query-execution concern, surfaced via the dedicated
// [`AletheiaDB::query_limit_counters`] accessor (Rust API) and, on the MCP
// surface, via the additive `resource_limits` block the `database_stats` tool
// appends — keeping `DatabaseStats` the pure storage-tier snapshot the MCP
// thin-aggregator contract asserts it is.

/// Push-changefeed subscription state for the `database_stats` snapshot
/// (Issue #3678). Reads the broadcaster registry under a brief read lock —
/// O(live subscriptions), not a version scan.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ChangefeedStats {
    /// Total currently-live changefeed subscriptions across all principals and
    /// the bare embedded path.
    pub active_subscriptions: usize,
    /// Per-principal live-subscription breakdown, sorted by principal id. Empty
    /// when no principal-scoped subscriptions are live. This is the per-principal
    /// quota visibility AC(e) surfaces on the authenticated JSON path rather than
    /// as an unbounded `/metrics` label.
    pub per_principal: Vec<PrincipalSubscriptionStat>,
}

/// One principal's live changefeed subscription count (Issue #3678).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct PrincipalSubscriptionStat {
    /// The principal id (or the shared `"anonymous"` bucket).
    pub principal: String,
    /// The principal's currently-live subscription count.
    pub active_subscriptions: usize,
}

/// Replication role + progress observability (Issue #3355).
///
/// `role` is an O(1) atomic read of [`AletheiaDB::node_role`]. `replica` is
/// `None` on a primary; on a replica it is populated by the replication
/// engine's shared progress handle -- a Slice B addition, so this Slice A
/// skeleton always reports `None` there, even while `role == "replica"`.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ReplicationStats {
    /// This node's current role: `"primary"` or `"replica"`.
    pub role: String,
    /// Replica-only progress/lag observability. Always `None` on a primary.
    pub replica: Option<ReplicaProgressStats>,
}

/// A replica's applied position and lag relative to its primary (Issue
/// #3355). Populated starting with the Slice B replication engine.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ReplicaProgressStats {
    /// Applier state: `"connecting"`, `"streaming"`, `"resync_required"`, or
    /// `"stopped"`.
    pub state: String,
    /// The last LSN this replica has fully applied.
    pub last_applied_lsn: u64,
    /// The primary's last-known flushed LSN, when reported by the feed.
    pub primary_flushed_lsn: Option<u64>,
    /// `primary_flushed_lsn - last_applied_lsn`, when both are known.
    pub entries_behind: Option<u64>,
    /// Estimated replication lag in milliseconds, when derivable.
    pub lag_ms: Option<u64>,
    /// The most recent applier error message, if any.
    pub last_error: Option<String>,
}

/// Status of the opt-in provenance hash chain (Issue #3351).
///
/// All fields are O(1) reads of the in-memory chain head (no scans), so this
/// is safe to include in the frequently-called `stats()` snapshot. When the
/// chain is disabled every optional field is `None`.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ProvenanceChainStats {
    /// Whether the provenance hash chain is active on this database.
    pub enabled: bool,
    /// Sequence number of the current chain head (`0` = genesis, no sealed
    /// transactions yet). `None` when disabled.
    pub head_seq: Option<u64>,
    /// Lowercase-hex digest of the current chain head. `None` when disabled.
    pub head_digest: Option<String>,
    /// Lowercase-hex digest of the genesis seed. `None` when disabled.
    pub genesis_digest: Option<String>,
    /// The most recent verification result, if a verify has run this session.
    pub last_verified: Option<LastVerifiedStats>,
}

/// Outcome of the most recent chain verification (Issue #3351).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct LastVerifiedStats {
    /// Whether the pass verified cleanly.
    pub passed: bool,
    /// Wallclock micros when the pass completed.
    pub at_micros: i64,
}

/// Current-state (hot tier) graph size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct CurrentStateStats {
    /// Number of nodes in the current state. O(1) index-length read.
    pub node_count: usize,
    /// Number of edges in the current state. O(1) index-length read.
    pub edge_count: usize,
}

/// Temporal depth and anchor/delta compression effectiveness of the in-RAM
/// historical store.
///
/// All counts come from `HistoricalStorage::stats()`, whose counters are
/// maintained incrementally (Issue #212) — reading them is O(1) and never
/// iterates versions.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct HistoricalDepthStats {
    /// Total node versions retained in the hot historical store.
    pub total_node_versions: usize,
    /// Total edge versions retained in the hot historical store.
    pub total_edge_versions: usize,
    /// Number of distinct nodes that have version history.
    pub unique_nodes: usize,
    /// Number of distinct edges that have version history.
    pub unique_edges: usize,
    /// Total anchor versions (node + edge). Anchors store full property
    /// snapshots; a lower anchor share means better delta compression.
    /// Always: `anchor_count + delta_count ==
    /// total_node_versions + total_edge_versions`.
    pub anchor_count: usize,
    /// Total delta versions (node + edge). Deltas store only changes
    /// relative to the previous version.
    pub delta_count: usize,
    /// Anchor versions among node versions only.
    pub node_anchor_count: usize,
    /// Delta versions among node versions only.
    pub node_delta_count: usize,
    /// Anchor versions among edge versions only.
    pub edge_anchor_count: usize,
    /// Delta versions among edge versions only.
    pub edge_delta_count: usize,
    /// Anchors as a fraction of total versions (0.0–1.0; 1.0 when empty).
    /// Lower is better compression. Computed from the cached counters above.
    pub compression_ratio: f64,
}

/// Cold-tier (disk) storage state.
///
/// When the tier is disabled this serializes as exactly `{"enabled": false}`
/// — count fields are structurally absent so they can never be misread as
/// "zero cold versions".
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ColdStorageTierStats {
    /// Whether a cold-storage tier (tiered storage + Redb backend) is
    /// configured for this database.
    pub enabled: bool,
    /// Counters for the enabled tier; absent when `enabled` is false.
    #[cfg_attr(feature = "serde", serde(flatten))]
    pub details: Option<ColdStorageDetails>,
}

/// Counters for an enabled cold-storage tier. All values are atomic counter
/// snapshots — O(1) reads, no disk access.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct ColdStorageDetails {
    /// Node versions in the cold (disk) tier. Seeded from the persisted
    /// tables when the database is opened (an O(1) metadata read), then
    /// incremented per migration — a restarted process reports its full
    /// on-disk cold history, not zero.
    pub node_versions_stored: u64,
    /// Edge versions in the cold (disk) tier. Seeded from the persisted
    /// tables at open, like [`node_versions_stored`](Self::node_versions_stored).
    pub edge_versions_stored: u64,
    /// Cold-tier write compression ratio (raw bytes / compressed bytes).
    /// Higher is better. The underlying byte counters are **not** persisted,
    /// so this covers writes since the current process opened the database
    /// (1.0 when this process has not written anything yet).
    pub compression_ratio: f64,
    /// Distribution of historical-version reads across storage tiers,
    /// counted since the current process opened the database (access
    /// counters are not persisted).
    pub tier_access: TierAccessStats,
}

/// Where historical-version reads were served from. Together these reveal
/// the hot / warm-cache / cold distribution of temporal query traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct TierAccessStats {
    /// Reads served from the hot (in-RAM) tier.
    pub hot_hits: u64,
    /// Reads served from the warm (LRU cache) tier.
    pub warm_hits: u64,
    /// Reads served from the cold (disk) tier.
    pub cold_hits: u64,
    /// Reads that found the version in no tier.
    pub misses: u64,
}

/// Write-ahead-log durability state.
///
/// AletheiaDB currently always runs with a WAL (there is no no-WAL
/// construction path), so `enabled` is always `true` today. The flag is
/// still emitted explicitly so the schema contract covers any future no-WAL
/// build and a consumer never has to infer WAL presence from other fields.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct WalStateStats {
    /// Whether a WAL is active. Always `true` in current builds (see type
    /// docs); a hypothetical no-WAL build would report `false` here instead
    /// of omitting the object.
    pub enabled: bool,
    /// Active durability mode as a stable token: `"synchronous"`,
    /// `"async"`, `"group_commit"`, or `"async_batched"`.
    pub durability_mode: String,
    /// The **next** log sequence number to be allocated (one past the most
    /// recently assigned LSN; starts at 1 on an empty log). Atomic read.
    pub current_lsn: u64,
    /// Total WAL entries appended since startup. Atomic read.
    pub total_appends: u64,
    /// `true` when the last WAL flush succeeded (no outstanding
    /// consecutive flush errors). Atomic read.
    pub healthy: bool,
}

/// Stable, machine-readable token for a durability mode.
///
/// Deliberately NOT the `Debug` representation, so the MCP/API contract
/// cannot drift if variant fields change.
fn durability_mode_token(mode: DurabilityMode) -> &'static str {
    match mode {
        DurabilityMode::Synchronous => "synchronous",
        DurabilityMode::Async { .. } => "async",
        DurabilityMode::GroupCommit { .. } => "group_commit",
        DurabilityMode::AsyncBatched { .. } => "async_batched",
    }
}

impl AletheiaDB {
    /// Get a holistic, serializable snapshot of database statistics:
    /// current-state size, bi-temporal depth (with anchor/delta compression
    /// effectiveness), storage-tier presence/distribution, and WAL state.
    ///
    /// This is the aggregator behind the MCP `database_stats` tool. It adds
    /// no storage logic of its own — every field is read from an existing
    /// O(1)/cached counter (see the [module docs](self) for per-field
    /// provenance), so this call never scans versions and completes in
    /// microseconds regardless of database size.
    ///
    /// # Lock behavior
    ///
    /// Follows the write-path lock acquisition order (`wal` before
    /// `historical`): WAL fields are lock-free atomic reads, then the
    /// `historical` read lock is taken briefly for the cached counters and
    /// released *before* the tiered/cold-storage counters are read (via the
    /// cloned tiered-storage handle). No adjacency or ID-generator locks are
    /// touched.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use aletheiadb::AletheiaDB;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// let stats = db.stats();
    /// assert_eq!(stats.current.node_count, 0);
    /// assert!(!stats.cold_storage.enabled); // disabled, not "0 versions"
    /// assert!(stats.wal.enabled);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "the statistics snapshot should be used"]
    pub fn stats(&self) -> DatabaseStats {
        // Current-state counts: lock-free index length reads (not part of
        // the ordered write-path primitives).
        let current_stats = self.current.stats();

        // WAL (lock order #2): field copy + atomic loads, acquired before
        // `historical` per the lock-order contract.
        let wal = WalStateStats {
            enabled: true,
            durability_mode: durability_mode_token(self.wal.durability_mode()).to_string(),
            current_lsn: self.wal.current_lsn().0,
            total_appends: self.wal.total_appends(),
            healthy: self.wal.is_healthy(),
        };

        // Historical (lock order #3): take the read guard only long enough
        // to snapshot the cached counters and clone the tiered-storage
        // handle; tiered/cold counters are read after the guard is dropped.
        #[cfg(not(target_arch = "wasm32"))]
        let (hist, tiered) = {
            let historical = self.historical.read();
            (historical.stats(), historical.tiered_storage_arc())
        };
        // Ephemeral wasm profile is hot-only: no tiered/cold tier.
        #[cfg(target_arch = "wasm32")]
        let hist = self.historical.read().stats();

        let historical = HistoricalDepthStats {
            total_node_versions: hist.total_node_versions,
            total_edge_versions: hist.total_edge_versions,
            unique_nodes: hist.unique_nodes,
            unique_edges: hist.unique_edges,
            anchor_count: hist.node_anchor_count + hist.edge_anchor_count,
            delta_count: hist.node_delta_count + hist.edge_delta_count,
            node_anchor_count: hist.node_anchor_count,
            node_delta_count: hist.node_delta_count,
            edge_anchor_count: hist.edge_anchor_count,
            edge_delta_count: hist.edge_delta_count,
            compression_ratio: hist.compression_ratio(),
        };

        #[cfg(not(target_arch = "wasm32"))]
        let cold_storage = match tiered {
            Some(tiered) => {
                let metrics = tiered.metrics();
                let cold = tiered.cold_stats();
                ColdStorageTierStats {
                    enabled: true,
                    details: Some(ColdStorageDetails {
                        node_versions_stored: cold.node_versions_stored,
                        edge_versions_stored: cold.edge_versions_stored,
                        compression_ratio: cold.compression_ratio(),
                        tier_access: TierAccessStats {
                            hot_hits: metrics.hot_hits,
                            warm_hits: metrics.warm_hits,
                            cold_hits: metrics.cold_hits,
                            misses: metrics.misses,
                        },
                    }),
                }
            }
            None => ColdStorageTierStats {
                enabled: false,
                details: None,
            },
        };
        // Ephemeral wasm profile is hot-only: the cold tier is never configured.
        #[cfg(target_arch = "wasm32")]
        let cold_storage = ColdStorageTierStats {
            enabled: false,
            details: None,
        };

        // Provenance hash chain (Issue #3351): O(1) reads of the in-memory head.
        let chain = match self.chain.as_ref() {
            Some(chain) => {
                let head = chain.head();
                ProvenanceChainStats {
                    enabled: true,
                    head_seq: Some(head.seq),
                    head_digest: Some(crate::provenance_chain::to_hex(&head.digest)),
                    genesis_digest: Some(crate::provenance_chain::to_hex(
                        &chain.genesis().genesis_digest,
                    )),
                    last_verified: chain.last_verified().map(|lv| LastVerifiedStats {
                        passed: lv.passed,
                        at_micros: lv.at_micros,
                    }),
                }
            }
            None => ProvenanceChainStats {
                enabled: false,
                head_seq: None,
                head_digest: None,
                genesis_digest: None,
                last_verified: None,
            },
        };

        // Per-namespace counts (Issue #3349): O(1) membership-index reads.
        let namespaces = self.namespace_counts();

        // Changefeed subscription state (Issue #3678): a brief registry read,
        // O(live subscriptions). The per-principal breakdown is the authenticated
        // surface for the fairness quota — never a `/metrics` label, and its
        // identity roster is admin-gated at the MCP/HTTP `database_stats`
        // handlers. Both the aggregate total and the per-principal rows are read
        // under a SINGLE registry read lock so the snapshot cannot straddle two
        // instants under concurrent subscribe/deregister.
        let (active_subscriptions, per_principal_counts) =
            self.changefeed.subscription_count_and_per_principal();
        let changefeed = ChangefeedStats {
            active_subscriptions,
            per_principal: per_principal_counts
                .into_iter()
                .map(
                    |(principal, active_subscriptions)| PrincipalSubscriptionStat {
                        principal,
                        active_subscriptions,
                    },
                )
                .collect(),
        };

        // Replication role (Issue #3355): an O(1) atomic read. `replica` is
        // populated from the replication engine's shared progress handle
        // (Slice B) when this node is currently a replica; always `None` on a
        // primary (including a promoted-back-to-primary former replica) and,
        // on the wasm32 ephemeral profile (no replication engine there),
        // always `None`.
        #[cfg(not(target_arch = "wasm32"))]
        let replica_progress = if self.is_replica() {
            self.replication_progress()
        } else {
            None
        };
        #[cfg(target_arch = "wasm32")]
        let replica_progress = None;
        let replication = ReplicationStats {
            role: self.node_role().to_string(),
            replica: replica_progress,
        };

        DatabaseStats {
            current: CurrentStateStats {
                node_count: current_stats.node_count,
                edge_count: current_stats.edge_count,
            },
            historical,
            cold_storage,
            wal,
            chain,
            namespaces,
            changefeed,
            replication,
        }
    }

    /// Snapshot of engine-lane per-query resource-limit termination/rejection
    /// counters (Issue #3368 observability): wall-clock-timeout, result-row, and
    /// estimated-memory terminations plus over-ceiling override rejections.
    /// Process-lifetime relaxed atomic reads (no locks, no version scan), so it
    /// is safe to poll frequently. This is the Rust-API observability surface
    /// for the engine lane; the MCP `database_stats` tool exposes the same
    /// counts in its additive `resource_limits` block.
    #[must_use]
    pub fn query_limit_counters(&self) -> crate::query::limits::LimitCountersSnapshot {
        self.limit_counters.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durability_mode_tokens_are_stable() {
        assert_eq!(
            durability_mode_token(DurabilityMode::Synchronous),
            "synchronous"
        );
        assert_eq!(
            durability_mode_token(DurabilityMode::Async {
                flush_interval_ms: 10
            }),
            "async"
        );
        assert_eq!(
            durability_mode_token(DurabilityMode::GroupCommit {
                max_delay_ms: 10,
                max_batch_size: 200
            }),
            "group_commit"
        );
        assert_eq!(
            durability_mode_token(DurabilityMode::AsyncBatched {
                max_delay_ms: 10,
                max_batch_size: 200
            }),
            "async_batched"
        );
    }

    /// WAL counters are `u64`; extreme values must survive JSON
    /// serialization without precision loss or sign flips.
    #[cfg(feature = "serde")]
    #[test]
    fn wal_stats_u64_max_survives_json_round_trip() {
        let stats = WalStateStats {
            enabled: true,
            durability_mode: "synchronous".to_string(),
            current_lsn: u64::MAX,
            total_appends: u64::MAX,
            healthy: true,
        };
        let value = serde_json::to_value(&stats).unwrap();
        assert_eq!(value["current_lsn"].as_u64(), Some(u64::MAX));
        assert_eq!(value["total_appends"].as_u64(), Some(u64::MAX));

        // Through text and back: serde_json must keep full u64 precision
        // (arbitrary-precision integer handling, not f64).
        let text = serde_json::to_string(&stats).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["current_lsn"].as_u64(), Some(u64::MAX));
        assert_eq!(parsed["total_appends"].as_u64(), Some(u64::MAX));
    }

    /// Disabled cold storage serializes as exactly `{"enabled": false}` —
    /// no count keys an LLM could misread as "0 cold versions".
    #[cfg(feature = "serde")]
    #[test]
    fn disabled_cold_storage_serializes_without_count_fields() {
        let value = serde_json::to_value(ColdStorageTierStats {
            enabled: false,
            details: None,
        })
        .unwrap();
        assert_eq!(value, serde_json::json!({ "enabled": false }));
    }
}
