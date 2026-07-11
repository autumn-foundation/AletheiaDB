# Core Concepts

This page explains the fundamental ideas behind AletheiaDB. You don't need to
read this before writing code, but it will make the rest of the docs click.

---

## What AletheiaDB Is

AletheiaDB is a **bi-temporal graph database**. Two terms to unpack:

- **Graph database**: Data is stored as nodes (entities) and edges (relationships
  between entities), rather than tables or documents.
- **Bi-temporal**: Every piece of data carries two independent time dimensions,
  so you can always answer "what was true, and when did we know it?"

The combination lets you model knowledge that changes over time — and then
query that knowledge as it existed at any point in the past.

---

## Nodes and Edges

A **node** is an entity: a person, a document, a concept, an event. Each node has:
- A **label** — the type of entity (`"Person"`, `"Document"`, `"Event"`)
- **Properties** — key/value pairs (`"name" => "Alice"`, `"age" => 30`)
- A **NodeId** — an opaque identifier assigned on creation

An **edge** is a relationship between two nodes. Each edge has:
- A **source** node and a **target** node
- A **relationship type** — what kind of relationship (`"KNOWS"`, `"AUTHORED"`, `"DEPENDS_ON"`)
- **Properties** — key/value pairs on the relationship itself
- An **EdgeId**

```
(Alice:Person) --[KNOWS since:2023]--> (Bob:Person)
```

Edges are directed. To model a bidirectional relationship, create two edges or
traverse with `direction: Both`.

---

## The Bi-Temporal Model

Most databases know one time: "what is stored right now." AletheiaDB tracks two:

### Valid Time

**When the fact was true in the real world.**

Example: Alice joined the company on January 1, 2024. That date is the valid time
for the fact "Alice is an employee" — regardless of when you entered it into the system.

### Transaction Time

**When the fact was recorded in the database.**

Example: You entered Alice's employment into the system on January 5, 2024. That
is the transaction time.

### Why Both?

Consider: on March 1, 2024, you discover that Alice actually joined on December 15,
2023 — two weeks earlier than recorded. You correct the record. Now:

- The **valid time** for Alice's employment is December 15, 2023.
- There are **two transaction times**:
  - January 5, 2024: we thought she started January 1.
  - March 1, 2024: we corrected it to December 15.

With bi-temporal storage, you can answer:
- "What did we know about Alice on February 1?" → The original (incorrect) record.
- "What is actually true about Alice as of December 15?" → She was an employee.
- "What does the database show right now, as of today?" → The corrected record.

An ordinary database can only answer the last question.

### Time-Travel Queries

```rust
// "What did Alice look like at this exact moment in time?"
let historical = db.get_node_at_time(
    alice_id,
    valid_time,       // when was it true in the world?
    transaction_time, // what did the DB know at that moment?
)?;
```

Both times default to "now" if you want the current view.

### Recording Facts at a Specific Valid Time

