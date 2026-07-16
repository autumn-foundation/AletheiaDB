# Correlated Point-in-Time Analysis: Temporal Joins (Issue #3379)

AletheiaDB can answer *"what did entity X look like AS OF T?"* for a single `T`
(`AS OF`, `#3225`, `#3236`). The questions analysts actually pay a temporal
database for are **correlative**:

- *"What was each customer's tier **at the moment** each of their orders was placed?"*
- *"Who was the account manager **when** the complaint was filed?"*
- *"Compare a product's price and its supplier's rating **at the same instants** across last year."*

Answering these without a temporal join forces the caller — human or LLM — into
a loop: enumerate one entity's version timeline, issue one `AS OF` lookup per
version for the other entity, then zip the results in application code with
hand-rolled interval-boundary logic. That is `O(versions)` round-trips, trivially
wrong at interval boundaries, and hopeless over MCP where each round-trip costs
tokens.

A **temporal join** (`ALIGN`) turns that loop into one declarative AQL statement
with correct-by-default half-open boundary semantics and bi-temporal pinning.

## Two modes

### Event-aligned (`ALIGN EVENTS`) — the SQL `ASOF JOIN` analog

For each **version-change instant** of a *driver* entity within the range,
evaluate every participant at *its most recent state at or before that instant*.
One output row per driver event.

```aql
MATCH (o:Purchase)-[:PLACED_BY]->(c:Customer)
ALIGN EVENTS DRIVER o
  OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2025-01-01T00:00:00Z'
RETURN o.total, c.tier AS tier
```

Each output row carries `align_valid_time` (RFC 3339) plus the `RETURN` columns:

| align_valid_time            | o.total | tier |
|-----------------------------|---------|------|
| 2024-02-01T00:00:00.000000Z | 10      | 1    |
| 2024-09-01T00:00:00.000000Z | 20      | 2    |

*(The Feb order sees the customer's tier-1 state; the Sep order sees the tier-2
state after a June upgrade — the tier is aligned to each order's placement
instant.)*

### Interval-overlap (`ALIGN OVERLAP`)

Return the piecewise maximal sub-intervals over the range during which **all**
participant states (and any gating edge) co-held — the intersection of the
participating valid intervals. One row per non-empty sub-interval.

```aql
MATCH (p:Product)-[:SUPPLIED_BY]->(s:Supplier)
ALIGN OVERLAP
  OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-06-01T00:00:00Z'
RETURN p.price AS price, s.rating AS rating
```

Each output row carries `overlap_from` / `overlap_to` (RFC 3339) plus the
`RETURN` columns. Sub-intervals where any participant is absent (a gap) are
dropped — never a row pairing states that did not co-hold.

| overlap_from | overlap_to | price | rating |
|--------------|------------|-------|--------|
| 2024-02-01…  | 2024-04-01…| 100   | 5      |
| 2024-04-01…  | 2024-05-01…| 150   | 5      |
| 2024-05-01…  | 2024-06-01…| 150   | 4      |

*(January is dropped because the supplier record did not yet exist — an empty
overlap.)*

## The client-side loop it replaces

The event-aligned example above, done by hand, is roughly:

```text
versions = get_node_history(order_id)                 # 1 call
for v in versions:                                     # ~N calls
    if v.valid_from in [from, to):
        cust = get_node_at_time(customer_id,
                                valid_time=v.valid_from,
                                transaction_time=now)   # 1 call each
        emit(v.valid_from, v.total, cust.tier)
        # ...and you still hand-roll: which timestamp wins at a boundary?
        # open or closed ends? what if the customer didn't exist yet?
```

For 1 driver with 100 versions and 1 joined entity that is **~101 calls**; the
`ALIGN` form is **one** query — a ≥ 99% round-trip reduction, and over MCP the
difference between one `< 30ms` response and 100+ sequential calls.

## Boundary and dimension semantics (the falsifiable contract)

- **Half-open `[from, to)`**, consistent with the storage model and the `#3363`
  temporal-window convention. A version whose `valid_from` **equals** an
  alignment instant *starts* that instant and is **included** there; a version at
  exactly `to` is **excluded**.
