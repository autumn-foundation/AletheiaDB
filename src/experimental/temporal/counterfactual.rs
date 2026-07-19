//! Counterfactual exclusion replay — *the world without source X* (Issue #3357).
//!
//! A **counterfactual view** materializes a read-only shadow of the database as
//! it would exist had a named source's writes never been recorded. Recorded
//! history is replayed in transaction-time order with writes matching an
//! *exclusion predicate* over provenance omitted, and the survivors are restored
//! into a fresh, physically separate shadow storage — so the real database is
//! never mutated (AC4) and the view is fully queryable through the existing
//! read surfaces, including `AS OF` and history reads with their bi-temporal
//! coordinates preserved (AC3).
//!
//! This answers questions no incumbent can even express: *"how much did the
//! poisoned feed contaminate?"*, *"does removing this low-confidence scraper
//! change any answers?"*, *"what does this expensive feed actually contribute?"*
//! — each in one materialization plus one divergence-report read.
//!
//! # Status
//!
//! Implemented (cohort `semantic-temporal`, Rust API). Materialization enumerates
//! recorded history, folds out the excluded writes, and restores the survivors as
//! anchors into a fresh, physically separate shadow [`HistoricalStorage`] whose
//! reads reuse the engine's point-in-time / history reconstruction. See
//! `docs/plans/2026-07-19-counterfactual-replay.md` for the full design,
//! including the AC2 orphaned-update contract and the reconstruction mechanism.
//! The MCP surface is a deferred follow-up (design §9).
//!
//! # Exclusion-replay semantics (AC2, summary)
//!
//! Survivors keep their exact `(valid, transaction)` intervals. A later write by
//! a surviving source that targets an entity with no surviving prior version is
//! *unappliable*: it is dropped and counted as an orphaned update rather than
//! promoted to a create (promoting it would re-introduce the excluded source's
//! carried-forward properties — see the design doc). Writes recorded without
//! provenance never match a source predicate and are never excluded (AC7).
//!
//! # Example
//!
//! ```rust,no_run
//! # #[cfg(feature = "semantic-temporal")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::temporal::counterfactual::{
//!     CounterfactualConfig, ExclusionPredicate,
//! };
//!
//! let db = AletheiaDB::new()?;
//! // "What would we believe if `poisoned-feed` had never written?"
//! let predicate = ExclusionPredicate::source("poisoned-feed");
//! let view = db.counterfactual_replay("no-poison", predicate, CounterfactualConfig::default())?;
//! assert!(view.is_counterfactual());
//! assert_eq!(view.handle().name(), "no-poison");
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "semantic-temporal"))]
//! # fn main() {}
//! ```

use crate::AletheiaDB;
use crate::core::graph::{Edge, Node};
use crate::core::history::{EntityHistory, VersionInfo};
use crate::core::id::{EdgeId, EntityId, NodeId, VersionId};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyMap;
use crate::core::provenance::{Provenance, ProvenanceFilter};
use crate::core::temporal::{BiTemporalInterval, TimeRange, Timestamp, time};
use crate::core::version::{EdgeVersion, NodeVersion, PropertyDelta};
use crate::storage::historical::HistoricalStorage;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Default cap on the number of recorded versions a single counterfactual replay
/// will materialize before failing with [`CounterfactualError::HistoryTooLarge`].
///
/// Chosen larger than the point-in-time scan cap (`max_schema_as_of_entities`,
/// default 50,000) because replay materializes whole-history *version records*, a
/// categorically larger working set than an entity scan. See the design doc §8.
pub const DEFAULT_MAX_REPLAY_VERSIONS: usize = 5_000_000;

