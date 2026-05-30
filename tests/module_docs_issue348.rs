//! TDD tests for issue #348: Missing module-level documentation in WAL and query planner modules.
//!
//! These tests define the documentation quality bar for the modules listed in
//! the issue. Each test reads the source file and asserts that specific
//! architectural sections are present in the `//!` module docs.
//!
//! ### Red → Green → Refactor cycle
//!
//! 1. **Red** – these tests are committed first; they fail because the sections
//!    are absent.
//! 2. **Green** – the missing sections are added to each source file.
//! 3. **Refactor** – `cargo clippy`, `cargo fmt`, and `cargo doc` are run.

use std::fs;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Extract only the `//!` module-doc block from a Rust source file.
///
/// Returns all lines that start with `//!` and stops at the first
/// line that does not (i.e. the first `use`, `pub`, or blank separator
/// line that ends the block).
fn module_docs(path: &str) -> String {
    let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("Failed to read {path}: {e}"));
    src.lines()
        .take_while(|l| l.starts_with("//!"))
        .filter(|l| l.starts_with("//!"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ── WAL: stripe.rs ───────────────────────────────────────────────────────────

/// `stripe.rs` should have an `# Architecture` section explaining how a stripe
/// fits into the overall striped WAL.
#[test]
fn test_stripe_has_architecture_section() {
    let docs = module_docs("src/storage/wal/stripe.rs");
    assert!(
        docs.contains("# Architecture") || docs.contains("# Design"),
        "stripe.rs module docs must include an `# Architecture` (or `# Design`) \
         section. Got:\n{docs}"
    );
}

/// `stripe.rs` should mention performance characteristics (latency / throughput)
/// so readers understand the performance contract of the stripe abstraction.
#[test]
fn test_stripe_has_performance_metrics() {
    let docs = module_docs("src/storage/wal/stripe.rs");
    let has_metrics = docs.contains("# Performance")
        || (docs.contains("ns") || docs.contains("µs") || docs.contains("us"));
    assert!(
        has_metrics,
        "stripe.rs module docs must include performance metrics (a `# Performance` \
         section or latency values). Got:\n{docs}"
    );
}

/// `stripe.rs` module docs should cross-reference the `ring_buffer` module by
/// name or link so readers can navigate the architecture.
#[test]
fn test_stripe_references_ring_buffer_module() {
    let docs = module_docs("src/storage/wal/stripe.rs");
    assert!(
        docs.contains("ring_buffer") || docs.contains("[`WalRingBuffer`]"),
        "stripe.rs module docs must reference the `ring_buffer` module or \
         `WalRingBuffer` type. Got:\n{docs}"
    );
}

// ── Query planner: cost.rs ───────────────────────────────────────────────────

/// `cost.rs` should explain *where* in the query-planning pipeline cost
/// estimation occurs, not just list calibration values.
#[test]
fn test_cost_has_architecture_section() {
    let docs = module_docs("src/query/planner/cost.rs");
    assert!(
        docs.contains("# Architecture") || docs.contains("# Role") || docs.contains("# Pipeline"),
        "cost.rs module docs must include an `# Architecture`, `# Role`, or \
         `# Pipeline` section explaining where cost estimation fits in the \
         query planner. Got:\n{docs}"
    );
}

/// `cost.rs` should reference the `stats` module (or `Statistics`) to make the
/// dependency between the two modules explicit for new readers.
#[test]
fn test_cost_references_stats_module() {
    let docs = module_docs("src/query/planner/cost.rs");
    assert!(
        docs.contains("stats") || docs.contains("Statistics") || docs.contains("[`Statistics`]"),
        "cost.rs module docs should reference the `stats`/`Statistics` module \
         it depends on. Got:\n{docs}"
    );
}

// ── Query planner: rules/mod.rs ──────────────────────────────────────────────

/// `rules/mod.rs` must list the individual optimization rules so that readers
/// know which rules exist without having to scan the submodule files.
#[test]
fn test_rules_module_lists_available_rules() {
    let docs = module_docs("src/query/planner/rules/mod.rs");
    let names = [
        "PredicatePushdown",
        "FilterScanFusion",
        "LimitPushdown",
        "OperationReordering",
    ];
    for name in names {
        assert!(
            docs.contains(name),
            "rules/mod.rs module docs must list `{name}` among the available \
             optimization rules. Got:\n{docs}"
        );
    }
}

/// `rules/mod.rs` must explain the rule application order (or rationale for the
/// ordering) so that contributors understand why rules must run in sequence.
#[test]
fn test_rules_module_explains_ordering() {
    let docs = module_docs("src/query/planner/rules/mod.rs");
    let has_order = docs.contains("# Rule Ordering")
        || docs.contains("# Ordering")
        || docs.contains("# Architecture")
        || docs.contains("order");
    assert!(
        has_order,
        "rules/mod.rs module docs must explain the rule application ordering \
         (e.g. a `# Rule Ordering` section). Got:\n{docs}"
    );
}

// ── WAL: segment_reader.rs ───────────────────────────────────────────────────

/// `segment_reader.rs` should document the binary segment format so that
/// contributors can understand the on-disk layout without reading the code.
#[test]
fn test_segment_reader_documents_segment_format() {
    let docs = module_docs("src/storage/wal/segment_reader.rs");
    assert!(
        docs.contains("# Segment Format") || docs.contains("# Format") || docs.contains("magic"),
        "segment_reader.rs module docs must describe the binary segment format \
         (magic bytes, version byte, entry framing). Got:\n{docs}"
    );
}
