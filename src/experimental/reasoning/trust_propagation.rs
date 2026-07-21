//! Trust propagation: computed confidence over derivation lineage (Issue #3382).
//!
//! Declared provenance confidence (Issue #3224) stops at the first hop: a fact
//! written directly from a source carries that source's confidence, but the
//! facts agents actually consume are *derived* — a summary distilled from ten
//! documents, an entity merged from three records, an inference chained across
//! prior inferences. Derivation lineage (Issue #3371) records the *structure*
//! of that evidence; this module **computes over it**, inferring a derived
//! fact's **computed confidence** from its upstream evidence under a declared,
//! deterministic combination policy — with an explainable per-fact breakdown
//! and recomputation when the evidence moves.
//!
//! # Lazy, never stored (AC4/AC5)
//!
//! Computed confidence is **never** persisted. It is computed on read by
//! walking the in-memory lineage closure and combining upstream confidences
//! under the active [`TrustPolicy`]. Evidence changes (a superseding write with
//! new provenance, or an Issue #3230 retraction) therefore flow downstream for
//! free — the next read recomputes from current state, so the staleness bound
//! is zero. Recorded history is never mutated; the write path is untouched.
//!
//! # The two combinators (AC1)
//!
//! Given the set of *contributing* upstream confidences `c_1..c_n ∈ [0,1]` at a
//! derivation node:
//!
//! - [`TrustCombinator::WeakestLink`] (conservative): `min(c_1..c_n)`.
//! - [`TrustCombinator::NoisyOr`] (independence): `1 − ∏_i (1 − c_i)`.
//!
//! Both are deterministic, order-independent, and hand-computable. They are
//! explicit approximations assuming evidence independence.
//!
//! # Feature gate
//!
//! The whole feature is gated behind the experimental `semantic-reasoning`
//! ("Nova") cohort flag — zero write-path/read-path overhead when disabled
//! (AC8).

use std::collections::HashMap;
use std::path::PathBuf;

use parking_lot::{Mutex, RwLock};

use crate::core::error::Result;
// `Error` is only referenced by the serde-gated `save_serialized` path (LOW-1).
#[cfg(feature = "serde")]
use crate::core::error::Error;
use crate::core::temporal::Timestamp;

/// Persisted-format version for the trust-policy registry sidecar file. Bumped
/// only on an incompatible on-disk change (mirrors the snapshot registry).
///
/// Only referenced by the serde-gated persistence path; the in-memory registry
/// needs no on-disk format version.
#[cfg(feature = "serde")]
pub(crate) const PERSIST_FORMAT_VERSION: u32 = 1;

/// Default maximum transitive depth for a [`trust_breakdown`](crate::AletheiaDB::trust_breakdown)
/// walk and the caller-facing closure bound. Mirrors
/// [`LineageQueryOptions::DEFAULT_MAX_DEPTH`](crate::core::lineage::LineageQueryOptions::DEFAULT_MAX_DEPTH).
pub const DEFAULT_MAX_DEPTH: usize = 32;

/// Default maximum number of breakdown nodes serialized before truncation.
pub const DEFAULT_MAX_NODES: usize = 1000;

/// Private hard recursion ceiling for the scalar evaluator — the overflow
/// backstop (review-fix #3), distinct from the caller-facing
/// [`DEFAULT_MAX_DEPTH`]. Cycles are impossible by construction (Issue #3371
/// rejects them); this is defence-in-depth so a pathological or future-relaxed
/// graph cannot overflow the stack.
pub(crate) const SCALAR_MAX_DEPTH: usize = 1024;

/// The documented neutral confidence constant used by
/// [`MissingConfidencePolicy::Neutral`] and as the flagged fallback for a node
/// whose entire contributing child set was excluded.
pub(crate) const NEUTRAL: f64 = 0.5;

/// The two built-in confidence combinators (AC1).
///
/// Both are deterministic and order-independent over the contributing set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum TrustCombinator {
    /// Conservative / pessimistic: a chain is only as strong as its weakest
    /// evidence — `computed = min(c_1..c_n)`.
    WeakestLink,
    /// Independence / corroboration: independent evidence corroborates —
    /// `computed = 1 − ∏_i (1 − c_i)`.
    NoisyOr,
}

impl TrustCombinator {
    /// Stable lowercase string form for JSON/display surfaces.
    pub const fn as_str(self) -> &'static str {
        match self {
            TrustCombinator::WeakestLink => "weakest_link",
            TrustCombinator::NoisyOr => "noisy_or",
        }
    }
}

/// The missing-confidence resolution rule (AC6) — explicit, per-policy, never a
/// silent default. Governs how a *root* fact written without any confidence
/// contributes to its parent's combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum MissingConfidencePolicy {
    /// Treat a missing confidence as `0.0` (pessimistic).
    Zero,
    /// Treat a missing confidence as the documented neutral constant
    /// ([`NEUTRAL`] = 0.5).
    Neutral,
    /// Drop the missing-confidence input from the contributing set entirely
    /// (it does not affect the combinator).
    Ignore,
}

/// A trust-propagation policy: the active combinator and the missing-confidence
/// rule. Resolved per fact (a per-label override wins over the database
/// default).
///
/// The [`Default`] is the conservative choice: [`TrustCombinator::WeakestLink`]
/// with [`MissingConfidencePolicy::Zero`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TrustPolicy {
    /// How confidence combines across a derivation node's contributing inputs.
    pub combinator: TrustCombinator,
    /// How a missing-confidence root fact contributes.
    pub missing: MissingConfidencePolicy,
}

