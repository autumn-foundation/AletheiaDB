//! Versioned gold-dataset schema and loader (Issue #3366).
//!
//! A dataset is committed JSON describing a small bi-temporal graph plus a set
//! of gold-labelled questions. The harness loads one, indexes it into an
//! in-memory [`AletheiaDB`](aletheiadb::AletheiaDB), and scores retrieval
//! against the gold labels.
//!
//! # Format
//!
//! ```json
//! {
//!   "version": "1.0.0",
//!   "name": "temporal_qa",
//!   "license": "CC0-1.0",
//!   "description": "...",
//!   "entities": [
//!     { "key": "acme", "label": "Company", "text": "Acme Corporation",
//!       "properties": { "name": "Acme" }, "source": "curated" }
//!   ],
//!   "updates": [
//!     { "entity": "acme", "valid_from": "2021-01-01", "properties": { "ceo": "Bob" } }
//!   ],
//!   "edges": [
//!     { "source": "alice", "target": "acme", "label": "WORKS_AT",
//!       "valid_from": "2020-01-01", "properties": {} }
//!   ],
//!   "questions": [
//!     { "id": "q1", "text": "Who was CEO of Acme in 2019?",
//!       "valid_time": "2019-06-01", "answer_property": "ceo",
//!       "gold_answer": "Alice", "seed_entity": "acme",
//!       "gold_evidence": ["acme"] }
//!   ]
//! }
//! ```
//!
//! Time anchors accept RFC 3339 (`2019-06-01T00:00:00Z`) or a bare date
//! (`2019-06-01`), parsed to second granularity.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A committed gold dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dataset {
    /// Semantic version of the dataset content. Part of the reproducibility
    /// triple `(dataset version, config, seed)`.
    pub version: String,
    /// Short machine name (e.g. `temporal_qa`).
    pub name: String,
    /// SPDX license identifier for the dataset content (bundled datasets are
    /// `CC0-1.0` synthetic data authored for this harness).
    pub license: String,
    /// Human description of what the dataset probes.
    #[serde(default)]
    pub description: String,
    /// Entities to create as nodes, in order.
    pub entities: Vec<Entity>,
    /// Point-in-time property updates applied after creation, in order, each
    /// at its own valid time. Models facts that change over valid time.
    #[serde(default)]
    pub updates: Vec<Update>,
    /// Directed edges between entities.
    #[serde(default)]
    pub edges: Vec<EdgeSpec>,
    /// Gold-labelled questions.
    pub questions: Vec<Question>,
}

/// An entity, created as a node carrying a synthetic embedding of `text`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entity {
    /// Stable string key referenced by edges/questions/updates.
    pub key: String,
    /// Node label.
    pub label: String,
    /// Free text embedded (deterministically) into the node's vector.
    pub text: String,
    /// Scalar properties (string/int/float/bool).
    #[serde(default)]
    pub properties: BTreeMap<String, ScalarValue>,
    /// Optional provenance source recorded on the write and used by the
    /// provenance filter.
    #[serde(default)]
    pub source: Option<String>,
    /// Optional valid-time start (RFC 3339 or bare date) for the initial
    /// version. Defaults to transaction time when absent.
    #[serde(default)]
    pub valid_from: Option<String>,
    /// Optional valid-time at which this entity is retracted (its valid
    /// interval closed) right after creation. Used to model a fact that was
    /// true only for a bounded valid-time era — e.g. a CEO tenure — so that a
    /// point-in-time (AS OF) query reconstructs the single fact valid at the
    /// anchor. See [`crate::harness`].
    #[serde(default)]
    pub retract_at: Option<String>,
}

/// A point-in-time property update to an existing entity (PATCH semantics).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    /// Key of the entity to update.
    pub entity: String,
    /// Valid time from which the new property values hold.
    pub valid_from: String,
    /// Properties to set/overwrite.
    pub properties: BTreeMap<String, ScalarValue>,
    /// Optional provenance source for this version.
    #[serde(default)]
    pub source: Option<String>,
}

/// A directed edge between two entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeSpec {
    /// Source entity key.
    pub source: String,
    /// Target entity key.
    pub target: String,
    /// Edge label.
    pub label: String,
    /// Optional valid-time start.
    #[serde(default)]
    pub valid_from: Option<String>,
    /// Scalar edge properties.
    #[serde(default)]
    pub properties: BTreeMap<String, ScalarValue>,
}

