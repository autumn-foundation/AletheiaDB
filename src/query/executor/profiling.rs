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
    /// The `(incl. children)` qualifier discloses that the reported time is
    /// cumulative over child operators (the Volcano `next()` recurses), so the
    /// number is not this operator's self-time.
    #[must_use]
    pub fn annotation(&self) -> String {
        format!(
            " | actual rows: {}, time: {}µs (incl. children)",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::{Error, QueryError};
    use crate::core::property::PropertyValue;
    use std::collections::VecDeque;
    use std::time::Duration;

    /// Minimal in-memory [`ResultIterator`] for exercising the profiling
    /// wrapper without any storage. Each `next()` optionally sleeps (so the
    /// wrapper records a non-zero elapsed) and returns the next preloaded
    /// response; once the queue is empty it yields `None` like any exhausted
    /// iterator.
    struct StubIterator {
        responses: VecDeque<Option<Result<QueryRow>>>,
        sleep_per_call: Duration,
    }

    impl StubIterator {
        fn new(responses: Vec<Option<Result<QueryRow>>>, sleep_per_call: Duration) -> Self {
            Self {
                responses: responses.into_iter().collect(),
                sleep_per_call,
            }
        }
    }

    impl ResultIterator for StubIterator {
        fn next(&mut self) -> Option<Result<QueryRow>> {
            if !self.sleep_per_call.is_zero() {
                std::thread::sleep(self.sleep_per_call);
            }
            self.responses.pop_front().flatten()
        }

        fn size_hint(&self) -> (usize, Option<usize>) {
            (0, Some(self.responses.len()))
        }
    }

    /// Build a row tagged with an integer id so forwarding order is observable.
    fn row_with_id(id: i64) -> QueryRow {
        QueryRow::from_columns(vec![("id".to_string(), PropertyValue::Int(id))])
    }

    fn row_id(row: &QueryRow) -> Option<i64> {
        match row.columns.as_ref()?.first()? {
            (_, PropertyValue::Int(n)) => Some(*n),
            _ => None,
        }
    }

    #[test]
    fn profiling_iterator_forwards_rows_and_counts_only_ok() {
        let profile = Arc::new(OpProfile::new("TestOp", 0));
        let inner = StubIterator::new(
            vec![
                Some(Ok(row_with_id(1))),
                Some(Err(Error::Query(QueryError::ExecutionError {
                    message: "boom".to_string(),
                }))),
                Some(Ok(row_with_id(2))),
            ],
            Duration::from_millis(1),
        );
        let mut wrapper = ProfilingIterator::new(Box::new(inner), Arc::clone(&profile));

        // Drain the stream, recording what came through the wrapper.
        let mut ok_ids = Vec::new();
        let mut saw_err = false;
        while let Some(item) = wrapper.next() {
            match item {
                Ok(row) => ok_ids.push(row_id(&row).expect("row carries an id column")),
                Err(_) => saw_err = true,
            }
        }

        // Rows are forwarded verbatim, in order.
        assert_eq!(ok_ids, vec![1, 2]);
        assert!(saw_err, "the Err item must be forwarded to the caller");

        // Only the two Ok rows count; the Err and the terminating None do not.
        assert_eq!(profile.actual_rows(), 2);

        // Every next() (incl. the Err and the terminating None) was timed with a
        // 1ms sleep, so the accumulated elapsed must be non-zero.
        assert!(
            profile.elapsed_micros() > 0,
            "wrapper must accumulate non-zero elapsed time"
        );
    }

    #[test]
    fn profiling_iterator_surfaces_and_does_not_count_none() {
        let profile = Arc::new(OpProfile::new("TestOp", 0));
        // An inner `None` in the middle terminates the stream (Volcano
        // contract) and must not be counted as a row.
        let inner = StubIterator::new(
            vec![Some(Ok(row_with_id(7))), None, Some(Ok(row_with_id(8)))],
            Duration::ZERO,
        );
        let mut wrapper = ProfilingIterator::new(Box::new(inner), Arc::clone(&profile));

        assert!(matches!(wrapper.next(), Some(Ok(_))));
        assert!(wrapper.next().is_none(), "inner None surfaces as None");
        // The None was not counted; only the single Ok row was.
        assert_eq!(profile.actual_rows(), 1);
    }

    #[test]
    fn profiling_iterator_forwards_size_hint() {
        let profile = Arc::new(OpProfile::new("TestOp", 0));
        let inner = StubIterator::new(
            vec![Some(Ok(row_with_id(1))), Some(Ok(row_with_id(2)))],
            Duration::ZERO,
        );
        let wrapper = ProfilingIterator::new(Box::new(inner), profile);
        assert_eq!(wrapper.size_hint(), (0, Some(2)));
    }

    #[test]
    fn op_profile_annotation_format() {
        let profile = OpProfile::new("NodeScan", 2);
        assert_eq!(profile.op_name(), "NodeScan");
        assert_eq!(profile.depth(), 2);
        // A freshly created profile is zeroed.
        assert_eq!(profile.actual_rows(), 0);
        assert_eq!(profile.elapsed_micros(), 0);
        assert_eq!(
            profile.annotation(),
            " | actual rows: 0, time: 0µs (incl. children)"
        );

        // Record some rows and time, then re-check the rendered annotation.
        profile.record_row();
        profile.record_row();
        profile.record_row();
        profile.record_time(5_000); // 5µs expressed in nanos
        assert_eq!(profile.actual_rows(), 3);
        assert_eq!(profile.elapsed_micros(), 5); // nanos / 1000
        assert_eq!(
            profile.annotation(),
            " | actual rows: 3, time: 5µs (incl. children)"
        );

        // Sub-microsecond time floors to 0µs (integer division).
        let sub = OpProfile::new("Empty", 0);
        sub.record_time(999);
        assert_eq!(sub.elapsed_micros(), 0);
    }

    #[test]
    fn profile_registry_preserves_push_order() {
        // The registry is a plain Vec seeded in plan-tree pre-order; assert the
        // per-operator handles keep their insertion order and identity.
        let mut registry: ProfileRegistry = Vec::new();
        for (name, depth) in [("Limit", 0usize), ("Filter", 1), ("NodeLookup", 2)] {
            registry.push(Arc::new(OpProfile::new(name, depth)));
        }

        let names: Vec<_> = registry.iter().map(|p| p.op_name()).collect();
        assert_eq!(names, vec!["Limit", "Filter", "NodeLookup"]);
        let depths: Vec<_> = registry.iter().map(|p| p.depth()).collect();
        assert_eq!(depths, vec![0, 1, 2]);
    }
}
