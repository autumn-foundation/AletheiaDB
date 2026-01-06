---
title: "Code Quality: apply_changes() function is too complex (~300 lines)"
labels: ["code-quality", "automated-scan", "refactoring", "complexity"]
---

## Location
`src/api/transaction/write_tx.rs:532-832`

## Complexity Metrics
- **Lines:** ~300
- **Nesting Depth:** 5+ levels
- **Responsibilities:** 6+ (tombstone ID generation, lock management, node/edge operations, adjacency rebuilding)

## Why This is Problematic
- Violates Single Responsibility Principle
- Massive match statement with 6 operation types
- Each match arm is 25-60+ lines with nested logic
- Difficult to test individual responsibilities
- Hard to maintain and reason about

## Suggested Improvement
Split into separate methods:

```rust
fn apply_changes(&mut self) -> Result<()> {
    self.pre_generate_tombstone_ids()?;

    for buffered_op in &self.buffer.operations {
        match buffered_op.op_type {
            CreateNode(_) => self.apply_create_node(buffered_op)?,
            UpdateNode(_) => self.apply_update_node(buffered_op)?,
            DeleteNode => self.apply_delete_node(buffered_op)?,
            CreateEdge(_) => self.apply_create_edge(buffered_op)?,
            UpdateEdge(_) => self.apply_update_edge(buffered_op)?,
            DeleteEdge => self.apply_delete_edge(buffered_op)?,
        }
    }

    Ok(())
}

fn apply_create_node(&mut self, op: &BufferedOperation) -> Result<()> { /* ... */ }
fn apply_update_node(&mut self, op: &BufferedOperation) -> Result<()> { /* ... */ }
// ... etc
```

## Impact on Maintainability
- **High**: Improves maintainability and testability
- Enables focused unit tests for each operation type
- Easier to understand control flow

## Effort Estimate
**High** - Requires refactoring and comprehensive testing
