# Code Review Report - "Core" 🦀

## Summary

**Status**: 🔴 **CRITICAL CORRECTNESS BUG FOUND**
**Risk Level**: Critical.

This review focused on the `ShardCoordinator` and `PersistentCommitLog` implementation in `src/storage/sharding/`. A critical correctness issue was identified where partial writes (due to crashes) corrupt the append-only log, causing permanent data loss for subsequent transactions. Additionally, severe operational risks exist due to unbounded log growth.

## Findings

### 1. Data Loss on Log Append Corruption (Correctness - Critical)

**File**: `src/storage/sharding/persistent_commit_log.rs`

**Issue**: The `PersistentCommitLog` opens the log file in append mode (`OpenOptions::new().append(true)`). However, `read_entries` stops parsing at the first error (e.g., a partial write from a crash). New writes are appended to the *physical* end of the file, after the garbage data.

**Scenario**:
1.  Node crashes while writing a commit entry (partial write).
2.  Node restarts. `read_entries` reads up to the partial write and stops (ignoring the garbage).
3.  Node appends new entries to the end of the file (after the garbage).
4.  Node restarts again. `read_entries` reads up to the partial write and STOPS.
5.  **Result**: All entries written in step 3 are effectively lost/invisible to recovery.

**Recommendation**:
-   **Immediate Fix**: In `PersistentCommitLog::new`, after `read_entries` returns the valid entries and `max_lsn`, truncate the file to the end of the valid data using `File::set_len`.
-   **Prevention**: Add a checksum/validation step that verifies the file end matches the last valid entry end.

### 2. Unbounded Log Growth / OOM (Availability - High)

**File**: `src/storage/sharding/persistent_commit_log.rs`

**Issue**: `PersistentCommitLog` defines `max_file_size` in config but never checks it. The log file grows indefinitely.
**Impact**:
-   **Disk Exhaustion**: Eventually fills the disk.
-   **Startup OOM**: `read_entries` reads the **entire file** into memory (`Vec<u8>`). A large log file will cause the process to crash on startup due to OOM.

**Recommendation**: Implement log rotation or compaction. Periodically write active (pending) transactions to a new file and atomically replace the old one.

### 3. V1 -> V2 Timestamp Inconsistency (Correctness - Medium)

**File**: `src/storage/sharding/coordinator.rs`

**Issue**: Recovering V1 transactions (which lack persisted timestamps) causes `ShardCoordinator` to generate *new* commit timestamps.
**Impact**: If a V1 transaction was partially committed with Timestamp A, and recovery commits the rest with Timestamp B, the system enters an inconsistent state.
**Mitigation**: Accept the risk as part of the V1->V2 migration (assuming V1 was experimental), but document it clearly.

### 4. Unlogged Aborts (Correctness - Low)

**File**: `src/storage/sharding/coordinator.rs`

**Issue**: `abort_distributed_transaction` attempts to log the abort decision but swallows the error if logging fails.
**Impact**: Minor. In 2PC, an unknown transaction (not prepared/committed) is implicitly aborted. The lack of an explicit Abort record is acceptable but makes debugging harder.

## Test Gaps

-   **Partial Write Recovery**: No test validates behavior when the log file ends with garbage bytes (simulating a crash during write).
-   **Large Log Files**: No test verifies behavior with log files larger than memory.
-   **Log Rotation**: No tests for rotation (as it's unimplemented).

## Minimal Patch Plan (Critical Fix)

To fix the critical data loss bug (#1), apply the following change to `PersistentCommitLog::new`:

```rust
// ... inside PersistentCommitLog::new ...
if path.exists() {
    let (entries, max_lsn, valid_len) = Self::read_entries(&path)?; // Update read_entries to return valid_len

    // ... (logic to build pending map) ...

    let file = OpenOptions::new()
        .create(true)
        .write(true) // Open with write access to allow truncation
        .append(true)
        .open(&path)?;

    // TRUNCATE to valid length to remove partial writes/garbage
    file.set_len(valid_len)?;

    // ...
}
```

This ensures new writes are appended immediately after the last valid entry, preserving the integrity of the log chain.
