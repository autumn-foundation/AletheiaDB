//! Per-query resource limits for the MCP `query` tool (Issue #3368).
//!
//! This is the MCP-surface counterpart to the HTTP `/query` resource limits in
//! [`crate::http::config`]. The two surfaces are compiled under independent
//! feature flags (`mcp-server` vs `http-server`), so the semantic types are
//! deliberately duplicated here rather than shared — a shared module would
//! force one feature to pull in the other. The merge/ceiling semantics are kept
//! byte-for-byte consistent with the HTTP surface so an operator configuring
//! both surfaces gets identical behavior.
//!
//! Each of the three limit dimensions — **wall-clock timeout**, **result rows**,
//! and **result bytes** (the working-memory proxy for a read-only query, see the
//! coverage matrix in `docs/guides/mcp-query-tool.md`) — carries a server-level
//! **default**, an operator **ceiling** that a per-call override cannot exceed,
//! and a per-call **override**. A value of `0` on any dimension means
//! "unlimited"; a ceiling of `0` means "no ceiling". The master
//! [`enabled`](QueryLimitsConfig::enabled) switch turns all enforcement off.

use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

/// Default per-query wall-clock timeout, in milliseconds (30 s).
pub const DEFAULT_QUERY_TIMEOUT_MS: u64 = 30_000;
/// Operator hard ceiling for a per-call timeout override, in milliseconds (5 min).
pub const DEFAULT_MAX_QUERY_TIMEOUT_MS: u64 = 300_000;
/// Default cap on the number of rows returned by a single query.
///
/// Aligned with the pre-existing `MAX_RESULT_LIMIT` hard cap on the `query`
/// tool so enabling limits by default changes no existing behavior.
pub const DEFAULT_MAX_RESULT_ROWS: usize = 10_000;
/// Operator hard ceiling for a per-call `max_result_rows` override.
pub const DEFAULT_MAX_RESULT_ROWS_CEILING: usize = 100_000;
/// Default cap on the serialized response size of a single query, in bytes (8 MiB).
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Operator hard ceiling for a per-call `max_response_bytes` override (64 MiB).
pub const DEFAULT_MAX_RESPONSE_BYTES_CEILING: usize = 64 * 1024 * 1024;
/// Default cap on a query's estimated working memory, in bytes (Issue #3368
/// memory-budget dimension).
///
/// `0` = unlimited. This is **default-off**: unlike the timeout / row / byte
/// dimensions, the memory budget is not engaged under the default config, so an
/// operator must explicitly opt in. See the documented proxy definition in
/// `docs/guides/mcp-query-tool.md` — the "working memory attributable to a
/// query's evaluation" is a *proxy* (serialized response length scaled by an
/// in-memory expansion factor), honestly post-hoc exactly like the byte cap, not
/// true per-task allocation accounting.
pub const DEFAULT_MAX_QUERY_MEMORY_BYTES: usize = 0;
/// Operator hard ceiling for the memory budget (`0` = no ceiling). Default-off:
/// no ceiling in v1 (there is no per-call memory override to bound).
pub const DEFAULT_MAX_QUERY_MEMORY_BYTES_CEILING: usize = 0;
/// Default cap on the number of `query` calls concurrently occupying a
/// timeout-race worker thread (Issue #3368 DoS guard).
///
/// A configured wall-clock timeout runs each query on a dedicated, detached
/// worker thread (it cannot be cooperatively cancelled). Without a bound, a
/// caller sending tiny `timeout_ms` overrides — or simply hammering the
/// documented retriable-timeout retry loop — could pile up unbounded still-
/// running expensive queries and exhaust threads/CPU/memory. This caps the
/// number of live workers; at the cap a new timed query is rejected
/// `UNAVAILABLE` (retriable) instead of spawning. `0` = unbounded.
pub const DEFAULT_MAX_IN_FLIGHT_QUERIES: usize = 64;

