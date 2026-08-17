{
  "lastUpdate": 1786991913165,
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
          "id": "c6b170d181cf648461eaf652a7039c471930e15f",
          "message": "⚡ Bolt: skip empty-tombstones per-edge check in adjacency reads (instructions -10.41%) (#3796)\n\n## 🎯 Workload\n\n`examples/bolt_workload.rs` (pre-existing harness, unmodified by this PR\n— added in #3795) — a deterministic mixed read/write workload driven\nentirely through the public `AletheiaDB` API: builds a 1,000-node /\n6,000-edge social-graph-shaped dataset, enables a property-equality\nindex, then runs 3,000 read iterations of `get_node` + single-hop\n`get_outgoing_edges` + a full 3-hop traversal + `find_nodes_by_property`\n— the exact call shapes the MCP\n`get_node`/`traverse`/`find_nodes_by_property` tools take.\n\nReproduce:\n```\ncargo build --profile bench --example bolt_workload\nBOLT_NODES=1000 BOLT_OUT_DEGREE=6 BOLT_READ_ITERS=3000 \\\n  valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind.out -- \\\n  target/release/examples/bolt_workload\ncallgrind_annotate --threshold=90 /tmp/callgrind.out | head -60\n```\n\n## 📈 Profile\n\nBaseline instruction-count breakdown (411,235,187 total instructions,\ntaken fresh on trunk after #3795's identity-hash fix landed):\n\n| Self cost | Site |\n|---|---|\n| 6.20% | `malloc.c:_int_free` |\n| 4.82% | `malloc.c:malloc` |\n| 4.17% | `core::iter::adapters::filter::Filter::next` |\n| 3.08% | `malloc.c:_int_malloc` |\n| 2.47% | `dashmap … Map::_get` (hashbrown probe) |\n| 2.37% | `dashmap … Map::_get` (dashmap lock+probe) |\n| 1.74% | `core::hash::sip::Hasher::write` |\n| 1.66% | `CurrentStorage::get_outgoing_edges::{{closure}}` |\n\nSeveral more `Filter::next` and `get_outgoing_edges::{{closure}}`\nentries recur across the list at multiple inlined call sites (option.rs,\nvec/mod.rs, map.rs, chain.rs, ptr/mod.rs — all the same source closure,\ninlined differently per callsite). Summed, this cluster plus the\n`dashmap::…::_get` entries it drives is well over the 5% floor.\n\n## 💡 Hypothesis\n\n`MergedAdjacencyGuard::iter()` (`src/index/incremental_adjacency.rs`)\ngates its per-edge tombstone `DashMap::contains_key()` lookup on a\nsingle `fast_path` flag, which is only `true` when **both** delta and\ntombstones are globally empty:\n\n```rust\nlet fast_path = self.fast_path;\nlet frozen_iter = frozen_slice.iter().filter(move |e| {\n    fast_path || !self.tombstones.contains_key(&e.edge_id)\n});\n```\n\nBut `get_adjacency()`'s default compaction config (`max_delta_edges:\n10_000`, `compaction_ratio: 0.1` with `frozen > 0` required) means\n**any** graph with fewer than 10K uncompacted delta edges — i.e. most\nsmall/medium graphs, or any graph shortly after a write burst — never\nhas an empty delta, so `fast_path` is `false` for its entire\nadjacency-read lifetime, *even if zero deletes have ever been issued*.\nEvery single edge of every\n`get_outgoing_edges`/`get_incoming_edges`/traversal call then pays for a\n`DashMap::contains_key()` probe into a tombstones map that is provably\nempty the whole time. `bolt_workload.rs`'s workload matches this\nexactly: 6,000 edges (under the 10K threshold), zero deletes.\n\nThe fix is to track tombstone-emptiness independently of\ndelta-emptiness, since they're orthogonal conditions.\n\n## 🔧 Change\n\n`src/index/incremental_adjacency.rs`:\n- Added a `tombstones_empty: bool` field to `MergedAdjacencyGuard`, set\nfrom the already-computed local in `get_adjacency()` (not gated on\n`delta_empty`).\n- `iter()`'s two filter predicates now check `tombstones_empty` instead\nof `fast_path`, so the per-edge DashMap lookup is skipped whenever\ntombstones are empty, regardless of delta state.\n- `is_tombstoned()` updated the same way.\n- `fast_path` is untouched in meaning/usage for `fast_len()` (which\ngenuinely needs both conditions for its O(1) shortcut).\n- No public API change, no new dependency, no behavior change —\n`contains_key` on an empty map already returned `false`; this only\nremoves the redundant probe.\n\n## 📊 Measurement\n\n`valgrind --tool=callgrind`, identical harness/env both runs\n(`BOLT_NODES=1000 BOLT_OUT_DEGREE=6 BOLT_READ_ITERS=3000`):\n\n| | Instructions (Ir) |\n|---|---|\n| Before | 411,235,187 |\n| After | 368,431,876 |\n| **Delta** | **-42,803,311 (-10.41%)** |\n\nClears the ≥5% impact floor. The after-profile confirms the mechanism:\n`dashmap::…::Map::_get` no longer appears anywhere in the\ntop-90%-threshold list (it previously accounted for ~7 distinct entries\nsumming to several percent), while unrelated costs (malloc churn,\nSipHash on other maps, WAL drain) are unchanged run-to-run.\n\nThe harness's `sink` checksum is byte-identical before/after (`975000`),\nconfirming pure speed win, no semantic change.\n\nAlso verified:\n- `cargo fmt --all -- --check`: clean.\n- `cargo clippy --all-targets --all-features -- -D warnings`: clean.\n- `cargo test --lib`: **4580 passed, 0 failed, 10 ignored** — identical\ncount to before the change.\n- Searched open PRs for\n`tombstones_empty`/`MergedAdjacencyGuard`/`is_tombstoned`: no duplicates\nin flight. (Note: PR #3793, open, targets a different site —\nempty-DashMap *iteration* in the vector-index write hooks during CSV\nimport — unrelated code path.)\n\n## 🔬 Reproduce\n\n```\ngit fetch origin claude/compassionate-mccarthy-1iitnx\ngit checkout claude/compassionate-mccarthy-1iitnx\ncargo build --profile bench --example bolt_workload\nBOLT_NODES=1000 BOLT_OUT_DEGREE=6 BOLT_READ_ITERS=3000 \\\n  valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind_after.out -- \\\n  target/release/examples/bolt_workload\ncallgrind_annotate --threshold=90 /tmp/callgrind_after.out | head -40\n```\n\nTo reproduce the before number, `git stash` the one `src/` change and\nre-run.\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_019aHw1w4Wv5QWXbfHwDwyrR)_\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-08-17T13:27:51-05:00",
          "tree_id": "9fca6d5dcb421ea585ededc7ad10f01eaef10f86",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/c6b170d181cf648461eaf652a7039c471930e15f"
        },
        "date": 1786991913164,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 20.884766045638067,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 181.93452174338566,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 174.83526088581885,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 173.45019660884242,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 318207.26625583385,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 174.24604876961865,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}