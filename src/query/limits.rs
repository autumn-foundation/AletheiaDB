//! Per-query resource limits — engine lane (Issue #3368 engine lane).
//!
//! This is the storage/executor-level counterpart to the MCP-surface limits in
//! [`crate::mcp::limits`] (which governs the `query` tool's response-byte cap
//! and per-call overrides). The engine lane enforces limits *inside* the
//! pull-based iterator pipeline itself — a [`ResourceGuardIterator`] wraps the
//! final result iterator and cooperatively aborts the scan when a dimension is
//! breached, rather than materializing an over-budget response and rejecting
//! it after the fact.
//!
//! Three dimensions are enforced, deliberately mirroring the MCP surface's
//! dimension tokens byte-for-byte so a caller correlating an engine-level
//! error with an MCP-level one never has to translate:
//!
//! - **wall-clock timeout** (`"wall_clock_timeout"`): checked *before* each
//!   pull from the inner iterator, so a query that has already blown its
//!   budget never does another unit of work.
//! - **result rows** (`"result_rows"`): the count of rows successfully pulled
//!   through the guard.
//! - **memory bytes** (`"memory_bytes"`): a cheap, allocation-free proxy for
//!   the working memory a row contributes (see [`estimate_row_bytes`]),
//!   accumulated across the scan.
//!
//! The merge semantics for folding a server default, an operator ceiling, and
//! a per-call override into one effective value are kept identical to
//! [`crate::mcp::limits::merge_dimension`] (a value of `0` means "unlimited";
//! a ceiling of `0` means "no ceiling").
//!
//! # Zero-cost when unlimited
//!
//! [`QueryResourceLimits::unlimited`] is the default, and
//! [`QueryExecutor::execute`](super::executor::QueryExecutor::execute) checks
//! [`QueryResourceLimits::is_unlimited`] before ever constructing a
//! [`ResourceGuardIterator`] — the common case allocates no guard and pays no
//! per-row overhead at all.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::executor::QueryRow;

/// Which per-query resource-limit dimension a value applies to.
///
/// The [`as_str`](Self::as_str) token is the stable value carried on
/// [`crate::core::error::QueryError::ResourceExhausted`]'s `dimension` field,
/// kept byte-for-byte identical to the equivalent tokens on
/// [`crate::mcp::limits::LimitDimension`] so the two layers never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitDimension {
    /// The wall-clock time budget for producing the result stream.
    WallClockTimeout,
    /// The number of rows pulled through the guard.
    ResultRows,
    /// The estimated working-set memory consumed materializing rows.
    MemoryBytes,
}

impl LimitDimension {
    /// Stable snake_case token, matching `crate::mcp::limits::LimitDimension`'s
    /// tokens for the dimensions the two enums share.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WallClockTimeout => "wall_clock_timeout",
            Self::ResultRows => "result_rows",
            Self::MemoryBytes => "memory_bytes",
        }
    }
}

/// The concrete limits in force for a single query's iterator pipeline.
///
/// Unlike [`crate::mcp::limits::EffectiveQueryLimits`] (which stays in
/// caller-facing units — milliseconds, row/byte counts — because it is
/// evaluated once before execution starts), this type carries an already
/// materialized [`Instant`] deadline: the executor computes it once when the
/// guard is installed, so every subsequent check is a single monotonic-clock
/// comparison with no unit conversion.
#[derive(Debug, Clone, Default)]
pub struct QueryResourceLimits {
    /// The wall-clock instant by which the query must finish producing rows.
    /// `None` = no wall-clock limit.
    pub deadline: Option<Instant>,
    /// Maximum number of rows the guard will let through. `None` = unlimited.
    pub max_rows: Option<usize>,
    /// Maximum estimated working-memory bytes the guard will let accumulate.
    /// `None` = unlimited.
    pub max_memory_bytes: Option<usize>,
}

impl QueryResourceLimits {
    /// No enforcement on any dimension.
    #[must_use]
    pub fn unlimited() -> Self {
        Self {
            deadline: None,
            max_rows: None,
            max_memory_bytes: None,
        }
    }

    /// True when no dimension is enforced. The executor uses this to decide
    /// whether to skip installing a [`ResourceGuardIterator`] entirely.
    #[must_use]
    pub fn is_unlimited(&self) -> bool {
        self.deadline.is_none() && self.max_rows.is_none() && self.max_memory_bytes.is_none()
    }
}

