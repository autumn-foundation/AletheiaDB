# Observability Phase 5: Future Enhancements

This document outlines potential enhancements for future phases of the observability infrastructure.

## Error Severity Levels

**Current State**: All errors are categorized by type (Storage, Temporal, Query, Transaction, Vector, I/O, Other) but treated equally in terms of severity.

**Problem**: Some errors are expected during normal operation (e.g., `NodeNotFound` during optional lookups), while others indicate serious issues (e.g., `CorruptedData`).

**Proposed Enhancement**:

```rust
/// Error severity classification for observability
pub enum ErrorSeverity {
    /// Expected errors during normal operation
    /// Examples: NodeNotFound during optional lookup, WriteConflict in high-contention workload
    Expected,

    /// Concerning but recoverable errors
    /// Examples: High rate of ValidationFailed, repeated QueryTimeout
    Warning,

    /// Critical errors indicating data corruption or invariant violations
    /// Examples: CorruptedData, LockPoisoned, TemporalParadox
    Critical,
}

/// Extended error categorization with severity
pub struct ErrorClassification {
    pub category: ErrorCategory,  // Storage, Temporal, etc.
    pub severity: ErrorSeverity,
    pub is_recoverable: bool,
}
```

### Implementation Considerations

**Metrics Enhancement**:
```rust
pub struct Metrics {
    // Existing category counters
    pub error_storage_total: AtomicU64,
    // ... other categories ...

    // Phase 5: Severity counters
    pub error_expected_total: AtomicU64,    // Normal operation
    pub error_warning_total: AtomicU64,     // Concerning patterns
    pub error_critical_total: AtomicU64,    // Immediate action required
}
```

**Honeycomb Query Examples**:
```
# Alert on critical errors only
WHERE error_severity = "Critical" | COUNT

# Expected errors are high - investigate if rate increases
WHERE error_severity = "Expected"
  | COUNT
  | COMPARE(1 hour ago)
  | WHERE increase > 50%

# Critical errors by type
WHERE error_severity = "Critical"
  | GROUP BY error_category
  | VISUALIZE
```

### Severity Classification Table

| Error Type | Severity | Rationale |
|------------|----------|-----------|
| `NodeNotFound` | Expected | Common during graph traversals with optional lookups |
| `EdgeNotFound` | Expected | Common during relationship checks |
| `WriteConflict` | Expected | Normal in concurrent workloads (Snapshot Isolation) |
| `QueryTimeout` | Warning | Indicates slow queries or overload |
| `ValidationFailed` | Warning | May indicate application logic issues |
| `TemporalParadox` | Critical | Violates bi-temporal correctness invariants |
| `CorruptedData` | Critical | Data integrity violation |
| `LockPoisoned` | Critical | Thread panicked in critical section |
| `TimestampViolation` | Critical | MVCC invariant broken |
| `WALChecksumFailure` | Critical | Durability guarantee violated |

### Alerting Strategy

**Expected Errors** (no alert unless rate spikes):
- Monitor baseline rate
- Alert if 2x baseline over 5 minutes
- Example: `NodeNotFound` rate doubles → possible cache invalidation issue

**Warning Errors** (alert on sustained rate):
- Alert if >10/min for 5 minutes
- Example: Sustained `QueryTimeout` → need query optimization or scaling

**Critical Errors** (immediate alert):
- Alert on FIRST occurrence
- Page on-call
- Example: ANY `CorruptedData` error → immediate investigation required

## Distributed Tracing Enhancements

**Current State**: Spans for operations, but no distributed context propagation.

**Proposed Enhancement**: Add OpenTelemetry integration for distributed tracing across services.

```rust
// Phase 5: Distributed tracing
use opentelemetry::{trace::Tracer, global};

pub fn execute_distributed_query(
    trace_context: Option<TraceContext>,
) -> Result<QueryResult> {
    let tracer = global::tracer("aletheiadb");

    let span = if let Some(ctx) = trace_context {
        // Continue existing trace
        tracer.start_with_context("query", &ctx)
    } else {
        // Start new trace
        tracer.start("query")
    };

    // ... query execution with span context
}
```

## Custom Metrics

**Current State**: Fixed set of built-in metrics.

**Proposed Enhancement**: Allow library users to register custom metrics.

```rust
// Phase 5: Custom metrics
pub trait CustomMetric: Send + Sync {
    fn name(&self) -> &str;
    fn record(&self, value: f64);
}

pub fn register_custom_metric(metric: Box<dyn CustomMetric>) {
    METRICS.register_custom(metric);
}

// Example: Track application-specific patterns
db.register_custom_metric(Box::new(CustomCounter::new(
    "user_graph_depth",
    "Track average graph depth for user queries"
)));
```

## Sampling Strategies

**Current State**: All instrumented operations logged.

**Proposed Enhancement**: Adaptive sampling based on load.

```rust
// Phase 5: Adaptive sampling
pub struct SamplingStrategy {
    pub base_rate: f64,        // e.g., 0.1 = 10%
    pub critical_always: bool, // Always sample critical errors
    pub adaptive: bool,        // Increase sampling when load is low
}

impl SamplingStrategy {
    pub fn should_sample(&self, severity: ErrorSeverity, current_load: f64) -> bool {
        match severity {
            ErrorSeverity::Critical => self.critical_always,
            ErrorSeverity::Warning => self.base_rate * self.adaptive_multiplier(current_load),
            ErrorSeverity::Expected => self.base_rate,
        }
    }
}
```

## Metric Aggregation

**Current State**: Atomic counters only.

**Proposed Enhancement**: Histograms for latency percentiles.

```rust
// Phase 5: Histogram support
pub struct LatencyHistogram {
    buckets: Vec<AtomicU64>,  // Predefined buckets (e.g., 1ms, 10ms, 100ms, 1s)
}

impl LatencyHistogram {
    pub fn record(&self, duration_us: u64) {
        let bucket_idx = self.bucket_for_duration(duration_us);
        self.buckets[bucket_idx].fetch_add(1, Ordering::Relaxed);
    }

    pub fn percentile(&self, p: f64) -> u64 {
        // Calculate percentile from bucket distribution
    }
}
```

## Implementation Priority

1. **Error Severity** (High Priority)
   - Immediate value for production alerting
   - Low implementation cost (just add severity classification)
   - Reduces alert fatigue

2. **Sampling Strategies** (Medium Priority)
   - Important for high-throughput deployments
   - Moderate implementation cost

3. **Custom Metrics** (Low Priority)
   - Nice-to-have for advanced users
   - Requires careful API design

4. **Distributed Tracing** (Low Priority)
   - Only valuable in multi-service architectures
   - High implementation cost

## Backward Compatibility

All Phase 5 enhancements should be:
- **Opt-in**: Feature-gated to maintain zero-cost default
- **Additive**: No breaking changes to existing metrics
- **Documented**: Clear migration guides

## Success Criteria

- [ ] Error severity reduces alert fatigue by 80%
- [ ] Sampling reduces observability overhead to <2% in high-throughput workloads
- [ ] Custom metrics enable advanced use cases without forking
- [ ] Distributed tracing integrates seamlessly with existing tools (Jaeger, Zipkin)
