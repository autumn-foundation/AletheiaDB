//! Belief-revision audit — when and why the database changed its mind (Issue #3362).
//!
//! A *belief-revision audit* walks a single node's or edge's already-stored
//! bi-temporal version history and classifies each transition, so a caller (or
//! an LLM) can answer *"why does the database now say Y when it used to say X,
//! and who says so?"* in one call instead of stitching `get_node_history` +
//! provenance lookups by hand.
//!
//! It is a **pure read**: it performs no writes, introduces no storage-format
//! change, and its result is fully determined by `(entity, options, as-of
//! transaction time)` — running the same audit twice at the same coordinate
//! returns byte-identical results.
//!
//! # Classification (deterministic, falsifiable)
//!
//! Each revision is classified purely from bi-temporal interval geometry and the
//! version's provenance (never from NLP over free-text — see the issue's
//! Out-of-Scope). For versions `v[0..n]` ordered oldest-first (after any
//! `as_of_transaction_time` filtering), revision `i` is:
//!
//! | Precedence | Class | Rule |
//! |---|---|---|
//! | 1 | [`RevisionClass::InitialAssertion`] | `i == 0` (first visible version). |
//! | 2 | [`RevisionClass::Retraction`] | this version's valid interval is **closed** (`valid_to != ∞`) — a delete tombstone (empty `[t,t)`) or a #3230 valid-time retraction. |
//! | 3 | [`RevisionClass::Reaffirmation`] | no value change vs the predecessor. |
//! | 4 | [`RevisionClass::WorldChange`] | value changed and `valid_from` advanced beyond every prior `valid_from` — the fact itself changed. |
//! | 5 | [`RevisionClass::Correction`] | value changed and `valid_from` did **not** advance — a later transaction-time rewrite of an already-recorded valid period. |
//!
//! Precedence is strict: a closed valid interval is always a retraction; the
//! `world_change`/`correction` split uses a strict `>` on `valid_from` so equal
//! `valid_from` is a correction.
//!
//! # Worked example — correction vs world-change on the same entity
//!
//! ```rust,no_run
//! # #[cfg(feature = "semantic-temporal")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::core::id::EntityId;
//! use aletheiadb::experimental::temporal::belief_revision::RevisionOptions;
//!
//! let db = AletheiaDB::new()?;
//! # let id = db.create_node("City", Default::default())?;
//! // ... writes: an initial assertion, a typo fix (same valid period => correction),
//! // and a later fact that became true later (advancing valid_from => world_change) ...
//! let log = db.belief_revisions(EntityId::Node(id), &RevisionOptions::default())?;
//! for rev in log.revisions() {
//!     println!("v{} {} — confidence {:?}",
//!         rev.version_number, rev.class.as_str(), rev.confidence);
//! }
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "semantic-temporal"))]
//! # fn main() {}
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, QueryError, Result};
use crate::core::history::{EntityHistory, VersionDiff, VersionInfo};
use crate::core::id::{EntityId, VersionId};
use crate::core::property::PropertyValue;
use crate::core::provenance::Provenance;
use crate::core::temporal::Timestamp;

/// Default number of revisions returned when `RevisionOptions::limit` is unset.
pub const DEFAULT_REVISION_LIMIT: usize = 100;

/// Maximum number of revisions returnable in a single audit; larger `limit`
/// values are clamped down to this bound.
pub const MAX_REVISION_LIMIT: usize = 1000;

/// Deterministic classification of a single belief revision (Issue #3362, AC2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum RevisionClass {
    /// The first (oldest visible) version of the entity.
    InitialAssertion,
    /// A later transaction-time rewrite of an already-recorded valid period
    /// (the recorded value about the same real-world time was fixed).
    Correction,
    /// The fact itself changed: a new/later valid interval was asserted.
    WorldChange,
    /// A deletion tombstone or a #3230 valid-time retraction (valid interval
    /// closed).
    Retraction,
    /// The same value was re-asserted (no value delta vs the predecessor),
    /// typically by a different source.
    Reaffirmation,
}

impl RevisionClass {
    /// Lowercase `snake_case` token used in JSON / MCP responses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RevisionClass::InitialAssertion => "initial_assertion",
            RevisionClass::Correction => "correction",
            RevisionClass::WorldChange => "world_change",
            RevisionClass::Retraction => "retraction",
            RevisionClass::Reaffirmation => "reaffirmation",
        }
    }
}