/// A predicate over write-time [`Provenance`] deciding which recorded writes are
/// *excluded* from a counterfactual replay (Issue #3357, AC1).
///
/// At minimum this expresses "source equals `S`" ([`ExclusionPredicate::source`])
/// and "source in `{S1..Sn}`" ([`ExclusionPredicate::sources`]), optionally
/// bounded to a transaction-time range
/// ([`ExclusionPredicate::within_transaction_time`]).
///
/// **Unattributed writes are never excluded** (AC7): a write with no provenance
/// bundle matches no source predicate. Internally this reuses [`ProvenanceFilter`]
/// (Issue #3348), whose `matches(None)` is `false`, so the caveat is enforced by
/// construction.
#[derive(Debug, Clone)]
pub struct ExclusionPredicate {
    /// The set of sources to exclude (any-of match), lifted into the shared
    /// [`ProvenanceFilter`] semantics. `None` means "no source constraint" — an
    /// inactive predicate that excludes nothing.
    filter: Option<ProvenanceFilter>,
    /// Inclusive lower bound on a write's transaction time for it to be excluded.
    tx_from: Option<Timestamp>,
    /// Exclusive upper bound on a write's transaction time for it to be excluded.
    tx_to: Option<Timestamp>,
}

impl ExclusionPredicate {
    /// Exclude all writes attributed to a single `source`.
    #[must_use]
    pub fn source(source: impl Into<String>) -> Self {
        let filter = ProvenanceFilter::validated(Some(source.into()), None, None, false)
            .ok()
            .flatten();
        Self {
            filter,
            tx_from: None,
            tx_to: None,
        }
    }

    /// Exclude all writes attributed to any source in the provided set
    /// (any-of match).
    #[must_use]
    pub fn sources(sources: impl IntoIterator<Item = String>) -> Self {
        let list: Vec<String> = sources.into_iter().collect();
        let filter = ProvenanceFilter::validated(None, Some(list), None, false)
            .ok()
            .flatten();
        Self {
            filter,
            tx_from: None,
            tx_to: None,
        }
    }

    /// Bound exclusion to writes whose transaction time falls in
    /// `[from, to)` (each bound optional; `from` inclusive, `to` exclusive).
    ///
    /// A write outside the bound is *kept* even if its source matches, so a
    /// caller can excise only the window a compromised source was active.
    #[must_use]
    pub fn within_transaction_time(
        mut self,
        from: Option<Timestamp>,
        to: Option<Timestamp>,
    ) -> Self {
        self.tx_from = from;
        self.tx_to = to;
        self
    }

    /// Whether the write bearing `provenance`, recorded at `tx_time`, is excluded
    /// by this predicate.
    ///
    /// Returns `false` for unattributed writes (`provenance.is_none()`), enforcing
    /// the AC7 caveat. A write is excluded iff its source matches **and** its
    /// transaction time falls within the optional `[tx_from, tx_to)` bound.
    #[must_use]
    pub(crate) fn excludes(&self, provenance: Option<&Provenance>, tx_time: Timestamp) -> bool {
        let Some(filter) = &self.filter else {
            return false;
        };
        if let Some(from) = self.tx_from
            && tx_time < from
        {
            return false;
        }
        if let Some(to) = self.tx_to
            && tx_time >= to
        {
            return false;
        }
        filter.matches(provenance)
    }
}

/// Guardrails for a counterfactual replay (Issue #3357, AC8).
#[derive(Debug, Clone, Copy)]
pub struct CounterfactualConfig {
    /// Maximum number of recorded versions to materialize before failing with a
    /// structured [`CounterfactualError::HistoryTooLarge`]. Defaults to
    /// [`DEFAULT_MAX_REPLAY_VERSIONS`].
    pub max_replay_versions: usize,
}

impl Default for CounterfactualConfig {
    fn default() -> Self {
        Self {
            max_replay_versions: DEFAULT_MAX_REPLAY_VERSIONS,
        }
    }
}

/// The blast-radius report produced when a counterfactual view is materialized
/// (Issue #3357, AC5).
///
/// Every field is a deterministic function of the recorded history and the
/// exclusion predicate, so two materializations with the same inputs produce
/// identical reports (AC6).
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct DivergenceReport {
    /// Number of recorded writes (versions) excluded by the predicate.
    excluded_writes: usize,
    /// Number of unattributed writes (no provenance) encountered during replay —
    /// surfaced so callers understand the AC7 unattributed-data caveat.
    unattributed_writes_encountered: usize,
    /// Number of later writes dropped as *unappliable* because their target had no
    /// surviving prior version at that point in the replay (AC2 orphaned updates).
    orphaned_updates: usize,
    /// Number of entities whose current state differs between the real and
    /// counterfactual views (but still exist).
    entities_changed: usize,
    /// Number of entities removed entirely (their whole version chain excluded).
    entities_removed: usize,
    /// The entities whose current state changed.
    #[cfg_attr(feature = "serde", serde(serialize_with = "serialize_entity_ids"))]
    changed_entities: Vec<EntityId>,
    /// The entities removed entirely.
    #[cfg_attr(feature = "serde", serde(serialize_with = "serialize_entity_ids"))]
    removed_entities: Vec<EntityId>,
}

