# Security: Missing Cascade Delete for Nodes - Referential Integrity Violation

**Labels**: `security`, `automated-scan`, `medium`, `P2`, `referential-integrity`, `data-consistency`
**Priority**: P2 - Medium priority

## Summary
Deleting a node does not automatically delete connected edges, potentially leaving orphaned edges that reference non-existent nodes. This violates referential integrity and can corrupt the graph structure.

## Location
- **File**: `src/storage/current.rs`
- **Line**: 301-302
- **Function**: `CurrentStorage::delete_node()`

## Code
```rust
/// Note: This does not delete edges connected to the node.
/// TODO: Add cascade delete option.
pub fn delete_node(&mut self, id: NodeId) -> Result<Node> {
    self.indexes
        .node_by_id
        .remove(&id)
        .ok_or_else(|| StorageError::NotFound { ... })
}
```

## Severity
**MEDIUM**

## Impact
- **Referential Integrity**: Edges point to deleted nodes
- **Dangling References**: Edge queries return invalid node IDs
- **Graph Corruption**: Traversals fail on dangling edges
- **Storage Leak**: Orphaned edges waste space permanently
- **Query Errors**: `get_node(edge.source)` returns NotFound
- **Data Inconsistency**: Graph is no longer a valid graph structure

## Attack Scenario

### Scenario 1: Orphaned Edge Attack
1. User creates Alice (node ID 1) and Bob (node ID 2)
2. User creates edge: `Alice --[KNOWS]--> Bob` (edge ID 100)
3. User deletes Alice (node ID 1)
4. **Bug**: Edge 100 still exists with `source = 1` (deleted node)
5. Query `get_edge(100)` returns edge with source=1
6. Query `get_node(1)` returns NotFound
7. Traversal from Bob's incoming edges fails

**Result**: Database contains invalid graph structure.

### Scenario 2: Storage Leak
1. Application creates 1,000,000 nodes with edges
2. Application deletes all nodes (expecting cleanup)
3. **Bug**: All edges remain in storage
4. Disk usage doesn't decrease
5. Orphaned edges accumulate over time
6. Eventually: out of disk space

**Result**: Memory/disk leak, denial of service.

### Scenario 3: Temporal Inconsistency
1. Node exists at time T1
2. Node deleted at time T2
3. Edges still reference node
4. Historical query at T1: "get neighbors of node" returns edges
5. Historical query at T2: node doesn't exist but edges do

**Result**: Temporal queries return inconsistent data.

## Expected Behavior

### Option 1: Restrict (SQL-style RESTRICT)
Prevent deletion if node has edges:

```rust
pub fn delete_node(&mut self, id: NodeId) -> Result<Node> {
    // Check for connected edges
    if self.has_edges(id) {
        return Err(StorageError::ReferentialIntegrityViolation {
            entity: "node",
            id: id.as_u64(),
            reason: "Cannot delete node with connected edges. Delete edges first.",
        });
    }
    // Safe to delete
    self.delete_node_unchecked(id)
}
```

### Option 2: Cascade (SQL-style CASCADE)
Automatically delete connected edges:

```rust
pub fn delete_node_cascade(&mut self, id: NodeId) -> Result<Node> {
    // 1. Find all connected edges
    let connected_edges = self.find_edges_for_node(id);

    // 2. Delete all edges first
    for edge_id in connected_edges {
        self.delete_edge(edge_id)?;
    }

    // 3. Delete the node
    self.delete_node_unchecked(id)
}
```

### Option 3: Configurable Behavior (Recommended)
Let user choose:

```rust
pub enum DeleteBehavior {
    /// Fail if node has connected edges (safest)
    Restrict,
    /// Delete node and all connected edges
    Cascade,
    /// Delete node, leave edges (current behavior, UNSAFE)
    OrphanEdges,
}

pub fn delete_node_with_behavior(
    &mut self,
    id: NodeId,
    behavior: DeleteBehavior,
) -> Result<DeletedNode> {
    match behavior {
        DeleteBehavior::Restrict => {
            if self.has_edges(id)? {
                return Err(StorageError::ReferentialIntegrityViolation {
                    entity: "node",
                    id: id.as_u64(),
                    reason: format!(
                        "Cannot delete node with {} connected edges",
                        self.count_edges(id)?
                    ),
                });
            }
            self.delete_node_unchecked(id)
        }
        DeleteBehavior::Cascade => {
            let edges = self.find_edges_for_node(id)?;
            for edge_id in edges {
                self.delete_edge(edge_id)?;
            }
            self.delete_node_unchecked(id)
        }
        DeleteBehavior::OrphanEdges => {
            // Current behavior - UNSAFE, kept for compatibility
            log::warn!("Deleting node {} with OrphanEdges - may cause dangling references", id);
            self.delete_node_unchecked(id)
        }
    }
}

/// Return type includes deleted edges for cascade
pub struct DeletedNode {
    pub node: Node,
    pub deleted_edges: Vec<EdgeId>,
}
```

## Recommended Fix (Option 3 - Configurable)

### Implementation

