//! Contradiction genealogy — the bi-temporal life of competing claims (Issue #3352).
//!
//! When two facts disagree ("Acme's CEO is Alice" vs "Bob"), AletheiaDB uniquely
//! holds what is needed to adjudicate: **valid time** (when each was true),
//! **transaction time** (when we learned it), **provenance** (source +
//! confidence), and graph structure. A *contradiction genealogy* turns silent
//! knowledge-base rot into an inspectable artifact: given a conflict target, it
//! reconstructs every competing claim's bi-temporal life, attributes each to its
//! sources, locates the **divergence point**, and classifies **retroactive
//! corrections** vs **contemporaneous disagreement** — read-only and
//! deterministic.
//!
//! # Bi-temporal ground truth
//!
//! AletheiaDB linearizes versions on transaction time, but a superseded
//! version's valid interval is left **open and unsplit** (Issue #3504): updating
//! `ceo=Alice valid[t0,∞)` to `ceo=Bob valid[t1,∞)` yields two versions whose
//! valid intervals **overlap** on `[max(t0,t1),∞)` with differing values, while
//! their transaction intervals are disjoint. **That overlap is the structural
//! contradiction** — the record literally asserts both, because the Alice claim
//! was never retracted. The escape hatch is #3230 retraction: closing the prior
//! claim's `valid_to` removes the overlap, and a clean succession produces **no**
//! contradiction. So this feature flags value changes made *without* retracting
//! the superseded claim.
//!
//! # v1 contradiction definition (falsifiable)
//!
//! For a single entity+property, a **contradiction** exists iff there are ≥ 2
//! versions (claims) whose (a) property key is the same, (b) asserted values
//! differ ([`PropertyValue`] inequality — it is `PartialEq`, not `Eq`; the
//! Float/Vector NaN caveat is inherited), and (c) valid-time intervals overlap
//! (`[max(vf), min(vt))` non-empty; an open end is `TIMESTAMP_MAX`). A retracted
//! prior claim (closed `valid_to` not overlapping the successor's `valid_from`)
//! is **not** a contradiction.
//!
//! # Classification — temporal honesty
//!
//! Each conflicting pair (earlier-recorded `A`, later-recorded `B`) is labeled
//! [`DivergenceKind::RetroactiveCorrection`] when `B.valid_from ≤ A.valid_from`
//! (a later transaction reached back over a window `A` already covered) or
//! [`DivergenceKind::ContemporaneousDisagreement`] otherwise (`B` extends
//! forward but, because `A` was never retracted, both remain asserted over the
//! overlap). Each claim's `origin` reuses the belief-revision
//! [`RevisionClass`](super::belief_revision::RevisionClass) classifier, so the
//! genealogy stays consistent with the belief-revision audit (Issue #3362).
//!
//! # Divergence point
//!
//! For a conflicting pair the divergence point is `transaction_time =
//! B.transaction_from` (when the second claim entered) and `valid_time =
//! max(A.valid_from, B.valid_from)` (start of the valid overlap). The
//! genealogy's top-level `divergence_point` is the earliest such coordinate
//! across all conflicting pairs (min transaction_time, tie-break earliest
//! valid).
//!
//! # Determinism & read-only
//!
//! Pure over `(history, options)`; no writes, no mutation of history. Claims are
//! sorted by `(transaction_from, version_id)`, sources by `(source name, None
//! last)`, backed values by display — byte-identical across runs and independent
//! of input key order.

use crate::AletheiaDB;
use crate::core::error::{Error, QueryError, Result};
use crate::core::history::{EntityHistory, VersionInfo};
use crate::core::id::{EntityId, VersionId};
use crate::core::property::PropertyValue;
use crate::core::provenance::Provenance;
use crate::core::temporal::{TIMESTAMP_MAX, Timestamp, time};

use super::belief_revision::{self, RevisionClass};

/// Default number of contradictions returned by [`AletheiaDB::find_contradictions`]
/// when [`ContradictionScope::limit`] is left at its zero sentinel.
pub const DEFAULT_CONTRADICTION_LIMIT: usize = 100;

/// Maximum number of contradictions returnable from a single scan; larger
/// `limit` values are clamped down to this bound (mirrors the MCP list
/// contract).
pub const MAX_CONTRADICTION_LIMIT: usize = 1000;

// ============================================================================
// Public API — targets & options
// ============================================================================

/// What to reconstruct a genealogy for.
#[derive(Debug, Clone, PartialEq)]
pub enum ContradictionTarget {
    /// All competing claims about a single property of a single entity.
    EntityProperty {
        /// The node or edge whose history is analyzed.
        entity: EntityId,
        /// The property key whose competing values are analyzed.
        property: String,
    },
    /// An explicit set of competing claims, which may span multiple entities.
    Claims(Vec<ClaimRef>),
}

/// A version-pinned reference to one claim (a specific version of an entity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClaimRef {
    /// The node or edge the claim belongs to.
    pub entity: EntityId,
    /// The specific version that asserted the claim.
    pub version: VersionId,
}

/// Options controlling a genealogy reconstruction (builder-style `with_*`).
#[derive(Debug, Clone, Default)]
pub struct GenealogyOptions {
    /// Time-travel: only claims recorded **at or before** this transaction time
    /// are considered (later-recorded claims are excluded).
    pub as_of_transaction_time: Option<Timestamp>,
    /// Bound the number of [`CompetingClaim`]s in the output (elision).
    pub max_claims: Option<usize>,
    /// Bound the number of [`SourceSummary`]s in the output (elision).
    pub max_sources: Option<usize>,
}

impl GenealogyOptions {
    /// Empty options (identical to [`Default::default`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Time-travel the genealogy to a transaction-time coordinate.
    #[must_use]
    pub fn with_as_of_transaction_time(mut self, ts: Timestamp) -> Self {
        self.as_of_transaction_time = Some(ts);
        self
    }

    /// Bound the number of competing claims returned.
    #[must_use]
    pub fn with_max_claims(mut self, max: usize) -> Self {
        self.max_claims = Some(max);
        self
    }

    /// Bound the number of source summaries returned.
    #[must_use]
    pub fn with_max_sources(mut self, max: usize) -> Self {
        self.max_sources = Some(max);
        self
    }
}

/// Which entity kinds a [`find_contradictions`](AletheiaDB::find_contradictions)
/// scan enumerates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKindScope {
    /// Scan node history only.
    Nodes,
    /// Scan edge history only.
    Edges,
    /// Scan both nodes and edges.
    Both,
}

/// Scope for a database-wide contradiction scan.
#[derive(Debug, Clone)]
pub struct ContradictionScope {
    /// Which entity kinds to enumerate.
    pub entity_kind: EntityKindScope,
    /// Optional node-label / edge-type filter (matched against the entity's
    /// current label).
    pub label: Option<String>,
    /// Optional single property to analyze (otherwise every property that ever
    /// appeared on the entity is analyzed).
    pub property: Option<String>,
    /// Keep only contradictions whose divergence valid-time falls in `[start,
    /// end]`.
    pub valid_time_window: Option<(Timestamp, Timestamp)>,
    /// Keep only contradictions whose divergence transaction-time falls in
    /// `[start, end]`.
    pub transaction_time_window: Option<(Timestamp, Timestamp)>,
    /// Page size. `0` uses [`DEFAULT_CONTRADICTION_LIMIT`]; values above
    /// [`MAX_CONTRADICTION_LIMIT`] are clamped.
    pub limit: usize,
    /// Page offset.
    pub offset: usize,
}

