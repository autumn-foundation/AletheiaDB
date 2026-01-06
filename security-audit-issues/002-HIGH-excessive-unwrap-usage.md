# Security: Excessive Use of .unwrap()/.expect() in Production Code

**Labels**: `security`, `automated-scan`, `high`, `P1`, `error-handling`
**Priority**: P1 - High priority for production readiness

## Summary
Production code contains 1,190+ instances of `.unwrap()`, `.expect()`, and panic paths across 30 files. While many are in test code (acceptable), numerous instances exist in production hot paths where panics can cause DoS.

## Statistics
- **Total occurrences**: 1,190+ across 30 files
- **Critical files affected**:
  - `src/db.rs`: 97 instances (main database coordinator)
  - `src/api/transaction/write_tx.rs`: 165+ instances
  - `src/storage/wal.rs`: 50+ instances
  - `src/core/temporal.rs`: 5 instances
  - `src/storage/persistence.rs`: 11+ instances

## Severity
**HIGH**

## Impact
- **Availability**: Panic cascades can bring down entire service
- **DoS Attack**: Malicious input can trigger panics
- **Poor UX**: Abrupt termination instead of graceful error handling
- **Data Corruption**: Panics during writes can leave inconsistent state
- **Cascading Failures**: One panic can poison locks, causing more panics

## Examples

### Example 1: Lock Panics (20 instances)
```rust
let t1 = *db.current_timestamp.lock().unwrap() - 1;
//                                    ^-- PANICS if lock poisoned
```

**Issue**: If any thread panics while holding this lock, all subsequent accesses panic.

**Fix**: Use `lock_or_err()` which already exists in the codebase:
```rust
let t1 = *db.current_timestamp.lock_or_err()? - 1;
```

### Example 2: Property Deserialization (property.rs:359, 371, 593)
```rust
// SAFETY: Length check above guarantees slice has 8 bytes
let value = i64::from_le_bytes(bytes[1..9].try_into().unwrap());
//                                                      ^-- Should never panic, but...
```

**Analysis**: While documented as safe after length checks, deserializing untrusted input should use explicit error messages for debugging.

**Fix**:
```rust
let value = i64::from_le_bytes(
    bytes[1..9].try_into()
        .expect("BUG: slice length validated above at line 352")
);
```

## Recommended Fixes

### Strategy 1: Convert to Result-based APIs
```rust
// BAD
pub fn with_config(config: AnchorConfig) -> Self {
    let wal = WriteAheadLog::new(WalConfig::default()).expect("Failed to create WAL");
    ...
}

// GOOD
pub fn with_config(config: AnchorConfig) -> Result<Self, Error> {
    let wal = WriteAheadLog::new(WalConfig::default())?;
    ...
    Ok(GallifreyDB { ... })
}
```

### Strategy 2: Use lock_or_err() Consistently
The codebase already has `LockExt` trait providing `lock_or_err()` and `lock_or_recover()`. Use them consistently:

```rust
// BAD
let timestamp = *self.current_timestamp.lock().unwrap();

// GOOD (already exists in codebase!)
let timestamp = *self.current_timestamp.lock_or_err()?;
```

### Strategy 3: Document Justified Unwraps
For cases where unwrap is truly justified (after validation):

```rust
// Justified unwrap - length already validated above
let value = i64::from_le_bytes(
    bytes[1..9].try_into()
        .expect("BUG: slice length validated above")
);
```

### Strategy 4: Add Clippy Lint
Prevent future unwraps:

```rust
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]

// Allow only in test code
#[cfg(test)]
#![allow(clippy::unwrap_used)]
```

## Audit Plan

1. **Classify all 1,190 instances**:
   - ✅ Test code → acceptable, document with `#[cfg(test)]`
   - ⚠️ After validation → add explanatory `expect("BUG: ...")`
   - ❌ Recoverable error → convert to `Result<?>`
   - ❌ User input → **must** convert to proper error

2. **Priority order**:
   - P0: User-facing APIs (can receive untrusted input)
   - P1: Lock acquisitions (prevent panic cascades)
   - P1: Database constructors (prevent startup failures)
   - P2: Internal functions with pre-conditions
   - P3: Test helpers (document only)

3. **Automation**:
   ```bash
   # Find all unwraps in non-test code
   rg '\.(unwrap|expect)\(' src/ -g '!*test*.rs' -g '!*tests.rs'

   # Count by file
   rg '\.(unwrap|expect)\(' src/ -g '!*test*.rs' --count-matches | sort -t: -k2 -nr
   ```

## Testing Strategy

### Panic Path Testing
```rust
#[test]
#[should_panic(expected = "some expected message")]
fn test_panics_are_intentional() {
    // Verify panics happen where expected
}
```

### Fuzzing Targets
Priority targets for fuzzing (to trigger panics):
1. `PropertyValue::deserialize()` - parse untrusted data
2. `WalEntry::deserialize()` - WAL corruption
3. Public APIs accepting user input
4. Lock acquisition paths

## References
- [Rust Error Handling](https://doc.rust-lang.org/book/ch09-00-error-handling.html)
- [Clippy::unwrap_used](https://rust-lang.github.io/rust-clippy/master/index.html#/unwrap_used)
- [Lock Poisoning in Rust](https://doc.rust-lang.org/std/sync/struct.Mutex.html#poisoning)

## Related Code
The codebase already has good lock handling infrastructure:
- `src/utils/lock.rs`: `LockExt` trait with `lock_or_err()` and `lock_or_recover()`
- Just needs consistent usage throughout

## Priority
**P1 - High priority**

Should be completed before beta release. Critical for production reliability.

## Estimated Effort
- 1-2 weeks for comprehensive audit
- 2-3 weeks for fixes and testing
- Ongoing: Add clippy lint to CI
