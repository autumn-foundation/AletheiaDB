{
  "lastUpdate": 1790093686758,
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
          "id": "cf23e9c8a9803b84f73d1659d614d1b18ba190f9",
          "message": "⚡ Bolt: identity-hash TraversalIterator's visited NodeId set (instructions -5.47%) (#3845)\n\n## 🎯 Workload\n\n`examples/bolt_aql_workload.rs` (new) — a deterministic,\npublic-API-driven benchmark for `AletheiaDB::execute_aql`'s full\nstring-in path (parse → `QueryPlanner::plan` →\n`QueryExecutor::execute`). No existing benchmark exercises this:\n`bolt_cypher_workload.rs` profiles the sibling `execute_cypher` path,\nbut that routes through an entirely different, `cypher`-feature-gated\nevaluator (`src/cypher/multi_pattern.rs`). AQL is always available (no\nfeature flag) and is the `query` MCP tool's default/fallback language\n(`language: \"aql\"`), going through `crate::query::parse_query` and the\ncost-based `src/query/planner` + `src/query/executor` — code the Cypher\nworkload never touches. `execute_aql` re-parses its input and builds a\nfresh `QueryPlanner`/`QueryExecutor` on every call, the same shape an\nLLM caller or the MCP `query` tool uses (no prepared-statement cache).\n\nWorkload: a 300-node/1,800-edge social-graph-shaped dataset (mirrors\n`bolt_workload.rs`/`bolt_cypher_workload.rs` for comparability), then\n200 iterations of the same statement text re-issued with a varying\nliteral:\n\n```\nMATCH (n:Person)-[:KNOWS]->(f:Person) WHERE n.age = $i RETURN f.name, f.age ORDER BY f.age DESC LIMIT 10\n```\n\nReproduce:\n```\ncargo build --profile bench --example bolt_aql_workload\nBOLT_NODES=300 BOLT_OUT_DEGREE=6 BOLT_QUERY_ITERS=200 \\\n  valgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out -- \\\n  target/release/examples/bolt_aql_workload\ncallgrind_annotate --threshold=90 /tmp/cg.out | head -60\n```\n\n## 📈 Profile\n\n`valgrind --tool=callgrind` 3.22.0, this session, unpatched baseline:\n**728,649,180 Ir** (mean of 2 runs, <0.01% spread).\n\n`callgrind_annotate --threshold=90` top entries: `memcpy` 6.68%,\n`_int_free` 5.08%, `sip::Hasher::write` 4.80%, then a long tail of\n`TraversalIterator::next` fragments (attributed across `iterators.rs`\nitself plus inlined `hashbrown`, `VecDeque`, `ptr`/`slice` library code\n— 27.7% + 12.8% + 11.0% + 7.1% + ... ≈ **17.3%** of the profile summed)\nand `DashMap::_get` fragments (`dashmap/lib.rs`, `hashbrown-0.14.5`,\n`sip.rs`, `intrinsics` ... ≈ **16.3%** summed, largely SipHash-family\ncost).\n\nFollowing the `TraversalIterator::next` fragments to source: `visited:\nHashSet<NodeId>` (the BFS node-distinct-reachability set),\nrebuilt/cleared once per traversed start node and probed via `.insert()`\nonce per candidate neighbor edge — on this single-hop query that's ~300\nclears + ~2,100 neighbor inserts per query call × 200 calls, each paying\nfull SipHash (the default `HashSet` hasher) on an already-unique 64-bit\nid.\n\n## 💡 Hypothesis\n\n`NodeId` is an internal, sequential id (never untrusted input) — exactly\nthe case `src/core/hasher.rs::IdHashBuilder` exists for, and already the\nblessed builder for every `NodeId`/`EdgeId`-keyed `DashMap` in `src/`\n(Issue #3795, `-27.9%` on its own workload). SipHash on an\nalready-unique integer key buys nothing and costs real cycles;\n`IdHashBuilder`'s `IdHasher` avoids it while still finalizing with one\nmultiply so `hashbrown`'s SIMD control byte (derived from the hash's\nhigh bits) stays well-distributed — a raw identity pass-through would\ncollapse it (see that module's docs). This is the same mechanism as\n#3795, applied to a `std::collections::HashSet` on a per-query hot path\ninstead of a `DashMap`.\n\n## 🔧 Change\n\n`src/query/executor/iterators.rs`: swap `TraversalIterator::visited`\nfrom `HashSet<NodeId>` (default `RandomState`) to `HashSet<NodeId,\nIdHashBuilder>`. One field-type annotation, one constructor line\n(`HashSet::new()` → `HashSet::default()`); `.insert()`/`.clear()` are\nhasher-generic, so no other call site changes. Behavior is unchanged\n(same set semantics), verified by the native workload's `sink` value and\nby the existing test suite.\n\n## 📊 Measurement\n\n`valgrind --tool=callgrind`, mean of 2 runs each side, same\n`bolt_aql_workload.rs`, `BOLT_NODES=300 BOLT_OUT_DEGREE=6\nBOLT_QUERY_ITERS=200`:\n\n| | Ir (run 1) | Ir (run 2) | Mean | Δ vs. baseline |\n|---|---:|---:|---:|---:|\n| Before (trunk) | 728,624,891 | 728,673,469 | 728,649,180 | — |\n| After (`IdHashBuilder`) | 688,649,204 | 688,892,271 | 688,770,738 |\n**-5.47%** |\n\nRun-to-run spread is <0.04% on each side, so -5.47% is signal, not noise\n— clearing this process's ≥5% instruction-count impact floor.\n\n**Verified:**\n- [x] `cargo fmt --all -- --check` — clean\n- [x] `cargo clippy --all-targets --all-features -- -D warnings` —\nclean, no warnings\n- [x] `cargo test --lib --all-features query::` — 858 passed, 0 failed\n- [x] `cargo test --all-features --test aql_temporal_join --test\naql_temporal_window --test edge_property_predicates` — 118 passed, 0\nfailed\n- [x] `cargo test --lib --all-features query::executor::` — 277 passed,\n0 failed\n- [x] Native run's `sink` value byte-identical before/after (`sink=2000\nnodes=300 edges=1800 query_iters=200`)\n\n## 🔬 Reproduce\n\n```\ngit checkout <first commit of this PR>   # benchmark-only, unpatched\ncargo build --profile bench --example bolt_aql_workload\nBOLT_NODES=300 BOLT_OUT_DEGREE=6 BOLT_QUERY_ITERS=200 \\\n  valgrind --tool=callgrind --callgrind-out-file=/tmp/cg_before.out -- \\\n  target/release/examples/bolt_aql_workload\n\ngit checkout <second commit of this PR>  # the fix\ncargo build --profile bench --example bolt_aql_workload\nBOLT_NODES=300 BOLT_OUT_DEGREE=6 BOLT_QUERY_ITERS=200 \\\n  valgrind --tool=callgrind --callgrind-out-file=/tmp/cg_after.out -- \\\n  target/release/examples/bolt_aql_workload\n```\n\n## Duplicate-work check\n\nSurveyed open PRs before starting: #3822 (adjacency-iterator CSR binary\nsearch hoist) and #3823 (`TraversalIterator` arena-backed path\nmaterialization) both touch performance-sensitive code, but neither\ntouches the `visited` field or its hasher — #3823 inserts an unrelated\nnew field directly above `visited` without modifying that line, so this\ndiff should apply/rebase cleanly against it.\n\n---\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_01SrweitrrPoxwzjAZd1RZYT\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_01SrweitrrPoxwzjAZd1RZYT)_\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-09-22T11:05:10-05:00",
          "tree_id": "b80f92551519c85ad9e870860f35b07b90873271",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/cf23e9c8a9803b84f73d1659d614d1b18ba190f9"
        },
        "date": 1790093686757,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 19.690897062363785,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 120.80863133402572,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 131.17326632137366,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 150.58634827507186,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 132.08843206252214,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 359745.93726009206,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}