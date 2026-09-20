{
  "lastUpdate": 1789932950855,
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
          "id": "b54792ce99ae4121404d9b687fa71a235bcb85a1",
          "message": "⚡ Bolt: collapse double version-fetch in anchor+delta reconstruction (instructions -8.72%, allocs -23.89%) (#3843)\n\n## Summary\n\n- Adds `examples/bolt_temporal_reconstruction_workload.rs`, a\ndeterministic, public-API-driven benchmark for\n`AletheiaDB::get_node_at_time`'s anchor+delta reconstruction path\n(`HistoricalStorage::reconstruct_node_properties_iterative`) — the\nmechanism behind CLAUDE.md's \"<10ms temporal reconstruction\" target and\n\"Storage Efficiency: Anchor+delta compression\" principle. No existing\n`examples/bolt_*.rs` harness drove this path.\n- Fixes a real double-fetch in `reconstruct_node_properties_iterative` /\n`reconstruct_edge_properties_iterative`: the function walked a version\nchain backwards to find its anchor (storing only each `VersionId`), then\nwalked the **same chain forward a second time**, re-fetching every one\nof those same versions by id to apply deltas.\n`get_node_version_any_tier`/`get_edge_version_any_tier` clones the full\n`NodeVersion`/`EdgeVersion` (a `Delta`'s two internal `HashMap`s +\n`HashSet` included) out of hot storage into a fresh `Arc` on every call\n— so every version in the chain was deep-cloned **twice**.\n\n## 🎯 Workload\n\n500 nodes, each carrying a 50-version anchor+49-delta chain\n(`anchor_interval` configured to 50 — longer than the engine's default\nof 10 — so the delta-chain-walk cost isn't diluted by the write phase in\nthe profile). One `get_node_at_time` call per recorded version, using\nthat version's own bi-temporal interval start as the query point, so\nevery call is a reconstruction-cache miss into a full chain walk (25,000\nreconstructions, average backward-walk depth ~25.5). Reproduce:\n\n```\ncargo build --profile bench --example bolt_temporal_reconstruction_workload\nvalgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out -- \\\n    target/release/examples/bolt_temporal_reconstruction_workload\ncallgrind_annotate /tmp/cg.out src/storage/historical/mod.rs | less\n```\n\n## 📈 Profile\n\n`callgrind_annotate` on `src/storage/historical/mod.rs` (baseline,\nbefore the fix) showed three call sites for `get_node_version_any_tier`\ninside `reconstruct_node_properties_iterative`:\n\n- backward-walk call site: 662,292,218 Ir (5.99% of the run's 11.05B\ntotal), 1,056,748 calls\n- explicit anchor re-fetch: 6,685,560 Ir, 37,142 calls\n- forward-apply loop re-fetch: 655,606,814 Ir, 1,019,606 calls\n\nThe two redundant sites sum to 662,292,374 Ir — **5.99% of the total\nprofile**, matching the backward-walk cost almost exactly, exactly as\nexpected for a chain where every version is fetched twice. This clears\nthe process's 5%-of-profile bar for \"worth changing.\"\n\n(Related: automated audit PR #1141, closed unmerged, independently\nflagged this exact \"O(2N) lookups\" shape in its findings doc without\nshipping a fix.)\n\n## 💡 Hypothesis\n\nCaching each already-fetched `Arc<NodeVersion>`/`Arc<EdgeVersion>` from\nthe backward walk and reusing it in the forward-apply pass (and for the\nanchor's base properties) — instead of re-fetching by id — collapses two\npasses' worth of hot-storage fetches into one, with identical behavior.\n\n## 🔧 Change\n\n`src/storage/historical/mod.rs`: `reconstruct_node_properties_iterative`\nand `reconstruct_edge_properties_iterative` now collect\n`Vec<Arc<NodeVersion>>`/`Vec<Arc<EdgeVersion>>` during the backward walk\n(instead of `Vec<VersionId>`) and reuse those cached `Arc`s in the\nforward-apply loop and for the anchor lookup. Error paths\n(`MaxDepthExceeded`/`MissingAnchor`/`CorruptedVersionChain`) and the\nreturned `PropertyMap` are unchanged — only the now-redundant second\nfetch is removed.\n\n## 📊 Measurement\n\n`valgrind --tool=callgrind`, 2 runs each side (spread <0.2% on both\nsides):\n\n| | Ir (run 1) | Ir (run 2) | Mean |\n|---|---:|---:|---:|\n| before | 11,051,604,474 | 11,059,233,659 | 11,055,419,067 |\n| after | 10,100,909,506 | 10,081,000,354 | 10,090,954,930 |\n\nDelta: **-964,464,137 Ir (-8.72%)**. `callgrind_annotate` after the fix\nconfirms the redundant call sites are gone entirely — only the\nbackward-walk call site remains.\n\n`valgrind --tool=dhat`, one run each side:\n\n| | Blocks | Bytes |\n|---|---:|---:|\n| before | 8,677,328 | 2,980,917,528 |\n| after | 6,604,504 | 2,157,069,264 |\n\nDelta: **-23.89% blocks, -27.64% bytes**.\n\nBoth the instruction-count floor (≥5%) and the allocation floor (≥10%,\ncleared on both count and bytes) are cleared.\n\n## 🔬 Reproduce\n\n```\ncargo build --profile bench --example bolt_temporal_reconstruction_workload\n\n# instructions\nvalgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out -- \\\n    target/release/examples/bolt_temporal_reconstruction_workload\ncallgrind_annotate /tmp/cg.out src/storage/historical/mod.rs | less\n\n# allocations\nvalgrind --tool=dhat --dhat-out-file=/tmp/dhat.out -- \\\n    target/release/examples/bolt_temporal_reconstruction_workload\n```\n\n## Test plan\n\n- [x] `cargo fmt --all -- --check` — clean\n- [x] `cargo clippy --all-targets --all-features -- -D warnings` —\nclean, no warnings\n- [x] `cargo test --lib` — 4,665 passed, 0 failed, 10 ignored (unchanged\nfrom trunk)\n- [x] `cargo test --lib storage::historical` — 128 passed, 0 failed\n(includes the\n`MaxDepthExceeded`/`MissingAnchor`/`CorruptedVersionChain`/anchor-cache/long-delta-chain\nerror-path and correctness suites)\n- [x] Native run of the new example produces a deterministic\n`sink=250000` line, byte-identical before and after the fix\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_012yBEAg3pcrce3P7GamFYR4\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_012yBEAg3pcrce3P7GamFYR4)_\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-09-20T14:24:49-05:00",
          "tree_id": "10a6a57b24bcfa7eb44d99b45a6a29b2da58e179",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/b54792ce99ae4121404d9b687fa71a235bcb85a1"
        },
        "date": 1789932950854,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 20.80965318886423,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 175.34615564303735,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 171.570842474838,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 183.12353194644254,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 173.79302326725775,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 388991.60644596297,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}