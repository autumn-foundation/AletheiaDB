//! Write-time attributive provenance for versions (Issue #3224).
//!
//! A [`Provenance`] bundle records *who/what* wrote a fact, *why*, and *how
//! confident* the writer was — complementing the bi-temporal axes ([`crate::core::temporal`])
//! that already record *when* a fact was valid and *when* it was recorded.
//!
//! Provenance is optional and attaches at the version granularity: every
//! historical version of a node or edge may carry its own bundle, distinct
//! from the versions before and after it.

use thiserror::Error;

/// Errors that can occur when constructing a [`Provenance`] bundle.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ProvenanceError {
    /// Confidence was outside the valid `[0.0, 1.0]` range (or NaN).
    #[error("confidence must be between 0.0 and 1.0, got {confidence}")]
    InvalidConfidence {
        /// The out-of-range (or NaN) confidence value that was rejected.
        confidence: f64,
    },
}

/// Write-time attributive provenance for a single version.
///
/// All fields are individually optional — a caller may supply only a
/// `source`, only a `confidence`, or any combination. An entirely empty
/// bundle (all fields `None`) is not distinguishable from "no provenance
/// supplied"; see [`Provenance::is_empty`].
///
/// Constructed via [`Provenance::builder`], which validates `confidence`
/// against `[0.0, 1.0]` (NaN is rejected).
///
/// Serde support is feature-gated behind the unified `serde` flag (Issue
/// #3390), which is pulled in by any serde-enabling feature — e.g.
/// `config-toml` (a default feature) and `mcp-server`, whose response types
/// embed [`Provenance`] directly; `--no-default-features` builds must still
/// compile without it.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "ProvenanceRaw")
)]
pub struct Provenance {
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    source: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    confidence: Option<f64>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    note: Option<String>,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    correlation_id: Option<String>,
    /// The authenticated principal (API-key identity) that made this write,
    /// if the write arrived through an authenticated server surface
    /// (Issue #3350). Server-stamped -- never caller-supplied -- and
    /// composes with `source` (which remains whatever the caller declared).
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    principal: Option<String>,
}

impl Provenance {
    /// Start building a new [`Provenance`] bundle.
    pub fn builder() -> ProvenanceBuilder {
        ProvenanceBuilder::default()
    }

    /// The source system/identifier that produced this write, if any.
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    /// The writer's confidence in this fact, in `[0.0, 1.0]`, if any.
    pub fn confidence(&self) -> Option<f64> {
        self.confidence
    }

    /// Free-text explanation of the write, if any.
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Correlation ID grouping all writes made in one logical operation, if any.
    pub fn correlation_id(&self) -> Option<&str> {
        self.correlation_id.as_deref()
    }

    /// The authenticated principal (API-key name) that made this write, if
    /// the write arrived through an authenticated server surface
    /// (Issue #3350).
    ///
    /// Unlike the other fields, this is stamped server-side from the
    /// verified session/request credential -- it is never accepted from the
    /// caller's provenance payload, so it cannot be forged through the MCP
    /// or HTTP surfaces. `None` for anonymous-mode writes and for writes
    /// made through the embedded Rust API.
    pub fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }

    /// Returns `true` if every field is absent.
    ///
    /// Used at API boundaries to normalize an all-`None` bundle to "no
    /// provenance" (`Option<Provenance>::None`), so callers never observe a
    /// fabricated empty object.
    pub fn is_empty(&self) -> bool {
        self.source.is_none()
            && self.confidence.is_none()
            && self.note.is_none()
            && self.correlation_id.is_none()
            && self.principal.is_none()
    }

    /// Construct and validate a [`Provenance`] bundle from five independently
    /// optional fields.
    ///
    /// This is the single shared implementation of "feed whichever fields are
    /// present through [`Provenance::builder`]" -- every storage tier that
    /// restores a persisted provenance record (WAL replay, index-persistence
    /// temporal index, Redb cold storage) and the MCP layer that parses a
    /// caller-supplied provenance request should call this instead of
    /// re-deriving the same builder chain, so a future field addition or
    /// validation change only needs to happen in one place.
    ///
    /// # Errors
    ///
    /// Returns [`ProvenanceError::InvalidConfidence`] if `confidence` is
    /// `Some` and outside `[0.0, 1.0]` or is NaN.
    pub fn from_parts(
        source: Option<String>,
        confidence: Option<f64>,
        note: Option<String>,
        correlation_id: Option<String>,
        principal: Option<String>,
    ) -> Result<Provenance, ProvenanceError> {
        let mut builder = ProvenanceBuilder::default();
        if let Some(source) = source {
            builder = builder.source(source);
        }
        if let Some(confidence) = confidence {
            builder = builder.confidence(confidence);
        }
        if let Some(note) = note {
            builder = builder.note(note);
        }
        if let Some(correlation_id) = correlation_id {
            builder = builder.correlation_id(correlation_id);
        }
        if let Some(principal) = principal {
            builder = builder.principal(principal);
        }
        builder.build()
    }

    /// Return a copy of this bundle with `principal` set to the given
    /// authenticated identity (Issue #3350).
    ///
    /// Used by server surfaces to stamp the verified principal onto a
    /// caller-supplied bundle without disturbing the caller's `source` /
    /// `confidence` / `note` / `correlation_id`. No validation is required
    /// (the principal name is server-verified, not caller data).
    #[must_use]
    pub fn with_principal<S: Into<String>>(mut self, principal: S) -> Self {
        self.principal = Some(principal.into());
        self
    }
}

