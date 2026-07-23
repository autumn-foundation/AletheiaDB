{
  "lastUpdate": 1784833278231,
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
          "id": "1af53d3c10be0c5b88548e681483751fb83752e2",
          "message": "WAL abort framing: never replay a post-WAL-rejected transaction (#3413) (#3777)\n\n## Summary\n\nCloses the remaining half of Issue #3413 — **abort framing**. The commit\nframing\nalready on trunk (`[BeginTx, ..ops.., CommitTx]`) fixed torn-batch\nprefix-replay\nand per-entry timestamp bisection. It did **not** fix the case this PR\npins: a\ntransaction whose **complete, valid** frame is durably fsync'd and is\n**then\nrejected** at the commit guard. Because the WAL write precedes the guard\ncheck\nand crash recovery replays frames without re-running preconditions, a\nwrite\ncorrectly refused **at runtime** was silently **re-applied on crash\nrecovery** —\nresurrecting exactly the zombie the guard refused (a lost CAS/lease\nclaim, a\nstale DBOS fence, an orphaning delete / dangling edge). This is the\nload-bearing\ncaveat documented by merged PR #3755.\n\nDesign doc: `docs/plans/2026-07-22-wal-abort-framing.md`.\n\n## Rejected-path inventory (every post-WAL-write rejection)\n\nAll three ran in `apply_changes` under `historical.write()`, **after** a\ndurable frame:\n\n| Path | Error | Exposed via | Pre-WAL fast-path? |\n|---|---|---|---|\n| `detect_cas_precondition_violations` (version / lease / **fence**) |\n`CasMismatch` / `FenceTooLow` | `compare_and_set_*`,\n`claim_with_lease[_fenced]`, `apply_batch` CAS | Only single-threaded\n**pure** CAS; a concurrent pure-CAS loser or **any lease/fenced claim**\nreached here |\n| `detect_delete_orphan_write_skew` | `ValidationFailed` | `delete_node`\n/ `retract_node` (#3416) | No |\n| `detect_create_edge_dangling_endpoint` | `ValidationFailed` |\n`create_edge` (#3416) | No |\n\n**Not part of the gap (verified pre-WAL):** uniqueness (#3218) + schema\n(#3378)\nconstraints, SI write-write conflicts, referential-integrity validation\n— all\nabort before the `current_timestamp` lock and WAL append.\n\n## Fix — guard-before-log\n\nEvery guard above reads **only current storage** (never `historical`);\nit sat\nunder `historical.write()` purely as a serialization barrier. The fix\nmoves all\nthree to run under `current_timestamp` **before** the WAL append, and\nholds\n`current_timestamp` **across apply** so a guard that passes stays valid\nthrough\napply (no other committer can apply in between). A rejected transaction\nreturns\nwith **no WAL frame appended** — nothing for recovery to reapply.\n\n- **No on-disk WAL format change**, no new `WalOperation`; the recovery\nresolver\nand all segment formats (incl. v16 KEYVERSIONED-encrypted, even-parity)\nare\n  untouched, so old segments replay identically.\n- **Lock order preserved** (`current_timestamp → wal → historical`):\n`wal` and\n`historical` are each acquired only while holding `current_timestamp`;\n`wal` is\nnever appended under `historical` (the inversion the WAL analysis warned\n  against).\n- Recovery is untouched → `<5s` medium-dataset target and replay\nidempotency\n  unchanged.\n\n## Notable interaction — lost-write persist race subsumed\n\nHolding `current_timestamp` across apply makes `persist_indexes()`\n(which needs\n`current_timestamp` briefly) **serialize behind any in-flight commit's\napply**,\nso it can never snapshot a durable-but-unapplied write and two commits\ncan no\nlonger overlap. The lost-write persist race (2026-07-13) is therefore\n**structurally impossible**; its in-flight watermark is now\nbelt-and-suspenders\n(retained, harmless), and `tests/lost_write_persist_race.rs` is updated\nto pin\nthe stronger invariant (a parked commit **blocks** a racing persist /\nsecond\ncommit; nothing lost or duplicated across a crash). The `#[cfg(test)]`\npre-apply\nwrite-skew seam is repositioned to fire **before** the commit clock\n(`pre_commit_clock`) to avoid a self-deadlock. Cost disclosed: a\ncommit's\nin-memory apply no longer overlaps the next commit's WAL append\n(in-memory apply\nis cheap; a micro-benchmark is deferred).\n\n## Tests (TDD, red → green)\n\n- **`tests/wal_abort_framing.rs`** (new): a **deterministic\nsingle-threaded\nfenced claim** (`new_fence ≤ stored`, excluded from the pre-WAL\nfast-path)\nleaves a durable frame today; forced WAL replay resurrects `fence=5`\n(**RED on\ntrunk**) → gone after the fix. Plus a concurrent orphaning-delete crash\ntest\n  and a positive control (committed fenced claim survives replay).\n- Reworked `lost_write_persist_race.rs` (serialization invariant) and\nthe #3416\n  `test_create_edge_write_skew_concurrent_delete_aborts` (new seam).\n- Green: `wal_tx_framing` (11), `recovery` (82), `cas_lease` (19),\n`dbos_phase3e_fence` (6), the #3416 write-skew suite, full `--lib`\n(4479), plus\nwal/recovery/checkpoint/backup/encryption/DBOS-workflow integration\nsuites.\n- Stale #3413 \"runtime-only / replays on crash\" caveats updated across\n  `cas.rs`, `ops.rs`, `error.rs`, `mod.rs`, `conflict.rs`.\n\n## Review\n\nTwo independent adversarial reviews (concurrency + crash-recovery\nlenses) found\n**no correctness or deadlock bug**; three documentation-accuracy\nfindings were\napplied.\n\n## Gates\n\n- `cargo +stable fmt --all -- --check` — clean.\n- clippy `-D warnings`: CI set\n(`config-toml,mcp-server,sharding-rpc,simulation`),\n  `--no-default-features`, and `--all-features` — all clean.\n- `cargo check --no-default-features --tests` — clean.\n- No `unsafe` touched (Miri N/A).\n\n## DRAFT / sign-off\n\nNo on-disk format change, so backward-compatible by construction. Kept\nDRAFT for\nmaintainer review of the commit-path reordering (HIGH blast radius) and\nthe\nlost-write test rework; a merge is the sign-off.\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-07-23T13:35:38-05:00",
          "tree_id": "09e8baa7c1804c477827b03d2d0fbfb00ae98215",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/1af53d3c10be0c5b88548e681483751fb83752e2"
        },
        "date": 1784833278230,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 484618.1246797746,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 156.2655410570154,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 176.42174237633049,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 173.28778453462616,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 181.19135607976528,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 19.162977140761793,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}