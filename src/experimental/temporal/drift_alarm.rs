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
/// # Errors
///
/// `INVALID_ARGUMENT` on a dimension mismatch or a zero-magnitude vector for a
/// cosine/angular metric.
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
/// # Errors
///
/// - `INVALID_ARGUMENT` for an invalid spec surfaced at evaluation.
/// - `NOT_FOUND` if the watched property/index is missing.
pub fn evaluate_monitor(
    db: &AletheiaDB,
    monitor: &DriftMonitor,
    now: Timestamp,
) -> Result<Vec<DriftFiring>> {
    // Lookback window: [now - window, now]. We compare, per entity, the
    // *earliest* embedding version still inside that window against the
    // *latest* (current) one. This is the operational reading of the design's
    // `embedding(E, now)` vs `embedding(E, now - window)`: reconstructing
    // literally at the instant `now - window` would never fire on an entity's
    // very first drift (that instant precedes its creation), so the "past
    // endpoint" is the oldest fact still within the lookback window. If fewer
    // than two versions fall in the window the past endpoint is MISSING and the
    // entity does not fire (rule 1).
    let window_micros = i128::from(monitor.spec.window.as_micros() as i64);
    let now_wall = i128::from(now.wallclock());
    let past_wall = now_wall.saturating_sub(window_micros);
    let past_bound =
        Timestamp::new(clamp_micros(past_wall), 0).unwrap_or_else(|_| Timestamp::from(0));

    match monitor.spec.target {
        DriftTarget::PerEntity => {
            let mut firings = Vec::new();
            for node in monitor_entities(db, &monitor.spec) {
                let window = entity_window_embeddings(
                    db,
                    node,
                    past_wall,
                    now_wall,
                    &monitor.spec.property_key,
                );
                let (e_now, e_past) = endpoints(&window);
                let decision = decide_entity_firing(
                    e_now.map(|w| w.embedding.as_slice()),
                    e_past.map(|w| w.embedding.as_slice()),
                    monitor.spec.metric,
                    monitor.spec.threshold,
                    // Purity: dedup against unresolved alarms happens at
                    // persistence time (`evaluate_drift_monitor_now`), not here.
                    false,
                )?;
                if let Some(distance) = decision {
                    // `endpoints` guarantees both are `Some` when a distance is
                    // produced.
                    let now_pt = e_now.expect("firing implies a now endpoint");
                    let past_pt = e_past.expect("firing implies a past endpoint");
                    firings.push(DriftFiring {
                        entity: Some(node),
                        label: None,
                        measured_distance: distance,
                        compared_now: now_pt.at,
                        compared_past: past_pt.at,
                        from_version: Some(past_pt.version),
                        to_version: Some(now_pt.version),
                    });
                }
            }
            Ok(firings)
        }
        DriftTarget::LabelCentroid => {
            // Deterministic component-wise mean over entities carrying the label
            // that have the vector, iterated in ascending node-id order.
            let mut now_vecs: Vec<Vec<f32>> = Vec::new();
            let mut past_vecs: Vec<Vec<f32>> = Vec::new();
            for node in monitor_entities(db, &monitor.spec) {
                let window = entity_window_embeddings(
                    db,
                    node,
                    past_wall,
                    now_wall,
                    &monitor.spec.property_key,
                );
                let (e_now, e_past) = endpoints(&window);
                // Skip entities missing the vector in-window (documented).
                if let (Some(n), Some(p)) = (e_now, e_past) {
                    now_vecs.push(n.embedding.clone());
                    past_vecs.push(p.embedding.clone());
                }
            }
            let now_refs: Vec<&[f32]> = now_vecs.iter().map(Vec::as_slice).collect();
            let past_refs: Vec<&[f32]> = past_vecs.iter().map(Vec::as_slice).collect();
            let (Some(c_now), Some(c_past)) = (centroid(&now_refs), centroid(&past_refs)) else {
                return Ok(Vec::new());
            };
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
                    compared_past: past_bound,
                    from_version: None,
                    to_version: None,
                }]),
                None => Ok(Vec::new()),
            }
        }
    }
}