- **Transaction-time pinning.** History is read as believed at the transaction
  time; it defaults to *now* and is independently pinnable with
  `AS OF SYSTEM_TIME <ts>` on the clause. A correction recorded **after** the pin
  never rewrites the pinned analytics (later corrections do not change the past).
  *v1 pinning scope:* the pin is exact for **same-`valid_from` corrections** — a
  restatement of a fact's value at an already-recorded `valid_from`, recorded
  after the pin, is correctly excluded. It is **not** a full as-of-transaction-time
  snapshot across the whole valid axis: a participant's believed timeline is
  reconstructed from every version with `transaction_from ≤ pin` (mirroring the
  `#3363` window reconstruction), so a distinct-`valid_from` fact whose
  transaction interval was later closed is still included. Full
  transaction-interval-`contains` scoping is a tracked follow-up (it is
  deliberately *not* used here because, under the append-only supersession model
  where a superseded version keeps its valid interval open, it would drop
  genuinely-held earlier history).
- **Edge validity is honored at each instant.** When a participant is bound via a
  relationship, that edge's validity gates the pairing: at an instant where the
  edge is not valid, the participant is treated as absent (its columns are `null`
  in event mode; the sub-interval is dropped in overlap mode).
- **Valid-time closure is honored.** A participant whose covering version's valid
  interval has **closed** — a retraction (`#3230`), a delete, or a valid-time gap
  before a later version — is treated as **absent** from the close instant onward,
  symmetric with the edge-gate path: its columns are `null` in event mode and the
  sub-interval ends (or is dropped) in overlap mode. A retracted or deleted
  participant is *not* presumed present forever.
- **Absent participants.** A participant with no state at/before the alignment
  instant (e.g. an order placed before the customer existed) yields `null`
  columns in event mode and drops the sub-interval in overlap mode.
- **Event-aligned driver.** The driver is **always present at its own event** by
  construction — it is ungated even when it is the far node of a traversal
  (`ALIGN EVENTS DRIVER <far-node>`), so a driver event never emits a row with
  the driver's own column `null`.

## Composition and access

- **Graph patterns.** `ALIGN` composes with a `MATCH` traversal — the joined
  entities may be bound via an edge (`order → customer`), not only by explicit
  ids. The pattern binding resolves at current state (or the outer `AS OF`), then
  the alignment reconstructs each bound entity's history over the range.
- **MCP `query` tool.** Available read-only through the MCP `query` tool (AQL
  path); results ride the existing row/limit contracts, and over-large ranges
  respect the standard result caps.

## v1 limitations

- **Supported pattern shapes:** a single bound node (`MATCH (v:Label)`) or a
  single-hop traversal (`MATCH (a)-[:R]->(b)`, both directions). Multi-hop,
  variable-length, and comma-separated patterns return a structured
  `UnsupportedFeature` error. Every node in the pattern must be named.
- **Fully retracted / deleted edges (and nodes) cannot be re-bound by a
  `MATCH`.** Like `#3225` AS OF traversal, candidate entities are enumerated from
  the *current* adjacency / label index, so an edge or node whose validity is
  entirely in the past (retracted / deleted) is not found by the pattern — anchor
  the binding with an outer `AS OF` at a point where it still held (both
  dimensions, per `#3225`). This is a **candidate-discovery** limitation only:
  once an entity *is* bound, the alignment fully honors **closed valid intervals**
  on both the participant node timeline (a mid-range retraction / delete / gap
  makes it absent from the close instant) and the gating edge — so a participant
  that is retracted *within* the range while still bindable is correctly nulled
  (event mode) or ends its co-hold sub-interval (overlap mode) at the retraction
  instant.
- **Range cap.** A range that would generate more than `MAX_ALIGN_INSTANTS`
  (100,000) alignment instants / boundaries is rejected with a structured
  `InvalidParameter` error.

## Out of scope (tracked elsewhere)

- SQL:2011 surface for the same semantics — `#311` owns SQL syntax.
- Temporal aggregation (windowed `COUNT`/`AVG`) over the aligned output — `#3363`.
- Alignment across more than one time dimension simultaneously beyond pinning.
- Allen's-interval-algebra predicate library (`MEETS`, `DURING`, …).
- Streaming / continuous evaluation of temporal joins — `#3375`.
