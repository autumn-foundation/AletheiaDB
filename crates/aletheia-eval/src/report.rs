//! Paired full-vs-baseline report, JSON schema, human summary, and gates
//! (Issue #3366).
//!
//! Every report the harness emits compares the full-feature config against the
//! vector-only baseline over the same dataset, computes the per-metric deltas,
//! and evaluates any regression [`Gates`](crate::config::Gates). The JSON is
//! deterministic (stable key order via `serde`, deterministic metric floats),
//! so `(dataset, config, seed)` reproduces byte-identical output.

use serde::Serialize;

use crate::config::{EvalConfig, Gates, RetrievalConfig};
use crate::harness::{RunMetrics, RunResult};

/// Machine-readable schema version of the report envelope.
pub const REPORT_SCHEMA_VERSION: &str = "1";

/// Snapshot of a [`RetrievalConfig`] as embedded in the report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigSnapshot {
    /// `k` cutoff.
    pub k: usize,
    /// Hybrid graph expansion on/off.
    pub hybrid: bool,
    /// Temporal anchoring on/off.
    pub temporal_anchoring: bool,
    /// Provenance filter on/off.
    pub provenance_filter: bool,
    /// Traversal depth.
    pub max_hops: usize,
    /// Embedding seed.
    pub seed: u64,
}

impl From<&RetrievalConfig> for ConfigSnapshot {
    fn from(c: &RetrievalConfig) -> Self {
        Self {
            k: c.k,
            hybrid: c.hybrid,
            temporal_anchoring: c.temporal_anchoring,
            provenance_filter: c.provenance_filter,
            max_hops: c.max_hops,
            seed: c.seed,
        }
    }
}

/// Per-metric full-minus-baseline deltas.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricDeltas {
    /// full − baseline precision@k.
    pub precision_at_k: f64,
    /// full − baseline recall@k.
    pub recall_at_k: f64,
    /// full − baseline grounding precision.
    pub grounding_precision: f64,
    /// full − baseline temporal accuracy (the #3366 headline).
    pub temporal_accuracy: f64,
    /// full − baseline citation validity.
    pub citation_validity: f64,
}

impl MetricDeltas {
    fn compute(full: &RunMetrics, baseline: &RunMetrics) -> Self {
        Self {
            precision_at_k: full.precision_at_k - baseline.precision_at_k,
            recall_at_k: full.recall_at_k - baseline.recall_at_k,
            grounding_precision: full.grounding_precision - baseline.grounding_precision,
            temporal_accuracy: full.temporal_accuracy - baseline.temporal_accuracy,
            citation_validity: full.citation_validity - baseline.citation_validity,
        }
    }
}

/// One side (full or baseline) of the paired comparison.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SideReport {
    /// The config used.
    pub config: ConfigSnapshot,
    /// Aggregate metrics.
    pub metrics: RunMetrics,
    /// Per-question breakdown.
    pub per_question: Vec<crate::harness::QuestionResult>,
}

/// Metadata identifying the dataset scored.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DatasetInfo {
    /// Dataset machine name.
    pub name: String,
    /// Dataset version.
    pub version: String,
    /// Dataset license.
    pub license: String,
    /// Number of questions.
    pub num_questions: usize,
}

/// The result of evaluating one gate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GateResult {
    /// Human name of the gate.
    pub name: String,
    /// Threshold that had to be met.
    pub threshold: f64,
    /// Observed value.
    pub observed: f64,
    /// Whether the gate passed.
    pub passed: bool,
}

/// The complete paired report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    /// Report schema version.
    pub schema_version: String,
    /// Dataset scored.
    pub dataset: DatasetInfo,
    /// Full-feature side.
    pub full: SideReport,
    /// Baseline (vector-only) side.
    pub baseline: SideReport,
    /// Full-minus-baseline deltas.
    pub deltas: MetricDeltas,
    /// Gate evaluations (empty when no gates configured).
    pub gates: Vec<GateResult>,
    /// Overall pass/fail: all gates passed (vacuously true with no gates).
    pub gates_passed: bool,
}

impl Report {
    /// Assemble a paired report from the two run results, config snapshots,
    /// dataset info, and gates.
    #[must_use]
    pub fn assemble(
        dataset: DatasetInfo,
        full_config: &RetrievalConfig,
        full: RunResult,
        baseline_config: &RetrievalConfig,
        baseline: RunResult,
        gates: &Gates,
    ) -> Self {
        let deltas = MetricDeltas::compute(&full.metrics, &baseline.metrics);
        let gate_results = evaluate_gates(gates, &full.metrics, &deltas);
        let gates_passed = gate_results.iter().all(|g| g.passed);
        Report {
            schema_version: REPORT_SCHEMA_VERSION.to_string(),
            dataset,
            full: SideReport {
                config: full_config.into(),
                metrics: full.metrics,
                per_question: full.per_question,
            },
            baseline: SideReport {
                config: baseline_config.into(),
                metrics: baseline.metrics,
                per_question: baseline.per_question,
            },
            deltas,
            gates: gate_results,
            gates_passed,
        }
    }

