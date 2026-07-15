//! End-to-end pipeline integration tests (Issue #3366).
//!
//! These exercise the whole harness — load a tiny dataset, index it into a live
//! `AletheiaDB`, run full and baseline configs, and assert the paired outcome —
//! plus the bundled gold datasets and their regression gates.

use std::path::{Path, PathBuf};

use aletheia_eval::config::{EvalConfig, Gates, RetrievalConfig};
use aletheia_eval::dataset::Dataset;
use aletheia_eval::harness::{index_dataset, run};
use aletheia_eval::report::{Report, dataset_info, run_manifest};

fn datasets_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("datasets")
}

fn full_config() -> RetrievalConfig {
    RetrievalConfig {
        k: 5,
        hybrid: true,
        temporal_anchoring: true,
        provenance_filter: true,
        max_hops: 2,
        trusted_sources: vec!["curated".to_string()],
        seed: 42,
    }
}

fn baseline_config() -> RetrievalConfig {
    RetrievalConfig {
        k: 5,
        hybrid: false,
        temporal_anchoring: false,
        provenance_filter: false,
        max_hops: 2,
        trusted_sources: vec!["curated".to_string()],
        seed: 42,
    }
}

/// A minimal in-line temporal dataset: one company, two CEO eras, one question
/// per era. The full (anchored) config must answer both; the baseline (current
/// state) can only answer the current era.
fn tiny_temporal_dataset() -> Dataset {
    let json = r#"
    {
      "version": "0.0.1-test",
      "name": "tiny_temporal",
      "license": "CC0-1.0",
      "entities": [
        {"key": "acme", "label": "Company", "text": "Acme semiconductor foundry",
         "properties": {"name": "Acme"}, "source": "curated", "valid_from": "2015-01-01"},
        {"key": "acme_t1", "label": "Tenure", "text": "leadership record",
         "properties": {"company": "Acme", "ceo": "Alice"},
         "source": "curated", "valid_from": "2015-01-01", "retract_at": "2020-01-01"},
        {"key": "acme_t2", "label": "Tenure", "text": "leadership record",
         "properties": {"company": "Acme", "ceo": "Carol"},
         "source": "curated", "valid_from": "2020-01-01"}
      ],
      "questions": [
        {"id": "past", "text": "Who was the boss of Acme in 2017",
         "gold_evidence": ["acme"], "valid_time": "2017-06-01",
         "answer_label": "Tenure", "answer_filter_key": "company",
         "answer_filter_value": "Acme", "answer_property": "ceo", "gold_answer": "Alice"},
        {"id": "now", "text": "Who was the boss of Acme in 2024",
         "gold_evidence": ["acme"], "valid_time": "2024-06-01",
         "answer_label": "Tenure", "answer_filter_key": "company",
         "answer_filter_value": "Acme", "answer_property": "ceo", "gold_answer": "Carol"}
      ]
    }
    "#;
    Dataset::from_json_str(json, Path::new("tiny.json")).unwrap()
}

#[test]
fn full_config_beats_baseline_on_temporal_accuracy() {
    let dataset = tiny_temporal_dataset();

    let (fdb, fgraph) = index_dataset(&dataset, full_config().seed).unwrap();
    let full = run(&fdb, &fgraph, &dataset, &full_config()).unwrap();

    let (bdb, bgraph) = index_dataset(&dataset, baseline_config().seed).unwrap();
    let baseline = run(&bdb, &bgraph, &dataset, &baseline_config()).unwrap();

    // Full anchors each query at its era → both questions correct.
    assert_eq!(full.metrics.temporal_accuracy, 1.0);
    // Baseline reads current state → only the current-era question is right.
    assert_eq!(baseline.metrics.temporal_accuracy, 0.5);
    assert!(full.metrics.temporal_accuracy > baseline.metrics.temporal_accuracy);
}

#[test]
fn temporal_question_that_changed_scores_one_for_full_zero_for_baseline() {
    let dataset = tiny_temporal_dataset();

    let (fdb, fgraph) = index_dataset(&dataset, 42).unwrap();
    let full = run(&fdb, &fgraph, &dataset, &full_config()).unwrap();
    let (bdb, bgraph) = index_dataset(&dataset, 42).unwrap();
    let baseline = run(&bdb, &bgraph, &dataset, &baseline_config()).unwrap();

    let full_past = full.per_question.iter().find(|q| q.id == "past").unwrap();
    let base_past = baseline
        .per_question
        .iter()
        .find(|q| q.id == "past")
        .unwrap();

    // The fact changed: full (correct AS OF 2017) → Alice; baseline (current) → Carol.
    assert_eq!(full_past.temporal_correct, Some(true));
    assert_eq!(full_past.predicted_answer.as_deref(), Some("Alice"));
    assert_eq!(base_past.temporal_correct, Some(false));
    assert_eq!(base_past.predicted_answer.as_deref(), Some("Carol"));
}

#[test]
fn determinism_two_runs_byte_identical() {
    let dataset = tiny_temporal_dataset();

    let render = || {
        let (fdb, fgraph) = index_dataset(&dataset, 42).unwrap();
        let full = run(&fdb, &fgraph, &dataset, &full_config()).unwrap();
        let (bdb, bgraph) = index_dataset(&dataset, 42).unwrap();
        let baseline = run(&bdb, &bgraph, &dataset, &baseline_config()).unwrap();
        Report::assemble(
            dataset_info(&dataset),
            &full_config(),
            full,
            &baseline_config(),
            baseline,
            &Gates::default(),
        )
        .to_json()
    };

    assert_eq!(
        render(),
        render(),
        "same (dataset, config, seed) must be byte-identical"
    );
}

#[test]
fn bundled_temporal_dataset_meets_headline_gate() {
    let manifest = datasets_dir().join("temporal_qa").join("eval.toml");
    let report = run_manifest(&manifest).unwrap();

    // The #3366 headline: >= 25 percentage points higher temporal accuracy.
    assert!(
        report.deltas.temporal_accuracy >= 0.25,
        "temporal accuracy delta {} below 25pp headline",
        report.deltas.temporal_accuracy
    );
    assert_eq!(report.full.metrics.temporal_accuracy, 1.0);
    assert!(report.gates_passed, "bundled temporal gates must pass");
}

#[test]
fn bundled_multihop_dataset_recall_lift_from_traversal() {
    let manifest = datasets_dir().join("multihop_qa").join("eval.toml");
    let report = run_manifest(&manifest).unwrap();

    // Two-hop gold evidence is unreachable by vector similarity; hybrid
    // traversal recovers it, so recall lifts sharply.
    assert!(
        report.deltas.recall_at_k >= 0.5,
        "recall delta {} below expected traversal lift",
        report.deltas.recall_at_k
    );
    assert!(report.gates_passed, "bundled multihop gates must pass");
}

#[test]
fn bundled_configs_parse() {
    for ds in ["temporal_qa", "multihop_qa"] {
        let dir = datasets_dir().join(ds);
        EvalConfig::from_toml_file(&dir.join("eval.toml")).unwrap();
        RetrievalConfig::from_toml_file(&dir.join("full.toml")).unwrap();
        RetrievalConfig::from_toml_file(&dir.join("baseline.toml")).unwrap();
        Dataset::from_json_file(&dir.join("dataset.json")).unwrap();
    }
}
