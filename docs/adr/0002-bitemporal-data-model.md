# ADR-0002: Bi-Temporal Data Model

**Status:** Accepted
**Date:** 2024-12-31
**Deciders:** GallifreyDB Core Team
**Categories:** core, temporal

## Context

GallifreyDB's primary goal is enabling LLMs to reason about knowledge evolution over time. To support this, we need to answer questions like:
- "What did we know about X at time T?" (historical knowledge state)
- "When did we learn about X?" (provenance tracking)
- "How has our understanding of X changed?" (knowledge evolution)
- "What was true about X during period P?" (validity tracking)

A single temporal dimension cannot answer all these questions:
- **Transaction time only**: Can't represent retroactive corrections ("we now know X was true in 2020")
- **Valid time only**: Can't track when information was recorded (provenance)

We need both dimensions to fully capture knowledge evolution.

## Decision

We will implement a **bi-temporal data model** tracking two independent time dimensions:

### Time Dimensions

```rust
pub struct BiTemporalInterval {
    /// When the fact was/is true in the real world
    pub valid_time: TimeRange,

    /// When the fact was/is known to the database
    pub transaction_time: TimeRange,
}
```

1. **Valid Time (VT)**: When a fact was true in the real world
   - User-specified (can be past, present, or future)
   - Represents the temporal validity of the fact
   - May be retroactively corrected

2. **Transaction Time (TT)**: When a fact was recorded in the database
   - System-managed, always monotonically increasing
   - Immutable once set
   - Represents the provenance/audit trail

### Time Range Semantics

```rust
pub struct TimeRange {
    pub start: Timestamp,  // Inclusive
    pub end: Timestamp,    // Exclusive
}

/// Timestamp is microseconds since Unix epoch (i64)
/// Supports dates from ~290,000 BCE to ~290,000 CE
pub type Timestamp = i64;
```

- Ranges are half-open: `[start, end)`
- `end = Timestamp::MAX` represents "ongoing" or "until now"
- All timestamps are in microseconds for high precision

### Temporal Query Patterns

| Query Type | Description | Example |
|------------|-------------|---------|
| **Current** | Latest valid state as of now | Default query mode |
| **As-Of VT** | State valid at specific time | "What was true on Jan 1, 2024?" |
| **As-Of TT** | State known at specific time | "What did we know on Jan 1, 2024?" |
| **Bi-Temporal** | Both dimensions | "What did we know on TT about VT?" |
| **Sequenced** | Changes over valid time range | "How did X change during 2024?" |
| **Non-Sequenced** | Changes over transaction time | "When did we learn about X?" |

## Consequences

### Positive

- **Complete temporal reasoning**: Can answer any temporal question about data
- **Retroactive corrections**: Can record that we now know something was different in the past
- **Audit trail**: Transaction time provides complete provenance
- **LLM-friendly**: Enables natural queries about knowledge evolution
- **No data loss**: Corrections append new versions, never delete history

### Negative

- **Increased complexity**: Two time dimensions to manage
- **Storage overhead**: More temporal metadata per version
- **Query complexity**: Users must understand both dimensions
- **Index requirements**: May need separate indexes for each dimension

### Neutral

- Aligns with SQL:2011 temporal standard
- Common pattern in financial and healthcare systems
- Requires clear API design to avoid confusion

## Alternatives Considered

### Alternative 1: Valid Time Only

Track only when facts were true, not when they were recorded.

**Rejected because:**
- Cannot answer "when did we learn this?" questions
- No audit trail for compliance/debugging
- LLMs need provenance for reasoning about knowledge reliability

### Alternative 2: Transaction Time Only

Track only when facts were recorded, not when they were true.

**Rejected because:**
- Cannot represent retroactive corrections
- Cannot model facts with future validity
- Limited temporal reasoning capability

### Alternative 3: System Time + Application Time (SQL:2011 naming)

Use SQL:2011 terminology instead of our naming.

**Partially adopted:**
- We use "transaction time" (= system time) and "valid time" (= application time)
- Our semantics align with SQL:2011
- Named for clarity in our domain (databases + LLM reasoning)

### Alternative 4: Versioning Without Time Ranges

Use version numbers instead of time ranges.

**Rejected because:**
- Cannot represent temporal relationships
- Cannot query "state at time T"
- Loses semantic meaning of temporal validity

## Implementation Notes

### Core Types Location

```rust
// src/core/temporal.rs
pub struct BiTemporalInterval { ... }
pub struct TimeRange { ... }
pub type Timestamp = i64;

// Helper functions
pub fn now() -> Timestamp { ... }
pub fn time_range(start: Timestamp, end: Timestamp) -> TimeRange { ... }
```

### Invariants

1. `valid_time.start <= valid_time.end`
2. `transaction_time.start <= transaction_time.end`
3. Transaction time is always set by the system (never user-provided)
4. Transaction time start is always the commit timestamp
5. Transaction time end is `MAX` for current versions

### Version Lifecycle

```
Create: VT=[user_specified], TT=[commit_time, MAX)
Update: Old version: TT.end = commit_time
        New version: VT=[user_specified], TT=[commit_time, MAX)
Delete: VT.end = commit_time (logical delete)
```

## References

- [Temporal Database Concepts](https://en.wikipedia.org/wiki/Temporal_database)
- [SQL:2011 Temporal Features](https://sigmodrecord.org/publications/sigmodRecord/1209/pdfs/07.industry.kulkarni.pdf)
- [XTDB Bi-temporality](https://v1-docs.xtdb.com/concepts/bitemporality/)
- [Snodgrass - Developing Time-Oriented Database Applications](https://www2.cs.arizona.edu/~rts/tdbbook.pdf)
