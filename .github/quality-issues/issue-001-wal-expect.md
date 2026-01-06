---
title: "Code Quality: WAL creation uses .expect() in production code"
labels: ["code-quality", "automated-scan", "error-handling"]
---

## Location
`src/db.rs:59`

## Current State
```rust
let wal = WriteAheadLog::new(WalConfig::default()).expect("Failed to create WAL");
```

The WAL creation in the `GallifreyDB::with_config()` constructor uses `.expect()`, which will panic if WAL initialization fails.

## Why This is Problematic
- Panics in production code violate Rust best practices
- Constructor failure should be recoverable
- Violates CLAUDE.md guideline: "Never `.unwrap()` in production - Prevents panics"
- Users cannot gracefully handle WAL initialization failures (e.g., permissions, disk space)

## Suggested Improvement
Change the constructor signature to return `Result<Self>`:

```rust
pub fn with_config(config: AnchorConfig) -> Result<Self> {
    let wal = WriteAheadLog::new(WalConfig::default())?;

    Ok(GallifreyDB {
        // ... rest of initialization
    })
}
```

Also update `new()` to propagate the error:
```rust
pub fn new() -> Result<Self> {
    Self::with_config(AnchorConfig::default())
}
```

## Impact on Maintainability
- **High**: This is a fundamental API that affects all users
- Better error handling improves robustness
- Allows users to handle initialization failures gracefully

## Effort Estimate
**Medium** - Requires updating constructor signatures and all call sites, plus tests
