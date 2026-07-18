//! Temporal semantic drift alarms — the database watches its own embedding
//! evolution and fires durable, queryable alarms when meaning moves (Issue #3367).
//!
//! AletheiaDB is the only engine that keeps *time-versioned embeddings*
//! (temporal vector indexes, `src/index/vector/temporal/`). A vector database
//! overwrites the previous vector on update; a temporal database has no vector
//! distance semantics. Only an engine holding embedding *history* can detect
//! that a concept's meaning has drifted. This module turns that latent asset
//! into an active capability: declarative **drift monitors** watch an embedding
//! property against a threshold + time window, and materialize **drift alarms**
//! when the current embedding has moved too far from its past self.
//!
//! # Falsifiable firing rule (documented exactly)
//!
//! **Per-entity.** For entity `E` and monitor `M` with metric `d_M`, threshold
//! `θ`, and window `w`, let `e_now = embedding(E, now)` and
//! `e_past = embedding(E, now − w)` resolved through the temporal vector
//! history. An alarm fires **iff**:
//!
//! 1. both `e_now` and `e_past` exist (a version actually in-window), **and**
//! 2. `d_M(e_now, e_past) > θ` (strict `>`; exactly-at-threshold does **not**
//!    fire), **and**
//! 3. no unresolved alarm for `(M, E)` already exists (suppress until resolved).
//!
//! If `e_past` is missing (no version within the window) or `e_now` is missing,
//! the entity does **not** fire.
//!
//! **Label-centroid.** For a label-targeted monitor,
//! `centroid(t) =` the component-wise arithmetic mean of the property vector,
//! at time `t`, over every entity carrying `M.label` that *has* the vector at
//! `t` — iterated in ascending node-id order, skipping entities missing the
//! vector. For [`DriftMetric::Cosine`] the mean is **not** renormalized
//! (documented). The monitor fires iff
//! `d_M(centroid(now), centroid(now − w)) > θ` and no unresolved label alarm
//! for `M` exists. This detects population-level meaning shift even when no
//! single entity crosses `θ`.
//!
//! The chosen metric must be consistent with the property's vector-index metric;
//! a mismatch is rejected at monitor creation with a `#3234` `INVALID_ARGUMENT`.
//!
//! # Alarms are first-class bi-temporal graph nodes
//!
//! A fired alarm is materialized as a graph node under the reserved label
//! [`DRIFT_ALARM_LABEL`]. This buys, for free, everything AC4/AC5 demand:
//! durability (WAL), `AS OF` stability (append-only bi-temporal versions),
//! changefeed delivery (#3216 emits a `Created` record), and "resolve = a
//! recorded update, never a delete". Alarm history is therefore temporally
//! honest by construction: a later write never retroactively deletes an alarm,
//! and the alarm log for a past period is stable under `AS OF` inspection.
//!
//! # Write-path safety (zero hook)
//!
//! Evaluation never touches the commit path. The [`DriftAlarmEngine`] subscribes
//! to the existing changefeed (delivered *outside* every write-path lock) and
//! evaluates on a background thread; a saturated evaluation queue **sheds** work
//! (observable via [`DriftAlarmEngine::shed_count`]) rather than back-pressuring
//! commits. Scheduled monitors are driven by a background ticker. With the
//! `semantic-temporal` feature off, this entire module — and every
//! `AletheiaDB` accessor below — is compiled out, so there is zero write-path
//! overhead.
//!
//! # Worked example
//!
//! ```rust,no_run
//! # #[cfg(feature = "semantic-temporal")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use std::time::Duration;
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::index::vector::temporal::DriftMetric;
//! use aletheiadb::experimental::temporal::drift_alarm::{
//!     DriftMonitorSpec, DriftTarget, EvalMode, DriftAlarmFilter,
//! };
//!
//! let db = AletheiaDB::new()?;
//! let monitor = db.create_drift_monitor(DriftMonitorSpec {
//!     property_key: "embedding".to_string(),
//!     label: Some("Product".to_string()),
//!     entities: None,
//!     metric: DriftMetric::Cosine,
//!     threshold: 0.25,
//!     window: Duration::from_secs(7 * 24 * 3600),
//!     target: DriftTarget::PerEntity,
//!     mode: EvalMode::OnWrite,
//! })?;
//! // ... embeddings drift over time ...
//! let alarms = db.query_drift_alarms(&DriftAlarmFilter::for_monitor(monitor.id))?;
//! for a in &alarms {
//!     println!("entity {:?} drifted {} > {}", a.entity, a.measured_distance, a.threshold);
//! }
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "semantic-temporal"))]
//! # fn main() {}
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::AletheiaDB;
use crate::core::error::{Error, QueryError, Result};
use crate::core::id::{NodeId, VersionId};
use crate::core::temporal::Timestamp;
use crate::index::vector::temporal::DriftMetric;