impl Default for ContradictionScope {
    fn default() -> Self {
        Self {
            entity_kind: EntityKindScope::Both,
            label: None,
            property: None,
            valid_time_window: None,
            transaction_time_window: None,
            limit: DEFAULT_CONTRADICTION_LIMIT,
            offset: 0,
        }
    }
}

impl ContradictionScope {
    /// A default scope (both kinds, no filter, default page).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Restrict the scan to a single entity kind.
    #[must_use]
    pub fn with_entity_kind(mut self, kind: EntityKindScope) -> Self {
        self.entity_kind = kind;
        self
    }

    /// Restrict the scan to a single node label / edge type.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Restrict the scan to a single property.
    #[must_use]
    pub fn with_property(mut self, property: impl Into<String>) -> Self {
        self.property = Some(property.into());
        self
    }

    /// Set the page size and offset.
    #[must_use]
    pub fn with_page(mut self, limit: usize, offset: usize) -> Self {
        self.limit = limit;
        self.offset = offset;
        self
    }
}

// ============================================================================
// Public API — output types
// ============================================================================

/// One committed bi-temporal coordinate `(transaction_time, valid_time)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BiTemporalCoordinate {
    /// The transaction-time coordinate.
    pub transaction_time: Timestamp,
    /// The valid-time coordinate.
    pub valid_time: Timestamp,
}

/// The direction of a conflicting pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum DivergenceKind {
    /// A later transaction reached back to (re)assert over a window the earlier
    /// claim already covered from its start (`B.valid_from ≤ A.valid_from`).
    RetroactiveCorrection,
    /// The later claim extends forward but, because the earlier claim was never
    /// retracted, both remain asserted over the overlap
    /// (`B.valid_from > A.valid_from`).
    ContemporaneousDisagreement,
}

impl DivergenceKind {
    /// Lowercase `snake_case` token used in JSON / prose.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DivergenceKind::RetroactiveCorrection => "retroactive_correction",
            DivergenceKind::ContemporaneousDisagreement => "contemporaneous_disagreement",
        }
    }

    /// Human-readable phrase for the narrative.
    #[must_use]
    const fn phrase(self) -> &'static str {
        match self {
            DivergenceKind::RetroactiveCorrection => "retroactive correction",
            DivergenceKind::ContemporaneousDisagreement => "contemporaneous disagreement",
        }
    }
}

/// The write-time provenance recorded on one claim.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimProvenance {
    /// The source system/identifier, if any.
    pub source: Option<String>,
    /// The writer's confidence in `[0.0, 1.0]`, if any.
    pub confidence: Option<f64>,
    /// A free-text note, if any.
    pub note: Option<String>,
}

impl ClaimProvenance {
    fn from_provenance(p: &Provenance) -> Self {
        Self {
            source: p.source().map(str::to_string),
            confidence: p.confidence(),
            note: p.note().map(str::to_string),
        }
    }
}

/// One competing claim's full bi-temporal life.
#[derive(Debug, Clone, PartialEq)]
pub struct CompetingClaim {
    /// Version-pinned reference to this claim.
    pub claim: ClaimRef,
    /// Deterministic display of the asserted value.
    pub value_display: String,
    /// Start of the claim's valid interval.
    pub valid_from: Timestamp,
    /// End of the claim's valid interval (`None` = open-ended).
    pub valid_to: Option<Timestamp>,
    /// Transaction time at which the claim was recorded.
    pub transaction_from: Timestamp,
    /// End of the claim's transaction interval (`None` = still tx-current).
    pub transaction_to: Option<Timestamp>,
    /// Whether this exact version is the currently-visible one.
    pub is_current: bool,
    /// The claim's write-time provenance, if any.
    pub provenance: Option<ClaimProvenance>,
    /// The belief-revision classification of the version that made this claim.
    pub origin: RevisionClass,
    /// The version this claim superseded (its predecessor in tx order on the
    /// same entity), if any.
    pub supersedes: Option<VersionId>,
    /// The version that superseded this claim (its successor in tx order on the
    /// same entity), if any.
    pub superseded_by: Option<VersionId>,
}

/// One conflicting pair of claims.
#[derive(Debug, Clone, PartialEq)]
pub struct DivergencePair {
    /// The earlier-recorded claim.
    pub earlier: ClaimRef,
    /// The later-recorded claim.
    pub later: ClaimRef,
    /// The classification of this pair.
    pub kind: DivergenceKind,
    /// The bi-temporal coordinate at which the record first held both claims.
    pub coordinate: BiTemporalCoordinate,
    /// Start of the overlapping valid window.
    pub overlapping_valid_from: Timestamp,
    /// End of the overlapping valid window (`None` = open-ended).
    pub overlapping_valid_to: Option<Timestamp>,
}

/// Per-source aggregation over the competing claims.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceSummary {
    /// The source name (`None` is the unattributed bucket — never dropped).
    pub source: Option<String>,
    /// Distinct value displays this source backs (sorted).
    pub backs_values: Vec<String>,
    /// Number of competing claims from this source.
    pub claim_count: usize,
    /// Minimum confidence recorded by this source, if any.
    pub min_confidence: Option<f64>,
    /// Maximum confidence recorded by this source, if any.
    pub max_confidence: Option<f64>,
    /// Confidence of this source's most-recently-recorded claim, if any.
    pub latest_confidence: Option<f64>,
    /// The most recent transaction time this source asserted a claim.
    pub most_recent_assertion: Timestamp,
}

/// A full contradiction genealogy.
#[derive(Debug, Clone, PartialEq)]
pub struct ContradictionGenealogy {
    /// The analyzed entity (`None` when the target spanned multiple entities).
    pub entity: Option<EntityId>,
    /// The analyzed property (`None` when unresolved).
    pub property: Option<String>,
    /// The competing claims, in `(transaction_from, version_id)` order.
    pub claims: Vec<CompetingClaim>,
    /// The conflicting pairs.
    pub pairs: Vec<DivergencePair>,
    /// The earliest divergence coordinate across all pairs (`None` = no
    /// contradiction).
    pub divergence_point: Option<BiTemporalCoordinate>,
    /// Per-source aggregation.
    pub sources: Vec<SourceSummary>,
    /// Deterministic prose summary.
    pub narrative: String,
    /// `true` when `max_claims`/`max_sources` elided output.
    pub truncated: bool,
}

impl ContradictionGenealogy {
    /// Whether at least one conflicting pair was found.
    #[must_use]
    pub fn has_contradiction(&self) -> bool {
        !self.pairs.is_empty()
    }
}

/// One contradiction found by a scan (one per `(entity, property)` group).
#[derive(Debug, Clone, PartialEq)]
pub struct ContradictionSummary {
    /// The entity holding the contradiction.
    pub entity: EntityId,
    /// The property in conflict.
    pub property: String,
    /// Number of competing claims.
    pub claim_count: usize,
    /// The earliest divergence coordinate.
    pub divergence_point: BiTemporalCoordinate,
    /// The dominant classification across the contradiction's pairs.
    pub classification: DivergenceKind,
}