/// Which per-query resource-limit dimension a value applies to (Issue #3368).
///
/// The [`as_str`](Self::as_str) token is the stable `details.dimension` value in
/// the structured error body, so callers can branch on it without string
/// matching the human message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitDimension {
    /// The wall-clock time budget for producing the response.
    WallClockTimeout,
    /// The number of rows in the result.
    ResultRows,
    /// The serialized byte size of the result (working-memory proxy).
    ResultBytes,
    /// The estimated working-set memory consumed materializing the result
    /// (Issue #3368 memory-budget dimension; a documented proxy, default-off).
    Memory,
}

impl LimitDimension {
    /// Stable snake_case token used in `error.details.dimension`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WallClockTimeout => "wall_clock_timeout",
            Self::ResultRows => "result_rows",
            Self::ResultBytes => "result_bytes",
            Self::Memory => "memory_bytes",
        }
    }
}

/// Per-call limit override carried on a `query` tool request under `"limits"`.
///
/// Every field is optional and additive, so existing request bodies that omit
/// `"limits"` parse and behave exactly as before. An override present but
/// *larger* than the operator ceiling is rejected (`INVALID_ARGUMENT`); an
/// override *smaller* than the server default is honored (a caller self-limiting
/// tighter). A value of `0` means "unlimited" for that dimension and is only
/// accepted when the operator ceiling is itself unbounded.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq, JsonSchema)]
pub struct QueryLimitsOverride {
    /// Per-call wall-clock timeout in milliseconds (`0` = unlimited).
    #[serde(default)]
    #[schemars(
        description = "Per-call wall-clock timeout in milliseconds (0 = unlimited). Rejected \
                              with INVALID_ARGUMENT if it exceeds the operator ceiling."
    )]
    pub timeout_ms: Option<u64>,
    /// Per-call maximum result rows (`0` = unlimited).
    #[serde(default)]
    #[schemars(
        description = "Per-call maximum result rows (0 = unlimited). Over-cap results are \
                              truncated with truncated:true, not rejected."
    )]
    pub max_result_rows: Option<usize>,
    /// Per-call maximum serialized response bytes (`0` = unlimited).
    #[serde(default)]
    #[schemars(
        description = "Per-call maximum serialized response bytes (0 = unlimited). Exceeding \
                              it fails closed with a RESOURCE_EXHAUSTED error."
    )]
    pub max_response_bytes: Option<usize>,
}

impl QueryLimitsOverride {
    /// True when no override fields are set (equivalent to no `"limits"` key).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.timeout_ms.is_none()
            && self.max_result_rows.is_none()
            && self.max_response_bytes.is_none()
    }
}

/// A per-call override that exceeded the operator hard ceiling (Issue #3368).
///
/// Maps to a `INVALID_ARGUMENT` query-tool error with
/// `details: {dimension, requested, ceiling}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitOverrideError {
    /// The dimension whose override was rejected.
    pub dimension: LimitDimension,
    /// The value the caller requested.
    pub requested: u64,
    /// The operator ceiling it exceeded.
    pub ceiling: u64,
}

/// The concrete limits in force for a single query, after folding the server
/// defaults, the per-call override, and the operator ceilings (Issue #3368). A
/// value of `0` on any dimension means "unlimited".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveQueryLimits {
    /// Wall-clock timeout in milliseconds (`0` = unlimited).
    pub timeout_ms: u64,
    /// Maximum result rows (`0` = unlimited).
    pub max_result_rows: usize,
    /// Maximum serialized response bytes (`0` = unlimited).
    pub max_response_bytes: usize,
    /// Maximum estimated working-set memory in bytes (`0` = unlimited; Issue
    /// #3368 memory-budget dimension, default-off).
    pub max_query_memory_bytes: usize,
}

impl EffectiveQueryLimits {
    /// No enforcement on any dimension.
    #[must_use]
    pub fn unlimited() -> Self {
        Self {
            timeout_ms: 0,
            max_result_rows: 0,
            max_response_bytes: 0,
            max_query_memory_bytes: 0,
        }
    }
}

