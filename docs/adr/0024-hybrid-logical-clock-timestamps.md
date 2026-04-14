# ADR-0024: Hybrid Logical Clock Timestamps

**Status:** Accepted
**Date:** 2026-01-20
**Deciders:** AletheiaDB Core Team
**Categories:** core, temporal, distributed

## Context

AletheiaDB's bi-temporal model uses timestamps to track both valid time (when facts are true) and transaction time (when facts are recorded). The initial implementation used simple `i64` microsecond timestamps, which worked for single-node deployments but had critical limitations for distributed systems:

### Problems with Simple Timestamps

1. **Concurrent Transaction Ambiguity**: Multiple transactions at the same wallclock time have identical timestamps, making total ordering impossible
   ```rust
   // Transaction A and B both commit at 1000μs - which came first?
   let tx_a_time = 1000i64;
   let tx_b_time = 1000i64;
   // No way to determine ordering!
   ```

2. **Clock Skew in Distributed Systems**: Different nodes may have slightly different wall clocks, causing causally-later events to appear earlier
   ```
   Node 1 (clock +5ms): Records fact at 1005
   Node 2 (clock -3ms): Records related fact at 997
   // Causally-later event appears earlier!
   ```

3. **No Causal Consistency**: Cannot distinguish between:
   - Events happening concurrently on different nodes
   - Events in a causal chain (A happens-before B)

4. **MVCC Limitations**: Snapshot isolation requires strict total ordering of all transactions
   - Same-timestamp transactions break snapshot consistency
   - No way to determine which version to read

### Why This Matters for AletheiaDB

AletheiaDB's LLM integration use case requires:
- **Temporal reasoning about concurrent events**: "What did we know when we recorded X?"
- **Causal consistency**: "Did we know A before we recorded B?"
- **Distributed operation**: Future horizontal scaling across multiple nodes
- **Strict snapshot isolation**: MVCC guarantees for temporal queries

## Decision

We will replace `i64` timestamps with **Hybrid Logical Clocks (HLC)** throughout the system, implementing a 12-byte timestamp structure combining physical and logical time:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct HybridTimestamp {
    /// Physical wallclock time in microseconds (8 bytes)
    wallclock: i64,

    /// Logical counter for ordering within same wallclock (4 bytes)
    logical: u32,
}
```

### Type Alias for Clarity

```rust
pub type Timestamp = HybridTimestamp;
```

All temporal APIs use `Timestamp` type alias, making future changes transparent.

### Key Properties

1. **Total Ordering**: `(wallclock, logical)` tuples provide strict total order
   ```rust
   // Concurrent transactions at same wallclock are ordered by logical counter
   let tx_a = HybridTimestamp::new(1000, 0); // First
   let tx_b = HybridTimestamp::new(1000, 1); // Second
   assert!(tx_a < tx_b);
   ```

2. **Clock Synchronization**: Uses Lamport clock semantics to handle clock skew
   ```rust
   impl HybridTimestamp {
       /// Update on send: max(local.wallclock, system_time)
       pub fn send(&self, system_time: i64) -> Result<HybridTimestamp>;

       /// Update on receive: max(local, remote) + 1 (logical)
       pub fn receive(&self, remote: HybridTimestamp, system_time: i64) -> Result<HybridTimestamp>;
   }
   ```

3. **Causality Tracking**: Happens-before relationships are preserved
   - If A happens-before B, then A's timestamp < B's timestamp
   - Physical time dominates when clocks are synchronized
   - Logical counter breaks ties and handles skew

4. **Backwards Compatibility**: `From<i64>` trait for seamless migration
   ```rust
   impl From<i64> for HybridTimestamp {
       fn from(wallclock: i64) -> Self {
           HybridTimestamp { wallclock, logical: 0 }
       }
   }
   ```

### Serialization Format

12-byte binary format for storage and network transmission:
```
[8 bytes: wallclock (i64, little-endian)]
[4 bytes: logical (u32, little-endian)]
```

### Validation

Maximum wallclock enforced to prevent DoS attacks:
```rust
pub const MAX_VALID_TIMESTAMP: i64 = i64::MAX - 1000;