/// The result of a database-wide contradiction scan.
#[derive(Debug, Clone, PartialEq)]
pub struct ContradictionScan {
    /// The contradictions on this page, sorted by `(entity, property)`.
    pub contradictions: Vec<ContradictionSummary>,
    /// Number of candidate entities scanned.
    pub scanned_entities: usize,
    /// `true` when the candidate set was capped (mirrors `max_schema_as_of_entities`).
    pub sampled: bool,
    /// `true` when more contradictions exist beyond this page.
    pub has_more: bool,
    /// The offset to pass for the next page, if `has_more`.
    pub next_offset: Option<usize>,
}

// ============================================================================
// Engine
// ============================================================================

/// Read-only engine that reconstructs contradiction genealogies.
///
/// Mirrors [`BeliefRevisions`](super::belief_revision::BeliefRevisions):
/// borrow the database, read history, analyze, return. No writes.
pub struct ContradictionGenealogyEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> ContradictionGenealogyEngine<'a> {
    /// Create a new engine over `db`.
    #[must_use]
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    fn fetch_history(&self, entity: EntityId) -> Result<EntityHistory> {
        match entity {
            EntityId::Node(id) => self.db.get_node_history(id),
            EntityId::Edge(id) => self.db.get_edge_history(id),
        }
    }

    /// Reconstruct a genealogy for `target` under `options`.
    ///
    /// # Errors
    ///
    /// - `NOT_FOUND` for an unknown entity or a missing referenced version.
    /// - `INVALID_ARGUMENT` for an empty `Claims` set or a property the entity
    ///   never had.
    pub fn genealogy(
        &self,
        target: ContradictionTarget,
        options: &GenealogyOptions,
    ) -> Result<ContradictionGenealogy> {
        match target {
            ContradictionTarget::EntityProperty { entity, property } => {
                let history = self.fetch_history(entity)?;
                if !history_contains_key(&history, &property) {
                    return Err(invalid_argument(
                        "property",
                        format!("entity {entity} never had property '{property}'"),
                    ));
                }
                Ok(genealogy_entity_property(
                    entity, &history, &property, options,
                ))
            }
            ContradictionTarget::Claims(refs) => self.genealogy_from_claims(refs, options),
        }
    }

    fn genealogy_from_claims(
        &self,
        refs: Vec<ClaimRef>,
        options: &GenealogyOptions,
    ) -> Result<ContradictionGenealogy> {
        if refs.is_empty() {
            return Err(invalid_argument("claims", "claims set must not be empty"));
        }

        // Resolve every referenced version, carrying its origin class.
        let mut resolved: Vec<(ClaimRef, VersionInfo, RevisionClass)> = Vec::with_capacity(refs.len());
        for r in &refs {
            let history = self.fetch_history(r.entity)?;
            let classes = belief_revision::classify_history(&history);
            let idx = history
                .versions
                .iter()
                .position(|v| v.version_id == r.version)
                .ok_or_else(|| version_not_found(r.entity, r.version))?;
            resolved.push((*r, history.versions[idx].clone(), classes[idx]));
        }

        // Choose the comparison property: the key present in the most referenced
        // versions, tie-broken lexicographically. Deterministic.
        let Some(property) = choose_comparison_property(&resolved) else {
            return Err(invalid_argument(
                "claims",
                "referenced versions share no comparable property",
            ));
        };

        let claims: Vec<ClaimCtx> = resolved
            .iter()
            .filter_map(|(r, v, cls)| {
                v.properties
                    .get(&property)
                    .map(|val| ClaimCtx::new(r.entity, v, val.clone(), *cls))
            })
            .collect();

        // Entity is Some iff every claim is on the same entity.
        let entity = single_entity(&claims);
        Ok(build_genealogy(entity, Some(property), claims, options))
    }

    /// Scan the database for contradictions under `scope`.
    pub fn scan(&self, scope: &ContradictionScope) -> Result<ContradictionScan> {
        let (candidates, sampled) = self.candidates(scope);
        let scanned_entities = candidates.len();

        let mut summaries: Vec<ContradictionSummary> = Vec::new();
        for entity in candidates {
            let Ok(history) = self.fetch_history(entity) else {
                continue;
            };
            if !label_matches(&history, scope.label.as_deref()) {
                continue;
            }
            let properties = scan_properties(&history, scope.property.as_deref());
            for property in properties {
                let g = genealogy_entity_property(
                    entity,
                    &history,
                    &property,
                    &GenealogyOptions::default(),
                );
                let Some(divergence_point) = g.divergence_point else {
                    continue;
                };
                if !window_contains(scope.valid_time_window, divergence_point.valid_time)
                    || !window_contains(
                        scope.transaction_time_window,
                        divergence_point.transaction_time,
                    )
                {
                    continue;
                }
                summaries.push(ContradictionSummary {
                    entity,
                    property,
                    claim_count: g.claims.len(),
                    divergence_point,
                    classification: dominant_kind(&g.pairs),
                });
            }
        }

        // Deterministic order, then paginate.
        summaries.sort_by(|a, b| {
            a.entity
                .cmp(&b.entity)
                .then_with(|| a.property.cmp(&b.property))
        });

        let limit = resolve_scan_limit(scope.limit);
        let total = summaries.len();
        let start = scope.offset.min(total);
        let end = start.saturating_add(limit).min(total);
        let page: Vec<ContradictionSummary> = summaries[start..end].to_vec();
        let has_more = end < total;
        let next_offset = if has_more { Some(end) } else { None };

        Ok(ContradictionScan {
            contradictions: page,
            scanned_entities,
            sampled,
            has_more,
            next_offset,
        })
    }

    /// Enumerate candidate entities for a scan, capped at
    /// `max_schema_as_of_entities` (mirrors #3236 / the AS-OF node finder).
    fn candidates(&self, scope: &ContradictionScope) -> (Vec<EntityId>, bool) {
        let historical = self.db.historical.read();
        let cap = historical.max_schema_as_of_entities();
        let mut sampled = false;
        let mut out: Vec<EntityId> = Vec::new();

        if matches!(scope.entity_kind, EntityKindScope::Nodes | EntityKindScope::Both) {
            let mut ids = historical.versioned_node_ids();
            sampled |= crate::db::schema::cap_ids(&mut ids, cap);
            ids.sort_unstable();
            out.extend(ids.into_iter().map(EntityId::Node));
        }
        if matches!(scope.entity_kind, EntityKindScope::Edges | EntityKindScope::Both) {
            let mut ids = historical.versioned_edge_ids();
            sampled |= crate::db::schema::cap_ids(&mut ids, cap);
            ids.sort_unstable();
            out.extend(ids.into_iter().map(EntityId::Edge));
        }
        drop(historical);
        (out, sampled)
    }
}

// ============================================================================
// Convenience methods on AletheiaDB
// ============================================================================

