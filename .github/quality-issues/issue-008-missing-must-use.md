---
title: "Code Quality: Missing #[must_use] on Result-returning functions"
labels: ["code-quality", "automated-scan", "api-design"]
---

## Location
Various files throughout the codebase

## Why This is Problematic
- Only 4 `#[must_use]` annotations found in entire codebase
- Many public functions return `Result<T>` without `#[must_use]`
- Users can accidentally ignore errors
- Violates Rust best practices and linter recommendations

## Examples Missing #[must_use]

```rust
// src/db.rs
pub fn create_node(&self, label: &str, properties: PropertyMap) -> Result<NodeId>
pub fn update_node(&self, id: NodeId, properties: PropertyMap) -> Result<()>
pub fn delete_node(&self, id: NodeId) -> Result<()>
pub fn create_edge(&self, source: NodeId, target: NodeId, label: &str, properties: PropertyMap) -> Result<EdgeId>

// src/api/transaction/write_tx.rs
pub fn commit(mut self) -> Result<()>

// Many more...
```

## Suggested Improvement
Add `#[must_use]` to all public Result-returning functions:

```rust
#[must_use]
pub fn create_node(&self, label: &str, properties: PropertyMap) -> Result<NodeId>

#[must_use]
pub fn update_node(&self, id: NodeId, properties: PropertyMap) -> Result<()>

#[must_use]
pub fn commit(mut self) -> Result<()>
```

## Impact on Maintainability
- **Medium**: Improves API safety
- Catches error-ignoring bugs at compile time
- Better developer experience

## Effort Estimate
**Low** - Automated with grep + sed, or use clippy's `unused_must_use` lint
