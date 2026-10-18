{
  "lastUpdate": 1792301097610,
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
          "id": "118fe5bed8300db6d254640c2499f992b6c62070",
          "message": "Bolt: add get_node_at_time point-in-time read benchmark (no fix) (#3829)\n\n## Summary\n\n- Adds `examples/bolt_temporal_workload.rs`, a deterministic,\npublic-API-driven benchmark for `AletheiaDB::get_node_at_time`'s full\nbi-temporal read path (`EntityTimeline` index lookup + anchor+delta\nproperty reconstruction). No existing benchmark exercised this path:\n`benches/historical_storage.rs`/`benches/property_delta.rs`\nmicrobenchmark internal functions directly, and\n`examples/bolt_workload.rs` (used by #3795/#3796/#3809/#3819) never\nbuilds version history or calls `get_node_at_time`.\n- **No `src/` changes.** This PR ships a benchmark harness only — no\nperformance fix.\n\n## Why no fix\n\nProfiling this harness under `valgrind --tool=callgrind` (500 nodes × 20\nversions, 150,000 scattered point-in-time reads) found\n`EntityTimeline::find_indices_at_point` (`src/index/temporal/mod.rs`) at\n**8.18%** of total instructions — the largest attributable cost in the\nread path. The mechanism: it binary-searches a `cutoff` correctly in\n`O(log N)`, then `.filter()`s the *entire* prefix `versions[..cutoff]`\ninstead of just the entries near the cutoff, making it effectively\n`O(cutoff)` per call.\n\nA safe fix requires augmenting `EntityTimeline` with an\ninterval-tree-style \"max end in subtree\" index so lookups stay correct\nunder overlapping/out-of-order backdated corrections (a supported\nfeature here — contradiction genealogy, belief revision) while becoming\ngenuinely `O(log N + K)`. That's a data-structure change to a type\nseveral other features depend on (cursor pagination, `AS OF` traversal,\n`temporal_extent`), which this project's Bolt process treats as an\narchitectural decision needing a human call, not something to ship\nunsupervised in a \"smallest change that moves the counter\" PR.\n\nFull writeup with the profile breakdown and a considered-but-rejected\noptimization (a smaller, ~2.34%-of-profile redundant-fetch in the\nanchor+delta chain-walk, under the 5% floor) is in #3828.\n\n## What this PR is for\n\nLanding the harness now means the 8.18% number above — and any future\nwork on it — is reproducible by anyone who checks out this branch,\nwithout redoing the setup.\n\n## Test plan\n\n- [x] `cargo fmt --all` (clean)\n- [x] `cargo build --profile bench --example bolt_temporal_workload`\nsucceeds\n- [x] Native run produces a deterministic `sink=...` line\n- [x] `cargo clippy --example bolt_temporal_workload --all-features --\n-D warnings` — clean, no warnings\n- [x] CI \"Linting\" job — passed\n- Reproduce the profile:\n  ```\n  cargo build --profile bench --example bolt_temporal_workload\nBOLT_NODES=500 BOLT_VERSIONS_PER_NODE=20 BOLT_CACHE_SIZE=200\nBOLT_READ_ITERS=150000 \\\n    valgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out -- \\\n    target/release/examples/bolt_temporal_workload\ncallgrind_annotate --threshold=100 /tmp/cg.out | grep -i\nfind_indices_at_point\n  ```\n\nCloses/references: #3828\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_01QFVfNGcEHYTFa56yepUfxY\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-10-18T00:04:36-05:00",
          "tree_id": "0598ffb0ec2a3805f1efd661def1c0d6a79d62f1",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/118fe5bed8300db6d254640c2499f992b6c62070"
        },
        "date": 1792301097609,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 16.670441540803676,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 146.07936256329404,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 174.08704184208364,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 176.49305178196465,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 143.12287557356137,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 288876.96696361253,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}