#### Step 1: Add DeleteBehavior Enum
```rust
// In src/storage/current.rs or src/core/graph.rs

/// Behavior when deleting a node with connected edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteBehavior {
    /// Fail if node has connected edges (SQL RESTRICT).
    /// Safest option - prevents accidental data loss.
    Restrict,

    /// Delete node and all connected edges (SQL CASCADE).
    /// Convenient but irreversible.
    Cascade,

    /// Delete node, leave edges (UNSAFE - for backward compatibility only).
    /// ⚠️ Creates dangling references. Use only if you know what you're doing.
    #[deprecated(note = "Use Restrict or Cascade instead")]
    OrphanEdges,
}
```

#### Step 2: Implement Helper Functions
```rust
impl CurrentStorage {
    /// Check if node has any connected edges (incoming or outgoing).
    fn has_edges(&self, id: NodeId) -> Result<bool> {
        let outgoing = self.indexes.adjacency.get_neighbors(id, Direction::Outgoing);
        let incoming = self.indexes.adjacency.get_neighbors(id, Direction::Incoming);
        Ok(!outgoing.is_empty() || !incoming.is_empty())
    }

    /// Count connected edges for error messages.
    fn count_edges(&self, id: NodeId) -> Result<usize> {
        let outgoing = self.indexes.adjacency.get_neighbors(id, Direction::Outgoing).len();
        let incoming = self.indexes.adjacency.get_neighbors(id, Direction::Incoming).len();
        Ok(outgoing + incoming)
    }

    /// Find all edges connected to node (for cascade delete).
    fn find_edges_for_node(&self, id: NodeId) -> Result<Vec<EdgeId>> {
        let mut edges = Vec::new();

        // Outgoing edges
        let outgoing = self.indexes.adjacency.get_neighbors(id, Direction::Outgoing);
        edges.extend(outgoing);

        // Incoming edges
        let incoming = self.indexes.adjacency.get_neighbors(id, Direction::Incoming);
        edges.extend(incoming);

        Ok(edges)
    }
}
```

#### Step 3: Update delete_node() API
```rust
/// Delete a node with specified behavior for connected edges.
///
/// # Arguments
/// * `id` - Node to delete
/// * `behavior` - How to handle connected edges
///
/// # Returns
/// * `Ok(DeletedNode)` - Deleted node and any cascade-deleted edges
/// * `Err(ReferentialIntegrityViolation)` - If Restrict mode and node has edges
pub fn delete_node_with_behavior(
    &mut self,
    id: NodeId,
    behavior: DeleteBehavior,
) -> Result<DeletedNode> {
    match behavior {
        DeleteBehavior::Restrict => {
            let edge_count = self.count_edges(id)?;
            if edge_count > 0 {
                return Err(StorageError::ReferentialIntegrityViolation {
                    entity: "node",
                    id: id.as_u64(),
                    reason: format!(
                        "Cannot delete node with {} connected edges. \
                         Delete edges first or use DeleteBehavior::Cascade",
                        edge_count
                    ),
                }.into());
            }
            let node = self.delete_node_unchecked(id)?;
            Ok(DeletedNode {
                node,
                deleted_edges: vec![],
            })
        }
        DeleteBehavior::Cascade => {
            let edge_ids = self.find_edges_for_node(id)?;
            for edge_id in &edge_ids {
                self.delete_edge(*edge_id)?;
            }
            let node = self.delete_node_unchecked(id)?;
            Ok(DeletedNode {
                node,
                deleted_edges: edge_ids,
            })
        }
        #[allow(deprecated)]
        DeleteBehavior::OrphanEdges => {
            // UNSAFE: Leaves dangling edges
            log::warn!(
                "Deleting node {} with OrphanEdges behavior - this may create dangling references",
                id
            );
            let node = self.delete_node_unchecked(id)?;
            Ok(DeletedNode {
                node,
                deleted_edges: vec![],
            })
        }
    }
}

/// Delete a node (defaults to Restrict behavior).
///
/// This is a safe default that prevents accidental data corruption.
/// Use `delete_node_with_behavior()` for other behaviors.
pub fn delete_node(&mut self, id: NodeId) -> Result<Node> {
    self.delete_node_with_behavior(id, DeleteBehavior::Restrict)
        .map(|deleted| deleted.node)
}
```

#### Step 4: Add New Error Variant
```rust
// In src/utils/error.rs

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    // ... existing variants

    /// Referential integrity violation
    #[error("Referential integrity violation for {entity} {id}: {reason}")]
    ReferentialIntegrityViolation {
        entity: &'static str,
        id: u64,
        reason: String,
    },
}
```

### API Comparison

| Approach | Safety | Breaking Change | Edge Cases |
|----------|--------|----------------|------------|
| **Option 1** (Restrict only) | ✅ Safest | ⚠️ Yes | Must manually delete edges first |
| **Option 2** (Cascade only) | ⚠️ Moderate | ⚠️ Yes | May delete more than intended |
| **Option 3** (Configurable) | ✅ Safe | ❌ No | Flexible, non-breaking |