/// Builder for [`Provenance`], validating `confidence` on [`build`](ProvenanceBuilder::build).
#[derive(Debug, Clone, Default)]
pub struct ProvenanceBuilder {
    source: Option<String>,
    confidence: Option<f64>,
    note: Option<String>,
    correlation_id: Option<String>,
    principal: Option<String>,
}

impl ProvenanceBuilder {
    /// Set the source system/identifier.
    pub fn source<S: Into<String>>(mut self, source: S) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Set the writer's confidence. Validated on [`build`](Self::build).
    pub fn confidence(mut self, confidence: f64) -> Self {
        self.confidence = Some(confidence);
        self
    }

    /// Set a free-text note.
    pub fn note<S: Into<String>>(mut self, note: S) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Set the correlation ID.
    pub fn correlation_id<S: Into<String>>(mut self, correlation_id: S) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }

    /// Set the authenticated principal that made this write (Issue #3350).
    ///
    /// Server surfaces stamp this from the verified credential; it should
    /// never be populated from caller-supplied provenance payloads.
    pub fn principal<S: Into<String>>(mut self, principal: S) -> Self {
        self.principal = Some(principal.into());
        self
    }

    /// Validate and construct the [`Provenance`] bundle.
    ///
    /// # Errors
    ///
    /// Returns [`ProvenanceError::InvalidConfidence`] if `confidence` was set
    /// and is outside `[0.0, 1.0]` or is NaN.
    pub fn build(self) -> Result<Provenance, ProvenanceError> {
        if let Some(confidence) = self.confidence
            && !(0.0..=1.0).contains(&confidence)
        {
            return Err(ProvenanceError::InvalidConfidence { confidence });
        }
        Ok(Provenance {
            source: self.source,
            confidence: self.confidence,
            note: self.note,
            correlation_id: self.correlation_id,
            principal: self.principal,
        })
    }
}

/// Unvalidated wire representation used only as the `serde` deserialization
/// target; [`Provenance`] itself has private fields so it cannot be
/// deserialized directly without going through validation.
#[cfg(feature = "serde")]
#[derive(Debug, serde::Deserialize)]
struct ProvenanceRaw {
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    correlation_id: Option<String>,
    #[serde(default)]
    principal: Option<String>,
}

#[cfg(feature = "serde")]
impl TryFrom<ProvenanceRaw> for Provenance {
    type Error = ProvenanceError;

    fn try_from(raw: ProvenanceRaw) -> Result<Self, Self::Error> {
        Provenance::from_parts(
            raw.source,
            raw.confidence,
            raw.note,
            raw.correlation_id,
            raw.principal,
        )
    }
}

// ============================================================================
// Provenance filtering (Issue #3348)
// ============================================================================

/// Errors returned when constructing a [`ProvenanceFilter`] from
/// caller-supplied values.
///
/// Each variant names the offending field so a server surface can surface it
/// verbatim in a structured `INVALID_ARGUMENT` payload (Issue #3234 / #3348
/// AC5) rather than returning a silently-empty result set.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ProvenanceFilterError {
    /// `min_confidence` was NaN or outside the inclusive `[0.0, 1.0]` range.
    #[error("min_confidence must be between 0.0 and 1.0, got {value}")]
    InvalidMinConfidence {
        /// The rejected value (may be NaN).
        value: f64,
    },
    /// A source list was supplied but was empty (`[]`), which can never match
    /// anything and is almost certainly a caller mistake — rejected loudly
    /// rather than silently returning nothing.
    #[error("provenance source list must not be empty")]
    EmptySourceList,
}

