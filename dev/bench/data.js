{
  "lastUpdate": 1790469878751,
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
          "id": "e3536a25a0017b69d33aa44e5e7718645c739423",
          "message": "⚡ Bolt: skip base-copy for keys PropertyDelta::apply overwrites (instructions -6.31%) (#3848)\n\n## Summary\n\n`PropertyDelta::apply` (`src/core/version.rs`) is the inner loop of\nanchor+delta bi-temporal reconstruction\n(`HistoricalStorage::reconstruct_node_properties_iterative`/`reconstruct_edge_properties_iterative`,\n`src/storage/historical/mod.rs`), walked once per delta on every\n`get_node_at_time`/`get_edge_at_time`/`get_node_history` call and during\ncheckpoint/index-persistence property replay.\n\nIts base-copy loop unconditionally inserted every `base` property (an\nArc clone + a `HashMap` insert) before the `changed` loop ran a few\nlines later and overwrote exactly the keys that had just changed —\npaying for an Arc clone, two `HashMap` inserts, and a drop of the\ndiscarded first value, for every property a version's update actually\ntouched, instead of one insert.\n\n## 🎯 Workload\n\n`examples/bolt_temporal_reconstruction_workload.rs` (pre-existing\nharness, unmodified) — 500 nodes, each updated 49 times (1 anchor +\n49-version delta chain), each update changing 9 of the record's 10\nproperties. Query phase: one `get_node_at_time` call per recorded\nversion (25,000 total), each resolving to a version never reconstructed\nbefore, so the reconstruction cache never masks the chain-walk cost.\n\n```\ncargo build --profile bench --example bolt_temporal_reconstruction_workload\nALETHEIADB_ADJACENCY_MAINTENANCE=off valgrind --tool=callgrind \\\n    --callgrind-out-file=/tmp/cg.out -- \\\n    target/release/examples/bolt_temporal_reconstruction_workload\ncallgrind_annotate --threshold=80 /tmp/cg.out | head -40\n```\n\n## 📈 Profile\n\nBaseline (trunk HEAD `3c94ca3`), `valgrind --tool=callgrind`, mean of 2\nruns (10,084,095,335 / 10,106,098,547 Ir, <0.22% spread):\n**10,095,096,941 Ir**. `callgrind_annotate --threshold=80` attributes\n~35% of the profile to `hashbrown::map::HashMap::insert` fragments\nacross inlined call sites, plus\n`PropertyValue::drop`/`drop_in_place::<PropertyValue>` at 4.97% + 4.68%\n— directly attributable to the wasted overwrite-after-insert this change\nremoves.\n\n## 💡 Hypothesis\n\nChecking `self.changed.contains_key(key)` (an existing `FastHashMap`,\nO(1) lookup) while copying `base` properties lets the loop skip\ninserting a value that gets immediately replaced a few lines later,\nturning \"insert base value, then insert replacement value\" into just\n\"insert replacement value\" for every property an update actually\ntouched. `vector_deltas` keys are deliberately **not** skipped:\n`VectorDelta::Sparse` is fail-open (if the base property is missing or\nnot a vector, its loop leaves the base value untouched — see\n`test_property_delta_apply_sparse_ignored_on_wrong_type`), so that map's\nbase entry must stay available as the fallback. An earlier version of\nthis change also skipped `vector_deltas` keys and broke exactly that\ntest — caught by `cargo test --lib version::` before this was measured\nor shipped.\n\n## 🔧 Change\n\nTwo-line condition change in `PropertyDelta::apply`'s base-copy loop:\n`!self.removed.contains(key)` gains `&&\n!self.changed.contains_key(key)`. No other call site changes. Output is\nidentical: skipped keys are exactly the ones the `changed` loop\noverwrites to the same final value either way.\n\n## 📊 Measurement\n\n`valgrind --tool=callgrind`, `ALETHEIADB_ADJACENCY_MAINTENANCE=off`\npinned, 2 runs each side:\n\n| | run 1 | run 2 | mean | Δ vs. baseline |\n|---|---:|---:|---:|---:|\n| Before (trunk `3c94ca3`) | 10,084,095,335 | 10,106,098,547 |\n10,095,096,941 | — |\n| After (this change) | 9,459,887,306 | 9,456,567,738 | 9,458,227,522 |\n**-6.31%** |\n\nRun-to-run spread: 0.22% before, 0.035% after — both well under the\n-6.31% delta. `sink=250000` (and `reconstructions=25000`) is\nbyte-identical across all 4 runs, confirming the fix is\nbehavior-preserving. Clears the ≥5% instruction floor.\n\n`valgrind --tool=dhat`, 1 run each side (informational — allocation\ncount/bytes aren't the claim here, since an `Arc`\nclone-then-immediate-drop on refcount > 1 neither allocates nor\ndeallocates):\n\n| | Before | After | Δ |\n|---|---:|---:|---:|\n| Total blocks | 6,599,969 | 6,596,491 | -0.05% (below the ≥10% alloc\nfloor — not claimed) |\n| Total bytes | 2,155,306,928 | 2,154,005,944 | -0.06% (below the ≥10%\nalloc floor — not claimed) |\n| Read traffic | 4,802,812,436 B | 4,308,987,997 B | -10.3% |\n| Write traffic | 2,347,799,450 B | 2,060,931,321 B | -12.2% |\n\nThe read/write memory-traffic drop corroborates the instruction count\nwithout itself being the gating metric.\n\n## 🔬 Reproduce\n\n```\ncargo build --profile bench --example bolt_temporal_reconstruction_workload\n\nALETHEIADB_ADJACENCY_MAINTENANCE=off valgrind --tool=callgrind \\\n    --callgrind-out-file=/tmp/cg_after.out -- \\\n    target/release/examples/bolt_temporal_reconstruction_workload\ncallgrind_annotate --threshold=80 /tmp/cg_after.out | head -40\n\n# before: checkout trunk HEAD, rebuild, repeat the run\n```\n\n## Other commits on this branch\n\nThis branch also carries 5 earlier Bolt cycles that were committed but\nnever got a PR opened (`b54792c`, `d0b32c8`, `cf23e9c`, `a4695d8`,\n`3c94ca3` — each already measured and documented in its own commit\nmessage: collapsing a double version-fetch in the same reconstruction\npath, caching a CSR binary search, and three more identity-hash\nconversions). They're included here since they're part of this branch's\ndiff against `trunk`; this PR's own new work is the commit above.\n\n## Test plan\n\n- [x] `cargo fmt --all -- --check` — clean\n- [x] `cargo clippy --all-targets --all-features -- -D warnings` —\nclean, no warnings\n- [x] `cargo test --lib version::` — 67 passed, 0 failed (includes\n`test_property_delta_apply_sparse_ignored_on_wrong_type`, which caught\nan incorrect first version of this change)\n- [x] `cargo test --features\n\"config-toml,mcp-server,sharding-rpc,simulation,cypher\" --lib` — 5,933\npassed, 0 failed, 12 ignored (unchanged from trunk)\n- [x] Native run of the harness: `sink=250000` byte-identical\nbefore/after\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_01UeXh22R6P8G8iPSUVk1WUs\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_01UeXh22R6P8G8iPSUVk1WUs)_\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-09-26T19:33:47-05:00",
          "tree_id": "0bfc5c7ce3aa53aac4afcbb2f857fe2ce8cdeb52",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/e3536a25a0017b69d33aa44e5e7718645c739423"
        },
        "date": 1790469878750,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 21.21889791039847,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 172.61315909715813,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 174.73810233315206,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 181.39512113728352,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 185.40368615583833,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 388868.48562421463,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}