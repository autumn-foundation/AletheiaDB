{
  "lastUpdate": 1786881398708,
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
          "id": "8a1581fcd0a53d3e61dd7b17254bf06ba4c67c01",
          "message": "⚡ Bolt: identity-hash NodeId/EdgeId-keyed DashMaps (instructions -27.9%) (#3795)\n\n## 🎯 Workload\n\n`examples/bolt_workload.rs` (new, committed here) — a deterministic\nmixed read/write workload driven entirely through the public\n`AletheiaDB` API, not a synthetic microbenchmark of the changed code. It\nbuilds a 1,000-node / 6,000-edge social-graph-shaped dataset\n(`create_node`/`create_edge`), enables a property-equality index\n(`db.property_index(\"Person\", \"category\").enable()` — matching a\nrealistically-configured deployment for the repeated property lookups\nthe read loop issues, rather than pathologically forcing every lookup\nthrough the documented opt-in-index fallback scan), then runs 3,000 read\niterations of `get_node` + single-hop `get_outgoing_edges` + a full\n3-hop traversal (chained `get_edge`/`get_outgoing_edges`) +\n`find_nodes_by_property` — the exact call shapes the MCP\n`get_node`/`traverse`/`find_nodes_by_property` tools take, and match the\nshapes already exercised in `benches/current_state.rs`.\n\nIt's a plain binary rather than a criterion benchmark because\ncriterion's own iteration/warmup loop is unusable stacked under\nvalgrind's 20-50x slowdown; one fixed deterministic pass keeps\ninstruction counts reproducible run to run.\n\nReproduce:\n```\ncargo build --profile bench --example bolt_workload\nBOLT_NODES=1000 BOLT_OUT_DEGREE=6 BOLT_READ_ITERS=3000 \\\n  valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind.out -- \\\n  target/release/examples/bolt_workload\ncallgrind_annotate --threshold=95 /tmp/callgrind.out | head -60\n```\n\n## 📈 Profile\n\nBaseline instruction-count breakdown (570,371,849 total instructions,\nproperty index enabled — see the harness's own two prior commits for the\nunindexed-scan variant, which is a separate,\nexpected/documented-behavior finding, not this PR's target):\n\n| Self cost | Site |\n|---|---|\n| 9.91% | `core::hash::sip::Hasher::write` |\n| 4.50% | `malloc.c:_int_free` |\n| 4.48% / 3.79% / 3.69% / 2.81% / 2.39% | `dashmap::t::Map::_get`\n(several inlined call sites, hashing + probe) |\n| 3.48% | `malloc.c:malloc` |\n| 2.30% / 2.05% | `malloc.c:_int_malloc` / `free` |\n\nSummed, SipHash-attributed cost (`core::hash::sip::*` +\n`BuildHasher::hash_one`) alone is **~15.1%** of total instructions.\nCombined with the DashMap probe/lock overhead in the same `_get` call\nchain, \"get a value out of a DashMap\" — i.e. `get_node`, `get_edge`, and\nadjacency lookups — accounts for **~27%** of the entire profile: the\nsingle largest fixable share, ahead of malloc/free churn (~14.3%).\n\n## 💡 Hypothesis\n\n`CurrentIndexes::nodes` / `node_headers` / `edges`\n(`src/index/current.rs`) and `IncrementalAdjacencyIndex::delta` /\n`tombstones` (`src/index/incremental_adjacency.rs`) — the maps behind\nevery `get_node`, `get_edge`, and adjacency lookup — are keyed by\n`NodeId`/`EdgeId`, already-unique internal u64s minted by an atomic\n`IdGenerator`, never derived from untrusted input. They were using\n`DashMap`'s default hasher (`std`'s SipHash via `RandomState`), a\ncryptographic, DoS-resistant hash designed to protect against\nadversarially-crafted keys. Hashing an already-random internal integer\nwith SipHash is pure overhead with zero benefit.\n\nThe codebase already has an in-house `IdentityHasher`\n(`src/core/hasher.rs`) purpose-built for exactly this case — its own doc\ncomment: *\"keys are already high-quality unique identifiers... hashing\nthem again provides zero benefit and costs valuable CPU cycles\"* — and\nit's *already used* for the sibling `ns_nodes`/`ns_edges`/`prop_index`\nmaps in the very same `CurrentIndexes` struct (Issue #3349, PR2). This\nPR just extends the same, already-reviewed pattern to the remaining\ninternal-id-keyed maps that were missed.\n\n## 🔧 Change\n\n- `src/core/hasher.rs`: added `pub type IdHashBuilder =\nBuildHasherDefault<IdentityHasher>;` as the single named alias for this\npattern.\n- `src/index/current.rs`: `nodes`, `node_headers`, `edges` DashMaps\nswitched to `IdHashBuilder`. The file-local, oddly-named `NsHasher`\nalias (its doc comment described it as being only for the\nnamespace-membership index, though it was structurally identical) was\nreplaced by the shared alias — no behavior change to\n`ns_nodes`/`ns_edges`/`prop_index`, which already used it.\n- `src/index/incremental_adjacency.rs`: `delta` and `tombstones`\nDashMaps (and `MergedAdjacencyGuard`'s `tombstones` reference field)\nswitched to `IdHashBuilder`.\n- **No new dependency** — `IdentityHasher` already existed in the tree\nand was already used for this exact pattern elsewhere.\n- **No public API change** — `dashmap` 6.1's `Ref`/`RefMut` guard types\ncarry no hasher generic parameter, so no downstream signature needed\nupdating; verified by a full crate build.\n- **Safety boundary preserved** — only maps keyed by internal ids were\ntouched. `IdentityHasher`'s own doc explicitly warns it is unsafe for\nmaps exposed to untrusted input; no user-string-keyed map (e.g. the\n`StringInterner`, `PropertyMap`) was changed.\n\n## 📊 Measurement\n\n`valgrind --tool=callgrind`, identical harness/env both runs\n(`BOLT_NODES=1000 BOLT_OUT_DEGREE=6 BOLT_READ_ITERS=3000`, property\nindex enabled in both):\n\n| | Instructions (Ir) |\n|---|---|\n| Before | 570,371,849 |\n| After | 411,455,241 |\n| **Delta** | **-158,916,608 (-27.86%)** |\n\nThe harness's `sink` checksum accumulator is byte-identical before/after\n(`975000`), confirming behavior is unchanged — this is a pure speed win,\nnot a semantic change.\n\nAlso verified:\n- `cargo fmt --all`: no changes beyond this diff.\n- `cargo clippy --all-targets --all-features -- -D warnings`: clean.\n- `cargo test --lib` (default features): **4580 passed, 0 failed, 10\nignored**.\n- `cargo test --all-features --lib`: **6816 passed, 0 failed, 14\nignored**.\n- `cargo test --all-features` (full integration suite) could not\ncomplete in this run's environment: `target/debug` alone reached 28G\nbefore the session's disk allowance was exhausted (an infrastructure\nlimit, not a test failure — no test output was ever produced before the\nrun was killed). Given the unit-test suite already covers\n`src/index/current.rs`, `src/index/incremental_adjacency.rs`, and\n`src/core/hasher.rs` under every feature combination, and clippy already\ntypechecks every feature combination, this is disclosed here rather than\nsilently skipped.\n\n## 🔬 Reproduce\n\n```\ngit fetch origin claude/compassionate-mccarthy-dg8oes\ngit checkout claude/compassionate-mccarthy-dg8oes\ncargo build --profile bench --example bolt_workload\nBOLT_NODES=1000 BOLT_OUT_DEGREE=6 BOLT_READ_ITERS=3000 \\\n  valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind_after.out -- \\\n  target/release/examples/bolt_workload\ncallgrind_annotate --threshold=95 /tmp/callgrind_after.out | head -40\n```\n\nTo reproduce the before number, `git stash` the three `src/` changes\n(keep the harness) and re-run the same commands.\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_01D91KgyrnRj4FWVLRAAqfKt)_\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-08-16T06:45:28-05:00",
          "tree_id": "aa02be1ab33a73f433d9a84cf3f14d6f8d6f7da2",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/8a1581fcd0a53d3e61dd7b17254bf06ba4c67c01"
        },
        "date": 1786881398708,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 21.228168032445705,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 182.10499402640937,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 175.19883840020066,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 173.215357586625,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 315761.2635284633,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 173.68285876733665,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}