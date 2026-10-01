# Knowledge Half-Life Analytics

> **Experimental feature.** Gated on the `semantic-temporal` cohort flag
> (`aletheiadb = { version = "0.2", features = ["semantic-temporal"] }`).
> APIs in this cohort are still evolving.

## What it is

Every knowledge base silently decays: a person's `email` changes every couple
of years, a company's `stock_price` changes hourly, a country's `capital`
almost never. **Knowledge half-life analytics** performs *survival analysis*
over AletheiaDB's full supersession history — per node label, per edge type,
per property — to answer *how long does a fact of this kind survive before it
is superseded?*

Three read-only products fall out of that one measurement:

- **Volatility statistics** (`knowledge_half_life`) — the median survival time
  (the "half-life"), a dispersion measure, and observation / event / censored
  counts for a cohort.
- **Freshness scores** (`fact_freshness`) — a single fact's age expressed in
  cohort half-lives plus an estimated survival probability ("recorded 3.2
  half-lives ago; treat as probably stale").
- **Staleness inventories** (`staleness_inventory`) — the operator's
  re-verification worklist: facts in a scope exceeding an age threshold,
  paginated.

It is a **pure read** over already-stored history: no writes, no storage-format
change. Its result is fully determined by `(cohort, options, as-of transaction
time)`.

## Why it exists

AletheiaDB records the raw material — every fact's full lifespan across valid
and transaction time — that other engines discard. Measuring decay turns that
history into an actionable staleness signal for retrieval ranking, cache
invalidation, and human re-verification worklists.

## The estimator

A fact still current at analysis time has *not yet* been superseded; treating
its current age as a completed lifespan would bias every half-life downward.
Half-life analytics uses the **Kaplan–Meier** non-parametric estimator, which
stays unbiased under such right-censoring. The half-life is the KM median: the
smallest `t` at which the survival curve `Ŝ(t) ≤ 0.5`.

One *observation* is one lifespan of a fact version. The terminating-event
oracle is reused verbatim from the [belief-revision audit](belief-revision.md):
a `WorldChange` or `Retraction` **terminates** a lifespan (a completed event);
a `Correction` or `Reaffirmation` does **not** (record churn, not
change-in-world); an open valid interval is a **right-censored** observation.

## Rust API

```rust
use aletheiadb::experimental::temporal::half_life::{
    Cohort, HalfLifeOptions, StalenessThreshold,
};
use aletheiadb::core::id::EntityId;

let options = HalfLifeOptions::new()
    .with_min_events(5);        // require at least N completed events

// Cohort volatility: how long does a Person.email survive?
let stats = db.knowledge_half_life(
    Cohort::node_property("Person", "email"),
    &options,
)?;
println!("half-life = {:?}", stats.half_life());

// One fact's freshness relative to its cohort
let freshness = db.fact_freshness(EntityId::Node(node_id), &options)?;

// Operator worklist: facts older than 3 half-lives
let page = db.staleness_inventory(
    Cohort::node_property("Person", "email"),
    StalenessThreshold::HalfLives(3.0),
    /* offset */ 0,
    /* limit */ 50,
    &options,
)?;
```

> The exact `Cohort` constructors and `VolatilityStats` accessors are the
> single source of truth in
> `src/experimental/temporal/half_life.rs` — verify field/method names against
> source before relying on them.

Errors follow the structured code contract: `INVALID_ARGUMENT` for a `limit` of
0 or a non-finite / negative `HalfLives` threshold.

## Semantics & caveats

- Cohort statistics are computed once per `(cohort, as-of)` and served from a
  warm cache (`CohortStatsCache`), so `knowledge_half_life` and `fact_freshness`
  share one Kaplan–Meier pass.
- `as_of_transaction_time` pins the analysis clock so ages and censored
  durations are reproducible; otherwise the live wall clock is used.
- Analysis scans hot-tier history; very large cohorts are bounded by
  `with_max_entities`.

## See also

- Source: `src/experimental/temporal/half_life.rs`
- Design plan: [../plans/2026-07-19-knowledge-half-life-analytics.md](../plans/2026-07-19-knowledge-half-life-analytics.md)
- [Belief-Revision Audit](belief-revision.md) — the shared event oracle
- [Temporal Drift Alarms demo](drift-alarms-demo.md)
