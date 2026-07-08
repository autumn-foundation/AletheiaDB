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

Codes may be **added** over time; existing codes never change. Treat an
unrecognized code as non-retriable.

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

**Coverage caveat (cold storage + restarts).** Bounds cover all history
recorded during the **current process lifetime**, plus the hot-tier history
restored at startup. On databases with cold-storage migration enabled,
versions migrated to the cold tier *before the last restart* are **not**
reflected — the temporal indexes (and their extent aggregate) are rebuilt at
startup from hot-tier versions only. Within a single process lifetime,
cold-tier migration never shrinks the bounds. The per-label breakdown is
additionally hot-tier-only even within a process lifetime: after cold
migration a label's bounds may be narrower than the overall bounds, or a
label may be absent entirely. A follow-up could persist cold-tier bounds
metadata to close the restart gap.

**Calibration pattern:** if `temporal_extent` reports
`valid_time.earliest = 2021-03-01`, an `AS OF '2019-01-01'` query is
guaranteed to be out of recorded range — its empty result means "before our
records begin," not "nothing existed." Rerun the query at an instant inside
the extent (or report the range mismatch) instead of concluding absence.

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
  reports `is_current: false` with its closed bounds. The valid-time
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

## Notes

- **AQL has no parameter binding.** Sending `params` with `language: "aql"`
  returns an `invalid_request` error — inline literal values or use Cypher.
- **Feature gating.** When AletheiaDB is built without the `cypher` feature,
  `language: "cypher"` returns `language_unavailable` rather than failing; AQL
  remains available.
- **Result cap.** Large result sets are capped (default 100, max 10000); use the
  `truncated` flag to detect a cap hit. A streaming/cursor protocol is future
  work.
