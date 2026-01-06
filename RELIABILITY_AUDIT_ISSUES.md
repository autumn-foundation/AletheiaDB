# Reliability Audit - GitHub Issues to Create

This file contains all GitHub issues identified in the NASA-grade reliability audit.
Please create these issues manually or use the GitHub web interface.

---

## Issue 1: [RELIABILITY-P0-CRITICAL] Database constructor can panic on WAL initialization failure

**Labels:** `reliability`, `automated-audit`, `P0-CRITICAL`, `failure-mode-elimination`

### Description

#### Location
`src/db.rs:59`

#### Current State
The `GallifreyDB::with_config()` constructor uses `.expect("Failed to create WAL")` when creating the Write-Ahead Log. This causes the entire application to panic if WAL initialization fails.

```rust
pub fn with_config(config: AnchorConfig) -> Self {
    let wal = WriteAheadLog::new(WalConfig::default()).expect("Failed to create WAL");
    // ...
}
```

#### Risk Assessment
- **Failure Mode:** Panic during database initialization
- **Impact:** Complete application crash, no graceful degradation possible
- **Likelihood:** High in constrained environments (disk full, permissions, I/O errors)
- **Severity:** CRITICAL - Violates NASA-grade reliability standards

#### Recommended Fix
Change constructor signature to return `Result<Self>`:

```rust
pub fn with_config(config: AnchorConfig) -> Result<Self> {
    let wal = WriteAheadLog::new(WalConfig::default())?;
    Ok(GallifreyDB {
        // ... rest of initialization
    })
}
```

Update all call sites to handle the Result:
- `GallifreyDB::new()` → `GallifreyDB::new()?`
- Add error context for user-facing errors

#### Verification
- [ ] Constructor returns Result<Self>
- [ ] All call sites updated to handle Result
- [ ] Integration test covering WAL initialization failure
- [ ] Error messages provide actionable guidance

#### Effort Estimate
**LOW** (2-4 hours) - Straightforward API change with clear fix pattern

---

## Issue 2: [RELIABILITY-P0-CRITICAL] Complete absence of logging/tracing infrastructure

**Labels:** `reliability`, `automated-audit`, `P0-CRITICAL`, `observability`

### Description

#### Location
Codebase-wide (no files use `tracing` or `log` crates)

#### Current State
GallifreyDB has **ZERO logging or tracing infrastructure**. There are no:
- Log statements for critical operations
- Trace spans for performance monitoring
- Error context in failure paths
- Audit trail for data mutations (beyond WAL)
- Metrics collection for observability

Search results: `grep -r "tracing::|log::" src/` returned **0 matches**.

#### Risk Assessment
- **Failure Mode:** Blind operation in production, impossible debugging
- **Impact:**
  - Cannot diagnose production issues
  - Cannot identify performance bottlenecks
  - Cannot detect gradual degradation
  - No compliance audit trail
  - Operations team has zero visibility
- **Likelihood:** CERTAIN - Already exists
- **Severity:** CRITICAL - Violates mission-critical system requirements

#### Recommended Fix

##### Phase 1: Immediate (Critical Paths)
Add `tracing` crate with spans for:

1. **Transaction Operations**
```rust
#[instrument(skip(self))]
pub fn commit(&mut self) -> Result<()> {
    info!("Committing transaction");
    // ...
    if result.is_err() {
        error!("Transaction commit failed: {:?}", result);
    }
}
```

2. **WAL Operations**
```rust
#[instrument(skip(self, operation))]
fn log_operation(&mut self, operation: Operation) -> Result<()> {
    debug!("Logging WAL operation: {:?}", operation);
    // ...
}
```

3. **Index Updates**
```rust
#[instrument(skip(self))]
pub fn add_vector(&self, node_id: NodeId, vector: &[f32]) -> Result<()> {
    trace!(node_id = %node_id, dims = vector.len(), "Adding vector to index");
    // ...
}
```

##### Phase 2: Structured Metrics
- Operation latency histograms (p50, p95, p99)
- Error rates by type
- Resource usage (lock contention, allocation sizes)
- Queue depths and backpressure indicators

##### Phase 3: Distributed Tracing
- OpenTelemetry integration for temporal queries
- Cross-component correlation IDs