/// Fold one dimension's server default, operator ceiling, and per-call override
/// into a single effective value.
///
/// Semantics (all in the dimension's native unit; `0` = unlimited, a ceiling of
/// `0` = no ceiling), kept consistent with `crate::http::config::merge_dimension`:
///
/// - **Override present**: rejected when a ceiling exists and the override
///   either requests unlimited (`0`) or exceeds the ceiling; otherwise the
///   override wins (it may be *below* the default — a tighter self-limit).
/// - **Override absent**: the server default, clamped down to the ceiling if the
///   default is unlimited or above it.
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

/// Server-level configuration for per-query resource limits on the MCP `query`
/// tool (Issue #3368).
///
/// Each dimension has a **default** (applied when the request supplies no
/// override), a **ceiling** (the largest value a per-call override may request;
/// `0` = no ceiling). A value of `0` on a default/override means "unlimited".
///
/// The master [`enabled`](Self::enabled) switch disables all enforcement when
/// `false` (see [`disabled`](Self::disabled)): the effective limits are then
/// unconditionally unlimited and per-call overrides are ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryLimitsConfig {
    /// Master switch. When `false`, no limit is enforced.
    pub enabled: bool,
    /// Default wall-clock timeout in milliseconds (`0` = unlimited).
    pub default_timeout_ms: u64,
    /// Operator ceiling for a per-call timeout override (`0` = no ceiling).
    pub max_timeout_ms: u64,
    /// Default maximum result rows (`0` = unlimited).
    pub default_max_result_rows: usize,
    /// Operator ceiling for a per-call row override (`0` = no ceiling).
    pub max_result_rows: usize,
    /// Default maximum serialized response bytes (`0` = unlimited).
    pub default_max_response_bytes: usize,
    /// Operator ceiling for a per-call byte override (`0` = no ceiling).
    pub max_response_bytes: usize,
    /// Default cap on a query's estimated working memory in bytes (`0` =
    /// unlimited; Issue #3368 memory-budget dimension, default-off).
    ///
    /// There is no per-call memory override in v1, so this default (clamped by
    /// [`max_query_memory_bytes`](Self::max_query_memory_bytes)) is the only
    /// value that ever applies.
    pub default_max_query_memory_bytes: usize,
    /// Operator ceiling for the memory budget (`0` = no ceiling; Issue #3368).
    pub max_query_memory_bytes: usize,
    /// Cap on the number of `query` calls concurrently occupying a
    /// timeout-race worker thread (Issue #3368 DoS guard; `0` = unbounded).
    ///
    /// This is a server-level (not per-call) bound: it is not part of
    /// [`EffectiveQueryLimits`] and cannot be overridden per request. It only
    /// affects the spawned-worker path taken when a wall-clock timeout is in
    /// force; the inline `timeout_ms == 0` path never spawns and is unguarded.
    pub max_in_flight_queries: usize,
}

