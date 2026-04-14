# ADR-0008: Property Value Type System

**Status:** Accepted
**Date:** 2024-12-31
**Deciders:** AletheiaDB Core Team
**Categories:** core, api

## Context

Graph databases store properties on nodes and edges. The property type system must balance:

1. **Expressiveness**: Support common data types (strings, numbers, booleans, etc.)
2. **Performance**: Avoid boxing and indirection overhead
3. **Memory efficiency**: Minimize allocation and duplication
4. **Serialization**: Easy to persist and transfer
5. **Future extensibility**: Support new types (e.g., vectors for embeddings)

Key considerations for AletheiaDB:
- LLM integration benefits from structured metadata
- Embeddings will be stored as properties (vector search)
- Properties may be large (document content) or small (timestamps)
- Properties are shared across versions via Arc

## Decision

We will implement a **discriminated union (enum)** for property values with Arc-based sharing:

### PropertyValue Type

```rust
#[derive(Clone, Debug, PartialEq)]
pub enum PropertyValue {
    /// Null/missing value
    Null,

    /// Boolean value
    Bool(bool),

    /// 64-bit signed integer
    Int(i64),

    /// 64-bit floating point
    Float(f64),

    /// UTF-8 string (Arc for sharing)
    String(Arc<str>),

    /// Binary data (Arc for sharing)
    Bytes(Arc<[u8]>),

    /// Nested array of values (Arc for sharing)
    Array(Arc<Vec<PropertyValue>>),

    // Future: Vector for embeddings
    // Vector(Arc<[f32]>),
}
```

### PropertyMap Type

```rust
/// Immutable property collection with Arc sharing
#[derive(Clone, Debug, Default)]
pub struct PropertyMap {
    inner: Arc<HashMap<String, PropertyValue>>,
}

/// Builder for constructing PropertyMaps
pub struct PropertyMapBuilder {
    map: HashMap<String, PropertyValue>,
}
```

### API Design

```rust
impl PropertyMap {
    /// Get a property value
    pub fn get(&self, key: &str) -> Option<&PropertyValue> {
        self.inner.get(key)
    }

    /// Check if property exists
    pub fn contains_key(&self, key: &str) -> bool {
        self.inner.contains_key(key)
    }

    /// Iterate over properties
    pub fn iter(&self) -> impl Iterator<Item = (&String, &PropertyValue)> {
        self.inner.iter()
    }

    /// Number of properties
    pub fn len(&self) -> usize {
        self.inner.len()
    }
}

impl PropertyMapBuilder {
    pub fn new() -> Self { ... }

    pub fn set(mut self, key: impl Into<String>, value: impl Into<PropertyValue>) -> Self {
        self.map.insert(key.into(), value.into());
        self
    }

    pub fn build(self) -> PropertyMap {
        PropertyMap { inner: Arc::new(self.map) }
    }
}

// Ergonomic conversions
impl From<&str> for PropertyValue {
    fn from(s: &str) -> Self { PropertyValue::String(s.into()) }
}
impl From<i64> for PropertyValue {
    fn from(n: i64) -> Self { PropertyValue::Int(n) }
}
// ... etc for other types
```

### Usage Example

```rust
let props = PropertyMapBuilder::new()
    .set("name", "Alice")
    .set("age", 30i64)
    .set("active", true)
    .set("score", 98.5)
    .set("tags", PropertyValue::Array(Arc::new(vec![
        "developer".into(),
        "researcher".into(),
    ])))
    .build();

// Properties can be cloned cheaply (Arc increment)
let props2 = props.clone();  // Only increments refcount
```

## Consequences

### Positive

- **Type safety**: Enum ensures exhaustive matching
- **Memory efficient**: Arc sharing avoids duplication
- **Cheap cloning**: Clone only increments refcounts
- **Extensible**: Easy to add new variants (e.g., Vector)
- **Serialization friendly**: Can implement Serialize/Deserialize
- **Pattern matching**: Rust's match ensures all cases handled

### Negative

- **Dynamic typing at runtime**: Type errors are runtime, not compile-time
- **Enum size**: Size is largest variant + discriminant
- **No schema enforcement**: Application must validate structure
- **Nested Arrays**: Can create deeply nested structures

### Neutral

- Similar to JSON value types
- Common pattern in dynamic graph databases
- Schema can be added as a layer on top

## Alternatives Considered

### Alternative 1: Trait Object (dyn Any)

```rust
struct PropertyMap {
    values: HashMap<String, Box<dyn Any>>,
}
```

**Rejected because:**
- Type erasure loses type information
- Downcasting is verbose and error-prone
- Cannot implement PartialEq easily
- Boxing every value is expensive

### Alternative 2: JSON-only

Store all properties as JSON strings.

**Rejected because:**
- Parsing overhead on every access
- No native number types (JSON numbers are imprecise)
- Binary data requires encoding (base64)
- Poor performance for frequent access

### Alternative 3: Schema-first (Protobuf/Flatbuffers)

Define schema and generate code.

**Rejected because:**
- Rigid schema not suitable for knowledge graphs
- Schema evolution is complex
- LLM-generated content is inherently dynamic

### Alternative 4: Separate Maps per Type

```rust
struct PropertyMap {
    strings: HashMap<String, Arc<str>>,
    ints: HashMap<String, i64>,
    floats: HashMap<String, f64>,
    // ...
}
```

**Rejected because:**
- Complex API for mixed-type access
- Hard to iterate all properties
- Doesn't support heterogeneous arrays

## Implementation Notes

### Size Considerations

```rust
// PropertyValue size (on 64-bit):
// Discriminant: 8 bytes (aligned)
// Largest variant: Arc<[u8]> = 16 bytes (ptr + len)
// Total: 24 bytes

// Compare to String: 24 bytes (ptr + len + capacity)
// Compare to Box<dyn Any>: 16 bytes (ptr + vtable) + heap allocation
```

### Serialization Strategy

For persistence:
```rust
// Custom binary format for efficiency
impl PropertyValue {
    fn serialize(&self, buf: &mut Vec<u8>) {
        match self {
            PropertyValue::Null => buf.push(0),
            PropertyValue::Bool(b) => { buf.push(1); buf.push(*b as u8); }
            PropertyValue::Int(n) => { buf.push(2); buf.extend(&n.to_le_bytes()); }
            // ...
        }
    }
}
```

### Vector Extension (Future)

```rust
pub enum PropertyValue {
    // ... existing variants ...

    /// Dense float vector for embeddings
    Vector(Arc<[f32]>),

    /// Sparse vector for BM25/SPLADE
    SparseVector(Arc<SparseVec>),
}
```

## References

- [serde_json Value type](https://docs.rs/serde_json/latest/serde_json/enum.Value.html)
- [Neo4j Property Types](https://neo4j.com/docs/cypher-manual/current/values-and-types/)
- [Apache TinkerPop Property](https://tinkerpop.apache.org/docs/current/reference/#vertex-properties)
- ADR-0004: Anchor+Delta Compression (uses PropertyMap)
