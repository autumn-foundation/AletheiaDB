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
//! | 3 | [`RevisionClass::Reaffirmation`] | no value change vs the predecessor **and** this version's provenance `source` is present and **differs** from the predecessor's — AC2's "the same value re-asserted *by a different source*". If either side's source is absent/unknown, this is **not** a reaffirmation; it falls through to the valid-interval geometry (rules 4/5). |
//! | 4 | [`RevisionClass::WorldChange`] | value changed (or a same-source no-op) and `valid_from` advanced beyond every prior `valid_from` — the fact itself changed. |
//! | 5 | [`RevisionClass::Correction`] | value changed (or a same-source no-op) and `valid_from` did **not** advance — a later transaction-time rewrite **or backfill** of a same-or-earlier valid period. |
//!
//! Precedence is strict: a closed valid interval is always a retraction; the
//! reaffirmation check (rule 3) runs **before** the `world_change`/`correction`
//! split, so a same-value re-assertion by a different source is a
//! `reaffirmation` even when its `valid_from` advanced (value-unchanged wins
//! over validity-advanced). The `world_change`/`correction` split uses a strict
//! `>` on the running-max `valid_from` so equal `valid_from` is a correction.
//!
//! **Backfill is a `correction` (documented AC2 interpretation).** A backdated
//! write asserting a *new, previously-unrecorded* earlier valid interval is
//! classified `correction` via the `valid_from <= running-max` proxy — there is
//! no interval-overlap check. So `correction` here means "a same-or-earlier
//! valid interval recorded by a later transaction — i.e. rewriting **or
//! backfilling** our record of a past validity period", a deliberate reading of
//! AC2's "rewrites an already-recorded valid interval" wording (AC2 fixes
//! exactly five categories, so no sixth `backfill` class is introduced).
//!
//! **Retraction rendering.** A #3230 valid-time retraction and a hard delete
//! both render their `changes` as the predecessor's properties "removed"
//! (`(key, Some(old), None)`). For a valid-time retraction this means "no
//! longer asserted after `valid_to`", **not** that the stored values were
//! erased — the history remains readable `AS OF` a time before `valid_to`.
//!
//! v1 notes: a property-scoped audit reports each surviving revision with the
//! **entity-level** classification (not a per-property re-classification), and
//! `as_of_transaction_time` filtering relies on versions being appended in
//! transaction-time-monotonic order.
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
    /// The same value was re-asserted (no value delta vs the predecessor) **by a
    /// different, known source** (AC2). A same-source or unknown-source no-op is
    /// classified by valid-interval geometry instead, never as a reaffirmation.
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
        if let Some(key) = options.property_key.as_deref()
            && !history_contains_key(&history, key)
        {
            return Err(invalid_argument(
                "property_key",
                format!("entity {entity} never had property '{key}'"),
            ));
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
        let class = classify(v, predecessor, max_prior_valid_from);
        let changes = build_changes(v, predecessor, class);

        // Update the running max valid_from AFTER classifying this version.
        let vf = v.temporal.valid_time().start();
        max_prior_valid_from = Some(match max_prior_valid_from {
            Some(m) if m >= vf => m,
            _ => vf,
        });

        // Property scope: only emit revisions that touched the scoped key.
        // Note: a reaffirmation has no value delta, so its `changes` are empty
        // and it is skipped under a property scope — a scoped audit reports the
        // *value* trajectory of the key, not every re-assertion of it (v1).
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

/// Classify every version in `history` (oldest-first) into its
/// [`RevisionClass`], returning one class per version in input order.
///
/// Shared with contradiction genealogy (Issue #3352) so that a competing
/// claim's per-version `origin` is computed by the *same* classifier the
/// belief-revision audit uses (AC3) — the two analyses never disagree about
/// whether a given version was a correction, world-change, retraction, etc.
/// Pure over `history` (no I/O, no map-iteration-order leakage).
pub(crate) fn classify_history(history: &EntityHistory) -> Vec<RevisionClass> {
    let mut out = Vec::with_capacity(history.versions.len());
    let mut max_prior_valid_from: Option<Timestamp> = None;
    for (i, v) in history.versions.iter().enumerate() {
        let predecessor = if i == 0 {
            None
        } else {
            Some(&history.versions[i - 1])
        };
        out.push(classify(v, predecessor, max_prior_valid_from));
        let vf = v.temporal.valid_time().start();
        max_prior_valid_from = Some(match max_prior_valid_from {
            Some(m) if m >= vf => m,
            _ => vf,
        });
    }
    out
}

/// Classify a single version. Pure function of the version, its predecessor, and
/// the max `valid_from` recorded before it — unit-testable in isolation.
///
/// This deliberately recomputes the property [`VersionDiff`] rather than
/// threading one in, so the classifier stays a self-contained pure predicate
/// (the value the design leans on for the table-driven fixture test). The diff
/// is cheap relative to the single `historical.read()` the audit already paid
/// for, and the audit is a cold read, not a hot path.
///
// `pub(crate)` (not module-private) so it is the single source of truth for the
// terminating-event oracle: used by half_life (#3377) to decide which
// transitions (WorldChange / Retraction) end a fact's valid-time lifespan.
pub(crate) fn classify(
    version: &VersionInfo,
    predecessor: Option<&VersionInfo>,
    max_prior_valid_from: Option<Timestamp>,
) -> RevisionClass {
    // 1. First visible version (no predecessor). `predecessor.is_none()` holds
    //    exactly when the version is index 0 of the visible prefix, so this
    //    single guard subsumes the old `index == 0 || predecessor.is_none()`.
    let Some(pred) = predecessor else {
        return RevisionClass::InitialAssertion;
    };
    // 2. Closed valid interval => retraction / deletion (dominates value change).
    //    This rule holds only because no live write path today asserts a bounded
    //    valid interval on a normal write — a closed interval means a #3230
    //    valid-time retraction or a delete tombstone (the #3504 append-only
    //    valid-time invariant). A future bounded-validity assertion API would
    //    make "closed interval" ambiguous and this check would need revisiting.
    if version.temporal.valid_time().is_closed() {
        return RevisionClass::Retraction;
    }
    let diff = VersionDiff::compute(
        &pred.properties,
        &version.properties,
        pred.version_id,
        version.version_id,
    );
    // 3. No value change AND re-asserted by a *different, known* source =>
    //    reaffirmation (AC2). None-handling is explicit: if either side's source
    //    is absent/unknown we do NOT classify as reaffirmation — we fall through
    //    to the valid-interval geometry (rules 4/5) below. Checked before the
    //    world_change/correction split, so value-unchanged wins over
    //    validity-advanced (LOW-3 documented precedence).
    if !diff.has_changes() && sources_differ(pred, version) {
        return RevisionClass::Reaffirmation;
    }
    // 4/5. valid_from advanced beyond all prior => world_change, else correction.
    let vf = version.temporal.valid_time().start();
    match max_prior_valid_from {
        Some(max) if vf > max => RevisionClass::WorldChange,
        _ => RevisionClass::Correction,
    }
}

/// Whether `version`'s provenance `source` is present **and** differs from
/// `pred`'s present source. If either side has no provenance or no `source`
/// (absent/unknown), returns `false` — a reaffirmation requires two *known*
/// sources that disagree (AC2's "different source").
fn sources_differ(pred: &VersionInfo, version: &VersionInfo) -> bool {
    match (
        pred.provenance.as_ref().and_then(Provenance::source),
        version.provenance.as_ref().and_then(Provenance::source),
    ) {
        (Some(a), Some(b)) => a != b,
        _ => false,
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
    pub fn belief_revisions(
        &self,
        entity: EntityId,
        options: &RevisionOptions,
    ) -> Result<BeliefRevisionLog> {
        BeliefRevisions::new(self).audit(entity, options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::PropertyMapBuilder;
    use crate::core::id::NodeId;
    use crate::core::property::PropertyMap;
    use crate::core::temporal::{BiTemporalInterval, TimeRange};

    // ---- fixture builders --------------------------------------------------

    fn ts(micros: i64) -> Timestamp {
        Timestamp::new(micros, 0).expect("valid timestamp")
    }

    fn vid(n: u64) -> VersionId {
        VersionId::new(n).expect("valid version id")
    }

    fn status_props(status: &str) -> PropertyMap {
        PropertyMapBuilder::new()
            .insert("status", status)
            .insert("n", "1")
            .build()
    }

    fn prov(source: &str, confidence: Option<f64>) -> Provenance {
        let mut b = Provenance::builder().source(source);
        if let Some(c) = confidence {
            b = b.confidence(c);
        }
        b.build().expect("valid provenance")
    }

    /// A version with an **open** valid interval `[valid_from, ∞)`.
    fn open_version(
        num: u64,
        valid_from: i64,
        tx: i64,
        props: PropertyMap,
        provenance: Option<Provenance>,
    ) -> VersionInfo {
        VersionInfo {
            version_number: num,
            version_id: vid(num),
            temporal: BiTemporalInterval::with_valid_time(ts(valid_from), ts(tx)),
            properties: props,
            label: "Person".to_string(),
            provenance,
        }
    }

    /// A version with a **closed** valid interval `[valid_from, valid_to)`
    /// (a #3230 retraction, or a delete tombstone when `valid_to == valid_from`).
    fn closed_version(
        num: u64,
        valid_from: i64,
        valid_to: i64,
        tx: i64,
        props: PropertyMap,
        provenance: Option<Provenance>,
    ) -> VersionInfo {
        let valid = TimeRange::between(ts(valid_from), ts(valid_to)).expect("valid range");
        VersionInfo {
            version_number: num,
            version_id: vid(num),
            temporal: BiTemporalInterval::new(valid, TimeRange::from(ts(tx))),
            properties: props,
            label: "Person".to_string(),
            provenance,
        }
    }

    fn history(versions: Vec<VersionInfo>) -> EntityHistory {
        EntityHistory { versions }
    }

    fn node_entity() -> EntityId {
        EntityId::Node(NodeId::new(1).unwrap())
    }

    fn audit_history(h: &EntityHistory, opts: &RevisionOptions) -> BeliefRevisionLog {
        let limit = resolve_limit(opts.limit).expect("limit ok");
        build_log(node_entity(), h, opts, limit)
    }

    // ---- pure classifier unit tests (AC2) ----------------------------------

    #[test]
    fn classify_first_version_is_initial_assertion() {
        let v = open_version(1, 1000, 100, status_props("a"), None);
        assert_eq!(classify(&v, None, None), RevisionClass::InitialAssertion);
    }

    #[test]
    fn classify_same_valid_from_is_correction() {
        let pred = open_version(1, 1000, 100, status_props("a"), None);
        let v = open_version(2, 1000, 200, status_props("b"), None);
        // max prior valid_from == 1000, new == 1000 (not >) => correction.
        assert_eq!(
            classify(&v, Some(&pred), Some(ts(1000))),
            RevisionClass::Correction
        );
    }

    #[test]
    fn classify_earlier_valid_from_is_correction() {
        let pred = open_version(1, 2000, 100, status_props("a"), None);
        let v = open_version(2, 1500, 200, status_props("b"), None);
        assert_eq!(
            classify(&v, Some(&pred), Some(ts(2000))),
            RevisionClass::Correction
        );
    }

    #[test]
    fn classify_advanced_valid_from_is_world_change() {
        let pred = open_version(1, 1000, 100, status_props("a"), None);
        let v = open_version(2, 2000, 200, status_props("b"), None);
        assert_eq!(
            classify(&v, Some(&pred), Some(ts(1000))),
            RevisionClass::WorldChange
        );
    }

    #[test]
    fn classify_no_value_change_is_reaffirmation() {
        let pred = open_version(1, 1000, 100, status_props("a"), Some(prov("s1", None)));
        let v = open_version(2, 1000, 200, status_props("a"), Some(prov("s2", None)));
        assert_eq!(
            classify(&v, Some(&pred), Some(ts(1000))),
            RevisionClass::Reaffirmation
        );
    }

    #[test]
    fn classify_closed_interval_is_retraction_even_with_value_change() {
        let pred = open_version(1, 1000, 100, status_props("a"), None);
        // Closed valid interval AND a value change: retraction wins (precedence).
        let v = closed_version(2, 1000, 1500, 200, status_props("b"), None);
        assert_eq!(
            classify(&v, Some(&pred), Some(ts(1000))),
            RevisionClass::Retraction
        );
    }

    #[test]
    fn classify_delete_tombstone_is_retraction() {
        let pred = open_version(1, 1000, 100, status_props("a"), None);
        // Empty valid interval [t, t) == delete tombstone.
        let v = closed_version(2, 2000, 2000, 200, status_props("a"), None);
        assert_eq!(
            classify(&v, Some(&pred), Some(ts(1000))),
            RevisionClass::Retraction
        );
    }

    // ---- MED-1: reaffirmation honors AC2's "different source" ----------------

    #[test]
    fn classify_same_source_no_delta_is_not_reaffirmation() {
        // Same value re-recorded by the SAME source at a later tx-time. AC2 says
        // reaffirmation is a re-assertion *by a different source*, so this is NOT
        // a reaffirmation — it classifies by geometry (equal valid_from => correction).
        let pred = open_version(1, 1000, 100, status_props("a"), Some(prov("src-a", None)));
        let v = open_version(2, 1000, 200, status_props("a"), Some(prov("src-a", None)));
        let class = classify(&v, Some(&pred), Some(ts(1000)));
        assert_ne!(class, RevisionClass::Reaffirmation);
        assert_eq!(class, RevisionClass::Correction);
    }

    #[test]
    fn classify_absent_source_no_delta_is_not_reaffirmation() {
        // No provenance on either side: source unknown => not a reaffirmation,
        // classify by geometry (equal valid_from => correction).
        let pred = open_version(1, 1000, 100, status_props("a"), None);
        let v = open_version(2, 1000, 200, status_props("a"), None);
        assert_eq!(
            classify(&v, Some(&pred), Some(ts(1000))),
            RevisionClass::Correction
        );
        // One side present, the other absent: still not a reaffirmation.
        let v_one_side = open_version(2, 1000, 200, status_props("a"), Some(prov("src-a", None)));
        assert_eq!(
            classify(&v_one_side, Some(&pred), Some(ts(1000))),
            RevisionClass::Correction
        );
    }

    // ---- LOW-3: reaffirmation wins over world_change (same value, advanced vf) -

    #[test]
    fn classify_reaffirmation_precedes_world_change() {
        // Same value, DIFFERENT source, but valid_from advanced beyond the max:
        // value-unchanged wins => reaffirmation, not world_change (documented
        // precedence — the reaffirmation check runs before the corr/world split).
        let pred = open_version(1, 1000, 100, status_props("a"), Some(prov("src-a", None)));
        let v = open_version(2, 5000, 200, status_props("a"), Some(prov("src-b", None)));
        assert_eq!(
            classify(&v, Some(&pred), Some(ts(1000))),
            RevisionClass::Reaffirmation
        );
    }

    // ---- MED-3: running-max valid_from (not predecessor-only) ----------------

    #[test]
    fn running_max_valid_from_governs_correction() {
        use RevisionClass::*;
        // [vf1000 init] -> [vf5000 world_change] -> [vf2000 correction]
        //   -> [vf3000 correction]. The last version's valid_from (3000) is ABOVE
        // its immediate predecessor (2000) but AT-OR-BELOW the running max (5000),
        // so it MUST be a correction. A broken "compare to predecessor only" rule
        // would mislabel it world_change — this kills that mutant.
        let h = history(vec![
            open_version(1, 1000, 100, status_props("a1"), None),
            open_version(2, 5000, 200, status_props("a2"), None),
            open_version(3, 2000, 300, status_props("a3"), None),
            open_version(4, 3000, 400, status_props("a4"), None),
        ]);
        let log = audit_history(&h, &RevisionOptions::default());
        let classes: Vec<RevisionClass> = log.revisions.iter().map(|r| r.class).collect();
        assert_eq!(
            classes,
            vec![InitialAssertion, WorldChange, Correction, Correction],
            "vf above predecessor but <= running max must be correction"
        );
    }

    // ---- MED-4: backfill of a new, earlier valid interval => correction ------

    #[test]
    fn backfill_earlier_interval_is_correction() {
        // v1 records a fact valid from 5000; v2 backfills an EARLIER,
        // previously-unrecorded valid interval (from 1000). Documented AC2
        // interpretation: a same-or-earlier valid interval recorded by a later
        // transaction is a `correction` (no overlap check, no 6th category).
        let h = history(vec![
            open_version(1, 5000, 100, status_props("a1"), None),
            open_version(2, 1000, 200, status_props("a2"), None),
        ]);
        let log = audit_history(&h, &RevisionOptions::default());
        assert_eq!(log.revisions[1].class, RevisionClass::Correction);
    }

    // ---- 20-write scripted fixture: 100% classification accuracy (AC2) -----

    /// Build the canonical 20-version fixture and its expected classes.
    fn fixture_20() -> (EntityHistory, Vec<RevisionClass>) {
        use RevisionClass::*;
        // (num, valid_from, valid_to(None=open), tx, status, source, expected)
        let versions = vec![
            open_version(
                1,
                1000,
                100,
                status_props("a1"),
                Some(prov("src-a", Some(0.5))),
            ),
            open_version(
                2,
                1000,
                200,
                status_props("a2"),
                Some(prov("src-a", Some(0.6))),
            ),
            open_version(
                3,
                2000,
                300,
                status_props("a3"),
                Some(prov("src-b", Some(0.7))),
            ),
            open_version(
                4,
                2000,
                400,
                status_props("a3"),
                Some(prov("src-c", Some(0.8))),
            ),
            open_version(5, 1500, 500, status_props("a4"), Some(prov("src-a", None))),
            open_version(
                6,
                3000,
                600,
                status_props("a5"),
                Some(prov("src-b", Some(0.9))),
            ),
            closed_version(
                7,
                3000,
                3500,
                700,
                status_props("a5"),
                Some(prov("src-b", None)),
            ),
            open_version(
                8,
                4000,
                800,
                status_props("a6"),
                Some(prov("src-a", Some(0.4))),
            ),
            open_version(
                9,
                4000,
                900,
                status_props("a6"),
                Some(prov("src-d", Some(0.4))),
            ),
            open_version(10, 2500, 1000, status_props("a7"), None),
            open_version(
                11,
                5000,
                1100,
                status_props("a8"),
                Some(prov("src-a", Some(0.55))),
            ),
            open_version(
                12,
                5000,
                1200,
                status_props("a9"),
                Some(prov("src-a", Some(0.55))),
            ),
            open_version(
                13,
                5000,
                1300,
                status_props("a9"),
                Some(prov("src-e", Some(0.55))),
            ),
            open_version(
                14,
                6000,
                1400,
                status_props("a10"),
                Some(prov("src-b", Some(0.6))),
            ),
            open_version(
                15,
                100,
                1500,
                status_props("a11"),
                Some(prov("src-a", None)),
            ),
            closed_version(16, 6000, 6100, 1600, status_props("a11"), None),
            open_version(
                17,
                7000,
                1700,
                status_props("a12"),
                Some(prov("src-b", Some(0.95))),
            ),
            open_version(
                18,
                7000,
                1800,
                status_props("a12"),
                Some(prov("src-f", Some(0.95))),
            ),
            open_version(
                19,
                7000,
                1900,
                status_props("a13"),
                Some(prov("src-a", Some(0.3))),
            ),
            closed_version(20, 8000, 8000, 2000, status_props("a13"), None),
        ];
        let expected = vec![
            InitialAssertion, // 1
            Correction,       // 2
            WorldChange,      // 3
            Reaffirmation,    // 4
            Correction,       // 5
            WorldChange,      // 6
            Retraction,       // 7
            WorldChange,      // 8
            Reaffirmation,    // 9
            Correction,       // 10
            WorldChange,      // 11
            Correction,       // 12
            Reaffirmation,    // 13
            WorldChange,      // 14
            Correction,       // 15
            Retraction,       // 16
            WorldChange,      // 17
            Reaffirmation,    // 18
            Correction,       // 19
            Retraction,       // 20
        ];
        (history(versions), expected)
    }

    #[test]
    fn scripted_20_write_fixture_100pct() {
        let (h, expected) = fixture_20();
        let log = audit_history(&h, &RevisionOptions::default());
        assert_eq!(log.revisions.len(), 20, "one revision per version");
        for (i, (rev, want)) in log.revisions.iter().zip(expected.iter()).enumerate() {
            assert_eq!(
                rev.class,
                *want,
                "version {} (#{}) classified {:?}, expected {:?}",
                i + 1,
                rev.version_number,
                rev.class,
                want
            );
        }
        // All five categories are exercised.
        use std::collections::HashSet;
        let seen: HashSet<_> = log.revisions.iter().map(|r| r.class).collect();
        assert_eq!(seen.len(), 5, "all five classification categories present");
    }

    // ---- AC1: revision sequence shape --------------------------------------

    #[test]
    fn revision_sequence_carries_expected_fields() {
        let h = history(vec![
            open_version(
                1,
                1000,
                100,
                status_props("a1"),
                Some(prov("src", Some(0.5))),
            ),
            open_version(
                2,
                1000,
                200,
                status_props("a2"),
                Some(prov("src", Some(0.9))),
            ),
        ]);
        let log = audit_history(&h, &RevisionOptions::default());
        assert_eq!(log.revisions.len(), 2);
        let r0 = &log.revisions[0];
        assert_eq!(r0.class, RevisionClass::InitialAssertion);
        assert_eq!(r0.version_number, 1);
        assert_eq!(r0.transaction_time, ts(100));
        assert_eq!(r0.valid_from, ts(1000));
        assert_eq!(r0.valid_to, None);
        // Initial assertion records all keys as additions.
        assert!(
            r0.changes
                .iter()
                .any(|c| c.key == "status" && c.prior.is_none())
        );
        let r1 = &log.revisions[1];
        assert_eq!(r1.class, RevisionClass::Correction);
        // Modified key carries prior + new.
        let status = r1.changes.iter().find(|c| c.key == "status").unwrap();
        assert!(status.prior.is_some() && status.new.is_some());
        assert_eq!(r1.confidence, Some(0.9));
    }

    // ---- AC3: confidence trajectory with explicit nulls --------------------

    #[test]
    fn confidence_trajectory_surfaces_nulls() {
        let h = history(vec![
            // provenance present, confidence present
            open_version(1, 1000, 100, status_props("a"), Some(prov("s", Some(0.5)))),
            // provenance present, confidence ABSENT -> null
            open_version(2, 1000, 200, status_props("b"), Some(prov("s", None))),
            // provenance ABSENT entirely -> null
            open_version(3, 1000, 300, status_props("c"), None),
        ]);
        let log = audit_history(&h, &RevisionOptions::default());
        assert_eq!(
            log.confidence_trajectory(),
            vec![Some(0.5), None, None],
            "both 'no provenance' and 'provenance without confidence' surface as null"
        );
    }

    // ---- AC4: pure read, byte-identical on repeat --------------------------

    #[test]
    fn audit_is_byte_identical() {
        let (h, _) = fixture_20();
        let a = audit_history(&h, &RevisionOptions::default());
        let b = audit_history(&h, &RevisionOptions::default());
        assert_eq!(a, b, "structural equality");
        assert_eq!(
            format!("{a:?}"),
            format!("{b:?}"),
            "byte-identical debug rendering"
        );
        #[cfg(feature = "serde")]
        {
            // Where serde is enabled, the class tokens serialize deterministically too.
            let ja: Vec<_> = a.revisions.iter().map(|r| r.class).collect();
            let jb: Vec<_> = b.revisions.iter().map(|r| r.class).collect();
            assert_eq!(
                serde_json::to_string(&ja).unwrap(),
                serde_json::to_string(&jb).unwrap()
            );
        }
    }

    #[test]
    fn audit_is_key_order_independent() {
        // Two histories with byte-equal CONTENT but different property-map key
        // insertion orders. Because `changes` are sorted by key and no HashMap
        // iteration reaches the output, the serialized audits must be identical.
        // (The old `audit_is_byte_identical` ran twice on the SAME instance, so a
        // missing sort could hide behind stable-within-run HashMap order.)
        let props_a = |a: i64, z: i64, m: i64| {
            PropertyMapBuilder::new()
                .insert("alpha", a)
                .insert("zeta", z)
                .insert("mid", m)
                .build()
        };
        let props_shuffled = |a: i64, z: i64, m: i64| {
            PropertyMapBuilder::new()
                .insert("zeta", z)
                .insert("mid", m)
                .insert("alpha", a)
                .build()
        };
        let h1 = history(vec![
            open_version(1, 1000, 100, props_a(1, 2, 3), Some(prov("s", Some(0.5)))),
            open_version(2, 1000, 200, props_a(9, 8, 7), Some(prov("s2", Some(0.6)))),
        ]);
        let h2 = history(vec![
            open_version(
                1,
                1000,
                100,
                props_shuffled(1, 2, 3),
                Some(prov("s", Some(0.5))),
            ),
            open_version(
                2,
                1000,
                200,
                props_shuffled(9, 8, 7),
                Some(prov("s2", Some(0.6))),
            ),
        ]);
        let a = audit_history(&h1, &RevisionOptions::default());
        let b = audit_history(&h2, &RevisionOptions::default());
        assert_eq!(
            format!("{a:?}"),
            format!("{b:?}"),
            "audit output must not depend on property-map key insertion order"
        );
        // (b) The v2 revision is a correction touching >= 2 keys, all sorted.
        let r1 = &a.revisions[1];
        assert_eq!(r1.class, RevisionClass::Correction);
        let keys: Vec<&str> = r1.changes.iter().map(|c| c.key.as_str()).collect();
        assert!(
            keys.len() >= 2,
            "expected a multi-key correction, got {keys:?}"
        );
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(
            keys, sorted,
            "changes must be sorted by key on a multi-key correction"
        );
    }

    // ---- AC5: as_of_transaction_time time-travel ---------------------------

    #[test]
    fn as_of_scopes_history() {
        let h = history(vec![
            open_version(
                1,
                1000,
                100,
                status_props("married"),
                Some(prov("s", Some(0.9))),
            ),
            // A correction recorded at tx=500.
            open_version(
                2,
                1000,
                500,
                status_props("single"),
                Some(prov("s2", Some(0.8))),
            ),
        ]);
        // Before the correction (as_of=200): only the initial assertion is visible.
        let before = audit_history(
            &h,
            &RevisionOptions::new().with_as_of_transaction_time(ts(200)),
        );
        assert_eq!(before.revisions.len(), 1);
        assert_eq!(before.revisions[0].class, RevisionClass::InitialAssertion);
        // After (as_of=600): both visible, the second is a correction.
        let after = audit_history(
            &h,
            &RevisionOptions::new().with_as_of_transaction_time(ts(600)),
        );
        assert_eq!(after.revisions.len(), 2);
        assert_eq!(after.revisions[1].class, RevisionClass::Correction);
        // Byte-identical on repeat at the same coordinate (AC4 + AC5).
        let after2 = audit_history(
            &h,
            &RevisionOptions::new().with_as_of_transaction_time(ts(600)),
        );
        assert_eq!(format!("{after:?}"), format!("{after2:?}"));
    }

    // ---- AC7: bounded limit + completeness signaling -----------------------

    #[test]
    fn limit_bounds_and_has_more() {
        let (h, _) = fixture_20();
        let log = audit_history(&h, &RevisionOptions::new().with_limit(5));
        assert_eq!(log.revisions.len(), 5);
        assert!(log.has_more, "20 revisions truncated to 5 => has_more");

        let full = audit_history(&h, &RevisionOptions::new().with_limit(20));
        assert_eq!(full.revisions.len(), 20);
        assert!(!full.has_more, "limit == count => no has_more");
    }

    #[test]
    fn limit_zero_rejected() {
        let err = resolve_limit(Some(0)).unwrap_err();
        assert!(
            matches!(err, Error::Query(QueryError::InvalidParameter { .. })),
            "limit 0 must be INVALID_ARGUMENT, got {err:?}"
        );
    }

    #[test]
    fn limit_above_max_is_clamped() {
        assert_eq!(
            resolve_limit(Some(MAX_REVISION_LIMIT + 100)).unwrap(),
            MAX_REVISION_LIMIT
        );
        assert_eq!(resolve_limit(None).unwrap(), DEFAULT_REVISION_LIMIT);
    }

    // ---- AC6: property scope validation ------------------------------------

    #[test]
    fn property_scope_unknown_key_rejected() {
        let h = history(vec![open_version(1, 1000, 100, status_props("a"), None)]);
        assert!(!history_contains_key(&h, "never_here"));
        assert!(history_contains_key(&h, "status"));
    }

    #[test]
    fn property_scope_filters_to_key() {
        let h = history(vec![
            open_version(
                1,
                1000,
                100,
                PropertyMapBuilder::new()
                    .insert("status", "a")
                    .insert("age", 30)
                    .build(),
                None,
            ),
            // change only `age`
            open_version(
                2,
                1000,
                200,
                PropertyMapBuilder::new()
                    .insert("status", "a")
                    .insert("age", 31)
                    .build(),
                None,
            ),
            // change only `status`
            open_version(
                3,
                1000,
                300,
                PropertyMapBuilder::new()
                    .insert("status", "b")
                    .insert("age", 31)
                    .build(),
                None,
            ),
        ]);
        let log = audit_history(&h, &RevisionOptions::new().with_property_key("age"));
        // v1 (initial has age), v2 (age changed) emit; v3 (only status changed) is skipped.
        assert_eq!(log.revisions.len(), 2);
        assert_eq!(log.revisions[0].version_number, 1);
        assert_eq!(log.revisions[1].version_number, 2);
        for rev in &log.revisions {
            assert!(rev.changes.iter().all(|c| c.key == "age"));
        }
    }

    // ---- determinism of changes ordering -----------------------------------

    #[test]
    fn changes_are_sorted_by_key() {
        let h = history(vec![open_version(
            1,
            1000,
            100,
            PropertyMapBuilder::new()
                .insert("zeta", 1)
                .insert("alpha", 2)
                .insert("mid", 3)
                .build(),
            None,
        )]);
        let log = audit_history(&h, &RevisionOptions::default());
        let keys: Vec<&str> = log.revisions[0]
            .changes
            .iter()
            .map(|c| c.key.as_str())
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(
            keys, sorted,
            "changes must be sorted by key for determinism"
        );
    }
}

#[cfg(test)]
mod db_tests {
    //! End-to-end tests exercising the real write path + `db.belief_revisions`.
    use super::*;
    use crate::core::PropertyMapBuilder;
    use crate::core::id::NodeId;

    #[test]
    fn single_version_is_initial() {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        let log = db
            .belief_revisions(EntityId::Node(id), &RevisionOptions::default())
            .unwrap();
        assert_eq!(log.revisions.len(), 1);
        assert_eq!(log.revisions[0].class, RevisionClass::InitialAssertion);
        assert!(!log.has_more);
    }

    #[test]
    fn unknown_entity_not_found() {
        let db = AletheiaDB::new().unwrap();
        let missing = EntityId::Node(NodeId::new(999_999).unwrap());
        let err = db
            .belief_revisions(missing, &RevisionOptions::default())
            .unwrap_err();
        assert!(
            matches!(err, Error::Storage(_)),
            "unknown entity must be a storage NOT_FOUND, got {err:?}"
        );
    }

    #[test]
    fn unknown_property_key_rejected() {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        let err = db
            .belief_revisions(
                EntityId::Node(id),
                &RevisionOptions::new().with_property_key("nonexistent"),
            )
            .unwrap_err();
        assert!(
            matches!(err, Error::Query(QueryError::InvalidParameter { .. })),
            "unknown property key must be INVALID_ARGUMENT, got {err:?}"
        );
    }

    #[test]
    fn end_to_end_correction_and_world_change() {
        use crate::api::transaction::WriteRequestOptions;
        let db = AletheiaDB::new().unwrap();
        // Capture a single base so the "correction" write reuses the *exact*
        // same valid_from as the create (equal valid_from => correction).
        let base = crate::core::temporal::time::now().wallclock();
        let vt_early = Timestamp::new(base - 10_000_000, 0).unwrap();
        let vt_late = Timestamp::new(base - 1_000_000, 0).unwrap();
        // Initial assertion, valid from an early real-world time.
        let id = db
            .create_node_with_options(
                "Person",
                PropertyMapBuilder::new().insert("city", "Paris").build(),
                WriteRequestOptions::new().with_valid_from(vt_early),
            )
            .unwrap();
        // Correction: same valid_from, fix the recorded value later in tx-time.
        db.update_node_with_options(
            id,
            PropertyMapBuilder::new().insert("city", "Lyon").build(),
            WriteRequestOptions::new().with_valid_from(vt_early),
        )
        .unwrap();
        // World-change: a later valid_from (the fact changed).
        db.update_node_with_options(
            id,
            PropertyMapBuilder::new().insert("city", "Nice").build(),
            WriteRequestOptions::new().with_valid_from(vt_late),
        )
        .unwrap();

        let log = db
            .belief_revisions(EntityId::Node(id), &RevisionOptions::default())
            .unwrap();
        assert_eq!(log.revisions.len(), 3);
        assert_eq!(log.revisions[0].class, RevisionClass::InitialAssertion);
        assert_eq!(log.revisions[1].class, RevisionClass::Correction);
        assert_eq!(log.revisions[2].class, RevisionClass::WorldChange);
    }

    #[test]
    fn end_to_end_retraction() {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();
        // Retract (close valid time) — Bob has no edges.
        db.retract_node(id, crate::core::temporal::time::now())
            .unwrap();
        let log = db
            .belief_revisions(EntityId::Node(id), &RevisionOptions::default())
            .unwrap();
        assert!(log.revisions.len() >= 2);
        assert_eq!(
            log.revisions.last().unwrap().class,
            RevisionClass::Retraction
        );
    }

    #[test]
    fn reretraction_no_extra_revision() {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Cara").build(),
            )
            .unwrap();
        let t = crate::core::temporal::time::now();
        db.retract_node(id, t).unwrap();
        let after_first = db
            .belief_revisions(EntityId::Node(id), &RevisionOptions::default())
            .unwrap()
            .revisions
            .len();
        // Idempotent re-retraction: no new version appended.
        db.retract_node(id, t).unwrap();
        let after_second = db
            .belief_revisions(EntityId::Node(id), &RevisionOptions::default())
            .unwrap()
            .revisions
            .len();
        assert_eq!(after_first, after_second, "re-retraction adds no revision");
    }

    #[test]
    fn edge_belief_revisions() {
        let db = AletheiaDB::new().unwrap();
        let a = db
            .create_node("Person", PropertyMapBuilder::new().insert("n", "a").build())
            .unwrap();
        let b = db
            .create_node("Person", PropertyMapBuilder::new().insert("n", "b").build())
            .unwrap();
        let e = db
            .create_edge(
                a,
                b,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2020).build(),
            )
            .unwrap();
        db.update_edge_with_valid_time(
            e,
            PropertyMapBuilder::new().insert("since", 2021).build(),
            None,
        )
        .unwrap();
        let log = db
            .belief_revisions(EntityId::Edge(e), &RevisionOptions::default())
            .unwrap();
        assert!(log.revisions.len() >= 2);
        assert_eq!(log.revisions[0].class, RevisionClass::InitialAssertion);
    }
}
