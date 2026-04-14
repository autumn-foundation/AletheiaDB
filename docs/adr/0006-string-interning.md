# ADR-0006: String Interning for Labels

**Status:** Accepted
**Date:** 2024-12-31
**Deciders:** AletheiaDB Core Team
**Categories:** core, memory

## Context

Graph databases use labels extensively:
- **Node labels**: "Person", "Document", "Concept", "Fact"
- **Edge labels**: "KNOWS", "REFERENCES", "CONTAINS", "INFLUENCED_BY"
- **Property keys**: "name", "created_at", "embedding", "source"

In a knowledge graph with millions of nodes and edges:
- Labels are highly repetitive (few unique labels, many uses)
- Each `String` in Rust is 24 bytes (ptr + len + capacity) + heap allocation
- String comparison is O(n) where n is string length

Memory overhead and comparison cost become significant at scale.

## Decision

We will implement **string interning** using a global interner:

### Data Structures

```rust
/// 4-byte handle to an interned string
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct InternedString(u32);

/// Thread-safe bidirectional string interner
pub struct StringInterner {
    /// String → InternedString mapping
    to_id: DashMap<Arc<str>, InternedString>,

    /// InternedString → String mapping (for reverse lookup)
    to_string: DashMap<InternedString, Arc<str>>,

    /// Next available ID
    next_id: AtomicU32,
}

/// Global singleton interner
pub static GLOBAL_INTERNER: Lazy<StringInterner> = Lazy::new(StringInterner::new);
```

### API

```rust
impl StringInterner {
    /// Intern a string, returning its handle
    pub fn intern(&self, s: &str) -> InternedString {
        // Fast path: already interned
        if let Some(id) = self.to_id.get(s) {
            return *id;
        }

        // Slow path: use entry API for atomic insertion to avoid race conditions
        let arc: Arc<str> = s.into();
        *self.to_id.entry(arc.clone()).or_insert_with(|| {
            let new_id = InternedString(self.next_id.fetch_add(1, Ordering::Relaxed));
            self.to_string.insert(new_id, arc);
            new_id
        })
    }

    /// Resolve an interned string back to its value
    pub fn resolve(&self, id: InternedString) -> Option<Arc<str>> {
        self.to_string.get(&id).map(|r| r.clone())
    }
}

// Convenience functions
pub fn intern(s: &str) -> InternedString {
    GLOBAL_INTERNER.intern(s)
}

pub fn resolve(id: InternedString) -> Option<Arc<str>> {
    GLOBAL_INTERNER.resolve(id)
}
```

### Usage

```rust
pub struct Node {
    pub id: NodeId,
    pub label: InternedString,  // 4 bytes instead of 24+
    pub properties: PropertyMap,
    // ...
}

pub struct AdjacencyEntry {
    pub target: NodeId,
    pub edge_id: EdgeId,
    pub label: InternedString,  // Enables O(1) label comparison
}
```

## Consequences

### Positive

- **Memory savings**: 4 bytes vs 24+ bytes per label reference
- **O(1) equality**: Compare u32 instead of string contents
- **Deduplication**: Each unique string stored once
- **Cache-friendly**: Small handles fit in cache lines
- **Fast hashing**: u32 hashes trivially

### Negative

- **Global state**: Interner is a singleton (acceptable for our use case)
- **No garbage collection**: Interned strings never freed (labels are long-lived)
- **Indirection**: Must call `resolve()` to get string value
- **Serialization complexity**: Must serialize interner state for persistence

### Neutral

- Common pattern in compilers and databases
- Thread-safe via DashMap
- ID assignment is not deterministic across runs

## Alternatives Considered

### Alternative 1: Plain String

Use `String` or `Arc<str>` directly everywhere.

**Rejected because:**
- 6x memory overhead per reference
- O(n) string comparison
- Not viable at scale (millions of labels)

### Alternative 2: Static String Slices

Use `&'static str` for known labels.

**Rejected because:**
- Cannot handle dynamic/user-provided labels
- Requires compile-time knowledge of all labels
- Inflexible for knowledge graphs

### Alternative 3: Local Interning (Per-Structure)

Each structure maintains its own string table.

**Rejected because:**
- Cannot compare labels across structures efficiently
- Duplicated storage of same labels
- Complex coordination

### Alternative 4: Perfect Hashing

Use gperf or similar for compile-time perfect hash.

**Rejected because:**
- Requires static set of labels
- Cannot handle dynamic labels
- Complex build process

## Implementation Notes

### Thread Safety

```rust
// DashMap provides lock-free reads and fine-grained write locking
// AtomicU32 ensures unique IDs without locks
pub fn intern(&self, s: &str) -> InternedString {
    // Fast path: lock-free read
    if let Some(id) = self.to_id.get(s) {
        return *id;
    }

    // Slow path: acquire write lock on specific shard
    let arc: Arc<str> = s.into();
    let id = InternedString(self.next_id.fetch_add(1, Ordering::Relaxed));

    // Double-check to avoid race conditions
    self.to_id.entry(arc.clone()).or_insert(id);
    self.to_string.insert(id, arc);
    id
}
```

### Display and Debug

```rust
impl std::fmt::Display for InternedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match resolve(*self) {
            Some(s) => write!(f, "{}", s),
            None => write!(f, "<invalid interned string {}>", self.0),
        }
    }
}
```

### Persistence Considerations

For database persistence:
1. Serialize string table separately
2. On load, rebuild interner from table
3. IDs may differ between runs (remap on load)

### Memory Analysis

| Item | Size |
|------|------|
| InternedString | 4 bytes |
| String reference | 24 bytes |
| Arc<str> in interner | 16 bytes + string length |

For 100 unique labels used 1M times:
- Without interning: 24MB references + 100 × avg_len strings
- With interning: 4MB handles + 100 × (16 + avg_len) overhead

**Net savings: ~80% reduction in label memory**

## References

- [String Interning](https://en.wikipedia.org/wiki/String_interning)
- [lasso crate](https://crates.io/crates/lasso) - Production string interner
- [rustc Symbol Interning](https://doc.rust-lang.org/nightly/nightly-rustc/rustc_span/symbol/index.html)
- ADR-0005: CSR Adjacency Format (uses InternedString in AdjacencyEntry)