#### Verification
- [ ] `tracing` crate added to dependencies
- [ ] Spans added to all public API methods
- [ ] Error paths include diagnostic context
- [ ] Performance-critical paths have trace_span! guards
- [ ] Integration tests verify span emission
- [ ] Documentation updated with observability guide

#### Effort Estimate
**HIGH** (1-2 weeks) - Critical infrastructure gap requiring systematic addition

**Immediate:** 2-3 days for Phase 1 (critical paths)
**Short-term:** 1 week for Phase 2 (structured metrics)

**This is the #1 blocker for production deployment.**

---

## Issue 3: [RELIABILITY-P1-HIGH] Lock poisoning causes panic instead of graceful error handling

**Labels:** `reliability`, `automated-audit`, `P1-HIGH`, `failure-mode-elimination`

### Description

#### Location
- `src/api/transaction/write_tx.rs:178`
- `src/api/transaction/write_tx.rs:188`

#### Current State
Lock acquisition uses `.expect()` on poisoned mutex, causing panic that cascades to all threads:

```rust
let mut ts = self
    .current_timestamp
    .lock()
    .expect("timestamp lock poisoned - unrecoverable state");  // LINE 178

let mut wal = self
    .wal
    .lock()
    .expect("WAL lock poisoned - unrecoverable state");  // LINE 188
```

#### Risk Assessment
- **Failure Mode:** Single thread panic poisons mutex, crashes all threads using database
- **Impact:** Cascading failure - one panic takes down entire database
- **Likelihood:** Medium (requires thread panic while holding lock, but possible)
- **Severity:** HIGH - Violates isolation and fault tolerance requirements

#### Why This is Wrong
While the comments say "unrecoverable state", **lock poisoning is recoverable** in many scenarios:
1. The data may still be valid (panic occurred before mutation)
2. The database could enter read-only mode
3. Other threads could continue operating on unaffected resources
4. Graceful shutdown is preferable to crash

#### Recommended Fix

##### Option 1: Return Error (Preferred)
```rust
let mut ts = self
    .current_timestamp
    .lock()
    .map_err(|_| StorageError::LockPoisoned("current_timestamp"))?;

let mut wal = self
    .wal
    .lock()
    .map_err(|_| StorageError::LockPoisoned("wal"))?;
```

Add to `StorageError`:
```rust
#[error("Lock poisoned: {0}. Database state may be inconsistent.")]
LockPoisoned(&'static str),
```

##### Option 2: Attempt Recovery (Advanced)
```rust
let mut ts = match self.current_timestamp.lock() {
    Ok(guard) => guard,
    Err(poisoned) => {
        warn!("Timestamp lock poisoned, attempting recovery");
        // Decide: use the potentially-inconsistent data or fail?
        poisoned.into_inner()  // Risk: data might be inconsistent
        // OR
        return Err(StorageError::LockPoisoned("current_timestamp"));
    }
};
```

##### Option 3: Implement Fail-Safe Mode
- Detect poisoned locks
- Enter read-only mode
- Prevent new writes but allow reads
- Require explicit recovery operation

#### Verification
- [ ] All `.expect()` on locks replaced with `?` or `map_err()`
- [ ] `StorageError::LockPoisoned` variant added
- [ ] Integration test: panic in thread holding lock, verify other threads get error
- [ ] Documentation explains lock poisoning recovery strategy
- [ ] Stress test with concurrent panics

#### Effort Estimate
**MEDIUM** (3-5 days) - Requires careful error propagation and testing

---

## Issue 4: [RELIABILITY-P1-HIGH] Extensive use of .unwrap() in production code paths

**Labels:** `reliability`, `automated-audit`, `P1-HIGH`, `failure-mode-elimination`

### Description

#### Location
30 files affected, with 100+ total occurrences of `.unwrap()`

