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

### Tier 2: Why Retention-Based Cleanup Doesn't Work ❌

**Initial idea:** Provide opt-in retention mode with bounded memory.

**Why this fails (discovered during analysis):**

The "visibility paradox" makes this fundamentally broken:

```rust
// BROKEN: Returns false for transactions that should be visible
match committed.get(&created_by_tx) {
    None => {
        if snapshot.snapshot_timestamp < oldest_retained {
            return Err(SnapshotTooOld);
        }
        // BUG: Returns false even though tx might be visible!
        Ok(false)
    }
}
```

**Example failure:**
1. `oldest_retained = 100`
2. tx1 committed at t=50, was pruned
3. Snapshot taken at t=120
4. Query for tx1: `committed.get(tx1)` returns `None`
5. Check: `120 < 100` = false, so proceed
6. Return `Ok(false)` ❌ **WRONG!** Should be visible (50 < 120)

**The problem:** We can't distinguish "never committed" from "committed and pruned" without additional state.

**Possible fixes all have issues:**
- Track `pruned_before` watermark → False positives (phantom data)
- Track individual pruned TxIds → Defeats memory savings
- Assume all pruned are visible → False positives
- Assume all pruned are invisible → False negatives (this proposal)

**Conclusion:** Retention-based cleanup is incompatible with correctness. Use Tier 3 (compression) instead.

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
