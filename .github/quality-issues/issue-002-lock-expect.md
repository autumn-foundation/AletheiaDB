---
title: "Code Quality: Lock poisoning uses .expect() in critical transaction path"
labels: ["code-quality", "automated-scan", "error-handling", "high-priority"]
---

## Location
- `src/api/transaction/write_tx.rs:178`
- `src/api/transaction/write_tx.rs:188`

## Current State
```rust
let mut ts = self
    .current_timestamp
    .lock()
    .expect("timestamp lock poisoned - unrecoverable state");

// ...

let mut wal = self
    .wal
    .lock()
    .expect("WAL lock poisoned - unrecoverable state");
```

The commit path uses `.expect()` for lock acquisition, causing panics on lock poisoning.

## Why This is Problematic
- Violates coding standards: "Never `.unwrap()` in production"
- Panics in the commit path can leave transactions in inconsistent state
- Lock poisoning is recoverable in many cases - the mutex guard can still be acquired
- Ironically, `.expect()` itself can cause more lock poisoning if it panics
- Comments at lines 716 and 771 acknowledge this: "CRITICAL: Use proper error handling instead of .expect() to avoid lock poisoning"

## Suggested Improvement
Use the existing `lock_or_err()` helper which returns `Result`:

```rust
let mut ts = self.current_timestamp.lock_or_err()?;
let mut wal = self.wal.lock_or_err()?;
```

This converts lock poisoning into a proper `Error::LockPoisoned` that can be handled gracefully.

## Impact on Maintainability
- **High**: Affects critical commit path
- Prevents cascade failures from lock poisoning
- Aligns with existing error handling infrastructure

## Effort Estimate
**Low** - Simple find-and-replace, helper already exists