/// A gold-labelled question.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    /// Stable question id.
    pub id: String,
    /// Natural-language question text (embedded to seed vector retrieval).
    pub text: String,
    /// Entity keys that constitute correct supporting evidence.
    pub gold_evidence: Vec<String>,
    /// Optional time anchor (RFC 3339 or bare date). Present for temporal
    /// questions; absent for purely structural ones.
    #[serde(default)]
    pub valid_time: Option<String>,
    /// Property whose reconstructed value answers the question (e.g. `ceo`).
    /// Required together with `gold_answer` for temporal-accuracy scoring.
    #[serde(default)]
    pub answer_property: Option<String>,
    /// The correct value of `answer_property` at the anchor.
    #[serde(default)]
    pub gold_answer: Option<String>,
    /// Label of the fact node the temporal answer is resolved from (e.g.
    /// `Tenure`). When present, the harness resolves the answer with a
    /// point-in-time `find_nodes_by_property_at` on
    /// `(answer_label, answer_filter_key = answer_filter_value)` as of the
    /// anchor (full) or current state (baseline) — the temporal-retrieval
    /// mechanism the metric probes.
    #[serde(default)]
    pub answer_label: Option<String>,
    /// Property key used to select the answer fact node (e.g. `company`).
    #[serde(default)]
    pub answer_filter_key: Option<String>,
    /// Property value used to select the answer fact node (e.g. `Acme`).
    #[serde(default)]
    pub answer_filter_value: Option<String>,
    /// Entity the retriever should treat as the traversal seed (defaults to the
    /// nearest vector hit when absent).
    #[serde(default)]
    pub seed_entity: Option<String>,
}

/// A scalar JSON value usable as a node/edge property.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScalarValue {
    /// Boolean.
    Bool(bool),
    /// Signed integer.
    Int(i64),
    /// Floating point.
    Float(f64),
    /// String.
    Str(String),
}

/// Errors from loading a dataset file.
#[derive(Debug)]
pub enum DatasetError {
    /// The file could not be read.
    Io {
        /// Path that failed to read.
        path: PathBuf,
        /// I/O error message.
        source: String,
    },
    /// The file could not be parsed as JSON.
    Parse {
        /// Path that failed to parse.
        path: PathBuf,
        /// JSON parse error message.
        source: String,
    },
}

impl std::fmt::Display for DatasetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DatasetError::Io { path, source } => {
                write!(f, "failed to read dataset '{}': {source}", path.display())
            }
            DatasetError::Parse { path, source } => {
                write!(f, "failed to parse dataset '{}': {source}", path.display())
            }
        }
    }
}

impl std::error::Error for DatasetError {}

impl Dataset {
    /// Parse a dataset from a JSON string.
    pub fn from_json_str(text: &str, path: &Path) -> Result<Self, DatasetError> {
        serde_json::from_str(text).map_err(|e| DatasetError::Parse {
            path: path.to_path_buf(),
            source: e.to_string(),
        })
    }

    /// Load a dataset from a JSON file.
    pub fn from_json_file(path: &Path) -> Result<Self, DatasetError> {
        let text = std::fs::read_to_string(path).map_err(|e| DatasetError::Io {
            path: path.to_path_buf(),
            source: e.to_string(),
        })?;
        Self::from_json_str(&text, path)
    }

    /// Number of time-anchored questions (those with a `valid_time`).
    #[must_use]
    pub fn num_temporal_questions(&self) -> usize {
        self.questions
            .iter()
            .filter(|q| q.valid_time.is_some())
            .count()
    }
}

impl ScalarValue {
    /// Render this scalar as the plain string used for gold-answer comparison.
    #[must_use]
    pub fn as_answer_string(&self) -> String {
        match self {
            ScalarValue::Bool(b) => b.to_string(),
            ScalarValue::Int(i) => i.to_string(),
            ScalarValue::Float(f) => f.to_string(),
            ScalarValue::Str(s) => s.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_minimal_dataset() {
        let text = r#"
        {
          "version": "1.0.0",
          "name": "demo",
          "license": "CC0-1.0",
          "entities": [
            {"key": "acme", "label": "Company", "text": "Acme",
             "properties": {"ceo": "Alice"}, "source": "curated"}
          ],
          "questions": [
            {"id": "q1", "text": "Who runs Acme?", "gold_evidence": ["acme"]}
          ]
        }
        "#;
        let ds = Dataset::from_json_str(text, &PathBuf::from("d.json")).unwrap();
        assert_eq!(ds.name, "demo");
        assert_eq!(ds.entities.len(), 1);
        assert_eq!(ds.num_temporal_questions(), 0);
        assert_eq!(
            ds.entities[0].properties.get("ceo"),
            Some(&ScalarValue::Str("Alice".to_string()))
        );
    }

    #[test]
    fn unknown_field_rejected() {
        let text = r#"
        {
          "version": "1", "name": "d", "license": "CC0-1.0",
          "entities": [], "questions": [],
          "surprise": true
        }
        "#;
        let err = Dataset::from_json_str(text, &PathBuf::from("d.json")).unwrap_err();
        assert!(err.to_string().contains("surprise") || err.to_string().contains("unknown"));
    }

    #[test]
    fn scalar_answer_strings() {
        assert_eq!(ScalarValue::Int(3).as_answer_string(), "3");
        assert_eq!(ScalarValue::Str("x".into()).as_answer_string(), "x");
        assert_eq!(ScalarValue::Bool(true).as_answer_string(), "true");
    }
}