impl AletheiaDB {
    /// Reconstruct a contradiction genealogy — the bi-temporal life of every
    /// competing claim behind a value conflict (Issue #3352).
    ///
    /// # Errors
    ///
    /// - `NOT_FOUND` for an unknown entity or a missing referenced version.
    /// - `INVALID_ARGUMENT` for an empty `Claims` set or an unknown property.
    pub fn contradiction_genealogy(
        &self,
        target: ContradictionTarget,
        options: &GenealogyOptions,
    ) -> Result<ContradictionGenealogy> {
        ContradictionGenealogyEngine::new(self).genealogy(target, options)
    }

    /// Scan the database for `(entity, property)` contradictions (Issue #3352).
    ///
    /// # Errors
    ///
    /// Returns an error only if candidate enumeration fails; per-entity history
    /// read failures are skipped, not surfaced.
    pub fn find_contradictions(&self, scope: &ContradictionScope) -> Result<ContradictionScan> {
        ContradictionGenealogyEngine::new(self).scan(scope)
    }
}

// ============================================================================
// Internal: claim context & pure detection
// ============================================================================

/// A fully-materialized competing claim, decoupled from storage iteration order.
#[derive(Debug, Clone)]
struct ClaimCtx {
    entity: EntityId,
    version_id: VersionId,
    value: PropertyValue,
    value_display: String,
    valid_from: Timestamp,
    valid_to: Option<Timestamp>,
    transaction_from: Timestamp,
    transaction_to: Option<Timestamp>,
    is_current: bool,
    provenance: Option<Provenance>,
    origin: RevisionClass,
}

impl ClaimCtx {
    fn new(entity: EntityId, version: &VersionInfo, value: PropertyValue, origin: RevisionClass) -> Self {
        let valid = version.temporal.valid_time();
        let tx = version.temporal.transaction_time();
        let value_display = value.to_string();
        ClaimCtx {
            entity,
            version_id: version.version_id,
            value,
            value_display,
            valid_from: valid.start(),
            valid_to: if valid.is_current() {
                None
            } else {
                Some(valid.end())
            },
            transaction_from: tx.start(),
            transaction_to: if tx.is_current() {
                None
            } else {
                Some(tx.end())
            },
            is_current: tx.is_current() && valid.contains(time::now()),
            provenance: version.provenance.clone(),
            origin,
        }
    }

    fn claim_ref(&self) -> ClaimRef {
        ClaimRef {
            entity: self.entity,
            version: self.version_id,
        }
    }

    fn valid_end(&self) -> Timestamp {
        self.valid_to.unwrap_or(TIMESTAMP_MAX)
    }
}

/// Whether `key` appears in the properties of any version in `history`.
fn history_contains_key(history: &EntityHistory, key: &str) -> bool {
    history.versions.iter().any(|v| v.properties.get(key).is_some())
}

/// Build a genealogy for a single entity+property from its full history.
///
/// Pure over `(history, options)`. Callers must have already validated that the
/// property exists somewhere in `history`.
fn genealogy_entity_property(
    entity: EntityId,
    history: &EntityHistory,
    property: &str,
    options: &GenealogyOptions,
) -> ContradictionGenealogy {
    let classes = belief_revision::classify_history(history);
    let as_of = options.as_of_transaction_time;
    let claims: Vec<ClaimCtx> = history
        .versions
        .iter()
        .enumerate()
        .filter(|(_, v)| as_of.is_none_or(|t| v.temporal.transaction_time().start() <= t))
        .filter_map(|(i, v)| {
            v.properties
                .get(property)
                .map(|val| ClaimCtx::new(entity, v, val.clone(), classes[i]))
        })
        .collect();
    build_genealogy(Some(entity), Some(property.to_string()), claims, options)
}

/// The overlapping valid window of two claims, if any (`[max(vf), min(vt))`).
fn valid_overlap(a: &ClaimCtx, b: &ClaimCtx) -> Option<(Timestamp, Option<Timestamp>)> {
    let start = a.valid_from.max(b.valid_from);
    let end = a.valid_end().min(b.valid_end());
    if start < end {
        let end_opt = if end == TIMESTAMP_MAX { None } else { Some(end) };
        Some((start, end_opt))
    } else {
        None
    }
}

/// Core detector + assembler. Pure over `(claims, options)`.
fn build_genealogy(
    entity: Option<EntityId>,
    property: Option<String>,
    mut claims: Vec<ClaimCtx>,
    options: &GenealogyOptions,
) -> ContradictionGenealogy {
    // Deterministic claim order: (transaction_from, version_id).
    claims.sort_by(|a, b| {
        a.transaction_from
            .cmp(&b.transaction_from)
            .then_with(|| a.version_id.cmp(&b.version_id))
    });

    // Detect conflicting pairs (i earlier-tx, j later-tx).
    let mut pairs: Vec<DivergencePair> = Vec::new();
    let mut participating: Vec<usize> = Vec::new();
    for i in 0..claims.len() {
        for j in (i + 1)..claims.len() {
            let (a, b) = (&claims[i], &claims[j]);
            if a.value == b.value {
                continue;
            }
            let Some((ov_from, ov_to)) = valid_overlap(a, b) else {
                continue;
            };
            let kind = if b.valid_from <= a.valid_from {
                DivergenceKind::RetroactiveCorrection
            } else {
                DivergenceKind::ContemporaneousDisagreement
            };
            let coordinate = BiTemporalCoordinate {
                transaction_time: b.transaction_from,
                valid_time: a.valid_from.max(b.valid_from),
            };
            pairs.push(DivergencePair {
                earlier: a.claim_ref(),
                later: b.claim_ref(),
                kind,
                coordinate,
                overlapping_valid_from: ov_from,
                overlapping_valid_to: ov_to,
            });
            participating.push(i);
            participating.push(j);
        }
    }

    participating.sort_unstable();
    participating.dedup();

    // Supersession chain among participating claims, per entity, in tx order.
    let competing: Vec<CompetingClaim> = participating
        .iter()
        .map(|&idx| {
            let c = &claims[idx];
            let supersedes = participating
                .iter()
                .rev()
                .map(|&k| &claims[k])
                .find(|o| o.entity == c.entity && o.version_id < c.version_id)
                .map(|o| o.version_id);
            let superseded_by = participating
                .iter()
                .map(|&k| &claims[k])
                .find(|o| o.entity == c.entity && o.version_id > c.version_id)
                .map(|o| o.version_id);
            CompetingClaim {
                claim: c.claim_ref(),
                value_display: c.value_display.clone(),
                valid_from: c.valid_from,
                valid_to: c.valid_to,
                transaction_from: c.transaction_from,
                transaction_to: c.transaction_to,
                is_current: c.is_current,
                provenance: c.provenance.as_ref().map(ClaimProvenance::from_provenance),
                origin: c.origin,
                supersedes,
                superseded_by,
            }
        })
        .collect();

    let divergence_point = pairs
        .iter()
        .map(|p| p.coordinate)
        .min_by(|a, b| {
            a.transaction_time
                .cmp(&b.transaction_time)
                .then_with(|| a.valid_time.cmp(&b.valid_time))
        });

    let participating_ctx: Vec<&ClaimCtx> = participating.iter().map(|&idx| &claims[idx]).collect();
    let sources = aggregate_sources(&participating_ctx);

    let narrative = build_narrative(
        entity,
        property.as_deref(),
        &competing,
        divergence_point,
        &pairs,
    );

    // Elision.
    let mut truncated = false;
    let mut claims_out = competing;
    if let Some(max) = options.max_claims
        && claims_out.len() > max
    {
        claims_out.truncate(max);
        truncated = true;
    }
    let mut sources_out = sources;
    if let Some(max) = options.max_sources
        && sources_out.len() > max
    {
        sources_out.truncate(max);
        truncated = true;
    }

    ContradictionGenealogy {
        entity,
        property,
        claims: claims_out,
        pairs,
        divergence_point,
        sources: sources_out,
        narrative,
        truncated,
    }
}

