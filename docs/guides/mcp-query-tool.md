# MCP `query` Tool — Read-Only Cypher/AQL over MCP

AletheiaDB's primary client is the LLM driving it through the [MCP
server](../../src/mcp/server.rs). The `query` tool lets that LLM answer a
multi-hop, filtered, temporally-scoped question with **one declarative
statement** instead of orchestrating and stitching together several structured
tool calls (`get_node` → `traverse` → fetch each → filter client-side).

It wraps the engine's existing public APIs — `execute_aql`,
`execute_cypher`, and `execute_cypher_with_params` (see
[`src/db/query.rs`](../../src/db/query.rs)) — and is strictly **read-only**.

## When to use it

Reach for `query` when the question is a join/filter/temporal-scope expressible
as one Cypher/AQL statement. Keep using the structured tools (`create_node`,
`update_edge`, …) for writes and for simple single-entity lookups.

## Tool contract

| Field | Type | Notes |
|-------|------|-------|
| `language` | string | `"cypher"` or `"aql"`. AQL is always available; Cypher requires the `cypher` build feature. |
| `query` | string | The read-only statement. |
| `params` | object (optional) | `$param` bindings — **Cypher only**. Numbers, strings, booleans, and null map directly; a numeric array is treated as an **embedding**. |
| `limit` | number (optional) | Max rows. Default `100`, capped at `10000`. |

### Success response

```json
{
  "language": "cypher",
  "columns": [
    {"name": "entity", "type": "node|edge", "description": "..."},
    {"name": "score", "type": "number|null", "description": "..."},
    {"name": "path", "type": "array<{type:string,id:number}>|null", "description": "..."},
    {"name": "timestamp", "type": "string|null", "description": "..."}
  ],
  "rows": [
    {
      "entity": {"type": "node", "id": 7, "label": "Product",
                 "properties": {"name": "Widget", "price": 42}},
      "score": null,
      "path": null,
      "timestamp": null
    }
  ],
  "row_count": 1,
  "truncated": false
}
```

`truncated` is `true` when the result hit the `limit` cap (there were more rows).

### Structured errors

Errors are returned as a machine-readable payload so the LLM can self-correct:

```json
{ "error": { "kind": "read_only_violation", "clause": "CREATE",
             "language": "cypher",
             "code": "INVALID_ARGUMENT", "retriable": false,
             "message": "The `query` tool is read-only; the `CREATE` clause ..." } }
```

`kind` is one of: `invalid_request`, `read_only_violation`,
`language_unavailable`, `parse_error`, `unsupported_construct`,
`invalid_params`, `runtime_error`.

