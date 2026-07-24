{
  "lastUpdate": 1784874111186,
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
          "id": "026fb101c16d33ffd8d7726eb5ce6108b16d13b1",
          "message": "Asynchronous replication: read scale-out and manual failover (#3355) (#3779)\n\nImplements #3355 — asynchronous WAL-shipping replication with strictly\nread-only replicas, replication-lag observability, manual promotion, and\nfailure handling.\n\nBuilt in TDD slices, all landed:\n\n- [x] Design plan (`docs/plans/2026-07-23-3355-async-replication.md`)\n- [x] Slice A — node roles + read-only replica enforcement (Rust API,\nMCP, HTTP) + stats skeleton\n- [x] Slice B — replication engine: WAL feed, replica applier\n(whole-commit-frame apply), bootstrap from `.albk`, resync-required,\npromotion\n- [x] Slice C — TCP transport (framed protocol, token auth, snapshot\nstreaming), `[replication]` unified-config wiring, `POST /admin/promote`\n- [x] Slice D — chaos test (primary kill mid-load → promote → verify) +\nRPO/RTO/lag/overhead measurement harness\n- [x] Slice E — docs: replication guide, promotion runbook, ADR-0059,\nCLAUDE.md section\n- [x] Adversarial review round — 2 critical + 4 major findings fixed\n(see below)\n\n## Design\n\nPull-based shipping of durable (flushed) WAL entries: the replica polls\nthe primary&#39;s feed and applies only whole commit frames through the\nsame replay engine crash recovery and PITR use, so replicas never expose\na torn transaction and the primary write path carries no replication\noverhead. Bootstrap reuses the `.albk` backup artifact (consistent at\nits recorded `source_lsn`). Promotion stops the applier, seeds WAL LSN\ncontinuity, persists indexes, and flips the role.\n\n## Acceptance criteria verification\n\n| AC | Status | Evidence |\n|----|--------|----------|\n| 1. Configure primary, replica bootstraps + follows WAL in commit order\n| ✅ | `tests/replication_tcp.rs` (config auto-start, snapshot bootstrap,\nstreaming parity), `tests/replication_engine.rs` |\n| 2. Replicas strictly read-only on all 3 surfaces, structured role\nerror | ✅ | `tests/replication_role_enforcement.rs`,\n`tests/replication_mcp_readonly.rs` (full write/admin tool sweep),\n`tests/replication_http_readonly.rs`; `FAILED_PRECONDITION` +\n`details.reason: &#34;read_only_replica&#34;` |\n| 3. Consistency contract documented + testable (no torn state; AS OF\nparity ≤ applied position) | ✅ | Guide &#34;Consistency contract&#34;;\n`replication_engine.rs::as_of_temporal_parity_mid_stream`,\n`torn_frame_safety_never_exposes_a_partial_transaction` |\n| 4. Lag observable (applied LSN, entries/time behind) via stats | ✅ |\n`replication_progress()` + `DatabaseStats.replication` (flows through\nMCP `database_stats`);\n`stats_report_entries_behind_while_pending_then_zero_once_converged` |\n| 5. Manual promotion + fencing runbook | ✅ | `promote_to_primary()` →\n`PromotionReport`; `POST /admin/promote` (admin class);\n`docs/guides/promotion-runbook.md` |\n| 6. Reconnect/resume without full resync; structured resync-required\nstate | ✅ | `reconnect_after_server_restart_resumes_without_resync`,\n`restart_resumes_from_persisted_applied_lsn_not_from_scratch`,\n`resync_required_*` tests |\n| 7. Documented RPO/RTO with reproducible measurement | ✅ |\n`tests/replication_slo_harness.rs` (CI-sized asserted + `--ignored`\nreference fixture); measured numbers in guide/runbook |\n| 8. Chaos test in CI: kill primary mid-load, promote, verify | ✅ |\n`tests/replication_chaos.rs` (≥2000 acked writes, suffix-only loss per\nwriter, bi-temporal history checks, 12 consecutive passes) |\n\n**Measured (reference fixture 10K nodes / 50K edges):** promotion RTO\n17–23 ms (target &lt; 10 s); primary write-path overhead ≈ 0 (target ≤\n5%); CI-sized lag p99 254 ms.\n\n## Adversarial review round (fixed in `b95c5ab`)\n\nAn adversarial review of the full diff surfaced 2 critical and 4 major\ncorrectness findings, all fixed with regression tests:\n\n- **LSN contiguity (critical)**: the striped WAL&#39;s durable stream is\nnot dense at the read frontier, so the feed could ship past a gap →\nreplica silently skips committed transactions or applies a torn frame\n(this reproduced in CI as the `torn_frame_safety` failure). Feed now\nwithholds at the first discontinuity; applier independently validates\nbatch contiguity.\n- **Bootstrap coordinate race (critical)**: `backup()` read `source_lsn`\nwithout the commit clock, so a transaction mid append→apply could be\nmissing from both the snapshot and the resume stream. Now captured under\n`current_timestamp`.\n- **Resync soundness**: a truncation watermark keeps `min_available_lsn`\ntruthful after all sealed segments are truncated (was indistinguishable\nfrom a fresh WAL); TOCTOU between check and read closed.\n- **Promotion**: `last_applied_lsn` read after joining the applier (was\nracy, could seed the LSN allocator inside applied history);\nindex-persist failures surfaced via\n`PromotionReport.index_persist_error` instead of swallowed.\n- **Apply-error bookkeeping**: complete-frame prefix drains from\n`pending` only after successful replay; errored batches retry\nidempotently instead of corrupting `from_lsn` arithmetic.\n- **Oversize batches**: TCP server packs the longest frame-fitting\nprefix instead of erroring into a permanent reconnect loop;\n`MAX_FRAME_SIZE` &gt; `MAX_WAL_ENTRY_SIZE` + framing so any legal entry\nships. Hardening: 64 KiB pre-auth frame cap, decode preallocation\nbounded by payload length, DNS hostnames accepted for `primary_addr`.\n\nReview minors deliberately deferred (documented): replica manifest LSN\noff-by-one convention (masked by idempotent replay), demotion-direction\ncheck-then-act race (requires live demotion), no TLS (v1 scope), chained\nreplication not supported (doc claim corrected — the applier does not\nappend to the replica&#39;s local WAL).\n\n## Known limitations / follow-ups\n\n- **Config-only `listen_addr` serves entries but not snapshots** (no\n`Arc` exists during construction): bootstrap of a fresh replica requires\nthe programmatic `ReplicationServer::start`. Documented in the\nguide&#39;s quick start.\n- **Reference-load lag under sustained heavy write load** exceeds the\n500 ms success-metric aspiration due to the applier&#39;s per-batch\ntemporal-index rebuild (documented perf follow-up in `applier.rs`).\n- **No TLS in v1** — token auth only; run on a trusted network\n(documented).\n- **A permanent WAL allocator hole** (serialize-failure reserved LSNs)\nnow surfaces as observable, growing replication lag rather than silent\ndata loss; a stalled replica in that rare state needs re-bootstrap. The\ntruncation watermark is in-memory (post-restart, gap-withholding is the\nbackstop).\n- **Pre-existing, replication-independent bug found by the chaos work**:\nWAL replay reconstructs `CreateNode` version ids from a local counter,\nwhich can collide under genuinely concurrent multi-threaded writers on\ncrash recovery (~2.8% entities in a 3-thread repro; 0 single-threaded).\nDocumented in `docs/testing/crash-scenarios.md` Gaps; proper fix (logged\nversion id + WAL wire-version bump) deserves its own issue.\n- Replica read-throughput scaling benchmark (2-replica ≥ 2.5× metric)\nnot yet run — replica reads use the unchanged current-state read path;\nharness follow-up.\n- In-scope engine bug found and fixed by the chaos slice:\n`apply_replica_batch` now detaches/reattaches temporal indexes around\nreplay (was a `debug_assert` panic killing the applier on\ncreate+update-same-entity batches).\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_01AKNaovG1dmNr16ZsJCPKyr\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-07-24T01:08:41-05:00",
          "tree_id": "8a65f718593f96dadb8d88190244393fc4fab9ef",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/026fb101c16d33ffd8d7726eb5ce6108b16d13b1"
        },
        "date": 1784874111185,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 160.862157506138,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 19.141977697876406,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 499050.48939063505,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 198.3274735103142,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 191.43190158668816,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 186.8717681712411,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}