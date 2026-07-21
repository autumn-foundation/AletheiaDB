# Counterfactual Exclusion Replay

> **Experimental feature.** Gated on the `semantic-temporal` cohort flag
> (`aletheiadb = { version = "0.2", features = ["semantic-temporal"] }`).
> APIs in this cohort are still evolving. The MCP surface is a deferred
> follow-up — this is a **Rust-API** feature today.

## What it is

A **counterfactual view** materializes a read-only shadow of the database as it
would exist *had a named source's writes never been recorded*. Recorded history
is replayed in transaction-time order with writes matching an **exclusion
predicate** over provenance omitted, and the survivors are restored into a
fresh, physically separate shadow storage. The real database is never mutated,
and the view is fully queryable through the existing read surfaces — including
`AS OF` and history reads, with bi-temporal coordinates preserved.

## Why it exists

It answers questions no incumbent can even express:

- *"How much did the poisoned feed contaminate?"*
- *"Does removing this low-confidence scraper change any answers?"*
- *"What does this expensive feed actually contribute?"*

Each is one materialization plus one divergence-report read.

## Rust API

```rust
use aletheiadb::experimental::temporal::counterfactual::{
    CounterfactualConfig, ExclusionPredicate,
};

// Build the "world without source X" and inspect what changed
let view = db.counterfactual_replay(
    "no-scrapers",
    ExclusionPredicate::source("scraper-v1"),
    CounterfactualConfig::default(),
)?;

let report = view.report();
println!(
    "excluded {} writes; {} entities changed, {} removed",
    report.excluded_writes(),
    report.entities_changed(),
    report.entities_removed(),
);

// Query the shadow through the normal read surfaces
let node = view.get_node(node_id)?;                 // current-state read
let past = view.get_node_at_time(node_id, vt, tt)?; // bi-temporal AS OF read
assert!(view.is_counterfactual());
```

`ExclusionPredicate` can target a single `source`, a set of `sources`, or be
narrowed to a transaction-time window (`within_transaction_time`). Errors are a
dedicated `CounterfactualError` (e.g. `HistoryTooLarge` when the replay exceeds
`config.max_replay_versions`).

## Semantics & caveats

- **Exclusion replay:** survivors keep their exact `(valid, transaction)`
  intervals. A later write by a surviving source that targets an entity with no
  surviving prior version is *unappliable* — it is dropped and counted as an
  **orphaned update**, not promoted to a create (promoting it would
  re-introduce the excluded source's carried-forward properties).
- **Unattributed writes are never excluded:** a write recorded without
  provenance never matches a source predicate.
- **Labeling:** reads are reachable *only* through a `CounterfactualView`, never
  a real-DB handle — a type-level guarantee. The per-response-envelope
  `counterfactual: true` marker lands with the deferred MCP surface.
- **Scope:** materialization sees **hot-tier** history only (cold-migrated
  versions are omitted from the view, report, and version cap); the cap counts
  *versions*, not bytes.

## See also

- Source: `src/experimental/temporal/counterfactual.rs`
- Design plan (full AC contract, reconstruction mechanism, caveats):
  [../plans/2026-07-19-counterfactual-replay.md](../plans/2026-07-19-counterfactual-replay.md)
- ADR: [../adr/0038-counterfactual-graph-analysis.md](../adr/0038-counterfactual-graph-analysis.md)
- [Derivation Lineage](derivation-lineage.md) and [Trust Propagation](trust-propagation.md) — provenance-driven analysis