/// Reserved node label under which fired drift alarms are materialized as
/// first-class bi-temporal graph nodes.
pub const DRIFT_ALARM_LABEL: &str = "__drift_alarm";

/// Default number of alarms returned by [`AletheiaDB::query_drift_alarms`] when
/// [`DriftAlarmFilter::limit`] is left at its default.
pub const DEFAULT_ALARM_QUERY_LIMIT: usize = 100;

/// Maximum number of alarms returnable in a single query; larger limits clamp.
pub const MAX_ALARM_QUERY_LIMIT: usize = 1000;

/// Stable identifier for a declared drift monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MonitorId(u64);

impl MonitorId {
    /// Wrap a raw monitor identifier.
    #[must_use]
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// The raw monitor identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// What a monitor watches: individual entities, or the label's centroid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum DriftTarget {
    /// Fire per entity whose own embedding drifted past the threshold.
    #[default]
    PerEntity,
    /// Fire once for the label when the population centroid drifted past the
    /// threshold, even if no single entity crossed it.
    LabelCentroid,
}

impl DriftTarget {
    /// Lowercase `snake_case` token used in JSON / MCP responses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DriftTarget::PerEntity => "per_entity",
            DriftTarget::LabelCentroid => "label_centroid",
        }
    }
}

/// When a monitor is evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum EvalMode {
    /// Evaluate reactively whenever a matching entity is written (driven by the
    /// changefeed subscription, off the write path).
    #[default]
    OnWrite,
    /// Evaluate on a fixed cadence driven by the background ticker.
    Scheduled {
        /// The evaluation interval.
        interval: Duration,
    },
}

/// Declarative definition of a drift monitor.
#[derive(Debug, Clone)]
pub struct DriftMonitorSpec {
    /// The vector property key to watch (e.g. `"embedding"`).
    pub property_key: String,
    /// Optional node-label restriction. Required for [`DriftTarget::LabelCentroid`].
    pub label: Option<String>,
    /// Optional explicit entity set. Mutually informative with `label`.
    pub entities: Option<Vec<NodeId>>,
    /// Distance metric; must be consistent with the property's index metric.
    pub metric: DriftMetric,
    /// Firing threshold (strict `>`); must be positive.
    pub threshold: f32,
    /// Comparison window: current embedding vs the embedding as of `now - window`.
    pub window: Duration,
    /// Per-entity vs label-centroid firing.
    pub target: DriftTarget,
    /// On-write vs scheduled evaluation.
    pub mode: EvalMode,
}

/// A registered drift monitor (its spec plus assigned id and creation time).
#[derive(Debug, Clone)]
pub struct DriftMonitor {
    /// Assigned monitor id.
    pub id: MonitorId,
    /// The declaration.
    pub spec: DriftMonitorSpec,
    /// When the monitor was created (transaction time).
    pub created_at: Timestamp,
}

/// A durable, first-class drift alarm record.
///
/// Materialized as a graph node under [`DRIFT_ALARM_LABEL`]; `alarm_id` is that
/// node's id, so the caller can `get_node` it and follow `from_version` /
/// `to_version` to pull both compared embedding versions (subject to vector
/// elision defaults, #3220).
#[derive(Debug, Clone)]
pub struct DriftAlarm {
    /// The graph node id of the materialized alarm record.
    pub alarm_id: NodeId,
    /// The monitor that fired.
    pub monitor_id: MonitorId,
    /// The entity that drifted (for [`DriftTarget::PerEntity`]).
    pub entity: Option<NodeId>,
    /// The label whose centroid drifted (for [`DriftTarget::LabelCentroid`]).
    pub label: Option<String>,
    /// Measured distance that crossed the threshold.
    pub measured_distance: f32,
    /// The threshold at fire time.
    pub threshold: f32,
    /// The metric used.
    pub metric: DriftMetric,
    /// Bi-temporal coordinate of the "now" embedding compared.
    pub compared_now: Timestamp,
    /// Bi-temporal coordinate of the "past" (`now - window`) embedding compared.
    pub compared_past: Timestamp,
    /// Version reference for the "past" embedding.
    pub from_version: Option<VersionId>,
    /// Version reference for the "now" embedding.
    pub to_version: Option<VersionId>,
    /// Whether the alarm has been resolved (a recorded, `AS OF`-stable update).
    pub resolved: bool,
    /// When the alarm fired (transaction time).
    pub fired_at: Timestamp,
}

