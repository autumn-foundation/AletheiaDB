---
title: "Code Quality: parse_wal_entries_versioned() duplicates read_segment() (~339 lines)"
labels: ["code-quality", "automated-scan", "refactoring", "duplication", "high-priority"]
---

## Location
`src/storage/wal.rs:1190-1529`

## Why This is Problematic
- ~70% code duplication with `read_segment()`
- Both functions have nearly identical structure
- Changes to WAL format require updating both functions
- Increases maintenance burden and risk of bugs
- **Total duplication:** ~730 lines of nearly identical code

## Suggested Improvement
Consolidate into single implementation:

```rust
fn read_wal_entries<R: Read>(&mut self, reader: R, source: &str) -> Result<Vec<WalEntry>> {
    // Unified parsing logic with configurable input source
}

fn read_segment(&mut self, segment_id: u64) -> Result<Vec<WalEntry>> {
    let buffer = self.read_segment_file(segment_id)?;
    self.read_wal_entries(Cursor::new(buffer), "segment")
}

fn parse_wal_entries_versioned(&mut self, path: &Path) -> Result<Vec<WalEntry>> {
    let file = File::open(path)?;
    self.read_wal_entries(BufReader::new(file), "file")
}
```

## Impact on Maintainability
- **High**: Eliminates ~300 lines of duplication
- Single source of truth for WAL parsing
- Reduces bug surface area

## Effort Estimate
**Medium** - Consolidation is straightforward but requires thorough testing
