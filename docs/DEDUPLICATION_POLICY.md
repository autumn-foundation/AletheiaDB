# Deduplication Policy Analysis

## Executive Summary

This document analyzes whether we need a configurable `DeduplicationPolicy` enum for handling duplicate version IDs during batch inserts in the temporal index.

**Current Behavior**: "First in sorted order wins" (implicit via `dedup_by_key`)

**Recommendation**: Keep the current implicit policy for now, but document it clearly. Add explicit policy enum only if use cases emerge that require "latest-wins" semantics.

## Current Implementation

```rust
fn insert_batch(&mut self, mut entries: Vec<TimelineEntry>) {
    if entries.is_empty() {
        return;
    }
    self.versions.reserve(entries.len());
    self.versions.append(&mut entries);
    self.versions.sort_by_key(|e| e.start);  // Sort by start time
    self.versions.dedup_by_key(|e| e.version_id);  // Keep first occurrence after sort
}
```

**Deduplication semantics**:
- After sorting by `start` time, consecutive duplicates (same `version_id`) are removed
- `dedup_by_key` keeps the **first occurrence** in the sorted vector
- This means: **earliest start time wins** for duplicate version IDs

## Use Case Analysis

### Use Case 1: Idempotent WAL Replay (Current Primary Use Case)

**Scenario**: During recovery, the same WAL operation is replayed multiple times.

**Example**:
```
WAL Entry 1: Create version_id=100 at start=1000, end=2000
WAL Entry 2: Create version_id=100 at start=1000, end=2000  (duplicate replay)
```

**Expected behavior**: Keep the first occurrence, ignore the duplicate.

**Current policy**: ✅ **CORRECT** - First occurrence wins.

### Use Case 2: Retroactive Corrections with Same Version ID (Hypothetical)

**Scenario**: A correction is made to historical data, reusing the same version ID but with updated time ranges.

**Example**:
```
Original: version_id=100 at start=1000, end=2000
Correction: version_id=100 at start=1000, end=2500  (extended end time)
```

**Expected behavior**: Depends on intent:
- **Keep original**: First-wins (current policy)
- **Keep correction**: Latest-wins (would need new policy)

**Current policy**: ✅ First-wins is reasonable (corrections should use new version IDs)

### Use Case 3: Out-of-Order Batch Processing (Edge Case)

**Scenario**: Multiple batches arrive out of order, each containing the same version ID with different metadata.

**Example**:
```
Batch 1: version_id=100 at start=1000, end=2000, recorded_at=T1
Batch 2: version_id=100 at start=1000, end=2500, recorded_at=T2 (arrived first)
```

**Expected behavior**:
- **Chronological order**: Keep version from earlier recorded_at (T1)
- **Latest-wins**: Keep version from later recorded_at (T2)

**Current policy**: ⚠️ **Ambiguous** - Keeps whichever has the earliest `start` time after merging both batches. If `start` is identical, keeps first in the sorted vector (arbitrary order).

## Proposed Deduplication Policies

If we were to add an enum, it could look like:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeduplicationPolicy {
    /// Keep the first occurrence after sorting by start time (default).
    /// Correct for idempotent WAL replay.
    FirstOccurrence,

    /// Keep the last occurrence after sorting by start time.
    /// Use when later data should override earlier data with same version ID.
    LastOccurrence,

    /// Reject duplicates with an error.
    /// Use when duplicates indicate a bug or data corruption.
    Reject,
}