    /// Serialize to pretty JSON (deterministic ordering).
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("report is always serializable")
    }

    /// A compact human-readable summary for stdout.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let f = &self.full.metrics;
        let b = &self.baseline.metrics;
        let d = &self.deltas;
        let mut out = String::new();
        out.push_str(&format!(
            "Dataset: {} v{} ({} questions, {} temporal)\n",
            self.dataset.name, self.dataset.version, f.num_questions, f.num_temporal_questions
        ));
        out.push_str("Metric                 full      baseline   delta\n");
        out.push_str(&row(
            "precision@k",
            f.precision_at_k,
            b.precision_at_k,
            d.precision_at_k,
        ));
        out.push_str(&row(
            "recall@k",
            f.recall_at_k,
            b.recall_at_k,
            d.recall_at_k,
        ));
        out.push_str(&row(
            "grounding_precision",
            f.grounding_precision,
            b.grounding_precision,
            d.grounding_precision,
        ));
        out.push_str(&row(
            "temporal_accuracy",
            f.temporal_accuracy,
            b.temporal_accuracy,
            d.temporal_accuracy,
        ));
        out.push_str(&row(
            "citation_validity",
            f.citation_validity,
            b.citation_validity,
            d.citation_validity,
        ));
        if !self.gates.is_empty() {
            out.push_str("\nGates:\n");
            for g in &self.gates {
                out.push_str(&format!(
                    "  [{}] {} (threshold {:.3}, observed {:.3})\n",
                    if g.passed { "PASS" } else { "FAIL" },
                    g.name,
                    g.threshold,
                    g.observed
                ));
            }
            out.push_str(&format!(
                "Overall: {}\n",
                if self.gates_passed { "PASS" } else { "FAIL" }
            ));
        }
        out
    }
}

fn row(name: &str, full: f64, baseline: f64, delta: f64) -> String {
    format!("{name:<22} {full:>7.3}  {baseline:>8.3}  {delta:>+7.3}\n")
}

/// Compute a [`DatasetInfo`] from a loaded dataset.
#[must_use]
pub fn dataset_info(dataset: &crate::dataset::Dataset) -> DatasetInfo {
    DatasetInfo {
        name: dataset.name.clone(),
        version: dataset.version.clone(),
        license: dataset.license.clone(),
        num_questions: dataset.questions.len(),
    }
}

fn evaluate_gates(gates: &Gates, full: &RunMetrics, deltas: &MetricDeltas) -> Vec<GateResult> {
    let mut results = Vec::new();
    if let Some(threshold) = gates.min_temporal_accuracy_delta {
        results.push(GateResult {
            name: "min_temporal_accuracy_delta".to_string(),
            threshold,
            observed: deltas.temporal_accuracy,
            passed: deltas.temporal_accuracy >= threshold,
        });
    }
    if let Some(threshold) = gates.min_full_temporal_accuracy {
        results.push(GateResult {
            name: "min_full_temporal_accuracy".to_string(),
            threshold,
            observed: full.temporal_accuracy,
            passed: full.temporal_accuracy >= threshold,
        });
    }
    if let Some(threshold) = gates.min_recall_delta {
        results.push(GateResult {
            name: "min_recall_delta".to_string(),
            threshold,
            observed: deltas.recall_at_k,
            passed: deltas.recall_at_k >= threshold,
        });
    }
    if let Some(threshold) = gates.min_full_citation_validity {
        results.push(GateResult {
            name: "min_full_citation_validity".to_string(),
            threshold,
            observed: full.citation_validity,
            passed: full.citation_validity >= threshold,
        });
    }
    results
}

/// Load an [`EvalConfig`] manifest and both retrieval configs, run the paired
/// evaluation, and assemble the report. This is the one-call entry point used
/// by the binary and integration tests.
pub fn run_manifest(manifest_path: &std::path::Path) -> Result<Report, Box<dyn std::error::Error>> {
    let manifest = EvalConfig::from_toml_file(manifest_path)?;
    let manifest_dir = manifest_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let paths = manifest.resolve_relative_to(manifest_dir);

    let dataset = crate::dataset::Dataset::from_json_file(&paths.dataset)?;
    let full_config = RetrievalConfig::from_toml_file(&paths.full)?;
    let baseline_config = RetrievalConfig::from_toml_file(&paths.baseline)?;

    // Each config gets its own freshly-indexed database so the two runs are
    // fully independent (embeddings depend on the config seed).
    let (full_db, full_graph) = crate::harness::index_dataset(&dataset, full_config.seed)?;
    let full = crate::harness::run(&full_db, &full_graph, &dataset, &full_config)?;

    let (base_db, base_graph) = crate::harness::index_dataset(&dataset, baseline_config.seed)?;
    let baseline = crate::harness::run(&base_db, &base_graph, &dataset, &baseline_config)?;

    Ok(Report::assemble(
        dataset_info(&dataset),
        &full_config,
        full,
        &baseline_config,
        baseline,
        &manifest.gates,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(temporal: f64) -> RunMetrics {
        RunMetrics {
            precision_at_k: 1.0,
            recall_at_k: 1.0,
            grounding_precision: 1.0,
            temporal_accuracy: temporal,
            citation_validity: 1.0,
            num_questions: 4,
            num_temporal_questions: 4,
        }
    }

    #[test]
    fn deltas_computed() {
        let d = MetricDeltas::compute(&metrics(1.0), &metrics(0.25));
        assert!((d.temporal_accuracy - 0.75).abs() < 1e-12);
    }

    #[test]
    fn gate_passes_when_delta_meets_threshold() {
        let gates = Gates {
            min_temporal_accuracy_delta: Some(0.25),
            ..Default::default()
        };
        let deltas = MetricDeltas::compute(&metrics(1.0), &metrics(0.0));
        let results = evaluate_gates(&gates, &metrics(1.0), &deltas);
        assert_eq!(results.len(), 1);
        assert!(results[0].passed);
    }

    #[test]
    fn gate_fails_when_delta_below_threshold() {
        let gates = Gates {
            min_temporal_accuracy_delta: Some(0.25),
            ..Default::default()
        };
        // full 0.3, baseline 0.2 → delta 0.1 < 0.25.
        let deltas = MetricDeltas::compute(&metrics(0.3), &metrics(0.2));
        let results = evaluate_gates(&gates, &metrics(0.3), &deltas);
        assert!(!results[0].passed);
    }
}
