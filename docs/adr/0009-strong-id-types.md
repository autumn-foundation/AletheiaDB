# ADR-0009: Strong ID Types

**Status:** Accepted
**Date:** 2024-12-31
**Deciders:** GallifreyDB Core Team
**Categories:** core, type-safety

## Context

Graph databases use IDs extensively:
- **NodeId**: Identifies nodes
- **EdgeId**: Identifies edges
- **VersionId**: Identifies specific versions
- **TxId**: Identifies transactions

Using raw `u64` for all IDs creates risks:
- Accidentally passing a NodeId where EdgeId is expected
- Function signatures like `fn get(id: u64)` are ambiguous
- Compiler cannot catch ID type mismatches

This is especially problematic in a temporal database where version IDs are distinct from entity IDs.

## Decision

We will implement **newtype wrappers** for all ID types:

### ID Types

```rust
/// Unique identifier for nodes
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub struct NodeId(pub u64);

/// Unique identifier for edges
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub struct EdgeId(pub u64);

/// Unique identifier for versions
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub struct VersionId(pub u64);

/// Unique identifier for transactions
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub struct TxId(pub u64);

/// Union type for entity IDs (node or edge)
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum EntityId {
    Node(NodeId),
    Edge(EdgeId),
}
```

### ID Generation

```rust
/// Thread-safe atomic ID generator
pub struct IdGenerator {
    counter: AtomicU64,
}

impl IdGenerator {
    pub fn new() -> Self {
        Self { counter: AtomicU64::new(0) }
    }

    pub fn with_start(start: u64) -> Self {
        Self { counter: AtomicU64::new(start) }
    }

    pub fn next_node_id(&self) -> NodeId {
        NodeId(self.counter.fetch_add(1, Ordering::Relaxed))
    }

    pub fn next_edge_id(&self) -> EdgeId {
        EdgeId(self.counter.fetch_add(1, Ordering::Relaxed))
    }

    pub fn next_version_id(&self) -> VersionId {
        VersionId(self.counter.fetch_add(1, Ordering::Relaxed))
    }
}
```

### Type-Safe API

```rust
// Compiler enforces correct ID types
pub trait ReadOps {
    fn get_node(&self, id: NodeId) -> Result<Node>;
    fn get_edge(&self, id: EdgeId) -> Result<Edge>;
    fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId>;
}

// This would be a compile error:
// let node = db.get_node(edge_id);  // Error: expected NodeId, found EdgeId
```

## Consequences

### Positive

- **Compile-time safety**: Compiler catches ID type mismatches
- **Self-documenting**: Function signatures clearly indicate expected ID types
- **Zero runtime cost**: Newtypes compile to the same as u64
- **IDE support**: Better autocomplete and error messages
- **Refactoring safety**: Changing ID usage is caught by compiler

### Negative

- **Verbosity**: Must wrap/unwrap when interfacing with raw u64
- **Serialization**: Need custom serialize/deserialize or derive macros
- **External APIs**: May need conversion at boundaries

### Neutral

- Standard Rust pattern (newtype idiom)
- Common in database implementations
- Minimal learning curve

## Alternatives Considered

### Alternative 1: Raw u64 Everywhere

```rust
fn get_node(id: u64) -> Node;
fn get_edge(id: u64) -> Edge;
```

**Rejected because:**
- No compile-time type safety
- Easy to accidentally use wrong ID type
- Function signatures are ambiguous

### Alternative 2: Generic ID<T>

```rust
struct Id<T>(u64, PhantomData<T>);
type NodeId = Id<Node>;
type EdgeId = Id<Edge>;
```

**Considered but:**
- More complex generic bounds
- Phantom data adds complexity
- Separate types are clearer

### Alternative 3: UUID

Use 128-bit UUIDs for all IDs.

**Rejected because:**
- Larger memory footprint (16 bytes vs 8)
- Slower comparison and hashing
- Overkill for single-node database
- Can be added later for distributed version

### Alternative 4: String IDs

Use string identifiers (like Neo4j element IDs).

**Rejected because:**
- Much larger memory footprint
- Slower comparison
- Allocation overhead
- u64 sufficient for our scale

## Implementation Notes

### Display Implementation

```rust
impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "node:{}", self.0)
    }
}

impl std::fmt::Display for EdgeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "edge:{}", self.0)
    }
}
```

### Conversion Traits

```rust
impl From<u64> for NodeId {
    fn from(id: u64) -> Self { NodeId(id) }
}

impl From<NodeId> for u64 {
    fn from(id: NodeId) -> Self { id.0 }
}
```

### EntityId for Unified Handling

```rust
impl EntityId {
    pub fn as_node(&self) -> Option<NodeId> {
        match self {
            EntityId::Node(id) => Some(*id),
            _ => None,
        }
    }

    pub fn as_edge(&self) -> Option<EdgeId> {
        match self {
            EntityId::Edge(id) => Some(*id),
            _ => None,
        }
    }
}

// Useful for version chains that can be for nodes or edges
pub struct VersionChain {
    entity_id: EntityId,
    versions: Vec<VersionId>,
}
```

### ID Space Considerations

- IDs are generated sequentially from 0
- Different ID types can have overlapping numeric values
- The type distinguishes them (NodeId(1) != EdgeId(1))
- For persistence, store type tag with ID

## References

- [Rust Newtype Pattern](https://doc.rust-lang.org/rust-by-example/generics/new_types.html)
- [Parse, don't validate](https://lexi-lambda.github.io/blog/2019/11/05/parse-don-t-validate/)
- [Type-safe IDs in Rust](https://www.lpalmieri.com/posts/2020-08-31-zero-to-production-3-5-html-forms-databases-integration-tests/#4-3-type-safe-ids)
- ADR-0003: MVCC with Snapshot Isolation (uses TxId)
