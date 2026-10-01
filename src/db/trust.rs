//! Database-level trust propagation: computed confidence over lineage (Issue #3382).
//!
//! Wires the [`TrustRegistry`](crate::experimental::reasoning::trust_propagation::TrustRegistry)
//! into a lazy, read-time evaluator that walks a fact's derivation lineage
//! (Issue #3371) and combines its upstream confidences under the active
//! [`TrustPolicy`]. Computed confidence is **never stored**: every read
//! recomputes from current state, so a superseding write or an Issue #3230
//! retraction of an upstream fact flows downstream for free (AC4), and asking
//! `AS OF` transaction time `T` replays confidences as recorded at `T` (AC5).
//!
//! # Reactive-at-now, stable-as-of
//!
//! [`resolve_ref`](AletheiaDB::resolve_ref) resolves each version-pinned lineage
//! reference to the **visible** version at the evaluation transaction time
//! (the latest version whose transaction-time start `<= tt`) and reads that
//! version's provenance confidence. With `tt = now` the visible version is the
//! current head, so an upstream update is reflected immediately; with `tt = T`
//! the visible version is the head as recorded at `T`, so a historical read is
//! revision-stable.
//!
//! # Leaf & status rules
//!
//! - A fact with **no visible lineage** is a leaf: its computed confidence is
//!   its declared confidence (`computed == declared`), or the per-label
//!   [`MissingConfidencePolicy`] value when the writer asserted none (always
//!   flagged, AC6).
//! - A **retracted** (valid interval ended as of now) or **absent** (deleted /
//!   dangling) upstream contributes `0.0` and **dominates**: the node combine
//!   step explicitly checks each direct child's terminal status and forces its
//!   own value to `0.0` under BOTH combinators (never relying on `0.0` being a
//!   noisy-OR identity term, which would let a live sibling absorb it — Group 1
//!   fix). The two are distinct [`ConfidenceSource`] variants (review-fix #2).
//!
//! # Valid-time-aware terminality (Issue #3382 follow-up)
//!
//! Terminality is keyed on an explicit **valid-time evaluation coordinate**,
//! independent of the transaction-time `AS OF` scope (valid time and
//! transaction time are orthogonal axes): a fact whose valid interval has
//! *ended* at the coordinate (`valid_to <= valid_now`) is [`Retracted`], while a
//! fact retracted **effective-after** the coordinate or holding a
//! naturally-bounded interval that **contains** the coordinate is still **live**
//! and contributes its confidence. The coordinate flows through the whole
//! recursive lineage closure, so every upstream fact is judged terminal-or-not
//! at the same valid time.
//!
//! The coordinate is supplied by
//! [`computed_confidence_as_of_bitemporal`](AletheiaDB::computed_confidence_as_of_bitemporal)
//! and by [`TrustOptions::as_of_valid_time`]. When it is omitted it defaults to
//! wallclock `time::now()`, which reproduces the prior behavior **exactly**:
//! [`computed_confidence`](AletheiaDB::computed_confidence) evaluates at
//! `(now, now)` and
//! [`computed_confidence_as_of`](AletheiaDB::computed_confidence_as_of)
//! evaluates valid time at `now` while scoping transaction time to `tt`.
//!
//! A version that merely **predates** the as-of `tt` (not yet recorded at `T`)
//! is Absent and contributes `0.0`, but is NOT a retraction and does not flag
//! `has_retracted_inputs` (adversarial #7) — this tx-time carve-out is
//! unaffected by the valid-time coordinate.
//!
//! [`TrustOptions::as_of_valid_time`]: crate::experimental::reasoning::trust_propagation::TrustOptions::as_of_valid_time
//!
//! [`Retracted`]: ConfidenceSource::Retracted

#![cfg(feature = "semantic-reasoning")]

use std::collections::{HashMap, HashSet};

use crate::core::error::{Error, Result, StorageError};
use crate::core::id::{EdgeId, EntityId, NodeId, VersionId};
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::lineage::{LineageRecord, LineageRef};
use crate::core::provenance::Provenance;
use crate::core::temporal::{Timestamp, time};
use crate::db::AletheiaDB;
use crate::db::lineage::FactStatus;
use crate::experimental::reasoning::trust_propagation::{
    ComputedConfidence, ComputedConfidenceFilter, ConfidenceSource, MissingConfidencePolicy,
    NEUTRAL, SCALAR_MAX_DEPTH, TrustBreakdown, TrustOptions, TrustPolicy, TrustPolicyView, clamp01,
    combine_values,
};
use crate::storage::historical::HistoricalStorage;

/// The result of resolving a version-pinned [`LineageRef`] to the fact version
/// visible at a given transaction time.
struct ResolvedRef {
    /// The visible version (meaningful only when `terminal` is `None`).
    version: VersionId,
    /// Current-vs-superseded status of the pinned reference against the visible
    /// version (only meaningful when `terminal` is `None`).
    status: FactStatus,
    /// The visible version's declared provenance confidence, if any.
    confidence: Option<f64>,
    /// The visible version's entity label, used to resolve the per-label policy.
    label: String,
    /// `Some` when the fact is gone: [`ConfidenceSource::Retracted`] (valid-time
    /// closed) or [`ConfidenceSource::Absent`] (deleted / dangling). Both
    /// contribute `0.0` and dominate.
    terminal: Option<ConfidenceSource>,
    /// `true` when `terminal` is `Absent` **only because the fact predates the
    /// evaluation transaction time** (no version was recorded at or before `tt`)
    /// rather than because it was deleted / retracted. Such a fact contributes
    /// `0.0` but is NOT a retraction: it does not dominate and does not flag
    /// `has_retracted_inputs` (adversarial #7).
    not_yet_recorded: bool,
}

/// The scalar result of evaluating a fact's computed confidence, plus the flags
/// propagated up the recursion.
#[derive(Clone)]
struct ScalarEval {
    /// The computed confidence value in `[0, 1]`.
    value: f64,
    /// `true` when this input should be dropped from its parent's contributing
    /// set (an [`MissingConfidencePolicy::Ignore`]-excluded missing leaf).
    excluded: bool,
    /// `true` when this eval is itself a directly-terminal, **dominating**
    /// retracted/absent contributor (contributes `0.0` and forces its parent's
    /// value to `0.0` under BOTH combinators). Set only on the terminal leaf,
    /// never propagated: a node capped to `0.0` by a dominating child reports
    /// `dominating == false`, so its `0.0` flows to *its* parent as an ordinary
    /// value. A fact that merely predates the evaluation time (not-yet-recorded)
    /// is NOT dominating (adversarial #7).
    dominating: bool,
    /// Whether any missing-confidence input was encountered in this subtree.
    has_missing: bool,
    /// Whether any retracted/absent input was encountered in this subtree.
    has_retracted: bool,
    /// Whether a hard depth/cycle bound truncated this subtree.
    truncated: bool,
}

fn intern_to_string(label: InternedString) -> String {
    GLOBAL_INTERNER
        .resolve_with(label, |s| s.to_string())
        .unwrap_or_else(|| label.to_string())
}

impl AletheiaDB {
    // ===================================================================
    // Policy management (durable sidecar registry)
    // ===================================================================

    /// The database-wide default trust policy (AC1 discoverable).
    #[must_use]
    pub fn trust_policy(&self) -> TrustPolicy {
        self.trust_policies.default_policy()
    }

    /// The trust policy governing facts with `label` — the per-label override
    /// if one is registered, else the database default.
    #[must_use]
    pub fn trust_policy_for_label(&self, label: &str) -> TrustPolicy {
        self.trust_policies.policy_for_label(label)
    }

    /// A stable, label-sorted view of every active trust policy (AC1).
    #[must_use]
    pub fn list_trust_policies(&self) -> TrustPolicyView {
        self.trust_policies.view()
    }

