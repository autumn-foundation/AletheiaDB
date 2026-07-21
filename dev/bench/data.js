{
  "lastUpdate": 1784614201117,
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
          "id": "e3661decb9a4ccdc70af3e23b897c315260b5e8c",
          "message": "Upgrade autumn-web to 0.6.0 in aletheia-server and autumn-spike (#3761)\n\n## What & why\n\nBumps `autumn-web` (and its lockstep `autumn-macros`) from **0.5.0 →\n0.6.0** for the two isolated autumn-web member crates:\n\n- `crates/aletheia-server` — the production HTTP + MCP-over-HTTP +\nOpenAPI server (Issue #3524).\n- `crates/autumn-spike` (`aletheia-autumn-spike`) — the migration spike\nslice.\n\n**Before:** both members pinned `autumn-web = \"0.5.0\"` (features `maud`,\n`mcp`).\n**After:** both pinned `autumn-web = \"0.6.0\"`; `Cargo.lock` resolves\n`autumn-web`/`autumn-macros` to `0.6.0` for these members (the main\n`aletheiadb` crate keeps its own `autumn-web 0.4` — see deferral below).\nNo source changes were needed — the bump is Cargo.toml pins + lockfile\nonly. autumn 0.5→0.6 is additive for how these crates use it; no\ndeprecations surfaced during compile/clippy.\n\nNo other autumn-family crates (`autumn-admin-plugin`,\n`autumn-schema-core`, `autumn-media-plugin`, etc.) are used by these\nmembers, so nothing else to bump.\n\n## MCP schema-edge verdict: no drift\n\nautumn 0.6.0's headline MCP change is the new opt-in `OpenApiSchema`\nderive (#1983/#1972) that can generate real tool `inputSchema`s from\ntyped request args. It is **opt-in and additive** — a handler arg struct\nwithout `#[derive(OpenApiSchema)]` keeps its prior placeholder schema,\nand hand-authored `register_schema` impls still work.\n\nThis app:\n- uses **per-route `#[api_doc(mcp)]`** gating (not\n`expose_all_as_mcp()`), so the 0.6 change that auto-includes untagged\nread-only 204/205 routes under `expose_all_as_mcp()` does not apply\nhere;\n- declares **no** `#[derive(OpenApiSchema)]` and **no**\n`register_schema` in either member;\n- has **no** 204/205 routes.\n\nI captured the live `/mcp` `tools/list` inputSchemas on 0.5 (64 tools)\nand again on 0.6 and **diffed them byte-for-byte: identical.** So the\nschema edge is a confirmed no-op for this application and **no golden\nre-bless is needed.** (The temporary dump harness used for the diff was\nremoved before commit; it is not part of this PR.)\n\n## Gates\n\n| Gate | Result |\n|------|--------|\n| `cargo test -p aletheia-server -p aletheia-autumn-spike --tests` |\nPASS (identical to 0.5 baseline; same 1 ignored test) |\n| MCP inputSchema diff (0.5 vs 0.6, 64 tools) | PASS — byte-identical |\n| `cargo fmt --all --check` | PASS |\n| `cargo clippy -p aletheia-server -p aletheia-autumn-spike\n--all-features -- -D warnings` | PASS (only the pre-existing\n`proc-macro-error2` future-incompat note) |\n| `cargo check --no-default-features --tests` (workspace) | PASS |\n| `cargo test --features http-server --test parity_http` | PASS (33) |\n| `cargo test --features mcp-server --test parity_mcp` | PASS (11) |\n| in-crate golden/conformance (`live_tool_inventory_matches_golden`,\n`every_advertised_tool_has_a_classification`,\n`every_advertised_tool_returns_structured_error_on_invalid_arguments`) |\nPASS |\n\nDisclosed, not chased: a full `cargo clean` was needed once mid-run to\nclear an ENOSPC on the larger `mcp-server` build variant (target had\ngrown to ~29 GB); all gates above were then re-run green.\n\n## Main crate (`aletheiadb`) 0.4 → 0.6: **deferred** (documented\nfollow-up for #3524)\n\nI probed it. The **library** compiles clean on 0.6 with `--features\nhttp-server` (zero fixes) and passes clippy — `src/http` uses autumn's\nnon-macro builder/trait API (`autumn_web::app()`,\n`FromRequestParts<AppState>`, `ConfigLoader`), not the route proc-macros\nthat force the version-binding split. **However, the autumn 0.6 test API\nis a breaking change:** `TestApp::from_router(router)` now takes\n`from_router(router, state)` (requires an `AppState`), and recorder\nwiring moved to `TestApp::new().merge(router).build()`. That breaks the\nHTTP test harness across **13 test files** — a bounded but\nnon-mechanical migration with test-semantics risk, and out of scope for\nthis dependency bump. Consistent with\n`docs/spikes/autumn-web-migration-spike.md`, which explicitly rejects an\nin-place whole-repo bump of `src/http` in favor of the isolated-crate\napproach. Leaving the main crate on 0.4; the 0.4→0.6 test-harness\nmigration should be its own PR under the #3524 migration effort.\n\n## 0.6.0 goodies considered (not adopted here)\n\n0.6.0 ships per-route rate limiting (`#[throttle]`), `Server-Timing`,\nseedable `InMemoryApiTokenStore` for tests, resumable SSE, web/worker\nprocess roles, and config-typo detection. None are needed for this bump;\nnoting them as available for future work (this app already has its own\nrate-limit / resource-limit seams).\n\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_01Bgc4P8ZCBLEwZtpe7pt2FV)_\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-07-21T00:56:20-05:00",
          "tree_id": "e3fdd390efa3ffad5b623ce83ab2be75f8e20719",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/e3661decb9a4ccdc70af3e23b897c315260b5e8c"
        },
        "date": 1784614201116,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 154.43134898673847,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 19.09145921235067,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 479890.72944894794,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 181.04260972591132,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 176.2420939967866,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 173.4304329701901,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}