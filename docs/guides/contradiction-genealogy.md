# Contradiction Genealogy

> **Experimental feature.** Gated on the `semantic-temporal` cohort flag
> (`aletheiadb = { version = "0.2", features = ["semantic-temporal"] }`).
> APIs in this cohort are still evolving.

## What it is

When two facts disagree — *"Acme's CEO is Alice"* vs. *"Bob"* — a
**contradiction genealogy** reconstructs every competing claim's bi-temporal
life, attributes each to its sources, locates the **divergence point**, and
classifies **retroactive corrections** vs. **contemporaneous disagreement**. It
is read-only and deterministic over `(history, options)`.

## Why it exists

AletheiaDB uniquely holds what is needed to adjudicate a conflict: valid time
(when each claim was true), transaction time (when we learned it), provenance
(source + confidence), and graph structure. Contradiction genealogy turns
silent knowledge-base rot into an inspectable artifact.

## Bi-temporal ground truth

AletheiaDB linearizes versions on transaction time, but a superseded version's
valid interval is left **open and unsplit**. Updating `ceo=Alice valid[t0,∞)`
to `ceo=Bob valid[t1,∞)` yields two versions whose valid intervals **overlap**
on `[max(t0,t1),∞)` with differing values — the record literally asserts both,
because the Alice claim was never retracted. **That overlap is the structural
contradiction.** The escape hatch is valid-time retraction (closing the prior
claim's `valid_to`): a clean succession produces **no** contradiction. So the
feature flags value changes made *without* retracting the superseded claim.

A **contradiction** exists for an `(entity, property)` iff there are ≥ 2 claims
whose property key is the same, asserted values differ, and valid-time intervals
overlap.

## Classification

Each conflicting pair (earlier-recorded `A`, later-recorded `B`) is labeled:

- **`RetroactiveCorrection`** — `B.valid_from ≤ A.valid_from` (a later
  transaction reached back over a window `A` already covered).
- **`ContemporaneousDisagreement`** — otherwise (`B` extends forward, but
  because `A` was never retracted both remain asserted over the overlap).

Each claim's `origin` reuses the [belief-revision](belief-revision.md)
`RevisionClass` classifier, keeping the two features consistent.

## Rust API

```rust
use aletheiadb::experimental::temporal::contradiction_genealogy::{
    ContradictionTarget, GenealogyOptions, ContradictionScope,
};

// Genealogy for a specific entity+property conflict
let genealogy = db.contradiction_genealogy(
    ContradictionTarget::node_property(node_id, "ceo"),
    &GenealogyOptions::new(),
)?;

if genealogy.has_contradiction() {
    println!("divergence at {:?}", genealogy.divergence_point);
}

// Scan a scope for all contradictions
let scope = ContradictionScope::new()
    .with_label("Company")
    .with_property("ceo")
    .with_page(/* limit */ 100, /* offset */ 0);
let scan = db.find_contradictions(&scope)?;
```

> The exact `ContradictionTarget` / `ContradictionScope` constructors are the
> single source of truth in
> `src/experimental/temporal/contradiction_genealogy.rs` — verify names against
> source before relying on them.

Errors follow the structured code contract: `NOT_FOUND` for an unknown entity or
missing referenced version, `INVALID_ARGUMENT` for an empty claim set or an
unknown property.

## Semantics & caveats

- Detection is per `(entity, property)`; a retracted prior claim (closed
  `valid_to` not overlapping the successor) is **not** a contradiction.
- Value inequality uses `PropertyValue`'s `PartialEq` (inheriting the
  Float/Vector NaN caveat).
- Output is deterministic: claims sorted by `(transaction_from, version_id)`,
  sources by name.

## See also

- Source: `src/experimental/temporal/contradiction_genealogy.rs`
- Design plan: [../plans/2026-07-19-contradiction-genealogy.md](../plans/2026-07-19-contradiction-genealogy.md)
- [Belief-Revision Audit](belief-revision.md) — the shared claim-origin classifier
- [Provenance Hash Chain](provenance-hash-chain.md) — source attribution
