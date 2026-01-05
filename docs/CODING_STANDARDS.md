# Rust Coding Standards

This document defines coding standards and best practices for GallifreyDB development.

## Type Safety

### Strong Typing for IDs

**Always use newtype wrappers for IDs:**

```rust
// GOOD: Distinct types prevent mix-ups
pub struct NodeId(u64);
pub struct EdgeId(u64);
pub struct VersionId(u64);

// BAD: Using raw u64 everywhere
fn get_node(id: u64) -> Node { /* which kind of ID? */ }
```

### ID Validation and Security

All ID types validate values on construction to prevent security issues:

```rust
// GOOD: Use validated constructors in public API
pub fn create_node(&self, id: u64) -> Result<NodeId> {
    NodeId::new(id)  // Validates ID is within MAX_VALID_ID
}

// INTERNAL USE ONLY: new_unchecked() bypasses validation
// - MUST remain pub(crate) - never expose in public API
// - Only use when ID is known valid (WAL recovery, trusted storage)
// - Document safety reasoning at call site
impl NodeId {
    pub(crate) const fn new_unchecked(id: u64) -> Self {
        NodeId(id)
    }
}
```

**Critical Security Rule**: The `new_unchecked()` methods MUST remain `pub(crate)`.

Never expose them in:
- Public API functions
- C FFI boundaries
- External plugin systems
- Any untrusted context

IDs exceeding `MAX_VALID_ID` (u64::MAX - 1000) are rejected to prevent:
- Arithmetic overflow in ID operations
- Excessive memory allocation attempts
- Serialization buffer overflow
- DoS attacks via extreme values

### Temporal Types

Use explicit temporal semantics:

```rust
// GOOD: Explicit temporal semantics
pub struct BiTemporalInterval {
    valid_time: TimeRange,
    transaction_time: TimeRange,
}

// BAD: Using raw tuples or generic ranges
type TemporalData = (TimeRange, TimeRange);
```

## Error Handling

### Use Result for Fallible Operations

```rust
pub fn get_node(&self, id: NodeId) -> Result<Node, Error> {
    self.nodes.get(&id).ok_or(Error::NodeNotFound(id))
}
```

### Define Specific Error Types

```rust
pub enum StorageError {
    NodeNotFound(NodeId),
    EdgeNotFound(EdgeId),
    VersionNotFound(VersionId),
    TemporalConstraintViolation {
        entity_id: String,
        reason: String,
    },
    Io(io::Error),
}

impl From<io::Error> for StorageError {
    fn from(err: io::Error) -> Self {
        StorageError::Io(err)
    }
}
```

### Never Use unwrap/expect in Production

**Rules:**
- Only use `.unwrap()` or `.expect()` in tests or when impossible to fail (document why)
- Prefer `?` operator for error propagation
- Handle errors at appropriate levels

```rust
// GOOD: Proper error handling
pub fn process_data(&self) -> Result<Data> {
    let value = self.get_value()?;
    let transformed = self.transform(value)?;
    Ok(transformed)
}

// BAD: unwrap in production code
pub fn process_data(&self) -> Data {
    let value = self.get_value().unwrap();  // Will panic!
    self.transform(value).unwrap()
}

// ACCEPTABLE: unwrap with justification in tests
#[test]
fn test_known_good_input() {
    // unwrap is OK here because we control the input
    let value = parse_value("42").unwrap();
    assert_eq!(value, 42);
}
```

## Performance Guidelines

### Minimize Allocations

```rust
// GOOD: Reuse buffers
let mut buffer = Vec::with_capacity(100);
for item in items {
    buffer.clear();
    process_into_buffer(item, &mut buffer);
}

// BAD: Allocate per iteration
for item in items {
    let buffer = vec![];  // New allocation each time
    process(item, buffer);
}
```

### Use Zero-Copy Where Possible

```rust
// GOOD: Return references
pub fn get_properties(&self) -> &PropertyMap {
    &self.properties
}

// BAD: Clone unnecessarily
pub fn get_properties(&self) -> PropertyMap {
    self.properties.clone()
}
```

