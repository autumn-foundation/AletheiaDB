{
  "lastUpdate": 1785437022100,
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
          "id": "042abde470254184eab55f06ed5a36bce19e9405",
          "message": "release: v0.2.0 prep — changelog completion, publish preflight, PyPI rename (#3787)\n\n## Summary\n\nRelease-prep for v0.2.0. Along the way this surfaced several things that\nwere not previously known, one of which **blocks the release**.\n\n> ### 🚫 0.2.0 is blocked on #3788\n> Replica reads can observe a partially-applied transaction. Replication\nis a headline 0.2.0 feature, so the release should not be tagged until\nthat is resolved. **This PR is still worth merging on its own** — it\nmakes the changelog accurate and the release pipeline correct — but it\nis **prep, not the release**. Merging it tags nothing.\n\n## What was wrong\n\n**1. The changelog was ~43 commits behind.** The 0.2.0 section was\nwritten by release-prep PR #3725; **45 commits** landed after it, of\nwhich only **2** were recorded under `[Unreleased]`.\n\n**2. Two stale facts in it**, caught by checking source rather than\ntrusting prose:\n- claimed **63 MCP tools**; `src/mcp/tests.rs:20209` asserts the live\ncatalog is exactly **74**\n- claimed **WAL v5/v6**; it is **v13/v14**, plus the v16 keyversioned\ncontainer\n\n**3. The pre-v13 WAL-tail refusal (#3746) was undocumented** — the one\naction a 0.1.x operator must take before upgrading (drain the WAL, or\n`open()` refuses). Now a breaking change with the migration step.\n\n**4. Nothing from this repo has ever reached PyPI**, and the\n`aletheiadb` name there belongs to someone else (see below).\n\n## Changes\n\n### `CHANGELOG.md`\n- Fold `[Unreleased]` into `## [0.2.0]`, re-dated.\n- Document the ~43 unrecorded changes: multi-tenant isolation (#3365),\nasynchronous replication (#3355), engine-lane query resource limits\n(#3368), drift alarms (#3367), contradiction genealogy (#3352),\ncounterfactual replay (#3357), trust propagation (#3382), knowledge\nhalf-life (#3377), secondary property index (#3774), `USE`/`IN\nNAMESPACE` grammar (#3349), DBOS workflow journal + fencing\n(#3755/#3759), shell completions (#3619), wasm groundwork (#3772/#3776),\nLDBC bench (#3628), rotation-cancel fixes (#3680/#3783), PITR/recovery\nfixes (#3745/#3746/#3764).\n- Correct the tool count, the WAL versions, and the replication\nguarantee (below).\n\n### `.github/workflows/release.yml`\nBoth prior releases failed at `cargo publish` with `a value is required\nfor '--token <TOKEN>'` — the secret expanded to an empty argument. (The\ncrate still reached crates.io: 0.1.0 and 0.1.1 were published **by\nhand** shortly *before* their tags were pushed, so the publish job is\nnot on the critical path.)\n\n- New `preflight` job gating the pipeline: tag ↔ `Cargo.toml` agreement\nand a matching CHANGELOG section. Both are cheap, local, and\nunrecoverable if wrong.\n- The **token check lives in `publish-crate`, deliberately not in\n`preflight`** — in the gating job it would abort the GitHub release and\nbinary assets on every tag whenever the secret is absent, which is a\nregression against the flow actually in use.\n- Token passed via the environment; publish uses `--locked`.\n- The empty-secret case is named explicitly, and distinguished from an\n*expired* token (empty: clap usage error in 0s, before packaging;\nexpired: 401 after packaging).\n\n### PyPI: distribution renamed to `aletheiadatabase`\nThe PyPI name `aletheiadb` is held by an **unrelated project**\n(pure-Python `py3-none-any` wheels, first uploaded 2026-06-01, different\nauthor and source repo). The release job is gated on\n`refs/tags/python-v*` and only `python-v0.1.0` was ever tagged, so it\nhas never run. Tagging under the old name would 403 regardless of\ntrusted-publisher config — a name conflict, not a configuration problem.\n\nThe **import name is unchanged**: `pip install aletheiadatabase` then\n`import aletheiadb`. The Rust crate name is untouched; crates.io\n`aletheiadb` is ours.\n\n### TypeScript SDK: made publishable\n- **`publishConfig.access: \"public\"`** — scoped packages default to\n*restricted*, so the first `npm publish` of `@aletheiadb/client` would\nhave failed on a free org without it. Invisible until the moment you\npublish.\n- **`repository.url`** — was `madmax983/AletheiaDB`, now\n`autumn-foundation`. Same stale-URL class #3773 fixed for Cargo.\n\nNo npm publish job: publishing is done manually, and wiring a job\nagainst a secret that doesn't exist is exactly how `publish-crate` came\nto fail on every release since v0.1.0.\n\n### Replication guarantee corrected\nThe changelog claimed replica reads are \"always a consistent (possibly\nstale, never torn) snapshot\". They are not — see #3788. Replaced with\nthe guarantee the code provides plus an explicit **Known issue**.\nShipping an overclaim is the same defect class this PR fixes elsewhere.\n\n### `tests/release_workflow.rs`\nGuards for: preflight ordering, the token check living in\n`publish-crate` and *not* in `preflight`, the env-var token form,\n`--locked`, and CHANGELOG ↔ `Cargo.toml` version agreement. **8 tests,\nall passing.**\n\n## Verification\n\n**CI is fully green on `de57b5b`** — all 39 checks `success` or expected\n`skipped`, `mergeable_state: clean`, including all six Test Suite jobs\nand Code Coverage.\n\nLocally:\n- `cargo test --test release_workflow` — 8 passed\n- `cargo fmt --all`, `cargo clippy --test release_workflow\n--all-features -- -D warnings` — clean\n- `maturin sdist` — builds `aletheiadatabase-0.1.3.tar.gz`, `PKG-INFO`\nName `aletheiadatabase`, still ships `aletheiadb/__init__.py`,\n`_native.pyi`, `py.typed`\n- TS SDK full publish gate — lint, typecheck, **155 tests**, ESM+CJS\nsmoke, build; tarball contains both bundles and both `.d.ts` variants\n\nIn CI specifically: python-wheels built on all five targets and the\nwheel-install test passed on **Python 3.9 and 3.12** under the new name;\nthe ts-sdk gate passed on **Node 18, 20, 22**.\n\n> `torn_frame_safety_never_exposes_a_partial_transaction` (#3788) is\nintermittent — it failed on two earlier runs and **passes on the current\nhead**. Pre-existing and unrelated to this diff (which touches no\n`src/`);\n[analysis](https://github.com/autumn-foundation/AletheiaDB/pull/3787#issuecomment-5131893223),\n[frequency](https://github.com/autumn-foundation/AletheiaDB/issues/3788#issuecomment-5132282723).\n\n## Still outstanding (not in this PR)\n\n- **#3788 must be fixed before tagging 0.2.0.**\n- The 0.2.0 changelog date needs bumping to the actual tag date.\n- The `@aletheiadb` **npm scope is unclaimed** and `@aletheiadb/client`\nis unpublished — the manifest is ready, the scope registration and first\npublish are manual.\n- `CARGO_REGISTRY_TOKEN` is unset — only matters if CI should publish\nrather than publishing by hand.\n- A `python-v0.1.3` tag is needed to ship the Python SDK, and the PyPI\ntrusted publisher must point at `autumn-foundation/AletheiaDB` (it may\nstill reference `madmax983`).\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_01LT1b4FQ9K4q9UcRFXP22HQ\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-07-30T13:20:48-05:00",
          "tree_id": "44b87eb22f702cc90db51325bbf4c8d405cdfb1e",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/042abde470254184eab55f06ed5a36bce19e9405"
        },
        "date": 1785437022099,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 483440.08397749317,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 172.46522897491354,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 175.75167040177976,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 172.06431200986833,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 183.5154044786386,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 21.200591317589634,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}