impl TrustPolicy {
    /// Construct a policy from an explicit combinator and missing-confidence
    /// rule.
    pub const fn new(combinator: TrustCombinator, missing: MissingConfidencePolicy) -> Self {
        Self {
            combinator,
            missing,
        }
    }

    /// A weakest-link policy with the given missing-confidence rule.
    pub const fn weakest_link(missing: MissingConfidencePolicy) -> Self {
        Self::new(TrustCombinator::WeakestLink, missing)
    }

    /// A noisy-OR policy with the given missing-confidence rule.
    pub const fn noisy_or(missing: MissingConfidencePolicy) -> Self {
        Self::new(TrustCombinator::NoisyOr, missing)
    }
}

impl Default for TrustPolicy {
    fn default() -> Self {
        Self::new(TrustCombinator::WeakestLink, MissingConfidencePolicy::Zero)
    }
}

/// Per-upstream classification driving a fact's contribution to its parent
/// combination.
///
/// `Absent` (deleted / dangling in current state) is a DISTINCT variant from
/// `Retracted` (Issue #3230 valid-time-closed) — both contribute `0.0` today,
/// but the explanation distinguishes "we withdrew this as of a valid time" from
/// "this is gone", and a future policy can treat them differently without a
/// format change (review-fix #2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ConfidenceSource {
    /// A root fact with a declared Issue #3224 confidence.
    Declared,
    /// An intermediate derivation whose value was recursively computed.
    Computed,
    /// A root written without any confidence (AC6): contributes per the
    /// active [`MissingConfidencePolicy`], always flagged.
    Missing,
    /// An Issue #3230 valid-time retraction: contributes `0.0` and dominates.
    Retracted,
    /// Deleted / dangling in current state: contributes `0.0` and dominates.
    /// Distinct from [`ConfidenceSource::Retracted`] (review-fix #2).
    Absent,
}

impl ConfidenceSource {
    /// Stable lowercase string form for JSON/display surfaces.
    pub const fn as_str(self) -> &'static str {
        match self {
            ConfidenceSource::Declared => "declared",
            ConfidenceSource::Computed => "computed",
            ConfidenceSource::Missing => "missing",
            ConfidenceSource::Retracted => "retracted",
            ConfidenceSource::Absent => "absent",
        }
    }
}

/// The two readable confidence fields for a fact, never conflated (AC2), plus
/// the flags that make the computed number self-describing.
///
/// `declared` is a straight read of the fact's own
/// [`Provenance::confidence`](crate::core::provenance::Provenance::confidence)
/// (`None` when the writer asserted none) — computation never overwrites it.
/// `computed` is always a concrete number; `has_lineage == false` signals it
/// simply equals the declared/leaf value (nothing was combined).
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ComputedConfidence {
    /// The writer-declared confidence from provenance (Issue #3224), untouched.
    /// `None` when the writer asserted no confidence.
    pub declared: Option<f64>,
    /// The value computed from upstream evidence under the active policy.
    pub computed: f64,
    /// Whether the fact had a lineage record that was expanded. When `false`,
    /// `computed` equals the fact's own declared/leaf value.
    pub has_lineage: bool,
    /// Which combinator produced `computed`, or `None` for a leaf (no lineage).
    pub combinator: Option<TrustCombinator>,
    /// `true` when any contributing input was missing-confidence and a rule was
    /// applied (AC6 "always flagged").
    pub has_missing_inputs: bool,
    /// `true` when any contributing input was retracted or absent (AC4 "never
    /// silently retains its old weight").
    pub has_retracted_inputs: bool,
    /// `true` when the closure walk hit the hard depth cap and the computation
    /// was bounded (defence-in-depth; not normally reachable).
    pub truncated: bool,
}

/// One node of the explainable computation tree (AC3).
///
/// Each node names the fact ([`reference`](Self::reference)), its current-state
/// [`status`](Self::status), its resulting `confidence`, the classification of
/// that confidence ([`source`](Self::source)), the `combinator` applied at this
/// node over its children (or `None` for a leaf), and the child breakdowns.
///
/// **Truncation is honest** (review-fix #1): when the tree is bounded by a
/// depth or node-count cap the node's `confidence` remains the **full-accuracy**
/// computed value — the cap governs how many nodes are *serialized*, never how
/// many confidences are *combined* — so a truncated explanation never inflates
/// (or deflates) the reported number. A truncated node carries
/// `truncated == true` and empty `children`.
///
/// Not `serde`-serializable (it embeds [`LineageRef`] and [`FactStatus`]); the
/// MCP projection is a deferred follow-up (§2.11).
#[derive(Debug, Clone, PartialEq)]
pub struct TrustBreakdown {
    /// The version-pinned fact this node explains.
    pub reference: crate::core::lineage::LineageRef,
    /// The referenced fact's current-state status at the evaluation time.
    pub status: crate::db::lineage::FactStatus,
    /// This node's resulting (full-accuracy) computed confidence.
    pub confidence: f64,
    /// The classification of this node's contribution.
    pub source: ConfidenceSource,
    /// The combinator applied at this node over its children, or `None` for a
    /// leaf / terminal (retracted / absent) node.
    pub combinator: Option<TrustCombinator>,
    /// The child breakdowns (empty for a leaf, terminal, or truncated node).
    pub children: Vec<TrustBreakdown>,
    /// `true` when this node's subtree was elided by a depth / node-count /
    /// cycle bound. The `confidence` is still full-accuracy (review-fix #1).
    pub truncated: bool,
}

