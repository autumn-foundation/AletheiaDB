//! Provenance-weighted retrieval fusion (Issue #3372).
//!
//! Pure, deterministic, monotone re-scoring of vector-similarity candidates that
//! fuses three already-recorded signals into one tunable, explainable ranking:
//!
//! 1. **similarity** — the vector index score, normalized to `[0, 1]`;
//! 2. **confidence** — [`Provenance::confidence`](crate::core::provenance::Provenance::confidence),
//!    or a configurable *neutral* default when a candidate has no recorded
//!    provenance (never a silent exclusion — see Issue #3372 AC5);
//! 3. **recency** — an exponential decay of the age of the fact's validity
//!    (`valid_from`) measured against a caller-supplied reference instant.
//!
//! The fused score is the **normalized weighted arithmetic mean**
//!
//! ```text
//! fused = (w_s·s + w_c·c + w_r·r) / (w_s + w_c + w_r)
//! ```
//!
//! with non-negative weights whose sum is positive (enforced at
//! [`FusionPolicyBuilder::build`]). Because `∂fused/∂x = w_x / Σw ≥ 0` for each
//! component `x ∈ {s, c, r}`, raising any one component (weights fixed) can never
//! lower the fused score — the monotonicity contract of AC1.
//!
//! Every result carries a full [`FusionBreakdown`] (AC3): the ranking is always
//! explainable, never a single opaque number.
//!
//! This is a **ranking** change over recorded inputs — no storage-format change.
//! It composes *after* the #3348 provenance hard filter (filter first, fuse the
//! survivors) and, at the Rust-API level, is entered through
//! [`AletheiaDB::similarity_search_fused`](crate::AletheiaDB::similarity_search_fused).
//!
//! Gated behind the experimental `semantic-retrieval-fusion` cohort flag
//! (ADR-0050); it graduates into `semantic-search` once the #3366 eval-harness
//! precision@10 gate is met.

use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::db::AletheiaDB;
use crate::db::similarity_query::SimilarityQuery;
use thiserror::Error;

/// Default neutral confidence assigned to a candidate with no recorded
/// provenance confidence (AC5). A neutral 0.5 neither rewards nor punishes an
/// un-attributed fact relative to the `[0, 1]` confidence scale.
pub const DEFAULT_NEUTRAL_CONFIDENCE: f64 = 0.5;

/// Default recency half-life in seconds (30 days): a fact whose validity began
/// one half-life ago scores `recency = 0.5`.
pub const DEFAULT_RECENCY_HALF_LIFE_SECS: f64 = 60.0 * 60.0 * 24.0 * 30.0;

/// How many candidates to over-fetch from the vector index before fusing.
///
/// Fusion must return the **true** fused top-k, not a re-sort of the
/// similarity-only shortlist (AC4). We therefore fetch a wide horizon (mirroring
/// the MCP `MAX_VECTOR_K` bound) so a high-trust candidate that is geometrically
/// far still enters the fused ranking. The guarantee holds within this horizon —
/// the same bounded caveat #3348 documents.
pub const FUSION_OVERFETCH_HORIZON: usize = 1000;

/// Validation error for a [`FusionPolicy`] (AC7).
///
/// At the Rust-API level these are returned by [`FusionPolicyBuilder::build`];
/// the (deferred) MCP surface maps them to `INVALID_ARGUMENT` (#3234,
/// `retriable: false`).
#[derive(Debug, Clone, PartialEq, Error)]
pub enum FusionPolicyError {
    /// A weight was negative, infinite, or NaN.
    #[error("fusion weight `{field}` must be finite and non-negative, got {value}")]
    InvalidWeight {
        /// Which weight was invalid (`w_similarity` / `w_confidence` / `w_recency`).
        field: &'static str,
        /// The rejected value.
        value: f64,
    },
    /// Every weight was zero, leaving the weighted mean undefined.
    #[error("at least one fusion weight must be positive (all weights were zero)")]
    AllZeroWeights,
    /// `neutral_confidence` was outside `[0.0, 1.0]` or non-finite.
    #[error("neutral_confidence must be finite and within [0.0, 1.0], got {value}")]
    InvalidNeutralConfidence {
        /// The rejected value.
        value: f64,
    },
    /// `recency_half_life_secs` was non-positive or non-finite.
    #[error("recency_half_life_secs must be finite and positive, got {value}")]
    InvalidHalfLife {
        /// The rejected value.
        value: f64,
    },
}

