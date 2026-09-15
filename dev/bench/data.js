{
  "lastUpdate": 1789488041870,
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
          "id": "906e581a16a4801f86cc5c8986ebc8c9fcfa0931",
          "message": "Bolt: add crash-recovery / WAL-replay workload benchmark (no fix) (#3835)\n\n## 🎯 Workload\n\n`examples/bolt_recovery_workload.rs` (new) — a deterministic,\npublic-API-only benchmark for `AletheiaDB::open()`'s cold-start path:\nthe \"Recovery Flow: Startup → Load Checkpoint → Replay WAL → Restore\nIndexes → Ready\" documented in `CLAUDE.md`, which cites `<5s for 10K\nnodes/50K edges` as the target for the \"medium\" scenario. No existing\nbenchmark profiles this through the public entry point at instruction\nlevel: `benches/recovery_benchmarks.rs` drives internal\n`CurrentStorage`/`ConcurrentWalSystem`/`CheckpointManager` structs\ndirectly and is a criterion suite (too slow stacked under valgrind's own\n20-50x slowdown).\n\nThis is two OS-process invocations of one binary, not a single\nself-contained run: `AletheiaDB`'s `Drop` always performs a final full\nindex checkpoint before returning (`src/db/mod.rs`,\n`src/storage/index_persistence/worker.rs`'s \"Final persist on\nshutdown\"), so a single-process write→drop→reopen cycle (as\n`tests/open_durable.rs` exercises for correctness) always reopens\nagainst an up-to-date checkpoint with an empty WAL tail — replay does\nnear-zero work. `setup` mode checkpoints a 3,000-node/12,000-edge\nbaseline via `persist_indexes()`, writes 800 more nodes + 3,200 more\nedges after that checkpoint (fanned across 32 threads so a lone caller\ndoesn't pay GroupCommit's full ~10ms batch-window latency per write —\nsee the file's doc comment), then calls `std::process::exit(0)` —\nskipping `Drop` entirely to simulate a crash right after the last\nacknowledged (already-fsynced under `GroupCommit`) write, leaving a real\nWAL tail on disk. `recover` mode then measures a fresh\n`AletheiaDB::open()` against that on-disk state in a separate process,\nso only checkpoint-load + WAL-replay cost is in the profiled process.\n\nReproduce:\n```\ncargo build --profile bench --example bolt_recovery_workload\n\nrm -rf /tmp/bolt_recovery_workload_data\nBOLT_MODE=setup BOLT_CKPT_NODES=3000 BOLT_TAIL_NODES=800 BOLT_OUT_DEGREE=4 \\\n    target/release/examples/bolt_recovery_workload\n\nBOLT_MODE=recover \\\n    valgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out -- \\\n    target/release/examples/bolt_recovery_workload\ncallgrind_annotate --threshold=95 /tmp/cg.out | head -40\n```\n\n## 📈 Profile\n\nvalgrind 3.22.0, `--tool=callgrind`, this session. Baseline (unpatched\ntrunk): **368,965,987 Ir** (mean of 2 runs against a byte-identical\non-disk fixture, <0.03% run-to-run spread — see Methodology below).\n\nTop self-cost lines (`callgrind_annotate --threshold=95`): `_int_malloc`\n11.00%, `malloc` 5.81%, `core::hash::sip::Hasher::write` 4.38%,\n`crc32fast::baseline::update_fast_16` 3.71%, `_int_free` 3.66%, `memcpy`\n2.69%, `hashbrown::raw::RawTable::reserve_rehash` 1.85%, `malloc::free`\n1.54%, `wal::segment_reader::parse_entry_at` 1.44% — malloc-family\nself-cost sums to ~23% of the profile.\n\nFollowing the `Hasher::write` call edges (`callgrind_annotate\n--tree=both`) traces a large share into `<dashmap::DashMap as\nMap>::_get`/`_entry` calls originating from\n`TemporalIndexes::insert_version` (`src/index/temporal/mod.rs`) — during\nboth checkpoint restoration's version-chain rebuild and WAL replay. That\nmap, `TemporalIndexes.index: DashMap<EntityId, EntityTimelines>`, is\nexactly the one identified (on a different, read-heavy workload) in\n#3818 as still on the default SipHash hasher — the one id-keyed\n`DashMap` in `src/` that missed the #3795 identity-hash pass every other\n`NodeId`/`EdgeId`-keyed `DashMap` already got.\n\n## 💡 Hypothesis\n\n#3818 measured this exact gap at only -0.80% Ir on `bolt_workload.rs`\n(1,000 nodes/6,000 edges, current-state-read-heavy — `TemporalIndexes`\nis only touched once per write there) and predicted: *\"It would likely\nclear the floor... on a write-heavy or temporal-query-heavy workload...\nrather than this current-state-read-heavy one.\"* Checkpoint restoration\n+ WAL replay is exactly that workload: every restored/replayed node and\nedge version calls `TemporalIndexes::insert_version` for both its\nvalid-time and tx-time timelines, so this benchmark is a direct test of\nthat prediction.\n\n## 🔧 Change tried\n\nIdentical diff to the one in #3818: swap `TemporalIndexes.index` from\n`DashMap::new()` (default `RandomState`/SipHash) to\n`DashMap::with_hasher(IdHashBuilder::default())` — the exact\n`IdHashBuilder` pattern already used for every other id-keyed `DashMap`\nin `src/index/current.rs` and `src/index/incremental_adjacency.rs`\n(#3795). Two-line change (one field-type annotation, one constructor\ncall), no other call site touched\n(`.entry()`/`.get()`/`.get_mut()`/`.iter()`/`.clear()` are all\nhasher-generic). `cargo test --lib index::temporal` — 71/71 pass\nunchanged; `cargo build --profile bench --example\nbolt_recovery_workload` clean.\n\n## 📊 Measurement\n\n**Methodology**: because `setup` writes across 32 concurrent threads,\ntwo independent `setup` runs produce slightly different WAL/checkpoint\nsplits (same final graph, different exact version counts — I hit this\nfirst-hand: 13,472 vs. 13,024 edge versions between two setup runs,\nwhich would have invalidated a naive before/after comparison). To\nisolate the change under test, the on-disk fixture from **one** `setup`\nrun was snapshotted once and restored byte-identical before every\nprofiled `recover` invocation, for both the unpatched and patched\nbinaries.\n\n`valgrind --tool=callgrind`, 2 runs each side against the identical\nfixture:\n\n| | Ir (run 1) | Ir (run 2) | Mean | Δ vs. baseline |\n|---|---:|---:|---:|---:|\n| Before (trunk) | 368,925,780 | 369,006,193 | 368,965,987 | — |\n| After (`IdHashBuilder`) | 359,336,477 | 359,427,996 | 359,382,237 |\n**-2.60%** |\n\nRun-to-run spread is <0.03% on each side, so -2.60% is signal, not noise\n— real and reproducible, better than #3818's -0.80%, but still under\nthis process's ≥5% instruction floor.\n\n`valgrind --tool=dhat`, same fixture: allocation count essentially\nunchanged (397,735 → 397,737 blocks, +2), bytes unchanged (153,738,007 →\n153,765,143, +0.02%) — expected, since this change only swaps a hash\nfunction, not an allocation pattern. Neither clears the ≥10% allocation\nfloor either.\n\nPer process rules, the change was **reverted** — this is a documented\nnegative result, not a shippable fix. Full before/after numbers and the\ndiff are also posted as a comment on #3818, which already tracks this\nexact gap.\n\n## 🔬 Reproduce\n\n```\ngit fetch origin claude/compassionate-mccarthy-dzm26g\ngit checkout 2f0bae8\ncargo build --profile bench --example bolt_recovery_workload\n\nrm -rf /tmp/fixture && BOLT_MODE=setup BOLT_CKPT_NODES=3000 BOLT_TAIL_NODES=800 BOLT_OUT_DEGREE=4 \\\n    BOLT_DATA_DIR=/tmp/fixture target/release/examples/bolt_recovery_workload\ncp -r /tmp/fixture /tmp/fixture_snapshot\n\n# restore /tmp/fixture from /tmp/fixture_snapshot before each run below\nBOLT_MODE=recover BOLT_DATA_DIR=/tmp/fixture \\\n    valgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out -- \\\n    target/release/examples/bolt_recovery_workload\ncallgrind_annotate --tree=both --threshold=80 /tmp/cg.out | less\n```\n\n## What this PR is for\n\nLanding the harness now gives a reusable, reproducible way to profile\n`AletheiaDB::open()`'s recovery path (checkpoint load + WAL replay)\nthrough the public API — something no existing benchmark does — and the\nnumbers above are a stronger, independently-reproduced data point for\n#3818's already-tracked gap, useful to whoever eventually bundles that\nhasher swap with an unrelated `src/index/temporal/` change (as #3818\nsuggests).\n\nCloses/references: #3818 (companion finding comment posted there)\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_01MJp4VMxJGqbvFeZV7HCQFv\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_01MJp4VMxJGqbvFeZV7HCQFv)_\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-09-15T10:50:01-05:00",
          "tree_id": "6b7c73f9373f0ac8980dc175b1f2b867ed750beb",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/906e581a16a4801f86cc5c8986ebc8c9fcfa0931"
        },
        "date": 1789488041870,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 18.785964332485175,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 118.91767012822254,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 128.814220110937,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 134.86935662319064,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 129.68704484810988,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 353909.48050400044,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}