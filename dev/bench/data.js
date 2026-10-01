{
  "lastUpdate": 1790871063503,
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
          "id": "31d68a07ca98ffeabec89a79f9e93f6d00821547",
          "message": "fix(temporal): append-only valid-time supersession on update (ADR-0061) (#3852)\n\n## Summary\n\n`update_node_with_valid_time` / `update_edge_with_valid_time` (and every\nplain update) closed only the predecessor's **transaction** time. As of\nthe current transaction time the predecessor was then invisible at every\nvalid time. So after `create(valid_from = t1)` → `update(valid_from =\nt2)`, `get_node_at_valid_time(id, mid)` with `t1 < mid < t2` returned\n`NodeNotFound`.\n\nThe issue suggests closing the predecessor's valid interval in place.\nThat is exactly what **#3504** removed: it breaks snapshot isolation for\nearlier transaction-time readers, and\n`tests/readops_snapshot_isolation.rs` covers it. The predecessor's\nbelief is L-shaped (tx `[c1,c2)` × valid `[t1,∞)` ∪ tx `[c2,∞)` × valid\n`[t1,t2)`), so this PR adds the missing rectangle instead (approved\napproach, see ADR-0061):\n\n- **Carry-forward.** The update transaction-closes the slice containing\n`t` and appends a *structural* version with the predecessor's content\nover `[old_valid_from, t)`. The original record is untouched, so #3504\nholds. A live entity's still-recorded versions now partition valid time\nwith no gaps and no overlaps.\n- **Backfill** between versions covers only `[t, next_valid_from)` and\nre-asserts the open head. The successor's interval and the current state\nare not touched.\n- **Equal `valid_from`** is a degenerate replace: no carry-forward and\nno empty interval.\n- **Retraction**: retract-after-update keeps the earlier slices.\nUpdate-after-retract is still rejected and leaves history unchanged.\n- **Delete** transaction-closes every slice, so delete semantics are\nunchanged: the entity is absent at every valid time as of the delete,\nand system time before the delete still shows the slices.\n- **Temporal indexes**: carry-forwards are inserted into\n`temporal_indexes`. The temporal adjacency index now closes the exact\nversion's entry instead of \"the edge's latest one\", because an edge can\nhave several open slices.\n\n### Structural versions\n- Their ids carry a reserved tag bit (`1 << 62`,\n`VersionId::is_structural`). The marker survives WAL replay, index\npersistence, the cold tier and backups with **no format change**.\n`IdGenerator::{reset_to, ensure_at_least}` strip the bit.\n- They are excluded from `list_changes` (so pull and push feeds stay\nidentical), from `get_*_history` and `get_*_at_version` (history lists\nwrites, so numbering is unchanged), and from the per-entity version cap\n(so update headroom is unchanged).\n- New `get_node_valid_time_slices` / `get_edge_valid_time_slices` return\nthe current valid-time partition. This is where acceptance criterion 2\n(version 1 shown over `[t1, t2)`) is visible.\n- Still-recorded versions never migrate to cold storage (cold versions\nare immutable, and a later delete or backfill must close them).\n\n### Recovery / replication\nLive apply and WAL replay share one planner\n(`storage/historical/slices.rs`). Replay re-derives the same plan and\nmints structural ids in step with the primary's allocation. An earlier\ndraft seeded replay's id synthesizer above every logged id; that broke\nincremental replication (`replication_chaos` caught a version-id\ncollision across batches), so it was removed.\n`rebuild_version_chains` now treats persisted links (#3387) as\nauthoritative per entity. The tx-sort heuristic only runs for legacy\nfiles, which predate structural versions. The head is found by following\n`next_version`.\n\n### Behavior changes to review\n- **Cypher `BETWEEN`** now returns each distinct state a node is\n*currently believed* to have across the range. A→B at `v2` yields\n`[\"A\",\"B\"]`, where it previously yielded `[\"B\"]`, which was the bug. The\ntwo #552 tests and their oracle (previously keyed per node) are updated,\nand each gains a genuinely-superseded case (an equal-`valid_from`\ncorrection) that must still be excluded.\n- **Raw storage counts** (`stats()`, `historical_stats()`,\n`get_node_versions()`) include structural versions: an ordinary update\nstores 2 versions and a backfill stores 3. Every stored version advances\nthe anchor counter, so anchors land every ~5 writes instead of every 10\n(more anchor memory, shallower reconstruction). Tests asserting these\ncounts are updated with comments.\n- Counterfactual replay folds over write history and rebuilds its shadow\nwith the legacy heuristic. It behaves exactly as before; it just doesn't\ngain carry-forward slices (possible follow-up).\n\n## Test plan\n- [x] New `tests/update_valid_time_supersession.rs` (15 tests): the\nissue repro (node and edge), history vs slices, #3504 earlier-tx view,\nbackfill (node and edge), equal `valid_from`, retract-after-update,\nupdate-after-retract, delete-after-update, plain-update partition, a\n**model-based** random forward/backfill test against a breakpoint map,\nchangefeed, **WAL-only replay** and **durable reopen ×2**.\n- [x] Unit tests: planner cases, open-slice index across restore/delete,\nthe logical-version cap checked before mutation, migration skips\nstill-recorded slices, structural id and generator re-seed.\n- [x] RED check: the repro fails on trunk (`NodeNotFound`) and passes\nhere.\n- [x] `cargo test --features\n\"config-toml,mcp-server,sharding-rpc,simulation,cypher\"` (CI set): lib\n5945, doc tests 431, and all integration targets pass. The exceptions\nare `havoc::test_flush_deadlock_on_io_error` and\n`regression_flush_corruption`, which fail only because this sandbox runs\nas root (their read-only-dir I/O fault injection is bypassed). They\ndon't depend on this change.\n- [x] `cargo test -p aletheia-server`; nova and semantic cohorts: lib\n6285 plus belief-revision / contradiction / counterfactual / drift /\nhalf-life e2e suites.\n- [x] `cargo clippy --all-targets --all-features -D warnings`, `cargo\nclippy` with the CI feature set, `cargo fmt --check`.\n- [x] Bench `benches/update_supersession.rs`. Against trunk, end-to-end\nupdate latency is unchanged within noise (plain p=0.87, backfill p=0.20;\nWAL fsync dominates under default durability). An as-of read between\nversions takes ~290 ns (trunk returned `NodeNotFound`).\n\nDocs: `docs/adr/0061-append-only-valid-time-supersession.md`, ADR index,\nCLAUDE.md.\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_012BTryMc27FsakZFP2nJyJQ\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_012BTryMc27FsakZFP2nJyJQ)_\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-10-01T10:57:27-05:00",
          "tree_id": "d82fff45cce04acf711fdc3b359f723fef8e731e",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/31d68a07ca98ffeabec89a79f9e93f6d00821547"
        },
        "date": 1790871063502,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 21.000132395136305,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 172.77608382998253,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 239.4449152735954,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 241.29900955495512,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 194.91952345262573,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 403580.9598858632,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}