/// Serialize a slice of [`EntityId`] as a sequence of their `Display` strings
/// (e.g. `"Node(7)"`), since [`EntityId`] does not implement `Serialize`.
#[cfg(feature = "serde")]
fn serialize_entity_ids<S>(ids: &[EntityId], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = serializer.serialize_seq(Some(ids.len()))?;
    for id in ids {
        seq.serialize_element(&id.to_string())?;
    }
    seq.end()
}

impl DivergenceReport {
    /// Count of recorded writes excluded by the predicate.
    #[must_use]
    pub fn excluded_writes(&self) -> usize {
        self.excluded_writes
    }

    /// Count of unattributed writes (no provenance) encountered during replay.
    #[must_use]
    pub fn unattributed_writes_encountered(&self) -> usize {
        self.unattributed_writes_encountered
    }

    /// Count of later writes dropped as unappliable orphaned updates (AC2).
    #[must_use]
    pub fn orphaned_updates(&self) -> usize {
        self.orphaned_updates
    }

    /// Count of entities whose current state differs between real and view.
    #[must_use]
    pub fn entities_changed(&self) -> usize {
        self.entities_changed
    }

    /// Count of entities removed entirely from the view.
    #[must_use]
    pub fn entities_removed(&self) -> usize {
        self.entities_removed
    }

    /// The entities whose current state changed.
    #[must_use]
    pub fn changed_entities(&self) -> &[EntityId] {
        &self.changed_entities
    }

    /// The entities removed entirely.
    #[must_use]
    pub fn removed_entities(&self) -> &[EntityId] {
        &self.removed_entities
    }
}

/// A handle naming a materialized counterfactual view (Issue #3357, AC1).
///
/// The name surfaces in every response so a caller can never mistake a
/// counterfactual answer for real state (AC8).
#[derive(Debug, Clone)]
pub struct CounterfactualHandle {
    name: String,
}

impl CounterfactualHandle {
    /// The caller-supplied name of the view.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A materialized, read-only counterfactual view of the database (Issue #3357).
///
/// Owns a physically separate shadow storage; dropping the view reclaims it,
/// leaving the real database byte-identical (AC4). Reads delegate to the existing
/// historical read implementations bound to the shadow storage, so `AS OF` and
/// history reads behave as if the excluded writes never happened, including
/// bi-temporal coordinates (AC3).
///
/// Every response is labeled counterfactual via [`CounterfactualView::is_counterfactual`].
pub struct CounterfactualView {
    handle: CounterfactualHandle,
    report: DivergenceReport,
    /// Physically separate shadow storage holding only the surviving versions
    /// (with their original bi-temporal valid intervals). Owned by the view, so
    /// dropping the view reclaims it and the real database is never touched (AC4).
    shadow: HistoricalStorage,
}

impl CounterfactualView {
    /// The divergence report produced at materialization (AC5).
    #[must_use]
    pub fn report(&self) -> &DivergenceReport {
        &self.report
    }

    /// The handle naming this view (AC1).
    #[must_use]
    pub fn handle(&self) -> &CounterfactualHandle {
        &self.handle
    }

    /// Always `true`: every counterfactual view response is labeled as such so no
    /// caller mistakes counterfactual answers for real ones (AC8).
    #[must_use]
    pub fn is_counterfactual(&self) -> bool {
        true
    }