### Prefer Iterator Chains

```rust
// GOOD: Lazy evaluation
edges.iter()
    .filter(|e| e.label == target_label)
    .map(|e| e.target)
    .collect()

// BAD: Intermediate collections
let filtered: Vec<_> = edges.iter()
    .filter(|e| e.label == target_label)
    .collect();
filtered.iter().map(|e| e.target).collect()
```

### Collection Sizing

Pre-allocate when size is known:

```rust
// GOOD: Pre-allocate with known capacity
let mut results = Vec::with_capacity(nodes.len());
for node in nodes {
    results.push(process(node));
}

// ACCEPTABLE: Unknown size
let results: Vec<_> = nodes.iter()
    .filter(|n| expensive_check(n))
    .map(|n| process(n))
    .collect();
```

## Concurrency

### Use Lock-Free Structures for Hot Paths

```rust
// Current indexes use DashMap (concurrent hashmap)
pub struct CurrentIndexes {
    nodes: DashMap<NodeId, Node>,
    edges: DashMap<EdgeId, Edge>,
}
```

### Immutable History Needs No Locks

```rust
// Historical versions are immutable after creation
// Safe to read concurrently without locks
pub struct HistoricalStorage {
    versions: Vec<Arc<NodeVersion>>,  // Immutable, shared
}
```

### Avoid RwLock and Mutex on Hot Paths

**Guidelines:**
- Use lock-free data structures (DashMap, atomic types)
- Prefer immutability over locking
- If locking is necessary, hold locks for minimal time
- Never hold multiple locks simultaneously (deadlock risk)

```rust
// GOOD: Lock-free concurrent access
let value = self.cache.get(&key);

// ACCEPTABLE: Short-lived lock
let value = {
    let guard = self.data.read();
    guard.get(&key).cloned()
};  // Lock released here

// BAD: Long-held lock with I/O
let guard = self.data.write();
let result = expensive_network_call();  // Lock held during I/O!
guard.insert(key, result);
```

## Memory Management

### Use Arc for Shared Ownership

```rust
// Properties shared across versions
pub struct PropertyMap {
    inner: Arc<HashMap<PropertyKey, PropertyValue>>,
}

impl Clone for PropertyMap {
    fn clone(&self) -> Self {
        // Cheap: only increments reference count
        PropertyMap { inner: Arc::clone(&self.inner) }
    }
}
```

### String Interning for Repeated Strings

```rust
// Labels and property keys are interned
pub struct StringInterner {
    strings: DashMap<Arc<str>, InternedString>,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct InternedString(u32);  // 4 bytes instead of 24
```

### Profile Before Optimizing

**Process:**
1. Use `cargo flamegraph` for CPU profiling
2. Use `heaptrack` or `valgrind` for memory profiling
3. Benchmark before/after optimizations
4. Document trade-offs in code comments

**Don't guess, measure!**

## Async/Await Considerations

### Use Async for I/O, Not CPU

```rust
// GOOD: Async for I/O operations
pub async fn flush_wal(&self) -> Result<()> {
    self.wal.sync().await
}

// BAD: Async for pure computation
// (Adds overhead without benefit)
pub async fn compute_graph_stats(&self) -> Stats {
    // CPU-bound work doesn't benefit from async
}
```

### When to Use Async

| Operation | Use Async? | Reason |
|-----------|------------|--------|
| File I/O | ✅ Yes | Blocks waiting for disk |
| Network | ✅ Yes | Blocks waiting for network |
| Computation | ❌ No | CPU-bound, no waiting |
| Multiple I/O | ✅ Yes | Can run concurrently |
| Hot path reads | ❌ No | Async overhead not worth it |

## Unsafe Rust Guidelines

### When Unsafe Is Acceptable

- Performance-critical hot paths with proven bottlenecks
- Zero-copy optimizations
- FFI boundaries
- Interacting with hardware or memory-mapped files

### Requirements for Unsafe Code

**ALWAYS document safety invariants:**

