//! Regression tests for stack-overflow-safe equality on deeply nested
//! `PropertyValue::Array` values (supersedes PR #3098).
//!
//! A `PropertyValue::Array` can be nested arbitrarily deeply when constructed
//! programmatically (serialization enforces `MAX_RECURSION_DEPTH`, but in-memory
//! construction does not). A naive recursive `PartialEq` / `semantically_equal`
//! walks one stack frame per nesting level, so comparing two deeply nested
//! values overflows the thread stack and aborts the process (SIGABRT/SIGSEGV) —
//! an uncatchable crash that a malicious or accidental deep value can trigger.
//!
//! These tests construct values far deeper than any reasonable call stack can
//! handle and assert the comparison returns cleanly. Without the iterative
//! (explicit-stack) equality implementations they abort the test process; with
//! them they pass. `PropertyValue::Drop` is already iterative, so tearing the
//! values down at end of scope is also overflow-safe.

use aletheiadb::core::property::PropertyValue;
use std::sync::Arc;

/// Nesting depth that comfortably exceeds the default test-thread stack for a
/// per-level-recursive comparison, guaranteeing an overflow without the fix.
const DEEP: usize = 200_000;

/// Wrap `leaf` in `depth` layers of single-element `Array`.
fn nest(leaf: PropertyValue, depth: usize) -> PropertyValue {
    let mut v = leaf;
    for _ in 0..depth {
        v = PropertyValue::Array(Arc::new(vec![v]));
    }
    v
}

#[test]
fn deeply_nested_arrays_compare_equal_without_overflow() {
    let a = nest(PropertyValue::Int(1), DEEP);
    let b = nest(PropertyValue::Int(1), DEEP);
    assert!(a == b, "structurally identical deep arrays must be equal");
}

#[test]
fn deeply_nested_arrays_detect_inequality_without_overflow() {
    let a = nest(PropertyValue::Int(1), DEEP);
    let b = nest(PropertyValue::Int(2), DEEP);
    assert!(a != b, "deep arrays differing at the leaf must be unequal");
}

#[test]
fn deeply_nested_arrays_semantically_equal_without_overflow() {
    // `semantically_equal` (NaN-aware) also recurses on `Array` and must be
    // overflow-safe too. Use a NaN leaf so we exercise the NaN-aware path.
    let a = nest(PropertyValue::Float(f64::NAN), DEEP);
    let b = nest(PropertyValue::Float(f64::NAN), DEEP);
    assert!(
        a.semantically_equal(&b),
        "deeply nested NaN arrays must be semantically equal"
    );
    assert!(
        a != b,
        "NaN != NaN under PartialEq, even when deeply nested"
    );
}

#[test]
fn deeply_nested_arrays_semantically_unequal_without_overflow() {
    // Complements the deep-equal case above: a deeply nested value that differs
    // only at the leaf must be reported unequal by `semantically_equal` (still
    // NaN-aware) without overflowing the stack.
    let a = nest(PropertyValue::Float(1.0), DEEP);
    let b = nest(PropertyValue::Float(2.0), DEEP);
    assert!(
        !a.semantically_equal(&b),
        "deep arrays differing at the leaf must be semantically unequal"
    );

    // Also exercise a NaN-vs-number leaf mismatch: NaN is only equal to NaN.
    let nan = nest(PropertyValue::Float(f64::NAN), DEEP);
    let num = nest(PropertyValue::Float(1.0), DEEP);
    assert!(
        !nan.semantically_equal(&num),
        "NaN leaf must not be semantically equal to a numeric leaf"
    );
}

/// Branching depth for the wide+deep tree. Kept modest because the node count
/// grows as `BRANCH^WIDE_DEEP`; the point is to exercise the work stack holding
/// many pending pairs at once (multiple children pushed per level), not raw
/// depth (already covered by the single-chain `DEEP` tests above).
const BRANCH: usize = 3;
const WIDE_DEEP: usize = 10;

/// Build a balanced tree where every internal node is an `Array` of `BRANCH`
/// children, `depth` levels deep, with every leaf equal to `leaf`.
fn wide_nest(leaf: &PropertyValue, depth: usize) -> PropertyValue {
    if depth == 0 {
        return leaf.clone();
    }
    let child = wide_nest(leaf, depth - 1);
    PropertyValue::Array(Arc::new(vec![child; BRANCH]))
}

#[test]
fn wide_and_deep_arrays_compare_equal_without_overflow() {
    // A branching tree forces the iterative work stack to actually grow with
    // multiple pending element pairs per level (unlike the depth-1 chain in the
    // other tests, which never holds more than one pending pair).
    let a = wide_nest(&PropertyValue::Int(7), WIDE_DEEP);
    let b = wide_nest(&PropertyValue::Int(7), WIDE_DEEP);
    assert!(a == b, "identical wide+deep trees must be equal");
    assert!(
        a.semantically_equal(&b),
        "identical wide+deep trees must be semantically equal"
    );
}

#[test]
fn wide_and_deep_arrays_detect_inequality_without_overflow() {
    // Differ at a single leaf buried in one branch: the comparison must still
    // return `false` cleanly while the work stack holds many pending pairs.
    let a = wide_nest(&PropertyValue::Int(7), WIDE_DEEP);

    // Build a same-shaped tree whose top level mixes identical subtrees with a
    // differing one, so the walk descends several branches (the work stack
    // holds many pending pairs) before finding the buried leaf difference.
    let identical_subtree = wide_nest(&PropertyValue::Int(7), WIDE_DEEP - 1);
    let differing_leaf_tree = PropertyValue::Array(Arc::new(vec![
        identical_subtree.clone(),
        identical_subtree,
        wide_nest(&PropertyValue::Int(8), WIDE_DEEP - 1),
    ]));

    assert!(
        a != differing_leaf_tree,
        "wide+deep trees differing at a buried leaf must be unequal"
    );
    assert!(
        !a.semantically_equal(&differing_leaf_tree),
        "wide+deep trees differing at a buried leaf must be semantically unequal"
    );
}