    /// Set the database-wide default trust policy, persisting the change.
    ///
    /// # Errors
    ///
    /// Returns an error if the durable sidecar write fails (the in-memory
    /// change is rolled back).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn set_trust_policy(&self, policy: TrustPolicy) -> Result<()> {
        self.trust_policies.set_default(policy)
    }

    /// Set a per-label / per-edge-type override policy, persisting the change.
    ///
    /// # Errors
    ///
    /// Returns an error if the durable sidecar write fails (the in-memory
    /// change is rolled back).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn set_trust_policy_for_label(&self, label: &str, policy: TrustPolicy) -> Result<()> {
        self.trust_policies.set_label(label, policy)
    }

    /// Remove a per-label override, reverting facts with that label to the
    /// database default. Dropping an absent label is a no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if the durable sidecar write fails.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn drop_trust_policy_for_label(&self, label: &str) -> Result<()> {
        self.trust_policies.drop_label(label)
    }

    // ===================================================================
    // Computed confidence (lazy, AC2/AC4/AC5)
    // ===================================================================

    /// The computed confidence of `reference` as of now (AC2).
    ///
    /// Walks the fact's upstream derivation lineage and combines the upstream
    /// confidences under the active policy, keeping the writer-declared value
    /// (`declared`) distinct from the derived value (`computed`). A fact with no
    /// lineage keeps its declared confidence unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::VersionNotFound`] if `reference` does not resolve
    /// to an existing version of its entity.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn computed_confidence(&self, reference: LineageRef) -> Result<ComputedConfidence> {
        self.computed_confidence_as_of(reference, time::now())
    }

    /// The computed confidence of `reference` as recorded at transaction time
    /// `tt` (AC5) — "how confident were we then?".
    ///
    /// With `tt` at or after the latest transaction time this equals
    /// [`computed_confidence`](Self::computed_confidence) (review-fix #4).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::VersionNotFound`] if `reference` does not resolve
    /// to an existing version of its entity.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn computed_confidence_as_of(
        &self,
        reference: LineageRef,
        tt: Timestamp,
    ) -> Result<ComputedConfidence> {
        // Default the valid-time evaluation coordinate to wallclock now, which
        // reproduces the prior (single-axis `AS OF` tx-time) behavior exactly.
        self.computed_confidence_as_of_bitemporal(reference, time::now(), tt)
    }

    /// The computed confidence of `reference` at the bi-temporal coordinate
    /// `(valid_time, tt)` (Issue #3382 valid-time-aware follow-up).
    ///
    /// Valid time and transaction time are **independent** axes: `tt` scopes
    /// *which recorded version* is visible ("how confident were we then?"),
    /// while `valid_time` scopes *terminality* — a fact whose valid interval has
    /// ended at `valid_time` is [`Retracted`](ConfidenceSource::Retracted),
    /// whereas a future-dated retraction (or a naturally-bounded interval that
    /// still contains `valid_time`) is live and contributes its confidence. The
    /// coordinate flows through the whole recursive lineage closure, so every
    /// upstream fact is judged terminal-or-not at the same `valid_time`.
    ///
    /// [`computed_confidence_as_of`](Self::computed_confidence_as_of) is exactly
    /// this method with `valid_time = time::now()`, and
    /// [`computed_confidence`](Self::computed_confidence) is `(now, now)`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::VersionNotFound`] if `reference` does not resolve
    /// to an existing version of its entity.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn computed_confidence_as_of_bitemporal(
        &self,
        reference: LineageRef,
        valid_time: Timestamp,
        tt: Timestamp,
    ) -> Result<ComputedConfidence> {
        self.ensure_ref_exists(reference)?;
        let historical = self.historical.read();

        // Valid-time terminality is keyed on the explicit `valid_time`
        // coordinate, independent of the transaction-time `tt` scope (the two
        // axes are orthogonal): a fact retracted/bounded effective-after
        // `valid_time` is still LIVE at `valid_time`, and a naturally-bounded
        // interval that contains `valid_time` still contributes.
        let valid_now = valid_time;
        let resolved = self.resolve_ref(reference, tt, valid_now, &historical);
        let mut memo: HashMap<VersionId, ScalarEval> = HashMap::new();
        let mut stack: HashSet<VersionId> = HashSet::new();
        let eval = self.eval_scalar(
            reference,
            tt,
            valid_now,
            &historical,
            &mut memo,
            &mut stack,
            0,
        );

        // Root lineage decides `has_lineage` / `combinator` for the response.
        let root_lineage = if resolved.terminal.is_none() {
            self.visible_lineage(resolved.version, tt)
        } else {
            None
        };
        let has_lineage = root_lineage.is_some();
        let combinator = if has_lineage {
            Some(self.trust_policy_for_label(&resolved.label).combinator)
        } else {
            None
        };

        Ok(ComputedConfidence {
            declared: resolved.confidence,
            computed: eval.value,
            has_lineage,
            combinator,
            has_missing_inputs: eval.has_missing,
            has_retracted_inputs: eval.has_retracted,
            truncated: eval.truncated,
        })
    }

    /// The computed confidence of a node's current version (AC2).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NodeNotFound`] if the node has no current
    /// version.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn node_computed_confidence(&self, node_id: NodeId) -> Result<ComputedConfidence> {
        let reference = self
            .node_lineage_ref(node_id)
            .ok_or_else(|| Error::from(StorageError::NodeNotFound(node_id)))?;
        self.computed_confidence(reference)
    }

    /// The computed confidence of an edge's current version (AC2).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::EdgeNotFound`] if the edge has no current
    /// version.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn edge_computed_confidence(&self, edge_id: EdgeId) -> Result<ComputedConfidence> {
        let reference = self
            .edge_lineage_ref(edge_id)
            .ok_or_else(|| Error::from(StorageError::EdgeNotFound(edge_id)))?;
        self.computed_confidence(reference)
    }

    // ===================================================================
    // Computed-confidence predicate & composition (AC7)
    // ===================================================================

    /// Whether `reference`'s computed confidence satisfies `filter` (AC7).
    ///
    /// An inactive filter matches every fact. Composes with the declared
    /// confidence [`ProvenanceFilter`](crate::core::provenance::ProvenanceFilter)
    /// via [`passes_confidence_filters`](Self::passes_confidence_filters).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::VersionNotFound`] if `reference` does not resolve.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn computed_confidence_matches(
        &self,
        reference: LineageRef,
        filter: &ComputedConfidenceFilter,
    ) -> Result<bool> {
        if !filter.is_active() {
            // Inactive filter still validates that the reference exists, so a
            // dangling ref is reported rather than silently "matching".
            self.ensure_ref_exists(reference)?;
            return Ok(true);
        }
        let cc = self.computed_confidence(reference)?;
        Ok(filter.matches(cc.computed))
    }

    /// Whether `reference` passes BOTH the declared-confidence
    /// [`ProvenanceFilter`](crate::core::provenance::ProvenanceFilter) (evaluated
    /// against the reference's pinned version) AND the computed-confidence
    /// [`ComputedConfidenceFilter`] (AC7). The two constraints AND together.
    ///
    /// A `None` declared filter (or an inactive one) imposes no declared
    /// constraint; an inactive computed filter imposes no computed constraint.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::VersionNotFound`] if `reference` does not resolve.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn passes_confidence_filters(
        &self,
        reference: LineageRef,
        declared: Option<&crate::core::provenance::ProvenanceFilter>,
        computed: &ComputedConfidenceFilter,
    ) -> Result<bool> {
        self.ensure_ref_exists(reference)?;
        if let Some(declared) = declared
            && declared.is_active()
        {
            let provenance = self.pinned_version_provenance(reference)?;
            if !declared.matches(provenance.as_ref()) {
                return Ok(false);
            }
        }
        self.computed_confidence_matches(reference, computed)
    }

    /// Retain only the references whose computed confidence satisfies `filter`,
    /// preserving input order (AC7 — filtering reads/traversal results).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::VersionNotFound`] if any reference does not
    /// resolve.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn filter_by_computed_confidence(
        &self,
        references: &[LineageRef],
        filter: &ComputedConfidenceFilter,
    ) -> Result<Vec<LineageRef>> {
        let mut kept = Vec::new();
        for reference in references {
            if self.computed_confidence_matches(*reference, filter)? {
                kept.push(*reference);
            }
        }
        Ok(kept)
    }

    /// The computed confidence of `reference` as a fusion signal for
    /// provenance-weighted retrieval (Issue #3372), or `None` if it cannot be
    /// evaluated. Gated behind both the `semantic-reasoning` and
    /// `semantic-retrieval-fusion` cohorts — #3372 owns the fusion mechanics;
    /// this only supplies the signal.
    #[cfg(feature = "semantic-retrieval-fusion")]
    #[must_use]
    pub fn computed_confidence_signal(&self, reference: LineageRef) -> Option<f64> {
        self.computed_confidence(reference)
            .ok()
            .map(|cc| cc.computed)
    }

    // ===================================================================
    // Internal evaluation helpers
    // ===================================================================

    /// The provenance bundle recorded on the reference's pinned version, if any.
    fn pinned_version_provenance(&self, reference: LineageRef) -> Result<Option<Provenance>> {
        let historical = self.historical.read();
        let provenance = match reference.entity {
            EntityId::Node(_) => historical
                .get_node_version(reference.version)
                .and_then(|v| v.provenance.as_deref().cloned()),
            EntityId::Edge(_) => historical
                .get_edge_version(reference.version)
                .and_then(|v| v.provenance.as_deref().cloned()),
        };
        Ok(provenance)
    }

    /// Validate that `reference` resolves to an existing version whose entity
    /// matches the reference.
    fn ensure_ref_exists(&self, reference: LineageRef) -> Result<()> {
        let historical = self.historical.read();
        let ok = match reference.entity {
            EntityId::Node(node_id) => historical
                .get_node_version(reference.version)
                .is_some_and(|v| v.node_id == node_id),
            EntityId::Edge(edge_id) => historical
                .get_edge_version(reference.version)
                .is_some_and(|v| v.edge_id == edge_id),
        };
        if ok {
            Ok(())
        } else {
            Err(StorageError::VersionNotFound(reference.version).into())
        }
    }

    /// The visible lineage record for `version` at transaction time `tt`: the
    /// record for that version, dropped if it was recorded after `tt`.
    pub(crate) fn visible_lineage(
        &self,
        version: VersionId,
        tt: Timestamp,
    ) -> Option<LineageRecord> {
        self.lineage
            .record_for(version)
            .filter(|record| record.recorded_at <= tt)
    }

    /// Resolve a version-pinned reference to the fact version visible at
    /// transaction time `tt`, classifying its terminality against the
    /// valid-time evaluation coordinate `valid_now` (the caller-supplied
    /// valid-time at which trust is evaluated, defaulting to wallclock now when
    /// unscoped; independent of `tt`).
    ///
    /// Walks the entity's version chain from its current head backward to the
    /// latest version whose transaction-time start `<= tt`. Its valid interval
    /// is then classified by [`classify_valid_interval`]: an **empty** interval
    /// is a delete tombstone ([`ConfidenceSource::Absent`]); an interval that
    /// has **ended** as of `valid_now` (`valid_to <= valid_now`) is a retraction
    /// ([`ConfidenceSource::Retracted`]); an interval still open or that
    /// currently **contains** `valid_now` is live and contributes its confidence
    /// (Group 2 fix — an effective-future retraction or a naturally-bounded
    /// interval containing now is NOT terminal).
    ///
    /// When no version was recorded at or before `tt`, the fact is absent: it is
    /// flagged `not_yet_recorded` when versions exist but were all recorded after
    /// `tt` (predates `tt`, not a retraction — adversarial #7), and a plain
    /// dangling absence otherwise.
    fn resolve_ref(
        &self,
        reference: LineageRef,
        tt: Timestamp,
        valid_now: Timestamp,
        historical: &HistoricalStorage,
    ) -> ResolvedRef {
        match reference.entity {
            EntityId::Node(node_id) => {
                let mut current = historical.get_current_node_version(node_id);
                let mut saw_any = false;
                while let Some(vid) = current {
                    let Some(version) = historical.get_node_version(vid) else {
                        break;
                    };
                    saw_any = true;
                    if version.commit_timestamp <= tt {
                        let valid = version.temporal.valid_time();
                        let terminal =
                            classify_valid_interval(valid.start(), valid.end(), valid_now);
                        let status = if version.id == reference.version {
                            FactStatus::Current
                        } else {
                            FactStatus::Superseded
                        };
                        return ResolvedRef {
                            version: version.id,
                            status,
                            confidence: version
                                .provenance
                                .as_deref()
                                .and_then(Provenance::confidence),
                            label: intern_to_string(version.label),
                            terminal,
                            not_yet_recorded: false,
                        };
                    }
                    current = version.prev_version;
                }
                // No version at or before `tt`: dangling (no versions at all) vs
                // predates `tt` (versions exist but were all recorded after `tt`,
                // i.e. not yet recorded at `tt`). Only the former is a retraction.
                absent_ref(saw_any)
            }
            EntityId::Edge(edge_id) => {
                let mut current = historical.get_current_edge_version(edge_id);
                let mut saw_any = false;
                while let Some(vid) = current {
                    let Some(version) = historical.get_edge_version(vid) else {
                        break;
                    };
                    saw_any = true;
                    if version.commit_timestamp <= tt {
                        let valid = version.temporal.valid_time();
                        let terminal =
                            classify_valid_interval(valid.start(), valid.end(), valid_now);
                        let status = if version.id == reference.version {
                            FactStatus::Current
                        } else {
                            FactStatus::Superseded
                        };
                        return ResolvedRef {
                            version: version.id,
                            status,
                            confidence: version
                                .provenance
                                .as_deref()
                                .and_then(Provenance::confidence),
                            label: intern_to_string(version.label),
                            terminal,
                            not_yet_recorded: false,
                        };
                    }
                    current = version.prev_version;
                }
                absent_ref(saw_any)
            }
        }
    }

    /// Recursively evaluate the computed confidence of `reference` at `tt`.
    ///
    /// Memoizes per visible [`VersionId`] so a diamond DAG evaluates each shared
    /// ancestor once, guards the DFS path with `stack` (belt-and-braces against
    /// the impossible cycle), and hard-caps recursion at [`SCALAR_MAX_DEPTH`].
    #[allow(clippy::too_many_arguments)]
    fn eval_scalar(
        &self,
        reference: LineageRef,
        tt: Timestamp,
        valid_now: Timestamp,
        historical: &HistoricalStorage,
        memo: &mut HashMap<VersionId, ScalarEval>,
        stack: &mut HashSet<VersionId>,
        depth: usize,
    ) -> ScalarEval {
        let resolved = self.resolve_ref(reference, tt, valid_now, historical);

        // Retracted / absent dominates: 0.0, stop recursion (AC4). A fact that
        // merely predates `tt` (not yet recorded at the evaluation time) also
        // resolves terminal-absent but is NOT a retraction and does NOT dominate
        // or flag `has_retracted` (adversarial #7).
        if resolved.terminal.is_some() {
            let dominating = !resolved.not_yet_recorded;
            return ScalarEval {
                value: 0.0,
                excluded: false,
                dominating,
                has_missing: false,
                has_retracted: dominating,
                truncated: false,
            };
        }

        let vid = resolved.version;
        if let Some(cached) = memo.get(&vid) {
            return cached.clone();
        }

        // Hard depth backstop (review-fix #3): treat conservatively as a leaf,
        // never over-trusting the unreached subtree.
        if depth >= SCALAR_MAX_DEPTH || !stack.insert(vid) {
            return ScalarEval {
                value: resolved.confidence.map(clamp01).unwrap_or(0.0),
                excluded: false,
                dominating: false,
                has_missing: resolved.confidence.is_none(),
                has_retracted: false,
                truncated: true,
            };
        }

        let eval = match self.visible_lineage(vid, tt) {
            None => self.leaf_eval(&resolved),
            Some(record) => {
                let combinator = self.trust_policy_for_label(&resolved.label).combinator;
                let mut child_values: Vec<Option<f64>> = Vec::with_capacity(record.sources.len());
                let mut has_missing = false;
                let mut has_retracted = false;
                let mut truncated = false;
                let mut any_dominating = false;
                for source in &record.sources {
                    let child = self.eval_scalar(
                        *source,
                        tt,
                        valid_now,
                        historical,
                        memo,
                        stack,
                        depth + 1,
                    );
                    has_missing |= child.has_missing;
                    has_retracted |= child.has_retracted;
                    truncated |= child.truncated;
                    any_dominating |= child.dominating;
                    child_values.push(if child.excluded {
                        None
                    } else {
                        Some(child.value)
                    });
                }
                if any_dominating {
                    // A directly-terminal (retracted / absent) contributor forces
                    // this node to 0.0 under BOTH combinators — an explicit
                    // terminal-child short-circuit, never relying on 0.0 being a
                    // noisy-OR identity term (Group 1 review fix). Domination is
                    // local: the resulting 0.0 flows to the parent as an ordinary
                    // value (`dominating: false`), while `has_retracted` bubbles up.
                    ScalarEval {
                        value: 0.0,
                        excluded: false,
                        dominating: false,
                        has_missing,
                        has_retracted: true,
                        truncated,
                    }
                } else {
                    let (value, all_excluded) = combine_values(&child_values, combinator);
                    if all_excluded {
                        has_missing = true;
                    }
                    ScalarEval {
                        value,
                        excluded: false,
                        dominating: false,
                        has_missing,
                        has_retracted,
                        truncated,
                    }
                }
            }
        };

        stack.remove(&vid);
        memo.insert(vid, eval.clone());
        eval
    }

    /// Evaluate a leaf fact (no visible lineage): its declared confidence, or
    /// the per-label missing-confidence policy value when none was written.
    fn leaf_eval(&self, resolved: &ResolvedRef) -> ScalarEval {
        match resolved.confidence {
            Some(c) => ScalarEval {
                value: clamp01(c),
                excluded: false,
                dominating: false,
                has_missing: false,
                has_retracted: false,
                truncated: false,
            },
            None => {
                let missing = self.trust_policy_for_label(&resolved.label).missing;
                let (value, excluded) = match missing {
                    MissingConfidencePolicy::Zero => (0.0, false),
                    MissingConfidencePolicy::Neutral => (NEUTRAL, false),
                    MissingConfidencePolicy::Ignore => (NEUTRAL, true),
                };
                ScalarEval {
                    value,
                    excluded,
                    dominating: false,
                    has_missing: true,
                    has_retracted: false,
                    truncated: false,
                }
            }
        }
    }

    /// A full-accuracy scalar evaluation of `reference` at `tt` with fresh memo
    /// and stack. The breakdown reads confidences from a single shared memo
    /// (Group 3); this is the defensive fallback for a node absent from that memo
    /// so a reported confidence stays full-accuracy regardless of the
    /// presentation depth / node-count cap (review-fix #1).
    fn eval_scalar_value(
        &self,
        reference: LineageRef,
        tt: Timestamp,
        valid_now: Timestamp,
        historical: &HistoricalStorage,
    ) -> f64 {
        let mut memo: HashMap<VersionId, ScalarEval> = HashMap::new();
        let mut stack: HashSet<VersionId> = HashSet::new();
        self.eval_scalar(
            reference, tt, valid_now, historical, &mut memo, &mut stack, 0,
        )
        .value
    }

    // ===================================================================
    // Explainable breakdown (AC3)
    // ===================================================================

    /// The explainable computation tree for `reference` (AC3).
    ///
    /// Returns the per-fact breakdown: each upstream fact's contribution, its
    /// own confidence (declared or computed), the combinator applied at each
    /// node, and the resulting value — bounded by
    /// [`TrustOptions::max_depth`] and [`TrustOptions::max_nodes`] with the
    /// standard truncation signal ([`TrustBreakdown::truncated`], Issue #3226).
    ///
    /// The reported per-node `confidence` is always the **full-accuracy**
    /// computed value, even where the tree is truncated (review-fix #1): the
    /// caps govern how many nodes are serialized, never how many confidences are
    /// combined, so a truncated explanation never over- or under-trusts.
    #[must_use]
    pub fn trust_breakdown(&self, reference: LineageRef, options: &TrustOptions) -> TrustBreakdown {
        let tt = options.as_of.unwrap_or_else(time::now);
        // Valid-time terminality is keyed on the explicit valid-time coordinate
        // when supplied (Issue #3382), independent of the tx-time `as_of` scope;
        // omitting it defaults to wallclock now (prior behavior).
        let valid_now = options.as_of_valid_time.unwrap_or_else(time::now);
        let historical = self.historical.read();

        // Compute the full-accuracy scalar values for the WHOLE closure ONCE
        // (Group 3): a single `eval_scalar` from the root fills a shared memo the
        // breakdown then reads per node, turning the previous per-node fresh
        // subtree walk (O(n^2)) into O(n). The memo holds full values computed
        // independently of the presentation depth/node caps, so a truncated
        // breakdown still reports full-accuracy confidences (review-fix #1).
        let mut memo: HashMap<VersionId, ScalarEval> = HashMap::new();
        let mut scalar_stack: HashSet<VersionId> = HashSet::new();
        let _ = self.eval_scalar(
            reference,
            tt,
            valid_now,
            &historical,
            &mut memo,
            &mut scalar_stack,
            0,
        );

        let mut budget = options.max_nodes;
        let mut stack: HashSet<VersionId> = HashSet::new();
        self.build_breakdown(
            reference,
            tt,
            valid_now,
            &historical,
            &memo,
            options.max_depth,
            0,
            &mut budget,
            &mut stack,
        )
    }

    /// Build one node of the breakdown tree, descending into children while the
    /// depth / node-count budget allows.
    #[allow(clippy::too_many_arguments)]
    fn build_breakdown(
        &self,
        reference: LineageRef,
        tt: Timestamp,
        valid_now: Timestamp,
        historical: &HistoricalStorage,
        memo: &HashMap<VersionId, ScalarEval>,
        max_depth: usize,
        depth: usize,
        budget: &mut usize,
        stack: &mut HashSet<VersionId>,
    ) -> TrustBreakdown {
        let resolved = self.resolve_ref(reference, tt, valid_now, historical);

        // Terminal: retracted / absent -> 0.0, no children.
        if let Some(terminal) = resolved.terminal {
            return TrustBreakdown {
                reference,
                status: FactStatus::Absent,
                confidence: 0.0,
                source: terminal,
                combinator: None,
                children: Vec::new(),
                truncated: false,
            };
        }

        let vid = resolved.version;
        let status = resolved.status;

        // Leaf: no visible lineage -> declared or missing-policy value.
        let Some(record) = self.visible_lineage(vid, tt) else {
            let (confidence, source) = leaf_confidence_source(self, &resolved);
            return TrustBreakdown {
                reference,
                status,
                confidence,
                source,
                combinator: None,
                children: Vec::new(),
                truncated: false,
            };
        };

        let combinator = self.trust_policy_for_label(&resolved.label).combinator;
        // The node's reported confidence is ALWAYS the full-accuracy value: read
        // it from the shared memo (Group 3, O(n)); fall back to a fresh scalar
        // eval only if the version was not memoized (defensive — not reached for
        // trees within the scalar depth cap).
        let confidence = memo
            .get(&vid)
            .map(|eval| eval.value)
            .unwrap_or_else(|| self.eval_scalar_value(reference, tt, valid_now, historical));

        // Stop descent at the depth cap or on the (impossible) cycle: keep the
        // full-accuracy confidence, drop the children, flag truncated.
        if depth >= max_depth || !stack.insert(vid) {
            return TrustBreakdown {
                reference,
                status,
                confidence,
                source: ConfidenceSource::Computed,
                combinator: Some(combinator),
                children: Vec::new(),
                truncated: true,
            };
        }

        let mut children = Vec::with_capacity(record.sources.len());
        let mut truncated = false;
        for source in &record.sources {
            if *budget == 0 {
                // Node budget exhausted: stop emitting siblings, flag truncated.
                truncated = true;
                break;
            }
            *budget -= 1;
            let child = self.build_breakdown(
                *source,
                tt,
                valid_now,
                historical,
                memo,
                max_depth,
                depth + 1,
                budget,
                stack,
            );
            truncated |= child.truncated;
            children.push(child);
        }

        stack.remove(&vid);

        TrustBreakdown {
            reference,
            status,
            confidence,
            source: ConfidenceSource::Computed,
            combinator: Some(combinator),
            children,
            truncated,
        }
    }
}

