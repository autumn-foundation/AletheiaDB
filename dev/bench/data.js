{
  "lastUpdate": 1785377852113,
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
          "id": "43a99977427f1ea34b9e68e18e7992ae696fef34",
          "message": "feat(daemon): daemon-owned AletheiaDB for MCP clients (#2905) (#3785)\n\nCloses #2905.\n\n## Problem\n\nAletheiaDB is an embedded, **single-writer** store, but MCP clients\nspawn one server process per client session. Without a daemon that\nleaves exactly two bad options:\n\n| Setup | What happens |\n|---|---|\n| No `ALETHEIADB_DATA_DIR` | Every session silently gets its own\n**ephemeral** database. Agents disagree about reality. |\n| Shared `ALETHEIADB_DATA_DIR` | N processes open the **same** WAL +\nindex directory — unsupported concurrent writers. |\n| Windows | Live `aletheia-mcp.exe` handles pin the installed binary, so\n`cargo install --force` fails with `Access is denied. (os error 5)`. |\n\n## What this does\n\nMakes the ownership boundary explicit: **one process owns the files,\neverything else is a client.**\n\n**Daemon** — new `aletheia-daemon` binary in `crates/aletheia-server`.\nOne process owns the WAL, indexes, and recovery, serving REST + **MCP\nover Streamable HTTP (`/mcp`)** + OpenAPI + `/metrics` from a single\nautumn-web app. Routes come from `app::all_routes()`, shared with the\nparity-tested proving ground, and the existing `/mcp` security gate\napplies unchanged (uniform 401, per-tool RBAC 403, unknown tool names\nrefused fail-closed).\n\n**Ownership** — new `aletheiadb::daemon_lock`. `{data_dir}/daemon.lock`\nis claimed **exclusively** (`O_CREAT|O_EXCL`, so two racing daemons\nresolve to one winner and the path can't be redirected through a\nsymlink) and released only by its owner. A lock left by a crashed daemon\nis reclaimed by *liveness*, not deleted blindly. Every storage-opening\nprocess honors the claim — the CLI, embedded `aletheia-mcp`, and\n`aletheia-server` all refuse a daemon-owned directory rather than\nbecoming a second writer. Ownership resolves from `ALETHEIADB_DATA_DIR`\n**or** an `ALETHEIADB_CONFIG` TOML's `persistence.data_dir`.\n\n**Proxy** — `aletheia-mcp` gains daemon-client mode via\n`ALETHEIADB_DAEMON_URL` / `--daemon-url`: a stdio↔HTTP JSON-RPC\n**relay** that never opens local storage. Frames are forwarded verbatim,\nso `initialize`, `tools/list`, `tools/call`, and anything a future\nprotocol revision adds pass through with **no per-method code**.\nRelaying is concurrent (bounded at 32 in flight) so one long\n`await_changes` can't stall a session; it never falls back to an\nembedded database when the daemon is unreachable (that would recreate\nthe bug this fixes); it exits 0 on stdin EOF. Embedded mode is unchanged\nwhen no daemon URL is set.\n\n**CLI** — `daemon start --surface unified|legacy` waits until the daemon\nis actually *serving* before reporting success. `daemon status [--json]`\nreports liveness, URLs, and a paste-ready MCP client config.\nLiveness/stop now work on Windows (`tasklist` parsed by pid column,\n`taskkill`), Linux (`/proc`), and other Unix (`kill(pid,0)` + `ps`) —\npreviously `/proc`-only, which made the daemon unstoppable on macOS.\n\n## Acceptance criteria\n\n| # | Criterion | Evidence |\n|---|---|---|\n| 1 | `aletheia daemon start` is the one local durable owner for CLI,\nHTTP, MCP, SDK | `daemon_serves_plain_http_alongside_mcp` (MCP write →\nREST read, one process); CLI/`aletheia-mcp`/`aletheia-server` all refuse\na daemon-owned dir |\n| 2 | `aletheia-mcp` daemon-client mode; no `open_from_env()`, no local\nstorage | `daemon_mode_relays_jsonrpc_to_the_daemon_mcp_endpoint`,\n`daemon_mode_never_opens_local_storage` (real process, real\n`ALETHEIADB_DATA_DIR`, asserted empty) |\n| 3 | Embedded mode still works as fallback |\n`without_daemon_url_the_embedded_mode_still_serves` (the control: it\n*does* create `wal/`) |\n| 4 | Windows/Codex documentation | `docs/guides/daemon-mode.md` §3;\n`daemon_mode_docs.rs` parses the documented config blocks and asserts\nthey name the env var the binary reads |\n| 5 | Tests prove multiple sessions ≠ independent DBs/writers |\n`two_mcp_sessions_share_one_database_through_the_daemon`,\n`two_relay_sessions_share_one_database` (concurrent, interleaved,\n`count_nodes == 2`),\n`second_daemon_refuses_a_data_dir_owned_by_a_live_daemon` |\n| 6 | Explicit shutdown behavior |\n`daemon_mode_proxy_exits_when_client_disconnects`,\n`daemon_mode_delivers_in_flight_replies_before_exiting`,\n`daemon_mode_survives_the_daemon_dying_mid_session`; guide §4 |\n\n## Testing\n\n- 5,832 root lib unit tests pass (includes 30 new: `daemon_lock`,\n`mcp::proxy`).\n- `cargo test -p aletheia-server` fully green, including 9 new\nend-to-end tests that drive the **real** `aletheia-daemon` binary.\n- 10 new proxy tests drive the **real** `aletheia-mcp` process against a\nhand-rolled stub daemon (assertions on bytes on the wire, headers\nincluded).\n- `cargo clippy --all-targets --all-features -- -D warnings` clean on\nboth crates; `cargo fmt --all` clean.\n- **Gating fix**: the justfile's `test` recipe and CI now run the\n`mcp-server` features and `-p aletheia-server` — previously a bare\n`cargo test` compiled the proxy suite to *zero* tests, and the server\ncrate ran only under the coverage job.\n\n## Review\n\nFour review passes (security, concurrency/correctness, test quality,\nAPI/docs accuracy) ran against the first draft; their confirmed findings\nare fixed in this branch — notably the lock TOCTOU and unconditional\nrelease, the unlocked `ALETHEIADB_CONFIG` path, macOS liveness, JSON-RPC\nbatch frames misclassified as notifications (which would hang a client),\nunbounded response/frame buffering, the missing embedded-mode\nsecond-writer guard, and the fact that `aletheia-daemon` isn't produced\nby `cargo install --path .` (now documented, with a `just\ndaemon-install` recipe).\n\n## Notes / follow-ups\n\n- The daemon binary lives in a workspace member that is not a\n`default-member`, so it needs `cargo install --path\ncrates/aletheia-server` (documented; `just daemon-build` / `just\ndaemon-install` added).\n- `--surface legacy` preserves today's `aletheia-server` behavior for\nanyone depending on the polymorphic `POST /query`.\n- The lock is **advisory** (pid-file based): it prevents accidents, not\na process that ignores it outright. Stated plainly in the guide.\n- Pre-existing flakiness observed in\n`crates/aletheia-server/tests/changefeed_await_slot_pinning.rs` under\nparallel load (3 timing-sensitive tests); they pass with\n`--test-threads=1` and are unrelated to this change.\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_01RPtMcyZuG4z1JeJ6GahsBz\n\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_01RPtMcyZuG4z1JeJ6GahsBz)_\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-07-29T20:53:18-05:00",
          "tree_id": "9a111a34aa0bcafc57989f25c4add5e8bec98491",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/43a99977427f1ea34b9e68e18e7992ae696fef34"
        },
        "date": 1785377852113,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 554343.5674868526,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 155.70917439141758,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 175.494089967758,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 175.78750922295006,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 182.0029964446347,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 23.663574561085365,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}