/// A validated fusion scoring model (see the [module docs](self)).
///
/// Construct via [`FusionPolicy::builder`]; the builder validates all fields, so
/// a `FusionPolicy` value is always well-formed (weights finite/non-negative
/// with a positive sum, `neutral_confidence ∈ [0,1]`, positive half-life).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct FusionPolicy {
    w_similarity: f64,
    w_confidence: f64,
    w_recency: f64,
    neutral_confidence: f64,
    recency_half_life_secs: f64,
}

/// Builder for [`FusionPolicy`]. Defaults: all weights `1.0`,
/// `neutral_confidence = 0.5`, `recency_half_life_secs = 30 days`.
#[derive(Debug, Clone)]
pub struct FusionPolicyBuilder {
    w_similarity: f64,
    w_confidence: f64,
    w_recency: f64,
    neutral_confidence: f64,
    recency_half_life_secs: f64,
}

impl Default for FusionPolicyBuilder {
    fn default() -> Self {
        Self {
            w_similarity: 1.0,
            w_confidence: 1.0,
            w_recency: 1.0,
            neutral_confidence: DEFAULT_NEUTRAL_CONFIDENCE,
            recency_half_life_secs: DEFAULT_RECENCY_HALF_LIFE_SECS,
        }
    }
}

impl FusionPolicyBuilder {
    /// Set the vector-similarity weight (default `1.0`).
    #[must_use]
    pub fn w_similarity(mut self, w: f64) -> Self {
        self.w_similarity = w;
        self
    }

    /// Set the provenance-confidence weight (default `1.0`).
    #[must_use]
    pub fn w_confidence(mut self, w: f64) -> Self {
        self.w_confidence = w;
        self
    }

    /// Set the temporal-recency weight (default `1.0`).
    #[must_use]
    pub fn w_recency(mut self, w: f64) -> Self {
        self.w_recency = w;
        self
    }

    /// Set the neutral confidence used for candidates with no recorded
    /// provenance confidence (default [`DEFAULT_NEUTRAL_CONFIDENCE`]).
    #[must_use]
    pub fn neutral_confidence(mut self, c: f64) -> Self {
        self.neutral_confidence = c;
        self
    }

    /// Set the recency half-life in seconds (default
    /// [`DEFAULT_RECENCY_HALF_LIFE_SECS`]).
    #[must_use]
    pub fn recency_half_life_secs(mut self, secs: f64) -> Self {
        self.recency_half_life_secs = secs;
        self
    }

    /// Validate and build the [`FusionPolicy`] (AC7).
    pub fn build(self) -> std::result::Result<FusionPolicy, FusionPolicyError> {
        for (field, value) in [
            ("w_similarity", self.w_similarity),
            ("w_confidence", self.w_confidence),
            ("w_recency", self.w_recency),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(FusionPolicyError::InvalidWeight { field, value });
            }
        }
        if self.w_similarity + self.w_confidence + self.w_recency <= 0.0 {
            return Err(FusionPolicyError::AllZeroWeights);
        }
        if !self.neutral_confidence.is_finite() || !(0.0..=1.0).contains(&self.neutral_confidence) {
            return Err(FusionPolicyError::InvalidNeutralConfidence {
                value: self.neutral_confidence,
            });
        }
        if !self.recency_half_life_secs.is_finite() || self.recency_half_life_secs <= 0.0 {
            return Err(FusionPolicyError::InvalidHalfLife {
                value: self.recency_half_life_secs,
            });
        }
        Ok(FusionPolicy {
            w_similarity: self.w_similarity,
            w_confidence: self.w_confidence,
            w_recency: self.w_recency,
            neutral_confidence: self.neutral_confidence,
            recency_half_life_secs: self.recency_half_life_secs,
        })
    }
}

