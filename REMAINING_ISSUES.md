# Remaining Issues for Temporal Index

This document tracks issues identified during code review that require more extensive changes beyond the current performance optimization scope.

## 🔴 Critical: Per-Entity Version Limits (DoS Prevention)

**Location**: `EntityTimeline::insert()` (temporal.rs lines 59-74)

**Issue**: No limit on `versions.len()` per entity. A malicious or buggy client could create millions of versions for a single entity, causing OOM.

**Recommendation**:
- Add `TemporalIndexConfig` struct with `max_versions_per_entity: usize`
- Default to 1,000,000 (reasonable for most use cases)
- Make `insert_node_version` and `insert_edge_version` return `Result<(), Error>`
- Add `StorageError::VersionLimitExceeded` variant
- Update all callsites to handle the error

**API Impact**: Breaking change - requires updating all callsites that use temporal indexes

**Example Implementation**:
```rust
pub struct TemporalIndexConfig {
    /// Maximum versions per entity (default: 1,000,000)
    pub max_versions_per_entity: usize,
}

impl TemporalIndexes {
    pub fn with_config(config: TemporalIndexConfig) -> Self { ... }

    pub fn insert_node_version(
        &self,
        node_id: NodeId,
        version_id: VersionId,
        temporal: BiTemporalInterval,
    ) -> Result<(), Error> {
        // Check limit before insert
        if self.get_version_count(EntityId::Node(node_id)) >= self.config.max_versions_per_entity {
            return Err(StorageError::VersionLimitExceeded {
                entity: format!("{:?}", node_id),
                limit: self.config.max_versions_per_entity,
            }.into());
        }
        ...
    }
}
```

---

## 🟡 Medium: End-to-End Temporal Reconstruction Benchmarks

**Location**: CLAUDE.md defines target for time-travel reconstruction (<10ms)

**Issue**: New benchmarks test index queries but not the full end-to-end paths mentioned in CLAUDE.md line 32:
- **Target**: <10ms for point-in-time reconstruction

**Recommendation**: Add benchmarks that integrate temporal index with historical storage reconstruction:
```rust
fn bench_end_to_end_reconstruction(c: &mut Criterion) {
    // 1. Setup: Create historical storage with 10K versions
    // 2. Benchmark: Full reconstruction at time T
    //    - Query temporal index
    //    - Find nearest anchor
    //    - Apply deltas
    //    - Return reconstructed state
    // 3. Assert: Completes in <10ms
}
```

**Files to modify**:
- Add new benchmark file `benches/end_to_end_temporal.rs`
- Test integration of `TemporalIndexes` + `HistoricalStorage`

---

## 🟡 Medium: Deduplication Policy Internal-Only Flag

**Location**: `EntityTimeline::insert_batch()` line 103

**Current Behavior**: `dedup_by_key(|e| e.version_id)` keeps first occurrence

**Issue**: The "first occurrence wins" policy is correct for idempotent WAL replay, but if a version is updated (different temporal intervals), callers may expect latest data to win.

**Recommendation**:
1. Keep current behavior as the default (correct for WAL replay)
2. Add an optional parameter or internal method variant:

```rust
// Public API (for WAL replay) - keeps first
fn insert_batch(&mut self, entries: Vec<TimelineEntry>) { ... }

// Internal variant (if needed) - keeps last
fn insert_batch_keep_latest(&mut self, entries: Vec<TimelineEntry>) {
    ...
    self.versions.sort_by_key(|e| e.start);
    // Reverse dedup to keep last occurrence
    self.versions.reverse();
    self.versions.dedup_by_key(|e| e.version_id);
    self.versions.reverse();
}
```

3. Document the policy more explicitly in the function contract
4. Add test cases for both behaviors

---

## 🟢 Minor: Benchmark Setup Scope

**Location**: `benches/temporal_query.rs` lines 12-31

**Issue**: Index setup happens outside `bench_with_input`, so the same index is reused across iterations. This could skew results due to cache effects.

**Fix**: Use `iter_batched` to isolate setup from measurement (already done correctly in `bench_insert_performance`):

```rust
fn bench_valid_at_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("valid_at_query");

    for version_count in [100, 1000, 10000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_versions", version_count)),
            &version_count,
            |b, &count| {
                b.iter_batched(
                    || {
                        // Setup: Create fresh index
                        let indexes = TemporalIndexes::new();
                        let node_id = NodeId::new(1).unwrap();
                        for i in 0..count {
                            indexes.insert_node_version(...);
                        }
                        (indexes, node_id, count)
                    },
                    |(indexes, node_id, count)| {
                        // Benchmark: Query
                        let query_time = (count / 2 * 1000) + 500;
                        let range = TimeRange::new(query_time, query_time + 1);
                        black_box(indexes.find_node_versions_in_valid_time_range(node_id, range))
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }
}
```

---

## 🟢 Minor: Magic Number as Named Constant

**Location**: `EntityTimeline::find_in_range()` line 114 (now line ~94 after edits)

**Issue**: Hardcoded `1000` in adaptive allocation heuristic

**Fix**: Extract as named constant with documentation:

```rust
/// Threshold for distinguishing point queries from range queries (in ticks).
/// Queries with range < POINT_QUERY_THRESHOLD are considered "point queries"
/// and pre-allocate less capacity (typically return 1-2 versions).
const POINT_QUERY_THRESHOLD_TICKS: i64 = 1000;

fn find_in_range(&self, range: TimeRange) -> Vec<VersionId> {
    ...
    let range_size = range.end() - range.start();
    let estimated_capacity = if range_size < POINT_QUERY_THRESHOLD_TICKS {
        cutoff.min(4)  // Point query or small range
    } else {
        cutoff.min(16) // Large range query
    };
    ...
}
```

---

## Priority for Next Steps

1. **Critical (🔴)**: Version limits - Requires breaking API changes, should be done in dedicated PR
2. **Medium (🟡)**: End-to-end benchmarks - Useful for validating CLAUDE.md targets
3. **Medium (🟡)**: Deduplication variants - Low priority, current behavior is correct for main use case
4. **Minor (🟢)**: Quick fixes that can be done anytime

## Notes

The current PR focuses on temporal index performance optimizations and is already comprehensive with:
- Critical memory leak fix (deduplication)
- Performance optimization (capacity pre-allocation)
- Extensive test coverage (13 tests)
- Comprehensive benchmarks (4 benchmark groups)
- Thorough documentation (complexity analysis, tradeoffs)

The remaining issues are important but should be addressed in follow-up PRs to maintain focus and reviewability.
