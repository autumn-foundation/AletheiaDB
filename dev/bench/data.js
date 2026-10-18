{
  "lastUpdate": 1792302744585,
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
          "id": "3f44bc8349cd015bbb5f5fed223d3471caf5c86e",
          "message": "⚡ Bolt: stop cloning the whole response per rung-4 probe (instructions -58.7%) (#3842)\n\n## Summary\n\nThe MCP token-budget response shaper's rung-4 array-truncation binary\nsearch (Issue #3353, `src/mcp/budget.rs::truncate_arrays_to_fit`)\nmeasured each candidate response size by doing `map.clone()` on the\n**entire** response object (every retained entity, not just the array\nfield being truncated) and then\n`serde_json::to_string_pretty(...).len()` — allocating and formatting a\nfull copy just to read a byte count, up to `O(log n)` times per\ntruncated array field.\n\nThis PR eliminates both allocations from the probe without changing any\nobservable behavior.\n\n## 🎯 Workload\n\n`examples/bolt_budget_truncation_workload.rs` — a `list_nodes` call over\na 3000-node dataset with `max_response_tokens` small enough that even\nthe fully-summarized page overflows, forcing the `counts_and_handles`\nrung's binary search to run (30 `list_nodes` calls total). Reproduce:\n\n```\ncargo build --profile bench --example bolt_budget_truncation_workload --features mcp-server\nvalgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out -- \\\n    target/release/examples/bolt_budget_truncation_workload\ncallgrind_annotate /tmp/cg.out src/mcp/budget.rs | grep assembled_len\n```\n\nNo existing benchmark covered this path: `benches/` has no\nbudget/token-shaping entry, and no other `examples/bolt_*.rs` workload\npasses `max_response_tokens`.\n\n## 📈 Profile\n\nBaseline (commit 9eaa0ab): **31,369,217,848 Ir** total. `assembled_len`\n(the per-probe measurement function) accounted for **17,414,260,845 Ir —\n55.52% of total**, across 360 calls (30 fast-exit checks + 330\nbinary-search probes). Malloc-family functions alone\n(`_int_malloc`/`_int_free`/`malloc`/`malloc_consolidate`/`free`/`unlink_chunk`)\nwere ~44% of total instructions — a direct signature of the per-probe\nfull-object clone.\n\n## 💡 Hypothesis\n\n`assembled_len`'s `map.clone()` deep-clones every entity in the response\non every binary-search probe, when the search only needs to know: (a)\nthe byte length of the array truncated to a shorter prefix, and (b) the\nbyte length of a couple of small pagination-sibling scalars rewritten to\ndescribe that prefix. Moving the array's dropped suffix aside instead of\ncloning it (`Vec::split_off`, no clone), and reverting a snapshot of the\nfew touched scalars, gets the identical measurement without touching the\nrest of the (possibly large) response.\n\nSeparately, `measured_bytes`'s\n`serde_json::to_string_pretty(value).len()` allocates and formats a\n`String` purely to read its length — a counting `io::Write` sink fed to\n`serde_json::to_writer_pretty` produces the identical byte count with no\nallocation.\n\n## 🔧 Change\n\n- `assembled_len` now mutates `map` in place instead of cloning it, then\nreverts every mutation before returning — bit-for-bit unchanged from the\ncaller's perspective.\n- `measured_bytes` counts bytes via a `ByteCounter: io::Write` sink\ninstead of allocating a `String`; a new `measured_bytes_map` does the\nsame for a `Map` directly (avoiding a `Value::Object(map.clone())`\nwrapper that would reintroduce the clone).\n- No behavior change: `set_array_prefix`/`rewrite_pagination_siblings`\nstill run exactly once per truncated field, with the real tool name,\nonce the search converges.\n\n## 📊 Measurement\n\nSame harness/session, `valgrind --tool=callgrind`, 3000 nodes, 30\n`list_nodes` calls, `BOLT_MAX_TOKENS=400`:\n\n| metric | before | after | delta |\n|---|---|---|---|\n| Total instructions (Ir) | 31,369,217,848 | 12,953,576,074 |\n**-58.71%** |\n| `assembled_len` inclusive Ir (360 calls) | 17,414,260,845 |\n716,551,308 | -95.88% |\n| `assembled_len` share of total | 55.52% | 5.53% | |\n\n58.71% total-instruction reduction clears the impact floor (≥5%) by more\nthan 11x. The workload's `sink` (sum of response byte-lengths across all\n30 calls) is **bit-for-bit identical before and after (45510)**,\nconfirming the bytes emitted to callers are unchanged.\n\n## 🔬 Reproduce\n\n```\ncargo build --profile bench --example bolt_budget_truncation_workload --features mcp-server\nvalgrind --tool=callgrind --callgrind-out-file=/tmp/cg_after.out -- \\\n    target/release/examples/bolt_budget_truncation_workload\ncallgrind_annotate /tmp/cg_after.out src/mcp/budget.rs | grep assembled_len\n```\n\n## Test plan\n\n- [x] `cargo fmt --all -- --check` — clean\n- [x] `cargo clippy --all-targets --all-features -- -D warnings` —\nclean, no warnings\n- [x] `cargo test --lib --features mcp-server` — 5363 passed, 0 failed,\n12 ignored\n- [x] `cargo test --lib --features mcp-server mcp::budget::` — 11 passed\n(module's own unit tests)\n- [x] `cargo test --lib --features mcp-server mcp::tests::` — 497 passed\n- [x] Native run of the workload produces byte-identical `sink=45510`\nbefore and after\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_01LUR7MNSPNNbxHR6bMtBkhR\n\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_01LUR7MNSPNNbxHR6bMtBkhR)_\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-10-18T00:05:35-05:00",
          "tree_id": "00a6c02c362a638c4bc868e9ae573f817581274d",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/3f44bc8349cd015bbb5f5fed223d3471caf5c86e"
        },
        "date": 1792302744585,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 21.217810050070565,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 171.97346133029222,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 212.7890191937108,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 213.73693793217456,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 163.7955820557715,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 375698.4891756775,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}