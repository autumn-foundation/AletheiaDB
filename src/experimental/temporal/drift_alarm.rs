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
//! `θ`, and window `w`, let `e_now = embedding(E, now)` be `E`'s **current**
//! embedding and `e_past = embedding(E, now − w)` the embedding **on record as
//! of transaction-time `now − w`** (read at the current valid coordinate). An
//! alarm fires **iff**:
//!
//! 1. both `e_now` and `e_past` exist, **and**
//! 2. `d_M(e_now, e_past) > θ` (strict `>`; exactly-at-threshold does **not**
//!    fire), **and**
//! 3. no unresolved alarm for `(M, E)` already exists (suppress until resolved).
//!
//! `e_now` is missing when `E` has no current embedding (never had one, or the
//! property was removed) — a since-dropped embedding never fires. `e_past` is
//! missing when `E` had no embedding on record at `now − w` (created after that
//! instant, or the property was absent then). Either missing endpoint means the
//! entity does **not** fire. The comparison is the literal
//! `d_M(embedding(E, now), embedding(E, now − w))`; the anchor is reconstructed
//! at the exact coordinate `now − w`, so `window` keeps its declared meaning
//! (how far back the past endpoint sits) rather than degrading to an
//! "earliest-write-within-the-window" heuristic. The past anchor is on the
//! **transaction-time** axis (see [`evaluate_monitor`]): the engine's
//! system-time-versioned storage cannot recall a superseded embedding along
//! valid time at `tx = now`, so transaction-time as-of `now − w` is the
//! faithful "what was the embedding `w` ago".
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
/// # Zero-magnitude vectors
///
/// For a cosine/angular metric, [`cosine_similarity`](crate::core::vector::cosine_similarity)
/// returns `Ok(0.0)` for a (near-)zero-magnitude vector rather than erroring, so
/// this function returns cosine distance `1.0` (angular `π/2`) — a short-circuit
/// artifact, not a true distance. Callers that could form a zero-magnitude
/// operand (e.g. a label centroid whose normalized members cancel) must guard
/// it as "no comparison" *before* calling; the drift evaluator does so via
/// [`cosine_family_zero_guard`].
///
/// # Errors
///
/// `INVALID_ARGUMENT` on a dimension mismatch.
pub fn metric_distance(a: &[f32], b: &[f32], metric: DriftMetric) -> Result<f32> {
    // Mirror the temporal vector index's private `compute_drift_distance`
    // (`src/index/vector/temporal/mod.rs`) exactly, so an alarm's measured
    // distance equals what `find_semantic_drift` would report for the same pair.
    use crate::core::vector::{cosine_similarity, euclidean_distance};
    match metric {
        DriftMetric::Cosine => {
            let similarity = cosine_similarity(a, b)?;
            Ok(1.0 - similarity)
        }
        DriftMetric::Euclidean => euclidean_distance(a, b),
        DriftMetric::Angular => {
            let similarity = cosine_similarity(a, b)?;
            Ok(similarity.clamp(-1.0, 1.0).acos())
        }
    }
}

/// Deterministic component-wise arithmetic mean over `vectors`.
///
/// Iterated in caller-provided order (callers pass entities sorted by node id).
/// Returns `None` when `vectors` is empty. For a cosine metric the result is
/// **not** renormalized (documented firing-rule contract).
#[must_use]
pub fn centroid(vectors: &[&[f32]]) -> Option<Vec<f32>> {
    let first = vectors.first()?;
    let dim = first.len();
    // Component-wise sum in the caller-provided (node-id-sorted) order, then
    // divide by count. Deliberately NOT renormalized: the firing rule compares
    // raw arithmetic means so a fixture is hand-computable. A vector whose
    // dimension does not match the first is skipped (defensive; all vectors of
    // one index share a dimension in practice).
    let mut sum = vec![0.0f32; dim];
    let mut count = 0usize;
    for v in vectors {
        if v.len() != dim {
            continue;
        }
        for (acc, x) in sum.iter_mut().zip(v.iter()) {
            *acc += *x;
        }
        count += 1;
    }
    if count == 0 {
        return None;
    }
    let inv = 1.0 / count as f32;
    for acc in &mut sum {
        *acc *= inv;
    }
    Some(sum)
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
    // Rule 3: an unresolved alarm for (M, E) suppresses re-firing.
    if has_unresolved {
        return Ok(None);
    }
    // Rule 1: both endpoints must exist (a version actually in-window).
    let (Some(now), Some(past)) = (e_now, e_past) else {
        return Ok(None);
    };
    let distance = metric_distance(now, past, metric)?;
    // Rule 2: strict `>` — exactly-at-threshold does NOT fire.
    if distance > threshold {
        Ok(Some(distance))
    } else {
        Ok(None)
    }
}

/// Evaluate a monitor against the database as of `now`, producing the set of
/// intended firings (pre-persistence). Deterministic given the stored history.
///
/// # Firing endpoints (literal rule)
///
/// The two compared embeddings are reconstructed through the point-in-time
/// history path, implementing the falsifiable rule
/// `distance(embedding(E, now), embedding(E, now − window))`:
///
/// - **`e_now`** is the entity's **current** embedding (the current-state value
///   of the vector property) together with its current [`VersionId`]. An entity
///   with no current embedding (never had one, or the property was removed) does
///   not fire.
/// - **`e_past`** is the embedding **on record as of transaction-time
///   `now − window`** — i.e. "what was the embedding, `window` ago" — read via
///   `get_node_at_time(E, valid = now, tx = now − window)`. If the entity had no
///   embedding on record at `now − window` (created after that instant, or the
///   property was absent then) `e_past` is MISSING and the entity does not fire.
///   We deliberately do **not** substitute "the earliest version within the
///   window", so `window` keeps its declared meaning: the exact lookback
///   coordinate of the past endpoint.
///
/// **Reconstruction dimension (transaction-time as-of `now − window`).** The
/// engine's historical storage is system-time-versioned: an update supersedes
/// the prior version by *closing its transaction interval* while its valid
/// interval stays open. A superseded version is therefore invisible at
/// `tx = now` for any past valid coordinate — a *valid-time* as-of `now − window`
/// at `tx = now` can never return a superseded embedding, only the current one
/// (or nothing). The embedding that was current `window` ago is recoverable only
/// along the **transaction-time** axis, so `e_past` is reconstructed at
/// `tx = now − window` (at the current valid coordinate `now`). This is the
/// faithful reading of "the embedding as it was `window` ago" for this engine
/// and, unlike the old earliest-in-window heuristic, keeps `window` meaning
/// exactly how far back the comparison anchor sits.
///
/// `from_version` is the version supplying `e_past`; `to_version` is the current
/// version. The firing's `compared_now` is the evaluation instant `now`;
/// `compared_past` is the transaction-time anchor `now − window` actually used.
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
    // The past comparison anchor is the transaction-time coordinate
    // `now - window`; `e_past` is the embedding on record then (see the
    // reconstruction-dimension note above), read at the current valid coord.
    // Checked conversion (robustness MINOR-5): `Duration::as_micros()` is `u128`;
    // an out-of-range window saturates to `i128::MAX` rather than wrapping to a
    // negative value that would invert the lookback window.
    let window_micros = i128::try_from(monitor.spec.window.as_micros()).unwrap_or(i128::MAX);
    let now_wall = i128::from(now.wallclock());
    let past_wall = now_wall.saturating_sub(window_micros);
    let past_tx = Timestamp::new(clamp_micros(past_wall), 0).unwrap_or_else(|_| Timestamp::from(0));
    let key = monitor.spec.property_key.as_str();

    match monitor.spec.target {
        DriftTarget::PerEntity => {
            let mut firings = Vec::new();
            for node in monitor_entities(db, &monitor.spec) {
                let (Some(e_now), Some(e_past)) = (
                    entity_current_embedding(db, node, key),
                    entity_past_embedding(db, node, now, past_tx, key),
                ) else {
                    continue;
                };
                // A zero-magnitude operand under cosine/angular reads as a
                // short-circuit artifact (distance 1.0), not a real drift.
                if cosine_family_zero_guard(
                    monitor.spec.metric,
                    &e_now.embedding,
                    &e_past.embedding,
                ) {
                    continue;
                }
                let decision = decide_entity_firing(
                    Some(&e_now.embedding),
                    Some(&e_past.embedding),
                    monitor.spec.metric,
                    monitor.spec.threshold,
                    // Purity: dedup against unresolved alarms happens at
                    // persistence time (`evaluate_drift_monitor_now`), not here.
                    false,
                )?;
                if let Some(distance) = decision {
                    firings.push(DriftFiring {
                        entity: Some(node),
                        label: None,
                        measured_distance: distance,
                        compared_now: now,
                        compared_past: past_tx,
                        from_version: Some(e_past.version),
                        to_version: Some(e_now.version),
                    });
                }
            }
            Ok(firings)
        }
        DriftTarget::LabelCentroid => {
            // Population centroids over ALL label members that have the property
            // at each time `t`: the now-centroid over every member with a current
            // embedding, the past-centroid over every member whose embedding
            // reconstructs at `now - window`. The two member sets need not
            // coincide (a member static over the window, or created inside it,
            // contributes to only one) — this is the documented AC3 population,
            // NOT only members with two in-window versions. Members missing the
            // vector at that coordinate are skipped; node-id-sorted iteration;
            // the mean is not renormalized (see [`centroid`]).
            let mut now_vecs: Vec<Vec<f32>> = Vec::new();
            let mut past_vecs: Vec<Vec<f32>> = Vec::new();
            for node in monitor_entities(db, &monitor.spec) {
                if let Some(e_now) = entity_current_embedding(db, node, key) {
                    now_vecs.push(e_now.embedding);
                }
                if let Some(e_past) = entity_past_embedding(db, node, now, past_tx, key) {
                    past_vecs.push(e_past.embedding);
                }
            }
            let now_refs: Vec<&[f32]> = now_vecs.iter().map(Vec::as_slice).collect();
            let past_refs: Vec<&[f32]> = past_vecs.iter().map(Vec::as_slice).collect();
            let (Some(c_now), Some(c_past)) = (centroid(&now_refs), centroid(&past_refs)) else {
                // A centroid with no contributing members: no comparison, no fire.
                return Ok(Vec::new());
            };
            // A zero-magnitude centroid (normalized members cancelling) is not a
            // real drift signal under cosine/angular: treat as no comparison.
            if cosine_family_zero_guard(monitor.spec.metric, &c_now, &c_past) {
                return Ok(Vec::new());
            }
            let decision = decide_entity_firing(
                Some(&c_now),
                Some(&c_past),
                monitor.spec.metric,
                monitor.spec.threshold,
                false,
            )?;
            match decision {
                Some(distance) => Ok(vec![DriftFiring {
                    entity: None,
                    label: monitor.spec.label.clone(),
                    measured_distance: distance,
                    compared_now: now,
                    compared_past: past_tx,
                    from_version: None,
                    to_version: None,
                }]),
                None => Ok(Vec::new()),
            }
        }
    }
}

/// A resolved endpoint embedding for one entity at one bi-temporal coordinate,
/// carrying the version that supplied it (for the alarm's version refs).
struct EndpointEmbedding {
    /// The reconstructed embedding.
    embedding: Vec<f32>,
    /// The version that supplied the embedding.
    version: VersionId,
}

