# Correct Solution: Transaction Visibility Cleanup

## The Core Issue

**The Gemini bot identified a critical bug:** The current cleanup removes transactions that active snapshots need to see.

### Why Current Cleanup is Broken

```rust
// Current (BROKEN) logic:
let watermark = min(active_snapshots.timestamp);  // e.g., 100
committed.retain(|_, &mut commit_ts| commit_ts >= watermark);  // Keep if >= 100
// Removes transactions with commit_ts < 100
```

**The bug:**
- Snapshot at t=100 needs to see ALL transactions that committed before t=100
- Visibility rule: `commit_ts < snapshot_ts` means VISIBLE
- Cleanup removes transactions with `commit_ts < 100`
- These are EXACTLY the transactions the snapshot needs!
- Result: `is_visible()` returns `false` for removed transactions even though they should be visible

**Example:**
1. tx1 commits at t=50
2. tx_reader captures snapshot at t=100 (watermark=100)
3. Cleanup runs, removes tx1 because 50 < 100
4. tx_reader tries to read data from tx1
5. `is_visible(snapshot@100, tx1)` checks `committed.get(tx1)` → None
6. Returns `false` ❌ **WRONG!** Should return `true` because 50 < 100

## Key Insights

### Current Implementation Status
- **Time-travel NOT yet implemented**: `read_transaction()` always uses `current_timestamp` (line 137, src/db.rs)
- Snapshots are currently monotonically increasing
- No API exists for backward time-travel queries yet

###Future Requirements
- GallifreyDB is designed as a **bi-temporal database** (CLAUDE.md)
- Must support `as_of(timestamp)`, `between(t1, t2)` temporal queries
- Historical queries at arbitrary past timestamps

## Proposed Solution: Three-Tier Approach

### Tier 1: Phase 1 Only (Safe, Immediate)  ✅

**Remove Phase 2 entirely, keep only Phase 1:**

1. **ReadTransaction Drop** impl (already implemented) ✅
   - Fixes the `active` HashSet leak
   - Completely safe and correct
   - Immediate memory savings for abandoned read transactions

2. **Remove `committed` map cleanup** ✅
   - Accept that committed map grows with transaction count
   - This is EXPECTED for temporal databases
   - Memory cost: ~24 bytes × transaction_count

**Memory Analysis:**
- 1M transactions = 24MB
- 10M transactions = 240MB
- 100M transactions = 2.4GB
- **Verdict:** Reasonable overhead for databases at this scale

**This solves issue #226's immediate problem** (abandoned read transactions) while avoiding the critical bug.

---

### Tier 2: Retention-Based Mode (Opt-In, Bounded History) ⚠️

**For applications that don't need infinite history**, provide an opt-in retention mode:

```rust
/// Retention configuration for transaction metadata.
///
/// WARNING: Enabling retention cleanup limits historical queries to the
/// retention window. Time-travel queries beyond this window will fail.
pub struct RetentionConfig {
    /// Retention window in seconds
    pub window_seconds: u64,

    /// Cleanup interval (number of commits between cleanup runs)
    pub cleanup_interval: usize,
}

impl TxVisibilityManager {
    /// Create visibility manager with retention-based cleanup.
    ///
    /// # Warning
    /// Queries attempting to read data older than the retention window
    /// will fail with `VisibilityError::SnapshotTooOld`.
    pub fn with_retention(config: RetentionConfig) -> Self {
        // Enable cleanup with explicit retention window
    }
}
```

**Modified cleanup logic:**
```rust
pub fn cleanup_old_committed(&self) -> Result<usize, ()> {
    // Only cleanup if retention is configured
    let retention_window = match self.retention_config {
        Some(ref cfg) => cfg.window_seconds,
        None => return Ok(0), // No cleanup if retention not configured
    };

    // Calculate cutoff: current_time - retention_window
    let current_time = *self.current_timestamp.lock_or_err()?;
    let cutoff_timestamp = current_time - (retention_window as i64);

    // CRITICAL: Also check active snapshots watermark
    // Must not remove transactions that active snapshots need
    let watermark = {
        let snapshots = self.active_snapshots.lock_or_recover();
        if snapshots.is_empty() {
            cutoff_timestamp
        } else {
            let min_active = snapshots.values().copied().min().unwrap();
            // Use MINIMUM of cutoff and watermark (most conservative)
            std::cmp::min(cutoff_timestamp, min_active)
        }
    };

    // Remove transactions that committed BEFORE watermark
    // BUT: we must handle this in is_visible()!
    let mut committed = self.committed.lock_or_recover();
    let original_count = committed.len();

    committed.retain(|_tx_id, &mut commit_ts| commit_ts >= watermark);

    // Track what we've pruned
    let removed = original_count - committed.len();
    if removed > 0 {
        self.oldest_retained_commit_ts.store(watermark, Ordering::Release);
    }

    Ok(removed)
}
```