Key files:
- `src/db.rs` (100+ calls, many in tests but some in production paths)
- `src/storage/current.rs`
- `src/index/current.rs`
- `src/core/interning.rs`
- `src/api/transaction/*.rs`
- `src/core/*.rs` (graph.rs, id.rs, property.rs, temporal.rs, vector.rs)
- `src/index/*.rs` (adjacency.rs, temporal.rs, vector/*.rs)
- `src/storage/*.rs` (historical.rs, persistence.rs, version.rs, wal.rs)
- `src/embeddings/*.rs`

#### Current State
Widespread use of `.unwrap()` throughout the codebase. While many are in test code, production code paths also use this panic-inducing pattern.

Example from production code:
```rust
// Many similar patterns throughout codebase
let node = db.get_node(node_id).unwrap();  // Panics if node doesn't exist
```

#### Risk Assessment
- **Failure Mode:** Panic on any None/Err value
- **Impact:** Application crash, no graceful error handling
- **Likelihood:** High - any unexpected None/Err triggers panic
- **Severity:** HIGH - Violates reliability requirements

#### Recommended Fix

##### Step 1: Audit and Categorize
1. Identify all `.unwrap()` calls in `src/` (excluding tests)
2. Categorize:
   - **Test code** - acceptable, mark with `// Test only`
   - **Infallible cases** - replace with `.expect("reason it's safe")`
   - **Production code** - replace with proper error handling

##### Step 2: Replace Production Unwraps
Replace with proper error handling:

```rust
// Before
let node = db.get_node(node_id).unwrap();

// After
let node = db.get_node(node_id)?;
// or
let node = match db.get_node(node_id) {
    Ok(n) => n,
    Err(e) => {
        error!("Failed to get node {}: {}", node_id, e);
        return Err(e);
    }
};
```

##### Step 3: Add Linter Rule
Add to `.cargo/config.toml` or CI:
```toml
[target.'cfg(not(test))']
rustflags = ["-Dclippy::unwrap_used"]
```

This prevents new `.unwrap()` calls in production code while allowing them in tests.

#### Verification
- [ ] All production `.unwrap()` calls audited
- [ ] Test code marked with `// Test only` or similar
- [ ] Production paths use `?` or explicit error handling
- [ ] Linter rule added to prevent future violations
- [ ] Integration tests verify error paths work correctly

#### Effort Estimate
**HIGH** (2-3 weeks) - Requires systematic review and refactoring

**Breakdown:**
- Audit phase: 3-5 days
- Refactoring: 1-2 weeks
- Testing: 3-5 days

---

## Issue 5: [RELIABILITY-P2-MEDIUM] Limited use of #[must_use] allows silent error ignoring

**Labels:** `reliability`, `automated-audit`, `P2-MEDIUM`, `api-robustness`

### Description

#### Location
Codebase-wide - only 4 uses of `#[must_use]` found (all in `src/index/vector/mod.rs`)

#### Current State
Public APIs return `Result` but lack `#[must_use]` annotations, allowing callers to silently ignore errors without compiler warnings.

```rust
// Current: No warning if Result is ignored
pub fn create_node(&self, label: &str, properties: PropertyMap) -> Result<NodeId> {
    // ...
}

// Caller can ignore error without warning:
let _ = db.create_node("Person", props);  // Silent failure!
```

#### Risk Assessment
- **Failure Mode:** Silent error ignoring, no user awareness of failure
- **Impact:** Data loss, inconsistent state, hard-to-debug issues
- **Likelihood:** Medium - developers may accidentally ignore Results
- **Severity:** MEDIUM - Reduces robustness

#### Recommended Fix

Add `#[must_use]` to all public Result-returning functions:

```rust
#[must_use = "Database operation result must be checked"]
pub fn create_node(&self, label: &str, properties: PropertyMap) -> Result<NodeId> {
    // ...
}

// For critical operations, be more specific:
#[must_use = "Ignoring transaction commit may cause data loss"]
pub fn commit(&mut self) -> Result<()> {
    // ...
}
```

##### Files to Update
All public APIs in:
- `src/db.rs` - All public methods
- `src/storage/*.rs` - Public storage operations
- `src/index/*.rs` - Index operations
- `src/api/transaction/*.rs` - Transaction operations

#### Verification
- [ ] All public Result-returning functions have `#[must_use]`
- [ ] Critical operations have descriptive messages
- [ ] Tests verify compiler warnings for ignored Results
- [ ] Documentation mentions error handling requirements

#### Effort Estimate
**LOW** (1 day) - Systematic annotation addition

Can be done incrementally with a simple script or search-replace.

---

## Issue 6: [RELIABILITY-P2-MEDIUM] Incomplete features marked with TODO comments

**Labels:** `reliability`, `automated-audit`, `P2-MEDIUM`, `technical-debt`

### Description

#### Location
4 TODO comments indicating incomplete features:

1. `src/storage/current.rs:301` - "TODO: Add cascade delete option"
2. `src/storage/persistence.rs:494` - "TODO: Implement WAL operation replay"
3. `src/index/vector/temporal.rs:887` - "TODO: This requires getting the vector for the node..."
4. `src/index/vector/temporal.rs:999` - "TODO: This requires retrieving node embeddings..."

#### Current State
Features marked as TODO indicate incomplete implementation or deferred functionality. This creates maintenance traps and potential user confusion.

#### Risk Assessment
- **Failure Mode:** Users depend on incomplete features, unexpected behavior
- **Impact:** Bugs, data inconsistencies, poor user experience
- **Likelihood:** Medium - depends on feature usage
- **Severity:** MEDIUM - Reduces reliability and maintainability

#### Recommended Fix

For each TODO:

##### 1. Cascade Delete (src/storage/current.rs:301)
**Decision Required:** Should node deletion cascade to edges?
- **Option A:** Implement cascade delete with configuration
- **Option B:** Document current behavior, remove TODO if intentional

##### 2. WAL Operation Replay (src/storage/persistence.rs:494)
**Critical Feature:** This affects crash recovery!
- Implement full WAL replay logic
- Add integration test for recovery scenarios
- Document recovery guarantees

##### 3-4. Temporal Vector Queries (src/index/vector/temporal.rs)
**Phase 3 Feature:** Part of vector search roadmap
- Either implement or move to backlog issue
- Don't leave incomplete code in main path
- Add feature flag if not ready for production

#### Verification
- [ ] Each TODO resolved (implemented or removed)
- [ ] Tests cover the implemented features
- [ ] Documentation explains behavior
- [ ] No TODOs remain in critical paths

#### Effort Estimate
**MEDIUM** (1 week) - Depends on feature complexity

**Breakdown:**
- Cascade delete: 1-2 days
- WAL replay: 2-3 days (CRITICAL)
- Temporal vectors: Remove or backlog (Phase 3 feature)

---

## Issue 7: [RELIABILITY-P2-MEDIUM] No resource limits on unbounded allocations

**Labels:** `reliability`, `automated-audit`, `P2-MEDIUM`, `resource-management`

### Description

#### Location
Multiple locations with potential unbounded allocations:
- HashMap usage in 5 files (write_buffer.rs, property.rs, adjacency.rs, historical.rs, version.rs)
- Vec allocations throughout codebase
- No visible string length limits on inputs

#### Current State
No visible limits on:
- PropertyMap size (number of properties, total bytes)
- Label/property key string lengths
- HashMap growth in buffers and indexes
- Vec allocations without capacity bounds

#### Risk Assessment
- **Failure Mode:** Memory exhaustion via unbounded growth
- **Impact:** OOM kills, DoS attacks, system instability
- **Likelihood:** Medium - requires malicious or unusual input
- **Severity:** MEDIUM - Can cause system-wide issues

#### Recommended Fix

##### Add Configurable Limits

```rust
pub struct DatabaseConfig {
    /// Maximum label length in bytes (default: 1KB)
    pub max_label_length: usize,

    /// Maximum property key length (default: 256 bytes)
    pub max_property_key_length: usize,

    /// Maximum property count per entity (default: 1000)
    pub max_properties_per_entity: usize,

    /// Maximum PropertyMap size in bytes (default: 64KB)
    pub max_property_map_size: usize,

    /// Maximum write buffer size (default: 10MB)
    pub max_write_buffer_size: usize,
}
```

##### Validate at API Boundaries

```rust
pub fn create_node(&self, label: &str, properties: PropertyMap) -> Result<NodeId> {
    // Validate inputs
    if label.len() > self.config.max_label_length {
        return Err(StorageError::LabelTooLong {
            length: label.len(),
            max: self.config.max_label_length
        });
    }

    if properties.len() > self.config.max_properties_per_entity {
        return Err(StorageError::TooManyProperties {
            count: properties.len(),
            max: self.config.max_properties_per_entity
        });
    }

    // ... proceed with operation
}
```

##### Add HashMap Size Tracking

```rust
impl WriteBuffer {
    fn check_capacity(&self) -> Result<()> {
        let current_size = self.estimate_size();
        if current_size > self.max_buffer_size {
            return Err(StorageError::BufferFull {
                current: current_size,
                max: self.max_buffer_size
            });
        }
        Ok(())
    }
}
```

#### Verification
- [ ] Config struct added with all limits
- [ ] Validation at all public API entry points
- [ ] Specific error types for limit violations
- [ ] Integration tests verify limit enforcement
- [ ] Documentation explains limits and rationale
- [ ] Performance benchmarks show overhead is acceptable

#### Effort Estimate
**MEDIUM** (1 week) - Systematic addition of validation layers

---

## Issue 8: [RELIABILITY-P3-LOW] Large functions exceed 1000 characters (complexity)

**Labels:** `reliability`, `automated-audit`, `P3-LOW`, `code-quality`

### Description

#### Location
31 files contain functions exceeding 1000 characters, indicating potential complexity issues:

Key files likely containing large functions:
- `src/db.rs`
- `src/storage/wal.rs`
- `src/api/transaction/write_tx.rs`
- `src/index/vector/hnsw.rs`
- `src/storage/persistence.rs`
- `src/core/property.rs`

#### Current State
Large functions (>80 lines / >1000 characters) are harder to:
- Understand and maintain
- Test comprehensively
- Review for correctness
- Refactor safely

#### Risk Assessment
- **Failure Mode:** Bugs hidden in complex logic, difficult maintenance
- **Impact:** Increased bug rate, slower development, technical debt
- **Likelihood:** Low - doesn't cause immediate failures
- **Severity:** LOW - Code quality issue, not reliability risk

#### Recommended Fix

##### Refactoring Strategy

1. **Identify Functions >80 Lines**
```bash
# Find large functions
rg -U "fn \w+.*\{[\s\S]{1500,}" --files-with-matches src/
```

2. **Extract Logical Blocks**
Break functions into smaller, testable units:

```rust
// Before: 150-line function
pub fn commit(&mut self) -> Result<()> {
    // ... 50 lines of validation
    // ... 50 lines of WAL logging
    // ... 50 lines of index updates
}

// After: Decomposed into smaller functions
pub fn commit(&mut self) -> Result<()> {
    self.validate_transaction()?;
    self.log_to_wal()?;
    self.update_indexes()?;
    Ok(())
}

fn validate_transaction(&self) -> Result<()> { /* 20 lines */ }
fn log_to_wal(&mut self) -> Result<()> { /* 25 lines */ }
fn update_indexes(&mut self) -> Result<()> { /* 25 lines */ }
```

3. **Benefits**
- Each function has single responsibility
- Easier to unit test
- Self-documenting code
- Easier to optimize hot paths

#### Verification
- [ ] All functions <80 lines (with rare justified exceptions)
- [ ] Each function has single clear purpose
- [ ] Unit tests for extracted functions
- [ ] Code review confirms improved readability

#### Effort Estimate
**LOW** (3-5 days) - Gradual refactoring as part of regular development

**Note:** This is a gradual improvement task, not urgent. Can be done opportunistically during feature work.

---

## Issue 9: [RELIABILITY-P3-LOW] Add property-based tests for temporal invariants

**Labels:** `reliability`, `automated-audit`, `P3-LOW`, `testing`

### Description

#### Location
Codebase-wide - missing property-based testing for temporal logic

#### Current State
Test coverage is good (86%+) but lacks property-based tests that verify temporal invariants hold under all conditions:
- Transaction time monotonicity
- Valid time consistency
- Temporal paradox prevention
- MVCC snapshot isolation
- Anchor+delta reconstruction correctness

#### Risk Assessment
- **Failure Mode:** Temporal invariants violated in edge cases
- **Impact:** Data corruption, temporal inconsistencies, wrong query results
- **Likelihood:** Low - current tests provide good coverage
- **Severity:** LOW - Additional safety layer, not critical gap

#### Recommended Fix

Add `proptest` crate for property-based testing:

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn transaction_time_is_monotonic(
        operations in prop::collection::vec(operation_strategy(), 1..100)
    ) {
        let db = GallifreyDB::new();
        let mut last_timestamp = 0;

        for op in operations {
            let tx = db.write_transaction()?;
            // Execute operation
            let ts = tx.commit()?;

            // Property: Transaction time always increases
            assert!(ts > last_timestamp);
            last_timestamp = ts;
        }
    }

    #[test]
    fn time_travel_is_consistent(
        operations in prop::collection::vec(operation_strategy(), 1..50),
        query_time in 0u64..1000000
    ) {
        let db = GallifreyDB::new();
        let mut history = vec![];

        // Execute operations, recording state at each timestamp
        for op in operations {
            let state_before = db.snapshot();
            let tx = db.write_transaction()?;
            // ... execute op
            let ts = tx.commit()?;
            history.push((ts, state_before));
        }

        // Property: Querying past time returns exact historical state
        for (ts, expected_state) in history {
            let historical = db.as_of(ts);
            assert_eq!(historical.snapshot(), expected_state);
        }
    }

    #[test]
    fn no_temporal_paradoxes(
        node_id in node_id_strategy(),
        create_time in 100u64..1000,
        delete_time in 0u64..100
    ) {
        let db = GallifreyDB::new();

        // Property: Cannot delete entity before it was created
        let result = db.delete_node_at_time(node_id, delete_time, create_time);
        assert!(matches!(result, Err(StorageError::TemporalParadox { .. })));
    }
}
```