impl QueryLimitsConfig {
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
            default_max_response_bytes: 0,
            max_response_bytes: 0,
            default_max_query_memory_bytes: 0,
            max_query_memory_bytes: 0,
            max_in_flight_queries: 0,
        }
    }

    /// Fold this config and an optional per-call override into the concrete
    /// [`EffectiveQueryLimits`] for one query.
    ///
    /// When [`enabled`](Self::enabled) is `false`, this always returns
    /// [`EffectiveQueryLimits::unlimited`] and never errors (overrides ignored).
    ///
    /// # Errors
    ///
    /// Returns [`LimitOverrideError`] if `override_` requests a value above the
    /// corresponding ceiling (or requests unlimited under a finite ceiling).
    pub fn effective(
        &self,
        override_: Option<&QueryLimitsOverride>,
    ) -> Result<EffectiveQueryLimits, LimitOverrideError> {
        if !self.enabled {
            return Ok(EffectiveQueryLimits::unlimited());
        }
        let ov = override_.filter(|o| !o.is_empty());
        let timeout_ms = merge_dimension(
            self.default_timeout_ms,
            self.max_timeout_ms,
            ov.and_then(|o| o.timeout_ms),
            LimitDimension::WallClockTimeout,
        )?;
        let max_result_rows = merge_dimension(
            self.default_max_result_rows as u64,
            self.max_result_rows as u64,
            ov.and_then(|o| o.max_result_rows).map(|v| v as u64),
            LimitDimension::ResultRows,
        )? as usize;
        let max_response_bytes = merge_dimension(
            self.default_max_response_bytes as u64,
            self.max_response_bytes as u64,
            ov.and_then(|o| o.max_response_bytes).map(|v| v as u64),
            LimitDimension::ResultBytes,
        )? as usize;
        // Memory budget has no per-call override in v1 (default-off, server
        // defaults only), so it is folded with `None` — only the default,
        // clamped down to a finite ceiling, ever applies.
        let max_query_memory_bytes = merge_dimension(
            self.default_max_query_memory_bytes as u64,
            self.max_query_memory_bytes as u64,
            None,
            LimitDimension::Memory,
        )? as usize;
        Ok(EffectiveQueryLimits {
            timeout_ms,
            max_result_rows,
            max_response_bytes,
            max_query_memory_bytes,
        })
    }
}

impl Default for QueryLimitsConfig {
    /// Enabled by default, but with generous limits chosen so no existing
    /// request behavior changes (Issue #3368).
    fn default() -> Self {
        Self {
            enabled: true,
            default_timeout_ms: DEFAULT_QUERY_TIMEOUT_MS,
            max_timeout_ms: DEFAULT_MAX_QUERY_TIMEOUT_MS,
            default_max_result_rows: DEFAULT_MAX_RESULT_ROWS,
            max_result_rows: DEFAULT_MAX_RESULT_ROWS_CEILING,
            default_max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES_CEILING,
            // Memory budget is default-off (unlimited) even under the default
            // config: an operator must explicitly opt in (Issue #3368).
            default_max_query_memory_bytes: DEFAULT_MAX_QUERY_MEMORY_BYTES,
            max_query_memory_bytes: DEFAULT_MAX_QUERY_MEMORY_BYTES_CEILING,
            max_in_flight_queries: DEFAULT_MAX_IN_FLIGHT_QUERIES,
        }
    }
}

/// Per-dimension counters of over-limit query terminations (Issue #3368
/// observability). Cheap atomics incremented at the MCP surface so an operator
/// (or a test) can spot chronic offenders. Surfacing these through
/// `database_stats` requires a storage-layer counter and is a cross-lane
/// follow-up; the MCP-surface accessor is the interim.
#[derive(Debug, Default)]
pub struct LimitCounters {
    /// Queries terminated by the wall-clock timeout.
    pub wall_clock_timeout: std::sync::atomic::AtomicU64,
    /// Responses rejected for exceeding the result-byte cap.
    pub result_bytes: std::sync::atomic::AtomicU64,
    /// Responses rejected for exceeding the estimated-memory budget (Issue
    /// #3368 memory-budget dimension).
    pub memory_bytes: std::sync::atomic::AtomicU64,
    /// Per-call overrides rejected for exceeding an operator ceiling.
    pub override_rejected: std::sync::atomic::AtomicU64,
}

/// An immutable snapshot of [`LimitCounters`] (Issue #3368).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitCountsSnapshot {
    /// Queries terminated by the wall-clock timeout.
    pub wall_clock_timeout: u64,
    /// Responses rejected for exceeding the result-byte cap.
    pub result_bytes: u64,
    /// Responses rejected for exceeding the estimated-memory budget (Issue
    /// #3368 memory-budget dimension).
    pub memory_bytes: u64,
    /// Per-call overrides rejected for exceeding an operator ceiling.
    pub override_rejected: u64,
}

