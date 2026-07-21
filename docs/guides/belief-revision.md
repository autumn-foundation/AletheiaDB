# Belief-Revision Audit

> **Experimental feature.** Gated on the `semantic-temporal` cohort flag
> (`aletheiadb = { version = "0.2", features = ["semantic-temporal"] }`).
> APIs in this cohort are still evolving.

## What it is

A **belief-revision audit** walks a single node's or edge's already-stored
bi-temporal version history and classifies *each transition*, so a caller — or
an LLM — can answer *"why does the database now say Y when it used to say X,
and who says so?"* in one call, instead of stitching together
`get_node_history` and per-version provenance lookups by hand.

It is a **pure read**: no writes, no storage-format change. The result is fully
determined by `(entity, options, as-of transaction time)` — running the same
audit twice at the same coordinate returns byte-identical output.

## Why it exists

An append-only bi-temporal store already records everything needed to explain a
change of mind — valid time, transaction time, and provenance — but that signal
is scattered across the version chain. This audit turns it into a single,
falsifiable classification so downstream reasoning (or a human auditor) can
distinguish *"the world changed"* from *"we corrected our record."*

## Classification

Each revision is classified purely from bi-temporal interval geometry and the
version's provenance (never NLP over free text). For versions ordered
oldest-first:

| Class | Rule |
|-------|------|
| `InitialAssertion` | the first visible version |
| `Retraction` | the version's valid interval is **closed** (`valid_to != ∞`) — a delete tombstone or a valid-time retraction |
| `Reaffirmation` | value unchanged vs. predecessor **and** provenance `source` is present and differs |
| `WorldChange` | value changed and `valid_from` advanced beyond every prior `valid_from` — the fact itself changed |
| `Correction` | value changed but `valid_from` did **not** advance — a later transaction-time rewrite or backfill of a same-or-earlier valid period |

Precedence is strict and evaluated top to bottom. A backfill of a previously
unrecorded earlier interval is classified `Correction` (a deliberate reading of
the acceptance criteria — there is no sixth `backfill` class).

## Rust API

```rust
use aletheiadb::experimental::temporal::belief_revision::RevisionOptions;
use aletheiadb::core::id::EntityId;

let options = RevisionOptions::new()
    .with_limit(100);                 // optional: cap revisions returned
    // .with_property_key("email")    // optional: scope to one property
    // .with_as_of_transaction_time(ts)

let log = db.belief_revisions(EntityId::Node(node_id), &options)?;

for rev in log.revisions() {
    println!("{:?}: {:?}", rev.class, rev.changes);
}

// Per-revision confidence, aligned with `revisions()`
let trajectory: Vec<Option<f64>> = log.confidence_trajectory();
```

Errors follow the structured code contract: `NOT_FOUND` for an unknown entity,
`INVALID_ARGUMENT` for a `limit` of 0 or an unknown `property_key`.

## MCP tool surface

The `get_belief_revisions` tool exposes the same audit over MCP (requires the
`semantic-temporal` feature; returns `FAILED_PRECONDITION` with a
`required_feature` when the cohort is not compiled in). It returns the
classified revision sequence plus the confidence trajectory. See the MCP tool
reference in [mcp-query-tool.md](mcp-query-tool.md).

## Semantics & caveats

- A property-scoped audit reports each surviving revision with the
  **entity-level** classification, not a per-property re-classification.
- `as_of_transaction_time` filtering relies on versions being appended in
  transaction-time-monotonic order.
- A valid-time retraction renders its `changes` as the predecessor's properties
  "removed", meaning *no longer asserted after `valid_to`* — the stored values
  are **not** erased and remain readable `AS OF` a time before `valid_to`.

## See also

- Source: `src/experimental/temporal/belief_revision.rs`
- Design plan: [../plans/2026-07-18-3362-belief-revision-audit.md](../plans/2026-07-18-3362-belief-revision-audit.md)
- [Knowledge Half-Life](knowledge-half-life.md) reuses this classifier as its event oracle
- [Contradiction Genealogy](contradiction-genealogy.md)