impl FusionPolicy {
    /// Start building a [`FusionPolicy`] from the documented defaults.
    pub fn builder() -> FusionPolicyBuilder {
        FusionPolicyBuilder::default()
    }

    /// The vector-similarity weight.
    pub fn w_similarity(&self) -> f64 {
        self.w_similarity
    }

    /// The provenance-confidence weight.
    pub fn w_confidence(&self) -> f64 {
        self.w_confidence
    }

    /// The temporal-recency weight.
    pub fn w_recency(&self) -> f64 {
        self.w_recency
    }

    /// The neutral confidence used when a candidate has no recorded confidence.
    pub fn neutral_confidence(&self) -> f64 {
        self.neutral_confidence
    }

    /// The recency half-life in seconds.
    pub fn recency_half_life_secs(&self) -> f64 {
        self.recency_half_life_secs
    }

    /// Compute the recency component `r ∈ (0, 1]` for a fact whose validity
    /// began at `valid_from_micros`, evaluated against `reference_now_micros`
    /// (both microseconds since the Unix epoch).
    ///
    /// `r = exp(-ln2 · age / half_life)` with `age = max(0, now − valid_from)`
    /// in seconds, so a future-dated fact (`valid_from > now`, #3221) is clamped
    /// to `age = 0` and therefore `r = 1`. Supplying an `AS OF` coordinate as
    /// `reference_now_micros` makes recency deterministic under replay (AC6).
    pub fn recency(&self, reference_now_micros: i64, valid_from_micros: i64) -> f64 {
        let age_micros = reference_now_micros
            .saturating_sub(valid_from_micros)
            .max(0);
        let age_secs = age_micros as f64 / 1_000_000.0;
        (-std::f64::consts::LN_2 * age_secs / self.recency_half_life_secs).exp()
    }

    /// Fuse the three components into a [`FusionBreakdown`].
    ///
    /// `similarity` and `recency` are clamped to `[0, 1]` (NaN → 0, keeping the
    /// downstream sort total). `confidence` is `Some(c)` when recorded (clamped
    /// to `[0, 1]`) or `None`, in which case `neutral_confidence` is substituted
    /// and `confidence_defaulted` is set (AC5).
    pub fn fuse(&self, similarity: f64, confidence: Option<f64>, recency: f64) -> FusionBreakdown {
        let s = clamp_unit(similarity);
        let r = clamp_unit(recency);
        let (c, confidence_defaulted) = match confidence {
            Some(v) if v.is_finite() => (v.clamp(0.0, 1.0), false),
            _ => (self.neutral_confidence, true),
        };
        // Σw > 0 is guaranteed by FusionPolicyBuilder::build.
        let weight_sum = self.w_similarity + self.w_confidence + self.w_recency;
        let fused =
            (self.w_similarity * s + self.w_confidence * c + self.w_recency * r) / weight_sum;
        FusionBreakdown {
            similarity: s,
            confidence: c,
            confidence_defaulted,
            recency: r,
            fused,
        }
    }
}

/// The per-result score breakdown (AC3): the fused score plus every component
/// that produced it, so the ranking is auditable and never a single opaque
/// number.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct FusionBreakdown {
    /// The normalized vector similarity component in `[0, 1]`.
    pub similarity: f64,
    /// The confidence component in `[0, 1]` (recorded or defaulted).
    pub confidence: f64,
    /// `true` iff `confidence` was substituted from the policy's
    /// `neutral_confidence` because the candidate had no recorded confidence.
    pub confidence_defaulted: bool,
    /// The recency component in `(0, 1]`.
    pub recency: f64,
    /// The fused score in `[0, 1]` — the normalized weighted mean.
    pub fused: f64,
}

/// A fused similarity hit: a node id paired with its full [`FusionBreakdown`].
#[derive(Debug, Clone, PartialEq)]
pub struct FusedHit {
    /// The matched node.
    pub node_id: NodeId,
    /// The score breakdown that ranked it.
    pub breakdown: FusionBreakdown,
}

impl FusedHit {
    /// Convenience accessor for the fused score.
    pub fn fused_score(&self) -> f64 {
        self.breakdown.fused
    }
}

