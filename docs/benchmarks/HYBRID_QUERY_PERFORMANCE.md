# Hybrid Query Performance Characteristics

**Generated**: [Run date goes here]
**System**: [CPU, RAM, OS information goes here]
**Benchmark Suite**: VS-070 Hybrid Query Benchmarks

## Overview

This document captures performance characteristics of the hybrid query API,
including traverse_and_rank, temporal vector search, and composition patterns.

## traverse_and_rank

### Scaling Characteristics

- **Node count**: Linear O(N) where N = number of neighbors
- **k value**: O(N log k) with min-heap vs O(N log N) with full sort
- **Vector dimension**: Linear in similarity computation

### Topology Impact

| Topology | 100 nodes | 1K nodes | 10K nodes |
|----------|-----------|----------|-----------|
| Uniform (20 edges) | TBD | TBD | TBD |
| Power-law (mixed) | TBD | TBD | TBD |
| Sparse (2-5 edges) | TBD | TBD | TBD |

*Note: Run `cargo bench --bench hybrid_query` to populate these values*

### Performance vs Baselines

- Hybrid API: TBD µs
- Sequential (separate queries): TBD µs (TBD x slower)
- Naive composition: TBD µs (TBD x slower)

**Winner**: TBD

## find_similar_as_of

### Temporal Reconstruction Overhead

| Snapshot depth | Recent (10) | Medium (50) | Deep (100) |
|---------------|-------------|-------------|------------|
| Latency | TBD ms | TBD ms | TBD ms |

### Anchor+Delta Efficiency

- **Target**: <10ms reconstruction (from CLAUDE.md)
- **Actual** (100 snapshots deep): TBD ms
- **Status**: [PASS/FAIL]

## Full Hybrid Queries

### Chained Operations

| Pattern | Latency |
|---------|---------|
| Traverse → Rank → Filter → Temporal | TBD ms |
| Multi-hop traversal (2 hops) | TBD ms |

### Memory Overhead

| Approach | Memory Usage |
|----------|--------------|
| Streaming (hybrid) | TBD MB |
| Naive load-all | TBD MB |

## Query Optimization Overhead

### Cache Effects

| Cache state | Latency |
|-------------|---------|
| Cold cache | TBD µs |
| Warm cache | TBD µs (TBD x faster) |

## Instructions for Populating Data

1. Run full benchmark suite:
   ```bash
   cargo bench --bench hybrid_query
   ```

2. Results are in `target/criterion/*/report/index.html`

3. Extract key metrics and update this document

4. Compare against performance targets in `CLAUDE.md`

## Performance Targets

From `CLAUDE.md`:
- Current-state single-hop: <1µs
- Current-state 3-hop: <100µs
- Time-travel reconstruction: <10ms

## References

- Design: `docs/plans/2026-01-13-hybrid-query-benchmarks-design.md`
- Issue: #83 (VS-070)
- Benchmark code: `benches/hybrid_query.rs`
