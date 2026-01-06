---
title: "Code Quality: read_segment() function is too complex (~369 lines)"
labels: ["code-quality", "automated-scan", "refactoring", "complexity"]
---

## Location
`src/storage/wal.rs:618-987`

## Complexity Metrics
- **Lines:** ~369
- **Nesting Depth:** 4-5 levels
- **Operation Types:** 7 variants, each 40-80+ lines

## Why This is Problematic
- Massive while loop with large match statement
- Version-aware branching (V1 vs V2) adds complexity
- Deep nesting: while → if → match → if (5 levels)
- Parsing logic interleaved with boundary checking

## Suggested Improvement
Extract per-operation parsers:

```rust
fn read_segment(&mut self, segment_id: u64) -> Result<Vec<WalEntry>> {
    // ... header logic ...

    while offset < buffer.len() {
        let entry = match op_type {
            0 => self.parse_create_node(&buffer, &mut offset, version)?,
            1 => self.parse_create_edge(&buffer, &mut offset, version)?,
            // ... etc
        };
        entries.push(entry);
    }
}

fn parse_create_node(&self, buffer: &[u8], offset: &mut usize, version: u8) -> Result<WalEntry> {
    // Isolated parsing logic
}
```

## Impact on Maintainability
- **High**: Improves readability and maintainability
- Easier to test individual parsers
- Simpler to add new operation types

## Effort Estimate
**High** - Requires careful refactoring to maintain correctness
