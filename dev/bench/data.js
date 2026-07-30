{
  "lastUpdate": 1785436177416,
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
          "id": "2904486b4dd0a40819567faa26bbf9e694444934",
          "message": "fix(replication): isolate replica reads from an in-flight batch apply (#3788) (#3789)\n\nFixes #3788.\n\n## The gap\n\n`apply_replica_batch` is correct about frame **selection** —\n`last_complete_frame_boundary` never submits a partial `[BeginTx ..\nCommitTx]` band. But the selected frame is then applied **operation by\noperation** into `CurrentStorage`, and current-state reads take no lock.\nA read landing inside that window could observe one node of a two-node\ncommit and return a referentially inconsistent view (a node whose\nsibling or edge endpoint did not exist yet) with no error.\n\nWhole-frame *submission* is not reader *isolation* during apply. Only\nthe first was implemented; this PR adds the second.\n\n## Approach\n\nDirection 2/3 from the issue, resolved toward a **stage-free atomic\npublish**: a per-`CurrentStorage` `ApplyGate`\n(`src/storage/current/apply_gate.rs`). The applier opens a publish\nwindow around each batch's replay, and gated reads resolve to the state\nbefore the window or after it, never inside it.\n\nDirection 1 (a plain `RwLock` on reads) was rejected: it would put a\ncontended atomic RMW on every database's read path, primary included,\nand serialize exactly the reader fan-out a read-scale-out replica exists\nto provide. Staging the current-state delta off to the side was rejected\nas impractical here — the shared replay engine reads live current state\nmid-walk (the #3419 idempotency guards), so there is no clean seam to\nstage behind.\n\n| | Behaviour |\n|---|---|\n| Primary (disarmed) | One acquire load of a never-written cache line +\na predicted branch. No lock is ever touched. |\n| Replica, no apply in flight | Seqlock: loads only, no atomic RMW, so\nthe sequence word stays Shared in every core's cache and reader fan-out\nstill scales. |\n| Replica, read collides with an apply | Retries, then parks on a\nblocking fallback the publisher holds for the same window — bounded by\none batch's replay. |\n| Publishing thread | Gate is re-entrant via a thread-local publish\ndepth, because the replay engine reads current state from inside its own\nwindow. |\n| After `promote_to_primary()` | Disarmed once the applier thread is\njoined. |\n\nArmed by `start_replication`/`bootstrap_replica`, before the applier\nthread is spawned (thread spawn is the synchronization edge).\n\n**Gated:** `get_node`, `get_edge`,\n`get_edge_target`/`_source`/`_endpoints`/`_label`, `get_node_label`,\n`contains_node`, `node_has_label`, `with_node`, the four adjacency\nlookups, `get_outgoing_targets`/`get_incoming_sources` (± label),\n`out_degree`, `in_degree`.\n\n**Deliberately not gated:** bulk scans and borrowing iterators.\n`DashMap` iteration was never a point-in-time snapshot even on a\nprimary, so gating it would be a promise the storage layer cannot keep —\nthe bi-temporal reads already provide one. This is stated in the guide\nand in the Limitations section rather than left implicit.\n\n## A test-assertion correction worth flagging\n\n`torn_frame_safety_never_exposes_a_partial_transaction` asserted\n`a_present == b_present`. That flags `(false, true)` — which the issue\nitself lists as admissible, since the apply can legitimately land\n*between* the two reads. My new sustained-load test hit that\nimmediately. The invariant is one-directional and both tests now encode\nit: `a` is written first, so **`a` visible with `b` missing** is the\ntorn shape; the reverse is ordinary staleness. Without this, the\nexisting test would still flake after the fix.\n\n## Verification\n\n- `torn_frame_safety_holds_under_sustained_concurrent_load` (new): 60\nmulti-op transactions streamed through a source capped at 2 entries per\nfetch, so the applier publishes near-continuously, while 4 reader\nthreads hammer the point-lookup surface. Also asserts the end state is\ncomplete, not merely untorn.\n- Gate unit tests (primitive + through `CurrentStorage` under a real\npublish window), including the publisher-reads-its-own-window\nre-entrancy case.\n- **Negative check:** the `CurrentStorage`-level concurrency test was\nrun with the gate deliberately unarmed and fails immediately, so it is a\ngenuine regression test rather than a tautology.\n- `cargo test --test replication_engine` — 10/10, and the two torn-frame\ntests run 5× in a row, 5/5 clean.\n- `cargo test --lib` — 4580 passed, 0 failed.\n- `cargo clippy --all-targets --all-features -- -D warnings` — clean.\n`cargo fmt --all` applied.\n\n## Acceptance criteria\n\n- [x] A read concurrent with an in-progress apply can never observe a\npartial transaction\n- [x] `torn_frame_safety_never_exposes_a_partial_transaction` passes\nunder sustained load — plus a new dedicated sustained-load test with\nconcurrent readers\n- [x] The \"never torn\" guarantee restored in `CLAUDE.md`, the\nreplication guide, and ADR-0059, each now describing the mechanism\nrather than only the frame-selection half\n- [x] Replica read throughput does not regress materially — primary path\nis unchanged; the replica path avoids atomic RMWs on reads by design\n\n**Not covered:** the issue's third bullet also asks for the 0.2.0\nchangelog entry to be updated. The #3787 interim-mitigation text (the\ncorrected 0.2.0 wording + Known issue) is not present in this branch's\n`CHANGELOG.md`, so there was nothing to revise — I added a `### Fixed`\nentry under `[Unreleased]` instead. If #3787 lands first, that\nKnown-issue paragraph still needs removing in a follow-up or a rebase\nhere.\n\n## Benchmark\n\n`benches/replication_apply_gate.rs` (new, registered in `Cargo.toml`)\ncompares `get_node` and single-hop traversal on a primary vs an\narmed-but-idle replica, single-threaded and at 2/4/8 concurrent reader\nthreads. The idle-replica numbers are the ones that matter, since a\nreplica spends the overwhelming majority of its time between batches. I\nhave not run it on this container — the numbers should come from a quiet\nmachine, so I did not want to publish noise from a shared runner as if\nit were a measurement.\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_018J7wNBZTGYZR7BUGSEPbYY)_\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-07-30T13:18:55-05:00",
          "tree_id": "f429cf4442c338ebd69a4c99ac4c78c05d2a289e",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/2904486b4dd0a40819567faa26bbf9e694444934"
        },
        "date": 1785436177416,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 383721.21803134325,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 110.17258891652084,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 136.09092743162736,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 129.92915739874874,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 147.0163110477128,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 18.63689787053067,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}