/// Filter for [`AletheiaDB::query_drift_alarms`].
#[derive(Debug, Clone)]
pub struct DriftAlarmFilter {
    /// Restrict to a single monitor.
    pub monitor_id: Option<MonitorId>,
    /// Restrict to a label.
    pub label: Option<String>,
    /// Restrict by resolved state (`None` = both).
    pub resolved: Option<bool>,
    /// Restrict to alarms fired within `[start, end)` transaction time.
    pub time_range: Option<(Timestamp, Timestamp)>,
    /// Maximum alarms to return (clamped to [`MAX_ALARM_QUERY_LIMIT`]).
    pub limit: usize,
}

impl Default for DriftAlarmFilter {
    fn default() -> Self {
        Self {
            monitor_id: None,
            label: None,
            resolved: None,
            time_range: None,
            limit: DEFAULT_ALARM_QUERY_LIMIT,
        }
    }
}

impl DriftAlarmFilter {
    /// A filter selecting all alarms for a single monitor.
    #[must_use]
    pub fn for_monitor(monitor_id: MonitorId) -> Self {
        Self {
            monitor_id: Some(monitor_id),
            ..Self::default()
        }
    }
}

/// An intended firing produced by the pure evaluation core, *before* it is
/// persisted as a [`DriftAlarm`] node.
#[derive(Debug, Clone)]
pub struct DriftFiring {
    /// The entity that drifted (per-entity firing).
    pub entity: Option<NodeId>,
    /// The label whose centroid drifted (label-centroid firing).
    pub label: Option<String>,
    /// The measured distance that crossed the threshold.
    pub measured_distance: f32,
    /// Bi-temporal coordinate of the "now" embedding.
    pub compared_now: Timestamp,
    /// Bi-temporal coordinate of the "past" embedding.
    pub compared_past: Timestamp,
    /// Version reference for the "past" embedding.
    pub from_version: Option<VersionId>,
    /// Version reference for the "now" embedding.
    pub to_version: Option<VersionId>,
}

// ---------------------------------------------------------------------------
// Pure evaluation core (deterministic; unit-tested with hand-built fixtures).
// ---------------------------------------------------------------------------

/// Distance between two embeddings under `metric`.
///
/// `Cosine` = `1 - cosine_similarity`, `Euclidean` = L2 distance, `Angular` =
/// `arccos(cosine_similarity)` (radians). Mirrors the temporal vector index's
/// private `compute_drift_distance` so alarm distances match drift metrics.
///
/// # Errors
///
/// `INVALID_ARGUMENT` on a dimension mismatch or a zero-magnitude vector for a
/// cosine/angular metric.
pub fn metric_distance(a: &[f32], b: &[f32], metric: DriftMetric) -> Result<f32> {
    let _ = (a, b, metric);
    todo!("Stage B (#3367): metric distance computation")
}

/// Deterministic component-wise arithmetic mean over `vectors`.
///
/// Iterated in caller-provided order (callers pass entities sorted by node id).
/// Returns `None` when `vectors` is empty. For a cosine metric the result is
/// **not** renormalized (documented firing-rule contract).
#[must_use]
pub fn centroid(vectors: &[&[f32]]) -> Option<Vec<f32>> {
    let _ = vectors;
    todo!("Stage B (#3367): deterministic component-wise centroid")
}

/// Pure per-entity firing decision for one `(now, past)` embedding pair.
///
/// Returns `Some(distance)` iff both embeddings are present, the strict
/// threshold is crossed, and no unresolved alarm already suppresses the fire;
/// otherwise `None`.
///
/// # Errors
///
/// Propagates [`metric_distance`] errors.
pub fn decide_entity_firing(
    e_now: Option<&[f32]>,
    e_past: Option<&[f32]>,
    metric: DriftMetric,
    threshold: f32,
    has_unresolved: bool,
) -> Result<Option<f32>> {
    let _ = (e_now, e_past, metric, threshold, has_unresolved);
    todo!("Stage B (#3367): pure per-entity firing decision")
}

