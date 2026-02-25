# Code Review Report - "Core" 🦀

## Summary

**Status**: 🚨 **CRITICAL RISKS DETECTED** 🚨
**Risk Level**: **Critical** (Functionality Gaps & Data Consistency Risks)

The review identified a critical discrepancy between documentation and implementation in the Sharding module, alongside a high-severity consistency issue in distributed transactions.

## Findings

### 1. Sharding Functionality Stubbed (Critical)

**File**: `src/storage/sharding/coordinator.rs`

**Issue**: The `ShardCoordinator` relies on a local `ShardConnection` struct that stubs all network operations. The `prepare`, `commit`, and `abort` methods simply return `Ok(())` without communicating with any remote shard.

**Impact**:
- **Non-Functional Distributed Mode**: Multi-node deployments will **not work**. Transactions will appear to succeed locally but will not propagate to other nodes.
- **Data Loss**: Data meant for other shards is not persisted on them.
- **Misleading Status**: The documentation claims Sharding is "Complete ✅", which is factually incorrect for the current implementation path.

**Reasoning**: `ShardCoordinator::new` instantiates `ShardConnection` directly instead of using the `HttpShardClient` (found in `rpc_client.rs`) or the `ShardClient` trait. `ShardConnection` contains explicit comments: `"In a real implementation, this would make an RPC call"`.

**Recommendation**:
- Update `ShardCoordinator` to hold `Box<dyn ShardClient>`.
- In `ShardCoordinator::new`, verify the `sharding-rpc` feature and instantiate `HttpShardClient`.
- Update README to reflect accurate status (e.g., "Architecture Complete, Networking WIP").

### 2. Ambiguous Commit / "Ghost Success" (High)

**File**: `src/storage/sharding/coordinator.rs`

**Issue**: In `commit_distributed_transaction`, the function returns a `CommitFailed` error to the caller if network propagation fails, **even if the commit decision was already successfully logged to the WAL**.

**Impact**:
- **State Divergence**: The client believes the transaction failed (and might retry/rollback logic), but the system's recovery mechanism will eventually commit the transaction.
- **Data Integrity**: Retrying a transaction that actually succeeded (but reported failure) can lead to duplicate data or logical inconsistencies depending on application logic.

**Reasoning**: The "Point of No Return" is `log.log_commit`. Any failure after this point is a *liveness* issue (propagation delay), not a *correctness* failure (abort). Returning `Err(CommitFailed)` implies the latter.

**Recommendation**:
- Return a distinct error variant (e.g., `CommitPending` or `CommitAcceptedButNotPropagated`) when the decision is logged.
- Implement a background "sweeper" task in `ShardCoordinator` to actively retry these stuck commits without waiting for a full system restart.

### 3. HNSW Ingestion Scalability Bottleneck (Medium)

**File**: `src/index/vector/hnsw.rs`

**Issue**: The `add` method acquires a global `RwLock::write` lock on the inner `usearch` index (`self.inner.write()`).

**Impact**: Vector ingestion is strictly serialized. Parallel ingestion threads will block each other, limiting write throughput to single-core performance.

**Reasoning**: The implementation opts for "redundant safety" by wrapping the thread-safe C++ library in a Rust RwLock.

**Recommendation**: Investigate removing the global write lock for `add` if `usearch` handles concurrent inserts safely (it claims to), or use finer-grained locking.

## Test Gaps

- **Integration Tests**: No tests verify actual network communication for sharding (impossible given the stubbed implementation).
- **Failure Recovery**: No tests cover the "Ambiguous Commit" scenario where a client must handle a "Pending" state.

## Conclusion

The Sharding module requires immediate attention to wire up the actual RPC layer. Until then, it should be marked as "Experimental" or "In Progress". The Ambiguous Commit issue requires a logic update to safe-guard client assumptions.