By default, `create_node`/`create_edge`/`update_node`/`update_edge` set valid
time to "now" — the moment the transaction runs. But an LLM extracting facts
from a document is usually recording something that became true **in the
past** (or, less commonly, that won't become true until a **future** date).
Use the `_with_valid_time` variants to assert the real-world effective date
directly, without hand-writing a transaction:

```rust
use aletheiadb::{AletheiaDB, properties};
use aletheiadb::core::temporal::time;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
# let db = AletheiaDB::new()?;
// An LLM reads a document today and extracts: "Alice became CEO on 2021-03-01".
let march_2021 = time::from_secs(1614556800);

let alice = db.create_node_with_valid_time(
    "Person",
    properties! { "name" => "Alice", "title" => "CEO" },
    Some(march_2021),
)?;

// The fact is retrievable as of its stated valid time...
let as_of_2021 = db.get_node_at_valid_time(alice, march_2021)?;
assert_eq!(as_of_2021.properties.get("title").unwrap().as_str(), Some("CEO"));

// ...and invisible before it, even though we recorded it just now.
let before = time::from_secs(1609459200); // 2021-01-01
assert!(db.get_node_at_valid_time(alice, before).is_err());
# Ok(())
# }
```

Passing `None` (or calling the plain `create_node`/`create_edge`) behaves
exactly as before — this is purely additive. **Transaction time is always
system-assigned** to the commit time; there is no way for a caller to forge
when a fact was actually recorded, which preserves provenance integrity. A
`valid_from` that is malformed, more than a year in the future, or precedes
an entity's own creation time is rejected with a typed `TemporalError`,
never silently coerced.

The same `_with_valid_time` methods are available on `update_node`/`update_edge`
(to backdate a correction) and `delete_node`/`delete_edge` (to backdate when a
fact stopped being true). See the [MCP query tool guide](mcp-query-tool.md#recording-facts-at-a-specific-valid-time)
for the equivalent MCP tool usage.

---

## Storage Architecture

AletheiaDB uses a **hybrid storage** model to keep current-state queries fast
while still supporting full temporal history.

```
┌──────────────────────────────────────────────────────┐
│                    Query Engine                       │
└───────────────────────┬──────────────────────────────┘
                        │
          ┌─────────────┴─────────────┐
          ▼                           ▼
┌──────────────────┐       ┌────────────────────┐
│  Current Storage │       │ Historical Storage  │
│  (hot path)      │       │ (temporal path)     │
│  ~22ns lookup    │       │ anchor+delta        │
│  No overhead     │       │ compressed versions │
└──────────────────┘       └────────────────────┘
```

**Current Storage** holds only the latest version of each node and edge. Queries
for "what is true now" never touch the temporal layer — they're as fast as a
regular graph database.

**Historical Storage** keeps all past versions using **anchor+delta compression**:
a full snapshot (anchor) plus incremental changes (deltas). This gives 5–6×
storage savings while keeping time-travel queries fast.

When you need to go further back in history than RAM holds, **Tiered Storage**
extends this with warm (LRU cache) and cold (disk-backed Redb) layers. See
[Tiered Storage Guide](tiered-storage-guide.md).

---

## Transactions

Every write to AletheiaDB is wrapped in a transaction:

```rust
// Implicit transaction (single operation)
let id = db.create_node("Person", properties! { "name" => "Alice" })?;

// Explicit transaction (multiple operations, atomic)
db.write(|tx| {
    let a = tx.create_node("Person", properties! { "name" => "Alice" })?;
    let b = tx.create_node("Person", properties! { "name" => "Bob" })?;
    tx.create_edge(a, b, "KNOWS", properties! {})?;
    Ok(())
})?;
```

AletheiaDB provides **ACID transactions** with snapshot isolation and write
conflict detection. If any operation inside `write(|tx| ...)` fails, none of
them are committed.

---

## The Write-Ahead Log (WAL)

Durability is provided by the **Write-Ahead Log**: before any change is applied
to storage, it's written to an append-only log on disk. If the process crashes,
the log is replayed on startup to restore the last committed state.

Three durability modes trade off latency vs. throughput vs. safety:

| Mode | Latency | Throughput | Safety |
|------|---------|------------|--------|
| `Synchronous` | ~1.5ms | ~600/sec | Full ACID, fsync per commit |
| `GroupCommit` | ~10–50ms | ~100K+/sec | Full ACID, batched fsync |
| `Async` | <100ns | ~500K+/sec | Eventual (data may be lost on crash) |

For most use cases, `GroupCommit` is the right default. See [WAL docs](../WAL.md).

---

## Vector Search

Nodes can carry dense vector embeddings as properties. AletheiaDB indexes these
with **HNSW** (Hierarchical Navigable Small World graphs) for sub-millisecond
approximate nearest-neighbor search.

```
Node: { "title": "Intro to Rust", "embedding": [0.1, 0.4, ..., 0.9] }
                                                     ↑
                                         HNSW index on "embedding"
```

You can have multiple vector properties per database, each with its own index
and distance metric (cosine, Euclidean, dot product). Embeddings are fully
versioned — you can retrieve what an embedding looked like at any point in time.

See [Vector Search Integration](vector-search-integration.md).

---

## Hybrid Queries

The query builder combines all three dimensions:

```rust
db.query()
    .as_of(valid_time, tx_time)          // temporal: point-in-time snapshot
    .start(alice_id)                     // graph: starting node
    .traverse("KNOWS")                   // graph: follow edges
    .rank_by_similarity(&embedding, 10)  // vector: re-rank by similarity
    .execute(&db)?
```

This is the core value proposition: a single query that reasons about graph
structure, semantic meaning, and historical time simultaneously. No joins across
systems, no separate vector store, no ETL.

See [Hybrid Query Guide](hybrid-query-guide.md).

---

## Key Terms at a Glance

| Term | Meaning |
|------|---------|
| **Valid time** | When the fact was true in reality |
| **Transaction time** | When the fact was recorded in the database |
| **Bi-temporal** | Tracking both time dimensions independently |
| **Node** | An entity (has label + properties + NodeId) |
| **Edge** | A relationship between two nodes (has type + properties + EdgeId) |
| **Anchor** | A full historical snapshot of a node/edge |
| **Delta** | An incremental change on top of an anchor |
| **HNSW** | The graph index structure used for k-NN vector search |
| **WAL** | Write-ahead log; provides crash recovery and durability |
| **Hot tier** | Current state in RAM (~22ns lookup) |
| **Cold tier** | Historical versions on disk (<1ms lookup) |

---

## Next Steps

- **Write code** → [Getting Started](getting-started.md)
- **Configure persistence** → [Persistence Guide](PERSISTENCE.md)
- **Deep dive on time-travel** → [Tiered Storage Guide](tiered-storage-guide.md)
- **Semantic search** → [Vector Search Integration](vector-search-integration.md)
- **System design** → [Architecture](../ARCHITECTURE.md)