/// Evaluate a monitor against the database as of `now`, producing the set of
/// intended firings (pre-persistence). Deterministic given the stored history.
///
/// # Errors
///
/// - `INVALID_ARGUMENT` for an invalid spec surfaced at evaluation.
/// - `NOT_FOUND` if the watched property/index is missing.
pub fn evaluate_monitor(
    db: &AletheiaDB,
    monitor: &DriftMonitor,
    now: Timestamp,
) -> Result<Vec<DriftFiring>> {
    let _ = (db, monitor, now);
    todo!("Stage B (#3367): monitor evaluation over temporal vector history")
}

/// Build an `INVALID_ARGUMENT`-mapped error (`QueryError::InvalidParameter`).
#[allow(dead_code)] // Stage B (#3367): used by monitor validation in the green impl.
fn invalid_argument(parameter: &str, reason: impl Into<String>) -> Error {
    Error::Query(QueryError::InvalidParameter {
        parameter: parameter.to_string(),
        reason: reason.into(),
    })
}

// ---------------------------------------------------------------------------
// Background driver (Stage B fleshes out subscription + queue + shedding).
// ---------------------------------------------------------------------------

/// Background evaluator: subscribes to the changefeed for on-write monitors,
/// ticks scheduled monitors, and persists fired alarms — all off the write
/// path, shedding on queue saturation.
pub struct DriftAlarmEngine {
    db: Arc<AletheiaDB>,
    shed_count: Arc<AtomicU64>,
}