/// Aggregate competing claims by provenance source (`None` bucket last).
fn aggregate_sources(claims: &[&ClaimCtx]) -> Vec<SourceSummary> {
    // Distinct source keys, ordered: Some(name) ascending, None last.
    let mut keys: Vec<Option<String>> = Vec::new();
    for c in claims {
        let src = c.provenance.as_ref().and_then(|p| p.source().map(str::to_string));
        if !keys.contains(&src) {
            keys.push(src);
        }
    }
    keys.sort_by(|a, b| match (a, b) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    keys.into_iter()
        .map(|key| {
            let members: Vec<&&ClaimCtx> = claims
                .iter()
                .filter(|c| {
                    c.provenance
                        .as_ref()
                        .and_then(|p| p.source().map(str::to_string))
                        == key
                })
                .collect();

            let mut backs_values: Vec<String> =
                members.iter().map(|c| c.value_display.clone()).collect();
            backs_values.sort();
            backs_values.dedup();

            let confidences: Vec<f64> = members
                .iter()
                .filter_map(|c| c.provenance.as_ref().and_then(Provenance::confidence))
                .collect();
            let min_confidence = confidences
                .iter()
                .copied()
                .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let max_confidence = confidences
                .iter()
                .copied()
                .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

            // Latest = highest transaction_from member's own confidence.
            let latest_member = members
                .iter()
                .max_by(|a, b| {
                    a.transaction_from
                        .cmp(&b.transaction_from)
                        .then_with(|| a.version_id.cmp(&b.version_id))
                })
                .expect("source bucket is non-empty");
            let latest_confidence = latest_member
                .provenance
                .as_ref()
                .and_then(Provenance::confidence);
            let most_recent_assertion = members
                .iter()
                .map(|c| c.transaction_from)
                .max()
                .expect("source bucket is non-empty");

            SourceSummary {
                source: key,
                backs_values,
                claim_count: members.len(),
                min_confidence,
                max_confidence,
                latest_confidence,
                most_recent_assertion,
            }
        })
        .collect()
}

/// The dominant classification across a contradiction's pairs (tie →
/// retroactive-correction, deterministic).
fn dominant_kind(pairs: &[DivergencePair]) -> DivergenceKind {
    let retro = pairs
        .iter()
        .filter(|p| p.kind == DivergenceKind::RetroactiveCorrection)
        .count();
    let contemp = pairs.len() - retro;
    if retro >= contemp {
        DivergenceKind::RetroactiveCorrection
    } else {
        DivergenceKind::ContemporaneousDisagreement
    }
}

/// Deterministic prose summary naming the property, entity, competing values
/// (with sources/confidence), the divergence coordinate, and the dominant
/// classification.
fn build_narrative(
    entity: Option<EntityId>,
    property: Option<&str>,
    claims: &[CompetingClaim],
    divergence: Option<BiTemporalCoordinate>,
    pairs: &[DivergencePair],
) -> String {
    let prop = property.unwrap_or("value");
    let on = match entity {
        Some(e) => e.to_string(),
        None => "multiple entities".to_string(),
    };
    if pairs.is_empty() {
        return format!("No contradiction found for `{prop}` on {on}.");
    }
    let claim_descs: Vec<String> = claims
        .iter()
        .map(|c| {
            let src = c
                .provenance
                .as_ref()
                .and_then(|p| p.source.clone())
                .unwrap_or_else(|| "unattributed".to_string());
            let conf = c
                .provenance
                .as_ref()
                .and_then(|p| p.confidence)
                .map(|v| format!(", conf {v}"))
                .unwrap_or_default();
            format!("'{}' (source {src}{conf})", c.value_display)
        })
        .collect();
    let coord = divergence.map_or_else(String::new, |d| {
        format!(
            " at valid {}, recorded {}",
            time::to_iso8601(d.valid_time),
            time::to_iso8601(d.transaction_time)
        )
    });
    format!(
        "{} competing claims for `{prop}` on {on}: {}{coord} — {}.",
        claims.len(),
        claim_descs.join(" vs "),
        dominant_kind(pairs).phrase()
    )
}

// ============================================================================
// Internal: scan helpers
// ============================================================================

/// Whether the entity's current label matches an optional filter.
fn label_matches(history: &EntityHistory, label: Option<&str>) -> bool {
    match label {
        None => true,
        Some(want) => history
            .current_version()
            .is_some_and(|v| v.label == want),
    }
}

/// The set of properties to analyze for a scan (a single one, or every property
/// ever present, sorted for determinism).
fn scan_properties(history: &EntityHistory, property: Option<&str>) -> Vec<String> {
    match property {
        Some(p) => vec![p.to_string()],
        None => {
            let mut keys: Vec<String> = Vec::new();
            for v in &history.versions {
                for (k, _) in v.properties.iter() {
                    let ks = k.to_string();
                    if !keys.contains(&ks) {
                        keys.push(ks);
                    }
                }
            }
            keys.sort();
            keys
        }
    }
}

/// Whether an optional inclusive window contains `ts`.
fn window_contains(window: Option<(Timestamp, Timestamp)>, ts: Timestamp) -> bool {
    match window {
        None => true,
        Some((start, end)) => ts >= start && ts <= end,
    }
}

/// The comparison property for a `Claims` target: the key present in the most
/// referenced versions, tie-broken lexicographically.
fn choose_comparison_property(
    resolved: &[(ClaimRef, VersionInfo, RevisionClass)],
) -> Option<String> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (_, v, _) in resolved {
        for (k, _) in v.properties.iter() {
            *counts.entry(k.to_string()).or_insert(0) += 1;
        }
    }
    // BTreeMap iterates keys ascending; pick max count, first key wins ties.
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1))
        .map(|(k, _)| k)
}

/// `Some(entity)` iff every claim is on the same entity.
fn single_entity(claims: &[ClaimCtx]) -> Option<EntityId> {
    let mut iter = claims.iter();
    let first = iter.next()?.entity;
    if iter.all(|c| c.entity == first) {
        Some(first)
    } else {
        None
    }
}

/// Resolve a scan page size (0 → default, clamp to max).
fn resolve_scan_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_CONTRADICTION_LIMIT
    } else {
        limit.min(MAX_CONTRADICTION_LIMIT)
    }
}

// ============================================================================
// Errors
// ============================================================================

/// Build an `INVALID_ARGUMENT`-mapped error.
fn invalid_argument(parameter: &str, reason: impl Into<String>) -> Error {
    Error::Query(QueryError::InvalidParameter {
        parameter: parameter.to_string(),
        reason: reason.into(),
    })
}