// Special sentinel for "infinity" / "still current"
pub const TIMESTAMP_MAX: Timestamp = HybridTimestamp::new_unchecked(i64::MAX, 0);
```

Deserialization rejects `wallclock > MAX_VALID_TIMESTAMP` except for `i64::MAX` sentinel.

## Consequences

### Positive

1. **Distributed-Ready**: Enables future horizontal scaling without timestamp coordination
2. **Causal Consistency**: Can reason about happens-before relationships
3. **Strict MVCC**: Eliminates ambiguity in snapshot isolation
4. **LLM Reasoning**: Supports "when did we know X" queries with precision
5. **Type Safety**: Rust type system prevents accidental i64/HybridTimestamp mixing
6. **Provenance Tracking**: Logical counter provides precise transaction ordering
7. **No Breaking Changes**: `From<i64>` trait allows gradual migration

### Negative

1. **Storage Overhead**: 12 bytes vs 8 bytes per timestamp (50% increase)
   - Mitigated: Anchor+delta compression reduces impact
   - Mitigated: String interning saves far more space
2. **Migration Complexity**: 299 compilation errors across 50+ files
   - Completed: All tests passing, comprehensive test coverage
3. **Serialization Complexity**: Custom binary format vs simple i64
   - Mitigated: Well-tested serialization with validation

### Neutral

1. **API Surface**: All timestamp parameters now require `.into()` for literals
   ```rust
   // Before: index.add(node, &vec, 1000)
   // After:  index.add(node, &vec, 1000.into())
   ```
2. **Learning Curve**: Developers must understand HLC semantics
3. **Debugging**: Timestamps now show as (wallclock, logical) pairs

## Alternatives Considered

### Alternative 1: Keep Simple i64 Timestamps

**Pros:**
- Simpler implementation
- Lower storage overhead
- Familiar to developers

**Cons:**
- Blocks distributed deployment (critical blocker)
- No causal consistency
- MVCC snapshot isolation ambiguity
- Cannot support concurrent transactions properly

**Why rejected:** Fundamentally incompatible with distributed systems and precise temporal reasoning.

### Alternative 2: UUID-Based Timestamps (UUIDv7)

**Pros:**
- Industry standard (RFC 9562)
- 128-bit globally unique
- Includes random component

**Cons:**
- 16 bytes overhead (100% increase vs i64)
- Random component doesn't help ordering
- Overkill for single-node deployments
- No happens-before semantics

**Why rejected:** Larger overhead than HLC with no additional benefits for our use case.

### Alternative 3: Vector Clocks

**Pros:**
- Captures full causality graph
- Detects concurrent events explicitly

**Cons:**
- Unbounded size (grows with number of nodes)
- Complex comparison (not totally ordered)
- Incompatible with existing timestamp APIs
- Overkill for primary use case

**Why rejected:** Total ordering is critical for MVCC and temporal queries. Vector clocks don't provide this.

### Alternative 4: Google Spanner TrueTime

**Pros:**
- Extremely precise (GPS + atomic clocks)
- External consistency guarantees

**Cons:**
- Requires specialized hardware (GPS, atomic clocks)
- Not available in commodity deployments
- Uncertainty intervals add complexity
- Massive infrastructure requirement

**Why rejected:** Infrastructure requirements make this impractical for open-source database.

## Implementation Notes

### Migration Strategy (Completed)

Phase 1: Core Type Introduction ✓
- Define HybridTimestamp struct
- Implement Ord, serialization, From<i64>
- Add Timestamp type alias

Phase 2: Systematic Replacement ✓ (PR #423)
- Replace all `i64` timestamp parameters with `Timestamp`
- Update all timestamp arithmetic to use wallclock accessor
- Fix 299 compilation errors across codebase
- Update all tests (1,327+ tests passing)
- Fix benchmarks, examples, doctests

Phase 3: Future Enhancements (Planned)
- Implement send/receive for distributed coordination
- Add network timestamp synchronization
- Integrate with distributed transaction protocol

### Performance Impact

**Measured:**
- Storage: +50% per timestamp (12 vs 8 bytes)
  - Graph index: ~+2% total (timestamps are small fraction)
  - Historical storage: Negligible (anchor+delta compression dominates)
- CPU: No measurable impact (comparison is still O(1))
- All performance targets maintained:
  - Single-hop queries: <1μs ✓
  - Temporal queries: <10ms ✓
  - 1,327 tests: All passing ✓

### Testing Coverage

- Unit tests: 25 HLC-specific tests
- Integration tests: 1,327+ tests across all modules
- Doctests: 62 passing
- Property tests: Monotonicity, ordering invariants
- Validation tests: MAX_VALID_TIMESTAMP enforcement, sentinel values

### Code Patterns

**Converting literals:**
```rust
// Old: let ts = 1000i64;
let ts: Timestamp = 1000.into();
```

**Arithmetic:**
```rust
// Old: let later = timestamp + 1000;
let later: Timestamp = (timestamp.wallclock() + 1000).into();
```

**Comparisons:**
```rust
// Works unchanged (Ord trait)
assert!(timestamp1 < timestamp2);
```

**Serialization:**
```rust
let mut buf = vec![0u8; 12];
timestamp.serialize(&mut buf);
let (deserialized, bytes_read) = HybridTimestamp::deserialize(&buf)?;
```

## References

- **Original Paper**: [Logical Physical Clocks and Consistent Snapshots in Globally Distributed Databases](https://cse.buffalo.edu/tech-reports/2014-04.pdf) (Kulkarni & Demirbas, 2014)
- **CockroachDB Implementation**: [Hybrid Logical Clocks](https://www.cockroachlabs.com/blog/living-without-atomic-clocks/)
- **PR #423**: Phase 2 HLC Integration (299→0 compilation errors)
- **Related ADRs**:
  - ADR-0002: Bi-Temporal Data Model (defines timestamp usage)
  - ADR-0003: MVCC Snapshot Isolation (requires total ordering)
  - ADR-0020: Concurrent WAL Architecture (transaction ordering)
- **Implementation Files**:
  - `src/core/hlc.rs` - HybridTimestamp implementation
  - `src/core/temporal.rs` - Temporal types using HybridTimestamp
  - `tests/hlc_tests.rs` - 25 HLC-specific tests