impl LimitCounters {
    /// Increment the counter for `dimension` (a terminated query).
    pub fn record_termination(&self, dimension: LimitDimension) {
        use std::sync::atomic::Ordering;
        match dimension {
            LimitDimension::WallClockTimeout => {
                self.wall_clock_timeout.fetch_add(1, Ordering::Relaxed);
            }
            LimitDimension::ResultBytes => {
                self.result_bytes.fetch_add(1, Ordering::Relaxed);
            }
            LimitDimension::Memory => {
                self.memory_bytes.fetch_add(1, Ordering::Relaxed);
            }
            // Row-cap breaches degrade to a disclosed truncation, not a
            // termination, so they are not counted here.
            LimitDimension::ResultRows => {}
        }
    }

    /// Increment the rejected-override counter.
    pub fn record_override_rejected(&self) {
        self.override_rejected
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Read a consistent-enough snapshot of the counters.
    #[must_use]
    pub fn snapshot(&self) -> LimitCountsSnapshot {
        use std::sync::atomic::Ordering;
        LimitCountsSnapshot {
            wall_clock_timeout: self.wall_clock_timeout.load(Ordering::Relaxed),
            result_bytes: self.result_bytes.load(Ordering::Relaxed),
            memory_bytes: self.memory_bytes.load(Ordering::Relaxed),
            override_rejected: self.override_rejected.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_is_always_unlimited_and_ignores_overrides() {
        let cfg = QueryLimitsConfig::disabled();
        assert_eq!(
            cfg.effective(None).unwrap(),
            EffectiveQueryLimits::unlimited()
        );
        let over = QueryLimitsOverride {
            timeout_ms: Some(1),
            max_result_rows: Some(1),
            max_response_bytes: Some(1),
        };
        // Even an over-restrictive/over-large override is ignored when disabled.
        assert_eq!(
            cfg.effective(Some(&over)).unwrap(),
            EffectiveQueryLimits::unlimited()
        );
    }

    #[test]
    fn default_applies_server_defaults_with_no_override() {
        let cfg = QueryLimitsConfig::default();
        let eff = cfg.effective(None).unwrap();
        assert_eq!(eff.timeout_ms, DEFAULT_QUERY_TIMEOUT_MS);
        assert_eq!(eff.max_result_rows, DEFAULT_MAX_RESULT_ROWS);
        assert_eq!(eff.max_response_bytes, DEFAULT_MAX_RESPONSE_BYTES);
    }

    #[test]
    fn empty_override_is_equivalent_to_none() {
        let cfg = QueryLimitsConfig::default();
        let empty = QueryLimitsOverride::default();
        assert!(empty.is_empty());
        assert_eq!(
            cfg.effective(Some(&empty)).unwrap(),
            cfg.effective(None).unwrap()
        );
    }

    #[test]
    fn override_below_default_is_honored() {
        let cfg = QueryLimitsConfig::default();
        let over = QueryLimitsOverride {
            timeout_ms: Some(500),
            max_result_rows: Some(5),
            max_response_bytes: Some(1024),
        };
        let eff = cfg.effective(Some(&over)).unwrap();
        assert_eq!(eff.timeout_ms, 500);
        assert_eq!(eff.max_result_rows, 5);
        assert_eq!(eff.max_response_bytes, 1024);
    }

    #[test]
    fn override_above_ceiling_is_rejected_per_dimension() {
        let cfg = QueryLimitsConfig::default();
        for (over, dim, ceiling) in [
            (
                QueryLimitsOverride {
                    timeout_ms: Some(DEFAULT_MAX_QUERY_TIMEOUT_MS + 1),
                    ..Default::default()
                },
                LimitDimension::WallClockTimeout,
                DEFAULT_MAX_QUERY_TIMEOUT_MS,
            ),
            (
                QueryLimitsOverride {
                    max_result_rows: Some(DEFAULT_MAX_RESULT_ROWS_CEILING + 1),
                    ..Default::default()
                },
                LimitDimension::ResultRows,
                DEFAULT_MAX_RESULT_ROWS_CEILING as u64,
            ),
            (
                QueryLimitsOverride {
                    max_response_bytes: Some(DEFAULT_MAX_RESPONSE_BYTES_CEILING + 1),
                    ..Default::default()
                },
                LimitDimension::ResultBytes,
                DEFAULT_MAX_RESPONSE_BYTES_CEILING as u64,
            ),
        ] {
            let err = cfg.effective(Some(&over)).unwrap_err();
            assert_eq!(err.dimension, dim);
            assert_eq!(err.ceiling, ceiling);
        }
    }

    #[test]
    fn override_zero_under_finite_ceiling_is_rejected() {
        let cfg = QueryLimitsConfig::default();
        let over = QueryLimitsOverride {
            timeout_ms: Some(0),
            ..Default::default()
        };
        let err = cfg.effective(Some(&over)).unwrap_err();
        assert_eq!(err.dimension, LimitDimension::WallClockTimeout);
        assert_eq!(err.requested, 0);
    }

    #[test]
    fn override_zero_under_no_ceiling_means_unlimited() {
        let cfg = QueryLimitsConfig {
            max_timeout_ms: 0, // no ceiling
            ..QueryLimitsConfig::default()
        };
        let over = QueryLimitsOverride {
            timeout_ms: Some(0),
            ..Default::default()
        };
        assert_eq!(cfg.effective(Some(&over)).unwrap().timeout_ms, 0);
    }

    #[test]
    fn default_above_ceiling_is_clamped_to_ceiling() {
        let cfg = QueryLimitsConfig {
            default_timeout_ms: 1_000_000,
            max_timeout_ms: 500,
            ..QueryLimitsConfig::default()
        };
        assert_eq!(cfg.effective(None).unwrap().timeout_ms, 500);
    }

    #[test]
    fn counters_record_per_dimension() {
        let c = LimitCounters::default();
        c.record_termination(LimitDimension::WallClockTimeout);
        c.record_termination(LimitDimension::ResultBytes);
        c.record_termination(LimitDimension::ResultBytes);
        c.record_termination(LimitDimension::ResultRows); // not counted
        c.record_termination(LimitDimension::Memory);
        c.record_override_rejected();
        let s = c.snapshot();
        assert_eq!(s.wall_clock_timeout, 1);
        assert_eq!(s.result_bytes, 2);
        assert_eq!(s.memory_bytes, 1);
        assert_eq!(s.override_rejected, 1);
    }

    #[test]
    fn memory_dimension_token_is_stable() {
        assert_eq!(LimitDimension::Memory.as_str(), "memory_bytes");
    }

    #[test]
    fn memory_budget_is_off_under_default_and_disabled() {
        // Default-off: even the (enabled) default config leaves the memory
        // dimension unlimited, so no existing behavior changes.
        assert_eq!(
            QueryLimitsConfig::default()
                .effective(None)
                .unwrap()
                .max_query_memory_bytes,
            0
        );
        assert_eq!(
            QueryLimitsConfig::disabled()
                .effective(None)
                .unwrap()
                .max_query_memory_bytes,
            0
        );
    }

    #[test]
    fn memory_default_is_applied_when_opted_in() {
        let cfg = QueryLimitsConfig {
            default_max_query_memory_bytes: 4096,
            ..QueryLimitsConfig::default()
        };
        assert_eq!(cfg.effective(None).unwrap().max_query_memory_bytes, 4096);
    }

    #[test]
    fn memory_default_above_ceiling_is_clamped_to_ceiling() {
        let cfg = QueryLimitsConfig {
            default_max_query_memory_bytes: 1_000_000,
            max_query_memory_bytes: 4096, // finite ceiling
            ..QueryLimitsConfig::default()
        };
        assert_eq!(cfg.effective(None).unwrap().max_query_memory_bytes, 4096);
    }
}
