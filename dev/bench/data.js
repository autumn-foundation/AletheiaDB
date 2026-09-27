{
  "lastUpdate": 1790526503848,
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
          "id": "3e91eecdd93cbd700a97f30f2c56dcee11c80c0d",
          "message": "Bolt: add uniqueness-constraint write-path overhead workload (no fix) (#3849)\n\n## Summary\n\n- Adds `examples/bolt_constraint_overhead_workload.rs`, a deterministic,\npublic-API-driven benchmark for the write-path uniqueness-constraint\nreservation step\n(`src/api/transaction/write/constraint.rs::check_constraints` +\n`src/core/constraint::ConstraintRegistry::reserve_for_transaction`),\nwhich runs on every `WriteTransaction` commit for every `AletheiaDB`\n(the `constraint_registry` field is unconditionally constructed, never\n`Option`) regardless of whether `enable_unique_constraint` has ever been\ncalled.\n- Exercises `create_node`/`update_node`/`delete_node` -- the three\nbuffered-op kinds `check_constraints` walks -- through the public API,\nmatching the shapes the MCP `create_node`/`update_node`/`delete_node`\ntools issue, with zero unique or schema constraints declared (the\ncommon/default case: the feature is opt-in per CLAUDE.md).\n- **No `src/` changes.** This PR ships a benchmark harness only -- no\nperformance fix.\n\n## 🎯 Workload\n\n`examples/bolt_constraint_overhead_workload.rs`: create `BOLT_NODES`\n(default 6,000) 4-property nodes, then `BOLT_UPDATE_ITERS` (default\n6,000) full-property `update_node` revisions scattered across them, then\ndelete a `BOLT_DELETE_NODES` (default 2,000) prefix -- with **zero**\nunique or schema constraints declared. Reproduce:\n\n```\ncargo build --profile bench --example bolt_constraint_overhead_workload\nvalgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out -- \\\n    target/release/examples/bolt_constraint_overhead_workload\ncallgrind_annotate --threshold=100 /tmp/cg.out | grep -iE \"reserve_for_transaction|CurrentStorage.*get_node\"\n```\n\nNo existing benchmark covers this: `benches/uniqueness_constraints.rs`\ndoesn't exist (only `tests/uniqueness_constraints.rs`, a correctness\nsuite), and no `examples/bolt_*.rs` workload exercises\n`update_node`/`delete_node` at all (`bolt_workload.rs` is create-only).\n\n## 📈 Profile\n\nUnlike the already-existing `schema_constraints_empty()` guard used for\nthe Issue #3378 property-type/required-key check in the same function,\nthere is no analogous guard before the uniqueness-reservation step. For\n`CreateNode`/`UpdateNode`, `reserve_for_transaction` walks every\nbuffered property calling `ConstraintRegistry::is_constrained` (a\n`DashMap` lookup) even when `declarations` is empty; for\n`UpdateNode`/`DeleteNode`, `check_constraints` additionally calls\n`tx.current.get_node(id)` (an index lookup + `Node` clone) purely to\ngather old property values a removal step would need -- work that's\nprovably wasted whenever no unique constraint is declared for any label.\n\n`valgrind --tool=callgrind`, balanced config (6,000 nodes / 6,000\nupdates / 2,000 deletes): **514,517,105 Ir** total.\n`reserve_for_transaction` self-cost ≈ 13.2M Ir (≈2.6%);\n`CurrentStorage::get_node` self-cost ≈ 1.4M Ir (≈0.3%, shared with the\nunrelated PATCH-merge fetch `update_node` does at buffer time).\n\n## 💡 Hypothesis\n\nA `unique_constraints_empty()` guard (mirroring\n`schema_constraints_empty()`) that short-circuits `check_constraints`\nstraight to `reserve_for_transaction(&[], &[])` should eliminate\nessentially all of that ≈2.6-2.9% -- skipping both the per-property\n`is_constrained` calls and the `get_node` calls, with zero behavior\nchange (empty `declarations` means `is_constrained` always returns\n`false`, so the populated-path loops would produce an identical empty\n`ReservationGuard` anyway).\n\n## 🔧 Change (implemented, measured, then reverted -- see \"Why no fix\")\n\n```diff\n--- a/src/core/constraint.rs\n+++ b/src/core/constraint.rs\n@@ -412,6 +412,15 @@ impl ConstraintRegistry {\n         self.declarations.contains_key(&(label, property))\n     }\n \n+    /// Returns `true` if no uniqueness constraints are declared (used to skip\n+    /// the write-path reservation step -- including its per-op `get_node`\n+    /// lookups gathering stale values to free -- with zero overhead when the\n+    /// feature is unused; mirrors [`Self::schema_constraints_empty`]).\n+    #[inline]\n+    pub fn unique_constraints_empty(&self) -> bool {\n+        self.declarations.is_empty()\n+    }\n+\n     /// Record a constraint declaration (called by `enable()` and WAL replay).\n     pub fn declare(&self, label: InternedString, property: InternedString) {\n         self.declarations.insert((label, property), ());\n--- a/src/api/transaction/write/constraint.rs\n+++ b/src/api/transaction/write/constraint.rs\n@@ -59,6 +59,19 @@ pub(crate) fn check_constraints(\n         }\n     }\n \n+    // Uniqueness constraints (reservation step). Opt-in (`enable_unique_constraint`);\n+    // most databases never declare one, and `AletheiaDB` always attaches a\n+    // (possibly-empty) registry, so this runs on every commit unconditionally.\n+    // When no unique constraint is declared for any label, `is_constrained`\n+    // would return `false` for every property below, so skip straight to an\n+    // empty reservation -- in particular, skipping the `tx.current.get_node()`\n+    // call per Update/DeleteNode op below, which exists solely to gather old\n+    // property values for the removal step and is pure waste when there is\n+    // nothing to remove.\n+    if registry.unique_constraints_empty() {\n+        return registry.reserve_for_transaction(&[], &[]);\n+    }\n+\n     // Borrow property maps directly from the buffer to avoid cloning on the hot path.\n     let mut added_refs: Vec<(InternedString, &PropertyMap, NodeId)> = Vec::new();\n```\n\n## Why no fix\n\nMeasured against this harness under `valgrind --tool=callgrind`, on two\nconfigurations:\n\n| workload (nodes/updates/deletes) | before Ir | after Ir | Δ |\n|---|---:|---:|---:|\n| 6,000 / 6,000 / 2,000 (balanced) | 514,517,105 | 497,927,537 |\n**-3.22%** |\n| 2,000 / 40,000 / 2,000 (update-heavy, 20:1 update:create) |\n2,183,357,763 | 2,106,935,021 | **-3.50%** |\n\nSkewing the mix 20x further toward the op types that actually pay the\nwaste (updates/deletes) only moved the needle from -3.22% to -3.50% --\nboth well under this process's ≥5% instruction floor. `valgrind\n--tool=dhat` on the balanced config shows the same sub-floor story on\nthe allocation dimension:\n\n| | before | after | Δ |\n|---|---:|---:|---:|\n| Total bytes allocated | 68,473,498 | 64,832,946 | -5.32% |\n| Total blocks allocated | 708,378 | 680,376 | -3.95% |\n\nNeither clears the ≥10% allocation floor either -- expected, since\n`PropertyMap`'s `inner: Arc<HashMap<...>>` (`src/core/property/map.rs`)\nmakes a `Node`/`PropertyMap` clone an `Arc` bump, not a deep copy, so\nthe eliminated `get_node()` calls were mostly wasted *instructions*\n(DashMap lookup, refcount bump, struct copy), not wasted *allocations*.\nWAL append, historical/temporal-index versioning, and per-write hashing\n(visible in the profile as `crc32fast`/`siphash`/malloc-family\nself-cost) dominate the write-path cost regardless of how the\ncreate/update/delete ratio is skewed, so this constraint-check path\nstays a small, real, but sub-floor slice of it.\n\nPer process, the change was reverted -- this is a documented negative\nresult, not a shippable fix. The diff is functionally inert (verified\nwith `cargo test --test uniqueness_constraints` -- all 27 tests pass\nunchanged with it applied) and correct, just not worth the diff for the\nwin it buys today.\n\n## What this PR is for\n\nLanding the harness now means the profile, both before/after tables, and\nthe negative result above are reproducible by anyone who checks out this\nbranch, without redoing the setup -- and it's the natural target if\n`check_constraints` ever grows a costlier reservation step (e.g. richer\nconstraint types) where the same zero-constraints fast path would matter\nmore.\n\n## Test plan\n\n- [x] `cargo build --profile bench --example\nbolt_constraint_overhead_workload` succeeds\n- [x] Native run produces a deterministic `sink=8000 nodes=6000\nupdate_iters=6000 delete_nodes=2000` line, unchanged across the\n(reverted) fix attempt\n- [x] `cargo fmt --all -- --check` -- clean\n- [x] `cargo clippy --example bolt_constraint_overhead_workload\n--all-features -- -D warnings` -- clean, no warnings\n- [x] `cargo test --test uniqueness_constraints` -- 27 passed, 0 failed\n(run with the reverted diff applied, to verify it's behaviorally inert)\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_01PxJdVnJb8pJrBUCnseaNgZ\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_01PxJdVnJb8pJrBUCnseaNgZ)_\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-09-27T11:17:14-05:00",
          "tree_id": "c5c63c1aff96f208ba80f2def571fcbf7a4a7191",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/3e91eecdd93cbd700a97f30f2c56dcee11c80c0d"
        },
        "date": 1790526503847,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 21.188020371479492,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 173.31583731227786,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 176.34273992100762,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 180.0348949778432,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 172.4498876438854,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 389409.38874521875,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}