/// Clamp a 128-bit micros value back into `i64` range for `Timestamp`.
fn clamp_micros(v: i128) -> i64 {
    v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// Squared-magnitude floor below which a vector is treated as zero for
/// cosine/angular comparison, mirroring
/// [`cosine_similarity`](crate::core::vector::cosine_similarity)'s own
/// short-circuit (it returns `Ok(0.0)` — i.e. distance `1.0` — for such a
/// vector). Used to suppress a spurious max-drift reading from a zero-magnitude
/// embedding or centroid.
const ZERO_MAGNITUDE_SQUARED: f32 = 1e-30;

/// `true` when `metric` is cosine-family AND either operand is (near) zero
/// magnitude, so comparing them would be a `cosine_similarity` short-circuit
/// artifact rather than a real distance. Euclidean has no zero-norm hazard, so
/// it never guards.
fn cosine_family_zero_guard(metric: DriftMetric, a: &[f32], b: &[f32]) -> bool {
    if !matches!(metric, DriftMetric::Cosine | DriftMetric::Angular) {
        return false;
    }
    is_near_zero_magnitude(a) || is_near_zero_magnitude(b)
}

/// Whether `v`'s squared magnitude is at/below the zero floor.
fn is_near_zero_magnitude(v: &[f32]) -> bool {
    v.iter().map(|x| x * x).sum::<f32>() <= ZERO_MAGNITUDE_SQUARED
}

/// The entities a monitor watches, in ascending node-id order: the explicit
/// `entities` set if given, else every node carrying `label`, else empty.
fn monitor_entities(db: &AletheiaDB, spec: &DriftMonitorSpec) -> Vec<NodeId> {
    let mut ids: Vec<NodeId> = if let Some(entities) = &spec.entities {
        entities.clone()
    } else if let Some(label) = &spec.label {
        db.scan_nodes_by_label(label).collect()
    } else {
        Vec::new()
    };
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// The entity's **current** embedding (current-state value of `property_key`)
/// plus its current version id, or `None` when the entity has no current
/// embedding (does not currently exist, or the property is absent/non-vector).
/// This is the literal `embedding(E, now)`.
fn entity_current_embedding(
    db: &AletheiaDB,
    node: NodeId,
    property_key: &str,
) -> Option<EndpointEmbedding> {
    let current = db.get_node(node).ok()?;
    let Some(crate::core::property::PropertyValue::Vector(v)) =
        current.properties.get(property_key)
    else {
        return None;
    };
    Some(EndpointEmbedding {
        embedding: v.to_vec(),
        version: current.current_version,
    })
}

/// The entity's embedding **on record as of transaction-time `past_tx`** (read
/// at the current valid coordinate `valid_now`), plus the version that was
/// current at that transaction time. `None` when the entity had no embedding on
/// record then (created after `past_tx`, property absent, or non-vector). This
/// is the literal `embedding(E, now − window)` for this engine — see the
/// reconstruction-dimension note on [`evaluate_monitor`] for why the past anchor
/// is the transaction-time, not the valid-time, axis.
fn entity_past_embedding(
    db: &AletheiaDB,
    node: NodeId,
    valid_now: Timestamp,
    past_tx: Timestamp,
    property_key: &str,
) -> Option<EndpointEmbedding> {
    let past = db.get_node_at_time(node, valid_now, past_tx).ok()?;
    let Some(crate::core::property::PropertyValue::Vector(v)) = past.properties.get(property_key)
    else {
        return None;
    };
    Some(EndpointEmbedding {
        embedding: v.to_vec(),
        version: past.current_version,
    })
}

/// Build an `INVALID_ARGUMENT`-mapped error (`QueryError::InvalidParameter`).
fn invalid_argument(parameter: &str, reason: impl Into<String>) -> Error {
    Error::Query(QueryError::InvalidParameter {
        parameter: parameter.to_string(),
        reason: reason.into(),
    })
}

/// `NOT_FOUND`-mapped error for a missing monitor/alarm (reuses the string-
/// carrying storage not-found variant, mirroring the snapshot registry).
fn not_found(what: impl Into<String>) -> Error {
    Error::Storage(crate::core::error::StorageError::PropertyNotFound(
        what.into(),
    ))
}

// ---------------------------------------------------------------------------
// Monitor registry (durable sidecar `drift_monitors.json`, mirrors #3370).
// ---------------------------------------------------------------------------

/// Lowercase token for a [`DriftMetric`] (stable across JSON / sidecar).
fn metric_token(metric: DriftMetric) -> &'static str {
    match metric {
        DriftMetric::Cosine => "cosine",
        DriftMetric::Euclidean => "euclidean",
        DriftMetric::Angular => "angular",
    }
}

/// Parse a [`DriftMetric`] token. An unrecognized token returns `None` (the
/// caller skips the record/monitor rather than silently coercing a
/// forward-version or bit-flipped token to a default — robustness MINOR-7).
fn metric_from_token(token: &str) -> Option<DriftMetric> {
    match token {
        "cosine" => Some(DriftMetric::Cosine),
        "euclidean" => Some(DriftMetric::Euclidean),
        "angular" => Some(DriftMetric::Angular),
        _ => None,
    }
}

/// In-process registry of declared drift monitors, optionally persisted to a
/// `drift_monitors.json` sidecar (atomic temp→fsync→rename), mirroring the
/// named-snapshot registry (#3370). Entirely off the data write path.
pub(crate) struct DriftMonitorRegistry {
    entries: parking_lot::RwLock<std::collections::BTreeMap<u64, DriftMonitor>>,
    next_id: AtomicU64,
    persist_path: Option<std::path::PathBuf>,
    save_lock: parking_lot::Mutex<()>,
    /// Per-monitor evaluation locks that serialize `evaluate_drift_monitor_now`
    /// for one monitor id, closing the TOCTOU double-fire window between the
    /// unresolved-alarm query and the alarm-node create (concurrency MAJOR-1).
    eval_locks: parking_lot::Mutex<std::collections::HashMap<u64, Arc<parking_lot::Mutex<()>>>>,
    /// Set when [`open`](Self::open) quarantined a corrupt sidecar, so a
    /// post-load pass can reseed `next_id` above any orphaned alarm's
    /// `monitor_id` before ids are reused (robustness MINOR-4).
    quarantined: AtomicBool,
}

/// serde envelope for the sidecar. Foreign types (`DriftMetric`, `NodeId`,
/// `Timestamp`) have no serde derives, so a monitor is projected onto a flat
/// primitive record, mirroring the snapshot registry's `ts_hlc` approach.
#[cfg(feature = "serde")]
#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedMonitor {
    id: u64,
    property_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entities: Option<Vec<u64>>,
    metric: String,
    threshold: f32,
    window_micros: u64,
    target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scheduled_interval_micros: Option<u64>,
    created_wallclock: i64,
    created_logical: u32,
}

/// The on-disk registry envelope (versioned, mirrors the snapshot store).
#[cfg(feature = "serde")]
#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedRegistry {
    version: u32,
    monitors: Vec<PersistedMonitor>,
}

/// Persisted-format version for the drift-monitor sidecar.
#[cfg(feature = "serde")]
const PERSIST_FORMAT_VERSION: u32 = 1;

impl DriftMonitorRegistry {
    /// An empty, memory-only registry (no file is ever written).
    pub(crate) fn in_memory() -> Self {
        Self {
            entries: parking_lot::RwLock::new(std::collections::BTreeMap::new()),
            next_id: AtomicU64::new(1),
            persist_path: None,
            save_lock: parking_lot::Mutex::new(()),
            eval_locks: parking_lot::Mutex::new(std::collections::HashMap::new()),
            quarantined: AtomicBool::new(false),
        }
    }

    /// Open a registry, loading any existing sidecar at `path`.
    ///
    /// A corrupt or unparseable sidecar is quarantined aside (`*.corrupt`) and
    /// the registry starts empty — startup is never bricked (mirrors #3370).
    pub(crate) fn open(path: Option<std::path::PathBuf>) -> Result<Self> {
        let registry = Self {
            entries: parking_lot::RwLock::new(std::collections::BTreeMap::new()),
            next_id: AtomicU64::new(1),
            persist_path: path.clone(),
            save_lock: parking_lot::Mutex::new(()),
            eval_locks: parking_lot::Mutex::new(std::collections::HashMap::new()),
            quarantined: AtomicBool::new(false),
        };
        #[cfg(feature = "serde")]
        if let Some(path) = path {
            match std::fs::read_to_string(&path) {
                Ok(contents) => match serde_json::from_str::<PersistedRegistry>(&contents) {
                    Ok(parsed) if parsed.version <= PERSIST_FORMAT_VERSION => {
                        let mut max_id = 0u64;
                        let mut entries = registry.entries.write();
                        for pm in parsed.monitors {
                            if let Some(monitor) = persisted_to_monitor(pm) {
                                max_id = max_id.max(monitor.id.get());
                                entries.insert(monitor.id.get(), monitor);
                            }
                        }
                        // NIT-8: `saturating_add` guards the (unreachable)
                        // u64::MAX-monitors edge from a debug panic / release wrap.
                        registry
                            .next_id
                            .store(max_id.saturating_add(1), Ordering::SeqCst);
                    }
                    _ => {
                        quarantine(&path);
                        registry.quarantined.store(true, Ordering::SeqCst);
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    quarantine(&path);
                    registry.quarantined.store(true, Ordering::SeqCst);
                }
            }
        }
        Ok(registry)
    }

    /// Register a new monitor: assign a monotonic id, insert, persist.
    pub(crate) fn register(
        &self,
        spec: DriftMonitorSpec,
        created_at: Timestamp,
    ) -> Result<DriftMonitor> {
        let _guard = self.save_lock.lock();
        let id = MonitorId::new(self.next_id.fetch_add(1, Ordering::SeqCst));
        let monitor = DriftMonitor {
            id,
            spec,
            created_at,
        };
        self.entries.write().insert(id.get(), monitor.clone());
        if let Err(e) = self.save_locked() {
            self.entries.write().remove(&id.get());
            return Err(e);
        }
        Ok(monitor)
    }

    /// Fetch a monitor by id.
    pub(crate) fn get(&self, id: MonitorId) -> Option<DriftMonitor> {
        self.entries.read().get(&id.get()).cloned()
    }

    /// List all monitors in ascending id order.
    pub(crate) fn list(&self) -> Vec<DriftMonitor> {
        self.entries.read().values().cloned().collect()
    }

    /// Remove a monitor by id (NOT_FOUND if absent).
    pub(crate) fn remove(&self, id: MonitorId) -> Result<()> {
        let _guard = self.save_lock.lock();
        let removed = {
            let mut entries = self.entries.write();
            match entries.remove(&id.get()) {
                Some(removed) => removed,
                None => return Err(not_found(format!("drift monitor {}", id.get()))),
            }
        };
        if let Err(e) = self.save_locked() {
            self.entries.write().insert(id.get(), removed);
            return Err(e);
        }
        // Drop the per-monitor evaluation lock; a future id reuse starts fresh.
        self.eval_locks.lock().remove(&id.get());
        Ok(())
    }

    /// The per-monitor evaluation lock (created on first use), serializing
    /// `evaluate_drift_monitor_now` for one monitor id (concurrency MAJOR-1).
    /// A leaf lock: acquired only by the evaluation entry point, never from the
    /// write path, so it adds no edge to the documented lock-acquisition order.
    pub(crate) fn eval_lock(&self, id: MonitorId) -> Arc<parking_lot::Mutex<()>> {
        let mut locks = self.eval_locks.lock();
        Arc::clone(
            locks
                .entry(id.get())
                .or_insert_with(|| Arc::new(parking_lot::Mutex::new(()))),
        )
    }

    /// Whether [`open`](Self::open) quarantined a corrupt sidecar this session.
    pub(crate) fn was_quarantined(&self) -> bool {
        self.quarantined.load(Ordering::SeqCst)
    }

    /// Raise `next_id` above `max_monitor_id` (monotonic, idempotent) and clear
    /// the quarantined flag. Used after a corrupt-sidecar quarantine so a reused
    /// id cannot collide with an orphaned alarm's `monitor_id` (robustness
    /// MINOR-4). `saturating_add` (NIT-8).
    pub(crate) fn ensure_next_id_above(&self, max_monitor_id: u64) {
        self.next_id
            .fetch_max(max_monitor_id.saturating_add(1), Ordering::SeqCst);
        self.quarantined.store(false, Ordering::SeqCst);
    }

    /// Durable-write body, assuming `save_lock` is held. No-op when no path.
    fn save_locked(&self) -> Result<()> {
        let Some(path) = &self.persist_path else {
            return Ok(());
        };
        #[cfg(not(feature = "serde"))]
        {
            let _ = path;
            return Ok(());
        }
        #[cfg(feature = "serde")]
        {
            let monitors: Vec<PersistedMonitor> = self
                .entries
                .read()
                .values()
                .map(monitor_to_persisted)
                .collect();
            let serialized = serde_json::to_vec_pretty(&PersistedRegistry {
                version: PERSIST_FORMAT_VERSION,
                monitors,
            })
            .map_err(|e| Error::Other(format!("failed to serialize drift monitors: {e}")))?;
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
            #[cfg(unix)]
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::File::open(parent)?.sync_all()?;
            }
            Ok(())
        }
    }
}

/// Move a corrupt/unreadable sidecar aside (`*.corrupt`) so startup proceeds.
#[cfg(feature = "serde")]
fn quarantine(path: &std::path::Path) {
    let mut corrupt = path.as_os_str().to_owned();
    corrupt.push(".corrupt");
    let _ = std::fs::rename(path, std::path::PathBuf::from(corrupt));
}

#[cfg(feature = "serde")]
fn monitor_to_persisted(monitor: &DriftMonitor) -> PersistedMonitor {
    let (target, scheduled_interval_micros) = match monitor.spec.mode {
        EvalMode::OnWrite => (monitor.spec.target.as_str().to_string(), None),
        EvalMode::Scheduled { interval } => (
            monitor.spec.target.as_str().to_string(),
            // Checked conversion: an out-of-range interval saturates rather than
            // wrapping to a bogus (possibly tiny) value (robustness MINOR-5).
            Some(u64::try_from(interval.as_micros()).unwrap_or(u64::MAX)),
        ),
    };
    PersistedMonitor {
        id: monitor.id.get(),
        property_key: monitor.spec.property_key.clone(),
        label: monitor.spec.label.clone(),
        entities: monitor
            .spec
            .entities
            .as_ref()
            .map(|e| e.iter().map(|n| n.as_u64()).collect()),
        metric: metric_token(monitor.spec.metric).to_string(),
        threshold: monitor.spec.threshold,
        // Checked conversion (robustness MINOR-5): an out-of-range window
        // saturates rather than wrapping. `create_drift_monitor` already rejects
        // windows exceeding `i64::MAX` micros, so this only guards forged input.
        window_micros: u64::try_from(monitor.spec.window.as_micros()).unwrap_or(u64::MAX),
        target,
        scheduled_interval_micros,
        created_wallclock: monitor.created_at.wallclock(),
        created_logical: monitor.created_at.logical(),
    }
}

#[cfg(feature = "serde")]
fn persisted_to_monitor(pm: PersistedMonitor) -> Option<DriftMonitor> {
    // An unrecognized target/metric token skips the monitor rather than
    // silently coercing to a default (robustness MINOR-7).
    let target = match pm.target.as_str() {
        "per_entity" => DriftTarget::PerEntity,
        "label_centroid" => DriftTarget::LabelCentroid,
        _ => return None,
    };
    let mode = match pm.scheduled_interval_micros {
        Some(micros) => EvalMode::Scheduled {
            interval: Duration::from_micros(micros),
        },
        None => EvalMode::OnWrite,
    };
    let entities = pm.entities.map(|ids| {
        ids.into_iter()
            .filter_map(|n| NodeId::new(n).ok())
            .collect()
    });
    let created_at = Timestamp::new(pm.created_wallclock, pm.created_logical).ok()?;
    Some(DriftMonitor {
        id: MonitorId::new(pm.id),
        spec: DriftMonitorSpec {
            property_key: pm.property_key,
            label: pm.label,
            entities,
            metric: metric_from_token(&pm.metric)?,
            threshold: pm.threshold,
            window: Duration::from_micros(pm.window_micros),
            target,
            mode,
        },
        created_at,
    })
}