impl ProvenanceFilterError {
    /// The name of the request field that caused the rejection, for a
    /// structured error `details` payload.
    pub fn field(&self) -> &'static str {
        match self {
            ProvenanceFilterError::InvalidMinConfidence { .. } => "min_confidence",
            ProvenanceFilterError::EmptySourceList => "provenance_sources",
        }
    }
}

/// A reusable, surface-agnostic predicate that filters facts by their
/// write-time [`Provenance`] (Issue #3348).
///
/// Constructed once from validated inputs via [`ProvenanceFilter::validated`]
/// and then applied per *version* with [`ProvenanceFilter::matches`]. Because
/// the predicate operates on an `Option<&Provenance>` — the exact bundle
/// recorded on the version being returned — it composes for free with
/// bi-temporal reads: a point-in-time read hands the historical version's own
/// bundle, so the filter evaluates provenance *as recorded at that coordinate*
/// (AC3), never against the latest version.
///
/// This type is deliberately feature-independent (it lives next to
/// [`Provenance`] in always-compiled `core`) so the imperative MCP and HTTP
/// surfaces (Issue #3348) and a future declarative query-language lowering
/// (Issue #3354) share one implementation of the semantics.
///
/// # Semantics
///
/// - **Sources** (`sources`): any-of exact string match. `Some([...])` keeps a
///   version iff its `source` is present and equals one of the listed values.
/// - **Minimum confidence** (`min_confidence`): an **inclusive** lower bound.
///   `Some(t)` keeps a version iff its `confidence` is present and `>= t`.
/// - **Combined**: source and confidence constraints are ANDed.
/// - **Unattributed** versions (no bundle at all, `prov.is_none()`) are
///   **excluded** whenever the filter is active, unless `include_unattributed`
///   is `true`.
/// - A **partial** bundle (present, but missing the queried field) is treated
///   as *attributed*: it fails the constraint on the missing field and is
///   **not** rescued by `include_unattributed` (which concerns only the
///   no-bundle case).
/// - An **inactive** filter (no source and no confidence constraint) matches
///   everything, so omitting the filter is byte-identical to pre-#3348
///   behavior (AC2).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProvenanceFilter {
    sources: Option<Vec<String>>,
    min_confidence: Option<f64>,
    include_unattributed: bool,
}

impl ProvenanceFilter {
    /// Validate caller-supplied filter inputs and build a filter.
    ///
    /// Returns `Ok(None)` when there is no active constraint (no sources and
    /// no minimum confidence) so callers can cheaply skip all filtering and
    /// preserve byte-identical legacy behavior. `include_unattributed` on its
    /// own does not activate a filter (it only modulates an otherwise-active
    /// one).
    ///
    /// Merges the scalar `source` and the list `sources` into a single
    /// any-of set (their union), de-duplicated while preserving order.
    ///
    /// # Errors
    ///
    /// - [`ProvenanceFilterError::InvalidMinConfidence`] if `min_confidence`
    ///   is NaN or outside `[0.0, 1.0]`.
    /// - [`ProvenanceFilterError::EmptySourceList`] if a source list is
    ///   supplied but empty.
    pub fn validated(
        source: Option<String>,
        sources: Option<Vec<String>>,
        min_confidence: Option<f64>,
        include_unattributed: bool,
    ) -> Result<Option<ProvenanceFilter>, ProvenanceFilterError> {
        // An explicitly-supplied but empty list can never match; reject it
        // rather than silently returning nothing (AC5).
        if let Some(list) = &sources
            && list.is_empty()
        {
            return Err(ProvenanceFilterError::EmptySourceList);
        }

        if let Some(c) = min_confidence
            && (c.is_nan() || !(0.0..=1.0).contains(&c))
        {
            return Err(ProvenanceFilterError::InvalidMinConfidence { value: c });
        }

        // Union scalar + list, de-duplicated, order-preserving.
        let mut merged: Vec<String> = Vec::new();
        for s in source.into_iter().chain(sources.into_iter().flatten()) {
            if !merged.contains(&s) {
                merged.push(s);
            }
        }
        let sources = if merged.is_empty() {
            None
        } else {
            Some(merged)
        };

        if sources.is_none() && min_confidence.is_none() {
            // No active constraint: caller should skip filtering entirely.
            return Ok(None);
        }

        Ok(Some(ProvenanceFilter {
            sources,
            min_confidence,
            include_unattributed,
        }))
    }

