# AletheiaDB Python SDK

Native Python bindings for [AletheiaDB](https://github.com/autumn-foundation/AletheiaDB) — a high-performance **bi-temporal graph database** built in Rust, designed for LLM integration. Track both *valid time* (when facts were true in reality) and *transaction time* (when they were recorded), with first-class vector search and Cypher queries.

## Install

```bash
pip install aletheiadb
```

Pre-built wheels are published for Linux (x86_64, aarch64), macOS (x86_64, arm64), and Windows (x86_64) on Python 3.9 – 3.13. No Rust toolchain required.

## Quickstart

```python
from aletheiadb import AletheiaDB

db = AletheiaDB()

alice = db.create_node("Person", {"name": "Alice", "age": 30})
bob   = db.create_node("Person", {"name": "Bob"})
db.create_edge(alice, bob, "KNOWS", {"since": 2024})

# Read current state
node = db.get_node(alice)
print(node.label, node.properties)        # Person {'name': 'Alice', 'age': 30}

# Update properties (creates a new bi-temporal version under the hood)
db.update_node(alice, {"name": "Alice", "age": 31})

# Time-travel: get a node as it existed at a specific point in time
historical = db.get_node_at_time(alice, valid_time="2026-01-01T00:00:00Z")

# Inspect the full version history
for v in db.node_history(alice):
    print(v["version_number"], v["properties"])
```

## Graph traversal

```python
# Single-hop adjacency
db.outgoing_edges(alice, label="KNOWS")    # [edge_id, ...]
db.incoming_edges(bob)

# Multi-hop BFS
db.traverse(start=alice, label="KNOWS", max_depth=3, direction="out")
```

## Vector search

```python
from aletheiadb import HnswConfig, DistanceMetric, enable_vector_index, find_similar

enable_vector_index(db, "embedding", HnswConfig(384, DistanceMetric.COSINE))

# Embeddings come in as plain Python lists
doc1 = db.create_node("Document", {"title": "Rust", "embedding": [0.1] * 384})
doc2 = db.create_node("Document", {"title": "Python", "embedding": [0.2] * 384})

results = find_similar(db, query_node_id=doc1, k=10, label="Document")
# -> [(node_id, score), ...] sorted by similarity desc
```

## Cypher queries

```python
rows = db.execute_cypher(
    "MATCH (n:Person {name: $name})-[:KNOWS]->(friend) RETURN friend",
    params={"name": "Alice"},
)
for row in rows:
    print(row["kind"], row["entity"])
```

## API

| Area | Functions |
|---|---|
| Lifecycle | `AletheiaDB()`, `AletheiaDB.open(config_path)` |
| Nodes | `create_node`, `get_node`, `update_node`, `delete_node`, `delete_node_cascade`, `count_nodes`, `list_nodes`, `nodes_by_label` |
| Edges | `create_edge`, `get_edge`, `update_edge`, `delete_edge`, `count_edges` |
| Adjacency | `outgoing_edges`, `incoming_edges`, `out_degree`, `in_degree`, `traverse` |
| Temporal | `get_node_at_time`, `get_edge_at_time`, `node_history` |
| Vector | `enable_vector_index`, `find_similar`, `find_similar_by_vector` |
| Query | `execute_cypher`, `execute_aql` |

Timestamps accept `datetime`, ISO-8601 strings, integer microseconds since the Unix epoch, or `None` (which means *now*).

## License

MIT OR Apache-2.0