`kind` is the query tool's own execution-layer contract (Issue #3213) and is
preserved verbatim. The uniform `code` / `retriable` fields shared by **every**
MCP tool (Issue #3234, see
[Structured error codes](#structured-error-codes-and-the-retriable-contract))
are carried additively alongside it: `invalid_request`, `read_only_violation`,
`parse_error`, `unsupported_construct`, and `invalid_params` map to
`INVALID_ARGUMENT`; `language_unavailable` maps to `FAILED_PRECONDITION`; and
`runtime_error` is classified from the underlying engine error, so **any**
code from the enum is possible — e.g. `INTERNAL` for an unexpected execution
failure, `UNAVAILABLE`/retriable for a timeout, or `NOT_FOUND` if an entity
vanishes mid-execution.

## Supported read-only subset

The tool exposes whatever the shipped grammars already support — it does **not**
extend them. Advertised, supported constructs:

- `MATCH` patterns with node/edge labels and inline property filters
- Variable-depth traversal `-[:REL*1..3]->` and directions `->`, `<-`, `-`
- `WHERE`, `RETURN [DISTINCT]` / `AS` aliases, `ORDER BY`, `SKIP`/`LIMIT`,
  `WITH` chaining
- Vector similarity ranking
- Bi-temporal scoping: `AS OF TIMESTAMP/VALID_TIME/SYSTEM_TIME`,
  `FOR SYSTEM_TIME AS OF '…'`, `BETWEEN '…' AND '…'`

Mutating clauses (`CREATE`, `MERGE`, `SET`, `DELETE`, `REMOVE`, `DETACH`,
`DROP`, `CALL`, `FOREACH`, `LOAD`) are rejected **before execution**.

## End-to-end example (LLM-style)

**Question:** *"As of June 2026, which Products does Alice's KNOWS-network view
that cost under $100?"*

Previously this required ≥4 structured calls (resolve Alice → traverse `KNOWS`
→ traverse `VIEWED` → fetch each Product → filter by price). With `query` it is
**one** tool call:

```jsonc
// tools/call -> "query"
{
  "language": "cypher",
  "query": "MATCH (a:Person {name: $name})-[:KNOWS]->(:Person)-[:VIEWED]->(p:Product) FOR SYSTEM_TIME AS OF '2026-06-01T00:00:00Z' WHERE p.price < 100 RETURN p ORDER BY p.price",
  "params": { "name": "Alice" },
  "limit": 20
}
```

The response is structured rows of `Product` nodes, already filtered and
temporally scoped by the engine's planner.

### Passing an embedding as a parameter

```jsonc
{
  "language": "cypher",
  "query": "MATCH (d:Document) RETURN d ORDER BY vector.similarity(d.embedding, $q) DESC LIMIT 5",
  "params": { "q": [0.12, 0.04, 0.98, 0.33] }
}
```

A numeric array parameter is bound as an embedding, so the LLM never has to
string-concatenate vectors into the query text.

## Recording facts at a specific valid time

`query` is read-only, but the structured write tools it complements
(`create_node`, `create_edge`, `update_node`, `update_edge`, `delete_node`,
`delete_edge`) accept an optional `valid_time` field so an LLM ingesting a
document can record the fact's real-world effective date — including when it
stopped being true — in the **same** tool call, instead of defaulting to
"now". This closes the loop with the bi-temporal `AS OF` queries above:
without it, every LLM-ingested fact would collapse to "valid as of now" and
`AS OF VALID_TIME` reads over that data would be wrong.

On `delete_node`, `valid_time` is not supported together with `detach: true`
(cascade delete does not support backdating): pass one or the other, or
delete the connected edges individually with `valid_time` first.

**Example:** an LLM reads a filing and extracts "Alice became CEO on
2021-03-01," today.

```jsonc
// tools/call -> "create_node"
{
  "label": "Person",
  "properties": { "name": "Alice", "title": "CEO" },
  "valid_time": "2021-03-01T00:00:00Z"
}
```

Verifying the backdated fact is retrievable at its stated valid time, and
absent before it:

```jsonc
// tools/call -> "get_node_at_time"
{ "node_id": 7, "valid_time": "2021-03-01T00:00:00Z" }
// -> { "node": { "id": 7, "label": "Person", "properties": {...} }, ... }

// tools/call -> "get_node_at_time"
{ "node_id": 7, "valid_time": "2021-01-01T00:00:00Z" }
// -> { "error": { "code": "NOT_FOUND", "retriable": false, "message": "..." } }
//    (not yet true at this valid time)
```

`valid_time` accepts the same formats as every other MCP temporal field
(ISO 8601 / RFC 3339, or integer microseconds since epoch). Omitting it
reproduces today's behavior exactly (valid time defaults to the transaction
time). **Transaction time is always system-assigned** — there is no
`transaction_time` field on the write tools, so provenance (when the DB
actually learned the fact) can never be forged. A malformed, more-than-a-year-
in-the-future, or before-entity-creation `valid_time` is rejected with a
structured error, e.g.:

```jsonc
{ "error": { "code": "INVALID_ARGUMENT", "retriable": false,
             "message": "Invalid valid_time: Invalid timestamp format: 'not-a-timestamp'. ..." } }
{ "error": { "code": "INVALID_ARGUMENT", "retriable": false,
             "message": "valid_from ... is too far in future (current: ..., max offset: ...)" } }
{ "error": { "code": "INVALID_ARGUMENT", "retriable": false,
             "message": "valid_from ... is before entity creation time ... for node:7" } }
```

## Retracting a fact (closing valid time)

`retract_node` and `retract_edge` (Issue #3230) are the symmetric counterpart
to the valid-time **writes** above: those open a fact's valid interval,
retraction **closes** it. Retracting an entity at valid time `T` records
"this fact stopped being true in the real world at `T`" **without deleting
its history** — the correct way to model churn, an employment ending, a
contract expiring, or a sensor being decommissioned. Compare `delete_node`,
which is the right tool when the entity should end *now* and disappear as a
current fact; retraction is the right tool when you need `AS OF VALID_TIME`
reads before `T` to keep working.

The bi-temporal contract:

- **Valid time:** `AS OF VALID_TIME` queries strictly before `T` still return
  the entity; queries at or after `T` do not (the closed interval is
  half-open: `[valid_from, T)`). One qualifier for entities that were
  **updated** before being retracted: with a current `AS OF SYSTEM_TIME`,
  this visibility extends back to the *current head version's* `valid_from`
  (the last update's effective date), not all the way to the entity's
  original creation — earlier valid times were already superseded by the
  update, and recalling them requires anchoring `AS OF SYSTEM_TIME` before
  that update (pre-existing supersession semantics, unrelated to
  retraction).
- **Transaction time:** retraction is append-only. The pre-retraction record
  keeps its open-ended valid interval with its transaction time closed at the
  retraction's commit, so `AS OF SYSTEM_TIME` queries positioned *before* the
  retraction still show the fact as currently valid — the past record is
  never rewritten.
- **Current state:** like a delete, a retracted entity is absent from
  current-state tools (`get_node`, `list_nodes`, `traverse`, ...), while its
  full history remains queryable via `get_node_history` /
  `get_node_at_time`. This holds even when `valid_time` is in the
  **future**: current state reflects post-retraction *knowledge*, not
  post-`T` reality — an entity retracted at `now + 1h` disappears from
  current-state tools immediately (parity with `delete_node` +
  `valid_time`), even though `get_node_at_time` at today's valid time still
  returns it until `T` arrives.

**Parameters:** `node_id` / `edge_id` (required); `valid_time` (optional,
same formats as every other MCP temporal field; defaults to now; must not
precede the entity's `valid_from` — equality is allowed and yields an empty
interval — and must not be more than one year in the future, which is
rejected with `INVALID_ARGUMENT`); and, on `retract_node` only, `detach`
(default `false`). Transaction time is always system-assigned.

**Referential safety (mirrors `delete_node`'s #3209 contract):** retracting
a node that still has connected edges without `detach: true` is **refused**
with `FAILED_PRECONDITION` and `details.connected_edges` — never a `success`
that leaves edges pointing at a retracted node. `connected_edges` counts
**distinct** edges: a self-loop occupies both adjacency directions but is
one edge and counts once, so the refusal count always equals what
`detach: true` would retract. Passing `detach: true` co-retracts every
connected edge at the same `valid_time` and reports `edges_retracted`.

The detach form is **atomic**: every co-retracted edge is validated against
the same `valid_time`, and if ANY connected edge has a `valid_from` later
than `T` (retracting it at `T` would end it before it began), the whole
call fails with `INVALID_ARGUMENT` and **nothing** is retracted — not the
node, not the other edges. The fix is to retract that newer edge separately
at a `valid_time` at or after its own `valid_from`, then retry the detach.

```jsonc
// tools/call -> "retract_node"   (Alice churned on 2026-05-31)
{ "node_id": 7, "valid_time": "2026-05-31T00:00:00Z" }
// -> {
//      "success": true,
//      "node_id": 7,
//      "retracted": true,
//      "already_retracted": false,
//      "valid_from": "2021-03-01T00:00:00.000000Z",   // half-open interval
//      "valid_to":   "2026-05-31T00:00:00.000000Z",   // [valid_from, valid_to)
//      "edges_retracted": 0
//    }
```

Interval bounds are RFC 3339 strings with microsecond precision and a `Z`
suffix; both keys are always present in a retraction response (`null` would
denote an open-ended bound elsewhere).

The refusal, when connected edges exist and `detach` was not passed:

```jsonc
{
  "success": false, "node_id": 7, "connected_edges": 3, "detach_required": true,
  "error": {
    "code": "FAILED_PRECONDITION", "retriable": false,
    "message": "Node 7 has 3 connected edge(s); refusing to retract. ...",
    "details": { "node_id": 7, "connected_edges": 3, "detach_required": true }
  }
}
```

**Idempotency:** re-retracting an already-retracted (or deleted) entity is a
no-op that returns the **existing** `valid_to` — regardless of the
`valid_time` passed to the second call — with `already_retracted: true`. No
new version is appended and nothing is written, so an LLM retrying a
retraction can never accidentally move the end of a fact. One caveat when
the entity was **deleted** rather than retracted: the returned interval is
the delete tombstone's degenerate `[d, d)` — `valid_from` and `valid_to`
both equal the deletion's valid time, not the entity's original
`valid_from`.

**Errors:** a malformed `valid_time` is `INVALID_ARGUMENT`; a `valid_time`
before the entity's `valid_from` is `INVALID_ARGUMENT` (same
before-entity-creation family as the write tools above); a nonexistent
entity is `NOT_FOUND`.

Verifying the round trip:

```jsonc
// tools/call -> "get_node_at_time"   (before T: still true)
{ "node_id": 7, "valid_time": "2025-01-01T00:00:00Z" }
// -> { "node": { "id": 7, ... }, ... }

// tools/call -> "get_node_at_time"   (at/after T: no longer true)
{ "node_id": 7, "valid_time": "2026-06-01T00:00:00Z" }
// -> { "error": { "code": "NOT_FOUND", ... } }

// tools/call -> "get_node_history"   (zero version loss; the closed
// interval is visible as a distinct historical state)
{ "node_id": 7 }
```

## Point-in-time (AS OF) graph traversal

`traverse` accepts optional `as_of_valid_time` / `as_of_transaction_time`
fields (same formats as every other MCP temporal field), independently
settable, so an LLM can explore *relationships* as they existed at a past
bi-temporal instant in a single call instead of stitching together
`get_node_at_time`/`get_edge_at_time` lookups edge-by-edge. Omitting both
reproduces today's current-state traversal exactly.

**Example:** Alice and Bob became connected on 2021-03-01, backdated when
recorded via `create_edge`'s `valid_time` (see above). An LLM later asks for
Alice's `KNOWS` network as of last year:

```jsonc
// tools/call -> "traverse"
{
  "start_node_id": 7,
  "edge_label": "KNOWS",
  "direction": "outgoing",
  "depth": 1,
  "as_of_valid_time": "2025-07-06T00:00:00Z"
}
// -> {
//      "results": [ { "node": { "id": 8, "label": "Person", "properties": {"name": "Bob"} },
//                     "path": [7, 8], "depth": 1 } ],
//      "count": 1,
//      "as_of_valid_time": "2025-07-06T00:00:00Z",
//      "as_of_transaction_time": "<now, since only as_of_valid_time was supplied>"
//    }
```

Querying with `as_of_valid_time` set to a point *before* 2021-03-01 returns
`{"results": [], "count": 0}` -- the relationship, and Bob himself if he
didn't exist yet, are correctly excluded rather than silently falling back to
the current state. An edge created after the coordinate is excluded, and a
node no longer valid at the coordinate stops the traversal from continuing
past it (nothing reachable only through that node is reported).

**Recalling a since-deleted edge requires anchoring *both* dimensions before
the deletion.** `as_of_transaction_time` selects *which recorded version* of
the edge you see; `as_of_valid_time` is then checked against that version's
own valid-time interval. If Alice and Bob's edge above is later deleted for
real, the only version visible once `as_of_transaction_time` is at or after
that deletion is the tombstone -- and a tombstone has no valid-time interval
at all, so it can never match *any* `as_of_valid_time`, no matter how far in
the past. Concretely: supplying only `as_of_valid_time` (as the example
above does) lets `as_of_transaction_time` default to *now*, which is after
any real deletion -- so a since-deleted edge will **not** come back even for
an `as_of_valid_time` that falls squarely between its creation and
retirement. To see the relationship "as it was known before the deletion,"
also supply `as_of_transaction_time` anchored to a point before the deletion
was committed.

As with every other `as_of_*` field, an unparseable timestamp
returns a structured error instead of silently traversing current state:

```jsonc
{ "error": { "code": "INVALID_ARGUMENT", "retriable": false,
             "message": "Invalid as_of_valid_time: Invalid timestamp format: 'not-a-timestamp'. ..." } }
```

`MAX_TRAVERSAL_DEPTH` and `MAX_RESULT_LIMIT` apply identically whether or not
a temporal coordinate is supplied.

## Point-in-time (AS OF) node find by label and property

`find_nodes_at_time` resolves a real-world identifier to an entity's state at
a past bi-temporal point **without a prior `NodeId`** -- the entry-point
resolver the AS OF traversal above assumes the caller already has. Where
`list_nodes` answers "who is named Alice *now*?" and `get_node_at_time`
answers "what did node 42 look like at T?", this tool answers *"the Person
named Alice, as she was at T"* in one call (Issue #3236).

Parameters:

- `label` (**required**) -- the node label to match.
- `property_key` + `property_value` (optional, **both-or-neither**, mirroring
  `list_nodes`) -- exact-match property filter. Omit both for a label-only
  AS OF scan.
- `valid_time` (**required**) -- ISO 8601 / RFC 3339 or microseconds since
  epoch, like every other MCP temporal field.
- `transaction_time` (optional) -- defaults to **now**.
- `limit` / `offset` -- same pagination and `MAX_RESULT_LIMIT` /
  `MAX_PAGINATION_OFFSET` clamps as `list_nodes`; results are sorted by node
  id, so pages are stable.
- `include_vectors` -- vector properties are elided by default, as elsewhere.

Each returned node is **reconstructed as it existed at
`(valid_time, transaction_time)`** -- not its current state -- from the
historical version visible at that coordinate. Nodes that did not exist at
the queried point, or whose property value did not hold there, are excluded:
a node whose `name` became "Alice" only after T is not returned for a query
at T. With both dimensions at the current time, the result set equals the
current-state `list_nodes` property lookup **for nodes whose valid interval
has begun** -- the one divergence is future-dated facts: a node created with
a `valid_from` in the future (Issue #3221) is already present in current
state (so `list_nodes` returns it) but is not yet visible at `(now, now)` in
the bi-temporal view, so this tool excludes it until its valid time arrives.

```jsonc
// tools/call -> "find_nodes_at_time"
{
  "label": "Person",
  "property_key": "name",
  "property_value": "Alice",
  "valid_time": "2024-01-01T00:00:00Z",
  "transaction_time": "2024-01-01T00:00:00Z"
}
// -> {
//      "nodes": [ { "id": 7, "label": "Person",
//                   "properties": {"name": "Alice", "title": "Engineer"} } ],
//      "count": 1,
//      "offset": 0,
//      "limit": 100,
//      "sampled": false,  // true if the candidate scan was capped (see below)
//      "valid_time": "2024-01-01T00:00:00.000000Z",       // resolved coordinate,
//      "transaction_time": "2024-01-01T00:00:00.000000Z", // echoed as RFC 3339
//      "has_more": false,
//      "total_matching": 1
//    }
```

The response always echoes the **resolved** coordinate it was answered at --
an omitted `transaction_time` comes back as the concrete "now" it resolved
to, so the answer is reproducible later.

**Recalling superseded or deleted states requires anchoring *both*
dimensions**, exactly as with AS OF traversal above: a later update (or
delete) closes the previous version's *transaction* interval, so with
`transaction_time` defaulted to now only the latest recorded version is
visible. If Alice was renamed to "Alicia" yesterday, finding her as "Alice"
requires both `valid_time` *and* `transaction_time` anchored before the
rename; supplying only a past `valid_time` asks "using everything recorded so
far, who was named Alice at that instant?" -- and the current record says she
was already on her way to being Alicia. The same holds for since-deleted
nodes: this tool *does* find nodes that no longer exist in current state
(they are enumerated from history, not the live index), but only at a
`transaction_time` before the deletion was committed.

Validation errors are structured, consistent with `list_nodes`:

```jsonc
// property_key without property_value (or vice versa)
{ "error": { "code": "INVALID_ARGUMENT", "retriable": false,
             "message": "Both 'property_key' and 'property_value' are required together" } }
// unparseable/empty valid_time
{ "error": { "code": "INVALID_ARGUMENT", "retriable": false,
             "message": "Invalid valid_time: Invalid timestamp format: 'not-a-timestamp'. ..." } }
```

Performance note (v1): the tool scans every node that has ever had a version
recorded (the same candidate enumeration bi-temporal `get_schema` uses,
**capped at the same configurable limit** -- `max_schema_as_of_entities`,
default 50,000), resolving each candidate's version at the queried coordinate
and reconstructing properties only for candidates whose at-time label
matches -- complete, including deleted nodes, but a scan. When the cap is
hit, the lowest `cap` node ids are kept (a deterministic subset, so
pagination stays stable) and the response sets `sampled: true`, exactly as
`get_schema`'s AS OF form does; `total_matching` and `has_more` are then
honest only about the **sampled candidate set**, so a `sampled: true`
response may be missing matches with higher node ids. A dedicated temporal
label index is a deliberate follow-up if this misses its latency target at
scale. The edge equivalent (`find_edges_at_time`) is a planned fast-follow.

## Atomic multi-write batches (`apply_batch`)

Building an entity-with-relationships subgraph through the single-op tools
takes one call — one transaction — per write: if the third call fails, the
first two are already committed, leaving a half-built graph with no rollback.
`apply_batch` (Issue #3231) submits an **ordered** array of write operations
that commit **all-or-nothing** in one call, where later operations reference
nodes created earlier in the same batch via symbolic local refs (the
Datomic-tempid / XTDB-`submit-tx` pattern). The whole batch rides one
`WriteTransaction`: one WAL batch append, one GroupCommit fsync — commit
latency stays within the single-transaction envelope no matter how many
operations the batch carries.

### Request shape

```jsonc
{
  "operations": [
    {"op": "create_node", "label": "Person", "ref": "alice",
     "properties": {"name": "Alice"}},
    {"op": "create_node", "label": "Person", "ref": "bob",
     "properties": {"name": "Bob"}, "valid_time": "2024-01-15T10:00:00Z"},
    {"op": "create_edge", "source_id": "$alice", "target_id": "$bob",
     "label": "KNOWS", "properties": {"since": 2024}}
  ]
}
```

Supported `op` values: `create_node`, `create_edge`, `update_node`,
`update_edge`, `delete_node`, `delete_edge`. Each mirrors its single-op
tool's fields, including the optional #3221 `valid_time` (ISO 8601 or
microseconds since epoch) and the optional #3224 `provenance` bundle on
creates/updates.

**Local refs.** A `create_node` may carry a `ref` alias (unique within the
batch, must not start with `$`, must not be purely numeric). Later
`create_edge` operations may then use `"$alias"` — or the positional form
`"$<index>"` naming the create_node op at that array index — anywhere a node
id is accepted **as an edge endpoint**, freely mixed with committed integer
ids. A **forward reference** (naming a node only created later in the array)
is rejected, as are unknown refs and duplicate aliases — all statically,
before any transaction opens.

### Atomicity contract

If **any** operation fails — malformed op, unresolved ref, nonexistent
target, constraint violation, detach refusal, commit conflict — **none** of
the batch's writes take effect: for operation-time failures the
transaction's buffer is rolled back before anything is applied or
WAL-logged, and commit-phase failures (conflicts, constraint violations)
abort the transaction as a whole. Atomicity is guaranteed for **every
acknowledged outcome** (any success or error response the caller receives)
and for **all non-crash failure modes**. One narrow caveat: the WAL
currently has no transaction framing, so a process **crash during the
commit flush window** can persist a prefix of a batch that was never
acknowledged, and recovery may then replay that prefix — a pre-existing
engine property of all multi-operation transactions, tracked in issue
#3413. Separately, readers using non-snapshot read paths can briefly
observe a commit in progress mid-apply — also pre-existing behavior for
all multi-op transactions, not specific to `apply_batch`. The error
response reports the failing operation:

```jsonc
{ "error": {
    "code": "NOT_FOUND", "retriable": false,
    "message": "Node not found: NodeId(999999)",
    "details": { "failed_op_index": 2 } } }
```

`details.failed_op_index` is present on every **per-operation**
`apply_batch` error: the offending array index for per-op failures, or JSON
`null` for whole-batch failures that have no attributable op (a commit-phase
write-write `CONFLICT`, which is `retriable: true` — the whole batch is safe
to resubmit because nothing committed — and the over-cap rejection). A
top-level malformed-request error (arguments that don't parse as an
`operations` array at all) has no op to attribute and carries no
`failed_op_index`. A unique-constraint violation is
`CONSTRAINT_VIOLATION` with the usual `label`/`property`/`value`/
`existing_node_id` details. An over-cap batch is rejected up front with the
limit echoed (#3226 convention):
`details: {limit: 1000, submitted: 1042, failed_op_index: null}`.
The cap defaults to 1000 operations and is tunable via
`AletheiaMcpServer::with_max_batch_operations`.

`delete_node` inside a batch honors the #3209 safe-by-default DETACH
contract — against committed **and** batch-created edges (the handler keeps
a batch-local adjacency ledger, since committed-state adjacency cannot see
uncommitted edges). Deleting a node that still has connected edges at that
point in the batch is refused (`FAILED_PRECONDITION`,
`details.connected_edges` counting distinct edges — a self-loop counts once —
plus `failed_op_index`) unless the op passes `detach: true`, in which case
committed edges are cascade-deleted and batch-created edges touching the
node are removed too. Edges the batch already deleted no longer count.
`valid_time` is not supported together with `detach: true` (same rule as the
single-op tool), rejected per-op. Note one counting divergence: `apply_batch`
counts **distinct** edges (a self-loop counts once), while the single-op
`delete_node` tool's committed-state count tallies both adjacency directions
(a self-loop counts as 2) — convergence is tracked in issue #3416. As with
the single-op tools, a residual snapshot-isolation write-skew remains: a
concurrent transaction committing an edge to a node this batch deletes can
still orphan that edge despite the pre-delete count (also issue #3416).

### Success response

```jsonc
{
  "success": true,
  "operation_count": 3,
  "results": [                      // per-op results, in input order
    {"op": "create_node", "index": 0, "ref": "alice", "node_id": 7, "version_id": 12},
    {"op": "create_node", "index": 1, "ref": "bob",   "node_id": 8, "version_id": 13},
    {"op": "create_edge", "index": 2, "edge_id": 3, "version_id": 14,
     "source_id": 7, "target_id": 8}
  ],
  "ref_map": { "alice": 7, "bob": 8 }  // every alias -> committed real id
}
```

`update_node`/`update_edge` results carry the entity id and the new
`version_id`; `delete_node` results carry `detached` and `edges_removed`
(committed edges cascaded plus batch-created edges removed). A batch-created
edge that a later `delete_node` + `detach: true` removes is reported with
`edge_id: null`, `version_id: null`, and `removed_by_delete_at_index` — see
the last v1 limitation below.

### v1 limitations

- **No update/delete of batch-created entities**: refs are accepted only as
  edge endpoints. `update_node`/`delete_node` targeting `"$alias"` is
  rejected with a clear error (`INVALID_ARGUMENT` + `failed_op_index`) —
  read-your-writes inside an uncommitted batch is out of scope for #3231;
  commit the create first, then modify it in a follow-up call.
- **One write per committed entity per batch**: a second `update`/`delete`
  aimed at the same node/edge id is rejected (the second op would act on a
  stale read of the pre-batch state); `details` names both op indices.
- **Deletes carry no `version_id`**: tombstone versions are allocated inside
  the commit path and are not surfaced by the transaction API.
- **Batch-created edges removed by a later detach delete leave no trace**:
  such an edge is elided from the transaction entirely (never allocated,
  never written to history) — the committed record equals the batch's atomic
  net effect. Its per-op result says so via `removed_by_delete_at_index`.
  Because elision writes no history, a to-be-elided `create_edge` carrying an
  **explicit `valid_time`** is rejected up front (`INVALID_ARGUMENT` with
  `failed_op_index` = the create's index and `removed_by_delete_at_index` =
  the delete's index): silently eliding a backdated fact would erase history
  that single-op sequencing (create, commit, then delete) preserves — drop
  the create from the batch, or commit it in a separate call before the
  delete.
- An **empty** `operations` array is a harmless, idempotent no-op success
  (`results: []`, `ref_map: {}`).
- Updating an edge and then detach-deleting one of its endpoint nodes in the
  same batch is refused with both op indices (the cascade would silently
  discard the update).

## Structured error codes and the retriable contract

Every MCP tool error — across **all** tools, not just `query` — is a JSON
object of this shape (Issue #3234):

```json
{
  "error": {
    "code": "FAILED_PRECONDITION",
    "message": "Node 7 has 2 connected edge(s); refusing to delete. ...",
    "retriable": false,
    "details": { "node_id": 7, "connected_edges": 2, "detach_required": true }
  }
}
```

- **`code`** is drawn from a small, stable enum (below). Branch on `code`,
  never on `message` — the free text may be reworded; the codes never change
  meaning or spelling.
- **`message`** preserves the human-readable text that older releases
  returned as the entire `error` value (the change is additive — nothing is
  lost).
- **`retriable`** is the server's advisory classification: `true` **only**
  for transient failure classes (timeouts, clock skew, serialization/write
  conflicts) where an identical retry can succeed. It is always `false` for
  caller-fault classes — retrying the same not-found lookup or malformed
  argument cannot succeed. The client owns the retry loop (backoff, attempt
  caps); the server never retries on its behalf.
- **`details`** is optional structured metadata for specific codes, e.g. the
  DETACH-delete refusal's `connected_edges` or a unique-constraint
  violation's `existing_node_id`. When absent it is omitted entirely — never
  `null`.

### The code enum

| Code | Meaning | `retriable` | What the caller should do |
|------|---------|-------------|---------------------------|
| `NOT_FOUND` | Entity doesn't exist, or didn't exist at the requested bi-temporal coordinate; also an unknown tool name | `false` | Re-check the ID / coordinate or the tool name, or surface to the user |
| `INVALID_ARGUMENT` | Malformed arguments: bad JSON, out-of-range ID, unparseable timestamp, invalid query text, inconsistent parameter combination, unsupported constraint key type | `false` | Fix the arguments and re-issue |
| `CONSTRAINT_VIOLATION` | A declared uniqueness constraint rejected the write (`details` carries `label`, `property`, `value`, `existing_node_id`) | `false` | Use the existing entity or change the value |
| `FAILED_PRECONDITION` | Valid request, wrong system state: vector index not enabled, node still has connected edges without `detach: true`, referenced edge endpoint missing, enabling a unique constraint over already-existing duplicate values, feature not compiled in | `false` | Change the state (enable the index, pass `detach`, create the endpoint, dedupe the data), then re-issue |
| `CONFLICT` | Concurrency conflict: serialization failure, write-write conflict, aborted transaction | usually `true` | Retry the operation (a duplicate-ID conflict is the exception: `retriable: false`) |
| `UNAVAILABLE` | Transient condition: query timeout, clock skew, and other clock-related hiccups (non-monotonic transaction time, logical counter overflow) | `true` | Retry, ideally with backoff |
| `INTERNAL` | Unexpected internal failure: I/O, corruption, poisoned lock | `false` | Report; do not blind-retry |
| `UNAUTHENTICATED` | No valid session credential in `required` auth mode (Issue #3350) — missing, unknown, or revoked; deliberately indistinguishable, and returned for *every* tool including unknown tool names (no inventory leak). Never carries `details`, never echoes the credential | `false` | Supply a valid `ALETHEIADB_MCP_API_KEY` (or bootstrap key) and restart the session; retrying with the same credential cannot succeed |
| `PERMISSION_DENIED` | Authenticated, but the principal's role does not allow the tool's access class (Issue #3350). `details` carries `required_class` and `principal_role` | `false` | Use a credential whose role allows the class (see [docs/guides/access-control-matrix.md](access-control-matrix.md)); do not retry with the same key |

Codes may be **added** over time; existing codes never change. Treat an
unrecognized code as non-retriable. `UNAUTHENTICATED` and
`PERMISSION_DENIED` (Issue #3350) extend the original #3234 enum
**additively** — pre-#3350 consumers that treat unknown codes as
non-retriable already handle them correctly. Setup for authenticated
deployments is covered in the
[security quickstart](security-quickstart.md).

### Recovery loop example (LLM-style)

An agent driving AletheiaDB branches on `code` — zero substring matching:

```jsonc
// 1. tools/call -> "delete_node" { "node_id": 7 }
// -> { "error": { "code": "FAILED_PRECONDITION", "retriable": false,
//                 "message": "Node 7 has 2 connected edge(s); refusing to delete. ...",
//                 "details": { "node_id": 7, "connected_edges": 2, "detach_required": true } },
//      "node_id": 7, "connected_edges": 2, "detach_required": true, "success": false }

// 2. code == "FAILED_PRECONDITION" and details.detach_required
//    -> repair the call, don't retry blindly:
// tools/call -> "delete_node" { "node_id": 7, "detach": true }
// -> { "success": true, "deleted_node_id": 7, "detached": true, "edges_removed": 2 }
```

The general loop: `retriable: true` → retry with backoff (bounded attempts);
`INVALID_ARGUMENT` / `FAILED_PRECONDITION` / `CONSTRAINT_VIOLATION` → repair
the request using `message` + `details`, then re-issue; `NOT_FOUND` /
`INTERNAL` → escalate to the user.

Legacy top-level fields that predate this contract (the DETACH refusal's
`connected_edges` / `detach_required`, the unique-violation's
`constraint_violation` / `existing_node_id`) remain present alongside
`error.details`, so pre-#3234 consumers keep working.

## Discovering the queryable temporal extent (`temporal_extent`)

Every `AS OF` field above shares a silent failure mode: an instant *before
the data begins* returns an empty result that is indistinguishable from "the
fact never existed." The `temporal_extent` tool (Issue #3238) closes that gap
by reporting, in one call, the calendar range the dataset actually covers, so
a caller can check "does this dataset even reach time T?" *before* issuing an
`AS OF` query. It is backed by the additive public API
`AletheiaDB::temporal_extent()` / `temporal_extent_by_label()`.

```jsonc
// tools/call -> "temporal_extent" (no required arguments)
{}
// -> {
//      "valid_time":       { "earliest": "2021-03-01T00:00:00.000000Z",
//                            "latest":   "2026-07-01T09:15:00.123456Z" },
//      "transaction_time": { "earliest": "2026-06-20T08:00:00.000000Z",
//                            "latest":   "2026-07-01T09:15:00.123456Z" }
//    }
```

Semantics (also stated in the tool's description so an LLM can interpret the
snapshot without reading source):

- **Extent, not current state.** Bounds cover recorded history — including
  expired/superseded versions and deletions — not just the current state. A
  fact written for 2019 and later corrected still counts toward
  `valid_time.earliest`. This is a calendar *range*; for counts/magnitude use
  `count_nodes` (a dedicated stats tool is tracked in issue #3222).
- **Open-interval convention.** `earliest` is the minimum interval start in
  that dimension. `latest` is the maximum of interval starts and *closed*
  interval ends; open-ended intervals (still-valid facts / still-current
  records) contribute only their start. `latest` is therefore the newest
  finite recorded event coordinate — never `+infinity`, and never the
  open-interval sentinel.
- **Empty database.** All four bounds come back as explicit `null` — never
  `0`/`1970-01-01` — so "no data" cannot be misread as "data since the epoch."

Pass `by_label: true` to additionally receive the same
`{valid_time, transaction_time}` bounds per node label (`node_labels`) and per
edge type (`edge_types`), so calibration can be scoped to exactly the labels a
query touches:

```jsonc
// tools/call -> "temporal_extent"
{ "by_label": true }
// -> {
//      "valid_time": { ... }, "transaction_time": { ... },
//      "node_labels": [
//        { "label": "Company", "valid_time": { ... }, "transaction_time": { ... } },
//        { "label": "Person",  "valid_time": { ... }, "transaction_time": { ... } }
//      ],
//      "edge_types": [
//        { "edge_type": "WORKS_AT", "valid_time": { ... }, "transaction_time": { ... } }
//      ]
//    }
```

The overall bounds are read in O(1) from an aggregate the temporal indexes
maintain at write time; bounds only ever widen while the server runs, so a
caller can cache the result for the duration of a session. The per-label
breakdown is folded from the hot-tier historical version store.

**Coverage (cold storage + restarts).** Overall bounds cover all history
recorded during the **current process lifetime**, plus the hot-tier history
restored at startup, plus history migrated to the cold tier — including
across restarts (Issue #3389). The cold store persists its min/max extent
bounds per dimension in metadata (maintained incrementally as versions
migrate), and those bounds are merged back into the extent aggregate at
startup, so a fact migrated to cold before a restart still bounds the extent.
The **per-label breakdown** is still folded from the hot-tier historical
version store only: after cold migration a label's per-label bounds may be
narrower than the overall bounds, or a label may be absent entirely (the
persisted cold bounds are aggregate-only, not per-label). Bounds never
shrink. One narrow gap: bounds are **not** backfilled for a cold file created
by a pre-#3389 binary that already held versions — such pre-existing cold
history is captured only from the first new write onward, so the extent can
under-report (never over-report) until those versions are re-touched.

**Calibration pattern:** if `temporal_extent` reports
`valid_time.earliest = 2021-03-01`, an `AS OF '2019-01-01'` query is
guaranteed to be out of recorded range — its empty result means "before our
records begin," not "nothing existed." Rerun the query at an instant inside
the extent (or report the range mismatch) instead of concluding absence.

## Database stats and storage-tier health (`database_stats`)

*(Issue #3222)* The `database_stats` tool takes **no arguments** and returns a
holistic snapshot of dataset size, bi-temporal depth, storage-tier
distribution, and WAL state in a single call. Use it to orient yourself before
querying ("how big is this dataset? is there history to time-travel through?
is cold migration even on?") instead of stitching together `count_nodes` +
`count_edges` — which cannot reveal version depth, tier distribution, or WAL
state at all. It is backed by the public Rust API `AletheiaDB::stats()`, which
returns the same data as a serializable `DatabaseStats`; the MCP handler only
serializes that snapshot.

### Response shape

```jsonc
{
  "current": {                    // current-state (hot, in-RAM) graph size
    "node_count": 10000,
    "edge_count": 50000
  },
  "historical": {                 // bi-temporal depth of the in-RAM store
    "total_node_versions": 12500, // node states retained in RAM (versions
                                  // migrated to the cold tier are counted
                                  // under cold_storage, not here)
    "total_edge_versions": 50000,
    "unique_nodes": 10000,        // distinct nodes with any history
    "unique_edges": 50000,
    "anchor_count": 11000,        // full property snapshots (node + edge)
    "delta_count": 51500,         // change-only versions (node + edge)
    "node_anchor_count": 10500,   // per-entity-type breakdowns of the above
    "node_delta_count": 2000,
    "edge_anchor_count": 500,
    "edge_delta_count": 49500,
    "compression_ratio": 0.176    // anchors / total versions; LOWER = better
  },
  "cold_storage": {               // disk tier; see "disabled tiers" below
    "enabled": true,
    "node_versions_stored": 800,  // versions on disk (persists across restarts)
    "edge_versions_stored": 3200,
    "compression_ratio": 3.8,     // raw/compressed bytes written since this
                                  // process opened the DB; HIGHER = better
    "tier_access": {              // where historical reads were served from,
                                  // counted since this process opened the DB
      "hot_hits": 90210,
      "warm_hits": 4310,
      "cold_hits": 122,
      "misses": 0
    }
  },
  "wal": {
    "enabled": true,              // always true in current builds
    "durability_mode": "group_commit", // synchronous | async | group_commit | async_batched
    "current_lsn": 62501,         // NEXT LSN to be allocated
    "total_appends": 62500,
    "healthy": true               // false = outstanding WAL flush errors
  }
}
```

The cold-tier version counts are seeded from the persisted tables when the
database is opened (an O(1) metadata read), so a restarted process reports its
full on-disk cold history rather than zero. The byte-level
`compression_ratio` and the `tier_access` counters are **not** persisted:
they describe activity since the current process opened the database
(`compression_ratio` is `1.0` until this process writes to the cold tier).

### Disabled tiers are tagged, never zero-reported

When no cold-storage tier is configured, the response contains exactly
`"cold_storage": { "enabled": false }` — the count fields are **absent**, not
zero. Never interpret a disabled tier as "0 cold versions"; it means the
database retains history only in RAM (`historical`) and nothing has been (or
can be) migrated to disk. The same contract applies to `wal.enabled`, which is
always `true` in current builds (AletheiaDB has no no-WAL construction path)
but is emitted explicitly so consumers never have to infer WAL presence.

### Every field is O(1) — safe to call frequently

All values are reads of counters the storage engines already maintain
incrementally: current counts are index-length reads, historical counts come
from `HistoricalStorage::stats()` (cached counters per Issue #212 — never a
version scan), tier counters are atomic snapshots, and WAL fields are atomic
loads. A `database_stats` call completes in microseconds regardless of
database size.

Note the two `compression_ratio` fields measure different things:
`historical.compression_ratio` is the anchor share of versions (lower is
better delta compression); `cold_storage.compression_ratio` is the byte-level
Zstd/LZ4 ratio on disk (higher is better).

### Scope

`database_stats` reports magnitude/counts and tier health at a point in time.
It does **not** report the calendar range of stored history (earliest/latest
timestamps) — use
[`temporal_extent`](#discovering-the-queryable-temporal-extent-temporal_extent)
for that — and it does not break counts
down per label; use `get_schema` for per-label/per-edge-type counts.

## Temporal bounds on read responses

Every node/edge read response carries a `temporal` block describing the
bi-temporal bounds of the *exact version* the response reflects
(Issue #3232), so an LLM/caller always knows when the returned fact was true
in reality (valid time) and when it was recorded (transaction time) without
a follow-up `get_node_history` call.

Covered tools: `get_node`, `create_node`, `update_node`, `get_edge`,
`create_edge`, `update_edge`, `list_nodes`, `traverse`, `get_outgoing_edges`,
`get_incoming_edges`, `find_similar`, `hybrid_query`, `get_node_at_time`,
`get_edge_at_time`, `get_node_at_valid_time`, `get_node_at_transaction_time`,
`get_edge_at_valid_time`, and `get_edge_at_transaction_time` — the block has
the identical shape on nodes and edges, for current-state and point-in-time
reads alike.

```jsonc
// tools/call -> "get_node"
{ "node_id": 7 }
// -> {
//      "id": 7,
//      "label": "Person",
//      "properties": { "name": "Alice" },
//      "temporal": {
//        "valid_from": "2026-07-07T12:00:00.000000Z",
//        "valid_to": null,
//        "transaction_from": "2026-07-07T12:00:00.000000Z",
//        "transaction_to": null,
//        "is_current": true
//      }
//    }
```

Conventions:

- **Timestamps are RFC 3339 strings** with microsecond precision and a `Z`
  (UTC) suffix. Intervals are half-open (`[start, end)`).
- **Open-ended bounds are explicit JSON `null`** — `valid_to`/`transaction_to`
  are always present, never omitted. `null` means "still valid" / "still the
  recorded version". In the rare case the version metadata for a returned
  entity cannot be loaded, the whole `temporal` block is omitted (mirroring
  `provenance`), while open bounds within a present block are always explicit
  `null`.
- **`is_current`** is `true` iff the version's transaction interval is still
  open (it *is* the currently-recorded version) and the wallclock now falls
  within its valid interval — i.e. the response reflects the live, current
  version. A superseded version returned by a point-in-time read, or a fact
  whose `valid_to` has passed (or whose `valid_from` has not yet arrived),
  reports `is_current: false` with its closed bounds. Within a single
  response, every entity's `is_current` is evaluated against the same
  request-scoped instant — the wallclock is captured once per tool call,
  never once per entity (Issue #3391). The valid-time
  comparison is at wallclock (microsecond) granularity, so the logical
  component of a hybrid-logical-clock commit timestamp never affects the
  answer:

```jsonc
// tools/call -> "get_node_at_time" (anchored before an update)
{ "node_id": 7, "valid_time": "2026-07-01T00:00:00Z", "transaction_time": "2026-07-01T00:00:00Z" }
// -> {
//      "node": {
//        "id": 7,
//        "label": "Person",
//        "properties": { "name": "Alice" },
//        "temporal": {
//          "valid_from": "2026-06-01T09:30:00.000000Z",
//          "valid_to": "2026-07-03T08:15:27.412000Z",
//          "transaction_from": "2026-06-01T09:30:00.000000Z",
//          "transaction_to": "2026-07-03T08:15:27.412000Z",
//          "is_current": false
//        }
//      },
//      "valid_time": "...", "transaction_time": "..."
//    }
```

The bounds always describe the version actually returned and decode to the
same microsecond values `get_node_history` reports for that version (history
keeps its existing microseconds-as-string format — that contract is
unchanged). The block is purely additive: no existing field moved or changed
shape.

## Token-budget-aware responses (Issue #3353)

An LLM's context window is its scarcest resource, yet the read tools size their
responses by *row count* (`limit`), not by *cost*: a `limit: 50` traversal can
return 500 tokens or 50,000 depending on the property payloads, and the caller
cannot know in advance. The token budget lets a caller say "spend at most N
tokens answering this" and receive a response *guaranteed* to fit, with an
explicit, machine-readable account of what was reduced and how to fetch it.

### The parameters

The **thirteen** budgetable read tools — `get_node`, `list_nodes`, `get_edge`,
`list_edges`, `get_outgoing_edges`, `get_incoming_edges`, `traverse`,
`find_similar`, `hybrid_query`, `query`, `find_nodes_at_time`,
`get_node_history`, and `get_schema` — accept these optional parameters. This is
the exact set (the code's single source of truth is `BUDGETABLE_READ_TOOLS`);
it is **not** *every* read tool — single-entity/aggregate reads such as
`get_node_at_time`, `get_edge_history`, `diff_node_versions`, `temporal_extent`,
`database_stats`, and `count_nodes` are out of scope. The three parameters are
injected into each budgetable tool's advertised `inputSchema.properties`, so a
client that introspects the schema discovers them (with correct types) rather
than relying on the prose description alone. Omitting all of them leaves the
tool's behavior **completely unchanged**.

| Parameter | Meaning |
|-----------|---------|
| `max_response_tokens` | Maximum response size in **estimated tokens**. The serialized **success** response, *including the truncation metadata itself*, is guaranteed not to exceed this. |
| `max_response_bytes` | Byte-exact alternative. When both are set, the **tighter** bound wins. |
| `priority_properties` | Array of property keys to protect from elision; they out-survive unprotected properties at every degradation rung. |

The budget bounds **success** responses. A structured *error* response (for
example the too-small-budget `INVALID_ARGUMENT` below) is itself small and is
returned intact. In the rare case a budgetable tool returns a non-object success
payload (a JSON scalar/array, or plain text) it cannot degrade along the entity
ladder, but the byte cap is still enforced: the payload is truncated with a
disclosed marker rather than emitted unbounded.

A **misspelled or unknown budget key** (e.g. `max_tokens` instead of
`max_response_tokens`) is **ignored** — consistent with the surface's
unknown-field tolerance — and the full, unbudgeted response is returned. Use the
exact key names above so a budget you intend to apply is actually applied.

**Token-estimation basis:** tokens are estimated as `ceil(utf8_byte_len / 4)`.
Four bytes per token is the standard approximation of GPT/Claude-family BPE
tokenizers for English-plus-JSON text and holds within ~10% at the 1K-token
scale. Callers needing an exact wire bound use `max_response_bytes`, which is
enforced byte-for-byte.

### The degradation ladder (deterministic, disclosed)

Over budget, the response degrades along a fixed, ordered ladder. The same
request at the same budget on the same data always degrades **identically**:

1. **`full`** — nothing reduced.
2. **`elided_properties`** — inside each entity's `properties`, any value whose
   serialized size exceeds a threshold (and is not a protected
   `priority_properties` key) is replaced with an `{ "elided": true, ... }`
   descriptor, mirroring the vector-elision convention of #3220. A value is
   elided **only when the descriptor is actually smaller** than the value it
   replaces, so this rung can never enlarge the response.
3. **`entity_summaries`** — each entity's `properties` is reduced to the
   protected keys only. Result *structure* — ids, labels, relationships,
   temporal coordinates, provenance and similarity scores — survives because it
   lives *beside* `properties`, never inside it.
4. **`counts_and_handles`** — entity arrays are truncated to the prefix that
   fits; the omitted tail is disclosed as a count plus a fetch handle, **and the
   object's own pagination/count siblings are rewritten** to describe the
   retained prefix — `count`/`row_count` become the retained length,
   `has_more`/`truncated` become `true`, and `next_offset` (on offset-paginated
   tools) advances to exactly the cut point. This keeps a paginating caller
   gap-free and duplicate-free: following the disclosed resume call yields the
   dropped rows and nothing else. (`total_matching`, when present, still counts
   the full matching set and is left unchanged.)

`find_similar` and `hybrid_query` **never reach rung 4**: their ranked results
are never dropped or reordered to meet a budget — only the per-result payloads
degrade. Temporal responses never omit the temporal coordinates that make a
result interpretable.

Every response carries a `budget` block naming the rung applied per section:

```jsonc
// tools/call -> "get_node"
{ "node_id": 7, "max_response_tokens": 400 }
// -> {
//      "id": 7,
//      "label": "Person",
//      "properties": {
//        "name": "Alice",
//        "bio": {
//          "elided": true, "reason": "budget", "type": "string", "size_bytes": 4002,
//          "fetch": { "tool": "get_node",
//                     "arguments": { "node_id": 7, "include_vectors": true } }
//        }
//      },
//      "temporal": { /* ... always preserved ... */ },
//      "budget": {
//        "applied": true,
//        "rung": "elided_properties",
//        "token_estimation_basis": "ceil(utf8_bytes / 4)",
//        "requested_max_tokens": 400,
//        "effective_max_bytes": 1600,
//        "priority_properties": [],
//        "sections": [ { "section": "properties", "rung": "elided_properties",
//                        "elided_values": 1 } ]
//      }
//    }
```

### Fetch handles — nothing is lost

Every elision/truncation site carries a **fetch handle**: a concrete follow-up
call (`tool` + `arguments`) that retrieves exactly the omitted content.

- A **per-entity elision** on a live node/edge points at `get_node`/`get_edge`
  with `include_vectors: true`.
- A **history version** elision (`get_node_history`/`get_edge_history`) points at
  `get_node_at_time`/`get_edge_at_time` addressing the *exact superseded
  version* — the parent entity id (taken from the history wrapper) plus that
  version's own `valid_from`/`transaction_from` coordinates. A plain `get_node`
  would return the current state, not the historical version, so the handle uses
  the point-in-time tool instead.
- A **truncated array** on an offset-paginated tool
  (`list_nodes`/`traverse`/`find_nodes_at_time`) carries a concrete resume call:
  the original arguments with the budget knobs stripped and `offset` advanced to
  the cut point (composing with the #3226 `next_offset` completeness signal). On
  a tool that does **not** page by offset (e.g. `get_outgoing_edges`,
  `get_schema`, `query`) the handle honestly discloses the truncation and tells
  the caller to re-request with a larger budget — it never fabricates an `offset`
  argument the tool does not accept.

An agent that follows the handles can reconstruct the full, unbudgeted response.

### Budgets too small to satisfy

If the budget is too small to return even the minimal rung, the tool returns a
structured #3234 `INVALID_ARGUMENT` error stating the **minimum viable budget** —
never a silently empty success. The reported minimum is **self-consistent**:
re-issuing the same request at `min_viable_tokens` (or `min_viable_bytes`)
succeeds — the figure already accounts for the disclosed `budget` block's own
numbers growing at the larger budget.

```jsonc
{ "error": {
    "code": "INVALID_ARGUMENT",
    "message": "requested budget is too small to return even the minimal response for this request; minimum viable budget is approximately 1222 tokens (4886 bytes)",
    "retriable": false,
    "details": { "min_viable_tokens": 1222, "min_viable_bytes": 4886,
                 "requested_tokens": 1200, "requested_bytes": null }
} }
```

### Composition and scope

- **Composes with #3220 vector elision** (already-elided vectors are left as-is),
  **#3226 completeness signals** (truncation handles reference `next_offset`),
  and the **#3234 error contract** (the too-small-budget error is `INVALID_ARGUMENT`).
- **Read tools only.** Write and admin tools are out of scope (their responses
  are already small and fixed-shape). Cursor continuation of large results
  (#3360) is a complementary, now-landed feature — see
  [Paging large results](#paging-large-results-cursor-continuation) below for how
  the two compose.

## Paging large results (cursor continuation)

Offset pagination (`offset`/`next_offset`, Issue #3226) works, but over a
*concurrently written* database it is quietly unsafe: between fetching page 1
and page 2 other agents write, offsets shift, and the reader sees duplicates
or misses rows — and each page is computed against a *different* database
state, so an agent assembling "all Persons matching X" across five pages can
return a set that never existed at any single moment. It also degrades
linearly (page 50 recomputes and discards 4,900 rows).

**Cursor continuation (Issue #3360)** fixes this. Set `use_cursor: true` on
the first call to a bounded read tool; the response includes an opaque
`cursor` token. Pass that token back — *with no other parameters* — to fetch
the next page. When no `cursor` is present in a response, the scan is
complete.

### The consistency guarantee

Every page of one cursor scan is evaluated at the **bi-temporal coordinate
captured on the first page** (disclosed in each response as
`snapshot_valid_time` / `snapshot_transaction_time`), leveraging AletheiaDB's
existing point-in-time read semantics. Concretely, for the whole scan:

- a node/edge **created** after the first page is **never** seen (it did not
  exist in the snapshot) — no post-cursor leakage;
- a node/edge **deleted** after the first page is **still** seen (the snapshot
  predates the deletion) — no omission;
- a node/edge **updated** after the first page is returned **as it was** at
  the snapshot.

So the union of all pages equals *exactly* the unbounded result at one
consistent moment — zero duplicates, zero gaps — even under concurrent
mutation, **up to the candidate cap** (see below). This is uniquely cheap for
AletheiaDB: the bi-temporal engine already answers "the database as of
coordinate T" natively, so consistent paging falls out of existing semantics
rather than requiring a held-open transaction (contrast Qdrant/Weaviate/Milvus
scroll APIs, which drift under concurrent writes because they have no
coordinate to anchor to).

**Candidate cap (`sampled`).** The node scans (`list_nodes`,
`find_nodes_at_time`) route through the #3236 point-in-time finders, whose
candidate enumeration is capped at `max_schema_as_of_entities` (default 50,000,
lowest node ids kept). When the labelled candidate set exceeds that cap, every
page of the scan carries `sampled: true`, and the "union equals exactly the
unbounded result" guarantee holds only **up to the cap** — the scan is bounded
by the cap, not exhausted. `total_matching` then counts matches within the
sampled candidate set only. When `sampled` is `false` (the common case) the
union is the complete result. To scan a set larger than the cap with full
coverage, narrow it with a `property_key`/`property_value` filter (or a more
specific `label`) so the candidate set fits under the cap.

**Current-state vs. bi-temporal-at-now divergence.** `get_outgoing_edges`,
`get_incoming_edges`, and `traverse` in cursor mode (with no `as_of_*`
coordinate supplied) pin the snapshot at "now" on the first page and answer via
the bi-temporal **as-of-now** read path — *not* the plain current-state path
their default (non-cursor) mode uses. The practical consequence: a cursor scan
**excludes future-valid** rows (an edge or node whose `valid_from` is in the
future, e.g. a #3221 forward-dated fact), whereas a plain current-state
`get_outgoing_edges` / `traverse` call returns them. This is the same tradeoff
`find_nodes_at_time` (and #3236) already documents for point-in-time reads: a
future-dated `valid_from` row is in current state but not yet visible at
`(now, now)`. If you specifically need future-valid rows, use the tool's
non-cursor mode.

### The cursor loop

```jsonc
// Page 1 — opt in. `label` is required (an unlabeled list has no ordered set).
// tools/call -> "list_nodes"
{ "label": "Person", "use_cursor": true, "limit": 100 }
// -> {
//      "nodes": [ ...up to 100, ascending by id... ],
//      "count": 100,
//      "total_matching": 2500,
//      "has_more": true,
//      "paging": "cursor",
//      "snapshot_valid_time": "2026-07-09T12:00:00.000000Z",
//      "snapshot_transaction_time": "2026-07-09T12:00:00.000000Z",
//      "cursor": "aletheiadb.cursor.v1.eyJ2Ijox...==.Qk9x...",   // opaque
//      "cursor_ttl_seconds": 300
//    }

// Page 2..N — pass the cursor back verbatim, nothing else.
// tools/call -> "list_nodes"
{ "cursor": "aletheiadb.cursor.v1.eyJ2Ijox...==.Qk9x..." }
// -> { ...next 100..., "cursor": "aletheiadb.cursor.v1.…", ... }

// Last page: no `cursor` field and `has_more: false`. Stop.
```

An agent completing a 10K-row scan therefore sends the query text **once**
(one full request + N cursor-only continuations), not N filtered re-queries.

### Which tools are cursorable

| Tool | Cursor support | Ordering / continuation |
|------|----------------|-------------------------|
| `list_nodes` | Yes (requires `label`) | Ascending node id — **keyset** |
| `find_nodes_at_time` | Yes | Ascending node id — **keyset**; snapshot is the request's `(valid_time, transaction_time)` |
| `get_outgoing_edges` | Yes | Ascending edge id — **keyset** |
| `get_incoming_edges` | Yes | Ascending edge id — **keyset** |
| `traverse` | Yes | Deterministic DFS order — snapshot-pinned **offset** in v1 |
| `query` | No (v1) | Returns a structured `unsupported_construct` error — no silent fallback; use `list_nodes`/`find_nodes_at_time` |
| `list_edges` | No | Does not enumerate edges; returns `INVALID_ARGUMENT` pointing at `get_outgoing_edges`/`get_incoming_edges` |

The **keyset** continuation avoids **re-emitting prior result pages** (no
duplicates, no gaps): page N does not re-send the rows already returned on pages
1..N−1, unlike offset paging which re-materializes and discards them. Note this
is *result-page* deduplication, **not** a depth-independent seek — in v1 the
candidate enumeration is still O(total) per page (the node scans re-run the full
`find_nodes_at_time` candidate scan, and the adjacency scans re-resolve the
whole edge set, on every page). A true depth-independent keyset seek that skips
prior candidates is a follow-up. `traverse` likewise pins the snapshot (so every
page is consistent) but continues by an internal offset that re-runs the
traversal each page in v1.

### Token, lifecycle, and error contract

- **Opaque and LLM-safe.** The token is a printable, bounded-length,
  base64url string (`aletheiadb.cursor.v1.<payload>.<signature>`) with no
  escaping hazards — safe to echo back verbatim. It is *self-describing to the
  server*: the originating tool, the pinned snapshot, the keyset position, the
  page size, and the query filters are all encoded inside it and signed with a
  per-process secret. Continuation needs no other parameters.
- **Stateless design.** No server-side scan state is held, so there is no
  unbounded memory growth. A tiny in-process registry tracks only live cursor
  *ids* (not pages) to enforce the cap and make reclamation observable.
- **TTL.** Cursors expire after a documented, configurable TTL (default 5
  minutes, surfaced as `cursor_ttl_seconds`), refreshed on each page (an idle
  timeout between successive fetches). Expired cursors pin no storage.
- **Live-cursor cap.** A configurable per-connection cap (default 128) bounds
  concurrently open scans; continuation pages of one scan reuse its slot, so a
  thousand-page scan holds exactly one.
- **Cross-restart.** Cursors do **not** survive a server restart (the signing
  secret is per-process); a stale token simply fails verification and the
  caller re-issues the query.
- **Structured errors** (Issue #3234): a **tampered**, malformed, or
  wrong-tool token returns `INVALID_ARGUMENT` (never wrong data); an
  **expired** cursor or one **exceeding the cap** returns `FAILED_PRECONDITION`
  with remediation guidance (re-issue the query). Both are `retriable: false`.

### Composing with token budgets (Issue #3353)

Cursors and token-budget truncation (#3353) are both available and complementary.
They compose along a fixed order: within one `call_tool` the cursor page is
produced **first** (the handler pages the snapshot-anchored scan), then the token
budget shapes **that page** — a budget-constrained page simply ends up smaller
(fewer rows retained, or degraded payloads), while the cursor still resumes the
same consistent scan on the next call. Budgeting shapes *one* call; cursors move
data *across* calls; the snapshot is unchanged between pages.

Caveat for v1: the cursor continuation key is derived from the underlying keyset
scan (or, for `traverse`, the internal DFS offset), not from the last row that
*survived* a budget trim. So if a token budget truncates a cursor page's entity
array below the rows the scan actually advanced past, following the returned
`cursor` can skip the trimmed-off rows. To page a large scan losslessly, either
resume via the budget ladder's own offset-advancing fetch handle, or re-request
the page with a larger budget so the whole page is retained before advancing the
cursor. Deriving the continuation key from the last *emitted* row after budget
trim is a tracked follow-up.

### When to prefer cursors over offsets

Use a **cursor** whenever you scan a result set that may span multiple pages
while the graph is being written — you need snapshot consistency, no
duplicates/gaps, and flat latency at depth. Offset paging remains available
unchanged for backward compatibility and is fine for small, stable, one-shot
lists where re-planning per page is cheap and concurrent drift is not a
concern.

## Filtering by provenance (Issue #3348)

Issue #3224 lets every version carry write-time provenance (`source`,
`confidence`, `reason`). Issue #3348 makes that metadata **queryable**: the read
tools `get_node`, `list_nodes`, `get_outgoing_edges`, `get_incoming_edges`,
`traverse`, `find_similar`, and `hybrid_query` accept an optional provenance
filter so an agent reasons only over facts whose origin and trust level meet its
bar — no fetch-everything-then-filter in application code.

### The parameters

| Parameter | Type | Meaning |
|-----------|------|---------|
| `provenance_source` | string | Keep facts whose provenance `source` **exactly equals** this value. |
| `provenance_sources` | string[] | Keep facts whose `source` is **any of** these (unioned with `provenance_source`). An empty list is rejected. |
| `min_confidence` | number `[0,1]` | Keep facts whose recorded `confidence` is `>=` this **inclusive** lower bound. |
| `include_unattributed` | bool | When `true`, re-include facts with **no recorded provenance** (default `false` = excluded). |

Semantics:

- **AND across dimensions.** Source and confidence constraints must both hold.
- **Unattributed = no bundle at all.** A version with no provenance is excluded
  whenever a filter is active, unless `include_unattributed: true`. A *partial*
  bundle (present, but missing the queried field) is **attributed** and simply
  fails the constraint on the missing field — `include_unattributed` does not
  rescue it.
- **Per-version, so it composes with time.** The filter is evaluated against the
  provenance recorded on the exact version a read returns, so combining it with
  `as_of_valid_time` / `as_of_transaction_time` filters on provenance **as
  recorded at that coordinate**, not against the latest version.
- **Omitting all four is byte-identical to today.**
- **Invalid values fail closed.** A confidence outside `[0,1]` (or NaN) or an
  empty source list returns a structured `INVALID_ARGUMENT` naming the offending
  field under `details.field` — never a silently-empty result.
- **`find_similar` / `hybrid_query` never under-truncate.** The filter is applied
  to *candidates* (over-fetched up to the vector horizon), so the returned top-k
  are all filter-passing whenever at least k passing candidates exist — ranked
  order is preserved.
- **v1 caveat: cursor paging.** A provenance filter combined with cursor paging
  (`use_cursor` / `cursor`) is rejected with `INVALID_ARGUMENT`; use offset
  paging (`limit`/`offset`) to page a provenance-filtered scan.

### Worked examples

**1. Single source.** Only facts recorded by the `crm-sync` pipeline:

```json
{ "name": "list_nodes",
  "arguments": { "label": "Customer", "provenance_source": "crm-sync" } }
```

**2. Multi-source + confidence (AND).** Facts from either `crm-sync` or
`billing`, with recorded confidence at least 0.8:

```json
{ "name": "find_similar",
  "arguments": {
    "property_name": "embedding",
    "embedding": [0.12, 0.04, ...],
    "k": 10,
    "provenance_sources": ["crm-sync", "billing"],
    "min_confidence": 0.8
  } }
```

All ten returned neighbors are guaranteed to satisfy the filter (candidates are
filtered before the top-k is cut).

**3. Temporal + provenance.** Who did Alice know on 2024-01-01, following only
edges whose recorded confidence was `>= 0.9` **as recorded at that coordinate**:

```json
{ "name": "traverse",
  "arguments": {
    "start_node_id": 42,
    "edge_label": "KNOWS",
    "depth": 2,
    "as_of_valid_time": "2024-01-01T00:00:00Z",
    "min_confidence": 0.9
  } }
```

### On `get_node`

`get_node` returns the node when the filter passes; when the node exists but
does **not** satisfy the filter it returns a `NOT_FOUND` (the node is simply not
in the filtered view) rather than a fabricated result.

### Rust and HTTP surfaces

The same semantics are available to embedders through the surface-agnostic
`aletheiadb::core::ProvenanceFilter` predicate (`ProvenanceFilter::validated(..)`
then `.matches(Option<&Provenance>)`), and on the HTTP `/query` endpoint the
`find_node`, `get_node`, and `find_neighbors` operations accept the same four
keys inline (invalid values → `400` naming the field).

**HTTP paging is refilled, so a short page means end-of-data.** The HTTP
`find_node` / `find_neighbors` responses are bare JSON arrays with no
`has_more` / `next_offset` signal (unlike the MCP tools). When a provenance
filter is active these endpoints **over-fetch and refill**: they keep scanning
forward past the requested `limit` until the page holds `limit` filter-passing
rows or the underlying scan is exhausted. A returned page therefore has up to
`limit` rows whenever that many matches remain, and a **short or empty page
genuinely means end-of-data** — a client may use the standard "short page ⇒
stop" heuristic without under-reading later matches. The refill scan is bounded
by the same `MAX_DEEP_PAGINATION` (10,000) horizon already enforced on
`offset + limit`; in the rare case that horizon is reached before the page
fills, the shorter page is returned (the documented boundary). This preserves
the bare-array response shape exactly. (The MCP tools are unaffected: they
expose `has_more` and advance by a pre-filter page window.)

## Notes

- **AQL has no parameter binding.** Sending `params` with `language: "aql"`
  returns an `invalid_request` error — inline literal values or use Cypher.
- **Feature gating.** When AletheiaDB is built without the `cypher` feature,
  `language: "cypher"` returns `language_unavailable` rather than failing; AQL
  remains available.
- **Result cap.** Large result sets from the `query` tool are capped (default
  100, max 10000); use the `truncated` flag to detect a cap hit. The `query`
  tool is not cursorable in v1 — for consistent, resumable scans use the
  cursor-paged bounded read tools (see
  [Paging large results](#paging-large-results-cursor-continuation)).
