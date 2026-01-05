# Critical Bug Analysis: Transaction Visibility Cleanup

## The Bug (Identified by Gemini Code Review)

### Current Implementation
```rust
pub fn cleanup_old_committed(&self) -> usize {
    let watermark = min(active_snapshots.timestamp);
    committed.retain(|_tx_id, &mut commit_ts| commit_ts >= watermark);
    // Removes transactions with commit_ts < watermark
}

pub fn is_visible(&self, snapshot: &TransactionSnapshot, created_by_tx: TxId) -> bool {
    match committed.get(&created_by_tx) {
        Some(&commit_ts) => snapshot.is_visible(created_by_tx, Some(commit_ts)),
        None => false  // ← BUG: Can't distinguish "not committed" from "pruned"
    }
}
```

### The Problem
When cleanup removes a transaction from `committed` map, `is_visible()` returns `false` even though the transaction should be visible.

**Example scenario:**
1. tx1 commits at timestamp=50 → added to committed map
2. Long-running read transaction tx_A starts, captures snapshot at timestamp=100
3. Watermark becomes 100 (min of active snapshots)
4. cleanup_old_committed() runs, removes tx1 because 50 < 100
5. tx_A tries to read data created by tx1
6. is_visible(snapshot_A, tx1) returns false because committed.get(tx1) returns None
7. **Read fails even though it should succeed** (50 < 100 = visible)

## MVCC Visibility Rules (Snapshot Isolation)

A version created by tx_created is **visible** to a snapshot if:
```
commit_ts(tx_created) < snapshot_ts AND
tx_created NOT IN snapshot.active_transactions
```

**Key insight:** We need to know WHEN a transaction committed, not just IF it committed.

## Temporal Database Constraints

GallifreyDB is a **bi-temporal database** designed to support:
1. **Current queries** - "What is the state now?"
2. **Time-travel queries** - "What was the state at time T in the past?"
3. **Time-range queries** - "Show me all changes between T1 and T2"

This means:
- Snapshots can be taken at ANY timestamp (past, present, future)
- We cannot assume snapshots are monotonically increasing
- Historical metadata is REQUIRED for correctness

## Solution Options Analysis

### Option 1: Don't Cleanup At All ❌
**Approach:** Accept unbounded growth as inherent to temporal databases.

**Memory cost:**
- 1M transactions = 24MB (16-byte TxId + 8-byte Timestamp + overhead)
- 10M transactions = 240MB
- 100M transactions = 2.4GB

**Pros:** ✅ Correct, ✅ Simple
**Cons:** ❌ Doesn't solve the memory leak issue

**Verdict:** Not acceptable - issue #226 specifically asks for bounded memory.

---

### Option 2: Pruned Watermark (Track What We Removed) ⚠️
**Approach:** Add `pruned_before: AtomicI64` to track minimum commit_ts we've pruned.

```rust
pub struct TxVisibilityManager {
    committed: Mutex<BTreeMap<TxId, Timestamp>>,
    pruned_before: AtomicI64,  // Minimum commit_ts ever pruned
}

pub fn is_visible(&self, snapshot: &TransactionSnapshot, created_by_tx: TxId) -> bool {
    match committed.get(&created_by_tx) {
        Some(&commit_ts) => snapshot.is_visible(created_by_tx, Some(commit_ts)),
        None => {
            let pruned_before = self.pruned_before.load(Ordering::Acquire);
            if pruned_before == 0 {
                return false; // Never pruned - tx not committed
            }
            // Assume tx committed at pruned_before timestamp
            snapshot.is_visible(created_by_tx, Some(pruned_before))
        }
    }
}
```

**Problem - False Positives:**
- tx999 never committed, not in map
- We've pruned transactions up to timestamp=1000
- Query with snapshot_ts=2000 asks about tx999
- We return `snapshot.is_visible(tx999, Some(1000))` = true
- **Wrong!** tx999 never existed

**Verdict:** Unsafe - creates phantom data.

---

### Option 3: Two-Level Storage (Hot + Cold) ⚠️
**Approach:** Keep recent transactions in hot map, move old to cold storage.

```rust
pub struct TxVisibilityManager {
    committed_hot: Mutex<BTreeMap<TxId, Timestamp>>,
    committed_cold: Mutex<BTreeMap<TxId, Timestamp>>,
}
```

**Problem:** Still unbounded growth, just split across two structures.

**Verdict:** Doesn't solve memory leak.

---

### Option 4: Explicit Retention Policy with Errors ⚠️
**Approach:** Only cleanup transactions older than retention window. Error on queries outside window.

```rust
pub struct TxVisibilityManager {
    committed: Mutex<BTreeMap<TxId, Timestamp>>,
    oldest_retained_commit_ts: AtomicI64,
    retention_window_seconds: u64,
}

pub fn is_visible(&self, snapshot: &TransactionSnapshot, created_by_tx: TxId)
    -> Result<bool, VisibilityError> {
    match committed.get(&created_by_tx) {
        Some(&commit_ts) => Ok(snapshot.is_visible(created_by_tx, Some(commit_ts))),
        None => {
            let oldest = self.oldest_retained_commit_ts.load(Ordering::Acquire);
            if snapshot.snapshot_timestamp < oldest {
                // Query is outside retention window
                Err(VisibilityError::SnapshotTooOld {
                    snapshot_ts: snapshot.snapshot_timestamp,
                    oldest_retained: oldest
                })
            } else {
                // Within retention window - missing means not committed
                Ok(false)
            }
        }
    }
}
```