**Recommendation**: **Option 3** (configurable) with **Restrict as default**.

## Testing Requirements

### Test 1: Restrict Behavior
```rust
#[test]
fn test_delete_node_restrict_with_edges() {
    let mut storage = CurrentStorage::new();
    let alice = storage.create_node("Person", props).unwrap();
    let bob = storage.create_node("Person", props).unwrap();
    let edge = storage.create_edge(alice, bob, "KNOWS", props).unwrap();

    // Should fail - node has edges
    let result = storage.delete_node(alice);
    assert!(matches!(
        result,
        Err(Error::Storage(StorageError::ReferentialIntegrityViolation { .. }))
    ));

    // Node should still exist
    assert!(storage.get_node(alice).is_ok());
}
```

### Test 2: Cascade Behavior
```rust
#[test]
fn test_delete_node_cascade() {
    let mut storage = CurrentStorage::new();
    let alice = storage.create_node("Person", props).unwrap();
    let bob = storage.create_node("Person", props).unwrap();
    let edge = storage.create_edge(alice, bob, "KNOWS", props).unwrap();

    // Should succeed and delete edge
    let deleted = storage.delete_node_with_behavior(alice, DeleteBehavior::Cascade).unwrap();
    assert_eq!(deleted.deleted_edges.len(), 1);
    assert_eq!(deleted.deleted_edges[0], edge);

    // Node and edge should be gone
    assert!(storage.get_node(alice).is_err());
    assert!(storage.get_edge(edge).is_err());
}
```

### Test 3: Self-Loop
```rust
#[test]
fn test_delete_node_with_self_loop() {
    let mut storage = CurrentStorage::new();
    let alice = storage.create_node("Person", props).unwrap();
    let self_edge = storage.create_edge(alice, alice, "LIKES_SELF", props).unwrap();

    // Cascade should delete self-loop
    let deleted = storage.delete_node_with_behavior(alice, DeleteBehavior::Cascade).unwrap();
    assert_eq!(deleted.deleted_edges.len(), 1);
}
```

### Test 4: Bidirectional Edges
```rust
#[test]
fn test_delete_node_bidirectional_edges() {
    let mut storage = CurrentStorage::new();
    let alice = storage.create_node("Person", props).unwrap();
    let bob = storage.create_node("Person", props).unwrap();

    let edge1 = storage.create_edge(alice, bob, "KNOWS", props).unwrap();
    let edge2 = storage.create_edge(bob, alice, "KNOWS", props).unwrap();

    // Cascade should delete both edges
    let deleted = storage.delete_node_with_behavior(alice, DeleteBehavior::Cascade).unwrap();
    assert_eq!(deleted.deleted_edges.len(), 2);
    assert!(deleted.deleted_edges.contains(&edge1));
    assert!(deleted.deleted_edges.contains(&edge2));
}
```

### Test 5: Temporal Consistency
```rust
#[test]
fn test_delete_node_temporal_consistency() {
    let mut db = GallifreyDB::new();
    let alice = db.create_node("Person", props).unwrap();
    let t1 = db.current_time();

    let bob = db.create_node("Person", props).unwrap();
    let edge = db.create_edge(alice, bob, "KNOWS", props).unwrap();
    let t2 = db.current_time();

    // Delete Alice at t3 (cascade)
    db.delete_node_with_behavior(alice, DeleteBehavior::Cascade).unwrap();
    let t3 = db.current_time();

    // Temporal queries
    assert!(db.get_node_at_time(alice, t1).is_ok()); // Existed at t1
    assert!(db.get_node_at_time(alice, t2).is_ok()); // Existed at t2
    assert!(db.get_node_at_time(alice, t3).is_err()); // Deleted at t3

    assert!(db.get_edge_at_time(edge, t2).is_ok()); // Edge existed at t2
    assert!(db.get_edge_at_time(edge, t3).is_err()); // Edge deleted at t3
}
```

## Migration Guide

### For Existing Code (Non-Breaking)
```rust
// Old code continues to work (defaults to Restrict)
db.delete_node(node_id)?;

// Explicitly use cascade if needed
db.delete_node_with_behavior(node_id, DeleteBehavior::Cascade)?;
```

### For New Code
```rust
// Recommended: Explicit behavior
match db.delete_node_with_behavior(node_id, DeleteBehavior::Restrict) {
    Ok(deleted) => println!("Deleted node {}", deleted.node.id),
    Err(StorageError::ReferentialIntegrityViolation { .. }) => {
        println!("Cannot delete: node has edges. Use Cascade or delete edges first.");
    }
    Err(e) => return Err(e),
}
```

## Related Issues
- Similar issue for `delete_edge()` - should check if edge is referenced elsewhere?
- Temporal delete consistency (issue for Phase 3)

## Priority
**P2 - Medium priority**

Should be fixed before 1.0 release. Referential integrity is critical for data consistency.

## Estimated Effort
- Implementation: 2-3 days
- Testing: 2 days (especially temporal consistency)
- Documentation: 1 day
- Total: ~1 week