    /// Read a node's current state in the counterfactual view.
    ///
    /// Resolves the node as of *now* against the shadow timeline, so a node whose
    /// surviving chain was excluded entirely (or whose surviving head is a
    /// retraction) reads as absent.
    ///
    /// # Errors
    ///
    /// Returns [`CounterfactualError::NotFound`] for a node absent from the view.
    pub fn get_node(&self, id: NodeId) -> Result<Node, CounterfactualError> {
        let now = time::now();
        self.shadow
            .get_node_at_time(id, now, now)
            .map_err(|_| CounterfactualError::NotFound(format!("node {id}")))
    }

    /// Read an edge's current state in the counterfactual view.
    ///
    /// # Errors
    ///
    /// Returns [`CounterfactualError::NotFound`] for an edge absent from the view.
    pub fn get_edge(&self, id: EdgeId) -> Result<Edge, CounterfactualError> {
        let now = time::now();
        self.shadow
            .get_edge_at_time(id, now, now)
            .map_err(|_| CounterfactualError::NotFound(format!("edge {id}")))
    }

    /// Read a node as of a bi-temporal coordinate in the counterfactual view
    /// (AC3).
    ///
    /// The excluded writes never happened on the shadow timeline, so an `AS OF`
    /// read cannot resurrect an entirely-excluded node and reflects the surviving
    /// state at `(valid_time, transaction_time)`.
    ///
    /// # Errors
    ///
    /// Returns [`CounterfactualError::NotFound`] when no surviving version is
    /// visible at the requested coordinate.
    pub fn get_node_at_time(
        &self,
        id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Node, CounterfactualError> {
        self.shadow
            .get_node_at_time(id, valid_time, transaction_time)
            .map_err(|_| CounterfactualError::NotFound(format!("node {id}")))
    }

    /// Read an edge as of a bi-temporal coordinate in the counterfactual view
    /// (AC3).
    ///
    /// # Errors
    ///
    /// Returns [`CounterfactualError::NotFound`] when no surviving version is
    /// visible at the requested coordinate.
    pub fn get_edge_at_time(
        &self,
        id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Edge, CounterfactualError> {
        self.shadow
            .get_edge_at_time(id, valid_time, transaction_time)
            .map_err(|_| CounterfactualError::NotFound(format!("edge {id}")))
    }

    /// Read a node's full version history in the counterfactual view (AC3).
    ///
    /// The returned history contains only surviving versions — none attributed to
    /// an excluded source — with their original bi-temporal coordinates.
    ///
    /// # Errors
    ///
    /// Returns [`CounterfactualError::NotFound`] for a node with no surviving
    /// versions in the view.
    pub fn get_node_history(&self, id: NodeId) -> Result<EntityHistory, CounterfactualError> {
        self.shadow
            .get_node_history(id)
            .map_err(|_| CounterfactualError::NotFound(format!("node {id}")))
    }

    /// Read an edge's full version history in the counterfactual view (AC3).
    ///
    /// # Errors
    ///
    /// Returns [`CounterfactualError::NotFound`] for an edge with no surviving
    /// versions in the view.
    pub fn get_edge_history(&self, id: EdgeId) -> Result<EntityHistory, CounterfactualError> {
        self.shadow
            .get_edge_history(id)
            .map_err(|_| CounterfactualError::NotFound(format!("edge {id}")))
    }

    /// The outgoing edges of a node in the counterfactual view, resolved as of
    /// *now* against the shadow timeline.
    ///
    /// Provided so callers can traverse the counterfactual graph; edges whose
    /// creating write was excluded (or whose surviving head is retracted) are
    /// absent. Endpoints of a surviving edge whose node was removed entirely are
    /// returned as-is (dangling), mirroring the engine's documented
    /// orphaned-edge behavior. Edges are returned sorted by id for determinism.
    #[must_use]
    pub fn get_outgoing_edges(&self, id: NodeId) -> Vec<Edge> {
        self.adjacent_edges(id, true)
    }

    /// The incoming edges of a node in the counterfactual view, resolved as of
    /// *now* against the shadow timeline. Edges are returned sorted by id.
    #[must_use]
    pub fn get_incoming_edges(&self, id: NodeId) -> Vec<Edge> {
        self.adjacent_edges(id, false)
    }

    /// Collect the surviving edges adjacent to `id` (outgoing when `outgoing`,
    /// else incoming) as of now, by scanning the shadow edge versions. The
    /// shadow carries no adjacency index, so endpoints are read off the version
    /// records directly and presence is verified through the point-in-time read.
    fn adjacent_edges(&self, id: NodeId, outgoing: bool) -> Vec<Edge> {
        let now = time::now();
        let mut endpoints: BTreeMap<EdgeId, NodeId> = BTreeMap::new();
        for version in self.shadow.get_edge_versions().values() {
            let endpoint = if outgoing {
                version.source
            } else {
                version.target
            };
            endpoints.entry(version.edge_id).or_insert(endpoint);
        }
        let mut edges = Vec::new();
        for (edge_id, endpoint) in endpoints {
            if endpoint == id
                && let Ok(edge) = self.shadow.get_edge_at_time(edge_id, now, now)
            {
                edges.push(edge);
            }
        }
        edges
    }
}

/// Errors from counterfactual replay (Issue #3357).
///
/// Maps to the #3234 MCP structured error codes when the MCP surface lands (a
/// deferred follow-up): `HistoryTooLarge` → `FAILED_PRECONDITION`, `NotFound` →
/// `NOT_FOUND`, `Internal` → `INTERNAL` — all non-retriable.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CounterfactualError {
    /// The recorded history exceeds the configured replay cap (AC8).
    #[error(
        "recorded history too large for counterfactual replay: {versions} versions exceeds cap of {cap}"
    )]
    HistoryTooLarge {
        /// The number of recorded versions that would be materialized.
        versions: usize,
        /// The configured [`CounterfactualConfig::max_replay_versions`] cap.
        cap: usize,
    },
    /// The requested entity does not exist in the counterfactual view.
    #[error("entity not found in counterfactual view: {0}")]
    NotFound(String),
    /// An internal error occurred while materializing or reading the view.
    #[error("internal counterfactual replay error: {0}")]
    Internal(String),
}

