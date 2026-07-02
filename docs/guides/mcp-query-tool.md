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
             "message": "The `query` tool is read-only; the `CREATE` clause ..." } }
```

`kind` is one of: `invalid_request`, `read_only_violation`,
`language_unavailable`, `parse_error`, `unsupported_construct`,
`invalid_params`, `runtime_error`.

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
(`create_node`, `create_edge`, `update_node`, `update_edge`) accept an
optional `valid_time` field so an LLM ingesting a document can record the
fact's real-world effective date in the **same** tool call, instead of
defaulting to "now". This closes the loop with the bi-temporal `AS OF`
queries above: without it, every LLM-ingested fact would collapse to "valid
as of now" and `AS OF VALID_TIME` reads over that data would be wrong.

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
// -> { "error": "..." }  (not yet true at this valid time)
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
{ "error": "Invalid valid_time: Invalid timestamp format: 'not-a-timestamp'. ..." }
{ "error": "valid_from ... is too far in future (current: ..., max offset: ...)" }
{ "error": "valid_from ... is before entity creation time ... for node:7" }
```

## Notes

- **AQL has no parameter binding.** Sending `params` with `language: "aql"`
  returns an `invalid_request` error — inline literal values or use Cypher.
- **Feature gating.** When AletheiaDB is built without the `cypher` feature,
  `language: "cypher"` returns `language_unavailable` rather than failing; AQL
  remains available.
- **Result cap.** Large result sets are capped (default 100, max 10000); use the
  `truncated` flag to detect a cap hit. A streaming/cursor protocol is future
  work.
