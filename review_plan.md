# 🦀 Core Review Plan

## Findings
- No high-severity findings.
- The use of `.min_by(|a, b| a.key().cmp(b.key()))` correctly retains a `Ref` (read lock on the shard) for the currently discovered minimum while evaluating successive elements. This technically means that `min_by` will briefly hold *two* read locks simultaneously (one for the current minimum `a`, one for the candidate `b`) while comparing their keys.
- However, since `DashMap` is sharded, taking two *read* locks concurrently is completely safe and won't cause deadlocks, even when interleaved with concurrent writes (which take write locks, but write locks only block readers on *that specific shard*). The iterators don't upgrade locks, so cycle deadlocks are avoided.

## Residual Risk / Test Gaps
- While `min_by` itself is safe and performant for finding the minimum key without `O(N)` heap allocations, it *does* mean that a read lock on the shard containing the current minimum will be held for the *entire remaining duration* of the iteration. If the `vector_indexes` map is large, holding a read lock for the entire iteration could block concurrent insertions/mutations to that specific shard for an extended period, leading to contention.
- Given that `vector_indexes` is typically small (just the vector index properties enabled, usually << 10 elements), this contention risk is practically zero.

## Actions
- The current implementation is safe, highly performant, and resolves the prior `O(N)` allocation issue. I will submit the report.