impl AletheiaDB {
    /// Materialize a read-only counterfactual view excluding writes matching
    /// `predicate` from recorded history (Issue #3357).
    ///
    /// Returns a [`CounterfactualView`] handle naming the view, its divergence
    /// report, and shadow-bound read methods. The real database is never mutated
    /// (AC4).
    ///
    /// # Errors
    ///
    /// Returns [`CounterfactualError::HistoryTooLarge`] when the recorded version
    /// count exceeds `config.max_replay_versions` (AC8), or
    /// [`CounterfactualError::Internal`] on an unexpected storage error while
    /// enumerating or restoring history.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[cfg(feature = "semantic-temporal")]
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use aletheiadb::AletheiaDB;
    /// use aletheiadb::experimental::temporal::counterfactual::{
    ///     CounterfactualConfig, ExclusionPredicate,
    /// };
    ///
    /// let db = AletheiaDB::new()?;
    /// let predicate = ExclusionPredicate::sources(["scraper-a".into(), "scraper-b".into()]);
    /// let _ = db.counterfactual_replay("no-scrapers", predicate, CounterfactualConfig::default());
    /// # Ok(())
    /// # }
    /// # #[cfg(not(feature = "semantic-temporal"))]
    /// # fn main() {}
    /// ```
    pub fn counterfactual_replay(
        &self,
        name: impl Into<String>,
        predicate: ExclusionPredicate,
        config: CounterfactualConfig,
    ) -> Result<CounterfactualView, CounterfactualError> {
        let name = name.into();

        // --- Enumerate recorded history under a single read guard. ---------
        // Collect owned histories (and edge endpoints) so the guard is released
        // before we build the physically-separate shadow storage.
        let (node_histories, edge_histories) = {
            let hist = self.historical.read();

            // AC8 guardrail: fail fast, before allocating any shadow storage.
            let total_versions = hist.get_node_versions().len() + hist.get_edge_versions().len();
            if total_versions > config.max_replay_versions {
                return Err(CounterfactualError::HistoryTooLarge {
                    versions: total_versions,
                    cap: config.max_replay_versions,
                });
            }

            // Unique entity ids, iterated in sorted order for determinism (AC6).
            let mut node_ids: BTreeSet<NodeId> = BTreeSet::new();
            for version in hist.get_node_versions().values() {
                node_ids.insert(version.node_id);
            }
            // Edge endpoints are fixed at creation; record them once per edge.
            let mut edge_endpoints: BTreeMap<EdgeId, (NodeId, NodeId)> = BTreeMap::new();
            for version in hist.get_edge_versions().values() {
                edge_endpoints
                    .entry(version.edge_id)
                    .or_insert((version.source, version.target));
            }

            let mut node_histories: Vec<(NodeId, EntityHistory)> =
                Vec::with_capacity(node_ids.len());
            for id in node_ids {
                let history = hist
                    .get_node_history(id)
                    .map_err(|e| CounterfactualError::Internal(e.to_string()))?;
                node_histories.push((id, history));
            }
            let mut edge_histories: Vec<(EdgeId, NodeId, NodeId, EntityHistory)> =
                Vec::with_capacity(edge_endpoints.len());
            for (id, (source, target)) in edge_endpoints {
                let history = hist
                    .get_edge_history(id)
                    .map_err(|e| CounterfactualError::Internal(e.to_string()))?;
                edge_histories.push((id, source, target, history));
            }
            (node_histories, edge_histories)
        };

        // --- Fold each entity's history minus the excluded writes. ---------
        let mut shadow = HistoricalStorage::new();
        let mut report = DivergenceReport::default();
        let mut changed: Vec<EntityId> = Vec::new();
        let mut removed: Vec<EntityId> = Vec::new();

        for (id, history) in &node_histories {
            let outcome = replay_entity(history, &predicate);
            outcome.accumulate(&mut report);
            classify_divergence(
                EntityId::Node(*id),
                real_current_map(history),
                outcome.shadow_current.as_ref(),
                &mut changed,
                &mut removed,
            );
            for emitted in outcome.emitted {
                let label = GLOBAL_INTERNER
                    .intern(&emitted.label)
                    .map_err(|e| CounterfactualError::Internal(e.to_string()))?;
                let version = NodeVersion::new_anchor(
                    emitted.version_id,
                    *id,
                    emitted.temporal,
                    label,
                    emitted.properties,
                )
                .with_provenance(emitted.provenance.map(Arc::new));
                shadow
                    .insert_restored_node_version(version)
                    .map_err(|e| CounterfactualError::Internal(e.to_string()))?;
            }
        }

        for (id, source, target, history) in &edge_histories {
            let outcome = replay_entity(history, &predicate);
            outcome.accumulate(&mut report);
            classify_divergence(
                EntityId::Edge(*id),
                real_current_map(history),
                outcome.shadow_current.as_ref(),
                &mut changed,
                &mut removed,
            );
            for emitted in outcome.emitted {
                let label = GLOBAL_INTERNER
                    .intern(&emitted.label)
                    .map_err(|e| CounterfactualError::Internal(e.to_string()))?;
                let version = EdgeVersion::new_anchor(
                    emitted.version_id,
                    *id,
                    emitted.temporal,
                    label,
                    *source,
                    *target,
                    emitted.properties,
                )
                .with_provenance(emitted.provenance.map(Arc::new));
                shadow
                    .insert_restored_edge_version(version)
                    .map_err(|e| CounterfactualError::Internal(e.to_string()))?;
            }
        }

        // Reconstruct prev/next links and transaction-time supersession over the
        // SURVIVING chain only (a surviving version that superseded an excluded
        // one now closes at its own surviving successor, not at the excluded
        // write). No temporal index is wired: point-in-time reads fall back to
        // the version-chain scan, which is exact over these small shadow stores.
        shadow.rebuild_version_chains();

        // Deterministic blast-radius ordering (AC6).
        changed.sort_unstable();
        removed.sort_unstable();
        report.entities_changed = changed.len();
        report.entities_removed = removed.len();
        report.changed_entities = changed;
        report.removed_entities = removed;

        Ok(CounterfactualView {
            handle: CounterfactualHandle { name },
            report,
            shadow,
        })
    }
}