/// Build the sidecar path for the drift-monitor registry, or `None` when the
/// database is ephemeral. Lives inside the persistence dir at
/// `{persistence.data_dir}/drift_monitors.json`, mirroring `snapshots.json`.
pub(crate) fn registry_path_for(
    persistence: &crate::storage::index_persistence::PersistenceConfig,
) -> Option<std::path::PathBuf> {
    if !persistence.enabled {
        return None;
    }
    Some(persistence.data_dir.join("drift_monitors.json"))
}

// ---------------------------------------------------------------------------
// Background driver (Stage B fleshes out subscription + queue + shedding).
// ---------------------------------------------------------------------------

/// Default capacity of the bounded evaluation queue that the engine sheds from
/// on saturation. Deliberately small: the queue is a shock absorber for bursts,
/// not a work backlog — a saturated queue **sheds** (increments
/// [`DriftAlarmEngine::shed_count`]) rather than back-pressuring the changefeed
/// producer / commit path (AC6). Evaluation is idempotent and re-driven by the
/// next matching change or scheduled tick, so a shed task loses no correctness.
pub const DEFAULT_EVAL_QUEUE_CAPACITY: usize = 64;

/// How long a dispatcher/ticker thread parks between shutdown-flag checks. Bounds
/// `stop()` latency to roughly this interval.
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Background evaluator: subscribes to the changefeed for on-write monitors,
/// ticks scheduled monitors, and persists fired alarms — all off the write
/// path, shedding on queue saturation.
///
/// # Threads
///
/// [`start`](Self::start) spawns up to three background threads:
/// - a **dispatcher** that drains the changefeed [`Subscription`] and, for each
///   matching committed change, enqueues an evaluation of the affected
///   monitor(s) onto the bounded queue (never blocking — a full queue sheds);
/// - a **worker** that pops queued monitor ids and runs
///   [`AletheiaDB::evaluate_drift_monitor_now`] (the sole path that materializes
///   alarm nodes and dedups against unresolved alarms);
/// - a **ticker** (only when scheduled monitors exist) that enqueues an
///   evaluation for each scheduled monitor on its own interval.
///
/// Changes to the reserved [`DRIFT_ALARM_LABEL`] are ignored by the dispatcher,
/// so materializing an alarm never re-triggers evaluation (no feedback loop).
///
/// # Lock discipline
///
/// The engine's own synchronization primitives (the bounded `mpsc` queue, the
/// `shed_count` atomic, and the `state` mutex guarding the join handles) are
/// **leaves**: a worker never holds the `state` mutex while calling into the
/// database, and the database write-path locks are only ever taken *inside*
/// the public `AletheiaDB` API the worker calls, never around it. The engine
/// therefore introduces no new edge into the documented lock-acquisition order.
///
/// # v1 scope
///
/// The monitor set is snapshotted at [`start`](Self::start): monitors created
/// after start are not watched until the engine is restarted (a documented
/// follow-up). Evaluation is best-effort — errors from a since-deleted monitor
/// or a transient read are dropped, since the next change/tick re-drives it.
pub struct DriftAlarmEngine {
    db: Arc<AletheiaDB>,
    shed_count: Arc<AtomicU64>,
    capacity: usize,
    /// Gate the worker checks before draining each evaluation task, so a test can
    /// deterministically stall evaluation and force the bounded queue to saturate
    /// (see [`set_evaluation_paused`](Self::set_evaluation_paused)).
    eval_pause: Arc<EvalPause>,
    state: parking_lot::Mutex<EngineState>,
}

/// Mutable run state, guarded by the engine's leaf `state` mutex.
struct EngineState {
    /// Shutdown flag shared with the dispatcher/ticker; `Some` iff running.
    running: Option<Arc<AtomicBool>>,
    /// Join handles for the spawned background threads.
    handles: Vec<std::thread::JoinHandle<()>>,
}

/// A pause gate the evaluation worker parks on. Normally unpaused (a single
/// cheap lock+bool check per task); a diagnostic/test hook can pause it to stall
/// evaluation deterministically without touching the write path.
struct EvalPause {
    paused: parking_lot::Mutex<bool>,
    resumed: parking_lot::Condvar,
}

impl EvalPause {
    fn new() -> Self {
        Self {
            paused: parking_lot::Mutex::new(false),
            resumed: parking_lot::Condvar::new(),
        }
    }

    /// Block while paused (used by the worker before it drains the next task).
    fn wait_while_paused(&self) {
        let mut guard = self.paused.lock();
        while *guard {
            self.resumed.wait(&mut guard);
        }
    }

    /// Set the paused flag and wake any parked worker on resume.
    fn set(&self, paused: bool) {
        *self.paused.lock() = paused;
        if !paused {
            self.resumed.notify_all();
        }
    }
}

impl DriftAlarmEngine {
    /// Create an engine bound to `db` (not yet running; call [`start`]).
    ///
    /// Uses [`DEFAULT_EVAL_QUEUE_CAPACITY`] for the bounded evaluation queue.
    ///
    /// [`start`]: DriftAlarmEngine::start
    #[must_use]
    pub fn new(db: Arc<AletheiaDB>) -> Self {
        Self::with_capacity(db, DEFAULT_EVAL_QUEUE_CAPACITY)
    }

    /// Create an engine with an explicit bounded-queue capacity (min 1).
    ///
    /// A smaller capacity sheds sooner under load — useful for deterministically
    /// exercising the shed path. The capacity bounds only the *pending* backlog;
    /// it never bounds the number of evaluations performed over time.
    #[must_use]
    pub fn with_capacity(db: Arc<AletheiaDB>, capacity: usize) -> Self {
        Self {
            db,
            shed_count: Arc::new(AtomicU64::new(0)),
            capacity: capacity.max(1),
            eval_pause: Arc::new(EvalPause::new()),
            state: parking_lot::Mutex::new(EngineState {
                running: None,
                handles: Vec::new(),
            }),
        }
    }

    /// Pause or resume background evaluation (diagnostic / test hook).
    ///
    /// When paused, the worker stops draining the bounded evaluation queue, so a
    /// sustained burst of changes deterministically saturates the queue and
    /// sheds — the observable proof that a saturated evaluator sheds rather than
    /// back-pressuring commits (AC6). Commits continue unaffected while paused
    /// (the engine is off the write path). Resuming wakes the worker to drain
    /// whatever remains. This does not affect [`shed_count`](Self::shed_count)
    /// semantics. Not part of the stable API.
    #[doc(hidden)]
    pub fn set_evaluation_paused(&self, paused: bool) {
        self.eval_pause.set(paused);
    }

    /// Start the background subscription + ticker.
    ///
    /// Idempotent: calling `start` on an already-running engine is a no-op. The
    /// monitor set is snapshotted here (see the type-level v1-scope note).
    ///
    /// # Errors
    ///
    /// Fails if the changefeed subscription cannot be established.
    pub fn start(&self) -> Result<()> {
        // Fast idempotency check under a short-lived guard: an already-running
        // engine is a no-op. We release `state` immediately so the DB calls below
        // (list + subscribe) do NOT run while holding `state` — keeping `state`
        // the pure leaf its type doc claims (concurrency MINOR-1).
        {
            let state = self.state.lock();
            if state.running.is_some() {
                return Ok(());
            }
        }

        // Snapshot the monitor set WITHOUT holding `state`. Partition on-write
        // (changefeed-driven) from scheduled (ticker-driven). Entity-scoped
        // (unlabeled) on-write monitors are indexed by watched node id so the
        // dispatcher enqueues them only for their own entities (concurrency
        // MINOR-3), and a label-less entity-less monitor is inert (skipped).
        let monitors = self.db.list_drift_monitors();
        let mut labeled_on_write: std::collections::HashMap<String, Vec<MonitorId>> =
            std::collections::HashMap::new();
        let mut entity_on_write: std::collections::HashMap<u64, Vec<MonitorId>> =
            std::collections::HashMap::new();
        let mut scheduled: Vec<(MonitorId, Duration)> = Vec::new();
        for monitor in &monitors {
            match monitor.spec.mode {
                EvalMode::OnWrite => match &monitor.spec.label {
                    Some(label) => labeled_on_write
                        .entry(label.clone())
                        .or_default()
                        .push(monitor.id),
                    None => {
                        if let Some(entities) = &monitor.spec.entities {
                            for nid in entities {
                                entity_on_write
                                    .entry(nid.as_u64())
                                    .or_default()
                                    .push(monitor.id);
                            }
                        }
                        // label = None AND entities = None is inert (no entity
                        // set to evaluate); it is intentionally not dispatched.
                    }
                },
                EvalMode::Scheduled { interval } => {
                    if !interval.is_zero() {
                        scheduled.push((monitor.id, interval));
                    }
                }
            }
        }

        // Establish the subscription (a DB call) BEFORE taking `state`.
        let has_on_write = !labeled_on_write.is_empty() || !entity_on_write.is_empty();
        let subscription_setup = if has_on_write {
            // If any on-write monitor is entity-scoped (no label), we cannot
            // restrict the subscription by label — subscribe to all node changes
            // and filter in the dispatcher. Otherwise restrict to the watched
            // labels (the reserved alarm label is never among them, so alarm
            // materialization never reaches this subscription).
            let filter = if entity_on_write.is_empty() {
                let labels: Vec<String> = labeled_on_write.keys().cloned().collect();
                crate::core::changefeed_subscription::ChangeFilter::all().with_node_labels(labels)
            } else {
                crate::core::changefeed_subscription::ChangeFilter::all()
            };
            let subscription = self.db.subscribe_changes(filter.clone())?;
            Some((filter, subscription))
        } else {
            None
        };

        // Publish handles under `state`, re-checking `running` for a start/start
        // race. On a lost race, drop the just-made subscription (deregistering
        // it) and return.
        let mut state = self.state.lock();
        if state.running.is_some() {
            return Ok(());
        }

        let running = Arc::new(AtomicBool::new(true));
        // Bounded evaluation queue: `try_send` never blocks the producer; a full
        // queue yields `TrySendError::Full`, which the producers count as a shed.
        let (tx, rx) = std::sync::mpsc::sync_channel::<MonitorId>(self.capacity);
        let mut handles: Vec<std::thread::JoinHandle<()>> = Vec::new();

        if let Some((filter, subscription)) = subscription_setup {
            let dispatcher_db = Arc::clone(&self.db);
            let dispatcher_tx = tx.clone();
            let dispatcher_running = Arc::clone(&running);
            let dispatcher_shed = Arc::clone(&self.shed_count);
            handles.push(std::thread::spawn(move || {
                dispatcher_loop(
                    &dispatcher_db,
                    &filter,
                    subscription,
                    &dispatcher_tx,
                    &dispatcher_running,
                    &dispatcher_shed,
                    &labeled_on_write,
                    &entity_on_write,
                );
            }));
        }

        // Ticker: only needed if there is at least one scheduled monitor.
        if !scheduled.is_empty() {
            let ticker_tx = tx.clone();
            let ticker_running = Arc::clone(&running);
            let ticker_shed = Arc::clone(&self.shed_count);
            handles.push(std::thread::spawn(move || {
                ticker_loop(&scheduled, &ticker_tx, &ticker_running, &ticker_shed);
            }));
        }

        // Worker: drains the queue and evaluates. Exits when every sender
        // (the original `tx` plus dispatcher/ticker clones) has been dropped.
        drop(tx);
        let worker_db = Arc::clone(&self.db);
        let worker_pause = Arc::clone(&self.eval_pause);
        handles.push(std::thread::spawn(move || {
            loop {
                // Park while paused BEFORE consuming, so a paused worker stops
                // draining entirely and the bounded queue saturates (AC6 shed).
                worker_pause.wait_while_paused();
                match rx.recv() {
                    // Best-effort: a since-deleted monitor or transient read
                    // error is dropped; the next change/tick re-drives it. A
                    // PANIC inside one evaluation must NOT unwind the worker and
                    // permanently disconnect the queue (concurrency MINOR-2), so
                    // each task runs inside `catch_unwind`; a caught panic is
                    // dropped and the worker continues.
                    Ok(id) => {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let _ = worker_db.evaluate_drift_monitor_now(id);
                        }));
                    }
                    // Every sender dropped (shutdown): exit.
                    Err(_) => break,
                }
            }
        }));

        state.running = Some(running);
        state.handles = handles;
        Ok(())
    }

    /// Stop the background driver and deregister the subscription.
    ///
    /// Signals shutdown, then joins every spawned thread. Safe to call more than
    /// once and safe to call when never started (both are no-ops). Never panics,
    /// even if a background thread panicked (its join error is discarded).
    pub fn stop(&self) {
        let (running, handles) = {
            let mut state = self.state.lock();
            (state.running.take(), std::mem::take(&mut state.handles))
        };
        if let Some(running) = running {
            running.store(false, Ordering::SeqCst);
        }
        // Unpause so a worker parked on the eval gate observes the dropped
        // senders (below) and exits instead of blocking `join` forever.
        self.eval_pause.set(false);
        // Joining outside the `state` lock: the background threads never touch
        // `state`, so this cannot deadlock, and a concurrent `start`/`stop`
        // simply observes the already-cleared state.
        for handle in handles {
            let _ = handle.join();
        }
    }

    /// Number of evaluation tasks shed due to queue saturation (observable
    /// write-path-safety counter; monotonic for the engine's lifetime).
    #[must_use]
    pub fn shed_count(&self) -> u64 {
        self.shed_count.load(Ordering::Relaxed)
    }
}

