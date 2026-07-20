//! End-to-end smoke test: run the tiny suite and assert a well-formed report
//! with all expected operation keys, plus the temporal (<10ms) and vector
//! (k=10) extension entries.

use aletheia_bench_ldbc::gate::evaluate_gate;
use aletheia_bench_ldbc::generator::Scale;
use aletheia_bench_ldbc::report::Category;
use aletheia_bench_ldbc::runner::{RunOptions, run_suite};

/// Keys every run must contain.
const EXPECTED_KEYS: &[&str] = &[
    // short reads
    "is1_person_profile",
    "is2_person_recent_messages",
    "is3_person_friends",
    "is5_message_creator",
    "is6_forum_of_message",
    // complex reads
    "ic1_friends_within_2_hops",
    "ic2_recent_messages_by_friends",
    "ic9_messages_by_friends_of_friends",
    // insert/update stream
    "ins1_insert_person",
    "ins6_insert_post",
    "upd2_update_person_city",
    // extensions
    "ext_temporal_reconstruction",
    "ext_temporal_as_of_traversal",
    "ext_vector_knn",
    "ext_vector_hybrid_graph_vector",
];

fn tiny_opts() -> RunOptions {
    RunOptions {
        scale: Scale::Smoke,
        seed: 42,
        warmup: 2,
        iterations: 10,
        ..RunOptions::default()
    }
}

#[test]
fn vector_overrides_flow_into_reported_config() {
    // A tiny dedicated corpus + dim override; proves the overrides reach the
    // reported config end-to-end without materializing anything large.
    let opts = RunOptions {
        scale: Scale::Smoke,
        seed: 42,
        warmup: 1,
        iterations: 3,
        vector_count: Some(24),
        vector_dim: Some(48),
    };
    let report = run_suite(&opts).expect("suite should run with overrides");
    // vector_count is honest: post embeddings PLUS the 24-vector corpus.
    let posts = 6 * 8; // smoke: forums * posts_per_forum
    assert_eq!(report.config.vector_count, posts + 24);
    assert_eq!(report.config.vector_dim, 48);
}

#[test]
fn smoke_run_is_well_formed() {
    let report = run_suite(&tiny_opts()).expect("suite should run");

    // All expected operation keys present.
    for key in EXPECTED_KEYS {
        let op = report
            .operation(key)
            .unwrap_or_else(|| panic!("missing operation key: {key}"));
        assert_eq!(op.iterations, 10);
        assert!(op.stats.count == 10, "op {key} should have 10 samples");
        assert!(op.stats.p50_us >= 0.0);
        assert!(op.stats.p99_us >= op.stats.p50_us, "p99 >= p50 for {key}");
    }

    // At least 12 SNB operations (short + complex + insert/update), plus the
    // two extensions' operations.
    let snb_ops = report
        .operations
        .iter()
        .filter(|o| {
            matches!(
                o.category,
                Category::ShortRead | Category::ComplexRead | Category::InsertUpdate
            )
        })
        .count();
    assert!(
        snb_ops >= 11,
        "expected >= 11 SNB operations, got {snb_ops}"
    );

    // Report metadata sane.
    assert_eq!(report.schema_version, 1);
    assert!(report.config.node_count > 0);
    assert!(report.config.edge_count > 0);
    assert!(report.config.vector_count > 0);
    assert!(report.disclaimer.to_lowercase().contains("not an audited"));
}

#[test]
fn temporal_extension_present_and_reconstructs_under_10ms() {
    let report = run_suite(&tiny_opts()).expect("suite should run");
    let t = report
        .operation("ext_temporal_reconstruction")
        .expect("temporal reconstruction entry present");
    assert_eq!(t.category, Category::TemporalExtension);
    // The <10ms reconstruction target (reported in µs).
    assert!(
        t.stats.p99_us < 10_000.0,
        "temporal reconstruction p99 should be < 10ms, got {:.2}µs",
        t.stats.p99_us
    );
    assert_eq!(
        t.extra.get("reconstruction_target_ms"),
        Some(&serde_json::json!(10))
    );
}

#[test]
fn vector_extension_present_with_k10_and_reported_scale() {
    let report = run_suite(&tiny_opts()).expect("suite should run");
    let v = report
        .operation("ext_vector_knn")
        .expect("vector k-NN entry present");
    assert_eq!(v.category, Category::VectorExtension);
    assert_eq!(v.extra.get("knn_k"), Some(&serde_json::json!(10)));
    // The scale actually run is reported honestly (never a fake 1M).
    let reported = v
        .extra
        .get("vector_count")
        .and_then(|x| x.as_u64())
        .expect("vector_count reported");
    assert_eq!(reported, report.config.vector_count as u64);
    assert!(reported < 1_000_000, "smoke scale is far below 1M vectors");
}

#[test]
fn report_serializes_to_json_and_gates_against_itself() {
    let report = run_suite(&tiny_opts()).expect("suite should run");
    // JSON round-trips.
    let json = serde_json::to_string(&report).expect("serialize");
    let parsed: aletheia_bench_ldbc::report::BenchmarkReport =
        serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.operations.len(), report.operations.len());
    // A report gated against itself must pass (0% delta everywhere).
    let result = evaluate_gate(&report, &report, 10.0);
    assert!(result.passed(), "a report must not regress against itself");
}