/// One audited property change within a revision: the prior and new value of a
/// key. `prior == None` means the key was added (or is being asserted for the
/// first time); `new == None` means the key was removed / retracted.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyChange {
    /// The property key.
    pub key: String,
    /// The value before this revision (`None` = not previously present).
    pub prior: Option<PropertyValue>,
    /// The value after this revision (`None` = removed / retracted).
    pub new: Option<PropertyValue>,
}

/// A single revision entry in the belief-revision audit (Issue #3362, AC1).
#[derive(Debug, Clone, PartialEq)]
pub struct Revision {
    /// Sequential version number (1-based) of the version that introduced this
    /// revision.
    pub version_number: u64,
    /// Internal version id.
    pub version_id: VersionId,
    /// Transaction time at which this revision was recorded (commit time).
    pub transaction_time: Timestamp,
    /// Start of the valid-time interval asserted by this version.
    pub valid_from: Timestamp,
    /// End of the valid-time interval (`None` = open-ended / still valid).
    pub valid_to: Option<Timestamp>,
    /// Deterministic classification of the transition.
    pub class: RevisionClass,
    /// The prior/new values changed by this revision, sorted by key.
    pub changes: Vec<PropertyChange>,
    /// Provenance of the superseding write, if recorded (`source`, `confidence`,
    /// `note` — the AC's `reason` — and `principal`).
    pub provenance: Option<Provenance>,
    /// Convenience mirror of `provenance.confidence()` for the confidence
    /// trajectory (AC3): explicit `None` (JSON `null`) when the write carried no
    /// confidence.
    pub confidence: Option<f64>,
}

/// Options controlling a belief-revision audit.
#[derive(Debug, Clone, Default)]
pub struct RevisionOptions {
    /// Optionally scope the audit to a single property key. When set, only
    /// revisions that touched this key are emitted, and their `changes` are
    /// filtered to it. An unknown key (never present in any version) is an
    /// `INVALID_ARGUMENT` error.
    pub property_key: Option<String>,
    /// Optionally time-travel the audit itself: revisions recorded **after**
    /// this transaction time are excluded (AC5).
    pub as_of_transaction_time: Option<Timestamp>,
    /// Maximum number of revisions to return. `None` uses
    /// [`DEFAULT_REVISION_LIMIT`]; `Some(0)` is rejected; values above
    /// [`MAX_REVISION_LIMIT`] are clamped.
    pub limit: Option<usize>,
}

impl RevisionOptions {
    /// Empty options (identical to [`Default::default`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Scope the audit to a single property key.
    #[must_use]
    pub fn with_property_key(mut self, key: impl Into<String>) -> Self {
        self.property_key = Some(key.into());
        self
    }

    /// Time-travel the audit to a transaction-time coordinate (AC5).
    #[must_use]
    pub fn with_as_of_transaction_time(mut self, ts: Timestamp) -> Self {
        self.as_of_transaction_time = Some(ts);
        self
    }

    /// Bound the number of revisions returned.
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }
}

/// The result of a belief-revision audit: an ordered revision sequence plus the
/// confidence trajectory and completeness signaling.
#[derive(Debug, Clone, PartialEq)]
pub struct BeliefRevisionLog {
    /// The audited entity.
    pub entity: EntityId,
    /// The property key the audit was scoped to, if any.
    pub property_key: Option<String>,
    /// The transaction-time coordinate the audit was scoped to, if any (AC5).
    pub as_of_transaction_time: Option<Timestamp>,
    /// Ordered revisions (oldest first), bounded by the effective `limit`.
    pub revisions: Vec<Revision>,
    /// `true` when more revisions exist than were returned (AC7).
    pub has_more: bool,
}

impl BeliefRevisionLog {
    /// The ordered revisions (oldest first).
    #[must_use]
    pub fn revisions(&self) -> &[Revision] {
        &self.revisions
    }

    /// The confidence trajectory: one entry per returned revision, in order,
    /// `None` (JSON `null`) where the write carried no confidence (AC3).
    #[must_use]
    pub fn confidence_trajectory(&self) -> Vec<Option<f64>> {
        self.revisions.iter().map(|r| r.confidence).collect()
    }
}

/// Read-only engine that classifies an entity's belief revisions.
///
/// Mirrors the [`TemporalDiff`](super::temporal_diff::TemporalDiff) "analysis
/// over history" shape: borrow the database, read, classify, return. No writes.
pub struct BeliefRevisions<'a> {
    db: &'a AletheiaDB,
}