impl DriftAlarmEngine {
    /// Create an engine bound to `db` (not yet running; call [`start`]).
    ///
    /// [`start`]: DriftAlarmEngine::start
    #[must_use]
    pub fn new(db: Arc<AletheiaDB>) -> Self {
        Self {
            db,
            shed_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Start the background subscription + ticker.
    ///
    /// # Errors
    ///
    /// Fails if the changefeed subscription cannot be established.
    pub fn start(&self) -> Result<()> {
        // Stage B (#3367): subscribe_changes(reserved-label-filtered) + worker.
        let _ = &self.db;
        todo!("Stage B (#3367): drift alarm background driver")
    }

    /// Stop the background driver and deregister the subscription.
    pub fn stop(&self) {
        // Stage B (#3367): signal the worker to drain and join.
        todo!("Stage B (#3367): drift alarm driver shutdown")
    }

    /// Number of evaluation tasks shed due to queue saturation (observable
    /// write-path-safety counter; monotonic for the engine's lifetime).
    #[must_use]
    pub fn shed_count(&self) -> u64 {
        self.shed_count.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Feature-gated AletheiaDB accessors (present only under `semantic-temporal`).
// ---------------------------------------------------------------------------

impl AletheiaDB {
    /// Create and register a drift monitor (Issue #3367).
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for an unknown property, a metric inconsistent with
    /// the property's index metric, a non-positive threshold, or a zero window.
    pub fn create_drift_monitor(&self, spec: DriftMonitorSpec) -> Result<DriftMonitor> {
        let _ = spec;
        todo!("Stage B (#3367): validate + register drift monitor")
    }

    /// List all registered drift monitors.
    pub fn list_drift_monitors(&self) -> Vec<DriftMonitor> {
        todo!("Stage B (#3367): list drift monitors")
    }

    /// Fetch a single monitor by id.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` for an unknown monitor id.
    pub fn get_drift_monitor(&self, id: MonitorId) -> Result<DriftMonitor> {
        let _ = id;
        todo!("Stage B (#3367): get drift monitor")
    }

    /// Delete a monitor, removing it from future evaluation.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` for an unknown monitor id.
    pub fn delete_drift_monitor(&self, id: MonitorId) -> Result<()> {
        let _ = id;
        todo!("Stage B (#3367): delete drift monitor")
    }

    /// Evaluate a monitor immediately (synchronous), persisting any fired
    /// alarms and returning them. Used for scheduled cadence and tests.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` for an unknown monitor; evaluation/persistence errors.
    pub fn evaluate_drift_monitor_now(&self, id: MonitorId) -> Result<Vec<DriftAlarm>> {
        let _ = id;
        todo!("Stage B (#3367): synchronous monitor evaluation + persistence")
    }

    /// Query durable drift alarms by monitor / label / resolved state / time
    /// range.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for a malformed filter.
    pub fn query_drift_alarms(&self, filter: &DriftAlarmFilter) -> Result<Vec<DriftAlarm>> {
        let _ = filter;
        todo!("Stage B (#3367): query drift alarms")
    }

    /// Resolve an alarm — a recorded, `AS OF`-stable bi-temporal update (sets
    /// `resolved = true` with a resolution transaction time), never a delete.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` if `alarm_id` is not a drift-alarm node.
    pub fn resolve_drift_alarm(&self, alarm_id: NodeId) -> Result<DriftAlarm> {
        let _ = alarm_id;
        todo!("Stage B (#3367): resolve drift alarm as a recorded update")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(n: u64) -> NodeId {
        NodeId::new(n).expect("valid node id")
    }

    fn ts(micros: i64) -> Timestamp {
        Timestamp::new(micros, 0).expect("valid timestamp")
    }

    // -- id / token round-trips (do not depend on the todo!() core) ---------

    #[test]
    fn monitor_id_round_trips() {
        assert_eq!(MonitorId::new(42).get(), 42);
    }

    #[test]
    fn drift_target_tokens_are_stable() {
        assert_eq!(DriftTarget::PerEntity.as_str(), "per_entity");
        assert_eq!(DriftTarget::LabelCentroid.as_str(), "label_centroid");
    }

    #[test]
    fn filter_defaults_and_for_monitor() {
        let f = DriftAlarmFilter::default();
        assert_eq!(f.limit, DEFAULT_ALARM_QUERY_LIMIT);
        assert!(f.monitor_id.is_none());
        let f2 = DriftAlarmFilter::for_monitor(MonitorId::new(7));
        assert_eq!(f2.monitor_id, Some(MonitorId::new(7)));
    }

    // -- metric correctness (case 8): hand-computable small vectors ----------

    #[test]
    fn metric_distance_cosine_orthogonal_is_one() {
        // cos([1,0,0],[0,1,0]) = 0 -> cosine distance = 1.0
        let a = [1.0f32, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0];
        let d = metric_distance(&a, &b, DriftMetric::Cosine).expect("distance");
        assert!(
            (d - 1.0).abs() < 1e-6,
            "cosine orthogonal distance = 1.0, got {d}"
        );
    }

    #[test]
    fn metric_distance_cosine_identical_is_zero() {
        let a = [0.6f32, 0.8, 0.0];
        let d = metric_distance(&a, &a, DriftMetric::Cosine).expect("distance");
        assert!(d.abs() < 1e-6, "identical cosine distance = 0.0, got {d}");
    }

    #[test]
    fn metric_distance_euclidean_is_l2() {
        // ||[0,0,0]-[3,4,0]|| = 5
        let a = [0.0f32, 0.0, 0.0];
        let b = [3.0f32, 4.0, 0.0];
        let d = metric_distance(&a, &b, DriftMetric::Euclidean).expect("distance");
        assert!((d - 5.0).abs() < 1e-6, "euclidean distance = 5.0, got {d}");
    }

    #[test]
    fn metric_distance_angular_orthogonal_is_half_pi() {
        // arccos(0) = pi/2
        let a = [1.0f32, 0.0];
        let b = [0.0f32, 1.0];
        let d = metric_distance(&a, &b, DriftMetric::Angular).expect("distance");
        assert!(
            (d - std::f32::consts::FRAC_PI_2).abs() < 1e-6,
            "angular orthogonal distance = pi/2, got {d}"
        );
    }

    // -- centroid determinism (case 9) --------------------------------------

    #[test]
    fn centroid_of_empty_is_none() {
        let empty: [&[f32]; 0] = [];
        assert!(centroid(&empty).is_none());
    }

    #[test]
    fn centroid_single_entity_equals_that_entity() {
        let v = [0.2f32, 0.4, 0.6];
        let refs: [&[f32]; 1] = [&v];
        let c = centroid(&refs).expect("centroid");
        assert_eq!(c, vec![0.2, 0.4, 0.6]);
    }

    #[test]
    fn centroid_is_component_wise_arithmetic_mean_not_renormalized() {
        // mean([1,0,0],[0,1,0]) = [0.5, 0.5, 0] (deliberately NOT renormalized)
        let a = [1.0f32, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0];
        let refs: [&[f32]; 2] = [&a, &b];
        let c = centroid(&refs).expect("centroid");
        assert_eq!(c, vec![0.5, 0.5, 0.0]);
    }

    // -- pure firing decision (cases 1,2,3,5,6) -----------------------------

    #[test]
    fn firing_below_threshold_does_not_fire() {
        // cosine distance between near-identical unit vectors ~ 0 < 0.5
        let now = [1.0f32, 0.0, 0.0];
        let past = [0.9999f32, 0.01414, 0.0];
        let out = decide_entity_firing(Some(&now), Some(&past), DriftMetric::Cosine, 0.5, false)
            .expect("decision");
        assert!(out.is_none(), "sub-threshold must not fire");
    }

    #[test]
    fn firing_exactly_at_threshold_does_not_fire_strict_gt() {
        // orthogonal -> cosine distance exactly 1.0; threshold 1.0; strict > => no fire
        let now = [1.0f32, 0.0, 0.0];
        let past = [0.0f32, 1.0, 0.0];
        let out = decide_entity_firing(Some(&now), Some(&past), DriftMetric::Cosine, 1.0, false)
            .expect("decision");
        assert!(
            out.is_none(),
            "exactly-at-threshold must not fire (strict >)"
        );
    }

    #[test]
    fn firing_above_threshold_fires_with_distance() {
        // orthogonal -> distance 1.0 > threshold 0.5 => fire, distance reported
        let now = [1.0f32, 0.0, 0.0];
        let past = [0.0f32, 1.0, 0.0];
        let out = decide_entity_firing(Some(&now), Some(&past), DriftMetric::Cosine, 0.5, false)
            .expect("decision");
        let d = out.expect("must fire above threshold");
        assert!((d - 1.0).abs() < 1e-6, "reported distance = 1.0, got {d}");
    }

    #[test]
    fn firing_suppressed_while_unresolved_alarm_exists() {
        // above threshold but an unresolved alarm exists => no re-fire (case 4)
        let now = [1.0f32, 0.0, 0.0];
        let past = [0.0f32, 1.0, 0.0];
        let out = decide_entity_firing(Some(&now), Some(&past), DriftMetric::Cosine, 0.5, true)
            .expect("decision");
        assert!(
            out.is_none(),
            "must not re-fire while unresolved alarm exists"
        );
    }

    #[test]
    fn firing_no_past_embedding_does_not_fire() {
        // case 5: no version in-window => no fire even though "now" is very different
        let now = [1.0f32, 0.0, 0.0];
        let out = decide_entity_firing(Some(&now), None, DriftMetric::Cosine, 0.5, false)
            .expect("decision");
        assert!(out.is_none(), "missing past embedding must not fire");
    }

    #[test]
    fn firing_no_current_embedding_does_not_fire() {
        // case 6: no current embedding => no fire
        let past = [0.0f32, 1.0, 0.0];
        let out = decide_entity_firing(None, Some(&past), DriftMetric::Cosine, 0.5, false)
            .expect("decision");
        assert!(out.is_none(), "missing current embedding must not fire");
    }

    // -- validation (cases 11, 12) via the invalid_argument mapping ----------
    // Full spec validation (unknown property / metric mismatch) is exercised in
    // tests/drift_alarm_e2e.rs against the real write path. Here we assert the
    // pure evaluation core rejects a nonsensical monitor deterministically.

    #[test]
    fn evaluate_monitor_smoke_uses_now() {
        // Ensures the signature is stable; against the todo!() skeleton this
        // panics (RED). Kept as a compile-anchor + Stage B target.
        let db = AletheiaDB::new().expect("db");
        let spec = DriftMonitorSpec {
            property_key: "embedding".to_string(),
            label: Some("Doc".to_string()),
            entities: Some(vec![nid(1)]),
            metric: DriftMetric::Cosine,
            threshold: 0.5,
            window: Duration::from_secs(3600),
            target: DriftTarget::PerEntity,
            mode: EvalMode::OnWrite,
        };
        let monitor = DriftMonitor {
            id: MonitorId::new(1),
            spec,
            created_at: ts(1000),
        };
        let firings = evaluate_monitor(&db, &monitor, ts(10_000)).expect("evaluate");
        assert!(firings.is_empty(), "no in-window history => no firings");
    }
}