/// Options bounding a [`trust_breakdown`](crate::AletheiaDB::trust_breakdown)
/// walk and an optional `AS OF` transaction-time coordinate.
#[derive(Debug, Clone, Copy)]
pub struct TrustOptions {
    /// Maximum transitive depth to expand before marking a subtree truncated.
    pub max_depth: usize,
    /// Maximum number of **descendant** breakdown nodes to serialize before
    /// truncation. The root node is always emitted and does NOT count against
    /// this budget, so `max_nodes = N` serializes up to `root + N` nodes
    /// (adversarial #6).
    pub max_nodes: usize,
    /// Optional `AS OF` transaction-time bound: evaluate lineage and
    /// confidences as recorded by this transaction time (AC5). `None` = now.
    pub as_of: Option<Timestamp>,
    /// Optional valid-time evaluation coordinate (Issue #3382): terminality is
    /// keyed on this instant rather than wallclock now, so a fact whose valid
    /// interval has ended at this coordinate is retracted-class and a
    /// future-dated retraction is not yet terminal. Independent of `as_of`
    /// (valid time and transaction time are orthogonal axes). `None` = now,
    /// which reproduces the prior wallclock-now behavior exactly.
    pub as_of_valid_time: Option<Timestamp>,
}

impl TrustOptions {
    /// Options with the default depth and node caps and no `AS OF` bound.
    pub fn new() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
            max_nodes: DEFAULT_MAX_NODES,
            as_of: None,
            as_of_valid_time: None,
        }
    }

    /// Set the maximum transitive depth.
    #[must_use]
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Set the maximum number of serialized breakdown nodes.
    #[must_use]
    pub fn with_max_nodes(mut self, max_nodes: usize) -> Self {
        self.max_nodes = max_nodes;
        self
    }

    /// Set the `AS OF` transaction-time bound.
    #[must_use]
    pub fn with_as_of(mut self, as_of: Timestamp) -> Self {
        self.as_of = Some(as_of);
        self
    }

    /// Set the valid-time evaluation coordinate (Issue #3382). Independent of
    /// [`with_as_of`](Self::with_as_of); omitting it keys terminality on
    /// wallclock now (prior behavior).
    #[must_use]
    pub fn with_as_of_valid_time(mut self, as_of_valid_time: Timestamp) -> Self {
        self.as_of_valid_time = Some(as_of_valid_time);
        self
    }
}

impl Default for TrustOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Error returned when constructing a [`ComputedConfidenceFilter`] from
/// caller-supplied bounds (mirrors
/// [`ProvenanceFilterError`](crate::core::provenance::ProvenanceFilterError)).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ComputedConfidenceFilterError {
    /// A bound was NaN or outside the inclusive `[0.0, 1.0]` range.
    #[error("{field} must be between 0.0 and 1.0, got {value}")]
    InvalidBound {
        /// Which bound was invalid (`"min"` / `"max"`).
        field: &'static str,
        /// The rejected value (may be NaN).
        value: f64,
    },
    /// `min` exceeded `max` — an empty range that can never match.
    #[error("computed-confidence min {min} must not exceed max {max}")]
    InvertedRange {
        /// The supplied lower bound.
        min: f64,
        /// The supplied upper bound.
        max: f64,
    },
}

/// A predicate filtering facts by their **computed** confidence (AC7).
///
/// A reusable, surface-agnostic threshold that composes with the declared
/// confidence [`ProvenanceFilter`](crate::core::provenance::ProvenanceFilter):
/// both constraints AND together (a fact is kept iff it satisfies both). The
/// filter itself is a pure bound holder — evaluating a fact's computed
/// confidence requires the database's lineage walk, so it is applied via
/// [`AletheiaDB::computed_confidence_matches`](crate::AletheiaDB::computed_confidence_matches).
///
/// # Semantics
///
/// - `min` is an **inclusive** lower bound; `max` an **inclusive** upper bound.
/// - An **inactive** filter (both bounds `None`) matches everything, so omitting
///   it is byte-identical to no filtering.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ComputedConfidenceFilter {
    /// Inclusive lower bound on computed confidence, if any.
    pub min: Option<f64>,
    /// Inclusive upper bound on computed confidence, if any.
    pub max: Option<f64>,
}

impl ComputedConfidenceFilter {
    /// A filter keeping facts whose computed confidence is `>= min`.
    pub const fn at_least(min: f64) -> Self {
        Self {
            min: Some(min),
            max: None,
        }
    }

    /// A filter keeping facts whose computed confidence is `<= max`.
    pub const fn at_most(max: f64) -> Self {
        Self {
            min: None,
            max: Some(max),
        }
    }

    /// A filter keeping facts whose computed confidence is in `[min, max]`.
    pub const fn range(min: f64, max: f64) -> Self {
        Self {
            min: Some(min),
            max: Some(max),
        }
    }