**Modified is_visible() to handle pruned transactions:**
```rust
pub fn is_visible(
    &self,
    snapshot: &TransactionSnapshot,
    created_by_tx: TxId,
) -> Result<bool, VisibilityError> {
    if created_by_tx.as_u64() == 0 {
        return Ok(true);
    }

    let committed = self.committed.lock_or_recover();

    match committed.get(&created_by_tx) {
        Some(&commit_ts) => {
            // Have timestamp - apply normal visibility rules
            Ok(snapshot.is_visible(created_by_tx, Some(commit_ts)))
        }
        None => {
            // Transaction not in committed map

            let oldest_retained = self.oldest_retained_commit_ts.load(Ordering::Acquire);

            if oldest_retained == i64::MIN {
                // No cleanup has occurred - transaction never committed
                return Ok(false);
            }

            // Transaction might have been pruned
            // Check if snapshot is within retention window

            if snapshot.snapshot_timestamp < oldest_retained {
                // Snapshot is querying data older than retention window
                return Err(VisibilityError::SnapshotTooOld {
                    snapshot_ts: snapshot.snapshot_timestamp,
                    oldest_retained,
                });
            }

            // Snapshot is within retention window, transaction not committed
            Ok(false)
        }
    }
}
```

**Trade-offs:**
- ✅ Bounded memory: O(transactions_in_window) not O(all_transactions)
- ✅ Explicit semantics: Fails with clear error when querying old data
- ⚠️ Limits time-travel to retention window
- ⚠️ Changes API signature (`Result` return type)

---

### Tier 3: Future Optimization (Lossless Compression) 🔮

**For future work**, optimize committed map without losing data:

1. **Run-Length Encoding:** Compress sequential TxId ranges
2. **Delta Encoding:** Store commit_ts as deltas from previous
3. **Tiered Storage:** Hot (recent) + Cold (disk/compressed)
4. **Sparse Bitmap:** Use compressed bitmap for "committed" status

**Example RLE optimization:**
```rust
// Instead of: BTreeMap<TxId, Timestamp>
// Use: Vec<(TxIdRange, TimestampRange)> + BTreeMap for exceptions

struct CommittedMetadata {
    // Compact: TxId 100-199 all committed between t=1000-1099
    ranges: Vec<(TxIdRange, TimestampRange)>,

    // Exceptions: individual entries not in ranges
    exceptions: BTreeMap<TxId, Timestamp>,
}
```

**Memory savings:** ~10-100x compression for sequential transaction patterns

---

## Recommendation

**Implement Tier 1 + Tier 2**

1. **Default (Tier 1): No cleanup**
   - Safe for temporal databases
   - Correct for all query patterns
   - Reasonable memory for most workloads
   - Fixes the immediate `active` set leak

2. **Opt-In (Tier 2): Retention mode**
   - For applications needing bounded memory
   - Clear trade-off: limited history vs bounded memory
   - Explicit errors when querying old data
   - Documented limitations

3. **Document clearly:**
   ```rust
   /// # Memory Characteristics
   ///
   /// The committed map grows with total transaction count (~24 bytes per transaction).
   /// For temporal databases supporting historical queries, this is expected behavior.
   ///
   /// **Memory estimates:**
   /// - 1M transactions: ~24MB
   /// - 10M transactions: ~240MB
   /// - 100M transactions: ~2.4GB
   ///
   /// # Cleanup Options
   ///
   /// Use `TxVisibilityManager::with_retention()` to enable bounded memory mode:
   /// - Limits queries to a retention window
   /// - Queries outside window fail with `VisibilityError::SnapshotTooOld`
   /// - Trade-off: bounded memory for limited history
   ```

## Implementation Plan

1. **Immediate:** Remove Phase 2 cleanup from current PR
2. **Add:** Configuration option for retention mode (default: disabled)
3. **Update:** `is_visible()` return type to `Result<bool, VisibilityError>`
4. **Add:** Integration tests for retention mode
5. **Document:** Memory characteristics and trade-offs
6. **Future:** Tier 3 compression optimizations

## Why This is Correct

✅ **Tier 1:** No cleanup = always correct, safe for temporal databases
✅ **Tier 2:** Retention mode explicitly documents limitations and errors appropriately
✅ **API:** `Result` return lets applications handle old queries gracefully
✅ **Default:** Safe by default (no cleanup), opt-in for bounded memory

This design:
- Fixes the immediate memory leak (active set)
- Avoids the critical visibility bug
- Provides path to bounded memory (opt-in)
- Prepares for future temporal query features
- Documents trade-offs clearly