/// A surviving version to materialize into the shadow store: the recomputed
/// full property map (with the excluded source's carried-forward fields dropped)
/// plus the original bi-temporal valid interval, label, and provenance.
struct EmittedVersion {
    version_id: VersionId,
    temporal: BiTemporalInterval,
    label: String,
    provenance: Option<Provenance>,
    properties: PropertyMap,
}

/// The per-entity result of an exclusion replay.
struct EntityReplay {
    /// Surviving versions to materialize, oldest-first.
    emitted: Vec<EmittedVersion>,
    /// The entity's current full property map in the view (`None` when the
    /// entity was removed entirely or its surviving head is a retraction).
    shadow_current: Option<PropertyMap>,
    excluded_writes: usize,
    unattributed_writes: usize,
    orphaned_updates: usize,
}

impl EntityReplay {
    /// Fold this entity's counters into the running divergence report.
    fn accumulate(&self, report: &mut DivergenceReport) {
        report.excluded_writes += self.excluded_writes;
        report.unattributed_writes_encountered += self.unattributed_writes;
        report.orphaned_updates += self.orphaned_updates;
    }
}

/// Replay one entity's recorded history with the matching writes excluded.
///
/// Pure over `(history, predicate)` — no I/O, no map-iteration-order leakage —
/// so the same inputs always yield the same result (AC6). Each surviving version
/// is re-evaluated against the *surviving* timeline: its delta relative to its
/// immediate **real** predecessor is applied onto the surviving current map, so
/// an excluded source's untouched carried-forward fields never leak back (AC2,
/// the no-leak-back contract). Equal-valued keys diff as untouched — the
/// documented, deterministic reconstruction.
fn replay_entity(history: &EntityHistory, predicate: &ExclusionPredicate) -> EntityReplay {
    let empty = PropertyMap::default();
    let mut emitted: Vec<EmittedVersion> = Vec::new();
    // The surviving current full map; `None` == entity not present in the view.
    let mut shadow_current: Option<PropertyMap> = None;
    // The immediate *real* predecessor's full map (advances every version,
    // excluded or not, so deltas are always real-to-real).
    let mut real_prev: Option<&PropertyMap> = None;
    let mut excluded_writes = 0usize;
    let mut unattributed_writes = 0usize;
    let mut orphaned_updates = 0usize;

    for version in &history.versions {
        let tx_time = version.temporal.transaction_time().start();
        let is_excluded = predicate.excludes(version.provenance.as_ref(), tx_time);
        if version.provenance.is_none() {
            unattributed_writes += 1;
        }

        // The delta this write introduced relative to its real predecessor.
        let base_prev = real_prev.unwrap_or(&empty);
        let delta = PropertyDelta::from_diff(base_prev, &version.properties);

        if is_excluded {
            excluded_writes += 1;
        } else {
            let is_create = real_prev.is_none();
            // Appliable iff it is the genuine create, or an update/delete whose
            // entity still has a surviving prior version.
            if is_create || shadow_current.is_some() {
                let cf_base = shadow_current.as_ref().unwrap_or(&empty);
                let recomputed = delta.apply(cf_base);
                let valid_closed = version.temporal.valid_time().is_closed();
                emitted.push(EmittedVersion {
                    version_id: version.version_id,
                    // Preserve the original valid interval (incl. a closed
                    // delete/retract tombstone); open the transaction interval so
                    // `rebuild_version_chains` re-derives supersession from the
                    // surviving neighbours rather than the excluded ones.
                    temporal: BiTemporalInterval::new(
                        version.temporal.valid_time(),
                        TimeRange::from(tx_time),
                    ),
                    label: version.label.clone(),
                    provenance: version.provenance.clone(),
                    properties: recomputed.clone(),
                });
                shadow_current = if valid_closed { None } else { Some(recomputed) };
            } else {
                // Update/delete targeting an entity with no surviving prior
                // version: unappliable, dropped and counted (AC2).
                orphaned_updates += 1;
            }
        }

        real_prev = Some(&version.properties);
    }

    EntityReplay {
        emitted,
        shadow_current,
        excluded_writes,
        unattributed_writes,
        orphaned_updates,
    }
}

