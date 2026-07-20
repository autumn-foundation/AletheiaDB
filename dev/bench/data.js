{
  "lastUpdate": 1784561119372,
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
          "id": "3ebb3cd82eaefbc7eecea771ba70f5e6d7a6ac7b",
          "message": "feat(semantic-reasoning): trust propagation over derivation lineage (#3382) (#3748)\n\nImplements #3382: a derived fact's confidence is computed from its\nupstream evidence through a declared, deterministic combination policy,\nkept distinct from the writer-declared value, and recomputed lazily on\nread so evidence changes flow downstream. Design:\n`docs/plans/2026-07-20-trust-propagation.md`; user guide:\n`docs/guides/trust-propagation.md`. Closes #3382.\n\n- Combinators: weakest-link (min) and noisy-OR (1−∏(1−c)); per-database\ndefault + per-label overrides, discoverable via `list_trust_policies`;\ndurable `trust_policy.json` sidecar (atomic write, corrupt-quarantine),\nin-memory for ephemeral DBs.\n- `ComputedConfidence { declared, computed, … }` — computation never\noverwrites `provenance.confidence()`.\n- Explainable `trust_breakdown` tree bounded by depth/size; node\nconfidence stays full-accuracy under truncation.\n- Leaf rules: retracted/absent contributes 0.0 and dominates; missing\nconfidence resolved by explicit policy (zero/neutral/ignore), always\nflagged, never silently defaulted; `ConfidenceSource::Absent` distinct\nfrom `Retracted`.\n- Bi-temporal: `computed_confidence_as_of` replays confidences as\nrecorded at T; reactive at now to superseding writes; recorded history\nnever mutated.\n- Cycle-safe: version memoization + DFS stack guard + hard depth cap.\n- AC7: `ComputedConfidenceFilter` predicate composing with the\ndeclared-confidence filter; fusion-signal glue behind both flags.\n- Gated under `semantic-reasoning` (zero overhead when off, guaranteed\nby construction).\n\n## Review fixes (this round)\n\n- **Retraction dominates under BOTH combinators (HIGH).** A\nretracted/absent child yields `0.0`; under noisy-OR `1−∏(1−c)` a `0.0`\nterm is the *identity* and was silently absorbed by a live sibling\n(`noisy_or{retracted, 0.9}` gave `0.9`). Fixed with an **explicit\nterminal-child check** in the node combine step (`db/trust.rs`): if any\ndirect child is `Retracted`/`Absent` the node short-circuits to `0.0`\nunder both combinators and flags `has_retracted_inputs`.\n`combine_values` stays a pure combinator; domination is local (the `0.0`\nflows to the parent as an ordinary value while the flag bubbles up). The\ndesign §2.6/§5 \"recursion stops so it is not absorbed\" justification was\nmathematically false and has been corrected.\n- **Valid-time-aware terminality (MED, PRIMARY path).**\n`classify_valid_interval` now keys terminality on wallclock now, not\nmerely `is_closed()`: empty interval → `Absent`, interval *ended as of\nnow* (`valid_to <= now`) → `Retracted`, an interval that currently\n*contains* now (incl. an effective-future retraction) → **live**.\nValid-time reference is `time::now()` for both `computed_confidence` and\n`computed_confidence_as_of` (the as-of scopes tx-time only) — a\ndocumented approximation, fully valid-time-scoped eval is a tracked\nfollow-up. Adversarial #7: a fact that predates the as-of `T` is\n`Absent` without flagging `has_retracted_inputs`.\n- **O(n) `trust_breakdown` (MED).** The breakdown previously called a\nfresh full-subtree `eval_scalar` at every node (O(n²)). It now computes\none shared `HashMap<VersionId, ScalarEval>` via a single root\n`eval_scalar` and reads each node's full-accuracy confidence from that\nmemo — preserving review-fix #1 (values stay full-accuracy under\ntruncation).\n- **Bench + guide.** Added `benches/trust_propagation.rs` (Criterion,\ngated `semantic-reasoning`, wired in `Cargo.toml [[bench]]` with\n`required-features`) quantifying the opt-in on-cost; added the user\nguide `docs/guides/trust-propagation.md`. Zero-overhead-when-disabled is\nby construction and verified via `cargo build --no-default-features`.\n- Plus polish: valid-time depth-backstop (>1024) truncation test, lazy\npolicy-change test, diamond-DAG comment fix (independence approximation,\nnot de-dup), serde-gated `Error` import (LOW-1), `max_nodes`\nroot-not-counted doc (adversarial #6), §2.9 head-as-of-`tt` wording fix\n(L1), snapshot-registry rollback note (LOW-2).\n\n## Tests / gates\n\n- **60 trust-propagation tests** under `--features semantic-reasoning`:\n27 in `trust_propagation.rs` (combinator math + registry\npersistence/quarantine/rollback) + 33 in `db/trust.rs` (3/5-level trees,\ndiamond DAG, per-label override, declared-vs-computed, missing-policy\nzero/neutral/ignore, retracted/absent domination incl. multi-source\nnoisy-OR, valid-time terminality (future-effective /\nbounded-containing-now / predates-T), deep-chain + 1024 backstop, lazy\npolicy change, bi-temporal AS-OF, explainable breakdown +\ntruncation-honesty, AC7 predicate/composition/fusion-signal). All green.\n- Full lib suite under `--features semantic-reasoning`: **4556 passed, 0\nfailed, 10 ignored**.\n- `cargo fmt --all` clean; `cargo clippy --all-targets --all-features --\n-D warnings` clean; `cargo clippy --no-default-features --features\nsemantic-reasoning -- -D warnings` clean; `cargo build\n--no-default-features` compiles (feature fully compiled out).\n\nMCP surface (`trust_breakdown` tool + predicate params) designed but\ndeferred to the registry batch, per the one-registry-PR rule. Note:\nCoverage/Mutants checks are red repo-wide until #3737 merges — unrelated\nto this PR.",
          "timestamp": "2026-07-20T10:14:00-05:00",
          "tree_id": "f37ad2997861195648175ef66f57dbc5afa08af0",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/3ebb3cd82eaefbc7eecea771ba70f5e6d7a6ac7b"
        },
        "date": 1784561119371,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 160.93475147555296,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 19.0682043163403,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 481379.6587968466,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 182.9722588767533,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 176.23171823255268,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 172.13342038485555,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}