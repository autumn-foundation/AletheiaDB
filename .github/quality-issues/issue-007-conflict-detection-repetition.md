---
title: "Code Quality: detect_conflicts() has repetitive logic (~106 lines)"
labels: ["code-quality", "automated-scan", "refactoring"]
---

## Location
`src/api/transaction/write_tx.rs:288-394`

## Why This is Problematic
- Match statement with 4 nearly identical branches
- Each branch checks "exists" and "modified after snapshot"
- Could be consolidated with helper function
- Repetitive error construction

## Suggested Improvement

```rust
fn detect_conflicts(&self) -> Result<()> {
    for op in &self.buffer.operations {
        match &op.op_type {
            UpdateNode(_) => self.check_node_conflict(op.id, "update")?,
            DeleteNode => self.check_node_conflict(op.id, "delete")?,
            UpdateEdge(_) => self.check_edge_conflict(op.id, "update")?,
            DeleteEdge => self.check_edge_conflict(op.id, "delete")?,
            _ => {} // No conflicts for creates
        }
    }
    Ok(())
}

fn check_node_conflict(&self, id: NodeId, operation: &str) -> Result<()> {
    let node = self.current.get_node(id)?;
    if node.modified_after(self.snapshot.timestamp) {
        return Err(TransactionError::Conflict {
            entity: format!("Node {}", id),
            operation: operation.to_string(),
        }.into());
    }
    Ok(())
}

fn check_edge_conflict(&self, id: EdgeId, operation: &str) -> Result<()> {
    // Similar for edges
}
```

## Impact on Maintainability
- **Medium**: Improves maintainability
- Reduces code by ~30%
- Easier to extend with new conflict types

## Effort Estimate
**Low** - Simple refactoring