```rust
// GOOD: Clear safety documentation
unsafe {
    // SAFETY: We know the slice has at least `len` elements because
    // we just checked `slice.len() >= len` above. The pointer is valid
    // because it comes from a Vec allocation.
    std::slice::from_raw_parts(ptr, len)
}

// BAD: No explanation
unsafe {
    std::slice::from_raw_parts(ptr, len)  // Why is this safe?!
}
```

### Unsafe Checklist

Before writing unsafe code, verify:

- [ ] Is there a safe alternative? (Use that instead)
- [ ] Have you profiled and proven this is a bottleneck?
- [ ] Can you prove all safety invariants?
- [ ] Have you documented the safety reasoning?
- [ ] Are there tests that would catch violations?

## Testing Standards

### Unit Test Principles

- Test each module in isolation
- Use descriptive test names: `test_<what>_<scenario>_<expected>`
- Test both success and failure cases
- Test edge cases and boundary conditions

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_reconstruction_with_single_delta() {
        let anchor = create_anchor_version();
        let delta = create_delta_version();
        let reconstructed = reconstruct(&anchor, &delta);
        assert_eq!(reconstructed.properties, expected_properties);
    }

    #[test]
    fn test_temporal_invariants_reject_backwards_time() {
        let v1 = create_version(tx_time: 100);
        let v2 = create_version(tx_time: 99);
        assert!(v1.can_follow(&v2).is_err());
    }
}
```

### Property-Based Testing

Use `proptest` for invariant verification:

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn temporal_consistency(operations: Vec<Operation>) {
        let mut db = GallifreyDB::new();
        for op in operations {
            let _ = db.apply(op);
        }

        // Verify temporal invariants hold
        assert!(db.verify_transaction_time_monotonic());
        assert!(db.verify_no_temporal_paradoxes());
        assert!(db.verify_version_chain_integrity());
    }
}
```

## Code Organization

### Module Structure

```
src/
├── core/           # Core types (Node, Edge, Properties)
├── storage/        # Storage layer (Current, Historical, WAL)
├── index/          # Indexing (Temporal, Vector)
├── query/          # Query processing
└── utils/          # Utilities (errors, time)
```

### Visibility Guidelines

- Default to `pub(crate)` for internal APIs
- Only expose what's necessary in public API
- Use `pub(super)` for parent-module-only access
- Document why items are public

```rust
// Public API - exposed to users
pub struct GallifreyDB { ... }

// Internal - other modules can use
pub(crate) struct CurrentIndexes { ... }

// Private - only this module
struct InternalCache { ... }
```

## Documentation Standards

### Public API Documentation

All public items must have documentation:

```rust
/// Creates a new node in the graph with the given label and properties.
///
/// # Arguments
///
/// * `label` - The type/category of this node
/// * `properties` - Key-value properties attached to the node
///
/// # Returns
///
/// Returns `NodeId` on success, or `Error` if creation fails
///
/// # Examples
///
/// ```
/// let node_id = db.create_node("Person", properties! {
///     "name" => "Alice",
///     "age" => 30,
/// })?;
/// ```
pub fn create_node(&mut self, label: &str, properties: PropertyMap) -> Result<NodeId> {
    // Implementation
}
```

### Implementation Comments

Use comments to explain **why**, not **what**:

```rust
// GOOD: Explains the reasoning
// We use anchor+delta instead of full snapshots to reduce storage
// by 5-6X while keeping reconstruction fast (<10ms)
let version = create_delta_version();

// BAD: Just repeats the code
// Create a delta version
let version = create_delta_version();
```

## Summary Checklist

Before submitting a PR, verify:

- [ ] Strong typing used (no raw primitives for IDs)
- [ ] No `.unwrap()` or `.expect()` in production code
- [ ] Error handling is comprehensive
- [ ] Performance considerations addressed
- [ ] Concurrency safety verified
- [ ] Unsafe code is justified and documented
- [ ] Tests cover edge cases
- [ ] Public APIs are documented
- [ ] Code follows project structure conventions
