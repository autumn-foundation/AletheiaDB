//! Declarative, file-based evaluation configuration (Issue #3366).
//!
//! Two layers:
//!
//! * [`RetrievalConfig`] — the four toggles that define *how* retrieval runs
//!   (`k`, `hybrid`, `temporal_anchoring`, `provenance_filter`) plus a couple
//!   of supporting knobs. The bundled `full.toml` and `baseline.toml` are each
//!   one of these; the baseline is vector-only k-NN with graph/temporal/
//!   provenance features OFF (the "pgvector-equivalent"). A feature's eval
//!   impact is therefore a one-line diff between the two files.
//! * [`EvalConfig`] — the run manifest tying a dataset to a *paired* full and
//!   baseline [`RetrievalConfig`] plus optional regression [`Gates`]. Every
//!   report the harness emits is a paired full-vs-baseline comparison.
//!
//! All structs use `#[serde(deny_unknown_fields)]` so a misspelled key is a
//! hard, clearly-worded error rather than a silently-ignored setting.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How a single retrieval pass is configured.
///
/// The three feature toggles (`hybrid`, `temporal_anchoring`,
/// `provenance_filter`) are required with no default, so an incomplete config
/// fails loudly instead of silently assuming a mode. The baseline flips all
/// three off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalConfig {
    /// Top-`k` cutoff for vector retrieval and the `@k` metrics.
    pub k: usize,
    /// Enable graph-traversal expansion of the vector seed(s). When off,
    /// retrieval is pure vector k-NN (the pgvector-equivalent baseline).
    pub hybrid: bool,
    /// Reconstruct retrieved facts AS OF each question's time anchor. When
    /// off, facts are read at current state regardless of the anchor.
    pub temporal_anchoring: bool,
    /// Drop retrieved items whose provenance source is not in
    /// [`trusted_sources`](Self::trusted_sources). When off, provenance is
    /// ignored.
    pub provenance_filter: bool,
    /// Maximum traversal depth when `hybrid` is on (default 2). Multi-hop gold
    /// evidence needs at least 2.
    #[serde(default = "default_max_hops")]
    pub max_hops: usize,
    /// Provenance sources considered trustworthy when `provenance_filter` is
    /// on. Ignored otherwise.
    #[serde(default)]
    pub trusted_sources: Vec<String>,
    /// Seed for the deterministic embedding vectorizer. Part of the
    /// reproducibility triple `(dataset version, config, seed)`.
    #[serde(default = "default_seed")]
    pub seed: u64,
}

fn default_max_hops() -> usize {
    2
}

fn default_seed() -> u64 {
    42
}

/// Regression gates: a breach makes the harness exit non-zero so CI can gate.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gates {
    /// Minimum acceptable `full.temporal_accuracy - baseline.temporal_accuracy`.
    /// The #3366 headline gate (>= 0.25).
    #[serde(default)]
    pub min_temporal_accuracy_delta: Option<f64>,
    /// Minimum acceptable full-config temporal accuracy.
    #[serde(default)]
    pub min_full_temporal_accuracy: Option<f64>,
    /// Minimum acceptable `full.recall_at_k - baseline.recall_at_k`.
    #[serde(default)]
    pub min_recall_delta: Option<f64>,
    /// Minimum acceptable full-config citation validity.
    #[serde(default)]
    pub min_full_citation_validity: Option<f64>,
}

/// Run manifest: a dataset paired with a full and baseline retrieval config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalConfig {
    /// Path to the dataset JSON, relative to the manifest file's directory.
    pub dataset: PathBuf,
    /// Path to the full-feature [`RetrievalConfig`] TOML.
    pub full: PathBuf,
    /// Path to the baseline (vector-only) [`RetrievalConfig`] TOML.
    pub baseline: PathBuf,
    /// Optional regression gates.
    #[serde(default)]
    pub gates: Gates,
}

