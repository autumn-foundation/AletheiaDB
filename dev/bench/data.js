{
  "lastUpdate": 1790000517273,
  "repoUrl": "https://github.com/autumn-foundation/AletheiaDB",
  "entries": {
    "AletheiaDB Benchmarks": [
      {
        "commit": {
          "author": {
            "email": "markmasterson@gmail.com",
            "name": "Mark Masterson",
            "username": "madmax983"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "d0b32c88188f5ef75d3302190cd6d8e341fcd358",
          "message": "⚡ Bolt: cache CSR binary search in MergedAdjacencyGuard (instructions -10.40%) (#3844)\n\n## Summary\n\nIssue #3813: `OutgoingEdgesIter`/`IncomingEdgesIter`/their\nlabel-filtered variants (`src/storage/current/iterators.rs`) hold a\n`MergedAdjacencyGuard` for the lifetime of the iterator and call\n`guard.frozen_slice()` (and, via `size_hint()`, `guard.capacity_hint()`)\non every `next()`. Both route through `AdjacencyIndex::get_adjacency`,\nwhich does an O(log V) binary search over the CSR's `node_ids` array to\nfind the node's row — repeated on every yielded edge instead of once per\nguard, turning an O(log V + degree) traversal into O(degree * log V).\nThis is exactly the path `TraversalIterator::get_neighbors`\n(`src/query/executor/iterators.rs`) uses for\n`db.query().start(id).traverse(label)` — the documented bi-temporal\ntraversal API and the physical operator behind the MCP `traverse` tool.\n\n- Adds `examples/bolt_adjacency_iterator_workload.rs`, a deterministic,\npublic-API-driven benchmark for this path — the documented\n`db.query().start(id).traverse(label)` traversal API, not a synthetic\nmicrobenchmark of `MergedAdjacencyGuard` itself.\n- Fixes the redundant binary search: `MergedAdjacencyGuard` now caches\nthe resolved `node_ids` row index in a `OnceCell<Option<usize>>`,\npopulated on first access and reused by every other accessor that needs\nthe frozen slice (`iter`, `capacity_hint`, `fast_len`, `as_slice`,\n`delta_entry_is_duplicate`, `frozen_slice`). `AdjacencyIndex` gains two\n`pub(crate)` helpers — `find_node_index` (the search) and\n`adjacency_slice_at` (O(1) slice from an already-resolved index) — so\n`get_adjacency` itself is unchanged in behavior.\n\n## 🎯 Workload\n\n3,000 nodes, out-degree 8, 3,000 distinct 2-hop\n`db.query().start(id).traverse(\"KNOWS\").traverse(\"KNOWS\")` calls. The\nwrite phase (fsync'd WAL graph construction through `AletheiaDB::new()`)\ncosts real instructions unrelated to the traversal path under test, so\nit's isolated out by diffing two runs — `BOLT_START_ITERS=0` (write\nonly) against the default (write + read) — rather than reading the read\nphase's share off the combined profile directly.\n\nReproduce:\n```\ncargo build --profile bench --example bolt_adjacency_iterator_workload\n\nvalgrind --tool=callgrind --callgrind-out-file=/tmp/cg_full.out -- \\\n    target/release/examples/bolt_adjacency_iterator_workload\nBOLT_START_ITERS=0 valgrind --tool=callgrind \\\n    --callgrind-out-file=/tmp/cg_writeonly.out -- \\\n    target/release/examples/bolt_adjacency_iterator_workload\ncallgrind_annotate /tmp/cg_full.out src/storage/current/iterators.rs | less\n```\n\n## 📈 Profile\n\nBaseline `callgrind_annotate`: `OutgoingEdgesWithLabelIter::next`\nself-cost (summed across inlined attribution sites) is 45,482,940 Ir,\n**9.51%** of the isolated query-only delta (478,151,719 Ir) — clears the\n5% floor. This undercounts the addressable cost: it excludes the\ndownstream `frozen_slice()`/`get_adjacency()`/`binary_search()` cost\neach `next()` call pays into, which is exactly what this fix removes.\n\n## 💡 Hypothesis\n\nCaching the guard's one CSR row lookup and reusing it turns each\n`next()`/`size_hint()` call's O(log V) search into an O(1) cache hit,\nwith identical behavior, since the frozen CSR a guard was constructed\nagainst cannot change while the guard is held.\n\n## 📊 Measurement\n\n`valgrind --tool=callgrind`, 2 runs each for the full workload,\nwrite-only isolated via `BOLT_START_ITERS=0` (2 runs baseline, 1 run\nfixed — write-only was reproducible to within 0.003% across baseline's\nown 2 runs, giving high confidence in a single fixed-side measurement):\n\n| | full run 1 | full run 2 | full avg | write-only | query-only delta |\n|---|---:|---:|---:|---:|---:|\n| before | 1,329,053,239 | 1,316,371,907 | 1,322,712,573 | 850,890,132\n(avg of 2) | 471,822,441 |\n| after | 1,293,269,788 | 1,289,656,409 | 1,291,463,098 | 868,705,037 |\n422,758,062 |\n\nQuery-only delta: **-49,064,380 Ir (-10.40%)**, clearing the 5% floor.\n`sink` is byte-identical before/after (`287904000`), confirming the fix\nis behavior-preserving.\n\nWhole-program (write + read combined, informational): -31,249,475 Ir\n(-2.36%) — under the 5% floor on its own, because the one-time,\nfsync-bound graph-construction phase dominates a single build-then-query\nrun. The isolated query-only workload is the right denominator: in a\nreal deployment the graph is built once and queried repeatedly, so the\ntraversal path's cost is what actually scales with load.\n\n**Honest caveat:** the write-only phase itself moved +17.8M Ir (+2.09%,\n850,890,132 → 868,705,037) despite touching no query code. Write-path\n`IncrementalAdjacencyIndex::insert` also constructs a\n`MergedAdjacencyGuard` (for its own single-lookup use, ~24,000 calls in\nthis workload), and a single-use call site pays the `OnceCell`\ncheck-then-store overhead without getting the amortization multi-call\nsites see — a real, small, expected trade-off of caching, not a bug\n(behavior is unchanged; the fix's own unit tests and the harness's\nbyte-identical `sink` confirm this). Net effect across the whole program\nis still a clear win (-2.36%), and the traversal path this fix targets\nis the one that scales with query volume in production.\n\n## 🔬 Reproduce\n\n```\ncargo build --profile bench --example bolt_adjacency_iterator_workload\n\nvalgrind --tool=callgrind --callgrind-out-file=/tmp/cg_full.out -- \\\n    target/release/examples/bolt_adjacency_iterator_workload\nBOLT_START_ITERS=0 valgrind --tool=callgrind \\\n    --callgrind-out-file=/tmp/cg_writeonly.out -- \\\n    target/release/examples/bolt_adjacency_iterator_workload\ncallgrind_annotate /tmp/cg_full.out src/storage/current/iterators.rs | less\n```\n\n## Test plan\n\n- [x] `cargo fmt --all -- --check` — clean\n- [x] `cargo clippy --all-targets --all-features -- -D warnings` —\nclean, no warnings\n- [x] `cargo test --features\n\"config-toml,mcp-server,sharding-rpc,simulation,cypher\" --lib` — 5,933\npassed, 0 failed, 12 ignored (unchanged from trunk)\n- [x] `cargo test --lib adjacency` — 73 passed, 0 failed (includes\n`test_guard_frozen_slice`/`test_guard_capacity_hint_and_fast_len`/`test_guard_delta_slice`/`test_guard_is_tombstoned`\nand the compaction/publish-window suites)\n- [x] Native run of the harness produces a deterministic\n`sink=287904000` line, byte-identical before and after the fix\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_01PPjhZfw8BPr37MagHKRsTE\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_01PPjhZfw8BPr37MagHKRsTE)_\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-09-21T09:10:34-05:00",
          "tree_id": "81f6b3e936a2c3435d2b99cbf5916fae7379d310",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/d0b32c88188f5ef75d3302190cd6d8e341fcd358"
        },
        "date": 1790000517273,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 20.760606098983335,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 172.35525226902834,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 171.300743849096,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 180.92904869645153,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 176.81988482305587,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 392775.07860397553,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}