/// The displayed confidence and [`ConfidenceSource`] of a leaf fact (no visible
/// lineage): its declared confidence, or the per-label missing-confidence
/// policy value when none was written.
fn leaf_confidence_source(db: &AletheiaDB, resolved: &ResolvedRef) -> (f64, ConfidenceSource) {
    match resolved.confidence {
        Some(c) => (clamp01(c), ConfidenceSource::Declared),
        None => {
            let value = match db.trust_policy_for_label(&resolved.label).missing {
                MissingConfidencePolicy::Zero => 0.0,
                MissingConfidencePolicy::Neutral | MissingConfidencePolicy::Ignore => NEUTRAL,
            };
            (value, ConfidenceSource::Missing)
        }
    }
}

/// Classify a version's valid interval into a terminal source, if any, relative
/// to the valid-time evaluation coordinate `valid_now` (defaults to wallclock
/// now).
///
/// - An **empty** interval (`start == end`) is a delete tombstone
///   ([`ConfidenceSource::Absent`]).
/// - An interval that has **ended** as of `valid_now` (`end <= valid_now`) is an
///   Issue #3230 retraction / natural expiry ([`ConfidenceSource::Retracted`]).
/// - Otherwise the interval is live — open, or closed but still containing (or
///   entirely ahead of) `valid_now` — and contributes its confidence (`None`).
///
/// Keying terminality on `valid_now` rather than merely on `is_closed()` is the
/// Group 2 fix: a fact retracted **effective-future** (still valid now) and a
/// naturally-bounded interval that currently **contains** now are both LIVE, not
/// scored `0.0`.
///
/// **v1: terminality is END-only.** A not-yet-valid input (`start > valid_now`,
/// e.g. a #3221 future-dated create) is deliberately still classified live and
/// contributes its confidence at a coordinate before it is valid — the inverse
/// of the tx-time `not_yet_recorded` carve-out. This is a conscious v1 decision;
/// symmetric not-yet-valid (Absent-class) handling that gates on interval START
/// is a tracked follow-up.
fn classify_valid_interval(
    start: Timestamp,
    end: Timestamp,
    valid_now: Timestamp,
) -> Option<ConfidenceSource> {
    if start == end {
        return Some(ConfidenceSource::Absent);
    }
    if end <= valid_now {
        Some(ConfidenceSource::Retracted)
    } else {
        None
    }
}

