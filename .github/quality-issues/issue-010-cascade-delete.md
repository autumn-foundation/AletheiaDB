---
title: "Code Quality: Cascade delete not implemented"
labels: ["code-quality", "automated-scan", "technical-debt", "enhancement"]
---

## Location
`src/storage/current.rs:301`

## Current State
```rust
/// Delete a node.
///
/// Note: This does not delete edges connected to the node.
/// TODO: Add cascade delete option.
pub fn delete_node(&mut self, id: NodeId) -> Result<Node>
```

## Why This is Problematic
- Deleting nodes leaves orphaned edges
- Users must manually delete edges first
- Can lead to referential integrity issues
- Common feature in graph databases

## Suggested Implementation
Add optional cascade parameter:

```rust
pub fn delete_node(&mut self, id: NodeId, cascade: bool) -> Result<Node> {
    if cascade {
        // Delete all connected edges first
        let edges = self.get_edges_for_node(id);
        for edge_id in edges {
            self.delete_edge(edge_id)?;
        }
    }

    // Then delete node
    self.indexes.remove_node(id)
        .ok_or_else(|| StorageError::NodeNotFound(id).into())
}
```

Or provide a separate method:
```rust
pub fn delete_node_cascade(&mut self, id: NodeId) -> Result<(Node, Vec<Edge>)> {
    // Delete edges and return them
    let edges = self.delete_edges_for_node(id)?;
    let node = self.delete_node(id, false)?;
    Ok((node, edges))
}
```

## Impact on Maintainability
- **Medium**: Quality of life improvement
- Improves ergonomics and prevents user errors
- Common pattern in graph databases

## Effort Estimate
**Medium** - Requires adjacency index lookup and careful transaction handling
