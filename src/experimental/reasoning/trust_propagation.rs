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

use crate::core::temporal::Timestamp;

/// Persisted-format version for the trust-policy registry sidecar file. Bumped
/// only on an incompatible on-disk change (mirrors the snapshot registry).
///
/// Only referenced by the serde-gated persistence path; the in-memory registry
/// needs no on-disk format version.
// Consumed by the durable registry landing in a later milestone (M2).
#[cfg(feature = "serde")]
#[allow(dead_code)]
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
// Consumed by the scalar evaluator landing in a later milestone (M3).
#[allow(dead_code)]
pub(crate) const SCALAR_MAX_DEPTH: usize = 1024;

/// The documented neutral confidence constant used by
/// [`MissingConfidencePolicy::Neutral`] and as the flagged fallback for a node
/// whose entire contributing child set was excluded.
// Consumed by the scalar evaluator landing in a later milestone (M3).
#[allow(dead_code)]
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

/// Options bounding a [`trust_breakdown`](crate::AletheiaDB::trust_breakdown)
/// walk and an optional `AS OF` transaction-time coordinate.
#[derive(Debug, Clone, Copy)]
pub struct TrustOptions {
    /// Maximum transitive depth to expand before marking a subtree truncated.
    pub max_depth: usize,
    /// Maximum number of breakdown nodes to serialize before truncation.
    pub max_nodes: usize,
    /// Optional `AS OF` transaction-time bound: evaluate lineage and
    /// confidences as recorded by this transaction time (AC5). `None` = now.
    pub as_of: Option<Timestamp>,
}

impl TrustOptions {
    /// Options with the default depth and node caps and no `AS OF` bound.
    pub fn new() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
            max_nodes: DEFAULT_MAX_NODES,
            as_of: None,
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
}

impl Default for TrustOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Clamp a value into `[0.0, 1.0]`, mapping `NaN` to `0.0`.
///
/// Used on every confidence value fed into or out of a combinator so a
/// malformed provenance value can never produce an out-of-range or `NaN`
/// computed confidence.
// Consumed by the scalar evaluator landing in a later milestone (M3).
#[allow(dead_code)]
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
// Consumed by the scalar evaluator landing in a later milestone (M3).
#[allow(dead_code)]
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
}