/// Clamp a value to `[0, 1]`, mapping any non-finite input to `0.0` so the
/// fused-score sort is a total order.
fn clamp_unit(x: f64) -> f64 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

impl AletheiaDB {
    /// Provenance-weighted k-NN: return the **true** fused top-k under the
    /// query's [`FusionPolicy`] (Issue #3372).
    ///
    /// The query must carry a policy set with
    /// [`SimilarityQuery::fusion`](crate::SimilarityQuery::fusion); otherwise an
    /// error is returned. A wide horizon of candidates is over-fetched from the
    /// vector index (so a geometrically-far but high-trust candidate can still
    /// win — AC4), each candidate's confidence and validity are read from its
    /// current version, the fused score is computed, and the horizon is sorted
    /// by fused score (descending, with a stable node-id tie-break) before being
    /// truncated to `k`.
    ///
    /// Recency is evaluated against the query's `at_time` coordinate when set,
    /// else the current wallclock. Candidates with no recorded confidence are
    /// scored at the policy's `neutral_confidence`, never dropped (AC5).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn similarity_search_fused(&self, query: SimilarityQuery) -> Result<Vec<FusedHit>> {
        let policy = query.fusion_policy().cloned().ok_or_else(|| {
            Error::Vector(VectorError::IndexError(
                "similarity_search_fused requires a fusion policy; set one with \
                 SimilarityQuery::fusion(..)"
                    .to_string(),
            ))
        })?;

        let k = query.limit();
        // Recency is evaluated against the query's AS OF coordinate when set
        // (AC6), otherwise the current wallclock, captured once for the whole
        // request so every candidate is scored against the same instant.
        let reference_now = query
            .timestamp()
            .map(|ts| ts.wallclock())
            .unwrap_or_else(|| crate::core::temporal::time::now().wallclock());

        // Over-fetch a wide horizon so a geometrically-far but high-trust
        // candidate can still win (AC4). `similarity_search` ignores the fusion
        // policy, so re-using the query here is a pure similarity search.
        let horizon = k.max(FUSION_OVERFETCH_HORIZON);
        let candidates = self.similarity_search(query.k(horizon))?;

        let mut hits: Vec<FusedHit> = Vec::with_capacity(candidates.len());
        for (node_id, similarity) in candidates {
            let Some((confidence, valid_from_micros)) =
                self.node_fusion_metadata(node_id, reference_now)?
            else {
                // The node disappeared between the index read and the metadata
                // read; drop it rather than fabricating a score.
                continue;
            };
            let recency = policy.recency(reference_now, valid_from_micros);
            let breakdown = policy.fuse(f64::from(similarity), confidence, recency);
            hits.push(FusedHit { node_id, breakdown });
        }