impl Drop for DriftAlarmEngine {
    fn drop(&mut self) {
        // Ensure background threads are torn down even if `stop` was not called.
        self.stop();
    }
}

/// Drain the changefeed subscription and enqueue monitor evaluations, shedding
/// (incrementing `shed`) when the bounded queue is full. Never blocks the
/// producer side of the queue.
#[allow(clippy::too_many_arguments)]
fn dispatcher_loop(
    db: &Arc<AletheiaDB>,
    filter: &crate::core::changefeed_subscription::ChangeFilter,
    mut subscription: crate::core::changefeed_subscription::Subscription,
    tx: &std::sync::mpsc::SyncSender<MonitorId>,
    running: &AtomicBool,
    shed: &AtomicU64,
    labeled: &std::collections::HashMap<String, Vec<MonitorId>>,
    entity_index: &std::collections::HashMap<u64, Vec<MonitorId>>,
) {
    // `running` is loaded `Relaxed` here (and in the ticker) while `stop` stores
    // it `SeqCst`: this flag guards no other shared data, its visibility is
    // bounded by the poll interval, and the intentional asymmetry keeps the poll
    // cheap (NIT-1).
    while running.load(Ordering::Relaxed) {
        match subscription.recv_timeout(SHUTDOWN_POLL_INTERVAL) {
            Ok(records) => {
                for record in records {
                    // Never re-trigger on our own alarm-node writes (avoid a
                    // feedback loop); the label filter already excludes them when
                    // present, but guard unconditionally for the all-labels case.
                    if record.label == DRIFT_ALARM_LABEL {
                        continue;
                    }
                    // Only NODE changes drive monitor evaluation; skip edges to
                    // cut queue pressure on the all-node fallback (MINOR-3).
                    if record.kind != crate::core::changefeed::EntityKind::Node {
                        continue;
                    }
                    if let Some(ids) = labeled.get(&record.label) {
                        for &id in ids {
                            enqueue_or_shed(tx, id, shed);
                        }
                    }
                    // Entity-scoped monitors fire only for their own node ids
                    // (a record whose id no entity monitor watches is skipped).
                    if let Some(ids) = entity_index.get(&record.entity_id) {
                        for &id in ids {
                            enqueue_or_shed(tx, id, shed);
                        }
                    }
                }
            }
            // Timeout with nothing buffered: loop to re-check the shutdown flag.
            Err(crate::core::changefeed_subscription::RecvError::Lagged { .. }) => {
                // The subscription buffer overflowed. Do NOT die — that would
                // leave the engine `running` but permanently deaf (concurrency
                // MAJOR-2). Drop the lagged handle and RESUBSCRIBE with a fresh
                // baseline, then continue. The rebaseline may miss the lagged
                // events (the documented best-effort / at-least-once contract);
                // the next matching change or scheduled tick re-drives evaluation.
                match db.subscribe_changes(filter.clone()) {
                    Ok(fresh) => subscription = fresh,
                    // Transient inability to resubscribe (e.g. cap): back off and
                    // retry while still running.
                    Err(_) => std::thread::sleep(SHUTDOWN_POLL_INTERVAL),
                }
            }
        }
        // `recv_timeout` returns `Ok(vec![])` on a plain timeout, so the loop
        // naturally re-checks `running` above.
    }
    // Dropping `subscription` (owned by this thread) deregisters it; dropping
    // `tx` (the caller's clone lives here) releases one sender.
}

/// Enqueue `id` for evaluation, or count a shed if the bounded queue is full.
/// Returns early on a disconnected queue (worker gone).
fn enqueue_or_shed(tx: &std::sync::mpsc::SyncSender<MonitorId>, id: MonitorId, shed: &AtomicU64) {
    match tx.try_send(id) {
        Ok(()) => {}
        Err(std::sync::mpsc::TrySendError::Full(_)) => {
            // Relaxed is correct: `shed_count` is a standalone monotonic counter
            // with no happens-before dependency on other shared data (NIT-2).
            shed.fetch_add(1, Ordering::Relaxed);
        }
        // Worker gone: nothing to do (the loop's `running` check will exit).
        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {}
    }
}

/// Enqueue each scheduled monitor on its own interval until shutdown, shedding
/// when the bounded queue is full.
fn ticker_loop(
    scheduled: &[(MonitorId, Duration)],
    tx: &std::sync::mpsc::SyncSender<MonitorId>,
    running: &AtomicBool,
    shed: &AtomicU64,
) {
    let start = std::time::Instant::now();
    // Next fire instant (relative to `start`) per monitor.
    let mut next: Vec<Duration> = scheduled.iter().map(|&(_, interval)| interval).collect();
    while running.load(Ordering::Relaxed) {
        let elapsed = start.elapsed();
        for (slot, &(id, interval)) in next.iter_mut().zip(scheduled.iter()) {
            if elapsed >= *slot {
                enqueue_or_shed(tx, id, shed);
                // Advance to the next multiple strictly beyond `elapsed` so a
                // slow tick does not burst-fire to catch up.
                *slot = elapsed + interval;
            }
        }
        std::thread::sleep(SHUTDOWN_POLL_INTERVAL);
    }
}

// ---------------------------------------------------------------------------
// Feature-gated AletheiaDB accessors (present only under `semantic-temporal`).
// ---------------------------------------------------------------------------

// Property keys under which an alarm's AC4 fields live on its `__drift_alarm`
// node (append-only bi-temporal; resolve is a recorded update, never a delete).
const P_MONITOR_ID: &str = "monitor_id";
const P_ENTITY_ID: &str = "entity_id";
const P_LABEL: &str = "label";
const P_DISTANCE: &str = "measured_distance";
const P_THRESHOLD: &str = "threshold";
const P_METRIC: &str = "metric";
const P_NOW_WALL: &str = "compared_now_wallclock";
const P_NOW_LOG: &str = "compared_now_logical";
const P_PAST_WALL: &str = "compared_past_wallclock";
const P_PAST_LOG: &str = "compared_past_logical";
const P_FROM_VER: &str = "from_version";
const P_TO_VER: &str = "to_version";
const P_RESOLVED: &str = "resolved";
const P_FIRED_WALL: &str = "fired_at_wallclock";
const P_FIRED_LOG: &str = "fired_at_logical";
const P_RESOLUTION_WALL: &str = "resolution_wallclock";
const P_RESOLUTION_LOG: &str = "resolution_logical";

fn prop_int(props: &crate::core::property::PropertyMap, key: &str) -> Option<i64> {
    match props.get(key) {
        Some(crate::core::property::PropertyValue::Int(v)) => Some(*v),
        _ => None,
    }
}

fn prop_float(props: &crate::core::property::PropertyMap, key: &str) -> Option<f64> {
    match props.get(key) {
        Some(crate::core::property::PropertyValue::Float(v)) => Some(*v),
        _ => None,
    }
}

fn prop_bool(props: &crate::core::property::PropertyMap, key: &str) -> Option<bool> {
    match props.get(key) {
        Some(crate::core::property::PropertyValue::Bool(v)) => Some(*v),
        _ => None,
    }
}

fn prop_string(props: &crate::core::property::PropertyMap, key: &str) -> Option<String> {
    match props.get(key) {
        Some(crate::core::property::PropertyValue::String(v)) => Some(v.to_string()),
        _ => None,
    }
}

/// Reconstruct a [`DriftAlarm`] from the properties of a `__drift_alarm` node.
/// Returns `None` if a required field is absent (not an alarm node).
fn alarm_from_node(
    alarm_id: NodeId,
    props: &crate::core::property::PropertyMap,
) -> Option<DriftAlarm> {
    let monitor_id = MonitorId::new(prop_int(props, P_MONITOR_ID)? as u64);
    let measured_distance = prop_float(props, P_DISTANCE)? as f32;
    let threshold = prop_float(props, P_THRESHOLD)? as f32;
    let metric = metric_from_token(&prop_string(props, P_METRIC)?)?;
    let compared_now = Timestamp::new(
        prop_int(props, P_NOW_WALL)?,
        prop_int(props, P_NOW_LOG).unwrap_or(0) as u32,
    )
    .ok()?;
    let compared_past = Timestamp::new(
        prop_int(props, P_PAST_WALL)?,
        prop_int(props, P_PAST_LOG).unwrap_or(0) as u32,
    )
    .ok()?;
    let fired_at = Timestamp::new(
        prop_int(props, P_FIRED_WALL)?,
        prop_int(props, P_FIRED_LOG).unwrap_or(0) as u32,
    )
    .ok()?;
    Some(DriftAlarm {
        alarm_id,
        monitor_id,
        entity: prop_int(props, P_ENTITY_ID).and_then(|v| NodeId::new(v as u64).ok()),
        label: prop_string(props, P_LABEL),
        measured_distance,
        threshold,
        metric,
        compared_now,
        compared_past,
        from_version: prop_int(props, P_FROM_VER).and_then(|v| VersionId::new(v as u64).ok()),
        to_version: prop_int(props, P_TO_VER).and_then(|v| VersionId::new(v as u64).ok()),
        resolved: prop_bool(props, P_RESOLVED).unwrap_or(false),
        fired_at,
    })
}

impl AletheiaDB {
    /// Create and register a drift monitor (Issue #3367).
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for an unknown property, a metric inconsistent with
    /// the property's index metric, a non-positive threshold, or a zero window.
    pub fn create_drift_monitor(&self, spec: DriftMonitorSpec) -> Result<DriftMonitor> {
        // Threshold must be positive and finite (strict `>` firing rule).
        if !(spec.threshold.is_finite() && spec.threshold > 0.0) {
            return Err(invalid_argument(
                "threshold",
                "threshold must be a positive, finite number",
            ));
        }
        // Window must be a positive duration.
        if spec.window.is_zero() {
            return Err(invalid_argument(
                "window",
                "window must be a positive duration",
            ));
        }
        // Window must fit `i64` microseconds so the lookback arithmetic and the
        // durable sidecar conversion never wrap/overflow (AC8 overflow guard).
        if i64::try_from(spec.window.as_micros()).is_err() {
            return Err(invalid_argument(
                "window",
                "window is too large (exceeds i64::MAX microseconds)",
            ));
        }
        // A label-centroid monitor REQUIRES a label: without one every centroid
        // firing is `(entity: None, label: None)`, which the suppression match
        // can never dedup, so the monitor would re-fire on every evaluation
        // (unbounded `__drift_alarm` node creation). Reject at creation (AC8).
        if spec.target == DriftTarget::LabelCentroid && spec.label.is_none() {
            return Err(invalid_argument(
                "label",
                "label is required for a label-centroid monitor",
            ));
        }
        // An explicit but EMPTY entity set makes the monitor silently inert
        // (never fires) with no diagnostic; reject it (AC8).
        if let Some(entities) = &spec.entities
            && entities.is_empty()
        {
            return Err(invalid_argument(
                "entities",
                "explicit entity set must not be empty",
            ));
        }
        // The property must have a temporal vector index; its metric must be
        // consistent with the monitor's metric.
        let index = self
            .current
            .get_temporal_vector_index_for(&spec.property_key)
            .ok_or_else(|| {
                invalid_argument(
                    "property_key",
                    format!(
                        "no temporal vector index enabled for property '{}'",
                        spec.property_key
                    ),
                )
            })?;
        let index_metric = index.distance_metric();
        let consistent = matches!(
            (spec.metric, index_metric),
            (DriftMetric::Cosine, crate::index::vector::DistanceMetric::Cosine)
                | (
                    DriftMetric::Euclidean,
                    crate::index::vector::DistanceMetric::Euclidean
                )
                // Angular is a cosine-family refinement permitted on a cosine index.
                | (DriftMetric::Angular, crate::index::vector::DistanceMetric::Cosine)
        );
        if !consistent {
            return Err(invalid_argument(
                "metric",
                format!(
                    "metric {:?} is inconsistent with the property's index metric {index_metric:?}",
                    spec.metric
                ),
            ));
        }
        let created_at = crate::core::temporal::time::now();
        self.drift_monitors.register(spec, created_at)
    }

    /// List all registered drift monitors (ascending id order).
    #[must_use]
    pub fn list_drift_monitors(&self) -> Vec<DriftMonitor> {
        self.drift_monitors.list()
    }

    /// Fetch a single monitor by id.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` for an unknown monitor id.
    pub fn get_drift_monitor(&self, id: MonitorId) -> Result<DriftMonitor> {
        self.drift_monitors
            .get(id)
            .ok_or_else(|| not_found(format!("drift monitor {}", id.get())))
    }

    /// Delete a monitor, removing it from future evaluation.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` for an unknown monitor id.
    pub fn delete_drift_monitor(&self, id: MonitorId) -> Result<()> {
        self.drift_monitors.remove(id)
    }

