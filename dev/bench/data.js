{
  "lastUpdate": 1789567848796,
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
          "id": "fc598580f21b21e753d88234fbba60a8cb8ea411",
          "message": "⚡ Bolt: avoid per-op JSON clone in apply_batch prevalidation (allocs -14.05%) (#3836)\n\n## 🎯 Workload\n\n`examples/bolt_batch_workload.rs` (new) — a deterministic,\npublic-API-driven benchmark for the MCP `apply_batch` write path (Issue\n#3231): the \"LLM builds an entity-with-relationships subgraph in ONE\ncall instead of N calls\" shape CLAUDE.md documents as the feature's\nmotivating case. No existing benchmark exercises `apply_batch`:\n`benches/` has no batch/mcp entry, and `bolt_workload.rs` drives\n`create_node`/`create_edge` directly, one op per transaction, never\nthrough the MCP batch surface.\n\nEach simulated batch is a 40-node star-shaped subgraph (5 properties\neach) plus ~80 `create_edge` ops linking nodes purely by `$alias` (no\ncommitted ids, so batches never collide), repeated across 200\nindependent batches (24,000 ops total) via\n`AletheiaMcpServer::apply_batch`, with requests built as raw\n`serde_json::Value` bodies — exactly the shape an embedded/JSON-RPC\ncaller's payload takes.\n\nReproduce:\n```\ncargo build --profile bench --example bolt_batch_workload --features mcp-server\nBOLT_BATCHES=200 BOLT_NODES_PER_BATCH=40 BOLT_EDGES_PER_NODE=2 \\\n  valgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out -- \\\n  target/release/examples/bolt_batch_workload\ncallgrind_annotate --threshold=90 /tmp/cg.out | head -40\n```\n\n## 📈 Profile\n\nvalgrind 3.22.0, this session. Baseline (unpatched trunk), 2 runs:\n1,607,245,116 / 1,618,008,617 Ir (mean 1,612,626,867, spread ~0.67%).\n\nTop self-cost lines are almost entirely malloc-family: `_int_malloc`\n12.29%, `_int_free` 10.08%, `malloc` 6.98%, `malloc_consolidate` 4.08%,\n`free` 3.89%, plus `memcpy`/`memcmp` (~5.8%) and BTreeMap insert/drop\nmachinery (`serde_json::Map` is BTreeMap-backed) — malloc-family\nself-cost sums to ~41% of the profile. Following the call tree, a real\nand identifiable contributor is `prevalidate_batch`'s op-parsing loop:\n`serde_json::from_value::<BatchOperation>(raw.clone())` inside a loop\nover `raw_ops: &[serde_json::Value]`, deep-cloning every operation's\nJSON tree (label, the whole properties map, provenance, valid_time)\npurely to satisfy `from_value`'s by-value signature — even though\n`raw_ops` (`req.operations`) is owned data never read again after the\nloop.\n\n## 💡 Hypothesis\n\nTaking `raw_ops` by value and consuming it with `.into_iter()` instead\nof `.iter()` moves each `Value` into `from_value` directly instead of\ncloning it first, avoiding a full recursive deep-copy (every nested\nString, number, and map entry) of each operation once per `apply_batch`\ncall. This is on the shared `prevalidate_batch` path both\n`apply_batch()` (embedded Rust callers) and `handle_apply_batch()` (the\nreal MCP JSON-RPC dispatch entry, `server.rs:10830`) route through, so\nthe fix benefits the actual production dispatch path, not just this\nharness's entry point.\n\n## 🔧 Change\n\n- `prevalidate_batch`'s parameter: `&[serde_json::Value]` →\n`Vec<serde_json::Value>`.\n- Its single caller (`handle_apply_batch`) passes `req.operations` by\nvalue instead of by reference (`req` isn't used afterward).\n- The op-parsing loop: `raw_ops.iter().enumerate()` →\n`raw_ops.into_iter().enumerate()`, dropping the `.clone()`.\n\nNo behavior change: error messages, `failed_op_index` values, and the\ntwo-phase validate/execute contract are untouched. `sink=` harness\noutput is byte-identical before/after (`3690120`).\n\n## 📊 Measurement\n\n`valgrind --tool=dhat`, 2 runs each side:\n\n| | blocks (run1) | blocks (run2) | mean | bytes (run1) | bytes (run2) |\nmean |\n|---|---:|---:|---:|---:|---:|---:|\n| Before | 2,279,224 | 2,279,502 | 2,279,363 | 355,173,592 | 355,342,244\n| 355,257,918 |\n| After | 1,959,834 | 1,958,572 | 1,959,203 | 324,468,944 | 321,773,772\n| 323,121,358 |\n| **Delta** | | | **-14.05%** | | | -9.05% |\n\nAllocation-count delta clears this process's ≥10% floor (bytes just\nmisses it at -9.05%, but only one dimension needs to clear).\n\n`valgrind --tool=callgrind`, 2 runs each side:\n\n| | Ir (run1) | Ir (run2) | mean | Delta |\n|---|---:|---:|---:|---:|\n| Before | 1,607,245,116 | 1,618,008,617 | 1,612,626,867 | — |\n| After | 1,548,672,957 | 1,558,258,267 | 1,553,465,612 | **-3.67%** |\n\nThe instruction delta (-3.67%) is real (run-to-run spread is only\n~0.6-0.7% on each side) but stays under the 5% instruction floor on its\nown — this PR ships on the allocation-count floor instead. The residual\nmalloc-heavy profile is largely the *other* two JSON round-trips in this\ncall chain (`apply_batch`'s own `serde_json::to_value(req)`, and\n`handle_apply_batch`'s `serde_json::from_value::<ApplyBatchRequest>`),\nwhich are structural to the documented two-phase (typed-request → raw\nJSON → per-op-indexed re-validation) design and out of scope for a\n\"smallest change that moves the counter\" patch.\n\n## 🔬 Reproduce\n\n```\ncargo build --profile bench --example bolt_batch_workload --features mcp-server\nBOLT_BATCHES=200 BOLT_NODES_PER_BATCH=40 BOLT_EDGES_PER_NODE=2 \\\n  valgrind --tool=dhat --dhat-out-file=/tmp/dhat.out -- \\\n  target/release/examples/bolt_batch_workload\n# \"Total:\" line reports bytes/blocks\n```\n\n## Verify\n\n- [x] `cargo fmt --all` — clean\n- [x] `cargo clippy --example bolt_batch_workload --features mcp-server\n--all-features -- -D warnings` — clean, no warnings\n- [x] `cargo test --lib --features mcp-server apply_batch` — 52 passed,\n0 failed, 1 ignored (unchanged)\n\n## Test plan\n\n- [x] Harness builds and runs deterministically (`sink=3690120`,\nidentical before/after)\n- [x] All 52 `apply_batch` unit tests pass unchanged\n- [x] Clippy clean under `--all-features`\n- [x] `cargo fmt --all` applied\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_01FNpR5gvAaRX6gJRpeXAXTt\n\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_01FNpR5gvAaRX6gJRpeXAXTt)_\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-09-16T08:59:26-05:00",
          "tree_id": "5fc55360687378d06fc53ca730dada9d134c3f53",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/fc598580f21b21e753d88234fbba60a8cb8ea411"
        },
        "date": 1789567848795,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 16.677562684991006,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 144.33851478140784,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 146.65781506502444,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 154.6349645562154,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 148.92229579714308,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 305845.35217690084,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}