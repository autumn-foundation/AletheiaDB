{
  "lastUpdate": 1787539294836,
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
          "id": "d339e071d9dab19e51dec84e8cc09982aa448c19",
          "message": "⚡ Bolt: eliminate Filter adapter from empty-tombstone adjacency reads (instructions -7.46%) (#3809)\n\n## 🎯 Workload\n\n`examples/bolt_workload.rs` (pre-existing harness, unmodified by this\nPR) — a deterministic mixed read/write workload driven entirely through\nthe public `AletheiaDB` API: builds a 1,000-node / 6,000-edge\nsocial-graph-shaped dataset, enables a property-equality index, then\nruns 3,000 read iterations of `get_node` + single-hop\n`get_outgoing_edges` + a full 3-hop traversal + `find_nodes_by_property`\n— the exact call shapes the MCP\n`get_node`/`traverse`/`find_nodes_by_property` tools take.\n\nReproduce:\n```\ncargo build --profile bench --example bolt_workload\nBOLT_NODES=1000 BOLT_OUT_DEGREE=6 BOLT_READ_ITERS=3000 \\\n  valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind.out -- \\\n  target/release/examples/bolt_workload\ncallgrind_annotate --threshold=90 /tmp/callgrind.out | head -60\n```\n\n## 📈 Profile\n\nBaseline instruction-count breakdown (371,679,426 total instructions,\ntaken fresh on trunk after #3796's tombstone-emptiness fix landed):\n\n| Self cost | Site |\n|---|---|\n| 6.89% | `malloc.c:_int_free` |\n| 5.33% | `malloc.c:malloc` |\n| 4.62% | `core::iter::adapters::filter::Filter::next` |\n| 3.47% | `malloc.c:_int_malloc` |\n| 3.14% | `malloc.c:free` |\n| 1.95% | `core::hash::sip::Hasher::write` |\n| 1.84% | `CurrentStorage::get_outgoing_edges::{{closure}}` (option.rs)\n|\n| 1.56% | `flatten.rs: filter::Filter::next` |\n\nSeveral more `Filter::next` entries recur at different inlined call\nsites (slice/iter/macros.rs, iterator.rs, ptr/mut_ptr.rs — all the same\nsource predicate, inlined per callsite). Summed across all sites, the\n`Filter` adapter cluster alone accounts for ~7.25% of total\ninstructions, clearing the 5% floor on its own.\n\n## 💡 Hypothesis\n\n`MergedAdjacencyGuard::iter()` (`src/index/incremental_adjacency.rs`),\neven after #3796 made the tombstone-DashMap probe itself skippable,\nstill wraps both the frozen slice and delta slice in a `Filter` adapter\nwhose predicate is `tombstones_empty ||\n!self.tombstones.contains_key(...)`. When `tombstones_empty` is true\n(the common case — bolt_workload issues zero deletes, matching most\nfreshly-loaded or bulk-imported graphs), the predicate is provably\nalways `true`, but the compiler still emits and executes the\n`Filter::next` adapter machinery — the branch, the closure call, and the\niterator-vtable-shaped dispatch — once per adjacency entry, across every\n`get_outgoing_edges`/`get_incoming_edges`/traversal call.\n\nThe fix moves the branch outside the per-edge loop: decide once per\n`iter()` call whether tombstones are empty, and return either a raw\n`Chain` iterator (no `Filter` at all) or a `Filter`-wrapped one, via a\nsmall two-variant enum implementing `Iterator`. The always-true\npredicate per element is replaced with a single branch per call.\n\n## 🔧 Change\n\n`src/index/incremental_adjacency.rs`:\n- Added `AdjacencyIter<U, F>`, a two-variant enum\n(`Unfiltered`/`Filtered`) implementing `Iterator` by delegating to\nwhichever variant is active.\n- `MergedAdjacencyGuard::iter()` now builds the frozen+delta `Chain`\nonce, then wraps it in `AdjacencyIter::Unfiltered` when\n`tombstones_empty`, or `AdjacencyIter::Filtered` (with the\nDashMap-probing filter) otherwise — reusing the existing `delta_slice()`\nhelper instead of the previous `Option::into_iter().flat_map()`\nindirection.\n- No public API change (still `impl Iterator<Item = &AdjacencyEntry>`),\nno new dependency, no behavior change: the filtered branch is\nbyte-for-byte the same predicate as before, just applied only when\nneeded.\n\n## 📊 Measurement\n\n`valgrind --tool=callgrind`, identical harness/env both runs\n(`BOLT_NODES=1000 BOLT_OUT_DEGREE=6 BOLT_READ_ITERS=3000`):\n\n| | Instructions (Ir) |\n|---|---|\n| Before | 371,679,426 |\n| After | 343,958,936 |\n| **Delta** | **-27,720,490 (-7.46%)** |\n\nClears the ≥5% impact floor. The after-profile confirms the mechanism:\n`filter::Filter::next` no longer appears anywhere in the\ntop-90%-threshold `callgrind_annotate` output at all (fully eliminated),\nwhile unrelated costs (malloc churn, SipHash, WAL drain, `crc32fast`)\nare unchanged run-to-run.\n\nThe harness's `sink` checksum is byte-identical before/after (`975000`),\nconfirming pure speed win, no semantic change.\n\nAlso verified:\n- `cargo fmt --all -- --check`: clean.\n- `cargo clippy --all-targets --all-features -- -D warnings`: clean.\n- `cargo test --lib`: **4618 passed, 0 failed, 10 ignored** — identical\nto before the change.\n- Searched open PRs for\n`MergedAdjacencyGuard`/`incremental_adjacency`/`AdjacencyIter`/`get_outgoing_edges`:\nno duplicates in flight (#3793, open, targets a different site —\nempty-DashMap *iteration* in vector-index write hooks during CSV import\n— unrelated code path).\n\n## 🔬 Reproduce\n\n```\ngit fetch origin claude/compassionate-mccarthy-5txakg\ngit checkout claude/compassionate-mccarthy-5txakg\ncargo build --profile bench --example bolt_workload\nBOLT_NODES=1000 BOLT_OUT_DEGREE=6 BOLT_READ_ITERS=3000 \\\n  valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind_after.out -- \\\n  target/release/examples/bolt_workload\ncallgrind_annotate --threshold=90 /tmp/callgrind_after.out | head -40\n```\n\nTo reproduce the before number, `git stash` the one `src/` change and\nre-run.\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_01Um7YPBhhz62uopaCNEwsQp)_\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-08-23T21:28:24-05:00",
          "tree_id": "ca463b0ab05d6a0445ef59b281cd8e2908db6f67",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/d339e071d9dab19e51dec84e8cc09982aa448c19"
        },
        "date": 1787539294835,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 21.188718927517968,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 199.2841859474017,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 192.55455412494996,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 186.38223274332296,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 321102.5559961553,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 171.21158981226944,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}