/// The entity's current full property map in the *real* database, or `None` when
/// its real head is a delete/retraction (closed valid interval).
fn real_current_map(history: &EntityHistory) -> Option<PropertyMap> {
    history.versions.last().and_then(|last: &VersionInfo| {
        if last.temporal.valid_time().is_closed() {
            None
        } else {
            Some(last.properties.clone())
        }
    })
}

/// Classify one entity into the divergence report's changed / removed lists by
/// comparing its real vs shadow current state.
fn classify_divergence(
    entity: EntityId,
    real_current: Option<PropertyMap>,
    shadow_current: Option<&PropertyMap>,
    changed: &mut Vec<EntityId>,
    removed: &mut Vec<EntityId>,
) {
    match (real_current, shadow_current) {
        // Present in reality, gone from the view => removed entirely.
        (Some(_), None) => removed.push(entity),
        // Present in both but the current state differs => changed.
        (Some(real), Some(shadow)) if &real != shadow => changed.push(entity),
        // Absent in reality but present in the view (e.g. an excluded delete) =>
        // its current state changed relative to the real world.
        (None, Some(_)) => changed.push(entity),
        // Identical current state, or absent in both => no divergence.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(micros: i64) -> Timestamp {
        Timestamp::new(micros, 0).expect("valid timestamp")
    }

    fn prov(source: &str) -> Provenance {
        Provenance::builder()
            .source(source)
            .build()
            .expect("valid provenance")
    }

    #[test]
    fn source_predicate_excludes_matching_source() {
        let pred = ExclusionPredicate::source("bad-feed");
        assert!(pred.excludes(Some(&prov("bad-feed")), ts(100)));
        assert!(!pred.excludes(Some(&prov("good-feed")), ts(100)));
    }

    #[test]
    fn unattributed_writes_are_never_excluded() {
        // AC7: a write with no provenance matches no source predicate.
        let pred = ExclusionPredicate::source("bad-feed");
        assert!(!pred.excludes(None, ts(100)));
    }

    #[test]
    fn sources_predicate_matches_any_of() {
        let pred = ExclusionPredicate::sources(["a".into(), "b".into()]);
        assert!(pred.excludes(Some(&prov("a")), ts(100)));
        assert!(pred.excludes(Some(&prov("b")), ts(100)));
        assert!(!pred.excludes(Some(&prov("c")), ts(100)));
    }

    #[test]
    fn transaction_time_bound_gates_exclusion() {
        let pred = ExclusionPredicate::source("bad-feed")
            .within_transaction_time(Some(ts(100)), Some(ts(200)));
        assert!(!pred.excludes(Some(&prov("bad-feed")), ts(50))); // before window
        assert!(pred.excludes(Some(&prov("bad-feed")), ts(150))); // inside window
        assert!(!pred.excludes(Some(&prov("bad-feed")), ts(200))); // upper bound exclusive
        assert!(!pred.excludes(Some(&prov("bad-feed")), ts(250))); // after window
    }

    #[test]
    fn config_default_uses_documented_cap() {
        assert_eq!(
            CounterfactualConfig::default().max_replay_versions,
            DEFAULT_MAX_REPLAY_VERSIONS
        );
    }

    #[test]
    fn divergence_report_defaults_are_zero() {
        let report = DivergenceReport::default();
        assert_eq!(report.excluded_writes(), 0);
        assert_eq!(report.unattributed_writes_encountered(), 0);
        assert_eq!(report.orphaned_updates(), 0);
        assert_eq!(report.entities_changed(), 0);
        assert_eq!(report.entities_removed(), 0);
        assert!(report.changed_entities().is_empty());
        assert!(report.removed_entities().is_empty());
    }

    #[test]
    fn replay_over_empty_db_yields_empty_view() {
        let db = AletheiaDB::new().expect("db");
        let view = db
            .counterfactual_replay(
                "test-view",
                ExclusionPredicate::source("x"),
                CounterfactualConfig::default(),
            )
            .expect("empty replay materializes");
        assert!(view.is_counterfactual());
        assert_eq!(view.handle().name(), "test-view");
        assert_eq!(view.report().excluded_writes(), 0);
        assert_eq!(view.report().entities_changed(), 0);
        assert_eq!(view.report().entities_removed(), 0);
    }
}
