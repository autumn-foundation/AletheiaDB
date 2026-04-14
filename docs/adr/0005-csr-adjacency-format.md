# ADR-0005: CSR Adjacency Format

**Status:** Accepted
**Date:** 2024-12-31
**Deciders:** AletheiaDB Core Team
**Categories:** index, performance

## Context

Graph traversal is the core operation for a graph database. The data structure used to store adjacency information significantly impacts:

1. **Cache efficiency**: Memory access patterns affect CPU cache utilization
2. **Traversal speed**: Single-hop traversals must be <1µs target
3. **Memory overhead**: Index size affects overall memory footprint
4. **Update cost**: Writes must not excessively slow down reads

Common adjacency representations:
- **Adjacency List (HashMap)**: O(1) lookup but poor cache locality
- **Adjacency Matrix**: O(1) lookup but O(n²) space
- **CSR (Compressed Sparse Row)**: Sequential access, compact storage

For LLM query patterns, we expect:
- Read-heavy workload (90%+ reads)
- Multi-hop traversals (2-5 hops common)
- Batch updates during knowledge ingestion

## Decision

We will use **Compressed Sparse Row (CSR)** format for adjacency indexes:

### Data Structure

```rust
pub struct AdjacencyIndex {
    /// Offset into edges array for each node
    /// Node i's edges are at edges[offsets[i]..offsets[i+1]]
    offsets: Vec<usize>,

    /// Flat array of adjacency entries, sorted by source node
    edges: Vec<AdjacencyEntry>,

    /// Highest node ID in the index
    max_node_id: u64,
}

pub struct AdjacencyEntry {
    pub target: NodeId,
    pub edge_id: EdgeId,
    pub label: InternedString,
}
```

### Memory Layout

```
offsets: [0, 3, 5, 5, 9, ...]
           │  │  │  │
           │  │  │  └─ Node 3 has edges at [5..9]
           │  │  └─ Node 2 has no edges (5..5 is empty)
           │  └─ Node 1 has edges at [3..5]
           └─ Node 0 has edges at [0..3]

edges: [e0, e1, e2, e3, e4, e5, e6, e7, e8, ...]
        └─ Node 0 ─┘  └ N1 ┘      └─ Node 3 ─┘
```

### Query Pattern

```rust
impl AdjacencyIndex {
    pub fn get_edges(&self, node_id: NodeId) -> &[AdjacencyEntry] {
        let id = node_id.0 as usize;
        if id >= self.offsets.len() - 1 {
            return &[];
        }
        let start = self.offsets[id];
        let end = self.offsets[id + 1];
        &self.edges[start..end]
    }

    pub fn get_edges_with_label(&self, node_id: NodeId, label: InternedString) -> Vec<&AdjacencyEntry> {
        self.get_edges(node_id)
            .iter()
            .filter(|e| e.label == label)
            .collect()
    }
}
```

**Future Optimization**: For high-degree nodes, sorting edges by label within each node's edge list during the build process would enable binary search in `get_edges_with_label`, reducing complexity from O(degree) to O(log(degree) + matches).

### Build Process

```rust
impl AdjacencyIndex {
    pub fn build(edges: &[(NodeId, AdjacencyEntry)]) -> Self {
        // 1. Sort edges by source node
        let mut sorted = edges.to_vec();
        sorted.sort_by_key(|(src, _)| *src);

        // 2. Build offset array
        let max_id = sorted.iter().map(|(src, _)| src.0).max().unwrap_or(0);
        let mut offsets = vec![0; (max_id + 2) as usize];

        for (src, _) in &sorted {
            offsets[src.0 as usize + 1] += 1;
        }

        // Convert counts to cumulative offsets
        for i in 1..offsets.len() {
            offsets[i] += offsets[i - 1];
        }

        // 3. Extract edges array
        let edges: Vec<_> = sorted.into_iter().map(|(_, e)| e).collect();

        AdjacencyIndex { offsets, edges, max_node_id: max_id }
    }
}
```

## Consequences

### Positive

- **Cache-friendly**: Sequential memory access for traversals
- **Compact storage**: ~16 bytes per edge (target + edge_id + label)
- **O(k) traversal**: Returns k edges in O(k) time with no hash lookups
- **Read-optimized**: Perfect for read-heavy LLM query workload
- **Predictable performance**: No hash collisions, consistent timing

### Negative

- **Static structure**: Must rebuild for updates (not incremental)
- **Rebuild cost**: O(E log E) to rebuild entire index
- **Sparse node IDs**: Wastes space if node IDs have large gaps
- **Write latency**: Updates require full rebuild

### Neutral

- Standard format in graph processing (GraphBLAS, graph analytics)
- Well-understood in high-performance computing
- Trade-off favors reads over writes (matches our workload)

## Alternatives Considered

### Alternative 1: HashMap-based Adjacency List

```rust
HashMap<NodeId, Vec<AdjacencyEntry>>
```

**Rejected because:**
- Hash lookup overhead per node
- Poor cache locality (pointer chasing)
- Higher memory overhead (hash table + Vec headers)

### Alternative 2: BTreeMap Adjacency

```rust
BTreeMap<NodeId, Vec<AdjacencyEntry>>
```

**Rejected because:**
- Log(n) lookup vs O(1) offset lookup
- Tree traversal has cache misses
- More complex than CSR

### Alternative 3: Adjacency Matrix (Sparse)

Store edges in sparse matrix format.

**Rejected because:**
- Better for dense graphs
- Our graphs are typically sparse
- Higher memory for sparse adjacency

### Alternative 4: Delta-Based Updates

Allow incremental updates to CSR.

**Considered for future because:**
- Could reduce rebuild frequency
- More complex implementation
- Current rebuild-on-commit is fast enough

## Implementation Notes

### Dual Indexes

We maintain two CSR indexes:
- **Outgoing**: Indexed by source node
- **Incoming**: Indexed by target node

```rust
pub struct CurrentIndexes {
    nodes: DashMap<NodeId, Node>,
    edges: DashMap<EdgeId, Edge>,
    outgoing: Arc<RwLock<AdjacencyIndex>>,  // source → targets
    incoming: Arc<RwLock<AdjacencyIndex>>,  // target → sources
}
```

### Rebuild Strategy

CSR indexes are rebuilt at transaction commit:

```rust
impl WriteTransaction {
    fn commit(&mut self) -> Result<()> {
        // ... apply changes ...

        // Rebuild adjacency indexes (batched)
        self.rebuild_adjacency_indexes()?;

        // ... finalize commit ...
    }
}
```

### Performance Targets

| Operation | Target | Rationale |
|-----------|--------|-----------|
| Single-hop traversal | <100ns | Direct slice access |
| Multi-hop (3 hops) | <100µs | ~30 edges × 3 hops |
| Index rebuild (10k edges) | <10ms | O(E log E) sort |

### Memory Overhead

```
Per edge: 8 (target) + 8 (edge_id) + 4 (label) = 20 bytes
Offsets: 8 bytes per node
Total: 20E + 8N bytes (E=edges, N=nodes)
```

## References

- [Compressed Sparse Row Format](https://en.wikipedia.org/wiki/Sparse_matrix#Compressed_sparse_row_(CSR,_CRS_or_Yale_format))
- [GraphBLAS](https://graphblas.org/)
- [Ligra: A Lightweight Graph Processing Framework](https://people.csail.mit.edu/jshun/ligra.pdf)
- ADR-0001: Hybrid Storage Architecture
- ADR-0010: DashMap for Current Indexes