    /// Validate caller-supplied bounds, returning `Ok(None)` when neither bound
    /// is set (so callers can cheaply skip all filtering).
    ///
    /// # Errors
    ///
    /// - [`ComputedConfidenceFilterError::InvalidBound`] if a bound is NaN or
    ///   outside `[0.0, 1.0]`.
    /// - [`ComputedConfidenceFilterError::InvertedRange`] if `min > max`.
    pub fn validated(
        min: Option<f64>,
        max: Option<f64>,
    ) -> std::result::Result<Option<ComputedConfidenceFilter>, ComputedConfidenceFilterError> {
        for (field, bound) in [("min", min), ("max", max)] {
            if let Some(v) = bound
                && (v.is_nan() || !(0.0..=1.0).contains(&v))
            {
                return Err(ComputedConfidenceFilterError::InvalidBound { field, value: v });
            }
        }
        if let (Some(lo), Some(hi)) = (min, max)
            && lo > hi
        {
            return Err(ComputedConfidenceFilterError::InvertedRange { min: lo, max: hi });
        }
        if min.is_none() && max.is_none() {
            return Ok(None);
        }
        Ok(Some(ComputedConfidenceFilter { min, max }))
    }

    /// Whether this filter imposes any constraint.
    pub fn is_active(&self) -> bool {
        self.min.is_some() || self.max.is_some()
    }

    /// Evaluate the predicate against a concrete computed-confidence value.
    ///
    /// An inactive filter matches everything.
    pub fn matches(&self, value: f64) -> bool {
        if let Some(min) = self.min
            && value < min
        {
            return false;
        }
        if let Some(max) = self.max
            && value > max
        {
            return false;
        }
        true
    }
}

/// Clamp a value into `[0.0, 1.0]`, mapping `NaN` to `0.0`.
///
/// Used on every confidence value fed into or out of a combinator so a
/// malformed provenance value can never produce an out-of-range or `NaN`
/// computed confidence.
pub(crate) fn clamp01(value: f64) -> f64 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

/// Combine a set of child confidences under `combinator`.
///
/// `None` children are **excluded** (the [`MissingConfidencePolicy::Ignore`]
/// case); `Some(c)` children contribute `c`. Returns `(value, all_excluded)`:
///
/// - When the included set is **empty** (all children excluded, or no children
///   at all), the result is `(NEUTRAL, true)` — never `min` of an empty set or
///   an empty product — and the caller flags it as missing.
/// - [`TrustCombinator::WeakestLink`] folds the minimum.
/// - [`TrustCombinator::NoisyOr`] computes `1 − ∏(1 − c)`.
///
/// Every input is passed through [`clamp01`], and the result is clamped too.
pub(crate) fn combine_values(children: &[Option<f64>], combinator: TrustCombinator) -> (f64, bool) {
    let included: Vec<f64> = children.iter().filter_map(|c| c.map(clamp01)).collect();

    if included.is_empty() {
        return (NEUTRAL, true);
    }

    let value = match combinator {
        TrustCombinator::WeakestLink => included.iter().copied().fold(f64::INFINITY, f64::min),
        TrustCombinator::NoisyOr => {
            let product: f64 = included.iter().map(|c| 1.0 - c).product();
            1.0 - product
        }
    };

    (clamp01(value), false)
}

/// A read-only view of the active trust policies (AC1 "discoverable"): the
/// database default plus every per-label override in a stable, label-sorted
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TrustPolicyView {
    /// The database-wide default policy applied when no label override matches.
    pub default: TrustPolicy,
    /// Per-label / per-edge-type overrides, sorted by label.
    pub labels: Vec<(String, TrustPolicy)>,
}

/// In-memory state of the trust-policy registry: the default policy plus any
/// per-label overrides.
///
/// The derived [`Default`] uses [`TrustPolicy::default`] (weakest-link + Zero)
/// and an empty override map.
#[derive(Debug, Clone, Default)]
struct RegistryState {
    default: TrustPolicy,
    labels: HashMap<String, TrustPolicy>,
}

/// The on-disk registry envelope (versioned, mirrors the snapshot registry).
///
/// Only compiled with the `serde` feature; without it the registry is
/// in-memory-only and never (de)serialized.
#[cfg(feature = "serde")]
#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedRegistry {
    version: u32,
    default: TrustPolicy,
    /// Overrides as a sorted `(label, policy)` list for deterministic output.
    labels: Vec<(String, TrustPolicy)>,
}

/// In-process registry of trust policies, optionally persisted to a sidecar
/// JSON file (Issue #3382).
///
/// Entirely off the data write path — mutating it never touches
/// current/historical storage, the WAL, or `current_timestamp` — so it is a
/// leaf like the #3370 snapshot registry it mirrors. Durably persisted to
/// `{data_dir}/trust_policy.json` when index persistence is enabled;
/// in-memory-only for ephemeral databases.
pub(crate) struct TrustRegistry {
    state: RwLock<RegistryState>,
    /// Sidecar file path when persistence is enabled; `None` for ephemeral,
    /// in-memory-only registries (`AletheiaDB::new()`).
    persist_path: Option<PathBuf>,
    /// Serializes concurrent disk saves (the in-memory state has its own
    /// `RwLock`; this guards the temp-file+rename dance).
    save_lock: Mutex<()>,
}

impl TrustRegistry {
    /// Create an empty, memory-only registry (no file is ever written).
    pub(crate) fn in_memory() -> Self {
        Self {
            state: RwLock::new(RegistryState::default()),
            persist_path: None,
            save_lock: Mutex::new(()),
        }
    }

