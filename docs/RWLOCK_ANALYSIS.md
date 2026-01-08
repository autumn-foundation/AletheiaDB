# RwLock Pattern Analysis for Temporal Indexes

## Executive Summary

This document analyzes whether we should adopt `DashMap<EntityId, Arc<RwLock<EntityTimelines>>>` instead of the current `DashMap<EntityId, EntityTimelines>` for read-heavy workloads like LLM reasoning queries.

**Recommendation**: Keep the current `DashMap<EntityId, EntityTimelines>` implementation for now, but consider RwLock for future optimization if benchmarks show significant read contention on the same entity.

## Current Implementation

```rust
pub struct TemporalIndexes {
    index: DashMap<EntityId, EntityTimelines>,
    config: TemporalIndexConfig,
}
```

**Locking behavior**:
- DashMap internally shards the map into N buckets (default: based on CPU count)
- Each shard has its own `RwLock` (yes, DashMap already uses RwLock internally!)
- Reads acquire a **shared read lock** on the shard
- Writes acquire an **exclusive write lock** on the shard

**Key insight**: DashMap already allows concurrent reads to the **same shard**, but only if they're accessing **different entities** within that shard.

## Proposed Alternative

```rust
pub struct TemporalIndexes {
    index: DashMap<EntityId, Arc<RwLock<EntityTimelines>>>,
    config: TemporalIndexConfig,
}
```

**Locking behavior**:
- DashMap shard lock (shared for reads, exclusive for writes)
- Then, per-entity RwLock (shared for reads, exclusive for writes)

**Key benefit**: Multiple concurrent readers can access the **same entity's timeline** without blocking each other.

## Trade-off Analysis

### Current Implementation (DashMap Direct)

| Aspect | Behavior | Performance Characteristic |
|--------|----------|---------------------------|
| **Read same entity** | Readers share shard lock | Good: Multiple readers don't block |
| **Write same entity** | Writers block readers & writers on shard | Contention only on the specific shard |
| **Read different entities** | Different shards = no contention | Excellent: Zero contention |
| **Write different entities** | Different shards = no contention | Excellent: Zero contention |
| **Memory overhead** | Direct storage | Minimal: No extra allocations |
| **Code complexity** | Simple | Low: Single level of indirection |
| **API ergonomics** | `.get()` returns `Ref<EntityTimelines>` | Good: Direct access |

### Proposed Alternative (DashMap + RwLock)

| Aspect | Behavior | Performance Characteristic |
|--------|----------|---------------------------|
| **Read same entity** | Shared RwLock on entity | Excellent: True concurrent reads |
| **Write same entity** | Exclusive RwLock on entity | Contention localized to entity |
| **Read different entities** | Different shards + different locks | Slight overhead from double locking |
| **Write different entities** | Different shards + different locks | Slight overhead from double locking |
| **Memory overhead** | Arc allocation per entity | Higher: Arc + RwLock per entity |
| **Code complexity** | Nested locking | Medium: Two levels of indirection |
| **API ergonomics** | `.get()` → `.read()` → access | Worse: Requires explicit locking |

## Workload Analysis

### Workload 1: OLTP-style (Current Primary Use Case)
- **Pattern**: Writes to many different entities, occasional reads
- **Contention**: Low (different entities → different shards)
- **Verdict**: **Current implementation is optimal**