/// A resolved reference for an absent fact.
///
/// `not_yet_recorded` is `true` when the absence is because the fact **predates**
/// the evaluation transaction time (versions exist but were all recorded later) —
/// not a retraction, so it must not flag `has_retracted_inputs` (adversarial #7).
/// It is `false` for a genuinely dangling / deleted-from-current-state fact.
fn absent_ref(not_yet_recorded: bool) -> ResolvedRef {
    ResolvedRef {
        version: VersionId::new_unchecked(0),
        status: FactStatus::Absent,
        confidence: None,
        label: String::new(),
        terminal: Some(ConfidenceSource::Absent),
        not_yet_recorded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::{WriteOps, WriteRequestOptions};
    use crate::core::property::PropertyMap;
    use crate::experimental::reasoning::trust_propagation::TrustCombinator;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {b}, got {a}");
    }

    fn prov(confidence: f64) -> Provenance {
        Provenance::builder()
            .confidence(confidence)
            .build()
            .unwrap()
    }

    fn opts(confidence: Option<f64>) -> WriteRequestOptions {
        match confidence {
            Some(c) => WriteRequestOptions::new().with_provenance(prov(c)),
            None => WriteRequestOptions::new(),
        }
    }

    /// Create a node with the given label + optional confidence, derived from
    /// `sources`, returning its version-pinned reference.
    fn mk(
        db: &AletheiaDB,
        label: &str,
        confidence: Option<f64>,
        sources: &[LineageRef],
    ) -> LineageRef {
        let (node_id, version) = db
            .create_node_with_options_and_lineage(
                label,
                PropertyMap::new(),
                opts(confidence),
                sources,
            )
            .expect("create node with lineage");
        LineageRef::new(node_id, version)
    }

    // ---- C-4 / C-5: 3-level trees, hand-computed both combinators ----

    #[test]
    fn c4_three_level_weakest_link() {
        let db = AletheiaDB::new().unwrap();
        // default policy = weakest-link + Zero.
        let r1 = mk(&db, "Doc", Some(0.9), &[]);
        let r2 = mk(&db, "Doc", Some(0.8), &[]);
        let r3 = mk(&db, "Doc", Some(0.3), &[]);
        let m1 = mk(&db, "Merge", None, &[r1, r2]); // min(0.9,0.8)=0.8
        let m2 = mk(&db, "Merge", None, &[r3]); // pass-through 0.3
        let top = mk(&db, "Merge", None, &[m1, m2]); // min(0.8,0.3)=0.3

        let cc = db.computed_confidence(top).unwrap();
        approx(cc.computed, 0.3);
        assert!(cc.has_lineage);
        assert_eq!(cc.combinator, Some(TrustCombinator::WeakestLink));
        assert!(!cc.has_missing_inputs);
        assert!(!cc.has_retracted_inputs);
    }

    #[test]
    fn c5_three_level_noisy_or() {
        let db = AletheiaDB::new().unwrap();
        db.set_trust_policy(TrustPolicy::noisy_or(MissingConfidencePolicy::Zero))
            .unwrap();
        let r1 = mk(&db, "Doc", Some(0.9), &[]);
        let r2 = mk(&db, "Doc", Some(0.8), &[]);
        let r3 = mk(&db, "Doc", Some(0.3), &[]);
        let m1 = mk(&db, "Merge", None, &[r1, r2]); // 1-(0.1)(0.2)=0.98
        let m2 = mk(&db, "Merge", None, &[r3]); // 0.3
        let top = mk(&db, "Merge", None, &[m1, m2]); // 1-(0.02)(0.7)=0.986

        let cc = db.computed_confidence(top).unwrap();
        approx(cc.computed, 0.986);
        assert_eq!(cc.combinator, Some(TrustCombinator::NoisyOr));
    }

    // ---- C-3: single upstream passes through ----

    #[test]
    fn c3_single_upstream_pass_through() {
        for policy in [
            TrustPolicy::weakest_link(MissingConfidencePolicy::Zero),
            TrustPolicy::noisy_or(MissingConfidencePolicy::Zero),
        ] {
            let db = AletheiaDB::new().unwrap();
            db.set_trust_policy(policy).unwrap();
            let r = mk(&db, "Doc", Some(0.42), &[]);
            let d = mk(&db, "Merge", None, &[r]);
            approx(db.computed_confidence(d).unwrap().computed, 0.42);
        }
    }

    // ---- C-6: 5-level chain, both combinators, hand-computed ----

    #[test]
    fn c6_five_level_chain() {
        // roots 0.9,0.8,0.7,0.6,0.5,0.4 nested one level at a time.
        for (policy, expected) in [
            (
                TrustPolicy::weakest_link(MissingConfidencePolicy::Zero),
                0.4,
            ),
            (
                TrustPolicy::noisy_or(MissingConfidencePolicy::Zero),
                0.99928,
            ),
        ] {
            let db = AletheiaDB::new().unwrap();
            db.set_trust_policy(policy).unwrap();
            let r0 = mk(&db, "Doc", Some(0.9), &[]);
            let a1 = mk(&db, "Doc", Some(0.8), &[]);
            let l1 = mk(&db, "L", None, &[r0, a1]);
            let a2 = mk(&db, "Doc", Some(0.7), &[]);
            let l2 = mk(&db, "L", None, &[l1, a2]);
            let a3 = mk(&db, "Doc", Some(0.6), &[]);
            let l3 = mk(&db, "L", None, &[l2, a3]);
            let a4 = mk(&db, "Doc", Some(0.5), &[]);
            let l4 = mk(&db, "L", None, &[l3, a4]);
            let a5 = mk(&db, "Doc", Some(0.4), &[]);
            let l5 = mk(&db, "L", None, &[l4, a5]);
            approx(db.computed_confidence(l5).unwrap().computed, expected);
        }
    }

    // ---- C-7: diamond DAG ----
    //
    // NOTE: memoization computes the shared ancestor's value once, but that is
    // NOT de-duplication of its influence: noisy-OR treats the two parents `b`
    // and `c` as independent evidence and counts the shared ancestor's 0.6 in
    // EACH, so `d = noisy_or{b=0.6, c=0.6} = 1-(0.4)(0.4) = 0.84`, higher than
    // the ancestor's own 0.6. This is the documented independence approximation
    // (§2.3 / Out-of-Scope probabilistic caveat), not a bug. Under weakest-link
    // the same diamond is `min = 0.6` (min is idempotent under repetition).
    #[test]
    fn c7_diamond_dag() {
        let db = AletheiaDB::new().unwrap();
        db.set_trust_policy(TrustPolicy::noisy_or(MissingConfidencePolicy::Zero))
            .unwrap();
        let r = mk(&db, "Doc", Some(0.6), &[]);
        let b = mk(&db, "Merge", None, &[r]);
        let c = mk(&db, "Merge", None, &[r]);
        let d = mk(&db, "Merge", None, &[b, c]); // 1-(0.4)(0.4)=0.84 (each parent counts r)
        approx(db.computed_confidence(d).unwrap().computed, 0.84);

        // Weakest-link over the same diamond -> 0.6.
        let db = AletheiaDB::new().unwrap();
        let r = mk(&db, "Doc", Some(0.6), &[]);
        let b = mk(&db, "Merge", None, &[r]);
        let c = mk(&db, "Merge", None, &[r]);
        let d = mk(&db, "Merge", None, &[b, c]);
        approx(db.computed_confidence(d).unwrap().computed, 0.6);
    }

    // ---- C-8: per-label override resolved per fact ----

    #[test]
    fn c8_per_label_override() {
        let db = AletheiaDB::new().unwrap();
        // default weakest-link; override "OrNode" to noisy-OR.
        db.set_trust_policy_for_label(
            "OrNode",
            TrustPolicy::noisy_or(MissingConfidencePolicy::Zero),
        )
        .unwrap();
        let r1 = mk(&db, "Doc", Some(0.6), &[]);
        let r2 = mk(&db, "Doc", Some(0.6), &[]);
        // Weakest-link label.
        let wl = mk(&db, "MinNode", None, &[r1, r2]);
        approx(db.computed_confidence(wl).unwrap().computed, 0.6);
        // Noisy-OR label over the SAME roots.
        let or = mk(&db, "OrNode", None, &[r1, r2]);
        approx(db.computed_confidence(or).unwrap().computed, 0.84);
    }

    // ---- C-9 / C-10: no-lineage & declared-vs-computed distinct ----

    #[test]
    fn c9_no_lineage_fact_computed_equals_declared() {
        let db = AletheiaDB::new().unwrap();
        let r = mk(&db, "Doc", Some(0.7), &[]);
        let cc = db.computed_confidence(r).unwrap();
        approx(cc.computed, 0.7);
        assert_eq!(cc.declared, Some(0.7));
        assert!(!cc.has_lineage);
        assert_eq!(cc.combinator, None);
    }

    #[test]
    fn c10_declared_vs_computed_distinct() {
        let db = AletheiaDB::new().unwrap();
        let r = mk(&db, "Doc", Some(0.9), &[]);
        // The derived fact's writer *typed* 0.2, but it is derived from 0.9.
        let d = mk(&db, "Merge", Some(0.2), &[r]);
        let cc = db.computed_confidence(d).unwrap();
        assert_eq!(cc.declared, Some(0.2), "declared is untouched");
        approx(cc.computed, 0.9);
        assert!(cc.has_lineage);
    }

    // ---- M-1 / M-2 / M-3: missing-confidence policies ----

    #[test]
    fn m1_missing_zero_flagged() {
        let db = AletheiaDB::new().unwrap(); // default missing = Zero
        let r = mk(&db, "Doc", None, &[]); // root without confidence
        let cc = db.computed_confidence(r).unwrap();
        approx(cc.computed, 0.0);
        assert_eq!(cc.declared, None);
        assert!(cc.has_missing_inputs, "missing must be flagged");
    }

    #[test]
    fn m2_missing_neutral_flagged() {
        let db = AletheiaDB::new().unwrap();
        db.set_trust_policy(TrustPolicy::weakest_link(MissingConfidencePolicy::Neutral))
            .unwrap();
        let r = mk(&db, "Doc", None, &[]);
        let cc = db.computed_confidence(r).unwrap();
        approx(cc.computed, 0.5);
        assert!(cc.has_missing_inputs);
    }

    #[test]
    fn m3_missing_ignore_dropped_flagged() {
        let db = AletheiaDB::new().unwrap();
        db.set_trust_policy(TrustPolicy::weakest_link(MissingConfidencePolicy::Ignore))
            .unwrap();
        let r_missing = mk(&db, "Doc", None, &[]);
        let r_ok = mk(&db, "Doc", Some(0.8), &[]);
        // Ignore drops the missing input; weakest-link over the rest = 0.8.
        let d = mk(&db, "Merge", None, &[r_missing, r_ok]);
        let cc = db.computed_confidence(d).unwrap();
        approx(cc.computed, 0.8);
        assert!(cc.has_missing_inputs);
    }

    // ---- R-1 / R-2 / R-3: retracted & absent dominate ----

    #[test]
    fn r1_retracted_weakest_link_dominates() {
        let db = AletheiaDB::new().unwrap();
        let r = mk(&db, "Doc", Some(0.9), &[]);
        let r2 = mk(&db, "Doc", Some(0.8), &[]);
        let d = mk(&db, "Merge", None, &[r, r2]);
        // Retract r effective-now: a non-empty valid interval that has ENDED as
        // of now, so it is terminal (Group 2 valid-time-aware terminality).
        if let EntityId::Node(nid) = r.entity {
            db.retract_node(nid, time::now()).unwrap();
        }
        let cc = db.computed_confidence(d).unwrap();
        approx(cc.computed, 0.0); // min(0.0 retracted, 0.8) = 0.0
        assert!(cc.has_retracted_inputs);
    }

    #[test]
    fn r2_retracted_noisy_or_caps_node() {
        let db = AletheiaDB::new().unwrap();
        db.set_trust_policy(TrustPolicy::noisy_or(MissingConfidencePolicy::Zero))
            .unwrap();
        let r = mk(&db, "Doc", Some(0.9), &[]);
        let d = mk(&db, "Merge", None, &[r]); // single retracted source
        if let EntityId::Node(nid) = r.entity {
            db.retract_node(nid, time::now()).unwrap();
        }
        let cc = db.computed_confidence(d).unwrap();
        approx(cc.computed, 0.0); // 1-(1-0)=0 : retracted does not vanish
        assert!(cc.has_retracted_inputs);
    }

    // R-2b: a retracted contributor dominates a MULTI-source noisy-OR node — the
    // 0.0 must NOT be absorbed as the noisy-OR identity term by a live sibling.
    #[test]
    fn r2b_retracted_noisy_or_multisource_dominates() {
        let db = AletheiaDB::new().unwrap();
        db.set_trust_policy(TrustPolicy::noisy_or(MissingConfidencePolicy::Zero))
            .unwrap();
        let r = mk(&db, "Doc", Some(0.9), &[]);
        let live = mk(&db, "Doc", Some(0.9), &[]);
        let d = mk(&db, "Merge", None, &[r, live]);
        // Retract r effective-now: a non-empty closed interval ending at now.
        if let EntityId::Node(nid) = r.entity {
            db.retract_node(nid, time::now()).unwrap();
        }
        let cc = db.computed_confidence(d).unwrap();
        // WITHOUT the terminal-child short-circuit this would be
        // noisy_or{0.0, 0.9} = 0.9 (the retraction silently absorbed).
        approx(cc.computed, 0.0);
        assert!(cc.has_retracted_inputs);
    }

    #[test]
    fn r3_absent_deleted_upstream_dominates() {
        let db = AletheiaDB::new().unwrap();
        let r = mk(&db, "Doc", Some(0.9), &[]);
        let r2 = mk(&db, "Doc", Some(0.8), &[]);
        let d = mk(&db, "Merge", None, &[r, r2]);
        // Delete r: an empty-valid tombstone -> Absent (distinct from Retracted).
        if let EntityId::Node(nid) = r.entity {
            db.write(|tx| tx.delete_node(nid)).unwrap();
        }
        let cc = db.computed_confidence(d).unwrap();
        approx(cc.computed, 0.0);
        assert!(cc.has_retracted_inputs);
    }

    // ---- Group 2: valid-time-aware terminality ----

    // The classifier keys terminality on the eval instant, not merely on
    // is_closed(): empty -> Absent, ended-as-of-now -> Retracted, an interval
    // containing (or ahead of) now -> live.
    #[test]
    fn classify_valid_interval_is_valid_time_aware() {
        let now = time::now();
        let past = Timestamp::new_unchecked(now.wallclock() - 1_000_000, 0);
        let future = Timestamp::new_unchecked(now.wallclock() + 86_400_000_000, 0);
        let far_future = Timestamp::new_unchecked(i64::MAX / 2, 0);
        // Empty interval -> delete tombstone (Absent).
        assert_eq!(
            classify_valid_interval(past, past, now),
            Some(ConfidenceSource::Absent)
        );
        // Interval ended as of now -> Retracted.
        assert_eq!(
            classify_valid_interval(past, now, now),
            Some(ConfidenceSource::Retracted)
        );
        // (b) Bounded interval [past, future) currently CONTAINING now -> live.
        assert_eq!(classify_valid_interval(past, future, now), None);
        // (a) Effective-future retraction [~now, future) -> live.
        assert_eq!(classify_valid_interval(now, future, now), None);
        // Open-ended (effectively unbounded) interval -> live.
        assert_eq!(classify_valid_interval(past, far_future, now), None);
    }

    // (a) A fact retracted effective-FUTURE is still valid now, so it
    // contributes its confidence (provenance re-stamped so the live version
    // keeps it) — NOT scored 0.0.
    #[test]
    fn future_effective_retraction_still_contributes() {
        let db = AletheiaDB::new().unwrap();
        let r = mk(&db, "Doc", Some(0.9), &[]);
        let r2 = mk(&db, "Doc", Some(0.8), &[]);
        let d = mk(&db, "Merge", None, &[r, r2]);
        // Close the valid interval ~1 day out (still valid now); preserve the
        // 0.9 confidence on the live version by re-stamping provenance.
        if let EntityId::Node(nid) = r.entity {
            let valid_to = Timestamp::new_unchecked(time::now().wallclock() + 86_400_000_000, 0);
            db.retract_node_with_provenance(nid, valid_to, Some(prov(0.9)))
                .unwrap();
        }
        let cc = db.computed_confidence(d).unwrap();
        approx(cc.computed, 0.8); // r is live: min(0.9, 0.8) = 0.8
        assert!(!cc.has_retracted_inputs);
    }

    // (b) A fact with a naturally-bounded valid interval [past, future) that
    // currently CONTAINS now contributes its confidence.
    #[test]
    fn bounded_interval_containing_now_contributes() {
        let db = AletheiaDB::new().unwrap();
        let now = time::now().wallclock();
        let past = Timestamp::new_unchecked(now - 86_400_000_000, 0); // 1 day ago
        let (nid, vid) = db
            .create_node_with_options_and_lineage(
                "Doc",
                PropertyMap::new(),
                WriteRequestOptions::new()
                    .with_valid_from(past)
                    .with_provenance(prov(0.9)),
                &[],
            )
            .unwrap();
        let r = LineageRef::new(nid, vid);
        let r2 = mk(&db, "Doc", Some(0.8), &[]);
        let d = mk(&db, "Merge", None, &[r, r2]);
        // Bound the interval 1 day in the future: [1-day-ago, 1-day-hence)
        // contains now; keep the confidence on the live version.
        let future = Timestamp::new_unchecked(now + 86_400_000_000, 0);
        db.retract_node_with_provenance(nid, future, Some(prov(0.9)))
            .unwrap();
        let cc = db.computed_confidence(d).unwrap();
        approx(cc.computed, 0.8); // r live -> min(0.9, 0.8) = 0.8
        assert!(!cc.has_retracted_inputs);
    }

    // Adversarial #7: a fact that PREDATES the as-of transaction time (not yet
    // recorded at T) is Absent and contributes 0.0, but is NOT a retraction — it
    // must not flag `has_retracted_inputs`.
    #[test]
    fn predates_transaction_time_is_absent_without_retracted_flag() {
        let db = AletheiaDB::new().unwrap();
        let t_before = time::now(); // captured BEFORE the fact is recorded
        let r = mk(&db, "Doc", Some(0.9), &[]);
        let cc = db.computed_confidence_as_of(r, t_before).unwrap();
        approx(cc.computed, 0.0); // did not exist at T -> 0.0
        assert!(
            !cc.has_retracted_inputs,
            "predates-T is not a retraction, must not flag has_retracted_inputs"
        );
    }

    // ---- T1a..T1h: valid-time-AWARE terminality (#3382 follow-up) ----
    //
    // These exercise the explicit valid-time evaluation coordinate: terminality
    // is keyed on the EVALUATION coordinate, not wallclock `time::now()`. A fact
    // whose valid interval has ended at the coordinate is terminal; a
    // future-dated retraction is NOT yet terminal at an earlier coordinate.

    /// Microseconds in one day (valid-time offset unit for these fixtures).
    const DAY_US: i64 = 86_400_000_000;

    fn ts(micros: i64) -> Timestamp {
        Timestamp::new_unchecked(micros, 0)
    }

    // Far-future transaction time: makes every recorded version (incl. the
    // retraction) visible, so only the valid-time coordinate governs the result.
    fn late_tt() -> Timestamp {
        Timestamp::new_unchecked(i64::MAX / 2, 0)
    }

    // T1a / T1b: a future-dated retraction is terminal ONLY once the evaluation
    // valid-time coordinate reaches the cutoff.
    #[test]
    fn t1a_t1b_future_retraction_terminal_only_after_cutoff() {
        let db = AletheiaDB::new().unwrap(); // weakest-link default
        let r = mk(&db, "Doc", Some(0.9), &[]);
        let r2 = mk(&db, "Doc", Some(0.8), &[]);
        let d = mk(&db, "Merge", None, &[r, r2]);
        let now = time::now().wallclock();
        let cutoff = ts(now + DAY_US); // retract effective +1 day
        if let EntityId::Node(nid) = r.entity {
            db.retract_node_with_provenance(nid, cutoff, Some(prov(0.9)))
                .unwrap();
        }

        // T1a: coordinate BEFORE the cutoff -> r still live -> not terminal.
        let cc_before = db
            .computed_confidence_as_of_bitemporal(d, ts(now), late_tt())
            .unwrap();
        approx(cc_before.computed, 0.8); // min(0.9 live, 0.8)
        assert!(
            !cc_before.has_retracted_inputs,
            "T1a: future retraction is not terminal before its cutoff"
        );

        // T1b: coordinate AT/AFTER the cutoff -> r retracted -> terminal.
        let cc_after = db
            .computed_confidence_as_of_bitemporal(d, ts(now + 2 * DAY_US), late_tt())
            .unwrap();
        approx(cc_after.computed, 0.0);
        assert!(
            cc_after.has_retracted_inputs,
            "T1b: retraction is terminal at/after its cutoff"
        );
    }

    // T1c: a bounded-but-currently-valid interval [t0, t1) evaluated at a
    // coordinate strictly before t1 is NOT terminal.
    #[test]
    fn t1c_bounded_currently_valid_not_terminal() {
        let db = AletheiaDB::new().unwrap();
        let now = time::now().wallclock();
        let past = ts(now - DAY_US);
        let (nid, vid) = db
            .create_node_with_options_and_lineage(
                "Doc",
                PropertyMap::new(),
                WriteRequestOptions::new()
                    .with_valid_from(past)
                    .with_provenance(prov(0.9)),
                &[],
            )
            .unwrap();
        let r = LineageRef::new(nid, vid);
        let r2 = mk(&db, "Doc", Some(0.8), &[]);
        let d = mk(&db, "Merge", None, &[r, r2]);
        // Interval [now-1d, now+1d); coordinate now is strictly < t1.
        db.retract_node_with_provenance(nid, ts(now + DAY_US), Some(prov(0.9)))
            .unwrap();
        let cc = db
            .computed_confidence_as_of_bitemporal(d, ts(now), late_tt())
            .unwrap();
        approx(cc.computed, 0.8);
        assert!(!cc.has_retracted_inputs);
    }

    // T1d: the exact bug the wallclock-now approximation had. A fact whose
    // interval has ENDED relative to wallclock now, but had NOT ended at an
    // earlier coordinate, is live at that earlier coordinate. Mirror: a fact
    // live now but retracted in the future is terminal at a future coordinate.
    #[test]
    fn t1d_as_of_valid_time_differs_from_wallclock_now() {
        let db = AletheiaDB::new().unwrap();
        let now = time::now().wallclock();
        let past_start = ts(now - 2 * DAY_US);
        let (nid, vid) = db
            .create_node_with_options_and_lineage(
                "Doc",
                PropertyMap::new(),
                WriteRequestOptions::new()
                    .with_valid_from(past_start)
                    .with_provenance(prov(0.9)),
                &[],
            )
            .unwrap();
        let r = LineageRef::new(nid, vid);
        let r2 = mk(&db, "Doc", Some(0.8), &[]);
        let d = mk(&db, "Merge", None, &[r, r2]);
        // Retract effective 1 day ago: interval becomes [now-2d, now-1d), which
        // has ended as of wallclock now.
        db.retract_node_with_provenance(nid, ts(now - DAY_US), Some(prov(0.9)))
            .unwrap();

        // At wallclock now r is terminal (unchanged legacy behavior).
        let cc_now = db.computed_confidence(d).unwrap();
        approx(cc_now.computed, 0.0);
        assert!(cc_now.has_retracted_inputs);

        // Evaluated 1.5 days ago the interval had NOT ended -> live (the fix).
        let earlier = ts(now - DAY_US - DAY_US / 2);
        let cc_earlier = db
            .computed_confidence_as_of_bitemporal(d, earlier, late_tt())
            .unwrap();
        approx(cc_earlier.computed, 0.8);
        assert!(
            !cc_earlier.has_retracted_inputs,
            "T1d: interval had not ended at the earlier coordinate"
        );

        // Mirror: a fact valid now but retracted at now+1d is terminal when
        // evaluated at now+2d, live when evaluated at now.
        let lr = mk(&db, "Doc", Some(0.7), &[]);
        let dd = mk(&db, "Merge", None, &[lr]);
        if let EntityId::Node(n) = lr.entity {
            db.retract_node_with_provenance(n, ts(now + DAY_US), Some(prov(0.7)))
                .unwrap();
        }
        let cc_future = db
            .computed_confidence_as_of_bitemporal(dd, ts(now + 2 * DAY_US), late_tt())
            .unwrap();
        approx(cc_future.computed, 0.0);
        assert!(
            cc_future.has_retracted_inputs,
            "T1d mirror: terminal at a future coordinate past the end"
        );
        let cc_live = db
            .computed_confidence_as_of_bitemporal(dd, ts(now), late_tt())
            .unwrap();
        approx(cc_live.computed, 0.7);
        assert!(!cc_live.has_retracted_inputs);
    }

    // T1e: back-compat. Omitting the valid-time coordinate reproduces prior
    // behavior exactly (valid_now == wallclock now) across all entry points.
    #[test]
    fn t1e_back_compat_no_valid_coordinate_matches_now() {
        let db = AletheiaDB::new().unwrap();
        let r = mk(&db, "Doc", Some(0.9), &[]);
        let d = mk(&db, "Merge", None, &[r]);
        let cc_plain = db.computed_confidence(d).unwrap();
        let cc_as_of = db.computed_confidence_as_of(d, time::now()).unwrap();
        let cc_bi = db
            .computed_confidence_as_of_bitemporal(d, time::now(), time::now())
            .unwrap();
        approx(cc_plain.computed, 0.9);
        approx(cc_plain.computed, cc_as_of.computed);
        approx(cc_plain.computed, cc_bi.computed);
        // Default TrustOptions (no valid coordinate) also equals now.
        let bd = db.trust_breakdown(d, &TrustOptions::new());
        approx(bd.confidence, cc_plain.computed);
    }

    // T1f: carve-out preserved WITH a valid-time coordinate supplied. start==end
    // is Absent; a version predating the as-of tt is Absent but NOT a retraction.
    #[test]
    fn t1f_carveout_preserved_with_valid_coordinate() {
        let past = ts(time::now().wallclock() - DAY_US);
        assert_eq!(
            classify_valid_interval(past, past, past),
            Some(ConfidenceSource::Absent),
            "start==end is an Absent tombstone at any coordinate"
        );

        let db = AletheiaDB::new().unwrap();
        let t_before = time::now(); // captured BEFORE the fact is recorded
        let r = mk(&db, "Doc", Some(0.9), &[]);
        let cc = db
            .computed_confidence_as_of_bitemporal(r, time::now(), t_before)
            .unwrap();
        approx(cc.computed, 0.0); // did not exist at tt
        assert!(
            !cc.has_retracted_inputs,
            "T1f: predates-tt is not a retraction even with a valid coordinate"
        );
    }

    // T1g: boundary — a coordinate exactly equal to the interval end is terminal
    // (half-open [start, end)).
    #[test]
    fn t1g_boundary_coordinate_equals_end_is_terminal() {
        let db = AletheiaDB::new().unwrap();
        let now = time::now().wallclock();
        let r = mk(&db, "Doc", Some(0.9), &[]);
        let r2 = mk(&db, "Doc", Some(0.8), &[]);
        let d = mk(&db, "Merge", None, &[r, r2]);
        let cutoff = ts(now + DAY_US);
        if let EntityId::Node(nid) = r.entity {
            db.retract_node_with_provenance(nid, cutoff, Some(prov(0.9)))
                .unwrap();
        }
        let cc = db
            .computed_confidence_as_of_bitemporal(d, cutoff, late_tt())
            .unwrap();
        approx(cc.computed, 0.0);
        assert!(
            cc.has_retracted_inputs,
            "T1g: coordinate == interval end is terminal (half-open)"
        );
    }

    // T1h: the valid-time coordinate flows through the recursive closure so a
    // deep input is judged terminal-or-not at the SAME coordinate.
    #[test]
    fn t1h_valid_coordinate_flows_through_recursive_closure() {
        let db = AletheiaDB::new().unwrap();
        let now = time::now().wallclock();
        let r = mk(&db, "Doc", Some(0.9), &[]); // future-retracted, 2 levels down
        let m = mk(&db, "Merge", None, &[r]); // intermediate pass-through
        let sibling = mk(&db, "Doc", Some(0.8), &[]);
        let top = mk(&db, "Merge", None, &[m, sibling]);
        let cutoff = ts(now + DAY_US);
        if let EntityId::Node(nid) = r.entity {
            db.retract_node_with_provenance(nid, cutoff, Some(prov(0.9)))
                .unwrap();
        }
        // Before cutoff: deep input r live -> not terminal.
        let cc_before = db
            .computed_confidence_as_of_bitemporal(top, ts(now), late_tt())
            .unwrap();
        approx(cc_before.computed, 0.8); // min(m=0.9, sibling=0.8)
        assert!(!cc_before.has_retracted_inputs);
        // After cutoff: deep input judged terminal at the SAME coordinate and
        // dominates up the chain.
        let cc_after = db
            .computed_confidence_as_of_bitemporal(top, ts(now + 2 * DAY_US), late_tt())
            .unwrap();
        approx(cc_after.computed, 0.0);
        assert!(
            cc_after.has_retracted_inputs,
            "T1h: deep retracted input dominates at the shared coordinate"
        );
    }

    // ---- D-1: deep chain is cycle/overflow-safe; memoization exercised ----

    #[test]
    fn d1_deep_chain_no_overflow() {
        let db = AletheiaDB::new().unwrap();
        let mut prev = mk(&db, "Doc", Some(0.9), &[]);
        for _ in 0..200 {
            prev = mk(&db, "L", None, &[prev]);
        }
        // Single-parent chain passes the value through; no panic/overflow.
        approx(db.computed_confidence(prev).unwrap().computed, 0.9);
    }

    // T-2 backstop: a chain deeper than SCALAR_MAX_DEPTH (1024) exercises the
    // scalar truncation / conservative-leaf backstop and flags `truncated`.
    #[test]
    fn scalar_depth_backstop_truncates_beyond_1024() {
        let db = AletheiaDB::new().unwrap();
        let mut prev = mk(&db, "Doc", Some(0.9), &[]);
        // Build a single-parent chain deeper than SCALAR_MAX_DEPTH so the
        // recursion hits the hard depth cap and marks the result truncated.
        for _ in 0..(SCALAR_MAX_DEPTH + 32) {
            prev = mk(&db, "L", None, &[prev]);
        }
        let cc = db.computed_confidence(prev).unwrap();
        assert!(
            cc.truncated,
            "a chain deeper than SCALAR_MAX_DEPTH must set truncated"
        );
    }

    // Lazy policy reflection: a per-label policy set AFTER the facts already
    // exist is honored on the NEXT computed-confidence read (nothing is stored).
    #[test]
    fn policy_change_after_facts_is_lazily_reflected() {
        let db = AletheiaDB::new().unwrap(); // default weakest-link
        let r1 = mk(&db, "Doc", Some(0.6), &[]);
        let r2 = mk(&db, "Doc", Some(0.6), &[]);
        let d = mk(&db, "Merge", None, &[r1, r2]);
        // Weakest-link now: min(0.6, 0.6) = 0.6.
        approx(db.computed_confidence(d).unwrap().computed, 0.6);
        // Change the "Merge" label policy to noisy-OR AFTER the facts exist.
        db.set_trust_policy_for_label(
            "Merge",
            TrustPolicy::noisy_or(MissingConfidencePolicy::Zero),
        )
        .unwrap();
        // The next read reflects the NEW policy: 1-(0.4)(0.4) = 0.84.
        approx(db.computed_confidence(d).unwrap().computed, 0.84);
    }

    // ---- A-1 / A-2 / A-3: bi-temporal honesty ----

    #[test]
    fn as_of_reactive_and_stable() {
        let db = AletheiaDB::new().unwrap();
        // Root at 0.9, derived D from it.
        let (r_nid, r_v_old) = db
            .create_node_with_options_and_lineage("Doc", PropertyMap::new(), opts(Some(0.9)), &[])
            .unwrap();
        let r_ref = LineageRef::new(r_nid, r_v_old);
        let d = mk(&db, "Merge", None, &[r_ref]);

        // A-1 anchor: D's own commit time — after D (and its lineage) exist, but
        // before the update. `resolve_ref` at this tt sees the old root version.
        let t_anchor = db
            .historical
            .read()
            .get_node_version(d.version)
            .unwrap()
            .commit_timestamp;

        // Supersede the root with new confidence 0.5.
        db.update_node_with_options_and_lineage(r_nid, PropertyMap::new(), opts(Some(0.5)), &[])
            .unwrap();

        // A-2 reactive-at-now: D reflects the NEW upstream confidence.
        approx(db.computed_confidence(d).unwrap().computed, 0.5);

        // A-1 as-of earlier: D reflects the confidence as recorded at t_anchor.
        approx(
            db.computed_confidence_as_of(d, t_anchor).unwrap().computed,
            0.9,
        );

        // A-3 no-op AS OF: at/after the latest tx == unscoped.
        let late = Timestamp::new_unchecked(i64::MAX / 2, 0);
        approx(
            db.computed_confidence_as_of(d, late).unwrap().computed,
            db.computed_confidence(d).unwrap().computed,
        );
    }

    // ---- error surface: dangling ref -> VersionNotFound ----

    #[test]
    fn dangling_ref_is_version_not_found() {
        let db = AletheiaDB::new().unwrap();
        let node = db.create_node("Doc", PropertyMap::new()).unwrap();
        let bogus = LineageRef::new(node, VersionId::new(999_999).unwrap());
        let err = db.computed_confidence(bogus).unwrap_err();
        assert!(
            matches!(err, Error::Storage(StorageError::VersionNotFound(_))),
            "expected VersionNotFound, got {err:?}"
        );
    }

    #[test]
    fn node_computed_confidence_resolves_current_ref() {
        let db = AletheiaDB::new().unwrap();
        let r = mk(&db, "Doc", Some(0.75), &[]);
        if let EntityId::Node(nid) = r.entity {
            let cc = db.node_computed_confidence(nid).unwrap();
            approx(cc.computed, 0.75);
        }
    }

    // ---- M4: explainable breakdown ----

    // AC3: the breakdown matches the hand-computed tree node by node.
    #[test]
    fn breakdown_matches_hand_computed_tree() {
        let db = AletheiaDB::new().unwrap(); // weakest-link default
        let r1 = mk(&db, "Doc", Some(0.9), &[]);
        let r2 = mk(&db, "Doc", Some(0.8), &[]);
        let r3 = mk(&db, "Doc", Some(0.3), &[]);
        let m1 = mk(&db, "Merge", None, &[r1, r2]); // 0.8
        let m2 = mk(&db, "Merge", None, &[r3]); // 0.3
        let top = mk(&db, "Merge", None, &[m1, m2]); // 0.3

        let bd = db.trust_breakdown(top, &TrustOptions::new());
        assert_eq!(bd.reference, top);
        approx(bd.confidence, 0.3);
        assert_eq!(bd.source, ConfidenceSource::Computed);
        assert_eq!(bd.combinator, Some(TrustCombinator::WeakestLink));
        assert_eq!(bd.status, FactStatus::Current);
        assert!(!bd.truncated);
        assert_eq!(bd.children.len(), 2);

        // child 0 == m1 (0.8) over two declared roots.
        let m1_bd = &bd.children[0];
        assert_eq!(m1_bd.reference, m1);
        approx(m1_bd.confidence, 0.8);
        assert_eq!(m1_bd.children.len(), 2);
        assert_eq!(m1_bd.children[0].reference, r1);
        approx(m1_bd.children[0].confidence, 0.9);
        assert_eq!(m1_bd.children[0].source, ConfidenceSource::Declared);
        assert_eq!(m1_bd.children[0].combinator, None);
        assert!(m1_bd.children[0].children.is_empty());
        approx(m1_bd.children[1].confidence, 0.8);

        // child 1 == m2 (0.3) over one declared root.
        let m2_bd = &bd.children[1];
        assert_eq!(m2_bd.reference, m2);
        approx(m2_bd.confidence, 0.3);
        assert_eq!(m2_bd.children.len(), 1);
        approx(m2_bd.children[0].confidence, 0.3);
        assert_eq!(m2_bd.children[0].source, ConfidenceSource::Declared);
    }

    // T-1: a depth-truncated breakdown keeps full-accuracy confidence values.
    #[test]
    fn breakdown_truncated_value_intact() {
        let db = AletheiaDB::new().unwrap();
        let r1 = mk(&db, "Doc", Some(0.9), &[]);
        let r2 = mk(&db, "Doc", Some(0.8), &[]);
        let r3 = mk(&db, "Doc", Some(0.3), &[]);
        let m1 = mk(&db, "Merge", None, &[r1, r2]);
        let m2 = mk(&db, "Merge", None, &[r3]);
        let top = mk(&db, "Merge", None, &[m1, m2]);

        let full = db.computed_confidence(top).unwrap().computed;
        // Depth cap of 1: children present but their subtrees are elided.
        let bd = db.trust_breakdown(top, &TrustOptions::new().with_max_depth(1));
        approx(bd.confidence, full); // NO over-trust despite truncation.
        assert!(bd.truncated);
        assert_eq!(bd.children.len(), 2);
        for child in &bd.children {
            assert!(child.truncated, "depth-capped child is truncated");
            assert!(child.children.is_empty(), "child subtree elided");
        }
        // The elided child still reports its own full-accuracy value.
        approx(bd.children[0].confidence, 0.8);
        approx(bd.children[1].confidence, 0.3);
    }

    // T-2: a node-budget-truncated breakdown is conservative (full value, fewer
    // serialized nodes).
    #[test]
    fn breakdown_node_budget_conservative() {
        let db = AletheiaDB::new().unwrap();
        let r1 = mk(&db, "Doc", Some(0.9), &[]);
        let r2 = mk(&db, "Doc", Some(0.8), &[]);
        let r3 = mk(&db, "Doc", Some(0.3), &[]);
        let m1 = mk(&db, "Merge", None, &[r1, r2]);
        let m2 = mk(&db, "Merge", None, &[r3]);
        let top = mk(&db, "Merge", None, &[m1, m2]);

        let full = db.computed_confidence(top).unwrap().computed;
        let bd = db.trust_breakdown(top, &TrustOptions::new().with_max_nodes(1));
        approx(bd.confidence, full);
        assert!(bd.truncated);
        // Only one child fit within the node budget.
        assert_eq!(bd.children.len(), 1);
        approx(bd.children[0].confidence, 0.8);
    }

    // Review-fix #2: the breakdown distinguishes Retracted from Absent.
    #[test]
    fn breakdown_retracted_vs_absent_distinct() {
        // Retracted upstream.
        let db = AletheiaDB::new().unwrap();
        let r = mk(&db, "Doc", Some(0.9), &[]);
        let d = mk(&db, "Merge", None, &[r]);
        if let EntityId::Node(nid) = r.entity {
            db.retract_node(nid, time::now()).unwrap();
        }
        let bd = db.trust_breakdown(d, &TrustOptions::new());
        assert_eq!(bd.children.len(), 1);
        assert_eq!(
            bd.children[0].source,
            ConfidenceSource::Retracted,
            "retraction is labelled Retracted"
        );
        assert_eq!(bd.children[0].status, FactStatus::Absent);

        // Deleted upstream.
        let db = AletheiaDB::new().unwrap();
        let r = mk(&db, "Doc", Some(0.9), &[]);
        let d = mk(&db, "Merge", None, &[r]);
        if let EntityId::Node(nid) = r.entity {
            db.write(|tx| tx.delete_node(nid)).unwrap();
        }
        let bd = db.trust_breakdown(d, &TrustOptions::new());
        assert_eq!(
            bd.children[0].source,
            ConfidenceSource::Absent,
            "deletion is labelled Absent, distinct from Retracted"
        );
    }

    // A leaf breakdown reports its declared confidence with no combinator.
    #[test]
    fn breakdown_leaf_is_declared() {
        let db = AletheiaDB::new().unwrap();
        let r = mk(&db, "Doc", Some(0.7), &[]);
        let bd = db.trust_breakdown(r, &TrustOptions::new());
        approx(bd.confidence, 0.7);
        assert_eq!(bd.source, ConfidenceSource::Declared);
        assert_eq!(bd.combinator, None);
        assert!(bd.children.is_empty());
        assert!(!bd.truncated);
    }

    // ---- M5: computed-confidence predicate (AC7) ----

    #[test]
    fn computed_confidence_filter_threshold() {
        let db = AletheiaDB::new().unwrap(); // weakest-link
        let r1 = mk(&db, "Doc", Some(0.9), &[]);
        let r2 = mk(&db, "Doc", Some(0.3), &[]);
        let d = mk(&db, "Merge", None, &[r1, r2]); // computed = 0.3

        assert!(
            db.computed_confidence_matches(d, &ComputedConfidenceFilter::at_least(0.2))
                .unwrap()
        );
        assert!(
            !db.computed_confidence_matches(d, &ComputedConfidenceFilter::at_least(0.5))
                .unwrap()
        );
        // Inactive filter matches (and validates existence).
        assert!(
            db.computed_confidence_matches(d, &ComputedConfidenceFilter::default())
                .unwrap()
        );
    }

    #[test]
    fn filter_by_computed_confidence_retains_order() {
        let db = AletheiaDB::new().unwrap();
        let a = mk(&db, "Doc", Some(0.9), &[]); // 0.9
        let low_root = mk(&db, "Doc", Some(0.1), &[]);
        let b = mk(&db, "Merge", None, &[low_root]); // 0.1
        let c = mk(&db, "Doc", Some(0.6), &[]); // 0.6

        let kept = db
            .filter_by_computed_confidence(&[a, b, c], &ComputedConfidenceFilter::at_least(0.5))
            .unwrap();
        assert_eq!(
            kept,
            vec![a, c],
            "0.1-confidence fact filtered out, order kept"
        );
    }

    #[test]
    fn passes_confidence_filters_ands_declared_and_computed() {
        use crate::core::provenance::ProvenanceFilter;
        let db = AletheiaDB::new().unwrap();
        // Derived fact: writer declared 0.95, but computed from a 0.2 root = 0.2.
        let root = mk(&db, "Doc", Some(0.2), &[]);
        let (nid, vid) = db
            .create_node_with_options_and_lineage(
                "Merge",
                PropertyMap::new(),
                WriteRequestOptions::new().with_provenance(prov(0.95)),
                &[root],
            )
            .unwrap();
        let d = LineageRef::new(nid, vid);

        // Declared filter (>= 0.9) passes; computed filter (>= 0.5) fails -> AND = false.
        let declared = ProvenanceFilter::validated(None, None, Some(0.9), false)
            .unwrap()
            .unwrap();
        assert!(
            !db.passes_confidence_filters(
                d,
                Some(&declared),
                &ComputedConfidenceFilter::at_least(0.5),
            )
            .unwrap(),
            "declared passes but computed fails -> filtered out"
        );

        // Both satisfiable: declared >= 0.9 and computed >= 0.1 -> AND = true.
        assert!(
            db.passes_confidence_filters(
                d,
                Some(&declared),
                &ComputedConfidenceFilter::at_least(0.1),
            )
            .unwrap()
        );

        // Declared filter that the fact fails (>= 0.99) -> AND = false regardless.
        let strict = ProvenanceFilter::validated(None, None, Some(0.99), false)
            .unwrap()
            .unwrap();
        assert!(
            !db.passes_confidence_filters(d, Some(&strict), &ComputedConfidenceFilter::default())
                .unwrap()
        );
    }

    #[cfg(feature = "semantic-retrieval-fusion")]
    #[test]
    fn computed_confidence_signal_supplies_value() {
        let db = AletheiaDB::new().unwrap();
        let r1 = mk(&db, "Doc", Some(0.9), &[]);
        let r2 = mk(&db, "Doc", Some(0.3), &[]);
        let d = mk(&db, "Merge", None, &[r1, r2]);
        assert_eq!(db.computed_confidence_signal(d), Some(0.3));
    }
}