impl<'a> BeliefRevisions<'a> {
    /// Create a new belief-revision engine over `db`.
    #[must_use]
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Audit the belief revisions of `entity` under `options`.
    ///
    /// # Errors
    ///
    /// - `NOT_FOUND` (`StorageError::NodeNotFound`/`EdgeNotFound`) for an unknown
    ///   entity (AC6).
    /// - `INVALID_ARGUMENT` (`QueryError::InvalidParameter`) for a `limit` of 0
    ///   or a `property_key` the entity never had (AC6).
    pub fn audit(&self, entity: EntityId, options: &RevisionOptions) -> Result<BeliefRevisionLog> {
        // Validate the limit up front (INVALID_ARGUMENT on 0).
        let effective_limit = resolve_limit(options.limit)?;

        // Fetch the full history (NOT_FOUND for an unknown entity).
        let history = self.fetch_history(entity)?;

        // Property-scope validation: the key must exist somewhere in history.
        if let Some(key) = options.property_key.as_deref() {
            if !history_contains_key(&history, key) {
                return Err(invalid_argument(
                    "property_key",
                    format!("entity {entity} never had property '{key}'"),
                ));
            }
        }

        let log = build_log(entity, &history, options, effective_limit);
        Ok(log)
    }

    fn fetch_history(&self, entity: EntityId) -> Result<EntityHistory> {
        match entity {
            EntityId::Node(id) => self.db.get_node_history(id),
            EntityId::Edge(id) => self.db.get_edge_history(id),
        }
    }
}

/// Resolve the effective revision limit, rejecting an explicit `0`.
fn resolve_limit(limit: Option<usize>) -> Result<usize> {
    match limit {
        None => Ok(DEFAULT_REVISION_LIMIT),
        Some(0) => Err(invalid_argument("limit", "limit must be greater than 0")),
        Some(n) => Ok(n.min(MAX_REVISION_LIMIT)),
    }
}

/// Build an `INVALID_ARGUMENT`-mapped error (`QueryError::InvalidParameter`).
fn invalid_argument(parameter: &str, reason: impl Into<String>) -> Error {
    Error::Query(QueryError::InvalidParameter {
        parameter: parameter.to_string(),
        reason: reason.into(),
    })
}

/// Whether `key` appears in the properties of any version in `history`.
fn history_contains_key(history: &EntityHistory, key: &str) -> bool {
    history
        .versions
        .iter()
        .any(|v| v.properties.get(key).is_some())
}

/// Core classifier: build the (bounded) revision log from an entity's history.
///
/// Pure over `(history, options)` — no I/O, no map-iteration-order leakage — so
/// the same inputs always yield byte-identical output (AC4).
fn build_log(
    entity: EntityId,
    history: &EntityHistory,
    options: &RevisionOptions,
    effective_limit: usize,
) -> BeliefRevisionLog {
    // Filter to versions visible at the as-of transaction-time coordinate (AC5).
    let visible: Vec<&VersionInfo> = history
        .versions
        .iter()
        .filter(|v| match options.as_of_transaction_time {
            Some(as_of) => v.temporal.transaction_time().start() <= as_of,
            None => true,
        })
        .collect();

    let mut revisions: Vec<Revision> = Vec::new();
    let mut max_prior_valid_from: Option<Timestamp> = None;

    for (i, v) in visible.iter().enumerate() {
        let predecessor = if i == 0 { None } else { Some(visible[i - 1]) };
        let class = classify(i, v, predecessor, max_prior_valid_from);
        let changes = build_changes(v, predecessor, class);

        // Update the running max valid_from AFTER classifying this version.
        let vf = v.temporal.valid_time().start();
        max_prior_valid_from = Some(match max_prior_valid_from {
            Some(m) if m >= vf => m,
            _ => vf,
        });

        // Property scope: only emit revisions that touched the scoped key.
        if let Some(key) = options.property_key.as_deref() {
            let filtered: Vec<PropertyChange> =
                changes.into_iter().filter(|c| c.key == key).collect();
            if filtered.is_empty() {
                continue;
            }
            revisions.push(make_revision(v, class, filtered));
        } else {
            revisions.push(make_revision(v, class, changes));
        }
    }

    let has_more = revisions.len() > effective_limit;
    if has_more {
        revisions.truncate(effective_limit);
    }

    BeliefRevisionLog {
        entity,
        property_key: options.property_key.clone(),
        as_of_transaction_time: options.as_of_transaction_time,
        revisions,
        has_more,
    }
}