    /// Evaluate a monitor immediately (synchronous), persisting any fired
    /// alarms and returning them. Used for scheduled cadence and tests.
    ///
    /// Deduplicates against existing UNRESOLVED alarms for the same
    /// `(monitor, entity/label)`, so a monitor never double-fires while an
    /// alarm is outstanding (rule 3); it re-arms only after resolution.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` for an unknown monitor; evaluation/persistence errors.
    pub fn evaluate_drift_monitor_now(&self, id: MonitorId) -> Result<Vec<DriftAlarm>> {
        let monitor = self.get_drift_monitor(id)?;
        // Serialize evaluations of THIS monitor so two concurrent manual /
        // scheduled calls cannot both observe zero unresolved alarms and both
        // fire (TOCTOU double-fire, concurrency MAJOR-1). The lock spans the
        // whole query-unresolved -> persist window. It is a leaf (acquired only
        // here, never from the write path), so it adds no lock-order edge.
        let eval_lock = self.drift_monitors.eval_lock(id);
        let _eval_guard = eval_lock.lock();

        let now = crate::core::temporal::time::now();
        let firings = evaluate_monitor(self, &monitor, now)?;
        if firings.is_empty() {
            return Ok(Vec::new());
        }

        // Suppression set: entities / labels with an outstanding unresolved
        // alarm. This scan is COMPLETE (uncapped) — a monitor with more than
        // `MAX_ALARM_QUERY_LIMIT` outstanding alarms must still be fully
        // suppressed (rule 3), so it must not truncate (robustness MAJOR-2).
        let (mut unresolved_entities, mut unresolved_labels) =
            self.unresolved_suppression_sets(id)?;

        let mut created = Vec::new();
        for firing in firings {
            let suppressed = match (&firing.entity, &firing.label) {
                (Some(e), _) => unresolved_entities.contains(&e.as_u64()),
                (None, Some(l)) => unresolved_labels.contains(l),
                _ => false,
            };
            if suppressed {
                continue;
            }
            let alarm = self.persist_drift_alarm(id, &monitor, &firing, now)?;
            // Guard against two firings in one batch for the same key.
            if let Some(e) = alarm.entity {
                unresolved_entities.insert(e.as_u64());
            }
            if let Some(l) = &alarm.label {
                unresolved_labels.insert(l.clone());
            }
            created.push(alarm);
        }
        Ok(created)
    }

    /// Full (uncapped) scan of the UNRESOLVED alarms for `monitor_id`, returning
    /// the `(entity ids, labels)` suppression sets. Unlike the capped
    /// [`query_drift_alarms`](Self::query_drift_alarms), it never truncates, so a
    /// monitor with more than [`MAX_ALARM_QUERY_LIMIT`] outstanding alarms is
    /// still fully suppressed (robustness MAJOR-2). Cost is O(total alarm nodes)
    /// in v1 (alarm nodes are append-only with no per-monitor index yet — a
    /// documented follow-up), but correctness (no double-fire) takes precedence
    /// over the scan cost.
    fn unresolved_suppression_sets(
        &self,
        monitor_id: MonitorId,
    ) -> Result<(
        std::collections::HashSet<u64>,
        std::collections::HashSet<String>,
    )> {
        let mut entities: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut labels: std::collections::HashSet<String> = std::collections::HashSet::new();
        for id in self.scan_nodes_by_label(DRIFT_ALARM_LABEL) {
            let Ok(node) = self.get_node(id) else {
                continue;
            };
            let Some(alarm) = alarm_from_node(id, &node.properties) else {
                continue;
            };
            if alarm.monitor_id != monitor_id || alarm.resolved {
                continue;
            }
            if let Some(e) = alarm.entity {
                entities.insert(e.as_u64());
            }
            if let Some(l) = alarm.label {
                labels.insert(l);
            }
        }
        Ok((entities, labels))
    }

    /// After a corrupt-sidecar quarantine at open, raise the monitor registry's
    /// `next_id` above any `monitor_id` still stamped on an orphaned
    /// `__drift_alarm` node, so a reused id cannot collide with the alarms of a
    /// since-lost monitor (robustness MINOR-4). A no-op unless the sidecar was
    /// quarantined. Called once after startup index/WAL load, when alarm nodes
    /// are queryable.
    pub(crate) fn reseed_drift_registry_after_quarantine(&self) {
        if !self.drift_monitors.was_quarantined() {
            return;
        }
        let mut max_monitor_id = 0u64;
        for id in self.scan_nodes_by_label(DRIFT_ALARM_LABEL) {
            if let Ok(node) = self.get_node(id)
                && let Some(alarm) = alarm_from_node(id, &node.properties)
            {
                max_monitor_id = max_monitor_id.max(alarm.monitor_id.get());
            }
        }
        self.drift_monitors.ensure_next_id_above(max_monitor_id);
    }

    /// Materialize one firing as an append-only `__drift_alarm` bi-temporal node
    /// via the normal write path, carrying every AC4 field as a property.
    fn persist_drift_alarm(
        &self,
        monitor_id: MonitorId,
        monitor: &DriftMonitor,
        firing: &DriftFiring,
        now: Timestamp,
    ) -> Result<DriftAlarm> {
        let mut builder = crate::PropertyMapBuilder::new()
            .insert(P_MONITOR_ID, monitor_id.get() as i64)
            .insert(P_DISTANCE, f64::from(firing.measured_distance))
            .insert(P_THRESHOLD, f64::from(monitor.spec.threshold))
            .insert(P_METRIC, metric_token(monitor.spec.metric))
            .insert(P_NOW_WALL, firing.compared_now.wallclock())
            .insert(P_NOW_LOG, i64::from(firing.compared_now.logical()))
            .insert(P_PAST_WALL, firing.compared_past.wallclock())
            .insert(P_PAST_LOG, i64::from(firing.compared_past.logical()))
            .insert(P_RESOLVED, false)
            .insert(P_FIRED_WALL, now.wallclock())
            .insert(P_FIRED_LOG, i64::from(now.logical()));
        if let Some(entity) = firing.entity {
            builder = builder.insert(P_ENTITY_ID, entity.as_u64() as i64);
        }
        if let Some(label) = &firing.label {
            builder = builder.insert(P_LABEL, label.as_str());
        }
        if let Some(from) = firing.from_version {
            builder = builder.insert(P_FROM_VER, from.as_u64() as i64);
        }
        if let Some(to) = firing.to_version {
            builder = builder.insert(P_TO_VER, to.as_u64() as i64);
        }
        let alarm_id = self.create_node(DRIFT_ALARM_LABEL, builder.build())?;
        Ok(DriftAlarm {
            alarm_id,
            monitor_id,
            entity: firing.entity,
            label: firing.label.clone(),
            measured_distance: firing.measured_distance,
            threshold: monitor.spec.threshold,
            metric: monitor.spec.metric,
            compared_now: firing.compared_now,
            compared_past: firing.compared_past,
            from_version: firing.from_version,
            to_version: firing.to_version,
            resolved: false,
            fired_at: now,
        })
    }

    /// Query durable drift alarms by monitor / label / resolved state / time
    /// range.
    ///
    /// When `time_range` is set to `[start, end)`, results are restricted to
    /// alarms whose `fired_at` transaction time falls within that half-open
    /// range, and each retained alarm is reconstructed AS OF the range's `end`
    /// coordinate (deterministic historical read), so an alarm resolved *after*
    /// that coordinate still reads unresolved — the append-only contract (AC5).
    /// An empty range (`start == end`) therefore returns nothing.
    ///
    /// # Errors
    ///
    /// Propagates read errors from the storage layer.
    pub fn query_drift_alarms(&self, filter: &DriftAlarmFilter) -> Result<Vec<DriftAlarm>> {
        let limit = filter.limit.min(MAX_ALARM_QUERY_LIMIT);
        let mut ids: Vec<NodeId> = self.scan_nodes_by_label(DRIFT_ALARM_LABEL).collect();
        ids.sort_unstable();

        let mut out = Vec::new();
        for id in ids {
            let props = match filter.time_range {
                Some((_start, end)) => match self.get_node_at_time(id, end, end) {
                    Ok(node) => node.properties,
                    // Alarm did not exist at that coordinate: exclude it.
                    Err(_) => continue,
                },
                None => match self.get_node(id) {
                    Ok(node) => node.properties,
                    Err(_) => continue,
                },
            };
            let Some(alarm) = alarm_from_node(id, &props) else {
                continue;
            };
            // Honor the range START: only alarms fired within `[start, end)`.
            // (The `end` coordinate additionally governs the AS-OF reconstruction
            // above.) An alarm fired before `start` is excluded.
            if let Some((start, end)) = filter.time_range
                && !(alarm.fired_at >= start && alarm.fired_at < end)
            {
                continue;
            }
            if let Some(m) = filter.monitor_id
                && alarm.monitor_id != m
            {
                continue;
            }
            if let Some(l) = &filter.label
                && alarm.label.as_deref() != Some(l.as_str())
            {
                continue;
            }
            if let Some(r) = filter.resolved
                && alarm.resolved != r
            {
                continue;
            }
            out.push(alarm);
        }
        out.sort_by(|a, b| {
            a.fired_at
                .cmp(&b.fired_at)
                .then_with(|| a.alarm_id.cmp(&b.alarm_id))
        });
        out.truncate(limit);
        Ok(out)
    }

    /// Resolve an alarm — a recorded, `AS OF`-stable bi-temporal update (sets
    /// `resolved = true` with a resolution transaction time), never a delete.
    /// Idempotent: re-resolving returns the already-resolved alarm.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` if `alarm_id` is not a drift-alarm node.
    pub fn resolve_drift_alarm(&self, alarm_id: NodeId) -> Result<DriftAlarm> {
        // Confirm the id is a drift-alarm node (resolves the interned label
        // without needing the interner directly).
        if !self
            .scan_nodes_by_label(DRIFT_ALARM_LABEL)
            .any(|n| n == alarm_id)
        {
            return Err(not_found(format!("drift alarm {}", alarm_id.as_u64())));
        }
        let node = self.get_node(alarm_id)?;
        let alarm = alarm_from_node(alarm_id, &node.properties)
            .ok_or_else(|| not_found(format!("drift alarm {}", alarm_id.as_u64())))?;
        if alarm.resolved {
            // Idempotent no-op: already resolved.
            return Ok(alarm);
        }
        let now = crate::core::temporal::time::now();
        let props = crate::PropertyMapBuilder::new()
            .insert(P_RESOLVED, true)
            .insert(P_RESOLUTION_WALL, now.wallclock())
            .insert(P_RESOLUTION_LOG, i64::from(now.logical()))
            .build();
        // PATCH update -> a new bi-temporal version; the prior (unresolved)
        // version remains readable AS OF before this transaction time.
        self.update_node_with_valid_time(alarm_id, props, None)?;
        let mut resolved = alarm;
        resolved.resolved = true;
        Ok(resolved)
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

    // -- Literal-rule firing fixtures (Fix-1, correctness review lens4) --------
    //
    // These drive the real reconstruction path (`get_node` for e_now,
    // `get_node_at_time` for e_past) through the pure `evaluate_monitor(&db,
    // &monitor, now)` seam with an EXPLICIT `now`. Because the past anchor is on
    // the TRANSACTION-time axis (the engine cannot recall a superseded embedding
    // along valid time at tx=now — see `evaluate_monitor`'s doc), the fixtures
    // spread the versions' *transaction* times via an injected `SimulatedClock`
    // so `now - window` lands strictly between two commits. Each fixture is
    // constructed so its assertion is the literal
    // `distance(embedding@now, embedding@(now-window))` outcome and would FAIL
    // under the OLD "earliest-in-window vs latest-in-window" reading (noted per
    // test).

    use crate::PropertyMapBuilder;
    use crate::core::temporal::time;
    use crate::simulation::clock::SimulatedClock;

    const HOUR_US: i64 = 3600 * 1_000_000;
    const T0: i64 = 1_000_000_000_000;

    fn make_node(db: &AletheiaDB, label: &str, embedding: &[f32]) -> NodeId {
        db.create_node(
            label,
            PropertyMapBuilder::new()
                .insert_vector("embedding", embedding)
                .build(),
        )
        .expect("create vector node")
    }

    fn update_vec(db: &AletheiaDB, node: NodeId, embedding: &[f32]) {
        db.update_node_with_valid_time(
            node,
            PropertyMapBuilder::new()
                .insert_vector("embedding", embedding)
                .build(),
            None,
        )
        .expect("update vector node");
    }

    fn per_entity_monitor_for(label: &str, threshold: f32, window_secs: u64) -> DriftMonitor {
        DriftMonitor {
            id: MonitorId::new(1),
            spec: DriftMonitorSpec {
                property_key: "embedding".to_string(),
                label: Some(label.to_string()),
                entities: None,
                metric: DriftMetric::Cosine,
                threshold,
                window: Duration::from_secs(window_secs),
                target: DriftTarget::PerEntity,
                mode: EvalMode::OnWrite,
            },
            created_at: ts(1000),
        }
    }

    fn label_centroid_monitor_for(label: &str, threshold: f32, window_secs: u64) -> DriftMonitor {
        let mut m = per_entity_monitor_for(label, threshold, window_secs);
        m.spec.target = DriftTarget::LabelCentroid;
        m
    }

    /// Enable a Cosine temporal vector index on `"embedding"` (dim `dim`),
    /// needed by `create_drift_monitor` for the persistence-path fixtures.
    fn enable_index(db: &AletheiaDB, dim: usize) {
        use crate::index::vector::temporal::{
            RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
        };
        use crate::index::vector::{DistanceMetric, HnswConfig};
        let config = TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots: 200,
            full_snapshot_interval: 10,
            hnsw_config: Some(HnswConfig::new(dim, DistanceMetric::Cosine)),
        };
        db.enable_temporal_vector_index("embedding", config)
            .expect("enable temporal vector index");
    }

    /// An ephemeral DB with a Cosine temporal vector index on `"embedding"`.
    fn db_with_index(dim: usize) -> AletheiaDB {
        let db = AletheiaDB::new().expect("db");
        enable_index(&db, dim);
        db
    }

    /// Insert a minimal well-formed `__drift_alarm` node directly (bypassing
    /// evaluation) so a test can populate many outstanding alarms cheaply.
    fn insert_raw_alarm(db: &AletheiaDB, monitor_id: u64, entity_id: u64) {
        let props = PropertyMapBuilder::new()
            .insert(P_MONITOR_ID, monitor_id as i64)
            .insert(P_ENTITY_ID, entity_id as i64)
            .insert(P_DISTANCE, 1.0f64)
            .insert(P_THRESHOLD, 0.5f64)
            .insert(P_METRIC, "cosine")
            .insert(P_NOW_WALL, T0)
            .insert(P_NOW_LOG, 0i64)
            .insert(P_PAST_WALL, T0)
            .insert(P_PAST_LOG, 0i64)
            .insert(P_RESOLVED, false)
            .insert(P_FIRED_WALL, T0)
            .insert(P_FIRED_LOG, 0i64)
            .build();
        db.create_node(DRIFT_ALARM_LABEL, props)
            .expect("insert raw alarm node");
    }

