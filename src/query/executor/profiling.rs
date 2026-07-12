//! Per-operator execution profiling for the Cypher `PROFILE` entry point
//! (Issue #562).
//!
//! [`ProfilingIterator`] wraps another [`ResultIterator`] and records, for the
//! operator it fronts, (a) the number of rows it emits and (b) the cumulative
//! wall-clock time spent in its `next()` calls. Each wrapper shares an
//! [`OpProfile`] handle (interior-mutable via atomics, so the wrapper stays
//! `Send`); the executor collects those handles into an ordered
//! [`ProfileRegistry`] in plan-tree **pre-order** so the recorded stats line up
//! one-to-one with the operators rendered by `PhysicalPlan::explain`.
//!
//! Timing is inclusive of child operators (an operator's `next()` drives its
//! input's `next()`), which mirrors how engines report cumulative operator
//! time. Because timings are wall-clock and therefore non-deterministic, tests
//! assert on structure and row counts, never absolute times.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::core::error::Result;

use super::iterators::ResultIterator;
use super::results::QueryRow;

/// Recorded execution statistics for a single physical operator.
///
/// Counters are atomic so the shared handle stays `Send` (the pull-based
/// executor drives one thread, but the wrapper must satisfy the
/// [`ResultIterator`] `Send` bound). All updates use `Relaxed` ordering:
/// there is no cross-thread happens-before requirement, only monotonic
/// accumulation read back after the stream is fully drained.
#[derive(Debug)]
pub struct OpProfile {
    op_name: &'static str,
    depth: usize,
    actual_rows: AtomicU64,
    elapsed_nanos: AtomicU64,
}

impl OpProfile {
    /// Create a zeroed profile for an operator at the given tree `depth`.
    #[must_use]
    pub fn new(op_name: &'static str, depth: usize) -> Self {
        Self {
            op_name,
            depth,
            actual_rows: AtomicU64::new(0),
            elapsed_nanos: AtomicU64::new(0),
        }
    }

    /// The operator's name (matches `PhysicalOp::name`).
    #[must_use]
    pub fn op_name(&self) -> &'static str {
        self.op_name
    }

    /// The operator's depth in the plan tree (root is 0).
    #[must_use]
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Number of rows this operator emitted over the whole scan.
    #[must_use]
    pub fn actual_rows(&self) -> u64 {
        self.actual_rows.load(Ordering::Relaxed)
    }

    /// Cumulative wall-clock time spent in this operator's `next()`, in
    /// microseconds (inclusive of children).
    #[must_use]
    pub fn elapsed_micros(&self) -> u64 {
        self.elapsed_nanos.load(Ordering::Relaxed) / 1_000
    }

    /// The suffix appended to this operator's `explain` line in `PROFILE`
    /// output. Uses a stable, fixed-label format (`actual rows:` / `time:`) so
    /// substring assertions in tests are robust against timing non-determinism.
    #[must_use]
    pub fn annotation(&self) -> String {
        format!(
            " | actual rows: {}, time: {}µs",
            self.actual_rows(),
            self.elapsed_micros()
        )
    }

    fn record_row(&self) {
        self.actual_rows.fetch_add(1, Ordering::Relaxed);
    }

    fn record_time(&self, nanos: u64) {
        self.elapsed_nanos.fetch_add(nanos, Ordering::Relaxed);
    }
}

/// Ordered collection of per-operator profiles, in plan-tree pre-order.
///
/// The i-th entry corresponds to the i-th operator visited by
/// `PhysicalPlan::explain`'s pre-order walk, so the executor's registration
/// order and the renderer's traversal order stay aligned by index.
pub type ProfileRegistry = Vec<Arc<OpProfile>>;

/// A [`ResultIterator`] that records row-count and timing for the operator it
/// wraps, delegating all row production to `inner`.
pub struct ProfilingIterator {
    inner: Box<dyn ResultIterator>,
    profile: Arc<OpProfile>,
}

impl ProfilingIterator {
    /// Wrap `inner`, recording stats into the shared `profile` handle.
    #[must_use]
    pub fn new(inner: Box<dyn ResultIterator>, profile: Arc<OpProfile>) -> Self {
        Self { inner, profile }
    }
}

impl ResultIterator for ProfilingIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        let start = Instant::now();
        let item = self.inner.next();
        self.profile.record_time(start.elapsed().as_nanos() as u64);
        if matches!(item, Some(Ok(_))) {
            self.profile.record_row();
        }
        item
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}
