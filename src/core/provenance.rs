//! Write-time attributive provenance for versions (Issue #3224).
//!
//! A [`Provenance`] bundle records *who/what* wrote a fact, *why*, and *how
//! confident* the writer was — complementing the bi-temporal axes ([`crate::core::temporal`])
//! that already record *when* a fact was valid and *when* it was recorded.
//!
//! Provenance is optional and attaches at the version granularity: every
//! historical version of a node or edge may carry its own bundle, distinct
//! from the versions before and after it.

use serde::{Deserialize, Serialize};
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ProvenanceRaw")]
pub struct Provenance {
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    correlation_id: Option<String>,
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
    }

    /// Construct and validate a [`Provenance`] bundle from four independently
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
        builder.build()
    }
}

/// Builder for [`Provenance`], validating `confidence` on [`build`](ProvenanceBuilder::build).
#[derive(Debug, Clone, Default)]
pub struct ProvenanceBuilder {
    source: Option<String>,
    confidence: Option<f64>,
    note: Option<String>,
    correlation_id: Option<String>,
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
        })
    }
}

/// Unvalidated wire representation used only as the `serde` deserialization
/// target; [`Provenance`] itself has private fields so it cannot be
/// deserialized directly without going through validation.
#[derive(Debug, Deserialize)]
struct ProvenanceRaw {
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    correlation_id: Option<String>,
}

impl TryFrom<ProvenanceRaw> for Provenance {
    type Error = ProvenanceError;

    fn try_from(raw: ProvenanceRaw) -> Result<Self, Self::Error> {
        Provenance::from_parts(raw.source, raw.confidence, raw.note, raw.correlation_id)
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

    #[test]
    fn test_provenance_json_omits_absent_fields() {
        let p = Provenance::builder().source("csv-import").build().unwrap();
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"source\":\"csv-import\""));
        assert!(!json.contains("confidence"));
        assert!(!json.contains("note"));
        assert!(!json.contains("correlation_id"));
    }

    #[test]
    fn test_provenance_deserialize_valid_round_trips() {
        let json = r#"{"source":"claude-mcp","confidence":0.8}"#;
        let p: Provenance = serde_json::from_str(json).unwrap();
        assert_eq!(p.source(), Some("claude-mcp"));
        assert_eq!(p.confidence(), Some(0.8));
    }

    #[test]
    fn test_provenance_deserialize_rejects_invalid_confidence() {
        let json = r#"{"confidence":2.0}"#;
        let result: Result<Provenance, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
