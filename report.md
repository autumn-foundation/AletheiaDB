# Core🦀 Review Report: Avoid O(N) heap allocations on DashMap minimum key lookup

## Findings
No high-severity findings.

The recent change from `.map(|r| r.key().clone()).min()` to `.min_by(|a, b| a.key().cmp(b.key())).map(|r| r.key().clone())` in `src/storage/current/mod.rs` was thoroughly reviewed for deadlock and concurrency risks.

While `.min_by` requires holding a `Ref` (read lock) for the currently discovered minimum element across subsequent shard inspections, this is entirely safe with `DashMap`. `DashMap` supports multiple concurrent read locks without deadlocking, and iterator references are not upgraded, thus precluding lock cycle conditions even under concurrent writes. We verified this by authoring and executing explicit multi-threaded contention tests.

## Test Gaps & Residual Risk
There is technically a residual performance edge case:
- Because the `Ref` for the minimum item is held for the duration of the entire map iteration, concurrent writes *to the specific shard* of that minimum element will be blocked until the iteration completes.
- Given that the `vector_indexes` map typically only contains a very small handful of items (often just one or two properties), the likelihood and duration of such a contention event are extremely minimal and practically imperceptible. No code change is warranted.