/// Build a `NOT_FOUND`-mapped error for a missing referenced version.
fn version_not_found(_entity: EntityId, version: VersionId) -> Error {
    Error::Storage(crate::core::error::StorageError::VersionNotFound(version))
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

    fn node_entity(n: u64) -> EntityId {
        EntityId::Node(NodeId::new(n).unwrap())
    }

    fn prov(source: Option<&str>, confidence: Option<f64>) -> Provenance {
        Provenance::from_parts(source.map(String::from), confidence, None, None, None).unwrap()
    }

    fn one_prop(key: &str, val: impl Into<PropertyValue>) -> PropertyMap {
        PropertyMapBuilder::new().insert(key, val).build()
    }

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
            label: "Company".to_string(),
            provenance,
        }
    }

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
            label: "Company".to_string(),
            provenance,
        }
    }

    fn history(versions: Vec<VersionInfo>) -> EntityHistory {
        EntityHistory { versions }
    }

    fn analyze(h: &EntityHistory, property: &str) -> ContradictionGenealogy {
        genealogy_entity_property(node_entity(42), h, property, &GenealogyOptions::default())
    }

    // ---- case 1: no contradiction -----------------------------------------

    #[test]
    fn single_version_no_contradiction() {
        let h = history(vec![open_version(
            1,
            1000,
            100,
            one_prop("ceo", "Alice"),
            None,
        )]);
        let g = analyze(&h, "ceo");
        assert!(!g.has_contradiction());
        assert!(g.claims.is_empty());
        assert!(g.pairs.is_empty());
        assert!(g.divergence_point.is_none());
        assert!(!g.narrative.is_empty(), "narrative must never be empty");
    }

    #[test]
    fn retracted_prior_non_overlapping_not_flagged() {
        // v1 valid [1000, 2000) retracted; v2 valid [2000, ∞) — no overlap.
        let h = history(vec![
            closed_version(1, 1000, 2000, 100, one_prop("ceo", "Alice"), None),
            open_version(2, 2000, 200, one_prop("ceo", "Bob"), None),
        ]);
        let g = analyze(&h, "ceo");
        assert!(!g.has_contradiction(), "clean succession is not a contradiction");
    }

    // ---- case 2: backdated correction => retroactive -----------------------

    #[test]
    fn backdated_correction_is_retroactive() {
        // v1 valid_from 2000; v2 backdates to 1000 with a different value,
        // overlapping [2000, ∞).
        let h = history(vec![
            open_version(1, 2000, 100, one_prop("ceo", "Alice"), None),
            open_version(2, 1000, 200, one_prop("ceo", "Bob"), None),
        ]);
        let g = analyze(&h, "ceo");
        assert!(g.has_contradiction());
        assert_eq!(g.pairs.len(), 1);
        assert_eq!(g.pairs[0].kind, DivergenceKind::RetroactiveCorrection);
        let d = g.divergence_point.unwrap();
        // valid = max(2000, 1000) = 2000; tx = later.transaction_from = 200.
        assert_eq!(d.valid_time, ts(2000));
        assert_eq!(d.transaction_time, ts(200));
    }

    // ---- case 3: forward unretracted => contemporaneous --------------------

    #[test]
    fn forward_unretracted_is_contemporaneous() {
        // v1 valid_from 1000 (open); v2 valid_from 2000 (open), different value.
        // Both remain asserted over [2000, ∞).
        let h = history(vec![
            open_version(1, 1000, 100, one_prop("ceo", "Alice"), None),
            open_version(2, 2000, 200, one_prop("ceo", "Bob"), None),
        ]);
        let g = analyze(&h, "ceo");
        assert!(g.has_contradiction());
        assert_eq!(g.pairs[0].kind, DivergenceKind::ContemporaneousDisagreement);
        let d = g.divergence_point.unwrap();
        assert_eq!(d.valid_time, ts(2000));
        assert_eq!(d.transaction_time, ts(200));
    }

    // ---- case 5: three-way chain ------------------------------------------

    #[test]
    fn three_way_chain() {
        // Three overlapping open claims with distinct values.
        let h = history(vec![
            open_version(1, 1000, 100, one_prop("ceo", "Alice"), None),
            open_version(2, 1000, 200, one_prop("ceo", "Bob"), None),
            open_version(3, 1000, 300, one_prop("ceo", "Carol"), None),
        ]);
        let g = analyze(&h, "ceo");
        assert!(g.has_contradiction());
        assert_eq!(g.claims.len(), 3, "3 competing claims");
        assert_eq!(g.pairs.len(), 3, "AB, AC, BC");
        // Earliest divergence is the second claim entering at tx=200.
        assert_eq!(g.divergence_point.unwrap().transaction_time, ts(200));
        // The middle claim supersedes v1 and is superseded by v3.
        let mid = g.claims.iter().find(|c| c.claim.version == vid(2)).unwrap();
        assert_eq!(mid.supersedes, Some(vid(1)));
        assert_eq!(mid.superseded_by, Some(vid(3)));
    }

    // ---- case 6: reaffirmation (equal value) not flagged -------------------

    #[test]
    fn equal_value_reassertion_not_flagged() {
        let h = history(vec![
            open_version(1, 1000, 100, one_prop("ceo", "Alice"), Some(prov(Some("a"), None))),
            open_version(2, 1000, 200, one_prop("ceo", "Alice"), Some(prov(Some("b"), None))),
        ]);
        let g = analyze(&h, "ceo");
        assert!(!g.has_contradiction(), "same value is not a contradiction");
    }

    // ---- case 7: unrelated property untouched ------------------------------

    #[test]
    fn unrelated_property_not_flagged() {
        // `ceo` conflicts, but `sector` is stable — analyzing `sector` is clean.
        let h = history(vec![
            open_version(
                1,
                1000,
                100,
                PropertyMapBuilder::new()
                    .insert("ceo", "Alice")
                    .insert("sector", "tech")
                    .build(),
                None,
            ),
            open_version(
                2,
                1000,
                200,
                PropertyMapBuilder::new()
                    .insert("ceo", "Bob")
                    .insert("sector", "tech")
                    .build(),
                None,
            ),
        ]);
        assert!(analyze(&h, "sector").pairs.is_empty());
        assert!(!analyze(&h, "ceo").pairs.is_empty());
    }

    // ---- case 8: missing provenance => unattributed bucket -----------------

    #[test]
    fn missing_provenance_unattributed_bucket() {
        let h = history(vec![
            open_version(1, 1000, 100, one_prop("ceo", "Alice"), Some(prov(Some("sec"), Some(0.9)))),
            open_version(2, 1000, 200, one_prop("ceo", "Bob"), None),
        ]);
        let g = analyze(&h, "ceo");
        assert!(g.has_contradiction());
        // Two buckets: "sec" and the unattributed (None) bucket, None last.
        assert_eq!(g.sources.len(), 2);
        assert_eq!(g.sources[0].source.as_deref(), Some("sec"));
        assert_eq!(g.sources[1].source, None, "unattributed bucket is present and last");
    }

    // ---- case 9: multi-source summary --------------------------------------

    #[test]
    fn multi_source_summary() {
        let h = history(vec![
            open_version(1, 1000, 100, one_prop("ceo", "Alice"), Some(prov(Some("sec"), Some(0.9)))),
            open_version(2, 1000, 200, one_prop("ceo", "Bob"), Some(prov(Some("press"), Some(0.5)))),
            open_version(3, 1000, 300, one_prop("ceo", "Bob"), Some(prov(Some("press"), Some(0.7)))),
        ]);
        let g = analyze(&h, "ceo");
        let press = g.sources.iter().find(|s| s.source.as_deref() == Some("press")).unwrap();
        assert_eq!(press.claim_count, 2);
        assert_eq!(press.min_confidence, Some(0.5));
        assert_eq!(press.max_confidence, Some(0.7));
        assert_eq!(press.latest_confidence, Some(0.7), "latest = highest-tx member");
        assert_eq!(press.most_recent_assertion, ts(300));
        assert_eq!(press.backs_values.len(), 1, "press backs a single distinct value");
        assert!(press.backs_values[0].contains("Bob"));
    }

    // ---- case 11: determinism ----------------------------------------------

    #[test]
    fn genealogy_is_byte_identical() {
        let h = history(vec![
            open_version(1, 1000, 100, one_prop("ceo", "Alice"), Some(prov(Some("sec"), Some(0.9)))),
            open_version(2, 1000, 200, one_prop("ceo", "Bob"), Some(prov(Some("press"), Some(0.7)))),
        ]);
        let a = analyze(&h, "ceo");
        let b = analyze(&h, "ceo");
        assert_eq!(a, b);
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }

    #[test]
    fn genealogy_is_key_order_independent() {
        let props_a = PropertyMapBuilder::new()
            .insert("ceo", "Alice")
            .insert("hq", "NYC")
            .build();
        let props_a_shuffled = PropertyMapBuilder::new()
            .insert("hq", "NYC")
            .insert("ceo", "Alice")
            .build();
        let props_b = PropertyMapBuilder::new()
            .insert("ceo", "Bob")
            .insert("hq", "NYC")
            .build();
        let props_b_shuffled = PropertyMapBuilder::new()
            .insert("hq", "NYC")
            .insert("ceo", "Bob")
            .build();
        let h1 = history(vec![
            open_version(1, 1000, 100, props_a, None),
            open_version(2, 1000, 200, props_b, None),
        ]);
        let h2 = history(vec![
            open_version(1, 1000, 100, props_a_shuffled, None),
            open_version(2, 1000, 200, props_b_shuffled, None),
        ]);
        assert_eq!(format!("{:?}", analyze(&h1, "ceo")), format!("{:?}", analyze(&h2, "ceo")));
    }

    // ---- case 13: elision --------------------------------------------------

    #[test]
    fn max_claims_elision_sets_truncated() {
        let h = history(vec![
            open_version(1, 1000, 100, one_prop("ceo", "Alice"), None),
            open_version(2, 1000, 200, one_prop("ceo", "Bob"), None),
            open_version(3, 1000, 300, one_prop("ceo", "Carol"), None),
        ]);
        let g = genealogy_entity_property(
            node_entity(42),
            &h,
            "ceo",
            &GenealogyOptions::new().with_max_claims(2),
        );
        assert_eq!(g.claims.len(), 2);
        assert!(g.truncated, "elision must disclose truncation");
    }

    // ---- case 14: as_of_transaction_time -----------------------------------

    #[test]
    fn as_of_excludes_later_recorded() {
        let h = history(vec![
            open_version(1, 1000, 100, one_prop("ceo", "Alice"), None),
            open_version(2, 1000, 200, one_prop("ceo", "Bob"), None),
        ]);
        // Before the conflicting write entered (as_of=150): no contradiction.
        let g = genealogy_entity_property(
            node_entity(42),
            &h,
            "ceo",
            &GenealogyOptions::new().with_as_of_transaction_time(ts(150)),
        );
        assert!(!g.has_contradiction());
        // At/after (as_of=250): contradiction visible.
        let g2 = genealogy_entity_property(
            node_entity(42),
            &h,
            "ceo",
            &GenealogyOptions::new().with_as_of_transaction_time(ts(250)),
        );
        assert!(g2.has_contradiction());
    }

    // ---- case 17: value types ----------------------------------------------

    #[test]
    fn value_types_int_float_string_bool() {
        // Int
        let hi = history(vec![
            open_version(1, 1000, 100, one_prop("v", 1_i64), None),
            open_version(2, 1000, 200, one_prop("v", 2_i64), None),
        ]);
        assert!(analyze(&hi, "v").has_contradiction());
        // Float
        let hf = history(vec![
            open_version(1, 1000, 100, one_prop("v", 1.5_f64), None),
            open_version(2, 1000, 200, one_prop("v", 2.5_f64), None),
        ]);
        assert!(analyze(&hf, "v").has_contradiction());
        // String
        let hs = history(vec![
            open_version(1, 1000, 100, one_prop("v", "x"), None),
            open_version(2, 1000, 200, one_prop("v", "y"), None),
        ]);
        assert!(analyze(&hs, "v").has_contradiction());
        // Bool
        let hb = history(vec![
            open_version(1, 1000, 100, one_prop("v", true), None),
            open_version(2, 1000, 200, one_prop("v", false), None),
        ]);
        assert!(analyze(&hb, "v").has_contradiction());
    }

    // ---- case 20: narrative names divergence + sources ---------------------

    #[test]
    fn narrative_names_divergence_and_sources() {
        let h = history(vec![
            open_version(1, 1000, 100, one_prop("ceo", "Alice"), Some(prov(Some("sec"), Some(0.9)))),
            open_version(2, 1000, 200, one_prop("ceo", "Bob"), Some(prov(Some("press"), Some(0.5)))),
        ]);
        let g = analyze(&h, "ceo");
        assert!(g.narrative.contains("ceo"));
        assert!(g.narrative.contains("Alice"));
        assert!(g.narrative.contains("Bob"));
        assert!(g.narrative.contains("sec"));
        assert!(g.narrative.contains("press"));
    }

    // ---- helpers -----------------------------------------------------------

    #[test]
    fn resolve_scan_limit_clamps() {
        assert_eq!(resolve_scan_limit(0), DEFAULT_CONTRADICTION_LIMIT);
        assert_eq!(resolve_scan_limit(50), 50);
        assert_eq!(resolve_scan_limit(MAX_CONTRADICTION_LIMIT + 10), MAX_CONTRADICTION_LIMIT);
    }

    #[test]
    fn dominant_kind_tie_breaks_to_retroactive() {
        assert_eq!(dominant_kind(&[]), DivergenceKind::RetroactiveCorrection);
    }
}