    /// Open a registry, loading any existing sidecar at `path`.
    ///
    /// `path` is `None` for an in-memory-only registry. A missing file yields
    /// an empty registry (first run).
    ///
    /// # Tolerant load (mirrors the #3370 snapshot registry)
    ///
    /// Trust policies are non-critical configuration: losing them reverts to
    /// the conservative default policy, costing at most a re-declaration. So —
    /// like the snapshot registry and unlike the security-critical auth key
    /// store — a corrupt, unparseable, or unknown-future-version
    /// `trust_policy.json` must **not brick database startup**. On such a file
    /// we warn, quarantine it aside (rename to `*.corrupt`, preserving the
    /// bytes), and start with an empty registry. The load is driven off a
    /// single `read_to_string` (no `exists()` pre-check, so no TOCTOU gap): a
    /// `NotFound` error is the normal first-run case; any other read I/O error
    /// is treated exactly like a parse failure.
    pub(crate) fn open(path: Option<PathBuf>) -> Result<Self> {
        let registry = Self {
            state: RwLock::new(RegistryState::default()),
            persist_path: path.clone(),
            save_lock: Mutex::new(()),
        };
        #[cfg(feature = "serde")]
        if let Some(path) = path {
            match std::fs::read_to_string(&path) {
                Ok(contents) => match serde_json::from_str::<PersistedRegistry>(&contents) {
                    Ok(parsed) if parsed.version <= PERSIST_FORMAT_VERSION => {
                        let mut state = registry.state.write();
                        state.default = parsed.default;
                        state.labels = parsed.labels.into_iter().collect();
                    }
                    Ok(parsed) => {
                        log_registry_warning(&format!(
                            "trust-policy registry at {} has unsupported future version {} (this \
                             build understands up to {}); quarantining it and starting with an \
                             empty registry",
                            path.display(),
                            parsed.version,
                            PERSIST_FORMAT_VERSION
                        ));
                        quarantine_corrupt_registry(&path);
                    }
                    Err(e) => {
                        log_registry_warning(&format!(
                            "failed to parse trust-policy registry at {} ({e}); quarantining it \
                             and starting with an empty registry",
                            path.display()
                        ));
                        quarantine_corrupt_registry(&path);
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    log_registry_warning(&format!(
                        "failed to read trust-policy registry at {} ({e}); quarantining it and \
                         starting with an empty registry",
                        path.display()
                    ));
                    quarantine_corrupt_registry(&path);
                }
            }
        }
        Ok(registry)
    }

    /// The database-wide default policy.
    pub(crate) fn default_policy(&self) -> TrustPolicy {
        self.state.read().default
    }

    /// The policy governing `label`: the per-label override if one is
    /// registered, else the database default.
    pub(crate) fn policy_for_label(&self, label: &str) -> TrustPolicy {
        let state = self.state.read();
        state.labels.get(label).copied().unwrap_or(state.default)
    }

    /// A stable, label-sorted view of all active policies (AC1 discoverable).
    pub(crate) fn view(&self) -> TrustPolicyView {
        let state = self.state.read();
        let mut labels: Vec<(String, TrustPolicy)> =
            state.labels.iter().map(|(k, v)| (k.clone(), *v)).collect();
        labels.sort_by(|a, b| a.0.cmp(&b.0));
        TrustPolicyView {
            default: state.default,
            labels,
        }
    }

    /// Set the database-wide default policy, persisting the change.
    ///
    /// Rolls back the in-memory change if the disk save fails, so RAM and disk
    /// never diverge (the caller sees `Err` *and* the default is unchanged).
    pub(crate) fn set_default(&self, policy: TrustPolicy) -> Result<()> {
        let _guard = self.save_lock.lock();
        let previous = {
            let mut state = self.state.write();
            let previous = state.default;
            state.default = policy;
            previous
        };
        if let Err(e) = self.save_locked() {
            self.state.write().default = previous;
            return Err(e);
        }
        Ok(())
    }

    /// Set a per-label override policy, persisting the change.
    ///
    /// Rolls back the in-memory change (restoring any prior override, or
    /// removing the entry if there was none) if the disk save fails.
    pub(crate) fn set_label(&self, label: &str, policy: TrustPolicy) -> Result<()> {
        let _guard = self.save_lock.lock();
        let previous = {
            let mut state = self.state.write();
            state.labels.insert(label.to_string(), policy)
        };
        if let Err(e) = self.save_locked() {
            let mut state = self.state.write();
            match previous {
                Some(prev) => {
                    state.labels.insert(label.to_string(), prev);
                }
                None => {
                    state.labels.remove(label);
                }
            }
            return Err(e);
        }
        Ok(())
    }

    /// Remove a per-label override, persisting the change. Removing an absent
    /// label is a no-op (still re-saved, harmlessly).
    pub(crate) fn drop_label(&self, label: &str) -> Result<()> {
        let _guard = self.save_lock.lock();
        let previous = {
            let mut state = self.state.write();
            state.labels.remove(label)
        };
        if let Err(e) = self.save_locked() {
            if let Some(prev) = previous {
                self.state.write().labels.insert(label.to_string(), prev);
            }
            return Err(e);
        }
        Ok(())
    }

    /// The durable-write body, **assuming `save_lock` is already held**. A
    /// no-op for in-memory-only registries.
    fn save_locked(&self) -> Result<()> {
        let Some(path) = &self.persist_path else {
            return Ok(());
        };

        #[cfg(not(feature = "serde"))]
        {
            let _ = path;
            Ok(())
        }

        #[cfg(feature = "serde")]
        {
            self.save_serialized(path)
        }
    }

    /// Serialize the registry to `path` (temp file + rename + parent fsync),
    /// mirroring the snapshot registry.
    #[cfg(feature = "serde")]
    fn save_serialized(&self, path: &std::path::Path) -> Result<()> {
        let (default, mut labels) = {
            let state = self.state.read();
            let labels: Vec<(String, TrustPolicy)> =
                state.labels.iter().map(|(k, v)| (k.clone(), *v)).collect();
            (state.default, labels)
        };
        labels.sort_by(|a, b| a.0.cmp(&b.0));

        let serialized = serde_json::to_vec_pretty(&PersistedRegistry {
            version: PERSIST_FORMAT_VERSION,
            default,
            labels,
        })
        .map_err(|e| Error::Other(format!("failed to serialize trust-policy registry: {e}")))?;

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

/// Emit a warning about the trust-policy registry under either logging
/// configuration (mirrors the snapshot registry's `log_registry_warning`).
///
/// Only used by the serde-gated sidecar load path.
#[cfg(feature = "serde")]
fn log_registry_warning(message: &str) {
    #[cfg(feature = "observability")]
    tracing::warn!("{}", message);
    #[cfg(not(feature = "observability"))]
    eprintln!("WARNING: {}", message);
}

/// Move a corrupt/unreadable sidecar aside (`path.corrupt`) so startup can
/// proceed with an empty registry while preserving the bad bytes. Failure to
/// quarantine is logged but never fatal.
///
/// Only used by the serde-gated sidecar load path.
#[cfg(feature = "serde")]
fn quarantine_corrupt_registry(path: &std::path::Path) {
    let mut corrupt = path.as_os_str().to_owned();
    corrupt.push(".corrupt");
    let corrupt = PathBuf::from(corrupt);
    if let Err(e) = std::fs::rename(path, &corrupt) {
        log_registry_warning(&format!(
            "could not quarantine corrupt trust-policy registry {} -> {}: {e}",
            path.display(),
            corrupt.display()
        ));
    }
}

/// Build the sidecar path for a database's trust-policy registry, or `None`
/// when the database is ephemeral (persistence disabled).
///
/// The file lives **inside** the configured persistence directory, at
/// `{persistence.data_dir}/trust_policy.json`, mirroring the snapshot
/// registry's placement.
pub(crate) fn registry_path_for(
    persistence: &crate::storage::index_persistence::PersistenceConfig,
) -> Option<PathBuf> {
    if !persistence.enabled {
        return None;
    }
    Some(persistence.data_dir.join("trust_policy.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combinator_as_str_is_stable() {
        assert_eq!(TrustCombinator::WeakestLink.as_str(), "weakest_link");
        assert_eq!(TrustCombinator::NoisyOr.as_str(), "noisy_or");
    }

    #[test]
    fn confidence_source_as_str_is_stable() {
        assert_eq!(ConfidenceSource::Declared.as_str(), "declared");
        assert_eq!(ConfidenceSource::Computed.as_str(), "computed");
        assert_eq!(ConfidenceSource::Missing.as_str(), "missing");
        assert_eq!(ConfidenceSource::Retracted.as_str(), "retracted");
        assert_eq!(ConfidenceSource::Absent.as_str(), "absent");
    }

    #[test]
    fn retracted_and_absent_are_distinct() {
        // Review-fix #2: they are separate variants even though both mean 0.0.
        assert_ne!(ConfidenceSource::Retracted, ConfidenceSource::Absent);
    }

    #[test]
    fn default_policy_is_weakest_link_zero() {
        let p = TrustPolicy::default();
        assert_eq!(p.combinator, TrustCombinator::WeakestLink);
        assert_eq!(p.missing, MissingConfidencePolicy::Zero);
    }

    #[test]
    fn policy_constructors() {
        assert_eq!(
            TrustPolicy::weakest_link(MissingConfidencePolicy::Ignore),
            TrustPolicy::new(
                TrustCombinator::WeakestLink,
                MissingConfidencePolicy::Ignore
            )
        );
        assert_eq!(
            TrustPolicy::noisy_or(MissingConfidencePolicy::Neutral),
            TrustPolicy::new(TrustCombinator::NoisyOr, MissingConfidencePolicy::Neutral)
        );
    }

    #[test]
    fn trust_options_defaults_and_builders() {
        let o = TrustOptions::default();
        assert_eq!(o.max_depth, DEFAULT_MAX_DEPTH);
        assert_eq!(o.max_nodes, DEFAULT_MAX_NODES);
        assert!(o.as_of.is_none());

        let o = TrustOptions::new()
            .with_max_depth(3)
            .with_max_nodes(5)
            .with_as_of(Timestamp::new_unchecked(1234, 0));
        assert_eq!(o.max_depth, 3);
        assert_eq!(o.max_nodes, 5);
        assert_eq!(o.as_of, Some(Timestamp::new_unchecked(1234, 0)));
    }

    #[test]
    fn clamp01_handles_nan_and_range() {
        assert_eq!(clamp01(f64::NAN), 0.0);
        assert_eq!(clamp01(-0.5), 0.0);
        assert_eq!(clamp01(1.5), 1.0);
        assert_eq!(clamp01(0.3), 0.3);
        assert_eq!(clamp01(0.0), 0.0);
        assert_eq!(clamp01(1.0), 1.0);
        assert_eq!(clamp01(f64::INFINITY), 1.0);
        assert_eq!(clamp01(f64::NEG_INFINITY), 0.0);
    }

    // ---- combinator math, hand-verified (C-1, C-2, C-3) ----

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "expected {b}, got {a}");
    }

    #[test]
    fn weakest_link_basic_is_min() {
        // C-1: node over {0.9, 0.9, 0.3} -> 0.3 exactly.
        let (v, excluded) = combine_values(
            &[Some(0.9), Some(0.9), Some(0.3)],
            TrustCombinator::WeakestLink,
        );
        approx(v, 0.3);
        assert!(!excluded);
    }

    #[test]
    fn noisy_or_basic_hand_computed() {
        // C-2: node over {0.9, 0.9, 0.3} -> 1 - (0.1)(0.1)(0.7) = 0.993.
        let (v, excluded) =
            combine_values(&[Some(0.9), Some(0.9), Some(0.3)], TrustCombinator::NoisyOr);
        approx(v, 0.993);
        assert!(!excluded);
    }

    #[test]
    fn single_upstream_passes_through() {
        // C-3: both combinators pass through the single value.
        for comb in [TrustCombinator::WeakestLink, TrustCombinator::NoisyOr] {
            let (v, excluded) = combine_values(&[Some(0.42)], comb);
            approx(v, 0.42);
            assert!(!excluded);
        }
    }

    #[test]
    fn noisy_or_two_values() {
        // 1 - (1-0.5)(1-0.5) = 1 - 0.25 = 0.75.
        let (v, _) = combine_values(&[Some(0.5), Some(0.5)], TrustCombinator::NoisyOr);
        approx(v, 0.75);
    }

    #[test]
    fn empty_included_set_is_neutral_flagged() {
        // No children at all.
        let (v, excluded) = combine_values(&[], TrustCombinator::WeakestLink);
        approx(v, NEUTRAL);
        assert!(excluded);
        // All children excluded (Ignore).
        let (v, excluded) = combine_values(&[None, None], TrustCombinator::NoisyOr);
        approx(v, NEUTRAL);
        assert!(excluded);
    }

    #[test]
    fn excluded_children_are_dropped_from_combination() {
        // Ignore-excluded (None) children do not affect the combinator.
        let (v, excluded) =
            combine_values(&[Some(0.9), None, Some(0.3)], TrustCombinator::WeakestLink);
        approx(v, 0.3);
        assert!(!excluded);
    }

    #[test]
    fn combine_clamps_out_of_range_inputs() {
        // Out-of-range / NaN inputs are clamped before combining.
        let (v, _) = combine_values(&[Some(1.5), Some(f64::NAN)], TrustCombinator::WeakestLink);
        // 1.5 -> 1.0, NaN -> 0.0, min = 0.0.
        approx(v, 0.0);
    }

    #[test]
    fn zero_dominates_weakest_link() {
        let (v, _) = combine_values(&[Some(0.0), Some(0.9)], TrustCombinator::WeakestLink);
        approx(v, 0.0);
    }

    // ---- registry (M2) ----

    #[test]
    fn registry_default_and_label_override() {
        let reg = TrustRegistry::in_memory();
        // Fresh registry: conservative default, no overrides.
        assert_eq!(reg.default_policy(), TrustPolicy::default());
        assert_eq!(reg.policy_for_label("Person"), TrustPolicy::default());

        // Set a new default.
        let noisy = TrustPolicy::noisy_or(MissingConfidencePolicy::Neutral);
        reg.set_default(noisy).unwrap();
        assert_eq!(reg.default_policy(), noisy);
        // A label with no override inherits the (new) default.
        assert_eq!(reg.policy_for_label("Person"), noisy);

        // Per-label override wins over the default.
        let wl_ignore = TrustPolicy::weakest_link(MissingConfidencePolicy::Ignore);
        reg.set_label("Claim", wl_ignore).unwrap();
        assert_eq!(reg.policy_for_label("Claim"), wl_ignore);
        assert_eq!(reg.policy_for_label("Person"), noisy);

        // Dropping the override reverts to the default.
        reg.drop_label("Claim").unwrap();
        assert_eq!(reg.policy_for_label("Claim"), noisy);
    }

    #[test]
    fn registry_view_sorts_labels() {
        let reg = TrustRegistry::in_memory();
        reg.set_label("Zeta", TrustPolicy::default()).unwrap();
        reg.set_label(
            "Alpha",
            TrustPolicy::noisy_or(MissingConfidencePolicy::Zero),
        )
        .unwrap();
        reg.set_label("Mu", TrustPolicy::default()).unwrap();
        let view = reg.view();
        let labels: Vec<&str> = view.labels.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(labels, vec!["Alpha", "Mu", "Zeta"]);
    }

    #[test]
    fn registry_path_is_none_when_persistence_disabled() {
        let cfg = crate::storage::index_persistence::PersistenceConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(registry_path_for(&cfg), None);
    }

    #[test]
    fn registry_path_is_inside_data_dir() {
        let cfg = crate::storage::index_persistence::PersistenceConfig {
            enabled: true,
            data_dir: PathBuf::from("/var/lib/aletheia/indexes"),
            ..Default::default()
        };
        assert_eq!(
            registry_path_for(&cfg),
            Some(PathBuf::from("/var/lib/aletheia/indexes/trust_policy.json"))
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn registry_persistence_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust_policy.json");

        {
            let reg = TrustRegistry::open(Some(path.clone())).unwrap();
            reg.set_default(TrustPolicy::noisy_or(MissingConfidencePolicy::Neutral))
                .unwrap();
            reg.set_label(
                "Claim",
                TrustPolicy::weakest_link(MissingConfidencePolicy::Ignore),
            )
            .unwrap();
        }

        // Reopen: default + override survive the round-trip.
        let reopened = TrustRegistry::open(Some(path)).unwrap();
        assert_eq!(
            reopened.default_policy(),
            TrustPolicy::noisy_or(MissingConfidencePolicy::Neutral)
        );
        assert_eq!(
            reopened.policy_for_label("Claim"),
            TrustPolicy::weakest_link(MissingConfidencePolicy::Ignore)
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn registry_open_quarantines_corrupt_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust_policy.json");
        std::fs::write(&path, b"not valid json {{{").unwrap();

        // A corrupt sidecar must NOT brick startup: quarantine + empty (default).
        let reg = TrustRegistry::open(Some(path.clone())).unwrap();
        assert_eq!(reg.default_policy(), TrustPolicy::default());
        assert!(reg.view().labels.is_empty());
        assert!(!path.exists(), "corrupt file was moved aside");
        assert!(
            dir.path().join("trust_policy.json.corrupt").exists(),
            "quarantined file exists"
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn registry_open_quarantines_future_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust_policy.json");
        let future = format!(
            r#"{{ "version": {}, "default": {{"combinator":"weakest_link","missing":"zero"}}, "labels": [] }}"#,
            PERSIST_FORMAT_VERSION + 1
        );
        std::fs::write(&path, future.as_bytes()).unwrap();

        let reg = TrustRegistry::open(Some(path.clone())).unwrap();
        assert_eq!(reg.default_policy(), TrustPolicy::default());
        assert!(!path.exists(), "future-version file was moved aside");
        assert!(dir.path().join("trust_policy.json.corrupt").exists());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn registry_set_default_rolls_back_on_save_failure() {
        // Point the sidecar at a path whose parent is a *file*, so the atomic
        // write cannot succeed and save() returns Err.
        let dir = tempfile::tempdir().unwrap();
        let blocking_file = dir.path().join("not_a_dir");
        std::fs::write(&blocking_file, b"x").unwrap();
        let bad_path = blocking_file.join("nested").join("trust_policy.json");

        let reg = TrustRegistry::open(Some(bad_path)).unwrap();
        let err = reg.set_default(TrustPolicy::noisy_or(MissingConfidencePolicy::Ignore));
        assert!(err.is_err(), "save failure surfaces as Err");
        // RAM did not diverge: the failed set rolled back to the prior default.
        assert_eq!(reg.default_policy(), TrustPolicy::default());
    }

    // ---- computed-confidence filter (M5) ----

    #[test]
    fn filter_inactive_matches_everything() {
        let f = ComputedConfidenceFilter::default();
        assert!(!f.is_active());
        assert!(f.matches(0.0));
        assert!(f.matches(1.0));
        assert_eq!(
            ComputedConfidenceFilter::validated(None, None).unwrap(),
            None
        );
    }

    #[test]
    fn filter_bounds_are_inclusive() {
        let f = ComputedConfidenceFilter::at_least(0.5);
        assert!(f.matches(0.5));
        assert!(f.matches(0.9));
        assert!(!f.matches(0.49));

        let f = ComputedConfidenceFilter::at_most(0.5);
        assert!(f.matches(0.5));
        assert!(!f.matches(0.51));

        let f = ComputedConfidenceFilter::range(0.3, 0.7);
        assert!(f.matches(0.3));
        assert!(f.matches(0.7));
        assert!(!f.matches(0.2));
        assert!(!f.matches(0.8));
    }

    #[test]
    fn filter_validation_rejects_bad_bounds() {
        for bad in [1.5_f64, -0.1, f64::NAN] {
            assert!(matches!(
                ComputedConfidenceFilter::validated(Some(bad), None),
                Err(ComputedConfidenceFilterError::InvalidBound { field: "min", .. })
            ));
        }
        assert!(matches!(
            ComputedConfidenceFilter::validated(Some(0.8), Some(0.2)),
            Err(ComputedConfidenceFilterError::InvertedRange { .. })
        ));
        // A valid range round-trips.
        assert_eq!(
            ComputedConfidenceFilter::validated(Some(0.2), Some(0.8)).unwrap(),
            Some(ComputedConfidenceFilter::range(0.2, 0.8))
        );
    }

    #[test]
    fn registry_ephemeral_in_memory_no_path() {
        let reg = TrustRegistry::in_memory();
        // in_memory registry saves are no-ops and never fail.
        reg.set_default(TrustPolicy::noisy_or(MissingConfidencePolicy::Zero))
            .unwrap();
        assert_eq!(
            reg.default_policy(),
            TrustPolicy::noisy_or(MissingConfidencePolicy::Zero)
        );
    }
}