#### Test Categories

1. **Monotonicity Tests**
   - Transaction time never decreases
   - Version numbers always increase

2. **Consistency Tests**
   - Time-travel returns exact historical state
   - Snapshot isolation guarantees

3. **Invariant Tests**
   - No temporal paradoxes
   - Valid time ranges are consistent
   - Anchor+delta reconstruction is correct

4. **Concurrency Tests**
   - Concurrent transactions maintain MVCC guarantees
   - Lock-free operations are truly lock-free

#### Verification
- [ ] `proptest` added to dev-dependencies
- [ ] Property tests for all temporal invariants
- [ ] Tests run in CI with reasonable iteration count
- [ ] Documentation explains tested properties
- [ ] Shrinking works correctly for failures

#### Effort Estimate
**MEDIUM** (1 week) - Comprehensive temporal property testing

---

## Summary Statistics

**Total Issues:** 9

**By Priority:**
- P0-CRITICAL: 2 issues
- P1-HIGH: 2 issues
- P2-MEDIUM: 4 issues
- P3-LOW: 1 issue

**By Category:**
- Failure Mode Elimination: 3 issues
- Observability: 1 issue
- API Robustness: 1 issue
- Resource Management: 1 issue
- Technical Debt: 1 issue
- Code Quality: 1 issue
- Testing: 1 issue

**Estimated Total Effort:**
- P0 issues: 2-3 weeks
- P1 issues: 3-4 weeks
- P2 issues: 3-4 weeks
- P3 issues: 2 weeks

**Total: 10-13 weeks** for complete remediation

---

## Recommended Prioritization

### Week 1-2: Critical Reliability
1. Issue #2: Add logging/tracing infrastructure (P0)
2. Issue #1: Fix constructor panic (P0)
3. Issue #3: Fix lock poisoning (P1)

### Week 3-5: High-Priority Safety
4. Issue #4: Audit all .unwrap() calls (P1)
5. Issue #5: Add #[must_use] annotations (P2)

### Week 6-8: Robustness
6. Issue #7: Add resource limits (P2)
7. Issue #6: Complete TODO features (P2)

### Week 9+: Quality Improvements
8. Issue #8: Refactor large functions (P3)
9. Issue #9: Add property-based tests (P3)

---

**Note:** This file contains the complete specification for all reliability audit issues.
Create these issues in GitHub to track remediation progress.

See `RELIABILITY_AUDIT_REPORT.md` for full audit context and findings.