#[cfg(test)]
mod db_tests {
    //! End-to-end tests exercising the real write path + the convenience API.
    use super::*;
    use crate::api::transaction::WriteRequestOptions;
    use crate::core::PropertyMapBuilder;
    use crate::core::id::NodeId;
    use crate::core::temporal::time;

    fn backdated(base_offset_secs: i64) -> Timestamp {
        let base = time::now().wallclock();
        Timestamp::new(base - base_offset_secs * 1_000_000, 0).unwrap()
    }

    #[test]
    fn end_to_end_contemporaneous_disagreement() {
        let db = AletheiaDB::new().unwrap();
        let vt_early = backdated(1000);
        let vt_late = backdated(500);
        let id = db
            .create_node_with_options(
                "Company",
                PropertyMapBuilder::new().insert("ceo", "Alice").build(),
                WriteRequestOptions::new().with_valid_from(vt_early),
            )
            .unwrap();
        // Forward, unretracted change => both remain asserted over the overlap.
        db.update_node_with_options(
            id,
            PropertyMapBuilder::new().insert("ceo", "Bob").build(),
            WriteRequestOptions::new().with_valid_from(vt_late),
        )
        .unwrap();

        let g = db
            .contradiction_genealogy(
                ContradictionTarget::EntityProperty {
                    entity: EntityId::Node(id),
                    property: "ceo".to_string(),
                },
                &GenealogyOptions::default(),
            )
            .unwrap();
        assert!(g.has_contradiction());
        assert_eq!(g.pairs[0].kind, DivergenceKind::ContemporaneousDisagreement);
    }