/// Classify a single version. Pure function of the version, its predecessor, and
/// the max `valid_from` recorded before it — unit-testable in isolation.
fn classify(
    index: usize,
    version: &VersionInfo,
    predecessor: Option<&VersionInfo>,
    max_prior_valid_from: Option<Timestamp>,
) -> RevisionClass {
    // 1. First visible version.
    if index == 0 || predecessor.is_none() {
        return RevisionClass::InitialAssertion;
    }
    // 2. Closed valid interval => retraction / deletion (dominates value change).
    if version.temporal.valid_time().is_closed() {
        return RevisionClass::Retraction;
    }
    let pred = predecessor.expect("predecessor present for index > 0");
    let diff = VersionDiff::compute(
        &pred.properties,
        &version.properties,
        pred.version_id,
        version.version_id,
    );
    // 3. No value change => reaffirmation.
    if !diff.has_changes() {
        return RevisionClass::Reaffirmation;
    }
    // 4/5. valid_from advanced beyond all prior => world_change, else correction.
    let vf = version.temporal.valid_time().start();
    match max_prior_valid_from {
        Some(max) if vf > max => RevisionClass::WorldChange,
        _ => RevisionClass::Correction,
    }
}

/// Compute the sorted list of property changes for a revision given its class.
fn build_changes(
    version: &VersionInfo,
    predecessor: Option<&VersionInfo>,
    class: RevisionClass,
) -> Vec<PropertyChange> {
    let mut changes: Vec<PropertyChange> = match class {
        RevisionClass::InitialAssertion => version
            .properties
            .iter()
            .map(|(k, val)| PropertyChange {
                key: k.to_string(),
                prior: None,
                new: Some(val.clone()),
            })
            .collect(),
        RevisionClass::Retraction => match predecessor {
            Some(pred) => pred
                .properties
                .iter()
                .map(|(k, val)| PropertyChange {
                    key: k.to_string(),
                    prior: Some(val.clone()),
                    new: None,
                })
                .collect(),
            None => Vec::new(),
        },
        RevisionClass::Reaffirmation => Vec::new(),
        RevisionClass::Correction | RevisionClass::WorldChange => match predecessor {
            Some(pred) => diff_changes(pred, version),
            None => Vec::new(),
        },
    };
    changes.sort_by(|a, b| a.key.cmp(&b.key));
    changes
}

/// Turn a `VersionDiff` between `pred` and `version` into `PropertyChange`s.
fn diff_changes(pred: &VersionInfo, version: &VersionInfo) -> Vec<PropertyChange> {
    let diff = VersionDiff::compute(
        &pred.properties,
        &version.properties,
        pred.version_id,
        version.version_id,
    );
    let mut out = Vec::new();
    for (k, val) in diff.added.iter() {
        out.push(PropertyChange {
            key: k.to_string(),
            prior: None,
            new: Some(val.clone()),
        });
    }
    for (k, val) in diff.removed.iter() {
        out.push(PropertyChange {
            key: k.to_string(),
            prior: Some(val.clone()),
            new: None,
        });
    }
    for (k, old, new) in &diff.modified {
        out.push(PropertyChange {
            key: k.to_string(),
            prior: Some(old.clone()),
            new: Some(new.clone()),
        });
    }
    out
}

/// Assemble a `Revision` from a version, its class, and its (already filtered
/// and sorted) changes.
fn make_revision(
    version: &VersionInfo,
    class: RevisionClass,
    changes: Vec<PropertyChange>,
) -> Revision {
    let valid = version.temporal.valid_time();
    let valid_to = if valid.is_current() {
        None
    } else {
        Some(valid.end())
    };
    let confidence = version.provenance.as_ref().and_then(Provenance::confidence);
    Revision {
        version_number: version.version_number,
        version_id: version.version_id,
        transaction_time: version.temporal.transaction_time().start(),
        valid_from: valid.start(),
        valid_to,
        class,
        changes,
        provenance: version.provenance.clone(),
        confidence,
    }
}

impl AletheiaDB {
    /// Audit an entity's belief revisions — *when and why the database changed
    /// its mind* about a node or edge (Issue #3362).
    ///
    /// Convenience wrapper around [`BeliefRevisions::audit`]. See the module
    /// docs for the classification contract.
    ///
    /// # Errors
    ///
    /// - `NOT_FOUND` for an unknown entity.
    /// - `INVALID_ARGUMENT` for a `limit` of 0 or an unknown `property_key`.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn belief_revisions(
        &self,
        entity: EntityId,
        options: &RevisionOptions,
    ) -> Result<BeliefRevisionLog> {
        BeliefRevisions::new(self).audit(entity, options)
    }
}
