{
  "lastUpdate": 1787153250618,
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
          "id": "87b17dc5036ed48079b157fb0fd49d87d3865e00",
          "message": "WAL #3798: bounded, re-entrancy-safe group-commit locking + diagnosable append backpressure and flusher liveness (TDD) (#3806)\n\nFixes the silent-stall class behind #3798 (WAL group-commit: indefinite\nblock acquiring the state mutex, 4h14m observed) via strict\nred→green→refactor TDD, followed by three adversarially-verified review\nrounds.\n\n## What the audit established\n\nThe issue's leading hypothesis — re-entrant acquisition of the\ngroup-commit state mutex — is **falsified by construction**: all\nstate-mutex acquisitions live in `group_commit.rs`, no critical section\ncalls another coordinator method, `mark_flushed` is strictly sequential,\nand the guard type never escapes the module. The audit is committed as a\nmodule-doc section so it survives refactors.\n\nWhat the audit **did** confirm as real indefinite-block mechanisms\nmatching the reported signature (zero CPU, threads parked around\n`concurrent_system`/`ring_buffer`/`stripe` frames):\n1. `WalRingBuffer::append_blocking` looped forever on a full buffer with\nno deadline — reached from inside the commit path while\n`current_timestamp` is held, which amplifies one stuck writer into every\ncommitter parked in `Mutex::lock`.\n2. `finish_flush` ran `notify_all()` while still holding the state guard\n— an up-to-200-waiter thundering herd onto a held mutex.\n3. A dead background flusher was undetectable: `is_healthy()` only read\nan error counter a dead thread can never bump, and the flusher itself\ncould be killed by a panicking `eprintln!` (EPIPE) or a\npoison-`.expect()` on any coordinator error.\n\n## What shipped\n\n**Group-commit lock hardening** (the issue's explicit asks):\n- Single `lock_state` chokepoint: deterministic **re-entrancy\ndetection** (never-recycled per-thread owner tokens; structured error,\nnever a self-deadlock; opt-in `ALETHEIADB_WAL_REENTRANCY_PANIC`\nescalation), **bounded acquisition**\n(`GroupCommitConfig::acquire_timeout_ms`, default 120s — strictly above\nthe existing `wait_for_flush` deadlock-detection ceiling, test-enforced;\n`0` = legacy unbounded), and **forensic error payloads** (site, thread\ntokens, owner site, waiters, elapsed, epoch/batch mirrors; commit-path\nsites disclose durability status).\n- `finish_flush` now drops the guard before `notify_all` (200-waiter\nwakeup improved **19%** in the bench).\n\n**The confirmed mechanisms**:\n- Bounded, diagnosable WAL appends on **all** paths (async, batch,\nsync/handle): `max_append_block_ms` (default 30s, `0` = unbounded),\nstall-not-throughput semantics (progress re-arms the window;\nslow-but-healthy bulk imports complete), mode-true diagnosis\n(Synchronous-mode errors name the caller-drains reality, not a\nnonexistent flusher), `Closed` precedence preserved, zero clock reads on\nthe never-blocked fast path.\n- Flush-thread liveness: monotonic heartbeat + `is_healthy()` staleness\n(floor 5s), `flush_cycle_errors()`, non-panicking logging on every\nflush/drain-thread path (incl. `flush_coordinator.rs`), and the flusher\n**survives** non-poison coordinator errors instead of dying.\n- Durability honesty: the flusher can never silently drop a flush\noutcome (pending-finish FIFO queue + carried-failure fold — worst case\nover-reports failure, never reports false success), and both new knobs\nare reachable end-to-end from `AletheiaDBConfig`/TOML.\n\n## TDD + review process\n\n- Commits: 2× red (12 designed-failing tests, verified failing for the\ndesigned reasons), 2× green, 1× refactor/docs, then 3 review rounds\n(multi-lens adversarial review with refute+reproduce verification; every\nconfirmed finding fixed red-first or explicitly filed).\n- Tests: lib suite 4,583 → **4,618** (0 failures), +2 integration tests,\ndoc tests green, zero-flake runs on all new concurrency tests.\n- Gates: `cargo clippy --all-targets --all-features -- -D warnings`\nclean, `cargo fmt --all --check` clean.\n\n## Benchmarks (criterion, same container, pre-change baseline at\nc6b170d)\n\n| Bench | Pre | Post | Delta |\n|---|---|---|---|\n| `register_transaction` uncontended | 17.45 ns | 22.57 ns | +5.1 ns\n(owner arm/clear + mirror stores; far below the fsync-dominated path it\nfeeds) |\n| `register_transaction` contended ×8 | ~73 ns | 26.1 ns | **−64%** |\n| `finish_flush` wakeup, 200 waiters | 19.85 ms | 16.01 ms | **−19%** |\n| `single_transaction_latency/group_commit_default` | — | ~3.07 ms |\nwithin the documented 10–50 ms envelope |\n\n## Follow-ups filed (out of scope here, all cross-referenced in\ncode/docs)\n\n- #3799 commit-path abort record (durability-unknown → determined)\n- #3800 retriable classification for the append timeout (needs a semver\nwindow)\n- #3801 `shutdown_graceful` bound · #3802 `CompletionNotifier::wait`\nbound\n- #3803 ladder fairness at 64+ threads · #3804 fsync under the\nflush-coordinator writer mutex\n- #3805 **(P1, pre-existing)** append→register epoch-attribution race —\ninvestigated in round 3; the cheap LSN-watermark guard was proven\nunsound (watermark advances pre-fsync and isn't a contiguous frontier),\nso the gap is documented as KNOWN GAP at four surfaces and needs a\nprotocol-level fix.\n\n#3798 itself should stay open after merge: the reporter's specific 4h\nevent remains unreproduced (re-entrancy falsified; the\nbounded/diagnosable behavior here converts any recurrence into an\nactionable forensic error). A full `sample`/`lldb` all-threads capture\nfrom the reporter would settle the remaining attribution.\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_01JLYK7CRHgazcBrRSYgDCBv\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_01JLYK7CRHgazcBrRSYgDCBv)_\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-08-19T10:16:27-05:00",
          "tree_id": "e75ec99bc8a8ebe5c758fbe1780041a28dd1172b",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/87b17dc5036ed48079b157fb0fd49d87d3865e00"
        },
        "date": 1787153250618,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 16.486260905762293,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 154.61418563819575,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 146.98484269377386,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 145.07106821317157,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 249977.93325565298,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 139.3055581122164,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}