/// Errors that can occur while loading a config or manifest.
#[derive(Debug)]
pub enum ConfigError {
    /// The file could not be read.
    Io {
        /// Path that failed to read.
        path: PathBuf,
        /// Underlying I/O error message.
        source: String,
    },
    /// The file could not be parsed as TOML.
    Parse {
        /// Path that failed to parse.
        path: PathBuf,
        /// Underlying parse error message (includes the offending key/field).
        source: String,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io { path, source } => {
                write!(f, "failed to read config '{}': {source}", path.display())
            }
            ConfigError::Parse { path, source } => {
                write!(f, "failed to parse config '{}': {source}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl RetrievalConfig {
    /// Parse a [`RetrievalConfig`] from a TOML string. Unknown or missing
    /// required fields produce a descriptive [`ConfigError::Parse`].
    pub fn from_toml_str(text: &str, path: &Path) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            source: e.to_string(),
        })
    }

    /// Load a [`RetrievalConfig`] from a TOML file.
    pub fn from_toml_file(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: e.to_string(),
        })?;
        Self::from_toml_str(&text, path)
    }
}

impl EvalConfig {
    /// Parse an [`EvalConfig`] manifest from a TOML string.
    pub fn from_toml_str(text: &str, path: &Path) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            source: e.to_string(),
        })
    }

    /// Load an [`EvalConfig`] manifest from a TOML file.
    pub fn from_toml_file(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: e.to_string(),
        })?;
        Self::from_toml_str(&text, path)
    }

    /// Resolve `dataset`/`full`/`baseline` against the manifest's own
    /// directory so relative paths in the manifest work from any CWD.
    #[must_use]
    pub fn resolve_relative_to(&self, manifest_dir: &Path) -> ResolvedPaths {
        ResolvedPaths {
            dataset: manifest_dir.join(&self.dataset),
            full: manifest_dir.join(&self.full),
            baseline: manifest_dir.join(&self.baseline),
        }
    }
}

/// Manifest paths resolved against the manifest's directory.
#[derive(Debug, Clone)]
pub struct ResolvedPaths {
    /// Absolute (or CWD-relative) path to the dataset JSON.
    pub dataset: PathBuf,
    /// Path to the full retrieval config.
    pub full: PathBuf,
    /// Path to the baseline retrieval config.
    pub baseline: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("test.toml")
    }

    #[test]
    fn parses_full_config() {
        let text = r#"
            k = 5
            hybrid = true
            temporal_anchoring = true
            provenance_filter = true
            trusted_sources = ["curated"]
            seed = 42
        "#;
        let cfg = RetrievalConfig::from_toml_str(text, &p()).unwrap();
        assert_eq!(cfg.k, 5);
        assert!(cfg.hybrid);
        assert!(cfg.temporal_anchoring);
        assert_eq!(cfg.max_hops, 2); // default applied
        assert_eq!(cfg.trusted_sources, vec!["curated".to_string()]);
    }

    #[test]
    fn baseline_toggles_off() {
        let text = r#"
            k = 5
            hybrid = false
            temporal_anchoring = false
            provenance_filter = false
        "#;
        let cfg = RetrievalConfig::from_toml_str(text, &p()).unwrap();
        assert!(!cfg.hybrid);
        assert!(!cfg.temporal_anchoring);
        assert!(!cfg.provenance_filter);
        assert_eq!(cfg.seed, 42); // default
    }

    #[test]
    fn unknown_field_is_a_clear_error() {
        let text = r#"
            k = 5
            hybrid = true
            temporal_anchoring = true
            provenance_filter = true
            bogus_knob = 99
        "#;
        let err = RetrievalConfig::from_toml_str(text, &p()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bogus_knob"), "message was: {msg}");
    }

    #[test]
    fn missing_required_field_is_a_clear_error() {
        // `hybrid` omitted.
        let text = r#"
            k = 5
            temporal_anchoring = true
            provenance_filter = true
        "#;
        let err = RetrievalConfig::from_toml_str(text, &p()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("hybrid"), "message was: {msg}");
    }

    #[test]
    fn parses_eval_manifest() {
        let text = r#"
            dataset = "dataset.json"
            full = "full.toml"
            baseline = "baseline.toml"
            [gates]
            min_temporal_accuracy_delta = 0.25
        "#;
        let cfg = EvalConfig::from_toml_str(text, &p()).unwrap();
        assert_eq!(cfg.dataset, PathBuf::from("dataset.json"));
        assert_eq!(cfg.gates.min_temporal_accuracy_delta, Some(0.25));
    }
}
