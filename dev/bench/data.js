{
  "lastUpdate": 1788042779794,
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
          "id": "ac555489ede6b25ed2a280f88c21c4676063984d",
          "message": "Lock-free snapshot reads and multi-threaded scaling improvements (#3811)\n\n## Summary\n\nThis PR implements three major improvements to AletheiaDB's concurrency\nand scalability:\n\n1. **Lock-free snapshot reads** via a new `CommitClock` type that\nreplaces the global `Mutex<Timestamp>` with a 128-bit atomic frontier\ncarrying an in-flight bit and reservation bit\n2. **Fixed DashMap shard distribution** by replacing identity hashing\nwith Fibonacci hashing for sequential integer IDs\n3. **Reduced Arc clone overhead** by bundling read transaction handles\ninto a single `ReadHandles` struct\n\nThese changes address the four primary serialization points identified\nin the concurrency scaling analysis (see\n`docs/plans/2026-08-26-multi-reader-multi-writer.md`).\n\n## Key Changes\n\n### CommitClock: Lock-Free Snapshot Reads (Stage 2)\n\n- **New module** `src/core/commit_clock.rs` (~830 lines) implements a\nhybrid-logical commit clock with:\n- 128-bit atomic frontier encoding: 96-bit packed timestamp + IN_FLIGHT\nbit (127) + RESERVED bit (126)\n  - Lock-free read path for snapshot timestamp allocation via CAS loop\n  - Separate `applied` frontier for observability\n  - `CommitClockGuard` for writer serialization (unchanged from before)\n- Comprehensive module documentation explaining the protocol, why one\natomic is insufficient, and the read/write protocols\n\n- **Replaced** `Arc<Mutex<Timestamp>>` with `Arc<CommitClock>`\nthroughout:\n- `src/db/transaction.rs`: Removed `snapshot_timestamp_for_read()`\nmutex-based implementation\n- `src/api/transaction/write/mod.rs`: Updated write transaction to use\n`CommitClock`\n- `src/db/mod.rs`, `src/db/config.rs`: Initialize `CommitClock` instead\nof bare mutex\n  - Test updates to use `CommitClock::new()`\n\n- **Key insight**: Readers no longer contend on the writer's mutex. The\nin-flight bit prevents snapshot-isolation violations by ensuring readers\ncannot observe a commit's timestamp before its writes are durable.\nReservations allow subsequent readers to share the same snapshot without\natomic RMW, bounding staleness to clock resolution.\n\n### Fibonacci Hashing for DashMap (Stage 1)\n\n- **Enhanced** `src/core/hasher.rs`:\n- Introduced `IdHasher` backed by Fibonacci multiplier\n(`0x9E37_79B9_7F4A_7C15`)\n- Changed `IdHashBuilder` to use `IdHasher` instead of `IdentityHasher`\n  - Kept `IdentityHasher` public for non-DashMap use cases\n- Added extensive documentation explaining why sequential IDs need\nhigh-bit entropy for DashMap shard selection and hashbrown control bytes\n\n- **Updated all DashMap usages** to use `IdHashBuilder`:\n  - `src/core/interning.rs`\n  - `src/core/namespace.rs`\n  - `src/experimental/characterization/synapse.rs`\n\n- **Measured improvement**: ~3.5x better scaling at 4 threads (0.27x →\n0.95x) on read-heavy workloads by distributing entries across all shards\ninstead of concentrating in shard 0.\n\n### Reduced Arc Clone Overhead (Stage 2.5)\n\n- **New struct** `ReadHandles` in `src/api/transaction/read_tx.rs`\nbundles three `Arc`s:\n  - `current: Arc<CurrentStorage>`\n  - `visibility_manager: Arc<TxVisibilityManager>`\n  - `historical: Arc<RwLock<HistoricalStorage>>`\n\n- **Updated** `ReadTransaction` to hold single `Arc<ReadHandles>`\ninstead of three separate `Arc`s\n- **Benefit**: One refcount increment/decrement per read transaction\ninstead of three, eliminating atomic traffic on three shared cache lines\n\n### TransactionSnapshot Optimization\n\n- **Changed** `active_transactions` from `Arc<HashSet<TxId>>` to\n`Option<Arc<HashSet<TxId>>>`\n- **\n\nhttps://claude.ai/code/session_01T8Jb8qQtwrQfX1v4j9k2hv\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-08-29T17:19:39-05:00",
          "tree_id": "4ee5dc0f358986c8905580c6c5e8de2757ff76ef",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/ac555489ede6b25ed2a280f88c21c4676063984d"
        },
        "date": 1788042779793,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 21.250097228406005,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 199.8667918585542,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 192.43666015849612,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 190.17527270868644,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 392610.1369047852,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 172.08083845430806,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}