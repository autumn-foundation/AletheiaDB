{
  "lastUpdate": 1784582008417,
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
          "id": "7bcbd26800870598743363a61e4c6053ace1ba88",
          "message": "test(config): make interner-cap subprocess tests tolerate Windows CI spawn overhead (#3754)\n\n## Summary\n\nFixes the Windows-only CI failure of\n`interner_cap_reopen_with_higher_cap_via_subprocess` (and hardens three\nsibling subprocess tests) in `src/db/config.rs`.\n\n### Before\n\nFour interner-cap tests in `db::config` re-spawn the entire test binary\nas a child process to run one `#[ignore]` helper in isolation (needed\nbecause the helper mutates the process-global `GLOBAL_INTERNER`). Each\npolls the child against a hard `Instant::now() +\nDuration::from_secs(30)` deadline and `panic!`s \"…helper did not\ncomplete\" on timeout.\n\nOn the required `Test Suite (windows-latest, stable)` CI cell the entire\ntest binary takes ~560s, and process spawn + large debug-binary load\nunder heavy parallel load routinely exceeds 30s — so the child blows\npast the deadline and the parent panics with a **false timeout**.\nubuntu/macOS pass the same tests in the same run. The confirmed failure\nis `interner_cap_reopen_with_higher_cap_via_subprocess`; the three\nsiblings (`config_interner_cap_overrides_env_on_open_via_subprocess`,\n`interner_cap_multidb_high_water_mark_via_subprocess`,\n`interner_cap_concurrent_opens_converge_to_max_via_subprocess`) share\nthe identical fragile 30s pattern and are latent flake risks.\n\n### After\n\nThe deadline is centralized and made platform-aware, so these tests\nreflect **real hangs** rather than Windows spawn overhead. POSIX default\nis unchanged at 30s; behavior on Linux/macOS is effectively identical.\n\n### How\n\n- New shared `subprocess_test_timeout()` helper in the same\n`#[cfg(test)]` module:\n- `ALETHEIADB_SUBPROCESS_TEST_TIMEOUT_SECS` env override wins on any\nplatform (CI tuning / local debugging).\n- `#[cfg(windows)]` default = 180s (generously safe — still well inside\nthe observed ~560s whole-binary runtime, so a genuine single-helper hang\nis detected).\n  - `#[cfg(not(windows))]` default = 30s (unchanged).\n- All four subprocess tests route their deadline through the helper\ninstead of the inline `Duration::from_secs(30)`.\n- Panic messages now include the configured timeout (`… did not complete\nwithin {timeout:?}`) for better diagnosability. Child stdio is left\ninherited (already surfaced in CI logs); piping was deliberately avoided\nto prevent a fill-buffer deadlock on a chatty child.\n- Adds a Linux-runnable unit test\n`subprocess_test_timeout_honors_env_override_and_sane_default` asserting\nthe env override is honored, a malformed override falls back, and the\nplatform default is `>= 30s` (`>= 120s` under `cfg(windows)`).\n\n### Verification\n\nGates run on the repo-pinned toolchain (1.97.0):\n- `cargo fmt --all -- --check`: clean\n- `cargo clippy --all-targets --features\n\"config-toml,mcp-server,sharding-rpc,simulation,cypher\" -- -D warnings`:\npass\n- `cargo clippy --all-targets --all-features -- -D warnings`: pass\n- `cargo clippy --all-targets --no-default-features -- -D warnings`:\npass\n- `cargo check --no-default-features --tests`: pass\n- `cargo test --lib --features\n\"config-toml,mcp-server,sharding-rpc,simulation,cypher\" db::config`: 16\npassed, 0 failed, 4 ignored — including the new unit test and all four\nsubprocess parent tests. (`--lib` scoping was required because building\nevery `tests/` integration binary exhausts the sandbox disk; the `--lib`\ntarget contains exactly the affected unit + subprocess-parent tests.)\n\n**Note:** this is a Windows-timing fix that cannot be fully verified on\nthe Linux sandbox — the Linux run only proves POSIX behavior is\nunchanged. The real proof is the `Test Suite (windows-latest, stable)`\nCI cell going green on this PR.\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-07-20T15:50:49-05:00",
          "tree_id": "91c95c54c64088e89049c80707cd6734b658077b",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/7bcbd26800870598743363a61e4c6053ace1ba88"
        },
        "date": 1784582008416,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 152.05722978024457,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 23.805885654160015,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 535258.00410994,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 184.2753995146954,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 188.14793324225823,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 180.18165094811934,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}