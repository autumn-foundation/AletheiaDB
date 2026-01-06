# MVCC Garbage Collection Research Findings

## Papers Analyzed

1. **[Memory-Optimized Multi-Version Concurrency Control for Disk-Based Database Systems](https://www.vldb.org/pvldb/vol15/p2797-freitag.pdf)** (Freitag et al., VLDB 2022)
2. **[Scalable Garbage Collection for In-Memory MVCC Systems](https://users.cs.utah.edu/~pandey/courses/cs6530/fall22/papers/mvcc/p128-bottcher.pdf)** (Böttcher et al., VLDB 2019)
3. **[TiDB MVCC Garbage Collection](https://pingcap.github.io/tidb-dev-guide/understand-tidb/mvcc-garbage-collection.html)** (TiDB Documentation)

## Key Insights

### 1. What Gets Garbage Collected?

**Critical distinction:** MVCC GC in these papers removes **OLD DATA VERSIONS**, not **COMMIT METADATA**.

**Example (HyPer/TiDB approach):**
```
Record X has versions:
- v1 @ t=10 (created by tx1)
- v2 @ t=20 (created by tx2)
- v3 @ t=30 (created by tx3)

Oldest active snapshot: t=25

GC can remove: v1@t=10 (not visible to any active snapshot)
GC must keep: v2@t=20, v3@t=30
Commit metadata: STILL KEPT for tx1, tx2, tx3
```

The commit timestamps for tx1, tx2, tx3 are **embedded in the versions themselves**, not in a separate map!

### 2. The "Safe Point" / Watermark

**Definition:** `safe_point = min(start_timestamp of all active transactions)`

**Usage:** Can remove versions with `version_timestamp < safe_point` that are NOT the latest version.

**From TiDB:**
> "The safe point is the min transaction start timestamp between all TiDB instances. TiDB instances store their min start timestamp in PD's etcd."

### 3. Architectural Difference

**HyPer/TiDB Architecture:**
- Timestamps embedded IN versions
- No separate centralized commit metadata map
- Each tuple stores: (data, commit_timestamp)
- Visibility check: read timestamp directly from tuple

**GallifreyDB Current Architecture:**
- Timestamps in SEPARATE `committed: BTreeMap<TxId, Timestamp>`
- Versions stored without embedded timestamps
- Visibility check: lookup `committed.get(tx_id)` → then check rules

**This is why we have the problem!** We're trying to GC the metadata map, but that's not what these papers do.

## Why Our Current Approach is Broken

### The Fundamental Issue

We're trying to apply VERSION garbage collection techniques to METADATA garbage collection, which doesn't work!

**Version GC (what papers do):**
- Remove old versions of DATA
- Keep metadata (commit timestamps)
- Safe because: removed versions aren't visible anyway

**Metadata GC (what we're trying):**
- Remove commit timestamp records
- Keep data versions
- **UNSAFE because: we can't check visibility without the metadata!**

### The Visibility Paradox

```rust
// To know if we can GC transaction T's metadata:
if no_snapshot_will_ever_query(T) {
    remove_metadata(T);  // Safe!
}

// But how do we know this?
// - For temporal DB: We CAN'T know (future queries at arbitrary timestamps)
// - For forward-only DB: When T.commit_ts < min(active_snapshots.start_ts)
//   BUT this is exactly when snapshots NEED to see T!
```

**The paradox:**
- Metadata is needed precisely when `commit_ts < snapshot_ts` (visibility condition)
- We can only "safely" remove when `commit_ts < min(snapshots_ts)`
- But that's when it's MOST needed!

## Solutions from Literature

### Solution 1: Embed Timestamps (HyPer/TiDB)

**Refactor to embed commit timestamps in versions:**

```rust
pub struct NodeVersion {
    pub label: InternedString,
    pub properties: PropertyMap,
    pub commit_timestamp: Timestamp,  // NEW: embedded!
}

pub fn is_visible(&self, version: &NodeVersion, snapshot_ts: Timestamp) -> bool {
    version.commit_timestamp < snapshot_ts
    // No need for separate committed map!
}
```

**Pros:**
- ✅ No separate metadata map needed
- ✅ Can GC old versions safely
- ✅ Scales with concurrent transactions, not total transactions

**Cons:**
- ⚠️ Major architectural change
- ⚠️ Requires refactoring all version storage
- ⚠️ Breaks existing API

### Solution 2: Epoch-Based Compression (Research Direction)

**Instead of removing metadata, COMPRESS it:**

```rust
// Current: Individual entries
BTreeMap<TxId, Timestamp>  // 24 bytes × N transactions

// Compressed: Ranges + exceptions
struct EpochMetadata {
    // Transactions 1000-1999 all committed in epoch [t=5000..6000]
    epochs: Vec<(TxIdRange, TimestampRange)>,

    // Individual entries outside ranges
    exceptions: BTreeMap<TxId, Timestamp>,
}
```

**For sequential transaction patterns:** 10-100x compression!

**Pros:**
- ✅ Lossless (no data lost)
- ✅ Correct for all queries
- ✅ Can be implemented incrementally

**Cons:**
- ⚠️ Complex implementation
- ⚠️ Lookup overhead for range checks

### Solution 3: Two-Tier Metadata (Hot + Archived)

**Keep recent metadata hot, archive old to disk:**

```rust
pub struct TxVisibilityManager {
    // In-memory: recent transactions
    committed_hot: BTreeMap<TxId, Timestamp>,

    // On disk: archived metadata (lazy loaded)
    committed_archive: DiskBackedMap<TxId, Timestamp>,

    archive_threshold: Timestamp,
}
```

**Pros:**
- ✅ Bounded memory
- ✅ Lossless (all data kept)
- ✅ Correct for temporal queries

**Cons:**
- ⚠️ Disk I/O overhead for old queries
- ⚠️ Complex lifecycle management

## Recommendation Based on Research

### Short Term: Don't GC Metadata (Accept Memory Cost)

**The literature shows:** Systems either embed timestamps IN versions (no separate map) or keep all metadata.

**For GallifreyDB with separate metadata:**
- Memory cost is ~24 bytes per transaction
- For 100M transactions: 2.4GB
- **This is acceptable** for databases at this scale

### Medium Term: Compression

**Implement epoch-based compression:**
- Lossless - no correctness issues
- 10-100x memory savings for sequential workloads
- Maintains correctness for temporal queries

### Long Term: Architectural Refactor

**Embed timestamps in versions** (HyPer/TiDB approach):
- Eliminates separate metadata map entirely
- Natural fit for MVCC
- Enables proper version GC

## Specific Recommendation for This PR

**Remove Phase 2 cleanup, keep Phase 1 only.**

**Rationale based on literature:**
1. The papers GC *versions*, not *metadata*
2. Metadata is essential for visibility checks
3. Removing metadata breaks MVCC correctness (as Gemini bot correctly identified)
4. Memory cost is reasonable and expected for MVCC systems

**Document as future work:**
- Epoch-based compression for memory optimization
- Architectural refactor to embed timestamps in versions
- Version GC (separate from metadata GC)

## Key Takeaway

**The Gemini bot was right.** The current cleanup is fundamentally broken because it tries to remove commit metadata, which breaks visibility checks. The academic literature doesn't do this - they keep metadata and GC old versions instead.

Our options are:
1. Keep all metadata (current best choice)
2. Compress metadata (future optimization)
3. Refactor to embed timestamps (major change)

Trying to remove metadata (current PR approach) is incompatible with correct MVCC semantics.

## Sources

- [Memory-Optimized Multi-Version Concurrency Control](https://www.vldb.org/pvldb/vol15/p2797-freitag.pdf) (Freitag et al., VLDB 2022)
- [Scalable Garbage Collection for In-Memory MVCC Systems](https://users.cs.utah.edu/~pandey/courses/cs6530/fall22/papers/mvcc/p128-bottcher.pdf) (Böttcher et al., VLDB 2019)
- [TiDB MVCC Garbage Collection](https://pingcap.github.io/tidb-dev-guide/understand-tidb/mvcc-garbage-collection.html)
- [CMU 15-721 MVCC Lecture Notes](https://15721.courses.cs.cmu.edu/spring2020/notes/05-mvcc3.pdf)
- [Scalable and Robust Snapshot Isolation](https://www.vldb.org/pvldb/vol16/p1426-alhomssi.pdf)