impl Default for DeduplicationPolicy {
    fn default() -> Self {
        Self::FirstOccurrence
    }
}
```

**Implementation impact**:
```rust
fn insert_batch(&mut self, mut entries: Vec<TimelineEntry>, policy: DeduplicationPolicy) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    self.versions.reserve(entries.len());
    self.versions.append(&mut entries);
    self.versions.sort_by_key(|e| e.start);

    match policy {
        DeduplicationPolicy::FirstOccurrence => {
            self.versions.dedup_by_key(|e| e.version_id);
        }
        DeduplicationPolicy::LastOccurrence => {
            // Reverse, dedup, reverse back
            self.versions.reverse();
            self.versions.dedup_by_key(|e| e.version_id);
            self.versions.reverse();
        }
        DeduplicationPolicy::Reject => {
            // Check for duplicates
            let mut seen = std::collections::HashSet::new();
            for entry in &self.versions {
                if !seen.insert(entry.version_id) {
                    return Err(StorageError::DuplicateVersionId {
                        version_id: entry.version_id
                    });
                }
            }
        }
    }

    Ok(())
}
```

## Trade-off Analysis

| Aspect | No Policy Enum | With Policy Enum |
|--------|----------------|------------------|
| **Simplicity** | ✅ Simple: Implicit first-wins | ❌ More complex API |
| **Use case coverage** | ✅ Covers WAL replay (primary) | ✅ Covers hypothetical future cases |
| **API surface** | ✅ Minimal | ❌ Larger (new enum + config) |
| **Performance** | ✅ No overhead | ⚠️ Slight overhead for policy dispatch |
| **Maintenance** | ✅ Less code | ❌ More code paths to test |
| **Flexibility** | ❌ Fixed behavior | ✅ Configurable |

## Recommendation

**Keep the current implicit "first-wins" policy** for the following reasons:

1. **YAGNI (You Aren't Gonna Need It)**: No concrete use case requires alternative policies yet
2. **WAL replay semantics are clear**: First-wins is correct for idempotent replay
3. **Version IDs should be unique**: Duplicate version IDs with different data indicate a bug, not a feature request
4. **Simplicity wins**: Adding an enum now adds complexity without clear benefit

### What We Should Do Instead

1. **Document the current behavior clearly** (see next section)
2. **Add test coverage** for edge cases with duplicates
3. **Monitor for use cases** that need different policies
4. **Design the enum later** if concrete requirements emerge

## Documentation Improvements

### Current Documentation (Line 110-113)

```rust
/// # Deduplication Policy for Recovery
/// When duplicate version IDs exist after merge, **first occurrence wins**.
/// This is correct for idempotent WAL replay: if a version is replayed twice,
/// the first insertion is kept. For non-idempotent scenarios where latest data
/// should win, callers must deduplicate before calling this method.
```

### Recommended Clarification

```rust
/// # Deduplication Policy for Recovery
///
/// After merging and sorting by `start` time, consecutive entries with duplicate
/// `version_id` are removed. The **first occurrence in the sorted vector** is kept,
/// which corresponds to the version with the earliest `start` time.
///
/// **Rationale**: This is correct for idempotent WAL replay. If a version is
/// replayed multiple times, all replayed entries have identical start times, so
/// keeping the first occurrence (arbitrary among identical entries) is safe.
///
/// **Important**: This method assumes duplicate `version_id` values represent
/// the same logical version being inserted multiple times. If duplicates represent
/// different versions (corrections), callers MUST use unique version IDs or
/// deduplicate before calling this method.
///
/// **Future**: If use cases emerge requiring "latest-wins" semantics, we may add
/// a `DeduplicationPolicy` enum. Until then, first-wins is the implicit policy.
```

## When to Revisit This Decision

Add `DeduplicationPolicy` enum if any of these scenarios emerge:

1. **Retroactive corrections** require updating existing versions with same ID
2. **Out-of-order batch processing** requires latest-received data to win
3. **Multiple data sources** conflict on the same version ID
4. **User requests** for configurable deduplication semantics

## Alternative: Version ID Should Be Unique

The cleanest solution is to **enforce version ID uniqueness**:

```rust
/// Each version MUST have a unique VersionId. If you need to update an existing
/// version, create a new VersionId and mark the old version as superseded.
```

This eliminates the deduplication problem entirely:
- WAL replay: Same version ID + same data = idempotent (first-wins is fine)
- Corrections: New version ID + updated data = no conflict
- Conflicts: Error at insertion time, forcing caller to resolve

This is the **recommended long-term direction**: treat duplicate version IDs as bugs, not features.

## Conclusion

The current implicit "first occurrence in sorted order wins" policy is:
- ✅ Correct for WAL replay (primary use case)
- ✅ Simple and maintainable
- ✅ Performant (no policy dispatch overhead)
- ⚠️ Ambiguous for hypothetical edge cases (but those shouldn't exist)

**Action items**:
1. ✅ Clarify documentation (see "Documentation Improvements")
2. ✅ Add test for edge case with same start time, different version IDs
3. ⏸️ Hold on implementing policy enum until concrete need emerges
4. ⏸️ Consider enforcing version ID uniqueness in future major version

**Decision**: No policy enum needed at this time. Document current behavior clearly and revisit if requirements change.