/// Server-level configuration for the engine-lane per-query resource limits.
///
/// Mirrors `crate::mcp::limits::QueryLimitsConfig`'s shape and merge
/// semantics: each dimension has a **default** (applied when no override is
/// supplied) and a **ceiling** (the largest value an override may request;
/// `0` = no ceiling). A value of `0` on a default/override means "unlimited".
/// The master [`enabled`](Self::enabled) switch turns all enforcement off.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct EngineQueryLimitsConfig {
    /// Master switch. When `false`, [`effective`](Self::effective) always
    /// returns [`QueryResourceLimits::unlimited`] and ignores overrides.
    pub enabled: bool,
    /// Default wall-clock timeout in milliseconds (`0` = unlimited).
    pub default_timeout_ms: u64,
    /// Operator ceiling for a per-call timeout override (`0` = no ceiling).
    pub max_timeout_ms: u64,
    /// Default maximum result rows (`0` = unlimited).
    pub default_max_result_rows: usize,
    /// Operator ceiling for a per-call row override (`0` = no ceiling).
    pub max_result_rows: usize,
    /// Default maximum estimated working-memory bytes (`0` = unlimited).
    ///
    /// Default-off (`0`) even under [`Default`], mirroring the MCP surface's
    /// memory dimension: an operator must explicitly opt in.
    pub default_max_memory_bytes: usize,
    /// Operator ceiling for the memory budget (`0` = no ceiling).
    pub max_memory_bytes: usize,
}

impl Default for EngineQueryLimitsConfig {
    /// Enabled by default, with generous timeout/row ceilings chosen so no
    /// existing request behavior changes; the memory dimension stays
    /// default-off (opt-in), matching the MCP surface's convention.
    fn default() -> Self {
        Self {
            enabled: true,
            default_timeout_ms: 30_000,
            max_timeout_ms: 300_000,
            default_max_result_rows: 1_000_000,
            max_result_rows: 10_000_000,
            default_max_memory_bytes: 0,
            max_memory_bytes: 0,
        }
    }
}

impl EngineQueryLimitsConfig {
    /// A config that enforces nothing: every dimension unlimited, overrides
    /// ignored. Useful for trusted embedded deployments and tests.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            default_timeout_ms: 0,
            max_timeout_ms: 0,
            default_max_result_rows: 0,
            max_result_rows: 0,
            default_max_memory_bytes: 0,
            max_memory_bytes: 0,
        }
    }

    /// Fold this config and an optional per-call override into the concrete
    /// [`QueryResourceLimits`] for one query, resolving the wall-clock
    /// dimension to an [`Instant`] deadline anchored at the moment of the
    /// call.
    ///
    /// # Errors
    ///
    /// Returns [`LimitOverrideError`] if `override_` requests a value above
    /// the corresponding ceiling (or requests unlimited under a finite
    /// ceiling).
    pub fn effective(
        &self,
        override_: Option<&QueryLimitsOverride>,
    ) -> Result<QueryResourceLimits, LimitOverrideError> {
        if !self.enabled {
            return Ok(QueryResourceLimits::unlimited());
        }
        let timeout_ms = merge_dimension(
            self.default_timeout_ms,
            self.max_timeout_ms,
            override_.and_then(|o| o.timeout_ms),
            LimitDimension::WallClockTimeout,
        )?;
        let max_result_rows = merge_dimension(
            self.default_max_result_rows as u64,
            self.max_result_rows as u64,
            override_.and_then(|o| o.max_result_rows).map(|v| v as u64),
            LimitDimension::ResultRows,
        )? as usize;
        let max_memory_bytes = merge_dimension(
            self.default_max_memory_bytes as u64,
            self.max_memory_bytes as u64,
            override_.and_then(|o| o.max_memory_bytes).map(|v| v as u64),
            LimitDimension::MemoryBytes,
        )? as usize;

        Ok(QueryResourceLimits {
            deadline: if timeout_ms > 0 {
                Some(Instant::now() + Duration::from_millis(timeout_ms))
            } else {
                None
            },
            max_rows: if max_result_rows > 0 {
                Some(max_result_rows)
            } else {
                None
            },
            max_memory_bytes: if max_memory_bytes > 0 {
                Some(max_memory_bytes)
            } else {
                None
            },
        })
    }
}

