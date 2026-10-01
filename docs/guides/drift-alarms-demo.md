# Temporal Drift Alarms — 12-Week Demo (Issue #3367)

> **Feature cohort:** `semantic-temporal` (experimental; ADR-0050). Everything
> below compiles and runs only with `--features semantic-temporal`.

This is a narrated walkthrough of a concept whose meaning is rewritten week by
week for twelve simulated weeks. Two drift monitors watch it: one at the level of
the concept itself, one at the level of the concept's cohort. Over the twelve
weeks the cohort-level alarm fires **once** (an early warning), and the
concept-level alarm fires **once** (three weeks later, when the concept itself has
moved far enough). No other alarms fire.

Every number in this guide is produced and asserted by a deterministic backing
test, so the demo is verified rather than merely described:

> **Backing test:** `twelve_week_demo_fires_one_entity_and_one_label_alarm`
> in `src/experimental/temporal/drift_alarm.rs` (run with
> `cargo test --features semantic-temporal --lib
> experimental::temporal::drift_alarm::tests::twelve_week_demo`). It injects a
> `SimulatedClock`, advances transaction time one week per step, and asserts the
> crossing weeks, the measured distances, and that exactly one alarm of each kind
> is materialized.

## How a drift alarm decides to fire

A monitor compares an embedding **now** against the embedding **on record a
`window` ago** and fires (strictly) when the distance exceeds the threshold and
no unresolved alarm for that `(monitor, target)` is already outstanding. The past
endpoint is reconstructed on the **transaction-time** axis — `embedding(E)` as of
`tx = now − window` — because the engine's supersession model closes a version's
transaction interval while leaving its valid interval open, so a superseded
embedding is only recoverable along transaction time (see the `evaluate_monitor`
rustdoc for the full rationale). Because an unresolved alarm suppresses re-firing
until it is resolved, and this demo never resolves, each monitor fires **at most
once**.

## The setup

- **`concept`** — a `Concept` node carrying a 2-D unit-vector `embedding`
  property. (Two dimensions keep the arithmetic hand-checkable; the mechanism is
  identical at 384 or 1536 dimensions.)
- **Three sibling `Concept` nodes** — the rest of the cohort. They drift *mildly*
  in the same direction as `concept` (each week their angle advances only 30% as
  far as the concept's), so the cohort as a whole shifts, but far less than the
  concept does. This is a genuine population shift, not a static backdrop.

Two monitors, both `Cosine` distance with a **4-week trailing window**:

| Monitor | Target | Watches | Threshold | Window |
|---|---|---|---|---|
| Entity | `PerEntity` | `concept` **only** (`entities = [concept]`) | **0.40** | 4 weeks |
| Cohort | `LabelCentroid` | label `Concept` (all four nodes) | **0.03** | 4 weeks |

The entity monitor is scoped to `concept` alone, so a sibling's drift can never
raise an entity alarm. The cohort monitor's threshold is deliberately **smaller**
— it is the early-warning tripwire on the population's mean meaning.

Because the window is four weeks, the first three weekly evaluations have no past
endpoint yet (`now − 4 weeks` predates the concept's creation), so no alarm is
even possible until week 4.

## The twelve weekly rewrites

Each week the concept (and, more gently, its three siblings) is rewritten to a new
unit vector — a rotation, standing in for an embedding whose meaning is sliding.
The concept's angle accelerates: small early edits (synonyms, phrasing) give way
to larger topical shifts.

| Week | Concept angle | Entity 4-week drift `d(vₖ, vₖ₋₄)` | Cohort centroid 4-week drift |
|---:|---:|---:|---:|
| 0 | 0° | (created) | (created) |
| 1 | 4° | — (no past endpoint) | — |
| 2 | 9° | — | — |
| 3 | 15° | — | — |
| 4 | 22° | 0.0728 | 0.0165 |
| 5 | 31° | 0.1090 | 0.0248 |
| **6** | 42° | 0.1613 | **0.0366 → crosses 0.03** |
| 7 | 55° | 0.2340 | 0.0529 (suppressed) |
| 8 | 70° | 0.3309 | 0.0743 (suppressed) |
| **9** | 87° | **0.4408 → crosses 0.40** | 0.0974 (suppressed) |
| 10 | 106° | 0.5616 (suppressed) | 0.1200 (suppressed) |
| 11 | 127° | 0.6910 (suppressed) | 0.1391 (suppressed) |

(The distances are the literal `metric_distance` outputs the backing test
asserts, to within f32 tolerance.)

## The documented crossing points

**Cohort centroid — crosses at week 6.** The centroid is the deterministic,
component-wise arithmetic mean of the four members' current embeddings (node-id
sorted; not renormalized for cosine — the documented firing-rule contract). Its
4-week drift is **0.0248 at week 5** (below the 0.03 threshold) and **0.0366 at
week 6** (above it). Week 6 is therefore the first crossing → **one label-level
alarm** fires, naming `label = "Concept"` and no single entity. From week 7 on the
centroid drift keeps growing (0.053, 0.074, …) but the outstanding, unresolved
alarm suppresses any re-fire.

**Concept entity — crosses at week 9.** The concept's own 4-week drift is **0.3309
at week 8** (below the 0.40 threshold) and **0.4408 at week 9** (above it). Week 9
is the first crossing → **one entity-level alarm** fires, naming
`entity = concept`, with both `from_version` (the week-5 embedding on record as of
`now − 4 weeks`) and `to_version` (the current week-9 embedding). Weeks 10–11 are
suppressed.

The cohort alarm thus precedes the concept alarm by three weeks: the population's
mean meaning tripped the sensitive tripwire well before the concept itself moved
far enough to trip its own, coarser one.

## The resulting alarms

Two durable `__drift_alarm` nodes exist at the end of the run — no more, no fewer:

**Label-level alarm (fired week 6)**

| Field | Value |
|---|---|
| `monitor_id` | the cohort monitor |
| `entity` | *(none — a centroid alarm names no single entity)* |
| `label` | `"Concept"` |
| `metric` | `Cosine` |
| `threshold` | `0.03` |
| `measured_distance` | ≈ `0.0366` (> threshold) |
| `compared_now` / `compared_past` | the week-6 evaluation instant / its `now − 4 weeks` anchor |
| `resolved` | `false` |

**Entity-level alarm (fired week 9)**

| Field | Value |
|---|---|
| `monitor_id` | the entity monitor |
| `entity` | `concept` |
| `label` | *(none)* |
| `metric` | `Cosine` |
| `threshold` | `0.40` |
| `measured_distance` | ≈ `0.4408` (> threshold) |
| `from_version` / `to_version` | the week-5 (past) and week-9 (current) embedding versions |
| `compared_now` / `compared_past` | the week-9 evaluation instant / its `now − 4 weeks` anchor |
| `resolved` | `false` |

Both are ordinary bi-temporal graph nodes, so they are queryable via
`query_drift_alarms` (filter by monitor / label / resolved / time range),
`AS OF`-stable, resolvable as a recorded update (never a delete), and delivered
through the existing changefeed. The backing test asserts exactly one alarm per
monitor, the crossing weeks (`[6]` and `[9]`), the measured distances, and the
version references.

## Reproduce it

```bash
cargo test --features semantic-temporal --lib \
  experimental::temporal::drift_alarm::tests::twelve_week_demo_fires_one_entity_and_one_label_alarm
```

Keep this guide in lock-step with the constants in that test — the vectors,
thresholds (`0.40` / `0.03`), window (4 weeks), and crossing weeks (6 and 9) are a
single source of truth shared between the two.
