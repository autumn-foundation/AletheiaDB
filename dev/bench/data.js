{
  "lastUpdate": 1784476243770,
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
          "id": "65b67f24b5836f5138eb6f3fc234cc8028804bd6",
          "message": "Fix stale cypher multi-pattern query test under PR3d namespace scoping (#3737)\n\n## Summary\n\nBefore: the cypher-gated MCP test\n`handle_query_multi_pattern_returns_non_null_bindings` fails on trunk —\na multi-pattern Cypher query `MATCH (a:Person),(b:Company) RETURN a,b`\nreturns a structured `unsupported_construct` error with no `rows` field,\npanicking the test at `src/mcp/server.rs:11438`.\n\nAfter: the test passes an explicit `\"namespace\": \"all\"` and passes; a\ncompanion guard test pins the fail-closed behavior; the v1 limitation is\ndocumented; and the CI blind spot that hid the break is closed.\n\n## Root cause\n\nPre-existing trunk regression introduced by **Namespaces PR3d (#3731,\ncommit `12d3d0f`)**, not any in-flight PR. PR3d made the MCP `query`\ntool default an omitted `namespace` argument to a restricting\n`Single(default)` scope (fail-closed / isolated-by-default, #3349).\nMulti-variable-pattern MATCH queries reject *any* restricting scope by\ndesign (`src/db/query.rs:540-549`, `UnsupportedFeature` →\n`unsupported_construct`, \"never silently unscoped\"). The test issued its\nmulti-pattern query with no `namespace` arg, so post-#3731 it hit that\nrejection instead of receiving rows.\n\nThe break went unnoticed because the test is `#[cfg(feature =\n\"cypher\")]`-gated and the fast CI test job\n(`.github/workflows/ci.yml:71`) compiled\n`config-toml,mcp-server,sharding-rpc,simulation` **without** `cypher`,\nso the test never ran there.\n\n## Why fix the test, not the code\n\nThe scoped default is intentional and load-bearing: all three\nPR3d-scoped tools (`query`/`traverse`/`hybrid`) default\nomitted-namespace to `Single(default)`, the #3731 commit message states\n\"omitted ⇒ default-only; `all` ⇒ no filter\", and a dedicated guard\n`omitted_scope_is_default_only_not_all` forbids reverting the default to\n`All` (that would reopen a cross-namespace default-leak surface, against\nthe namespaces lane's fail-closed principle). Sibling tests already pass\n`namespace:\"all\"` to run unscoped. So the failing test was simply stale.\n\n## Changes\n\n- `src/mcp/server.rs`: add `\"namespace\": \"all\"` (with an explanatory\ncomment) to the failing test so its cartesian-product multi-pattern\nquery runs unscoped; assertions unchanged (still verifies non-null\n`a`/`b` bindings + dynamic columns).\n- `src/mcp/server.rs`: add companion guard test\n`handle_query_multi_pattern_omitted_scope_is_rejected` asserting the\nomitted-scope multi-pattern query returns `INVALID_ARGUMENT` /\n`unsupported_construct` — pinning the fail-closed semantics.\n- `docs/guides/mcp-query-tool.md`: document that multi-variable-pattern\nMATCH is unsupported under a restricting namespace scope in v1; callers\nmust pass `namespace:\"all\"` or use a single-variable MATCH.\n- `.github/workflows/ci.yml`: add `cypher` to the fast test job's\nfeature set (line 71) so cypher-gated MCP tests actually run in CI —\nclosing the blind spot that let this land on trunk.\n\n## CI cost of the gate change\n\nModest and low-risk: the coverage job (`ci.yml:200`) already compiles\nthe `mcp-server`+`cypher` combination, so this introduces no new\ncompile/breakage surface — it just makes the fast job execute the\ncypher-gated tests too. The added tests are unit-level and fast; locally\nthe full `--lib` suite under the expanded feature set runs 5551 tests in\n~80s with 0 failures.\n\n## Testing\n\n- `handle_query_multi_pattern_returns_non_null_bindings` and\n`handle_query_multi_pattern_omitted_scope_is_rejected`: both pass.\n- `cargo test --features\nconfig-toml,mcp-server,sharding-rpc,simulation,cypher --lib`: **5551\npassed, 0 failed** (no latent cypher-gated failures exposed by the new\ngate).\n- `cargo fmt --all --check` clean; `cargo clippy --all-targets` with `-D\nwarnings` clean across the primary (incl. `cypher`), all-features, and\nno-default-features sets; `cargo check --no-default-features --tests`\npasses.\n\n(Note: the full-workspace `cargo test` across all integration binaries\ncan't complete in the CI sandbox used here due to a disk-space allowance\nlimit; the bounded `--lib` run above is the relevant coverage for these\ntest/CI-config changes.)\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-07-19T10:26:28-05:00",
          "tree_id": "d9001762254500ad26b1984d827217fd143a94e7",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/65b67f24b5836f5138eb6f3fc234cc8028804bd6"
        },
        "date": 1784476243770,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 156.4701028210135,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 19.26681241252594,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 480314.84835561074,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 180.82413849573913,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 177.5076462437557,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 172.2596871753204,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}