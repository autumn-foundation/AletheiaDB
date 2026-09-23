{
  "lastUpdate": 1790169010965,
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
          "id": "a4695d80cfed1da9827fe93c6299891af43eb2d1",
          "message": "⚡ Bolt: identity-hash VersionId-keyed maps in LineageStore (instructions -15.49%) (#3846)\n\n## 🎯 Workload\n\n`examples/bolt_lineage_workload.rs` (new, first commit on this branch) —\na deterministic, public-API-only benchmark for `LineageStore`'s\nderivation-lineage closures (`AletheiaDB::create_node_with_lineage` /\n`upstream_lineage` / `downstream_lineage`, Issue #3371). No existing\n`bolt_*.rs` harness touched this module; `benches/derivation_lineage.rs`\nis criterion-only, and criterion wall-clock timing is inadmissible on\nthis shared-vCPU machine on its own.\n\nBuilds a 5-level x 2,000-facts/level (10,000-fact) citation DAG\n(matching `benches/derivation_lineage.rs`'s own sizing), then runs the\ntwo realistic access patterns the module's docs describe: 10x full-graph\n`downstream_lineage` blast-radius queries from the root, 10x from a\nmid-graph node, and 2,000x single-hop `upstream_lineage` citation\nlookups.\n\nReproduce:\n```\ncargo build --profile bench --example bolt_lineage_workload\nvalgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out -- \\\n  target/release/examples/bolt_lineage_workload\ncallgrind_annotate --threshold=99 /tmp/cg.out | head -60\n```\n\n## 📈 Profile\n\n`valgrind --tool=callgrind` 3.22.0, this session, unpatched baseline\n(mean of 2 runs): **521,205,470 Ir**.\n\n`callgrind_annotate --threshold=99` top entries: `_int_free` 8.00%,\n`malloc` 6.38%, **`core::hash::sip::Hasher::write` 5.60%**, `free`\n3.80%, `memcpy` 3.70%, `_int_malloc` 3.46%, then `LineageStore::closure`\n(hashbrown-attributed) 1.76%, `RawTable::reserve_rehash` 1.54%,\n`BuildHasher::hash_one` 1.51%, `DashMap::_get` 1.31%,\n`neighbours_downstream` fragments (0.66%+0.61%+0.53%+0.45%+...),\n`LineageStore::record` fragments. Summing every SipHash-attributed line\n(`sip.rs`/`BuildHasher::hash_one` self-cost, excluding the unrelated\nstring-keyed `ConstraintRegistry` consumer) comes to **~13% of the\nprofile** — all of it driven by `LineageStore`'s two `DashMap`s and BFS\n`HashSet`/`HashMap` locals, since every other internal-id-keyed map in\nthe tree already uses `IdHashBuilder` (#3795/#3844/#3845).\n\n## 💡 Hypothesis\n\n`VersionId` (`src/core/id.rs`) is an already-unique,\nsequentially-allocated internal id (`version_id_gen`, an atomic\n`IdGenerator`), never derived from untrusted input — exactly the case\n`IdHashBuilder` (`src/core/hasher.rs`) exists for, and already the\nblessed builder for `NodeId`/`EdgeId`-keyed maps throughout\n`src/index/`, `src/core/interning.rs`, `src/core/namespace.rs`, and\n`src/query/executor/iterators.rs`'s own BFS visited set (#3795 -27.9%,\n#3844 -10.40%, #3845 -5.47%). `LineageStore` (`src/core/lineage.rs`) was\nnever converted: its `upstream`/`downstream` `DashMap`s and four\n`HashSet`/`HashMap` locals (`closure()`'s visited set,\n`upstream_path_to()`'s visited set + parents map, `record()`'s\nsource-dedup set) all used the default hasher (SipHash via\n`RandomState`). Hashing an already-unique internal integer with SipHash\nis pure overhead with zero benefit — same mechanism as the three prior\nPRs, just applied to a module they hadn't reached yet.\n\n## 🔧 Change\n\n`src/core/lineage.rs`:\n- `upstream: DashMap<VersionId, LineageRecord>` → `DashMap<VersionId,\nLineageRecord, IdHashBuilder>`\n- `downstream: DashMap<VersionId, Vec<VersionId>>` → `DashMap<VersionId,\nVec<VersionId>, IdHashBuilder>`\n- `closure()`'s `visited: HashSet<VersionId>` and\n`frontier_has_unvisited_neighbour`'s matching parameter type →\n`HashSet<VersionId, IdHashBuilder>`\n- `upstream_path_to()`'s `visited`/`parents` (used by `record()`'s cycle\nguard, on the write path) → `IdHashBuilder`-hashed\n- `record()`'s source-dedup `seen: HashSet<VersionId>` →\n`IdHashBuilder`-hashed\n\nNo allocation-pattern change (pure hasher swap), no new dependency\n(`IdHashBuilder` already exists and is already used for this exact\npattern elsewhere), no public API change (all touched fields/locals are\nprivate).\n\n## 📊 Measurement\n\n`valgrind --tool=callgrind`, mean of 2 runs each side, same\n`bolt_lineage_workload.rs`, `BOLT_LEVELS=5 BOLT_PER_LEVEL=2000`\n(defaults):\n\n| | run 1 | run 2 | mean | Δ vs. baseline |\n|---|---:|---:|---:|---:|\n| Before (trunk) | 521,530,209 | 520,880,730 | 521,205,470 | — |\n| After (`IdHashBuilder`) | 440,956,819 | 439,965,045 | 440,460,932 |\n**-15.49%** |\n\nRun-to-run spread is ~0.13% (before) / ~0.23% (after) — far below the\n15.49% delta, so this is signal, not noise. Clears this process's ≥5%\ninstruction-count impact floor by a 3x margin. Allocation behavior is\nuntouched by design (pure hasher swap, no dhat claim made).\n\n**Verified:**\n- [x] `cargo fmt --all -- --check` — clean\n- [x] `cargo clippy --all-targets --all-features -- -D warnings` —\nclean, no warnings\n- [x] `cargo test --lib lineage::` — 17/17 passed\n- [x] `cargo test --test derivation_lineage` — 16/16 passed\n- [x] Native run's `sink` value byte-identical before/after\n(`sink=102020 levels=5 per_level=2000 total_facts=10000\ndownstream_queries=10 upstream_queries=2000`)\n\n## 🔬 Reproduce\n\n```\ngit checkout <first commit of this PR>   # benchmark-only, unpatched\ncargo build --profile bench --example bolt_lineage_workload\nvalgrind --tool=callgrind --callgrind-out-file=/tmp/cg_before.out -- \\\n  target/release/examples/bolt_lineage_workload\n\ngit checkout <second commit of this PR>  # the fix\ncargo build --profile bench --example bolt_lineage_workload\nvalgrind --tool=callgrind --callgrind-out-file=/tmp/cg_after.out -- \\\n  target/release/examples/bolt_lineage_workload\n```\n\n## Duplicate-work check\n\nSurveyed open PRs/issues on `autumn-foundation/aletheiadb` before\nstarting and again before opening this PR: no open PR or issue touches\n`src/core/lineage.rs` or `LineageStore`'s hashing.\n\n---\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_014AjSLdTQn8DUXwjiVV3bPQ\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_014AjSLdTQn8DUXwjiVV3bPQ)_\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-09-23T07:59:15-05:00",
          "tree_id": "7d0488b56f8529a699e8da3f8181ad2835575705",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/a4695d80cfed1da9827fe93c6299891af43eb2d1"
        },
        "date": 1790169010965,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 21.707206344321705,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 174.47734170588436,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 172.19934924745962,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 180.7764147018941,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 173.40746613764009,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 387592.12172511517,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}