### Workload 2: LLM Reasoning Queries (Future Use Case)
- **Pattern**: Multiple LLM instances querying the same entities repeatedly
- **Contention**: Potentially high (same entity → same shard → readers don't truly block, but have some overhead)
- **Verdict**: **RwLock pattern could help, but benefit is marginal**

Wait, let me re-check DashMap's implementation...

Actually, looking at DashMap's source code, I realize:
- DashMap uses `RwLock` at the **shard level**
- When you call `.get()`, it acquires a **shared read lock** on the shard
- Multiple readers to different entities in the same shard **share the shard's read lock**
- **This already provides concurrent read access!**

So the only scenario where RwLock pattern helps is:
- **Extreme contention**: Many threads reading the exact same entity simultaneously
- **With current impl**: All readers acquire shared lock on shard → minimal contention
- **With RwLock pattern**: All readers acquire shared lock on entity → also minimal contention

The difference is negligible in practice because:
1. Shared read locks (RwLock) don't block each other anyway
2. DashMap's sharding spreads entities across shards
3. The overhead of nested locking might negate any benefit

## Benchmark Results

### Test: `bench_concurrent_read_same_entity`
Measures multiple threads reading the same entity's timeline concurrently.

**Expected results**:
- Current impl: Good performance (DashMap's shard RwLock allows concurrent reads)
- RwLock impl: Slightly better? (One less level of indirection)

### Test: `bench_mixed_read_write_same_entity`
Simulates LLM reasoning: many readers, few writers, same entity.

**Expected results**:
- Current impl: Readers blocked when writer holds exclusive lock on shard
- RwLock impl: Readers blocked when writer holds exclusive lock on entity
- **Likely similar performance** because the bottleneck is the write lock, not the read lock

## Decision Matrix

| Criterion | Current (DashMap Direct) | Alternative (DashMap + RwLock) |
|-----------|-------------------------|-------------------------------|
| Read-heavy same entity | ✅ Good (shared shard lock) | ✅✅ Excellent (per-entity lock) |
| Read-heavy different entities | ✅✅ Excellent (no contention) | ✅ Good (double locking overhead) |
| Write-heavy different entities | ✅✅ Excellent (no contention) | ✅ Good (double locking overhead) |
| Memory efficiency | ✅✅ Excellent (no overhead) | ❌ Poor (Arc per entity) |
| Code simplicity | ✅✅ Excellent (direct access) | ❌ Medium (nested locks) |
| API ergonomics | ✅✅ Excellent (`.get()`) | ❌ Poor (`.get()?.read()`) |
| **Overall Score** | **Strong** | **Marginal benefit, higher cost** |

## Recommendation

**Keep the current `DashMap<EntityId, EntityTimelines>` implementation** for the following reasons:

1. **DashMap already provides concurrent reads** via shard-level RwLock
2. **Memory overhead**: Arc<RwLock<>> per entity adds significant overhead
3. **Code complexity**: Nested locking makes the codebase harder to maintain
4. **Marginal benefit**: The scenario where RwLock helps (extreme read contention on same entity) is rare
5. **API ergonomics**: Current API is simpler and safer

### When to Revisit This Decision

Revisit RwLock pattern if benchmarks show:
- **>50% of queries** target the same entity
- **>8 concurrent readers** per entity causing measurable contention
- **Profiling shows** DashMap shard locking as a bottleneck

### Alternative Optimizations for Read-Heavy Workloads

Instead of RwLock, consider:

1. **Caching layer**: Cache hot queries (e.g., "valid at T" for popular entities)
   - Benefits: Avoids index lookup entirely for cache hits
   - Cost: Memory overhead for cache, eviction policy complexity

2. **Read-optimized data structures**: Use immutable, lock-free structures
   - Benefits: Zero contention for reads
   - Cost: Higher write overhead (copy-on-write)

3. **Query result caching**: Cache query results with TTL
   - Benefits: Repeated queries return instantly
   - Cost: Stale reads until TTL expires

## Conclusion

The current `DashMap<EntityId, EntityTimelines>` implementation strikes an excellent balance between:
- Simplicity
- Performance
- Memory efficiency
- API ergonomics

DashMap's internal use of RwLock at the shard level already provides good concurrent read performance. The RwLock pattern would add complexity and memory overhead for marginal benefit in rare scenarios.

**Action**: Add benchmarks to monitor read contention and revisit if performance issues emerge.

## Appendix: DashMap Internal Structure

```
DashMap<K, V>
├── Shard 0 (RwLock)
│   ├── Entry (K1, V1)
│   ├── Entry (K2, V2)
│   └── ...
├── Shard 1 (RwLock)
│   ├── Entry (K3, V3)
│   └── ...
└── Shard N (RwLock)
    └── ...
```

**Key insight**: Multiple threads can:
- Read from the same shard concurrently (shared RwLock)
- Read from different shards without any locking

This already provides excellent read concurrency without the RwLock pattern.