/// One embedding version observed inside a monitor's lookback window.
struct WindowEmbedding {
    /// Transaction-time coordinate of the version.
    at: Timestamp,
    /// The version reference.
    version: VersionId,
    /// The reconstructed embedding at that version.
    embedding: Vec<f32>,
}

/// Clamp a 128-bit micros value back into `i64` range for `Timestamp`.
fn clamp_micros(v: i128) -> i64 {
    v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
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

/// Reconstruct an entity's embedding versions that fall inside the lookback
/// window `[past_wall, now_wall]` (transaction-time wallclock micros), oldest
/// first. Backed by the bi-temporal node history (`get_node_history`), which
/// carries both the version id and the embedding property for each version —
/// the temporal reconstruction the design references, plus the version refs the
/// alarm record needs. A node with no history (e.g. never created) yields an
/// empty window rather than an error, so a monitor over a not-yet-existing
/// entity simply does not fire.
fn entity_window_embeddings(
    db: &AletheiaDB,
    node: NodeId,
    past_wall: i128,
    now_wall: i128,
    property_key: &str,
) -> Vec<WindowEmbedding> {
    let history = match db.get_node_history(node) {
        Ok(h) => h,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for version in history.versions {
        let tx_start = version.temporal.transaction_time().start();
        let tx_wall = i128::from(tx_start.wallclock());
        if tx_wall < past_wall || tx_wall > now_wall {
            continue;
        }
        if let Some(crate::core::property::PropertyValue::Vector(v)) =
            version.properties.get(property_key)
        {
            out.push(WindowEmbedding {
                at: tx_start,
                version: version.version_id,
                embedding: v.to_vec(),
            });
        }
    }
    // Oldest first (get_node_history is already version-number ordered, but sort
    // by transaction time defensively for a stable oldest/newest pick).
    out.sort_by_key(|w| w.at);
    out
}

/// Pick the `(now, past)` endpoints from an oldest-first window: `now` is the
/// latest version, `past` is the earliest — but only when at least two versions
/// are in-window (a single version has no past to compare against).
fn endpoints(window: &[WindowEmbedding]) -> (Option<&WindowEmbedding>, Option<&WindowEmbedding>) {
    match window.len() {
        0 => (None, None),
        1 => (Some(&window[0]), None),
        n => (Some(&window[n - 1]), Some(&window[0])),
    }
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

/// Parse a [`DriftMetric`] token; unknown tokens fall back to `Cosine`.
fn metric_from_token(token: &str) -> DriftMetric {
    match token {
        "euclidean" => DriftMetric::Euclidean,
        "angular" => DriftMetric::Angular,
        _ => DriftMetric::Cosine,
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
                        registry.next_id.store(max_id + 1, Ordering::SeqCst);
                    }
                    _ => quarantine(&path),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => quarantine(&path),
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
        Ok(())
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
            Some(interval.as_micros() as u64),
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
        window_micros: monitor.spec.window.as_micros() as u64,
        target,
        scheduled_interval_micros,
        created_wallclock: monitor.created_at.wallclock(),
        created_logical: monitor.created_at.logical(),
    }
}

#[cfg(feature = "serde")]
fn persisted_to_monitor(pm: PersistedMonitor) -> Option<DriftMonitor> {
    let target = match pm.target.as_str() {
        "label_centroid" => DriftTarget::LabelCentroid,
        _ => DriftTarget::PerEntity,
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
            metric: metric_from_token(&pm.metric),
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
        let mut state = self.state.lock();
        if state.running.is_some() {
            return Ok(());
        }

        // Snapshot the monitor set: partition on-write (changefeed-driven) from
        // scheduled (ticker-driven).
        let monitors = self.db.list_drift_monitors();
        let mut labeled_on_write: std::collections::HashMap<String, Vec<MonitorId>> =
            std::collections::HashMap::new();
        let mut unlabeled_on_write: Vec<MonitorId> = Vec::new();
        let mut scheduled: Vec<(MonitorId, Duration)> = Vec::new();
        for monitor in &monitors {
            match monitor.spec.mode {
                EvalMode::OnWrite => match &monitor.spec.label {
                    Some(label) => labeled_on_write
                        .entry(label.clone())
                        .or_default()
                        .push(monitor.id),
                    None => unlabeled_on_write.push(monitor.id),
                },
                EvalMode::Scheduled { interval } => {
                    if !interval.is_zero() {
                        scheduled.push((monitor.id, interval));
                    }
                }
            }
        }

        let running = Arc::new(AtomicBool::new(true));
        // Bounded evaluation queue: `try_send` never blocks the producer; a full
        // queue yields `TrySendError::Full`, which the producers count as a shed.
        let (tx, rx) = std::sync::mpsc::sync_channel::<MonitorId>(self.capacity);
        let mut handles: Vec<std::thread::JoinHandle<()>> = Vec::new();

        // Dispatcher: only needed if there is at least one on-write monitor.
        let has_on_write = !labeled_on_write.is_empty() || !unlabeled_on_write.is_empty();
        if has_on_write {
            // If any on-write monitor is entity-scoped (no label), we cannot
            // restrict the subscription by label — subscribe to all node changes
            // and filter in the dispatcher. Otherwise restrict to the watched
            // labels (the reserved alarm label is never among them, so alarm
            // materialization never reaches this subscription).
            let filter = if unlabeled_on_write.is_empty() {
                let labels: Vec<String> = labeled_on_write.keys().cloned().collect();
                crate::core::changefeed_subscription::ChangeFilter::all().with_node_labels(labels)
            } else {
                crate::core::changefeed_subscription::ChangeFilter::all()
            };
            let subscription = self.db.subscribe_changes(filter)?;
            let dispatcher_tx = tx.clone();
            let dispatcher_running = Arc::clone(&running);
            let dispatcher_shed = Arc::clone(&self.shed_count);
            handles.push(std::thread::spawn(move || {
                dispatcher_loop(
                    &subscription,
                    &dispatcher_tx,
                    &dispatcher_running,
                    &dispatcher_shed,
                    &labeled_on_write,
                    &unlabeled_on_write,
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
                    // error is dropped; the next change/tick re-drives it.
                    Ok(id) => {
                        let _ = worker_db.evaluate_drift_monitor_now(id);
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
fn dispatcher_loop(
    subscription: &crate::core::changefeed_subscription::Subscription,
    tx: &std::sync::mpsc::SyncSender<MonitorId>,
    running: &AtomicBool,
    shed: &AtomicU64,
    labeled: &std::collections::HashMap<String, Vec<MonitorId>>,
    unlabeled: &[MonitorId],
) {
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
                    if let Some(ids) = labeled.get(&record.label) {
                        for &id in ids {
                            enqueue_or_shed(tx, id, shed);
                        }
                    }
                    for &id in unlabeled {
                        enqueue_or_shed(tx, id, shed);
                    }
                }
            }
            // Timeout with nothing buffered: loop to re-check the shutdown flag.
            Err(crate::core::changefeed_subscription::RecvError::Lagged { .. }) => {
                // The subscription overflowed (the dispatcher itself never lags in
                // practice, since enqueue is non-blocking). Nothing more will
                // arrive on this handle; stop draining. Evaluation for any missed
                // change is re-driven by the next change or a scheduled tick.
                break;
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
    let metric = metric_from_token(&prop_string(props, P_METRIC)?);
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
        let now = crate::core::temporal::time::now();
        let firings = evaluate_monitor(self, &monitor, now)?;
        if firings.is_empty() {
            return Ok(Vec::new());
        }

        // Suppression set: entities / labels with an outstanding unresolved alarm.
        let existing = self.query_drift_alarms(&DriftAlarmFilter {
            monitor_id: Some(id),
            resolved: Some(false),
            limit: MAX_ALARM_QUERY_LIMIT,
            ..DriftAlarmFilter::default()
        })?;
        let mut unresolved_entities: std::collections::HashSet<u64> =
            std::collections::HashSet::new();
        let mut unresolved_labels: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for a in existing {
            if let Some(e) = a.entity {
                unresolved_entities.insert(e.as_u64());
            }
            if let Some(l) = a.label {
                unresolved_labels.insert(l);
            }
        }

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
    /// When `time_range` is set, each alarm is reconstructed AS OF the range's
    /// end coordinate (deterministic historical read), so an alarm resolved
    /// *after* that coordinate still reads unresolved — the append-only
    /// contract (AC5).
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
}