    #[test]
    fn unknown_entity_not_found() {
        let db = AletheiaDB::new().unwrap();
        let err = db
            .contradiction_genealogy(
                ContradictionTarget::EntityProperty {
                    entity: EntityId::Node(NodeId::new(999_999).unwrap()),
                    property: "ceo".to_string(),
                },
                &GenealogyOptions::default(),
            )
            .unwrap_err();
        assert!(matches!(err, Error::Storage(_)), "got {err:?}");
    }

    #[test]
    fn unknown_property_invalid_argument() {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node("Company", PropertyMapBuilder::new().insert("ceo", "Alice").build())
            .unwrap();
        let err = db
            .contradiction_genealogy(
                ContradictionTarget::EntityProperty {
                    entity: EntityId::Node(id),
                    property: "never".to_string(),
                },
                &GenealogyOptions::default(),
            )
            .unwrap_err();
        assert!(
            matches!(err, Error::Query(QueryError::InvalidParameter { .. })),
            "got {err:?}"
        );
    }

    #[test]
    fn empty_claims_invalid_argument() {
        let db = AletheiaDB::new().unwrap();
        let err = db
            .contradiction_genealogy(
                ContradictionTarget::Claims(vec![]),
                &GenealogyOptions::default(),
            )
            .unwrap_err();
        assert!(
            matches!(err, Error::Query(QueryError::InvalidParameter { .. })),
            "got {err:?}"
        );
    }

    #[test]
    fn edge_property_contradiction_parity() {
        let db = AletheiaDB::new().unwrap();
        let a = db.create_node("P", PropertyMapBuilder::new().insert("n", "a").build()).unwrap();
        let b = db.create_node("P", PropertyMapBuilder::new().insert("n", "b").build()).unwrap();
        let vt_early = backdated(1000);
        let e = db
            .create_edge_with_options(
                a,
                b,
                "ROLE",
                PropertyMapBuilder::new().insert("title", "CEO").build(),
                WriteRequestOptions::new().with_valid_from(vt_early),
            )
            .unwrap();
        db.update_edge_with_options(
            e,
            PropertyMapBuilder::new().insert("title", "Chair").build(),
            WriteRequestOptions::new().with_valid_from(vt_early),
        )
        .unwrap();
        let g = db
            .contradiction_genealogy(
                ContradictionTarget::EntityProperty {
                    entity: EntityId::Edge(e),
                    property: "title".to_string(),
                },
                &GenealogyOptions::default(),
            )
            .unwrap();
        assert!(g.has_contradiction(), "edge-property contradictions detected");
    }

    #[test]
    fn find_contradictions_recall_precision() {
        let db = AletheiaDB::new().unwrap();
        let vt_early = backdated(1000);
        // 8 nodes; seed exactly 2 with contradictions.
        let mut seeded = Vec::new();
        for i in 0..8 {
            let id = db
                .create_node_with_options(
                    "Company",
                    PropertyMapBuilder::new().insert("ceo", format!("Person{i}")).build(),
                    WriteRequestOptions::new().with_valid_from(vt_early),
                )
                .unwrap();
            if i == 2 || i == 5 {
                // Unretracted overlapping change => contradiction.
                db.update_node_with_options(
                    id,
                    PropertyMapBuilder::new().insert("ceo", format!("Other{i}")).build(),
                    WriteRequestOptions::new().with_valid_from(vt_early),
                )
                .unwrap();
                seeded.push(EntityId::Node(id));
            }
        }
        let scan = db
            .find_contradictions(&ContradictionScope::new().with_label("Company"))
            .unwrap();
        assert_eq!(scan.contradictions.len(), 2, "exactly the 2 seeded conflicts");
        let found: std::collections::HashSet<EntityId> =
            scan.contradictions.iter().map(|c| c.entity).collect();
        for s in seeded {
            assert!(found.contains(&s), "seeded contradiction {s} must be found");
        }
    }

    #[test]
    fn find_contradictions_pagination() {
        let db = AletheiaDB::new().unwrap();
        let vt_early = backdated(1000);
        for i in 0..5 {
            let id = db
                .create_node_with_options(
                    "Company",
                    PropertyMapBuilder::new().insert("ceo", format!("P{i}")).build(),
                    WriteRequestOptions::new().with_valid_from(vt_early),
                )
                .unwrap();
            db.update_node_with_options(
                id,
                PropertyMapBuilder::new().insert("ceo", format!("Q{i}")).build(),
                WriteRequestOptions::new().with_valid_from(vt_early),
            )
            .unwrap();
        }
        let page1 = db
            .find_contradictions(&ContradictionScope::new().with_label("Company").with_page(2, 0))
            .unwrap();
        assert_eq!(page1.contradictions.len(), 2);
        assert!(page1.has_more);
        assert_eq!(page1.next_offset, Some(2));
        let page3 = db
            .find_contradictions(&ContradictionScope::new().with_label("Company").with_page(2, 4))
            .unwrap();
        assert_eq!(page3.contradictions.len(), 1, "5 total => last page has 1");
        assert!(!page3.has_more);
    }

    #[test]
    fn claims_target_spanning_two_entities() {
        let db = AletheiaDB::new().unwrap();
        let a = db.create_node("Company", PropertyMapBuilder::new().insert("ceo", "Alice").build()).unwrap();
        let b = db.create_node("Company", PropertyMapBuilder::new().insert("ceo", "Bob").build()).unwrap();
        let va = db.get_node_history(a).unwrap().versions[0].version_id;
        let vb = db.get_node_history(b).unwrap().versions[0].version_id;
        let g = db
            .contradiction_genealogy(
                ContradictionTarget::Claims(vec![
                    ClaimRef { entity: EntityId::Node(a), version: va },
                    ClaimRef { entity: EntityId::Node(b), version: vb },
                ]),
                &GenealogyOptions::default(),
            )
            .unwrap();
        assert!(g.has_contradiction(), "two entities' competing ceo claims conflict");
        assert_eq!(g.entity, None, "multi-entity target has no single entity");
        assert_eq!(g.property.as_deref(), Some("ceo"));
    }
}