    // ---- Distinguishing window fixtures (pure evaluate_monitor) --------------

    /// New literal rule FIRES where earliest-in-window would NOT.
    ///
    /// Versions A@t0, B@t0+2h (current == B). `now - 1.5h` (= t0+1.5h) lands in
    /// A's transaction interval, so `e_past` (as-of tx) = A while `e_now` = B =>
    /// distance 1.0 > θ => FIRE. The old earliest-in-window rule sees only B in
    /// its tx-window [t0+1.5h, t0+3h] (A's commit at t0 is outside) => single
    /// endpoint => no fire, so it would FAIL this `assert_eq!(len, 1)`.
    #[test]
    fn literal_asof_fires_where_earliest_in_window_would_not() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = AletheiaDB::new().expect("db");
        let node = make_node(&db, "Doc", &a);
        clock.jump_to(T0 + 2 * HOUR_US);
        update_vec(&db, node, &b);
        clock.jump_to(T0 + 3 * HOUR_US);
        let now = time::now();
        let monitor = per_entity_monitor_for("Doc", 0.5, 5400); // 1.5h
        let firings = evaluate_monitor(&db, &monitor, now).expect("evaluate");
        assert_eq!(
            firings.len(),
            1,
            "as-of(tx now-window)=A vs current=B must fire (old earliest-in-window would not)"
        );
        assert_eq!(firings[0].entity, Some(node));
        assert!((firings[0].measured_distance - 1.0).abs() < 1e-6);
    }

    /// New literal rule does NOT fire where earliest-in-window WOULD.
    ///
    /// Versions A@t0, B@t0+1h, A@t0+2h (current == A). With window 2.5h,
    /// `now - window` = t0+0.5h lands in the first A's tx interval, so `e_past`
    /// (as-of tx) = A == current => distance 0 => no fire. The old
    /// earliest-in-window rule (tx-window [t0+0.5h, t0+3h] holds B@t0+1h and
    /// A@t0+2h) compares earliest B to latest A => distance 1 => it WOULD fire,
    /// so it would FAIL this `assert!(is_empty)`.
    #[test]
    fn literal_asof_does_not_fire_where_earliest_in_window_would() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = AletheiaDB::new().expect("db");
        let node = make_node(&db, "Doc", &a);
        clock.jump_to(T0 + HOUR_US);
        update_vec(&db, node, &b);
        clock.jump_to(T0 + 2 * HOUR_US);
        update_vec(&db, node, &a);
        clock.jump_to(T0 + 3 * HOUR_US);
        let now = time::now();
        let monitor = per_entity_monitor_for("Doc", 0.5, 9000); // 2.5h
        let firings = evaluate_monitor(&db, &monitor, now).expect("evaluate");
        assert!(
            firings.is_empty(),
            "as-of(tx now-window)=A == current=A must NOT fire (old earliest-in-window would)"
        );
    }

    /// A single, static version: `e_past` (as-of tx) == `e_now` => no fire.
    #[test]
    fn single_static_version_does_not_fire() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = AletheiaDB::new().expect("db");
        let _node = make_node(&db, "Doc", &a);
        clock.jump_to(T0 + 3 * HOUR_US);
        let now = time::now();
        let monitor = per_entity_monitor_for("Doc", 0.5, 5400);
        let firings = evaluate_monitor(&db, &monitor, now).expect("evaluate");
        assert!(firings.is_empty(), "a single static version has no drift");
    }

    /// Design case 5: nothing on record `window` ago (entity created after
    /// `now - window`) => `e_past` MISSING via a real point-in-time
    /// reconstruction => no fire, even though the current embedding differs.
    #[test]
    fn no_record_at_now_minus_window_does_not_fire() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = AletheiaDB::new().expect("db");
        // Created only at t0+2h; `now - 1.5h` = t0+1.5h precedes its existence.
        clock.jump_to(T0 + 2 * HOUR_US);
        let node = make_node(&db, "Doc", &a);
        clock.jump_to(T0 + 2 * HOUR_US + HOUR_US / 4);
        update_vec(&db, node, &b);
        clock.jump_to(T0 + 3 * HOUR_US);
        let now = time::now();
        let monitor = per_entity_monitor_for("Doc", 0.5, 5400);
        let firings = evaluate_monitor(&db, &monitor, now).expect("evaluate");
        assert!(
            firings.is_empty(),
            "nothing on record at now-window => missing past endpoint => no fire"
        );
    }

    /// MAJOR-3: the current embedding was removed, though older versions carry
    /// one. `e_now` is the CURRENT embedding (absent) => no fire. Window 3.5h so
    /// the old "latest vector-bearing vs earliest vector-bearing" reading would
    /// see A@t0 and B@t0+2h and FIRE, so it would FAIL this `assert!(is_empty)`.
    #[test]
    fn removed_current_embedding_does_not_fire() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = AletheiaDB::new().expect("db");
        let node = make_node(&db, "Doc", &a);
        clock.jump_to(T0 + 2 * HOUR_US);
        update_vec(&db, node, &b);
        clock.jump_to(T0 + 2 * HOUR_US + HOUR_US / 2);
        db.remove_node_property(node, "embedding")
            .expect("remove embedding");
        clock.jump_to(T0 + 3 * HOUR_US);
        let now = time::now();
        let monitor = per_entity_monitor_for("Doc", 0.5, 12600); // 3.5h
        let firings = evaluate_monitor(&db, &monitor, now).expect("evaluate");
        assert!(
            firings.is_empty(),
            "no CURRENT embedding => no fire (old latest-vector-bearing reading would fire)"
        );
    }

    /// BLOCKER-2: multi-entity exact fired-set. One PerEntity monitor over four
    /// nodes: two genuine crossers, one sub-threshold jitter, one with no past
    /// endpoint. The fired set must equal EXACTLY the two crossers.
    #[test]
    fn multi_entity_exact_fired_set() {
        use std::collections::HashSet;
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let c = [0.0f32, 0.0, 1.0, 0.0];
        let jitter = [1.0f32, 0.02, 0.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = AletheiaDB::new().expect("db");
        let crosser1 = make_node(&db, "Doc", &a);
        let crosser2 = make_node(&db, "Doc", &a);
        let jittery = make_node(&db, "Doc", &a);
        clock.jump_to(T0 + 2 * HOUR_US);
        update_vec(&db, crosser1, &b);
        update_vec(&db, crosser2, &c);
        update_vec(&db, jittery, &jitter);
        // No past endpoint: created after now-window (t0+1.5h).
        clock.jump_to(T0 + 2 * HOUR_US + HOUR_US / 2);
        let no_past = make_node(&db, "Doc", &b);
        let _ = no_past;
        clock.jump_to(T0 + 3 * HOUR_US);
        let now = time::now();
        let monitor = per_entity_monitor_for("Doc", 0.5, 5400);
        let firings = evaluate_monitor(&db, &monitor, now).expect("evaluate");
        let fired: HashSet<NodeId> = firings.iter().filter_map(|f| f.entity).collect();
        let expected: HashSet<NodeId> = [crosser1, crosser2].into_iter().collect();
        assert_eq!(
            fired, expected,
            "exactly the two crossers fire; jitter and no-past entities do not"
        );
    }

    /// BLOCKER-3: label-centroid aggregate drift with NO single entity over θ.
    /// e1 keeps direction but grows in magnitude (per-entity cosine distance 0);
    /// e2 is static (distance 0). The non-renormalized component-wise mean
    /// nonetheless rotates enough to cross θ. A co-registered PerEntity monitor
    /// fires ZERO; the centroid fires once with the hand-computed distance. The
    /// old centroid (only members with two in-window versions) would EXCLUDE the
    /// single-version e2 (and the tx-windowed e1) and see no drift => it FAILS.
    #[test]
    fn label_centroid_aggregate_drift_no_single_entity_over_threshold() {
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = AletheiaDB::new().expect("db");
        let e1 = make_node(&db, "Pop", &[1.0, 0.0]);
        let _e2 = make_node(&db, "Pop", &[0.0, 1.0]);
        clock.jump_to(T0 + 2 * HOUR_US);
        update_vec(&db, e1, &[5.0, 0.0]); // same direction, larger magnitude
        clock.jump_to(T0 + 3 * HOUR_US);
        let now = time::now();
        let window = 5400u64;

        let per_entity = per_entity_monitor_for("Pop", 0.1, window);
        let per_firings = evaluate_monitor(&db, &per_entity, now).expect("evaluate");
        assert!(
            per_firings.is_empty(),
            "no single entity crosses θ (each per-entity cosine distance is 0)"
        );

        // past centroid (0.5,0.5), now centroid (2.5,0.5); cosine distance ~0.1679.
        let centroid_monitor = label_centroid_monitor_for("Pop", 0.1, window);
        let centroid_firings = evaluate_monitor(&db, &centroid_monitor, now).expect("evaluate");
        assert_eq!(
            centroid_firings.len(),
            1,
            "the population centroid crosses θ even though no single entity does"
        );
        assert!(centroid_firings[0].entity.is_none());
        assert_eq!(centroid_firings[0].label.as_deref(), Some("Pop"));
        let expected = 0.167_949_7f32;
        assert!(
            (centroid_firings[0].measured_distance - expected).abs() < 2e-3,
            "hand-computed centroid distance ~{expected}, got {}",
            centroid_firings[0].measured_distance
        );
    }

    /// An empty-label centroid monitor never fires and never panics.
    #[test]
    fn empty_label_centroid_does_not_fire() {
        let db = AletheiaDB::new().expect("db");
        let now = time::now();
        let monitor = label_centroid_monitor_for("NoSuchLabel", 0.1, 3600);
        let firings = evaluate_monitor(&db, &monitor, now).expect("evaluate");
        assert!(firings.is_empty(), "no members => no centroid => no fire");
    }

    /// A zero-magnitude centroid (opposed unit members cancelling) must NOT
    /// false-positive under cosine: treated as no comparison => no fire.
    #[test]
    fn zero_magnitude_centroid_does_not_fire() {
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = AletheiaDB::new().expect("db");
        // now-centroid mean((1,0),(-1,0)) = (0,0); past unchanged.
        let e1 = make_node(&db, "Zero", &[1.0, 0.0]);
        let e2 = make_node(&db, "Zero", &[0.0, 1.0]);
        clock.jump_to(T0 + 2 * HOUR_US);
        update_vec(&db, e1, &[1.0, 0.0]);
        update_vec(&db, e2, &[-1.0, 0.0]);
        clock.jump_to(T0 + 3 * HOUR_US);
        let now = time::now();
        let monitor = label_centroid_monitor_for("Zero", 0.1, 5400);
        let firings = evaluate_monitor(&db, &monitor, now).expect("evaluate");
        assert!(
            firings.is_empty(),
            "a zero-magnitude centroid is no comparison, never a spurious max-drift fire"
        );
    }

    // ---- Persistence-path fixtures (evaluate_drift_monitor_now + registry) ---

    #[test]
    fn above_threshold_write_produces_queryable_alarm() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = db_with_index(4);
        let node = make_node(&db, "Doc", &a);
        let monitor = db
            .create_drift_monitor(per_entity_monitor_for("Doc", 0.5, 5400).spec)
            .expect("create monitor");
        clock.jump_to(T0 + 2 * HOUR_US);
        update_vec(&db, node, &b);
        clock.jump_to(T0 + 3 * HOUR_US);
        let fired = db
            .evaluate_drift_monitor_now(monitor.id)
            .expect("evaluate now");
        assert_eq!(fired.len(), 1, "exactly one alarm expected");
        let alarm = &fired[0];
        assert_eq!(alarm.entity, Some(node));
        assert_eq!(alarm.monitor_id, monitor.id);
        assert!(!alarm.resolved);
        assert!((alarm.threshold - 0.5).abs() < 1e-6);
        assert!(alarm.measured_distance > 0.5);
        assert!(alarm.to_version.is_some() && alarm.from_version.is_some());
        assert_ne!(
            alarm.from_version, alarm.to_version,
            "from/to version refs name distinct versions"
        );
        // Version-ref correctness (review lens 4 #19): the refs name the ACTUAL
        // endpoint versions, not just "some" version. `to_version` is the node's
        // current version; `from_version` is the version current at the exact
        // reconstructed past coordinate `(compared_now valid, compared_past tx)`.
        let current_v = db.get_node(node).expect("get current").current_version;
        assert_eq!(
            alarm.to_version,
            Some(current_v),
            "to_version names the current (drifted) version"
        );
        let past_node = db
            .get_node_at_time(node, alarm.compared_now, alarm.compared_past)
            .expect("reconstruct past endpoint");
        assert_eq!(
            alarm.from_version,
            Some(past_node.current_version),
            "from_version names the past endpoint version"
        );
        let queried = db
            .query_drift_alarms(&DriftAlarmFilter::for_monitor(monitor.id))
            .expect("query alarms");
        assert_eq!(queried.len(), 1);
        assert_eq!(queried[0].alarm_id, alarm.alarm_id);
    }

    #[test]
    fn sub_threshold_jitter_never_fires() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = db_with_index(4);
        let node = make_node(&db, "Doc", &a);
        let monitor = db
            .create_drift_monitor(per_entity_monitor_for("Doc", 0.5, 5400).spec)
            .expect("create monitor");
        clock.jump_to(T0 + 2 * HOUR_US);
        update_vec(&db, node, &[1.0, 0.02, 0.0, 0.0]);
        clock.jump_to(T0 + 3 * HOUR_US);
        let fired = db.evaluate_drift_monitor_now(monitor.id).expect("evaluate");
        assert!(fired.is_empty(), "sub-threshold jitter must not fire");
    }

    #[test]
    fn re_crossing_fires_only_after_resolution() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let c = [0.0f32, 0.0, 1.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = db_with_index(4);
        let node = make_node(&db, "Doc", &a);
        let monitor = db
            .create_drift_monitor(per_entity_monitor_for("Doc", 0.5, 5400).spec)
            .expect("create monitor");
        clock.jump_to(T0 + 2 * HOUR_US);
        update_vec(&db, node, &b);
        clock.jump_to(T0 + 3 * HOUR_US);
        let first = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
        assert_eq!(first.len(), 1, "first crossing fires");
        let again = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
        assert!(again.is_empty(), "no re-fire while unresolved");
        db.resolve_drift_alarm(first[0].alarm_id).expect("resolve");
        clock.jump_to(T0 + 4 * HOUR_US);
        update_vec(&db, node, &c);
        clock.jump_to(T0 + 5 * HOUR_US);
        let third = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
        assert_eq!(third.len(), 1, "re-arms only after resolution");
    }

    #[test]
    fn resolve_is_recorded_and_as_of_stable() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = db_with_index(4);
        let node = make_node(&db, "Doc", &a);
        let monitor = db
            .create_drift_monitor(per_entity_monitor_for("Doc", 0.5, 5400).spec)
            .expect("create monitor");
        clock.jump_to(T0 + 2 * HOUR_US);
        update_vec(&db, node, &b);
        clock.jump_to(T0 + 3 * HOUR_US);
        let before_fire = time::now();
        let fired = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
        let alarm_id = fired[0].alarm_id;
        clock.jump_to(T0 + 4 * HOUR_US);
        let before_resolution = time::now();
        clock.jump_to(T0 + 5 * HOUR_US);
        let resolved = db.resolve_drift_alarm(alarm_id).expect("resolve");
        assert!(resolved.resolved, "resolve marks resolved");
        let unresolved = db
            .query_drift_alarms(&DriftAlarmFilter {
                resolved: Some(false),
                ..DriftAlarmFilter::for_monitor(monitor.id)
            })
            .expect("query");
        assert!(
            unresolved.iter().all(|x| x.alarm_id != alarm_id),
            "resolved alarm excluded from unresolved filter"
        );
        // AS OF within [before_fire, before_resolution): fired, not yet resolved.
        let historical = db
            .query_drift_alarms(&DriftAlarmFilter {
                resolved: None,
                time_range: Some((before_fire, before_resolution)),
                ..DriftAlarmFilter::for_monitor(monitor.id)
            })
            .expect("query as of");
        assert!(
            historical
                .iter()
                .any(|x| x.alarm_id == alarm_id && !x.resolved),
            "AS OF before resolution shows the alarm unresolved"
        );
    }

    /// MAJOR-4: `query_drift_alarms` honors `time_range.start` — alarms fired
    /// before `start` are excluded.
    #[test]
    fn query_drift_alarms_honors_time_range_start() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = db_with_index(4);
        let node = make_node(&db, "Doc", &a);
        let monitor = db
            .create_drift_monitor(per_entity_monitor_for("Doc", 0.5, 5400).spec)
            .expect("create monitor");
        clock.jump_to(T0 + 2 * HOUR_US);
        update_vec(&db, node, &b);
        clock.jump_to(T0 + 3 * HOUR_US);
        let before_fire = time::now();
        let fired = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
        let alarm_id = fired[0].alarm_id;
        let fired_at = fired[0].fired_at;
        let far_future = Timestamp::new(T0 + 10 * HOUR_US, 0).expect("ts");
        // start before the fire => included.
        let included = db
            .query_drift_alarms(&DriftAlarmFilter {
                resolved: None,
                time_range: Some((before_fire, far_future)),
                ..DriftAlarmFilter::for_monitor(monitor.id)
            })
            .expect("query");
        assert!(
            included.iter().any(|x| x.alarm_id == alarm_id),
            "alarm fired within [before_fire, far_future) is included"
        );
        // start strictly after the fire => excluded.
        let after_fire = Timestamp::new(fired_at.wallclock() + 1_000_000, 0).expect("ts");
        let excluded = db
            .query_drift_alarms(&DriftAlarmFilter {
                resolved: None,
                time_range: Some((after_fire, far_future)),
                ..DriftAlarmFilter::for_monitor(monitor.id)
            })
            .expect("query");
        assert!(
            excluded.iter().all(|x| x.alarm_id != alarm_id),
            "alarm fired before `start` must be excluded (time_range.start honored)"
        );
    }

    #[test]
    fn alarm_not_retroactively_deleted_by_later_writes() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = db_with_index(4);
        let node = make_node(&db, "Doc", &a);
        let monitor = db
            .create_drift_monitor(per_entity_monitor_for("Doc", 0.5, 5400).spec)
            .expect("create monitor");
        clock.jump_to(T0 + 2 * HOUR_US);
        update_vec(&db, node, &b);
        clock.jump_to(T0 + 3 * HOUR_US);
        let fired = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
        let alarm_id = fired[0].alarm_id;
        clock.jump_to(T0 + 4 * HOUR_US);
        update_vec(&db, node, &b);
        let still = db
            .query_drift_alarms(&DriftAlarmFilter::for_monitor(monitor.id))
            .expect("query");
        assert!(
            still.iter().any(|x| x.alarm_id == alarm_id),
            "alarm survives later entity writes"
        );
    }

    #[test]
    fn changefeed_delivers_alarm_created_event() {
        use crate::core::changefeed::ChangeType;
        use crate::core::changefeed_subscription::ChangeFilter;
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = db_with_index(4);
        let node = make_node(&db, "Doc", &a);
        let monitor = db
            .create_drift_monitor(per_entity_monitor_for("Doc", 0.5, 5400).spec)
            .expect("create monitor");
        let sub = db
            .subscribe_changes(ChangeFilter::all().with_node_labels([DRIFT_ALARM_LABEL]))
            .expect("subscribe");
        clock.jump_to(T0 + 2 * HOUR_US);
        update_vec(&db, node, &b);
        clock.jump_to(T0 + 3 * HOUR_US);
        db.evaluate_drift_monitor_now(monitor.id).expect("eval");
        let records = sub.poll();
        assert!(
            records
                .iter()
                .any(|r| r.label == DRIFT_ALARM_LABEL && r.change_type == ChangeType::Created),
            "changefeed delivers a Created event for the materialized alarm node"
        );
    }

    #[test]
    fn label_centroid_fires_on_population_shift() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = db_with_index(4);
        let mut nodes = Vec::new();
        for _ in 0..4 {
            nodes.push(make_node(&db, "Doc", &a));
        }
        let monitor = db
            .create_drift_monitor(label_centroid_monitor_for("Doc", 0.4, 5400).spec)
            .expect("create monitor");
        clock.jump_to(T0 + 2 * HOUR_US);
        for &n in &nodes {
            update_vec(&db, n, &b);
        }
        clock.jump_to(T0 + 3 * HOUR_US);
        let fired = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
        assert_eq!(fired.len(), 1, "exactly one label-level alarm");
        assert_eq!(fired[0].label.as_deref(), Some("Doc"));
        assert!(
            fired[0].entity.is_none(),
            "label alarm has no single entity"
        );
    }

    // ---- AC8 validation specificity (robustness review, #18) ----------------

    /// A label-centroid monitor with no label is rejected with the `label`
    /// parameter (would otherwise re-fire unbounded — robustness MAJOR-1).
    #[test]
    fn label_centroid_without_label_is_invalid_argument_label() {
        let db = db_with_index(4);
        let spec = DriftMonitorSpec {
            property_key: "embedding".to_string(),
            label: None,
            entities: Some(vec![nid(1)]),
            metric: DriftMetric::Cosine,
            threshold: 0.5,
            window: Duration::from_secs(3600),
            target: DriftTarget::LabelCentroid,
            mode: EvalMode::OnWrite,
        };
        match db.create_drift_monitor(spec) {
            Err(Error::Query(QueryError::InvalidParameter { parameter, .. })) => {
                assert_eq!(
                    parameter, "label",
                    "label-centroid requires the `label` param"
                );
            }
            other => panic!("expected InvalidParameter(label), got {other:?}"),
        }
    }

    /// An explicit but empty entity set is rejected with the `entities` param.
    #[test]
    fn empty_entities_is_invalid_argument_entities() {
        let db = db_with_index(4);
        let spec = DriftMonitorSpec {
            property_key: "embedding".to_string(),
            label: Some("Doc".to_string()),
            entities: Some(vec![]),
            metric: DriftMetric::Cosine,
            threshold: 0.5,
            window: Duration::from_secs(3600),
            target: DriftTarget::PerEntity,
            mode: EvalMode::OnWrite,
        };
        match db.create_drift_monitor(spec) {
            Err(Error::Query(QueryError::InvalidParameter { parameter, .. })) => {
                assert_eq!(
                    parameter, "entities",
                    "empty entity set rejected on `entities`"
                );
            }
            other => panic!("expected InvalidParameter(entities), got {other:?}"),
        }
    }

    /// A window whose microseconds exceed `i64::MAX` is rejected with the
    /// `window` param (AC8 overflow guard).
    #[test]
    fn window_overflow_is_invalid_argument_window() {
        let db = db_with_index(4);
        let mut spec = per_entity_monitor_for("Doc", 0.5, 3600).spec;
        // `Duration::from_secs(u64::MAX).as_micros()` far exceeds `i64::MAX`.
        spec.window = Duration::from_secs(u64::MAX);
        match db.create_drift_monitor(spec) {
            Err(Error::Query(QueryError::InvalidParameter { parameter, .. })) => {
                assert_eq!(
                    parameter, "window",
                    "overflowing window rejected on `window`"
                );
            }
            other => panic!("expected InvalidParameter(window), got {other:?}"),
        }
    }

    /// A deleted monitor no longer evaluates: the cadence entry point errors
    /// `NOT_FOUND` rather than silently firing (#20).
    #[test]
    fn deleted_monitor_does_not_evaluate() {
        let db = db_with_index(4);
        let monitor = db
            .create_drift_monitor(per_entity_monitor_for("Doc", 0.5, 5400).spec)
            .expect("create");
        db.delete_drift_monitor(monitor.id).expect("delete");
        let err = db
            .evaluate_drift_monitor_now(monitor.id)
            .expect_err("deleted monitor must not evaluate");
        assert!(
            matches!(err, Error::Storage(_)),
            "evaluating a deleted monitor is NOT_FOUND, got {err:?}"
        );
    }

    // ---- Concurrency: TOCTOU double-fire guard (concurrency MAJOR-1, #16) ----

    /// N threads evaluate the SAME above-threshold monitor at the same simulated
    /// instant. Without the per-monitor eval lock two could both pass the
    /// unresolved-alarm check and double-fire; with it, exactly ONE alarm exists.
    #[test]
    fn concurrent_evaluate_fires_exactly_one_alarm() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = Arc::new(db_with_index(4));
        let node = make_node(&db, "Doc", &a);
        let monitor = db
            .create_drift_monitor(per_entity_monitor_for("Doc", 0.5, 5400).spec)
            .expect("create");
        clock.jump_to(T0 + 2 * HOUR_US);
        update_vec(&db, node, &b);
        let id = monitor.id;

        const THREADS: usize = 8;
        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let db = Arc::clone(&db);
            handles.push(std::thread::spawn(move || {
                // Each worker thread injects its OWN simulated clock (the clock is
                // thread-local) at the same evaluation instant, so all see the
                // same above-threshold drift.
                let clock = SimulatedClock::new(T0 + 3 * HOUR_US);
                let _g = clock.inject();
                let _ = db.evaluate_drift_monitor_now(id);
            }));
        }
        for h in handles {
            h.join().expect("evaluation thread");
        }
        let alarms = db
            .query_drift_alarms(&DriftAlarmFilter::for_monitor(id))
            .expect("query");
        assert_eq!(
            alarms.len(),
            1,
            "exactly one alarm despite {THREADS} concurrent evaluations"
        );
    }

    // ---- Suppression is complete beyond the query cap (MAJOR-2, #17) --------

    /// The unresolved-alarm suppression scan must be COMPLETE, not truncated at
    /// `MAX_ALARM_QUERY_LIMIT`. Populate more outstanding alarms than the cap and
    /// assert every one appears in the suppression set (so none would re-fire),
    /// while the capped public query clamps — proving the two paths differ.
    #[test]
    fn suppression_is_complete_beyond_query_cap() {
        let db = AletheiaDB::new().expect("db");
        let monitor_id = 1u64;
        let count = MAX_ALARM_QUERY_LIMIT + 5;
        for e in 0..count as u64 {
            insert_raw_alarm(&db, monitor_id, e + 1);
        }
        let (entities, _labels) = db
            .unresolved_suppression_sets(MonitorId::new(monitor_id))
            .expect("suppression scan");
        assert_eq!(
            entities.len(),
            count,
            "suppression scan is uncapped (not truncated at MAX_ALARM_QUERY_LIMIT)"
        );
        let capped = db
            .query_drift_alarms(&DriftAlarmFilter {
                resolved: Some(false),
                limit: MAX_ALARM_QUERY_LIMIT,
                ..DriftAlarmFilter::for_monitor(MonitorId::new(monitor_id))
            })
            .expect("capped query");
        assert_eq!(
            capped.len(),
            MAX_ALARM_QUERY_LIMIT,
            "the public query clamps to the cap (distinct from the suppression scan)"
        );
    }

    // ---- Durability across restart (AC4/AC5, #14) ---------------------------

    /// Monitors reload from the sidecar and fired alarm nodes persist + remain
    /// queryable/resolvable across a durable reopen from the same data dir.
    #[test]
    fn monitors_and_alarms_survive_restart() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();

        let (monitor_id, alarm_id) = {
            let db = AletheiaDB::open(&path).expect("open durable db");
            enable_index(&db, 4);
            let node = make_node(&db, "Doc", &a);
            let monitor = db
                .create_drift_monitor(per_entity_monitor_for("Doc", 0.5, 5400).spec)
                .expect("create monitor");
            clock.jump_to(T0 + 2 * HOUR_US);
            update_vec(&db, node, &b);
            clock.jump_to(T0 + 3 * HOUR_US);
            let fired = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
            assert_eq!(fired.len(), 1, "one alarm fires before restart");
            (monitor.id, fired[0].alarm_id)
        }; // `db` dropped here, releasing WAL/index locks.

        let db2 = AletheiaDB::open(&path).expect("reopen durable db");
        let reloaded = db2.get_drift_monitor(monitor_id).expect("monitor reloads");
        assert_eq!(
            reloaded.id, monitor_id,
            "monitor id preserved across restart"
        );
        assert!((reloaded.spec.threshold - 0.5).abs() < 1e-6);
        assert_eq!(reloaded.spec.target, DriftTarget::PerEntity);
        assert_eq!(reloaded.spec.property_key, "embedding");

        let alarms = db2
            .query_drift_alarms(&DriftAlarmFilter::for_monitor(monitor_id))
            .expect("query reloaded alarms");
        assert!(
            alarms.iter().any(|x| x.alarm_id == alarm_id && !x.resolved),
            "the unresolved alarm node persists and is queryable after restart"
        );
        let resolved = db2
            .resolve_drift_alarm(alarm_id)
            .expect("resolve after restart");
        assert!(
            resolved.resolved,
            "persisted alarm is resolvable after restart"
        );
    }

    // ---- Corrupt-sidecar quarantine + next_id reseed (MINOR-4, #15) ---------

    /// A garbage sidecar is quarantined (`*.corrupt`) with an empty registry on
    /// reopen, and `next_id` is reseeded above any orphaned alarm's `monitor_id`
    /// so a fresh monitor cannot reuse an id that still labels orphaned alarms.
    #[test]
    fn corrupt_sidecar_quarantine_reseeds_next_id() {
        let a = [1.0f32, 0.0, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0, 0.0];
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        let orphan_monitor_id;
        {
            let mut clock = SimulatedClock::new(T0);
            let _g = clock.inject();
            let db = AletheiaDB::open(&path).expect("open durable db");
            enable_index(&db, 4);
            let node = make_node(&db, "Doc", &a);
            let monitor = db
                .create_drift_monitor(per_entity_monitor_for("Doc", 0.5, 5400).spec)
                .expect("create monitor");
            orphan_monitor_id = monitor.id.get();
            clock.jump_to(T0 + 2 * HOUR_US);
            update_vec(&db, node, &b);
            clock.jump_to(T0 + 3 * HOUR_US);
            let fired = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
            assert_eq!(fired.len(), 1, "one orphaned alarm exists");
        } // `db` dropped.

        // Corrupt the sidecar the first session wrote.
        let sidecar = path.join("indexes").join("drift_monitors.json");
        std::fs::write(&sidecar, b"{ not valid json ]").expect("write garbage sidecar");

        let db2 = AletheiaDB::open(&path).expect("reopen despite corrupt sidecar");
        assert!(
            db2.list_drift_monitors().is_empty(),
            "corrupt sidecar => empty registry (startup not bricked)"
        );
        assert!(
            path.join("indexes")
                .join("drift_monitors.json.corrupt")
                .exists(),
            "corrupt sidecar quarantined aside"
        );

        // A newly created monitor must get an id ABOVE the orphaned alarm's id.
        enable_index(&db2, 4);
        let fresh = db2
            .create_drift_monitor(per_entity_monitor_for("Doc", 0.5, 5400).spec)
            .expect("create fresh monitor");
        assert!(
            fresh.id.get() > orphan_monitor_id,
            "reseeded next_id avoids colliding with orphaned alarm's monitor_id \
             (orphan={orphan_monitor_id}, fresh={})",
            fresh.id.get()
        );
    }

    // ---- Scheduled cadence smoke + lifecycle (#20) --------------------------

    /// An engine with only a scheduled monitor spawns the ticker (no dispatcher)
    /// and starts/stops cleanly, including an idempotent restart.
    #[test]
    fn scheduled_monitor_engine_starts_and_stops_cleanly() {
        let db = Arc::new(db_with_index(4));
        let mut spec = per_entity_monitor_for("Doc", 0.5, 5400).spec;
        spec.mode = EvalMode::Scheduled {
            interval: Duration::from_millis(10),
        };
        db.create_drift_monitor(spec)
            .expect("create scheduled monitor");
        let engine = DriftAlarmEngine::new(Arc::clone(&db));
        engine.start().expect("start engine (ticker only)");
        engine.stop();
        // Idempotent restart works.
        engine.start().expect("restart engine");
        engine.stop();
    }

    // ---- SM4: 12-week demo scenario (docs/guides/drift-alarms-demo.md) -------

    /// Success Metric SM4 (#3367): a concept whose embedding is rewritten over 12
    /// simulated weeks triggers EXACTLY one entity-level alarm and EXACTLY one
    /// label-level alarm, each at the documented crossing week. This is the
    /// deterministic backing test for `docs/guides/drift-alarms-demo.md`; keep the
    /// vectors, thresholds, and crossing weeks in that guide in lock-step with the
    /// constants here.
    ///
    /// Cohort: a `Concept` node `concept` plus three sibling `Concept` nodes that
    /// drift mildly in the same direction (a genuine population shift, not a static
    /// backdrop). Both monitors use `Cosine` distance and a **4-week trailing
    /// window**: each weekly evaluation compares the current embedding to the one
    /// on record as of transaction-time `now - 4 weeks` (the tx-time reconstruction
    /// rule documented on `evaluate_monitor`), so a full window of history must
    /// exist before any fire is possible (weeks 1-3 have no past endpoint).
    ///
    /// Documented crossing points (unit 2-D embeddings; distances are the literal
    /// `metric_distance` outputs):
    ///
    /// - **LabelCentroid** monitor on label `Concept`, threshold **0.03** (the
    ///   smaller, early-warning threshold): the cohort centroid's 4-week drift is
    ///   0.0248 at week 5 (below) and 0.0366 at week 6 (above), so it first crosses
    ///   at **week 6** -> one label alarm.
    /// - **PerEntity** monitor on `concept` only, threshold **0.40**: the concept's
    ///   own 4-week drift is 0.331 at week 8 (below) and 0.441 at week 9 (above), so
    ///   it first crosses at **week 9** -> one entity alarm.
    ///
    /// Because an unresolved alarm suppresses re-firing (and this scenario never
    /// resolves), each monitor fires at most once even though both distances keep
    /// growing through week 11 -- hence exactly one of each.
    #[test]
    fn twelve_week_demo_fires_one_entity_and_one_label_alarm() {
        // One simulated week in microseconds (HOUR_US = 3600 * 1_000_000).
        const WEEK_US: i64 = 7 * 24 * HOUR_US;
        // 12 weekly concept embeddings: unit vectors at accelerating rotation
        // (angles 0,4,9,15,22,31,42,55,70,87,106,127 degrees).
        let concept: [[f32; 2]; 12] = [
            [1.000000, 0.000000],
            [0.997564, 0.069756],
            [0.987688, 0.156434],
            [0.965926, 0.258819],
            [0.927184, 0.374607],
            [0.857167, 0.515038],
            [0.743145, 0.669131],
            [0.573576, 0.819152],
            [0.342020, 0.939693],
            [0.052336, 0.998630],
            [-0.275637, 0.961262],
            [-0.601815, 0.798635],
        ];
        // 12 weekly sibling embeddings: the same schedule scaled to 30% of the
        // concept's angle -- the cohort drifts, but far less than the concept.
        let sib: [[f32; 2]; 12] = [
            [1.000000, 0.000000],
            [0.999781, 0.020942],
            [0.998890, 0.047106],
            [0.996917, 0.078459],
            [0.993373, 0.114937],
            [0.986856, 0.161604],
            [0.975917, 0.218143],
            [0.958820, 0.284015],
            [0.933580, 0.358368],
            [0.898028, 0.439939],
            [0.849893, 0.526956],
            [0.786935, 0.617036],
        ];

        let mut clock = SimulatedClock::new(T0);
        let _g = clock.inject();
        let db = db_with_index(2);

        // Week 0: create the concept and three sibling concepts.
        let concept_id = make_node(&db, "Concept", &concept[0]);
        let siblings: Vec<NodeId> = (0..3).map(|_| make_node(&db, "Concept", &sib[0])).collect();

        // Per-entity monitor watches the concept ONLY (so a sibling's drift can
        // never produce an entity alarm); label-centroid monitor watches the whole
        // `Concept` cohort. Same metric + window; deliberately different thresholds.
        let window = Duration::from_micros(4 * WEEK_US as u64);
        let entity_monitor = db
            .create_drift_monitor(DriftMonitorSpec {
                property_key: "embedding".to_string(),
                label: None,
                entities: Some(vec![concept_id]),
                metric: DriftMetric::Cosine,
                threshold: 0.40,
                window,
                target: DriftTarget::PerEntity,
                mode: EvalMode::OnWrite,
            })
            .expect("create per-entity monitor");
        let centroid_monitor = db
            .create_drift_monitor(DriftMonitorSpec {
                property_key: "embedding".to_string(),
                label: Some("Concept".to_string()),
                entities: None,
                metric: DriftMetric::Cosine,
                threshold: 0.03,
                window,
                target: DriftTarget::LabelCentroid,
                mode: EvalMode::OnWrite,
            })
            .expect("create label-centroid monitor");

        let mut entity_weeks: Vec<usize> = Vec::new();
        let mut label_weeks: Vec<usize> = Vec::new();
        let mut entity_alarm: Option<DriftAlarm> = None;
        let mut label_alarm: Option<DriftAlarm> = None;

        for week in 1..12usize {
            // Rewrite the concept and its cohort at the start of the week.
            clock.jump_to(T0 + week as i64 * WEEK_US);
            update_vec(&db, concept_id, &concept[week]);
            for &s in &siblings {
                update_vec(&db, s, &sib[week]);
            }
            // Evaluate MID-week so `now - 4 weeks` lands strictly inside the
            // week-(week-4) transaction interval -- an unambiguous as-of endpoint.
            clock.jump_to(T0 + week as i64 * WEEK_US + WEEK_US / 2);
            for alarm in db
                .evaluate_drift_monitor_now(entity_monitor.id)
                .expect("evaluate entity monitor")
            {
                entity_weeks.push(week);
                entity_alarm = Some(alarm);
            }
            for alarm in db
                .evaluate_drift_monitor_now(centroid_monitor.id)
                .expect("evaluate centroid monitor")
            {
                label_weeks.push(week);
                label_alarm = Some(alarm);
            }
        }

        // Exactly one entity alarm, at the documented crossing week 9.
        assert_eq!(
            entity_weeks,
            vec![9],
            "entity alarm fires exactly once, at the documented crossing week 9"
        );
        // Exactly one label alarm, at the documented crossing week 6.
        assert_eq!(
            label_weeks,
            vec![6],
            "label alarm fires exactly once, at the documented crossing week 6"
        );

        let entity_alarm = entity_alarm.expect("entity alarm present");
        assert_eq!(entity_alarm.entity, Some(concept_id));
        assert!(
            entity_alarm.label.is_none(),
            "a per-entity alarm carries no label"
        );
        assert!((entity_alarm.threshold - 0.40).abs() < 1e-6);
        assert!(
            entity_alarm.measured_distance > 0.40,
            "entity distance must exceed its threshold, got {}",
            entity_alarm.measured_distance
        );
        assert!(
            (entity_alarm.measured_distance - 0.4408).abs() < 1e-2,
            "entity distance ~0.441 at week 9, got {}",
            entity_alarm.measured_distance
        );
        assert!(
            entity_alarm.from_version.is_some() && entity_alarm.to_version.is_some(),
            "entity alarm carries both compared version refs"
        );

        let label_alarm = label_alarm.expect("label alarm present");
        assert!(
            label_alarm.entity.is_none(),
            "a label-centroid alarm names no single entity"
        );
        assert_eq!(label_alarm.label.as_deref(), Some("Concept"));
        assert!((label_alarm.threshold - 0.03).abs() < 1e-6);
        assert!(
            label_alarm.measured_distance > 0.03,
            "centroid distance must exceed its threshold, got {}",
            label_alarm.measured_distance
        );
        assert!(
            (label_alarm.measured_distance - 0.0366).abs() < 1e-2,
            "centroid distance ~0.037 at week 6, got {}",
            label_alarm.measured_distance
        );

        // Exactly one durable alarm node per monitor across the whole run.
        let entity_alarms = db
            .query_drift_alarms(&DriftAlarmFilter::for_monitor(entity_monitor.id))
            .expect("query entity alarms");
        let label_alarms = db
            .query_drift_alarms(&DriftAlarmFilter::for_monitor(centroid_monitor.id))
            .expect("query label alarms");
        assert_eq!(entity_alarms.len(), 1, "one durable entity alarm node");
        assert_eq!(label_alarms.len(), 1, "one durable label alarm node");
        assert_eq!(entity_alarms[0].alarm_id, entity_alarm.alarm_id);
        assert_eq!(label_alarms[0].alarm_id, label_alarm.alarm_id);
    }
}