**Pros:** ✅ Correct within window, ✅ Bounded memory, ✅ Explicit semantics
**Cons:** ⚠️ Breaks time-travel beyond retention, ⚠️ Changes API to return Result

**Verdict:** Workable but breaks temporal database promise.

---

### Option 5: Bloom Filter + Approximate Tracking ❌
**Approach:** Use Bloom filter to track "definitely not committed" vs "maybe committed or pruned".

**Problem:** Still can't distinguish "not committed" from "pruned". Plus false positives.

**Verdict:** Too imprecise.

---

### Option 6: TxId Range Tracking ⚠️
**Approach:** Track ranges of TxIds that were pruned with their commit timestamp ranges.

```rust
struct PrunedRange {
    tx_range: (TxId, TxId),
    commit_ts_range: (Timestamp, Timestamp),
}
```

**Problem:**
- Complex to maintain
- TxIds aren't monotonic with commit time
- Still imprecise (conservative approximation)

**Verdict:** Complex, still not fully correct.

---

### Option 7: Hybrid - Active Snapshot Tracking + Conservative Cleanup ✅
**Approach:** Only cleanup when we're CERTAIN it's safe.

**Key insight:** A transaction can be safely removed if its commit_timestamp is OLDER than all possible queries.

**Implementation:**
```rust
pub struct TxVisibilityManager {
    active: Mutex<HashSet<TxId>>,
    committed: Mutex<BTreeMap<TxId, Timestamp>>,
    active_snapshots: Mutex<BTreeMap<TxId, Timestamp>>,
    oldest_possible_snapshot: AtomicI64,  // NEW
}

pub fn cleanup_old_committed(&self) -> usize {
    // Calculate safe cleanup threshold
    let watermark = {
        let snapshots = self.active_snapshots.lock_or_recover();

        if snapshots.is_empty() {
            // No active snapshots - use oldest_possible_snapshot
            self.oldest_possible_snapshot.load(Ordering::Acquire)
        } else {
            // Min of active snapshots and oldest_possible
            let min_active = snapshots.values().copied().min().unwrap();
            let oldest_possible = self.oldest_possible_snapshot.load(Ordering::Acquire);
            std::cmp::min(min_active, oldest_possible)
        }
    };

    // Only remove transactions that committed BEFORE oldest possible query
    let mut committed = self.committed.lock_or_recover();
    let original_count = committed.len();
    committed.retain(|_tx_id, &mut commit_ts| commit_ts >= watermark);
    original_count - committed.len()
}
```

**But wait:** How do we set `oldest_possible_snapshot`?

**For forward-only queries:** `oldest_possible_snapshot = min(all snapshots ever taken)`

**For temporal databases:** We CAN'T know this because future queries might ask about arbitrary past times!

**Verdict:** Doesn't work for temporal databases. 😞

---

## The Fundamental Problem

**For a true temporal database that supports arbitrary historical queries:**
```
We CANNOT safely remove committed transaction metadata.
```

**Why?** Because a future query might ask "What was the state at time T?" where T is in the past, and we need to know which transactions had committed by time T.

## Recommended Solution: Phase 1 Only + Documentation

**Accept that Phase 2 (committed map cleanup) is fundamentally incompatible with temporal database semantics.**

### Implementation:

1. **Keep Phase 1 (ReadTransaction Drop)** ✅
   - Fixes the `active` HashSet leak
   - Correct and safe
   - Immediate impact

2. **Remove Phase 2 (committed map cleanup)** ✅
   - Accept that `committed` map grows with total transaction count
   - This is EXPECTED behavior for temporal databases
   - Document memory expectations

3. **Document memory characteristics** ✅
   - ~24 bytes per transaction
   - 1M transactions = 24MB
   - 100M transactions = 2.4GB
   - For databases at this scale, this is reasonable

4. **Future optimization paths:**
   - Compress TxId → Timestamp mapping (RLE, delta encoding)
   - Archive old metadata to disk with lazy loading
   - Provide opt-in "retention window" mode that breaks time-travel

### Memory Analysis: Is This Acceptable?

**Workload:** 100M transactions over database lifetime

**Memory cost:**
- Current: ~2.4GB for committed map
- Optimized (future): Could compress to <500MB with RLE

**For a database handling 100M transactions:**
- This is likely a high-volume production system
- 2.4GB of metadata is <1% of typical RAM allocation
- **Verdict:** Acceptable overhead for temporal database guarantees

## Alternative: Retention-Based Mode (Future Work)

For applications that DON'T need infinite history:

```rust
pub struct RetentionConfig {
    pub window_seconds: u64,
}

impl TxVisibilityManager {
    pub fn with_retention(retention: RetentionConfig) -> Self {
        // Cleanup enabled with explicit retention window
        // Time-travel beyond window returns SnapshotTooOld error
    }
}
```

**Document clearly:**
- ⚠️ Retention mode breaks time-travel beyond window
- ✅ Bounds memory to ~retention_window * tx_rate * 24 bytes
- Use only if you don't need full temporal history

## Conclusion

**The Gemini bot was correct:** The current cleanup logic is fundamentally broken.

**The fix:** Remove Phase 2 cleanup, keep Phase 1 only, accept memory cost as inherent to temporal database design.

**Memory cost is reasonable** for the guarantees provided.