        // Total-order sort by fused score (descending), with a stable node-id
        // tie-break so the ranking is deterministic.
        hits.sort_by(|a, b| {
            b.breakdown
                .fused
                .partial_cmp(&a.breakdown.fused)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.node_id.cmp(&b.node_id))
        });
        hits.truncate(k);
        Ok(hits)
    }

    /// Read a candidate node's confidence (if recorded) and validity start from
    /// its current version, for fusion scoring.
    ///
    /// Returns `Ok(None)` when the node no longer exists. When the version
    /// metadata cannot be loaded, recency is treated as "now" (`valid_from =
    /// reference_now`, i.e. recency 1) rather than dropping the candidate.
    fn node_fusion_metadata(
        &self,
        node_id: NodeId,
        reference_now: i64,
    ) -> Result<Option<(Option<f64>, i64)>> {
        let node = match self.get_node(node_id) {
            Ok(node) => node,
            Err(_) => return Ok(None),
        };
        match self.get_node_version_read_metadata(node.current_version)? {
            Some((provenance, interval)) => {
                let confidence = provenance.and_then(|p| p.confidence());
                let valid_from = interval.valid_time().start().wallclock();
                Ok(Some((confidence, valid_from)))
            }
            None => Ok(Some((None, reference_now))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MICROS_PER_SEC: i64 = 1_000_000;

    fn policy() -> FusionPolicy {
        FusionPolicy::builder()
            .build()
            .expect("default policy is valid")
    }

    // ---- AC7: invalid parameters rejected -------------------------------

    #[test]
    fn invalid_negative_weight_rejected() {
        let err = FusionPolicy::builder()
            .w_confidence(-0.1)
            .build()
            .unwrap_err();
        assert_eq!(
            err,
            FusionPolicyError::InvalidWeight {
                field: "w_confidence",
                value: -0.1
            }
        );
    }

    #[test]
    fn invalid_all_zero_weights_rejected() {
        let err = FusionPolicy::builder()
            .w_similarity(0.0)
            .w_confidence(0.0)
            .w_recency(0.0)
            .build()
            .unwrap_err();
        assert_eq!(err, FusionPolicyError::AllZeroWeights);
    }

    #[test]
    fn invalid_nan_weight_rejected() {
        let err = FusionPolicy::builder()
            .w_similarity(f64::NAN)
            .build()
            .unwrap_err();
        assert!(matches!(
            err,
            FusionPolicyError::InvalidWeight {
                field: "w_similarity",
                ..
            }
        ));
    }

    #[test]
    fn invalid_infinite_weight_rejected() {
        let err = FusionPolicy::builder()
            .w_recency(f64::INFINITY)
            .build()
            .unwrap_err();
        assert!(matches!(
            err,
            FusionPolicyError::InvalidWeight {
                field: "w_recency",
                ..
            }
        ));
    }

    #[test]
    fn invalid_neutral_confidence_rejected() {
        assert!(matches!(
            FusionPolicy::builder()
                .neutral_confidence(1.5)
                .build()
                .unwrap_err(),
            FusionPolicyError::InvalidNeutralConfidence { .. }
        ));
        assert!(matches!(
            FusionPolicy::builder()
                .neutral_confidence(-0.01)
                .build()
                .unwrap_err(),
            FusionPolicyError::InvalidNeutralConfidence { .. }
        ));
    }

    #[test]
    fn invalid_half_life_rejected() {
        assert!(matches!(
            FusionPolicy::builder()
                .recency_half_life_secs(0.0)
                .build()
                .unwrap_err(),
            FusionPolicyError::InvalidHalfLife { .. }
        ));
        assert!(matches!(
            FusionPolicy::builder()
                .recency_half_life_secs(-5.0)
                .build()
                .unwrap_err(),
            FusionPolicyError::InvalidHalfLife { .. }
        ));
    }

    #[test]
    fn valid_policy_builds_with_defaults() {
        let p = policy();
        assert_eq!(p.w_similarity(), 1.0);
        assert_eq!(p.w_confidence(), 1.0);
        assert_eq!(p.w_recency(), 1.0);
        assert_eq!(p.neutral_confidence(), DEFAULT_NEUTRAL_CONFIDENCE);
        assert_eq!(p.recency_half_life_secs(), DEFAULT_RECENCY_HALF_LIFE_SECS);
    }

    // ---- AC3: breakdown is complete and non-opaque ----------------------

    #[test]
    fn breakdown_is_complete_and_non_opaque() {
        let p = policy();
        let b = p.fuse(0.8, Some(0.6), 0.4);
        // Every component is exposed, not just the fused number.
        assert_eq!(b.similarity, 0.8);
        assert_eq!(b.confidence, 0.6);
        assert!(!b.confidence_defaulted);
        assert_eq!(b.recency, 0.4);
        // Fused equals the documented normalized weighted mean.
        let expected = (0.8 + 0.6 + 0.4) / 3.0;
        assert!((b.fused - expected).abs() < 1e-12);
    }

    #[test]
    fn fused_respects_weights() {
        // Confidence-only weighting => fused == confidence regardless of s/r.
        let p = FusionPolicy::builder()
            .w_similarity(0.0)
            .w_confidence(1.0)
            .w_recency(0.0)
            .build()
            .unwrap();
        let b = p.fuse(0.1, Some(0.9), 0.2);
        assert!((b.fused - 0.9).abs() < 1e-12);
    }

    // ---- AC5: missing provenance => neutral confidence, never dropped ----

    #[test]
    fn missing_provenance_uses_neutral_not_dropped() {
        let p = FusionPolicy::builder()
            .neutral_confidence(0.42)
            .build()
            .unwrap();
        let b = p.fuse(0.5, None, 0.5);
        assert_eq!(b.confidence, 0.42);
        assert!(b.confidence_defaulted);
    }

    #[test]
    fn nan_confidence_defaults_like_missing() {
        let p = policy();
        let b = p.fuse(0.5, Some(f64::NAN), 0.5);
        assert!(b.confidence_defaulted);
        assert_eq!(b.confidence, p.neutral_confidence());
    }

    // ---- Monotonicity (AC1) ---------------------------------------------

    #[test]
    fn monotonic_in_confidence() {
        let p = policy();
        let lo = p.fuse(0.5, Some(0.2), 0.5).fused;
        let hi = p.fuse(0.5, Some(0.9), 0.5).fused;
        assert!(hi >= lo, "raising confidence lowered fused: {hi} < {lo}");
    }

    #[test]
    fn monotonic_in_similarity() {
        let p = policy();
        let lo = p.fuse(0.1, Some(0.5), 0.5).fused;
        let hi = p.fuse(0.9, Some(0.5), 0.5).fused;
        assert!(hi >= lo);
    }

    #[test]
    fn monotonic_in_recency() {
        let p = policy();
        let lo = p.fuse(0.5, Some(0.5), 0.1).fused;
        let hi = p.fuse(0.5, Some(0.5), 0.9).fused;
        assert!(hi >= lo);
    }

    // ---- Determinism + total-order safety --------------------------------

    #[test]
    fn fuse_is_deterministic() {
        let p = policy();
        assert_eq!(p.fuse(0.3, Some(0.7), 0.5), p.fuse(0.3, Some(0.7), 0.5));
    }

    #[test]
    fn nan_similarity_clamped_to_zero() {
        let p = policy();
        let b = p.fuse(f64::NAN, Some(0.5), 0.5);
        assert_eq!(b.similarity, 0.0);
        assert!(b.fused.is_finite());
    }

    #[test]
    fn components_clamped_into_unit_interval() {
        let p = policy();
        let b = p.fuse(2.0, Some(3.0), -1.0);
        assert_eq!(b.similarity, 1.0);
        assert_eq!(b.confidence, 1.0);
        assert_eq!(b.recency, 0.0);
        assert!((0.0..=1.0).contains(&b.fused));
    }

    // ---- Recency semantics (AC6 core + risk cases) -----------------------

    #[test]
    fn recency_uses_supplied_reference_now() {
        let p = FusionPolicy::builder()
            .recency_half_life_secs(10.0)
            .build()
            .unwrap();
        let valid_from = 0_i64;
        // At the exact instant of validity, recency == 1.
        assert!((p.recency(0, valid_from) - 1.0).abs() < 1e-12);
        // One half-life later, recency == 0.5 — deterministic in the supplied now.
        let r = p.recency(10 * MICROS_PER_SEC, valid_from);
        assert!((r - 0.5).abs() < 1e-9, "expected ~0.5, got {r}");
        // Repeated evaluation is identical (determinism / replayability, AC6).
        assert_eq!(r, p.recency(10 * MICROS_PER_SEC, valid_from));
    }

    #[test]
    fn recency_decreases_with_age() {
        let p = policy();
        let now = 1_000_000_000 * MICROS_PER_SEC;
        let recent = p.recency(now, now - MICROS_PER_SEC);
        let old = p.recency(now, now - 100 * 24 * 3600 * MICROS_PER_SEC);
        assert!(recent > old);
        assert!(old > 0.0 && recent <= 1.0);
    }

    #[test]
    fn future_dated_validity_clamps_recency_to_one() {
        let p = policy();
        let now = 1_000 * MICROS_PER_SEC;
        // valid_from in the future => age clamped to 0 => recency == 1.
        let r = p.recency(now, now + 10_000 * MICROS_PER_SEC);
        assert!((r - 1.0).abs() < 1e-12);
    }
}
