---
title: "Code Quality: System clock .expect() can panic in time::now()"
labels: ["code-quality", "automated-scan", "error-handling"]
---

## Location
`src/core/temporal.rs:393`

## Current State
```rust
pub fn now() -> Timestamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("System clock is before Unix epoch")
        .as_micros() as i64
}
```

## Why This is Problematic
- Violates coding standards: "Never `.unwrap()` in production"
- While rare, system clock issues can occur (VM snapshots, incorrect time zones, NTP failures)
- Panics are not recoverable and will crash the database
- This function is called frequently in transaction paths

## Suggested Improvement
Return `Result<Timestamp>` and let callers handle the error:

```rust
pub fn now() -> Result<Timestamp> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .map_err(|e| Error::InvalidTimestamp(format!("System clock error: {}", e)))
}
```

Alternatively, if backwards clock movement is a critical invariant, consider using monotonic time or clamping to the last known timestamp.

## Impact on Maintainability
- **Medium**: Affects all timestamp operations
- Improves robustness against clock issues
- Requires updating all callers to handle Result

## Effort Estimate
**Medium** - Need to update signature and all call sites (~20 locations)