/// Per-call limit override for the engine lane.
///
/// Every field is optional; omitting all of them is equivalent to no
/// override at all. A value of `0` means "unlimited" for that dimension and
/// is only accepted when the operator ceiling is itself unbounded (`0`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryLimitsOverride {
    /// Per-call wall-clock timeout in milliseconds (`0` = unlimited).
    pub timeout_ms: Option<u64>,
    /// Per-call maximum result rows (`0` = unlimited).
    pub max_result_rows: Option<usize>,
    /// Per-call maximum estimated working-memory bytes (`0` = unlimited).
    pub max_memory_bytes: Option<usize>,
}

/// A per-call override that exceeded the operator hard ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitOverrideError {
    /// The dimension whose override was rejected.
    pub dimension: LimitDimension,
    /// The value the caller requested.
    pub requested: u64,
    /// The operator ceiling it exceeded.
    pub ceiling: u64,
}

impl std::fmt::Display for LimitOverrideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "requested {} of {} exceeds operator ceiling {}",
            self.dimension.as_str(),
            self.requested,
            self.ceiling
        )
    }
}

impl std::error::Error for LimitOverrideError {}

/// Fold one dimension's server default, operator ceiling, and per-call
/// override into a single effective value. Semantics kept byte-for-byte
/// identical to `crate::mcp::limits::merge_dimension`:
///
/// - **Override present**: rejected when a ceiling exists and the override
///   either requests unlimited (`0`) or exceeds the ceiling; otherwise the
///   override wins (it may be *below* the default — a tighter self-limit).
/// - **Override absent**: the server default, clamped down to the ceiling if
///   the default is unlimited or above it.
fn merge_dimension(
    default_v: u64,
    ceiling: u64,
    override_v: Option<u64>,
    dimension: LimitDimension,
) -> Result<u64, LimitOverrideError> {
    match override_v {
        Some(requested) => {
            if ceiling != 0 && (requested == 0 || requested > ceiling) {
                return Err(LimitOverrideError {
                    dimension,
                    requested,
                    ceiling,
                });
            }
            Ok(requested)
        }
        None => {
            let mut effective = default_v;
            if ceiling != 0 && (effective == 0 || effective > ceiling) {
                effective = ceiling;
            }
            Ok(effective)
        }
    }
}

/// A cheap, allocation-free, honest **proxy** for the working memory a single
/// [`QueryRow`] contributes to a query's evaluation.
///
/// This is deliberately *not* allocator-level accounting (mirroring the
/// documented philosophy of `crate::mcp::limits`'s response-byte proxy): it
/// sums `size_of::<QueryRow>()` with the O(1) cached serialized size of any
/// property maps the row carries
/// ([`PropertyMap::serialized_size`](crate::core::property::PropertyMap::serialized_size)
/// is a maintained running total, not a fresh walk) plus a small per-item
/// overhead for path/column/binding entries. It is a monotonically
/// increasing, deterministic function of the row's shape, safe to call on
/// every row of a hot scan.
#[must_use]
pub fn estimate_row_bytes(row: &QueryRow) -> usize {
    use super::executor::EntityResult;

    /// Fixed per-entity overhead (id + interned label + version/metadata),
    /// added on top of the entity's property-map size.
    const ENTITY_OVERHEAD: usize = 64;
    /// Overhead per `EntityId` in a path.
    const PATH_ENTRY_OVERHEAD: usize = std::mem::size_of::<super::executor::EntityId>();
    /// Fallback size for a `PropertyValue` whose `serialized_size()` errors
    /// (pathological recursion depth) rather than propagating a failure from
    /// a resource-accounting helper.
    const VALUE_FALLBACK_SIZE: usize = 64;

    fn entity_bytes(entity: &EntityResult) -> usize {
        match entity {
            EntityResult::Node(n) => ENTITY_OVERHEAD + n.properties.serialized_size(),
            EntityResult::Edge(e) => ENTITY_OVERHEAD + e.properties.serialized_size(),
            EntityResult::NodeId(_) => std::mem::size_of::<crate::core::NodeId>(),
            EntityResult::EdgeId(_) => std::mem::size_of::<crate::core::EdgeId>(),
            EntityResult::Null => 0,
        }
    }

    let mut total = std::mem::size_of::<QueryRow>();
    total += entity_bytes(&row.entity);

    if let Some(path) = &row.path {
        total += path.len() * PATH_ENTRY_OVERHEAD;
    }

    if let Some(columns) = &row.columns {
        for (key, value) in columns {
            total += key.len();
            total += value.serialized_size().unwrap_or(VALUE_FALLBACK_SIZE);
        }
    }

    if let Some(bindings) = &row.bindings {
        for (name, entity) in bindings {
            total += name.len();
            total += entity_bytes(entity);
        }
    }

    if let Some(edge) = &row.edge {
        total += ENTITY_OVERHEAD + edge.properties.serialized_size();
    }

    total
}

