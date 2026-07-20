{
  "lastUpdate": 1784514115190,
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
          "id": "cdc8ba2ba41c8ccfb018ebb1f2a8b32496267065",
          "message": "docs: DBOS-style durable-execution store design sketch (#3577) (#3744)\n\n## Before / After\n\n**Before:** no design exists for using AletheiaDB as a DBOS-style\ndurable-execution store.\n\n**After:** a design-only sketch\n([`docs/plans/2026-07-19-dbos-design-sketch.md`](docs/plans/2026-07-19-dbos-design-sketch.md))\nmaps the workflow-journal / exactly-once, atomic-claim, and notify\nrequirements onto AletheiaDB's shipped primitives.\n\n## What's in the doc\n\n- Problem / motivation\n- Goals & non-goals\n- Three candidate architectures (chosen: **graph-native workflow\njournal**)\n- The design composed on shipped primitives:\n  - `apply_batch` + unique-constraint (exactly-once recording)\n  - CAS / lease claims (atomic step claim)\n  - changefeed (#3375) for wakeups\n  - bi-temporal `AS OF` for time-travel debugging\n- A reference executor loop\n- Ideation: brainstorm + reverse-brainstorm + six-hats\n- Phased implementation outline\n- Risks-as-tests\n- Open questions\n- Acceptance-coverage table\n\n## Key honest finding\n\nExactly-once recording + notify **compose on shipped primitives**, but\n**SAFE multi-executor fencing needs a small primitive extension**:\n\n- `claim_with_lease` has no server-side fence (stale-fence collision on\nthe lease-steal path), and\n- `apply_batch` (`src/mcp/batch.rs:108`) has no CAS op (record txn\nunfenced).\n\nBoth are documented as open questions plus a new **Phase 3e**, not\npapered over.\n\n## Notes\n\n- **DESIGN-ONLY** — no code changes.\n- `cargo fmt --all --check` clean.\n- Links issue #3577.\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-07-19T20:59:48-05:00",
          "tree_id": "e1d52fd4a708f6a6ae39ce7e64f76d84b7e91b58",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/cdc8ba2ba41c8ccfb018ebb1f2a8b32496267065"
        },
        "date": 1784514115189,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 152.09632089100288,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 24.274380277207324,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 534591.5873515024,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 181.73974032029116,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 177.16015011427524,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 175.2693873355211,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}