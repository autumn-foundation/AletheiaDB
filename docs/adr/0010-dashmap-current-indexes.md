# ADR-0010: DashMap for Current Indexes

**Status:** Accepted
**Date:** 2024-12-31
**Deciders:** GallifreyDB Core Team
**Categories:** index, concurrency

## Context

The Current Storage layer requires a concurrent hashmap for node and edge lookups. Requirements:

1. **High read concurrency**: Many LLM queries simultaneously
2. **Low read latency**: <1µs for single lookups
3. **Occasional writes**: Updates during knowledge ingestion
4. **No read blocking**: Readers should never wait for writers

Standard library `HashMap` with `RwLock` has limitations:
- Writers block all readers
- Lock contention under high concurrency
- Priority inversion risks

## Decision

We will use **DashMap** for current-state node and edge indexes:

### Usage

```rust
use dashmap::DashMap;

pub struct CurrentIndexes {
    /// O(1) node lookup with lock-free reads
    pub nodes: DashMap<NodeId, Node>,

    /// O(1) edge lookup with lock-free reads
    pub edges: DashMap<EdgeId, Edge>,

    /// Adjacency indexes (separate, using RwLock for bulk rebuilds)
    pub outgoing: Arc<RwLock<AdjacencyIndex>>,
    pub incoming: Arc<RwLock<AdjacencyIndex>>,
}
```

### API Patterns

```rust
impl CurrentIndexes {
    /// Lock-free read
    pub fn get_node(&self, id: NodeId) -> Option<Node> {
        self.nodes.get(&id).map(|r| r.clone())
    }

    /// Fine-grained write lock
    pub fn insert_node(&self, id: NodeId, node: Node) {
        self.nodes.insert(id, node);
    }

    /// Atomic update
    pub fn update_node<F>(&self, id: NodeId, f: F) -> Option<Node>
    where
        F: FnOnce(&mut Node)
    {
        self.nodes.get_mut(&id).map(|mut r| {
            f(&mut r);
            r.clone()
        })
    }

    /// Iteration (snapshot)
    pub fn iter_nodes(&self) -> impl Iterator<Item = (NodeId, Node)> + '_ {
        self.nodes.iter().map(|r| (*r.key(), r.value().clone()))
    }
}
```

### DashMap Characteristics

| Feature | Behavior |
|---------|----------|
| **Read operations** | Lock-free (most cases) |
| **Write operations** | Per-shard locking (fine-grained) |
| **Sharding** | 64 shards by default |
| **Memory overhead** | ~2x HashMap (shard metadata) |
| **Iteration** | Consistent snapshot |

## Consequences

### Positive

- **Lock-free reads**: Readers never block on other readers
- **Fine-grained locking**: Writers only lock affected shard
- **High concurrency**: Excellent scaling with thread count
- **Drop-in replacement**: Similar API to HashMap
- **Production-proven**: Used in many Rust production systems

### Negative

- **External dependency**: Not in standard library
- **Memory overhead**: Higher than plain HashMap
- **Iteration cost**: Creates snapshot (allocates)
- **No true lock-freedom**: Writers still acquire shard locks

### Neutral

- De facto standard for concurrent maps in Rust
- Well-maintained with regular updates
- Familiar API for HashMap users

## Alternatives Considered

### Alternative 1: HashMap + RwLock

```rust
struct Indexes {
    nodes: RwLock<HashMap<NodeId, Node>>,
}
```

**Rejected because:**
- Writers block all readers
- Single lock becomes bottleneck
- High contention under load

### Alternative 2: HashMap + Mutex per Entry

```rust
struct Indexes {
    nodes: HashMap<NodeId, Mutex<Node>>,
}
```

**Rejected because:**
- Entry addition/removal still needs global lock
- High memory overhead (Mutex per entry)
- Complex implementation

### Alternative 3: Lock-Free HashMap (Custom)

Implement a fully lock-free hashmap.

**Rejected because:**
- Significant implementation effort
- Complex correctness proofs
- DashMap is "good enough" for our use case

### Alternative 4: Crossbeam SkipList

Use skip list for ordered concurrent access.

**Rejected because:**
- O(log n) vs O(1) lookup
- No ordering requirement for our indexes
- Higher memory overhead

### Alternative 5: evmap (Eventual Consistency)

Use evmap for fully lock-free reads with eventual consistency.

**Considered for future because:**
- Even better read performance
- More complex write semantics
- Would need careful integration with MVCC

## Implementation Notes

### Thread Safety

DashMap uses sharded locking:
```
Map → [Shard 0][Shard 1][Shard 2]...[Shard 63]
         ↓         ↓         ↓              ↓
      RwLock   RwLock    RwLock         RwLock
```

- Reads acquire read lock on one shard
- Writes acquire write lock on one shard
- Different shards can be accessed concurrently

### Reference Handling

DashMap returns `Ref<K, V>` guards:
```rust
// Guard holds reference to shard
let node_ref: dashmap::mapref::one::Ref<NodeId, Node> = self.nodes.get(&id)?;

// Clone to release guard quickly
let node: Node = node_ref.clone();
drop(node_ref);  // Releases shard reference

// Or use map pattern
self.nodes.get(&id).map(|r| r.clone())
```

### Performance Characteristics

| Operation | Complexity | Typical Latency |
|-----------|------------|-----------------|
| get() | O(1) | ~50ns |
| insert() | O(1) | ~100ns |
| remove() | O(1) | ~100ns |
| len() | O(shards) | ~500ns |
| iter() | O(n) | Snapshot creation |

### Memory Layout

```
DashMap<K, V>:
├─ shards: [Shard; 64]
│   ├─ Shard 0: RwLock<HashMap<K, V>>
│   ├─ Shard 1: RwLock<HashMap<K, V>>
│   └─ ...
└─ hasher: S (hash function)
```

## References

- [DashMap Documentation](https://docs.rs/dashmap/latest/dashmap/)
- [DashMap GitHub](https://github.com/xacrimon/dashmap)
- [Concurrent HashMap Design](https://www.cs.cmu.edu/~yixinluo/15740-F16/p1248-moir.pdf)
- ADR-0001: Hybrid Storage Architecture
- ADR-0005: CSR Adjacency Format
