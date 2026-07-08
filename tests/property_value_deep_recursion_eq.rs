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