    /// Whether this filter imposes any constraint (always `true` for a filter
    /// returned by [`validated`](Self::validated), which returns `None`
    /// otherwise).
    pub fn is_active(&self) -> bool {
        self.sources.is_some() || self.min_confidence.is_some()
    }

    /// Evaluate the predicate against the provenance bundle recorded on a
    /// single version.
    ///
    /// `prov` is the `Option<Provenance>` for the *exact* version being
    /// considered (e.g. the historical version resolved by a point-in-time
    /// read), so the decision reflects provenance as recorded at that
    /// coordinate.
    pub fn matches(&self, prov: Option<&Provenance>) -> bool {
        if !self.is_active() {
            return true;
        }
        let Some(p) = prov else {
            // No bundle at all: kept only if the caller opted in.
            return self.include_unattributed;
        };
        if let Some(sources) = &self.sources {
            match p.source() {
                Some(s) if sources.iter().any(|want| want == s) => {}
                _ => return false,
            }
        }
        if let Some(min) = self.min_confidence {
            match p.confidence() {
                Some(c) if c >= min => {}
                _ => return false,
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provenance_builder_valid_all_fields() {
        let p = Provenance::builder()
            .source("hr-system")
            .confidence(0.95)
            .note("Verified by HR sync")
            .correlation_id("batch-2026-06")
            .build()
            .unwrap();

        assert_eq!(p.source(), Some("hr-system"));
        assert_eq!(p.confidence(), Some(0.95));
        assert_eq!(p.note(), Some("Verified by HR sync"));
        assert_eq!(p.correlation_id(), Some("batch-2026-06"));
        assert!(!p.is_empty());
    }

    #[test]
    fn test_provenance_confidence_above_one_rejected() {
        let err = Provenance::builder().confidence(1.5).build().unwrap_err();
        assert_eq!(err, ProvenanceError::InvalidConfidence { confidence: 1.5 });
    }

    #[test]
    fn test_provenance_confidence_below_zero_rejected() {
        let err = Provenance::builder().confidence(-0.1).build().unwrap_err();
        assert_eq!(err, ProvenanceError::InvalidConfidence { confidence: -0.1 });
    }

    #[test]
    fn test_provenance_confidence_nan_rejected() {
        let err = Provenance::builder()
            .confidence(f64::NAN)
            .build()
            .unwrap_err();
        assert!(
            matches!(err, ProvenanceError::InvalidConfidence { confidence } if confidence.is_nan())
        );
    }

    #[test]
    fn test_provenance_confidence_boundaries_accepted() {
        assert!(Provenance::builder().confidence(0.0).build().is_ok());
        assert!(Provenance::builder().confidence(1.0).build().is_ok());
    }

    #[test]
    fn test_provenance_empty_bundle_is_empty() {
        let p = Provenance::builder().build().unwrap();
        assert!(p.is_empty());
        assert_eq!(p.source(), None);
        assert_eq!(p.confidence(), None);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn test_provenance_json_omits_absent_fields() {
        let p = Provenance::builder().source("csv-import").build().unwrap();
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"source\":\"csv-import\""));
        assert!(!json.contains("confidence"));
        assert!(!json.contains("note"));
        assert!(!json.contains("correlation_id"));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn test_provenance_deserialize_valid_round_trips() {
        let json = r#"{"source":"claude-mcp","confidence":0.8}"#;
        let p: Provenance = serde_json::from_str(json).unwrap();
        assert_eq!(p.source(), Some("claude-mcp"));
        assert_eq!(p.confidence(), Some(0.8));
    }

    #[test]
    fn test_provenance_principal_field_composes_with_source() {
        let p = Provenance::builder()
            .source("hr-system")
            .build()
            .unwrap()
            .with_principal("ingest-writer");
        assert_eq!(p.source(), Some("hr-system"));
        assert_eq!(p.principal(), Some("ingest-writer"));
        assert!(!p.is_empty());
    }

    #[test]
    fn test_provenance_principal_only_bundle_is_not_empty() {
        let p = Provenance::builder()
            .principal("ingest-writer")
            .build()
            .unwrap();
        assert!(!p.is_empty());
        assert_eq!(p.principal(), Some("ingest-writer"));
        assert_eq!(p.source(), None);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn test_provenance_principal_json_round_trip_and_omission() {
        // Absent principal is omitted from JSON entirely.
        let p = Provenance::builder().source("csv-import").build().unwrap();
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("principal"));

        // Present principal round-trips; pre-#3350 JSON (no principal key)
        // still deserializes.
        let stamped = p.with_principal("alice-key");
        let json = serde_json::to_string(&stamped).unwrap();
        assert!(json.contains("\"principal\":\"alice-key\""));
        let back: Provenance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.principal(), Some("alice-key"));
        let legacy: Provenance = serde_json::from_str(r#"{"source":"csv-import"}"#).unwrap();
        assert_eq!(legacy.principal(), None);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn test_provenance_deserialize_rejects_invalid_confidence() {
        let json = r#"{"confidence":2.0}"#;
        let result: Result<Provenance, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    // ProvenanceFilter (Issue #3348)
    // ------------------------------------------------------------------

    fn prov(source: Option<&str>, confidence: Option<f64>) -> Provenance {
        Provenance::from_parts(source.map(String::from), confidence, None, None, None).unwrap()
    }

    #[test]
    fn filter_no_constraint_returns_none() {
        assert_eq!(
            ProvenanceFilter::validated(None, None, None, false),
            Ok(None)
        );
        // include_unattributed alone does not activate a filter.
        assert_eq!(
            ProvenanceFilter::validated(None, None, None, true),
            Ok(None)
        );
    }

    #[test]
    fn filter_source_exact_and_any_of() {
        let f = ProvenanceFilter::validated(Some("crm".into()), None, None, false)
            .unwrap()
            .unwrap();
        assert!(f.matches(Some(&prov(Some("crm"), None))));
        assert!(!f.matches(Some(&prov(Some("hr"), None))));
        // Any-of via merged scalar + list.
        let f =
            ProvenanceFilter::validated(Some("crm".into()), Some(vec!["hr".into()]), None, false)
                .unwrap()
                .unwrap();
        assert!(f.matches(Some(&prov(Some("crm"), None))));
        assert!(f.matches(Some(&prov(Some("hr"), None))));
        assert!(!f.matches(Some(&prov(Some("other"), None))));
    }

    #[test]
    fn filter_min_confidence_is_inclusive() {
        let f = ProvenanceFilter::validated(None, None, Some(0.8), false)
            .unwrap()
            .unwrap();
        assert!(f.matches(Some(&prov(None, Some(0.8)))), "== bound included");
        assert!(f.matches(Some(&prov(None, Some(0.9)))));
        assert!(!f.matches(Some(&prov(None, Some(0.79)))));
        // Missing confidence fails the constraint.
        assert!(!f.matches(Some(&prov(Some("crm"), None))));
    }

    #[test]
    fn filter_unattributed_excluded_unless_opted_in() {
        let f = ProvenanceFilter::validated(None, None, Some(0.5), false)
            .unwrap()
            .unwrap();
        assert!(!f.matches(None), "no bundle excluded by default");
        let f = ProvenanceFilter::validated(None, None, Some(0.5), true)
            .unwrap()
            .unwrap();
        assert!(
            f.matches(None),
            "include_unattributed re-includes no-bundle"
        );
        // But a partial bundle is attributed and still fails the field.
        assert!(!f.matches(Some(&prov(Some("crm"), None))));
    }

    #[test]
    fn filter_combined_source_and_confidence_is_and() {
        let f = ProvenanceFilter::validated(Some("crm".into()), None, Some(0.8), false)
            .unwrap()
            .unwrap();
        assert!(f.matches(Some(&prov(Some("crm"), Some(0.9)))));
        assert!(
            !f.matches(Some(&prov(Some("crm"), Some(0.5)))),
            "conf fails"
        );
        assert!(
            !f.matches(Some(&prov(Some("hr"), Some(0.9)))),
            "source fails"
        );
    }

    #[test]
    fn filter_rejects_invalid_inputs() {
        for bad in [1.5_f64, -0.1, f64::NAN] {
            let err = ProvenanceFilter::validated(None, None, Some(bad), false).unwrap_err();
            assert_eq!(err.field(), "min_confidence");
        }
        let err = ProvenanceFilter::validated(None, Some(vec![]), None, false).unwrap_err();
        assert_eq!(err, ProvenanceFilterError::EmptySourceList);
        assert_eq!(err.field(), "provenance_sources");
    }

    #[test]
    fn filter_source_dedup_preserves_order() {
        let f = ProvenanceFilter::validated(
            Some("crm".into()),
            Some(vec!["crm".into(), "hr".into()]),
            None,
            false,
        )
        .unwrap()
        .unwrap();
        // Duplicate "crm" merged; both still match.
        assert!(f.matches(Some(&prov(Some("crm"), None))));
        assert!(f.matches(Some(&prov(Some("hr"), None))));
    }
}
