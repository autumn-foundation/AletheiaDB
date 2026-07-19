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
//! This is a **gated scaffold** (cohort `semantic-temporal`). The public API is
//! defined and compiles; the replay/materialization bodies are stubs that return
//! [`CounterfactualError::Unimplemented`] pending the implementation wave. See
//! `docs/plans/2026-07-19-counterfactual-replay.md` for the full design,
//! including the AC2 orphaned-update contract and the verified reconstruction
//! mechanism.
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
//! let view = db.counterfactual_replay("no-poison", predicate, CounterfactualConfig::default());
//! // (Scaffold: returns `Unimplemented` until the replay wave lands.)
//! assert!(view.is_err());
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "semantic-temporal"))]
//! # fn main() {}
//! ```

use crate::AletheiaDB;
use crate::core::graph::{Edge, Node};
use crate::core::history::EntityHistory;
use crate::core::id::{EdgeId, EntityId, NodeId};
use crate::core::provenance::{Provenance, ProvenanceFilter};
use crate::core::temporal::Timestamp;

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
    // These fields are read only by `excludes`, which the materialization wave
    // (and the unit tests) drive; allowed dead-code until that wave lands so the
    // gated scaffold stays warning-clean under `-D warnings`.
    /// The set of sources to exclude (any-of match), lifted into the shared
    /// [`ProvenanceFilter`] semantics. `None` means "no source constraint" — an
    /// inactive predicate that excludes nothing.
    #[allow(dead_code)]
    filter: Option<ProvenanceFilter>,
    /// Inclusive lower bound on a write's transaction time for it to be excluded.
    #[allow(dead_code)]
    tx_from: Option<Timestamp>,
    /// Exclusive upper bound on a write's transaction time for it to be excluded.
    #[allow(dead_code)]
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
    #[allow(dead_code)] // driven by the materialization wave and the unit tests
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
    // Shadow storage (fresh CurrentStorage + HistoricalStorage) is attached here
    // by the materialization wave; omitted from the scaffold.
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
    /// # Errors
    ///
    /// Returns [`CounterfactualError::Unimplemented`] in the scaffold; will return
    /// [`CounterfactualError::NotFound`] for a node absent from the view.
    pub fn get_node(&self, _id: NodeId) -> Result<Node, CounterfactualError> {
        Err(CounterfactualError::Unimplemented)
    }

    /// Read an edge's current state in the counterfactual view.
    ///
    /// # Errors
    ///
    /// Returns [`CounterfactualError::Unimplemented`] in the scaffold; will return
    /// [`CounterfactualError::NotFound`] for an edge absent from the view.
    pub fn get_edge(&self, _id: EdgeId) -> Result<Edge, CounterfactualError> {
        Err(CounterfactualError::Unimplemented)
    }

    /// Read a node as of a bi-temporal coordinate in the counterfactual view
    /// (AC3).
    ///
    /// # Errors
    ///
    /// Returns [`CounterfactualError::Unimplemented`] in the scaffold.
    pub fn get_node_at_time(
        &self,
        _id: NodeId,
        _valid_time: Timestamp,
        _transaction_time: Timestamp,
    ) -> Result<Node, CounterfactualError> {
        Err(CounterfactualError::Unimplemented)
    }

    /// Read a node's full version history in the counterfactual view (AC3).
    ///
    /// # Errors
    ///
    /// Returns [`CounterfactualError::Unimplemented`] in the scaffold.
    pub fn get_node_history(&self, _id: NodeId) -> Result<EntityHistory, CounterfactualError> {
        Err(CounterfactualError::Unimplemented)
    }
}

/// Errors from counterfactual replay (Issue #3357).
///
/// Maps to the #3234 MCP structured error codes when the MCP surface lands (a
/// deferred follow-up): `HistoryTooLarge` → `FAILED_PRECONDITION`, `NotFound` →
/// `NOT_FOUND`, `Internal` → `INTERNAL` — all non-retriable.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CounterfactualError {
    /// The requested operation is not yet implemented (scaffold placeholder).
    #[error("counterfactual replay is not yet implemented")]
    Unimplemented,
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
    /// Returns [`CounterfactualError::Unimplemented`] in the scaffold. Once
    /// implemented it will return [`CounterfactualError::HistoryTooLarge`] when
    /// the recorded version count exceeds `config.max_replay_versions` (AC8).
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
        // Scaffold: bind the arguments so the signature is exercised, then defer
        // to the materialization wave. Real body will: enumerate recorded
        // versions in ChangeCursor order, filter via `predicate.excludes`, cap at
        // `config.max_replay_versions`, restore survivors into fresh shadow
        // storage (preserving bi-temporal coordinates), and build the report.
        let _ = (name.into(), predicate, config);
        Err(CounterfactualError::Unimplemented)
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
    fn replay_scaffold_returns_unimplemented() {
        let db = AletheiaDB::new().expect("db");
        let result = db.counterfactual_replay(
            "test-view",
            ExclusionPredicate::source("x"),
            CounterfactualConfig::default(),
        );
        assert!(matches!(result, Err(CounterfactualError::Unimplemented)));
    }
}