/// Per-dimension counters of over-limit query terminations (engine lane).
///
/// Cheap atomics; intended to eventually live behind an `Arc` shared across
/// the executor and surfaced through `database_stats` (Issue #3368
/// observability). Recording is `Ordering::Relaxed` — these are best-effort
/// operational counters, not used for synchronization.
#[derive(Debug, Default)]
pub struct LimitCounters {
    /// Queries terminated by the wall-clock timeout.
    pub wall_clock_timeout: AtomicU64,
    /// Queries terminated by the result-row cap.
    pub result_rows: AtomicU64,
    /// Queries terminated by the estimated-memory budget.
    pub memory_bytes: AtomicU64,
    /// Per-call overrides rejected for exceeding an operator ceiling.
    pub override_rejected: AtomicU64,
}

/// An immutable snapshot of [`LimitCounters`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LimitCountersSnapshot {
    /// Queries terminated by the wall-clock timeout.
    pub wall_clock_timeout: u64,
    /// Queries terminated by the result-row cap.
    pub result_rows: u64,
    /// Queries terminated by the estimated-memory budget.
    pub memory_bytes: u64,
    /// Per-call overrides rejected for exceeding an operator ceiling.
    pub override_rejected: u64,
}

impl LimitCounters {
    /// Increment the counter for `dimension` (a terminated query).
    pub fn record_termination(&self, dimension: LimitDimension) {
        match dimension {
            LimitDimension::WallClockTimeout => {
                self.wall_clock_timeout.fetch_add(1, Ordering::Relaxed);
            }
            LimitDimension::ResultRows => {
                self.result_rows.fetch_add(1, Ordering::Relaxed);
            }
            LimitDimension::MemoryBytes => {
                self.memory_bytes.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Increment the rejected-override counter.
    pub fn record_override_rejected(&self) {
        self.override_rejected.fetch_add(1, Ordering::Relaxed);
    }

    /// Read a consistent-enough snapshot of the counters.
    #[must_use]
    pub fn snapshot(&self) -> LimitCountersSnapshot {
        LimitCountersSnapshot {
            wall_clock_timeout: self.wall_clock_timeout.load(Ordering::Relaxed),
            result_rows: self.result_rows.load(Ordering::Relaxed),
            memory_bytes: self.memory_bytes.load(Ordering::Relaxed),
            override_rejected: self.override_rejected.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id::NodeId;
    use crate::query::executor::{EntityResult, QueryRow};

    fn node_row() -> QueryRow {
        let id = NodeId::new(1).expect("valid id");
        QueryRow::from_entity(EntityResult::NodeId(id))
    }

    // ---- estimate_row_bytes ----

    #[test]
    fn estimate_row_bytes_is_positive_for_a_node_row() {
        let row = node_row();
        assert!(estimate_row_bytes(&row) > 0);
    }

    #[test]
    fn estimate_row_bytes_grows_with_columns() {
        let small = QueryRow::from_columns(vec![]);
        let big = QueryRow::from_columns(vec![(
            "name".to_string(),
            crate::core::property::PropertyValue::String("a fairly long string value".into()),
        )]);
        assert!(estimate_row_bytes(&big) > estimate_row_bytes(&small));
    }

    // ---- QueryResourceLimits ----

    #[test]
    fn unlimited_has_no_dimensions_set() {
        let l = QueryResourceLimits::unlimited();
        assert!(l.is_unlimited());
        assert!(l.deadline.is_none());
        assert!(l.max_rows.is_none());
        assert!(l.max_memory_bytes.is_none());
    }

    #[test]
    fn any_dimension_set_is_not_unlimited() {
        let l = QueryResourceLimits {
            max_rows: Some(10),
            ..QueryResourceLimits::unlimited()
        };
        assert!(!l.is_unlimited());
    }

    // ---- EngineQueryLimitsConfig::effective ----

    #[test]
    fn disabled_is_always_unlimited_and_ignores_overrides() {
        let cfg = EngineQueryLimitsConfig::disabled();
        let eff = cfg.effective(None).expect("disabled never errors");
        assert!(eff.is_unlimited());

        let over = QueryLimitsOverride {
            timeout_ms: Some(1),
            max_result_rows: Some(1),
            max_memory_bytes: Some(1),
        };
        let eff = cfg
            .effective(Some(&over))
            .expect("disabled ignores overrides");
        assert!(eff.is_unlimited());
    }

    #[test]
    fn default_applies_server_defaults_with_no_override() {
        let cfg = EngineQueryLimitsConfig::default();
        let eff = cfg.effective(None).expect("default config never errors");
        assert!(eff.deadline.is_some());
        assert_eq!(eff.max_rows, Some(cfg.default_max_result_rows));
        // Memory dimension is default-off.
        assert_eq!(eff.max_memory_bytes, None);
    }

    #[test]
    fn override_within_ceiling_is_honored_even_when_tighter() {
        let cfg = EngineQueryLimitsConfig::default();
        let over = QueryLimitsOverride {
            timeout_ms: Some(500),
            max_result_rows: Some(5),
            max_memory_bytes: None,
        };
        let eff = cfg.effective(Some(&over)).expect("within ceiling");
        assert_eq!(eff.max_rows, Some(5));
        let remaining = eff
            .deadline
            .expect("timeout set")
            .saturating_duration_since(Instant::now());
        assert!(remaining <= Duration::from_millis(500));
    }

    #[test]
    fn override_above_ceiling_is_rejected_with_dimension_and_values() {
        let cfg = EngineQueryLimitsConfig::default();
        let over = QueryLimitsOverride {
            timeout_ms: Some(cfg.max_timeout_ms + 1),
            max_result_rows: None,
            max_memory_bytes: None,
        };
        let err = cfg.effective(Some(&over)).unwrap_err();
        assert_eq!(err.dimension, LimitDimension::WallClockTimeout);
        assert_eq!(err.requested, cfg.max_timeout_ms + 1);
        assert_eq!(err.ceiling, cfg.max_timeout_ms);
    }

    #[test]
    fn override_zero_under_finite_ceiling_is_rejected() {
        let cfg = EngineQueryLimitsConfig::default();
        let over = QueryLimitsOverride {
            timeout_ms: Some(0),
            max_result_rows: None,
            max_memory_bytes: None,
        };
        let err = cfg.effective(Some(&over)).unwrap_err();
        assert_eq!(err.dimension, LimitDimension::WallClockTimeout);
        assert_eq!(err.requested, 0);
    }

    #[test]
    fn override_zero_under_no_ceiling_means_unlimited() {
        let cfg = EngineQueryLimitsConfig {
            max_timeout_ms: 0,
            ..EngineQueryLimitsConfig::default()
        };
        let over = QueryLimitsOverride {
            timeout_ms: Some(0),
            max_result_rows: None,
            max_memory_bytes: None,
        };
        let eff = cfg.effective(Some(&over)).expect("no ceiling, 0 accepted");
        assert!(eff.deadline.is_none());
    }

    #[test]
    fn default_above_ceiling_is_clamped_to_ceiling() {
        let cfg = EngineQueryLimitsConfig {
            default_max_result_rows: 1_000_000,
            max_result_rows: 10,
            ..EngineQueryLimitsConfig::default()
        };
        let eff = cfg.effective(None).expect("no override");
        assert_eq!(eff.max_rows, Some(10));
    }

    // ---- LimitCounters ----

    #[test]
    fn counters_record_per_dimension() {
        let c = LimitCounters::default();
        c.record_termination(LimitDimension::WallClockTimeout);
        c.record_termination(LimitDimension::ResultRows);
        c.record_termination(LimitDimension::ResultRows);
        c.record_termination(LimitDimension::MemoryBytes);
        c.record_override_rejected();

        let s = c.snapshot();
        assert_eq!(s.wall_clock_timeout, 1);
        assert_eq!(s.result_rows, 2);
        assert_eq!(s.memory_bytes, 1);
        assert_eq!(s.override_rejected, 1);
    }

    #[test]
    fn dimension_tokens_match_mcp_surface_tokens() {
        assert_eq!(
            LimitDimension::WallClockTimeout.as_str(),
            "wall_clock_timeout"
        );
        assert_eq!(LimitDimension::ResultRows.as_str(), "result_rows");
        assert_eq!(LimitDimension::MemoryBytes.as_str(), "memory_bytes");
    }
}
