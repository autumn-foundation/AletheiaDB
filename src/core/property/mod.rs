//! Property system with Arc-based deduplication.
//!
//! This module provides a copy-on-write property system where properties are
//! stored in immutable, reference-counted containers. This enables:
//! - Cheap cloning of property maps (just increment reference count)
//! - Deduplication of unchanged properties across versions
//! - Zero-copy sharing of immutable data

use std::collections::HashMap;
use std::fmt;
use std::hash::BuildHasherDefault;
use std::sync::Arc;

use crate::core::error::{Result, StorageError};
use crate::core::hasher::IdentityHasher;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::vector::SparseVec;

// ============================================================================
// Serialization Type Tags
// ============================================================================
// Binary format uses little-endian byte order (consistent with WAL format).
// Each PropertyValue variant has a unique 1-byte type tag.

/// Type tag for Null value.
pub const TAG_NULL: u8 = 0;
/// Type tag for Bool value.
pub const TAG_BOOL: u8 = 1;
/// Type tag for Int (i64) value.
pub const TAG_INT: u8 = 2;
/// Type tag for Float (f64) value.
pub const TAG_FLOAT: u8 = 3;
/// Type tag for String value.
pub const TAG_STRING: u8 = 4;
/// Type tag for Bytes value.
pub const TAG_BYTES: u8 = 5;
/// Type tag for Array value.
pub const TAG_ARRAY: u8 = 6;
// Re-export vector constants from core::vector
pub use crate::core::vector::constants::{MAX_VECTOR_DIMENSIONS, TAG_SPARSE_VECTOR, TAG_VECTOR};
use crate::core::vector::serialization::validate_vector_dimensions;
pub use crate::core::vector::serialization::{
    deserialize_sparse_vector, deserialize_vector, serialize_sparse_vector,
    serialize_sparse_vector_into, serialize_vector, serialize_vector_into,
    try_serialize_vector_into,
};

// ============================================================================
// Serialization Limits
// ============================================================================
// These limits prevent DoS attacks via memory exhaustion from malicious input.

/// Maximum number of elements allowed in a deserialized array.
/// Increased from 1M to 10M to support business scenarios:
/// - Time series data: 115 days at 1kHz, multiple years at hourly resolution
/// - IoT telemetry: High-frequency sensor data
/// - Batch processing: Large bulk imports
///
///   Still provides DoS protection (max 40MB for f32 array).
pub const MAX_ARRAY_ELEMENTS: usize = 10_000_000;

/// Maximum capacity allowed for a deserialized property map.
/// Increased from 10K to 100K to support business scenarios:
/// - E-commerce: Products with extensive attributes and variations
/// - Scientific data: Rich metadata and measurements
/// - Dynamic schemas: User profiles with custom fields
///
///   Still provides DoS protection (~1MB per node maximum).
pub const MAX_PROPERTY_MAP_CAPACITY: usize = 100_000;

/// Maximum recursion depth for nested properties (e.g., arrays of arrays).
/// Set to 100 to prevent stack overflow from malicious input.
pub const MAX_RECURSION_DEPTH: usize = 100;

/// Property key type.
///
/// Uses interned strings for memory efficiency and O(1) equality comparisons.
/// Common keys like "name", "age", and "id" are deduplicated in memory.
pub type PropertyKey = InternedString;

/// A value that can be stored as a property.
///
/// All complex types (strings, bytes, arrays) use Arc for cheap cloning and sharing.
#[derive(Clone)]
pub enum PropertyValue {
    /// Null/absent value.
    Null,
    /// Boolean value.
    Bool(bool),
    /// 64-bit signed integer.
    Int(i64),
    /// 64-bit floating point.
    Float(f64),
    /// UTF-8 string (reference counted).
    String(Arc<str>),
    /// Byte array (reference counted).
    Bytes(Arc<[u8]>),
    /// Array of values (reference counted).
    Array(Arc<Vec<PropertyValue>>),
    /// Dense vector for embeddings (reference counted).
    /// Uses f32 for memory efficiency - standard for ML embeddings.
    ///
    /// # Floating-Point Equality Note
    /// This variant uses derived PartialEq which compares f32 values bitwise.
    /// Be aware that NaN != NaN (IEEE 754) and floating-point precision may
    /// cause semantically equal vectors to compare unequal. For similarity
    /// comparisons, use dedicated vector utility functions (e.g., cosine similarity)
    /// rather than equality. This limitation will be revisited in Phase 3 when
    /// vectors are used in temporal storage for deduplication.
    Vector(Arc<[f32]>),
    /// Sparse vector for high-dimensional sparse embeddings (reference counted).
    /// Stores only non-zero values along with their indices, making it memory-efficient
    /// for vectors where most values are zero (e.g., BM25, SPLADE).
    ///
    /// # Use Cases
    /// - BM25 text retrieval vectors
    /// - SPLADE sparse learned embeddings
    /// - TF-IDF document vectors
    /// - One-hot categorical encodings
    ///
    /// # Memory Efficiency
    /// For a 10,000-dimensional vector with 10 non-zero values:
    /// - Dense: 40KB (10,000 * 4 bytes)
    /// - Sparse: ~80 bytes (10 * 8 bytes for index+value pairs)
    /// - Space savings: ~500x
    ///
    /// # Floating-Point Equality Note
    /// This variant uses derived PartialEq which compares f32 values bitwise.
    /// Be aware that NaN != NaN (IEEE 754) and floating-point precision may
    /// cause semantically equal vectors to compare unequal. For robust equality
    /// checks, use [`SparseVec::approx_eq`](crate::core::vector::SparseVec::approx_eq)
    /// with an appropriate epsilon value instead of direct `==` comparison.
    SparseVector(Arc<SparseVec>),
}

impl PartialEq for PropertyValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PropertyValue::Null, PropertyValue::Null) => true,
            (PropertyValue::Bool(a), PropertyValue::Bool(b)) => a == b,
            (PropertyValue::Int(a), PropertyValue::Int(b)) => a == b,
            (PropertyValue::Float(a), PropertyValue::Float(b)) => a == b,
            (PropertyValue::String(a), PropertyValue::String(b)) => Arc::ptr_eq(a, b) || a == b,
            (PropertyValue::Bytes(a), PropertyValue::Bytes(b)) => Arc::ptr_eq(a, b) || a == b,
            (PropertyValue::Array(a), PropertyValue::Array(b)) => Arc::ptr_eq(a, b) || a == b,
            (PropertyValue::Vector(a), PropertyValue::Vector(b)) => Arc::ptr_eq(a, b) || a == b,
            (PropertyValue::SparseVector(a), PropertyValue::SparseVector(b)) => {
                Arc::ptr_eq(a, b) || a == b
            }
            _ => false,
        }
    }
}

impl PropertyValue {
    /// Create a string property value from a &str.
    #[inline]
    pub fn string<S: AsRef<str>>(s: S) -> Self {
        PropertyValue::String(Arc::from(s.as_ref()))
    }

    /// Create a bytes property value from a slice.
    #[inline]
    pub fn bytes<B: AsRef<[u8]>>(b: B) -> Self {
        PropertyValue::Bytes(Arc::from(b.as_ref()))
    }

    /// Create an array property value from a Vec.
    #[inline]
    pub fn array(values: Vec<PropertyValue>) -> Self {
        PropertyValue::Array(Arc::new(values))
    }

    /// Create a vector property value from a slice.
    ///
    /// Dense vectors are used for storing embeddings in nodes and edges,
    /// enabling semantic search and similarity computations. The data is
    /// stored in an `Arc<[f32]>` for efficient cloning and sharing across
    /// versions.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::PropertyValue;
    ///
    /// // From a Vec
    /// let embedding = vec![0.1f32, 0.2, 0.3, 0.4];
    /// let prop = PropertyValue::vector(&embedding);
    ///
    /// // From a slice
    /// let prop2 = PropertyValue::vector(&[0.5f32, 0.6, 0.7]);
    ///
    /// // Retrieve the vector
    /// assert_eq!(prop.as_vector(), Some(&embedding[..]));
    /// ```
    ///
    /// # Performance
    ///
    /// - Cloning a `PropertyValue::Vector` is O(1) (just increments Arc refcount)
    /// - Unchanged vectors across versions share the same allocation
    /// - Typical embedding sizes (384-4096 dims) use ~1.5-16KB per vector
    ///
    /// # See Also
    ///
    /// - [`PropertyMapBuilder::insert_vector`] for a builder-pattern alternative
    /// - [`as_vector`](Self::as_vector) for retrieving the vector data
    /// - [`aletheiadb::core::vector`](crate::core::vector) for similarity functions
    ///
    /// # Panics
    ///
    /// Panics if the vector dimension exceeds [`MAX_VECTOR_DIMENSIONS`].
    /// This validation ensures that vectors can be serialized without error.
    ///
    /// For a fallible version that returns `Result` instead of panicking,
    /// use [`try_vector`](Self::try_vector).
    #[inline]
    pub fn vector<V: AsRef<[f32]>>(v: V) -> Self {
        Self::try_vector(v).unwrap_or_else(|e| panic!("{}", e))
    }

    /// Create a vector property value from a slice (fallible).
    ///
    /// This is the fallible version of [`vector`](Self::vector). It returns
    /// an error if the vector dimension exceeds [`MAX_VECTOR_DIMENSIONS`]
    /// instead of panicking.
    #[inline]
    pub fn try_vector<V: AsRef<[f32]>>(v: V) -> Result<Self> {
        let slice = v.as_ref();
        validate_vector_dimensions(slice.len())?;
        Ok(PropertyValue::Vector(Arc::from(slice)))
    }

    /// Returns true if this value is null.
    #[inline]
    pub const fn is_null(&self) -> bool {
        matches!(self, PropertyValue::Null)
    }

    /// Try to get this value as a bool.
    #[inline]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            PropertyValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Try to get this value as an integer.
    #[inline]
    pub const fn as_int(&self) -> Option<i64> {
        match self {
            PropertyValue::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Try to get this value as a float.
    #[inline]
    pub const fn as_float(&self) -> Option<f64> {
        match self {
            PropertyValue::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Try to get this value as a string reference.
    #[inline]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            PropertyValue::String(s) => Some(s.as_ref()),
            _ => None,
        }
    }

    /// Try to get this value as a byte slice.
    #[inline]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            PropertyValue::Bytes(b) => Some(b.as_ref()),
            _ => None,
        }
    }

    /// Try to get this value as an array.
    #[inline]
    pub fn as_array(&self) -> Option<&[PropertyValue]> {
        match self {
            PropertyValue::Array(a) => Some(a.as_ref()),
            _ => None,
        }
    }

    /// Try to get this value as a vector (dense embedding).
    ///
    /// Returns `Some(&[f32])` if this is a `Vector` variant, `None` otherwise.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::PropertyValue;
    ///
    /// let embedding = vec![0.1f32, 0.2, 0.3];
    /// let prop = PropertyValue::vector(&embedding);
    ///
    /// if let Some(vec) = prop.as_vector() {
    ///     assert_eq!(vec.len(), 3);
    ///     assert!((vec[0] - 0.1).abs() < f32::EPSILON);
    /// }
    ///
    /// // Returns None for non-vector types
    /// let int_prop = PropertyValue::Int(42);
    /// assert!(int_prop.as_vector().is_none());
    /// ```
    ///
    /// # See Also
    ///
    /// - [`vector`](Self::vector) for creating vector properties
    /// - [`aletheiadb::core::vector`](crate::core::vector) for similarity functions
    #[inline]
    pub fn as_vector(&self) -> Option<&[f32]> {
        match self {
            PropertyValue::Vector(v) => Some(v.as_ref()),
            _ => None,
        }
    }

    /// Get the underlying Arc for a vector (dense embedding) without copying.
    ///
    /// Returns `Some(Arc<[f32]>)` if this is a `Vector` variant, `None` otherwise.
    /// This is more efficient than `as_vector().map(|s| s.to_vec())` because it
    /// clones the Arc (O(1) reference count increment) rather than copying the
    /// entire vector data.
    ///
    /// # Use Case
    ///
    /// Use this method when you need an owned reference to the vector data that
    /// can outlive the PropertyValue, without incurring the cost of copying.
    /// This is particularly useful for:
    /// - Passing vectors to functions that need ownership
    /// - Storing vector references across async boundaries
    /// - Avoiding allocations in performance-critical paths (Issue #188)
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::PropertyValue;
    /// use std::sync::Arc;
    ///
    /// let embedding = vec![0.1f32, 0.2, 0.3];
    /// let prop = PropertyValue::vector(&embedding);
    ///
    /// // Get an Arc to the data without copying
    /// if let Some(arc) = prop.as_arc_vector() {
    ///     assert_eq!(arc.len(), 3);
    ///     // The Arc can outlive `prop` and be passed around cheaply
    /// }
    /// ```
    ///
    /// # Performance
    ///
    /// - O(1) operation (just increments Arc reference count)
    /// - No memory allocation or data copying
    /// - Safe to call multiple times (returns same underlying data)
    ///
    /// # See Also
    ///
    /// - [`as_vector`](Self::as_vector) for borrowing the vector as a slice
    /// - [`vector`](Self::vector) for creating vector properties
    #[inline]
    pub fn as_arc_vector(&self) -> Option<Arc<[f32]>> {
        match self {
            PropertyValue::Vector(v) => Some(Arc::clone(v)),
            _ => None,
        }
    }

    /// Create a sparse vector property value from a SparseVec.
    ///
    /// Sparse vectors store only non-zero values along with their indices,
    /// making them memory-efficient for high-dimensional vectors where most
    /// values are zero (e.g., BM25, SPLADE).
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::PropertyValue;
    /// use aletheiadb::core::vector::SparseVec;
    ///
    /// // Create a sparse vector: [0.0, 1.5, 0.0, 0.0, 2.3, 0.0]
    /// let sparse = SparseVec::new(vec![1, 4], vec![1.5, 2.3], 6).unwrap();
    /// let prop = PropertyValue::sparse_vector(sparse);
    ///
    /// // Retrieve the sparse vector
    /// assert!(prop.as_sparse_vector().is_some());
    /// ```
    ///
    /// # Performance
    ///
    /// - Cloning a `PropertyValue::SparseVector` is O(1) (just increments Arc refcount)
    /// - Memory usage: O(nnz) where nnz = number of non-zero elements
    /// - Can save 10-1000x memory compared to dense vectors for sparse data
    ///
    /// # See Also
    ///
    /// - [`as_sparse_vector`](Self::as_sparse_vector) for retrieving the sparse vector
    /// - [`SparseVec`] for creating sparse vectors from indices and values
    #[inline]
    pub fn sparse_vector(sparse: SparseVec) -> Self {
        PropertyValue::SparseVector(Arc::new(sparse))
    }

    /// Try to get this value as a sparse vector.
    ///
    /// Returns `Some(&SparseVec)` if this is a `SparseVector` variant, `None` otherwise.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::PropertyValue;
    /// use aletheiadb::core::vector::SparseVec;
    ///
    /// let sparse = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
    /// let prop = PropertyValue::sparse_vector(sparse);
    ///
    /// if let Some(sv) = prop.as_sparse_vector() {
    ///     assert_eq!(sv.nnz(), 2);
    ///     assert_eq!(sv.dimension(), 5);
    /// }
    ///
    /// // Returns None for non-sparse-vector types
    /// let int_prop = PropertyValue::Int(42);
    /// assert!(int_prop.as_sparse_vector().is_none());
    /// ```
    ///
    /// # See Also
    ///
    /// - [`sparse_vector`](Self::sparse_vector) for creating sparse vector properties
    /// - [`SparseVec`] for sparse vector operations
    #[inline]
    pub fn as_sparse_vector(&self) -> Option<&SparseVec> {
        match self {
            PropertyValue::SparseVector(sv) => Some(sv.as_ref()),
            _ => None,
        }
    }

    /// Get the type name of this value.
    pub const fn type_name(&self) -> &'static str {
        match self {
            PropertyValue::Null => "null",
            PropertyValue::Bool(_) => "bool",
            PropertyValue::Int(_) => "int",
            PropertyValue::Float(_) => "float",
            PropertyValue::String(_) => "string",
            PropertyValue::Bytes(_) => "bytes",
            PropertyValue::Array(_) => "array",
            PropertyValue::Vector(_) => "vector",
            PropertyValue::SparseVector(_) => "sparse_vector",
        }
    }

    /// Check if two property values are semantically equal.
    ///
    /// This differs from `PartialEq` in that it treats `NaN` values as equal.
    /// This is important for change detection systems (like `VersionDiff` and `PropertyDelta`)
    /// to avoid reporting spurious changes when a value remains `NaN`.
    ///
    /// # Handling of NaN
    /// - `Float(NaN)` is equal to `Float(NaN)`
    /// - `Vector` containing `NaN` at index `i` is equal to `Vector` containing `NaN` at index `i`
    /// - `SparseVector`: Guaranteed not to contain `NaN` (enforced at construction), so standard equality applies.
    pub fn semantically_equal(&self, other: &Self) -> bool {
        match (self, other) {
            (PropertyValue::Float(a), PropertyValue::Float(b)) => {
                if a.is_nan() {
                    b.is_nan()
                } else {
                    a == b
                }
            }
            (PropertyValue::Vector(a), PropertyValue::Vector(b)) => {
                // Optimization: If they point to the same allocation, they must be equal.
                if Arc::ptr_eq(a, b) {
                    return true;
                }
                if a.len() != b.len() {
                    return false;
                }
                a.iter()
                    .zip(b.iter())
                    .all(|(x, y)| if x.is_nan() { y.is_nan() } else { x == y })
            }
            (PropertyValue::Array(a), PropertyValue::Array(b)) => {
                // Optimization: If they point to the same allocation, they must be equal.
                if Arc::ptr_eq(a, b) {
                    return true;
                }
                if a.len() != b.len() {
                    return false;
                }
                a.iter().zip(b.iter()).all(|(x, y)| x.semantically_equal(y))
            }
            // For other types, fallback to PartialEq
            _ => self == other,
        }
    }

    // ========================================================================
    // Serialization Methods
    // ========================================================================

    /// Serialize this PropertyValue to bytes.
    ///
    /// # Binary Format
    /// - Tag (1 byte): Identifies the value type
    /// - Payload: Type-specific data in little-endian format
    ///
    /// | Type   | Format                                      |
    /// |--------|---------------------------------------------|
    /// | Null   | `[tag:1]`                                   |
    /// | Bool   | `[tag:1][value:1]`                          |
    /// | Int    | `[tag:1][i64:8]`                            |
    /// | Float  | `[tag:1][f64:8]`                            |
    /// | String | `[tag:1][len:4][utf8_bytes:len]`            |
    /// | Bytes  | `[tag:1][len:4][bytes:len]`                 |
    /// | Array  | `[tag:1][count:4][elements...]`             |
    /// | Vector | `[tag:1][dim:4][f32_values:dim*4]`          |
    ///
    /// # Errors
    /// Returns `StorageError::CorruptedData` if recursion depth exceeds limits.
    pub fn serialize(&self) -> Result<Vec<u8>> {
        let mut buffer = Vec::with_capacity(self.serialized_size().map_err(|_| {
            StorageError::CorruptedData(
                "Recursion depth limit exceeded in serialized_size".to_string(),
            )
        })?);
        self.serialize_into(&mut buffer)?;
        Ok(buffer)
    }

    /// Serialize this PropertyValue into an existing buffer.
    ///
    /// This is more efficient when serializing multiple values as it avoids
    /// allocating a new Vec for each value.
    ///
    /// # Errors
    /// Returns `StorageError::CorruptedData` if recursion depth exceeds limits.
    pub fn serialize_into(&self, buffer: &mut Vec<u8>) -> Result<()> {
        self.serialize_recursive(buffer, 0)
    }

    fn serialize_recursive(&self, buffer: &mut Vec<u8>, depth: usize) -> Result<()> {
        if depth > MAX_RECURSION_DEPTH {
            return Err(StorageError::CorruptedData(format!(
                "Property value recursion depth limit exceeded (max {})",
                MAX_RECURSION_DEPTH
            ))
            .into());
        }

        match self {
            PropertyValue::Null => {
                buffer.push(TAG_NULL);
                Ok(())
            }
            PropertyValue::Bool(b) => {
                buffer.push(TAG_BOOL);
                buffer.push(if *b { 1 } else { 0 });
                Ok(())
            }
            PropertyValue::Int(i) => {
                buffer.push(TAG_INT);
                buffer.extend_from_slice(&i.to_le_bytes());
                Ok(())
            }
            PropertyValue::Float(f) => {
                buffer.push(TAG_FLOAT);
                buffer.extend_from_slice(&f.to_le_bytes());
                Ok(())
            }
            PropertyValue::String(s) => {
                buffer.push(TAG_STRING);
                let bytes = s.as_bytes();
                buffer.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                buffer.extend_from_slice(bytes);
                Ok(())
            }
            PropertyValue::Bytes(b) => {
                buffer.push(TAG_BYTES);
                buffer.extend_from_slice(&(b.len() as u32).to_le_bytes());
                buffer.extend_from_slice(b);
                Ok(())
            }
            PropertyValue::Array(arr) => {
                buffer.push(TAG_ARRAY);
                buffer.extend_from_slice(&(arr.len() as u32).to_le_bytes());
                for item in arr.iter() {
                    item.serialize_recursive(buffer, depth + 1)?;
                }
                Ok(())
            }
            PropertyValue::Vector(v) => {
                try_serialize_vector_into(v, buffer)?;
                Ok(())
            }
            PropertyValue::SparseVector(sv) => {
                serialize_sparse_vector_into(sv, buffer);
                Ok(())
            }
        }
    }

    /// Deserialize a PropertyValue from bytes.
    ///
    /// Returns the deserialized value and the number of bytes consumed.
    ///
    /// # Recursion Depth
    ///
    /// This function implements recursion depth checking to prevent stack overflow
    /// attacks via deeply nested structures (e.g., Array of Array of ...).
    /// The maximum depth is defined by [`MAX_RECURSION_DEPTH`].
    pub fn deserialize(bytes: &[u8]) -> Result<(Self, usize)> {
        Self::deserialize_recursive(bytes, 0)
    }

    /// Internal recursive deserialization helper with depth tracking.
    fn deserialize_recursive(bytes: &[u8], depth: usize) -> Result<(Self, usize)> {
        // Prevent recursion-based stack overflow DoS
        // Depth 0 = top level, depth 100 = maximum nesting level
        if depth > MAX_RECURSION_DEPTH {
            return Err(StorageError::CorruptedData(format!(
                "Property value recursion depth limit exceeded (max {})",
                MAX_RECURSION_DEPTH
            ))
            .into());
        }

        if bytes.is_empty() {
            return Err(StorageError::CorruptedData(
                "Empty buffer when deserializing PropertyValue".to_string(),
            )
            .into());
        }

        let tag = bytes[0];

        match tag {
            TAG_NULL => Ok((PropertyValue::Null, 1)),
            TAG_BOOL => Self::deserialize_bool(bytes),
            TAG_INT => Self::deserialize_int(bytes),
            TAG_FLOAT => Self::deserialize_float(bytes),
            TAG_STRING => Self::deserialize_string(bytes),
            TAG_BYTES => Self::deserialize_bytes(bytes),
            TAG_ARRAY => Self::deserialize_array(bytes, depth),
            TAG_VECTOR => {
                let (vector, consumed) = deserialize_vector(bytes)?;
                Ok((PropertyValue::Vector(vector), consumed))
            }
            TAG_SPARSE_VECTOR => {
                let (sparse_vector, consumed) = deserialize_sparse_vector(bytes)?;
                Ok((PropertyValue::SparseVector(sparse_vector), consumed))
            }
            _ => Err(StorageError::CorruptedData(format!(
                "Unknown PropertyValue type tag: {}",
                tag
            ))
            .into()),
        }
    }

    fn deserialize_bool(bytes: &[u8]) -> Result<(Self, usize)> {
        if bytes.len() < 2 {
            return Err(
                StorageError::CorruptedData("Buffer too short for Bool value".to_string()).into(),
            );
        }
        let value = bytes[1] != 0;
        Ok((PropertyValue::Bool(value), 2))
    }

    fn deserialize_int(bytes: &[u8]) -> Result<(Self, usize)> {
        if bytes.len() < 9 {
            return Err(
                StorageError::CorruptedData("Buffer too short for Int value".to_string()).into(),
            );
        }
        // SAFETY: Length check above guarantees slice has 8 bytes
        let value = i64::from_le_bytes(bytes[1..9].try_into().unwrap());
        Ok((PropertyValue::Int(value), 9))
    }

    fn deserialize_float(bytes: &[u8]) -> Result<(Self, usize)> {
        if bytes.len() < 9 {
            return Err(StorageError::CorruptedData(
                "Buffer too short for Float value".to_string(),
            )
            .into());
        }
        // SAFETY: Length check above guarantees slice has 8 bytes
        let value = f64::from_le_bytes(bytes[1..9].try_into().unwrap());
        Ok((PropertyValue::Float(value), 9))
    }

    #[inline]
    fn deserialize_string(bytes: &[u8]) -> Result<(Self, usize)> {
        if bytes.len() < 5 {
            return Err(StorageError::CorruptedData(
                "Buffer too short for String length".to_string(),
            )
            .into());
        }
        let len = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
        let offset = 5usize;

        let required_len = offset
            .checked_add(len)
            .ok_or_else(|| StorageError::CorruptedData("String length overflow".to_string()))?;

        if bytes.len() < required_len {
            return Err(StorageError::CorruptedData(format!(
                "Buffer too short for String data: need {} bytes, have {}",
                required_len,
                bytes.len()
            ))
            .into());
        }

        let string_data = &bytes[offset..required_len];
        let s = std::str::from_utf8(string_data)
            .map_err(|e| StorageError::CorruptedData(format!("Invalid UTF-8 in String: {}", e)))?;
        Ok((PropertyValue::String(Arc::from(s)), required_len))
    }

    #[inline]
    fn deserialize_bytes(bytes: &[u8]) -> Result<(Self, usize)> {
        if bytes.len() < 5 {
            return Err(StorageError::CorruptedData(
                "Buffer too short for Bytes length".to_string(),
            )
            .into());
        }
        let len = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
        let offset = 5usize;

        let required_len = offset
            .checked_add(len)
            .ok_or_else(|| StorageError::CorruptedData("Bytes length overflow".to_string()))?;

        if bytes.len() < required_len {
            return Err(StorageError::CorruptedData(format!(
                "Buffer too short for Bytes data: need {} bytes, have {}",
                required_len,
                bytes.len()
            ))
            .into());
        }

        let byte_data = &bytes[offset..required_len];
        Ok((PropertyValue::Bytes(Arc::from(byte_data)), required_len))
    }

    fn deserialize_array(bytes: &[u8], depth: usize) -> Result<(Self, usize)> {
        if bytes.len() < 5 {
            return Err(StorageError::CorruptedData(
                "Buffer too short for Array count".to_string(),
            )
            .into());
        }
        let count = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
        let mut offset: usize = 5;

        // Prevent DoS via memory exhaustion from malicious input
        if count > MAX_ARRAY_ELEMENTS {
            return Err(StorageError::CorruptedData(format!(
                "Array count {} exceeds maximum allowed {}",
                count, MAX_ARRAY_ELEMENTS
            ))
            .into());
        }

        // Prevent DoS via pre-allocation amplification:
        // Ensure we have at least 1 byte per element in the buffer
        // before allocating the vector.
        if bytes.len().saturating_sub(offset) < count {
            return Err(StorageError::CorruptedData(format!(
                "Insufficient buffer size for Array elements: need {} bytes, have {}",
                count,
                bytes.len().saturating_sub(offset)
            ))
            .into());
        }

        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            if offset >= bytes.len() {
                return Err(StorageError::CorruptedData(
                    "Buffer exhausted while reading Array elements".to_string(),
                )
                .into());
            }
            // Recursive call with depth increment
            let (item, consumed) =
                PropertyValue::deserialize_recursive(&bytes[offset..], depth + 1)?;
            items.push(item);
            offset += consumed;
        }
        Ok((PropertyValue::Array(Arc::new(items)), offset))
    }

    /// Estimate the heap memory usage of this property value in bytes.
    ///
    /// This provides a rough estimate of heap allocations, useful for memory
    /// accounting in tiered storage migration decisions. The estimate includes:
    ///
    /// - String/Bytes: actual data size (shared via Arc)
    /// - Array: element sizes plus Vec overhead
    /// - Vector: f32 count * 4 bytes
    /// - SparseVector: indices + values + dimension overhead
    ///
    /// Note: This is an estimate. Due to Arc sharing, actual memory usage may
    /// be lower if values are shared across versions.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::PropertyValue;
    ///
    /// let small = PropertyValue::Int(42);
    /// assert_eq!(small.estimated_heap_size(), 0); // No heap allocation
    ///
    /// let string = PropertyValue::string("hello world");
    /// assert_eq!(string.estimated_heap_size(), 11); // String length
    /// ```
    pub fn estimated_heap_size(&self) -> usize {
        // Return a large "penalty" size (10MB) on error (recursion limit exceeded).
        // This ensures that malicious or excessively nested structures are
        // considered "large" by cache eviction policies, rather than "small" (0),
        // preventing them from monopolizing the cache.
        self.estimated_heap_size_recursive(0)
            .unwrap_or(10 * 1024 * 1024)
    }

    fn estimated_heap_size_recursive(&self, depth: usize) -> Result<usize> {
        if depth > MAX_RECURSION_DEPTH {
            return Err(StorageError::CorruptedData(format!(
                "Property value recursion depth limit exceeded (max {})",
                MAX_RECURSION_DEPTH
            ))
            .into());
        }

        match self {
            PropertyValue::Null
            | PropertyValue::Bool(_)
            | PropertyValue::Int(_)
            | PropertyValue::Float(_) => Ok(0),
            PropertyValue::String(s) => Ok(s.len()),
            PropertyValue::Bytes(b) => Ok(b.len()),
            PropertyValue::Array(arr) => {
                // Vec capacity overhead + recursive element sizes
                let mut size = arr.capacity() * std::mem::size_of::<PropertyValue>();
                for item in arr.iter() {
                    size += item.estimated_heap_size_recursive(depth + 1)?;
                }
                Ok(size)
            }
            PropertyValue::Vector(v) => Ok(v.len() * std::mem::size_of::<f32>()),
            PropertyValue::SparseVector(sv) => {
                // Indices + values + SparseVec struct overhead
                Ok(
                    sv.nnz() * (std::mem::size_of::<u32>() + std::mem::size_of::<f32>())
                        + std::mem::size_of::<usize>(),
                ) // dimension field
            }
        }
    }

    /// Calculate the number of bytes required for serialization.
    ///
    /// This is used to pre-allocate buffers for serialization, avoiding reallocation
    /// overhead during high-throughput operations (like WAL writing).
    ///
    /// # Size Breakdown
    /// - Null: 1 byte
    /// - Bool: 2 bytes
    /// - Int: 9 bytes
    /// - Float: 9 bytes
    /// - String: 1 + 4 + len
    /// - Bytes: 1 + 4 + len
    /// - Array: 1 + 4 + sum(elements)
    /// - Vector: 1 + 4 + (dims * 4)
    /// - SparseVector: 1 + 4 + 4 + (nnz * 8)
    pub fn serialized_size(&self) -> Result<usize> {
        self.serialized_size_recursive(0)
    }

    fn serialized_size_recursive(&self, depth: usize) -> Result<usize> {
        if depth > MAX_RECURSION_DEPTH {
            return Err(StorageError::CorruptedData(format!(
                "Property value recursion depth limit exceeded (max {})",
                MAX_RECURSION_DEPTH
            ))
            .into());
        }

        match self {
            PropertyValue::Null => Ok(1),
            PropertyValue::Bool(_) => Ok(2),
            PropertyValue::Int(_) => Ok(9),
            PropertyValue::Float(_) => Ok(9),
            PropertyValue::String(s) => Ok(1 + 4 + s.len()),
            PropertyValue::Bytes(b) => Ok(1 + 4 + b.len()),
            PropertyValue::Array(arr) => {
                let mut elements_size = 0;
                for v in arr.iter() {
                    elements_size += v.serialized_size_recursive(depth + 1)?;
                }
                Ok(1 + 4 + elements_size)
            }
            PropertyValue::Vector(v) => Ok(1 + 4 + (v.len() * 4)),
            PropertyValue::SparseVector(sv) => Ok(1 + 4 + 4 + (sv.nnz() * 8)),
        }
    }
}

impl Drop for PropertyValue {
    fn drop(&mut self) {
        // Prevent stack overflow when dropping deeply nested Arrays.
        // We linearize the drop chain by taking ownership of children
        // from unique Arcs and processing them iteratively.
        if let PropertyValue::Array(arc) = self {
            // Only process if we are the sole owner of the data (refcount == 1)
            if let Some(vec) = Arc::get_mut(arc) {
                // Initial work list: take all elements from the current vector
                // std::mem::take requires Vec to implement Default, which it does (empty vec)
                let mut stack = std::mem::take(vec);

                while let Some(mut item) = stack.pop() {
                    if let PropertyValue::Array(ref mut inner_arc) = item {
                        // Check if we own this inner array
                        if let Some(inner_vec) = Arc::get_mut(inner_arc) {
                            // Move its children to the stack to be processed later
                            // This effectively flattens the recursive structure
                            let children = std::mem::take(inner_vec);
                            stack.extend(children);
                        }
                    }
                    // item is dropped here.
                    // If it was a unique Array, its children were moved to stack, so it's empty.
                    // If it was shared Array, children remain (handled by other owners).
                    // If it was non-Array, standard drop.
                }
            }
        }
    }
}

impl PropertyValue {
    /// Safe recursive display formatter with depth limit.
    fn fmt_display(&self, f: &mut fmt::Formatter<'_>, depth: usize) -> fmt::Result {
        if depth > 50 {
            return write!(f, "...");
        }

        match self {
            PropertyValue::Null => write!(f, "null"),
            PropertyValue::Bool(b) => write!(f, "{}", b),
            PropertyValue::Int(i) => write!(f, "{}", i),
            PropertyValue::Float(fl) => write!(f, "{}", fl),
            PropertyValue::String(s) => write!(f, "\"{}\"", s),
            PropertyValue::Bytes(b) => write!(f, "<{} bytes>", b.len()),
            PropertyValue::Array(a) => {
                write!(f, "[")?;
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    if i >= 20 {
                        write!(f, "... ({} more)", a.len() - i)?;
                        break;
                    }
                    v.fmt_display(f, depth + 1)?;
                }
                write!(f, "]")
            }
            PropertyValue::Vector(v) => write!(f, "<vector[{}]>", v.len()),
            PropertyValue::SparseVector(sv) => {
                write!(
                    f,
                    "<sparse_vector[dim={}, nnz={}]>",
                    sv.dimension(),
                    sv.nnz()
                )
            }
        }
    }

    /// Safe recursive debug formatter with depth limit.
    fn fmt_debug(&self, f: &mut fmt::Formatter<'_>, depth: usize) -> fmt::Result {
        if depth > 50 {
            return write!(f, "...");
        }

        match self {
            PropertyValue::Null => write!(f, "Null"),
            PropertyValue::Bool(b) => write!(f, "Bool({:?})", b),
            PropertyValue::Int(i) => write!(f, "Int({:?})", i),
            PropertyValue::Float(fl) => write!(f, "Float({:?})", fl),
            PropertyValue::String(s) => write!(f, "String({:?})", s),
            PropertyValue::Bytes(b) => write!(f, "Bytes({:?})", b),
            PropertyValue::Array(a) => {
                write!(f, "Array([")?;
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    if i >= 20 {
                        write!(f, "... ({} more)", a.len() - i)?;
                        break;
                    }
                    v.fmt_debug(f, depth + 1)?;
                }
                write!(f, "])")
            }
            // Limit vector output in debug to prevent log spam
            PropertyValue::Vector(v) => {
                write!(f, "Vector([")?;
                for (i, val) in v.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    if i >= 10 {
                        write!(f, "... ({} more)", v.len() - i)?;
                        break;
                    }
                    write!(f, "{:?}", val)?;
                }
                write!(f, "])")
            }
            PropertyValue::SparseVector(sv) => write!(f, "SparseVector({:?})", sv),
        }
    }
}

impl fmt::Display for PropertyValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_display(f, 0)
    }
}

impl fmt::Debug for PropertyValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_debug(f, 0)
    }
}

// Convenient From implementations
impl From<bool> for PropertyValue {
    fn from(b: bool) -> Self {
        PropertyValue::Bool(b)
    }
}

impl From<i64> for PropertyValue {
    fn from(i: i64) -> Self {
        PropertyValue::Int(i)
    }
}

impl From<i32> for PropertyValue {
    fn from(i: i32) -> Self {
        PropertyValue::Int(i as i64)
    }
}

impl From<f64> for PropertyValue {
    fn from(f: f64) -> Self {
        PropertyValue::Float(f)
    }
}

impl From<String> for PropertyValue {
    fn from(s: String) -> Self {
        // Use Arc::from(s) directly to avoid unnecessary allocation.
        // This leverages Rust's built-in conversion chain:
        // String → Box<str> → Arc<str>
        // which reuses the String's allocation instead of copying.
        // See: https://github.com/madmax983/AletheiaDB/issues/200
        PropertyValue::String(Arc::from(s))
    }
}

impl From<&str> for PropertyValue {
    fn from(s: &str) -> Self {
        PropertyValue::String(Arc::from(s))
    }
}

impl From<Vec<u8>> for PropertyValue {
    fn from(b: Vec<u8>) -> Self {
        // Use Arc::from(b) directly to avoid unnecessary allocation.
        // This leverages Rust's built-in conversion chain:
        // Vec<u8> → Box<[u8]> → Arc<[u8]>
        // which reuses the Vec's allocation instead of copying.
        // See: https://github.com/madmax983/AletheiaDB/issues/200
        PropertyValue::Bytes(Arc::from(b))
    }
}

impl From<&[u8]> for PropertyValue {
    fn from(b: &[u8]) -> Self {
        PropertyValue::Bytes(Arc::from(b))
    }
}

impl From<Vec<PropertyValue>> for PropertyValue {
    fn from(v: Vec<PropertyValue>) -> Self {
        PropertyValue::Array(Arc::new(v))
    }
}

impl From<Vec<f32>> for PropertyValue {
    fn from(v: Vec<f32>) -> Self {
        // Use v.into() to reuse the Vec's buffer, avoiding allocation and copy
        PropertyValue::Vector(v.into())
    }
}

impl From<&[f32]> for PropertyValue {
    fn from(v: &[f32]) -> Self {
        PropertyValue::Vector(Arc::from(v))
    }
}

impl From<SparseVec> for PropertyValue {
    fn from(sv: SparseVec) -> Self {
        PropertyValue::SparseVector(Arc::new(sv))
    }
}

/// A map of property keys to values with copy-on-write semantics.
///
/// The underlying HashMap is wrapped in an Arc, making clones very cheap
/// (just incrementing a reference count). This enables efficient sharing
/// of unchanged properties across versions.
#[derive(Clone)]
pub struct PropertyMap {
    inner: Arc<HashMap<PropertyKey, PropertyValue, BuildHasherDefault<IdentityHasher>>>,
    /// Cached serialized size in bytes.
    ///
    /// # Invariants
    ///
    /// This field must strictly equal the result of `serialized_size()` if calculated
    /// from scratch. It is calculated at creation time and maintained incrementally
    /// by `PropertyMapBuilder` to allow O(1) access for WAL reservation.
    ///
    /// # Copy-on-Write Safety
    ///
    /// `PropertyMap` implements copy-on-write semantics. This struct is immutable once
    /// created. Any modification (via `PropertyMapBuilder`) creates a *new* instance
    /// with a new `cached_size`.
    ///
    /// While `inner` is wrapped in an `Arc` for cheap cloning, `cached_size` is
    /// copied by value. This is safe because the underlying `HashMap` is never
    /// mutated in place through shared references. The only way to "modify" a map
    /// is to create a new one, which calculates its own fresh `cached_size`.
    cached_size: usize,
}

impl PropertyMap {
    /// Create a new empty property map.
    pub fn new() -> Self {
        PropertyMap {
            inner: Arc::new(HashMap::with_hasher(BuildHasherDefault::default())),
            cached_size: 4, // 4 bytes for the count field (0)
        }
    }

    /// Create a property map with the specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        PropertyMap {
            inner: Arc::new(HashMap::with_capacity_and_hasher(
                capacity,
                BuildHasherDefault::default(),
            )),
            cached_size: 4, // 4 bytes for the count field (0)
        }
    }

    /// Get a property value by key.
    ///
    /// The key is looked up in the interner for efficient comparison.
    /// Returns None if the key hasn't been interned (and thus cannot be in the map).
    #[inline]
    pub fn get(&self, key: &str) -> Option<&PropertyValue> {
        let interned_key = GLOBAL_INTERNER.get_id(key)?;
        self.get_by_interned_key(&interned_key)
    }

    /// Get a property value by an already-interned key.
    ///
    /// This is more efficient than `get()` when you already have an InternedString.
    /// For internal use and performance-critical paths.
    #[inline]
    pub fn get_by_interned_key(&self, key: &PropertyKey) -> Option<&PropertyValue> {
        self.inner.get(key)
    }

    /// Check if a property exists.
    ///
    /// The key is looked up in the interner for efficient comparison.
    /// Returns false if the key hasn't been interned (and thus cannot be in the map).
    #[inline]
    pub fn contains_key(&self, key: &str) -> bool {
        let Some(interned_key) = GLOBAL_INTERNER.get_id(key) else {
            return false;
        };
        self.contains_interned_key(&interned_key)
    }

    /// Check if a property exists by an already-interned key.
    ///
    /// This is more efficient than `contains_key()` when you already have an InternedString.
    /// For internal use and performance-critical paths.
    #[inline]
    pub fn contains_interned_key(&self, key: &PropertyKey) -> bool {
        self.inner.contains_key(key)
    }

    /// Get the number of properties.
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Check if the property map is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Iterate over all key-value pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&PropertyKey, &PropertyValue)> {
        self.inner.iter()
    }

    /// Get all property keys.
    pub fn keys(&self) -> impl Iterator<Item = &PropertyKey> {
        self.inner.keys()
    }

    /// Get all property values.
    pub fn values(&self) -> impl Iterator<Item = &PropertyValue> {
        self.inner.values()
    }

    /// Create a builder for modifying this property map.
    ///
    /// This enables copy-on-write: if the Arc has multiple references,
    /// the HashMap will be cloned before modification.
    pub fn builder(self) -> PropertyMapBuilder {
        PropertyMapBuilder::from_map(self)
    }

    /// Check if this property map contains any vector properties (dense or sparse).
    ///
    /// This is used to optimize the transaction commit path by only triggering
    /// temporal vector index updates when vector data is actually present.
    ///
    /// Note: This only checks top-level properties. Nested vectors inside
    /// Array values are not currently detected (vectors-in-arrays are not
    /// a supported use case in the current implementation).
    #[inline]
    pub fn contains_vector(&self) -> bool {
        self.inner
            .values()
            .any(|v| matches!(v, PropertyValue::Vector(_) | PropertyValue::SparseVector(_)))
    }

    // ========================================================================
    // Serialization Methods
    // ========================================================================

    /// Serialize this PropertyMap to bytes.
    ///
    /// # Binary Format
    /// ```text
    /// [count:4][key1_len:4][key1_bytes:key1_len][value1_bytes:...]...
    /// ```
    ///
    /// - Count: u32 little-endian, number of key-value pairs
    /// - For each key-value pair:
    ///   - Key length: u32 little-endian
    ///   - Key bytes: UTF-8 encoded string
    ///   - Value: Serialized PropertyValue (includes type tag)
    ///
    /// Note: HashMap ordering is not guaranteed, so serialization order
    /// may vary. This is acceptable for correctness but may affect
    /// byte-for-byte reproducibility.
    ///
    /// # Errors
    ///
    /// Returns an error if any PropertyKey cannot be resolved from the interner.
    /// This should never happen in practice as all keys are created via interning.
    pub fn serialize(&self) -> Result<Vec<u8>> {
        let mut buffer = Vec::with_capacity(self.cached_size);
        self.serialize_into(&mut buffer)?;
        Ok(buffer)
    }

    /// Serialize this PropertyMap into an existing buffer.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::InconsistentState` if any PropertyKey cannot be
    /// resolved from the interner, indicating data corruption.
    pub fn serialize_into(&self, buffer: &mut Vec<u8>) -> Result<()> {
        // Reserve space for the entire map to avoid reallocations
        buffer.reserve(self.cached_size);

        buffer.extend_from_slice(&(self.inner.len() as u32).to_le_bytes());
        for (key, value) in self.inner.iter() {
            // Serialize key: resolve InternedString to actual string
            // Use with_str to avoid Arc cloning overhead
            GLOBAL_INTERNER
                .resolve_with(*key, |key_str| {
                    let key_bytes = key_str.as_bytes();
                    buffer.extend_from_slice(&(key_bytes.len() as u32).to_le_bytes());
                    buffer.extend_from_slice(key_bytes);
                })
                .ok_or_else(|| {
                    crate::core::error::Error::Storage(StorageError::InconsistentState {
                        reason: format!(
                            "PropertyKey {} not found in interner - data corruption detected",
                            key.as_u32()
                        ),
                    })
                })?;

            // Serialize value
            value.serialize_into(buffer)?;
        }
        Ok(())
    }

    /// Deserialize a PropertyMap from bytes.
    ///
    /// Returns the deserialized map and the number of bytes consumed.
    pub fn deserialize(bytes: &[u8]) -> Result<(Self, usize)> {
        if bytes.len() < 4 {
            return Err(StorageError::CorruptedData(
                "Buffer too short for PropertyMap count".to_string(),
            )
            .into());
        }

        let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;

        // Prevent DoS via memory exhaustion from malicious input
        if count > MAX_PROPERTY_MAP_CAPACITY {
            return Err(StorageError::CorruptedData(format!(
                "PropertyMap count {} exceeds maximum allowed {}",
                count, MAX_PROPERTY_MAP_CAPACITY
            ))
            .into());
        }

        let mut offset = 4;

        // Prevent DoS via pre-allocation amplification:
        // Ensure we have at least 5 bytes per entry (minimum size)
        // Key length (4) + Key data (0) + Value tag (1) = 5 bytes
        // Use checked arithmetic to prevent overflow in count * 5
        let min_required_bytes = count.saturating_mul(5);
        if bytes.len().saturating_sub(offset) < min_required_bytes {
            return Err(StorageError::CorruptedData(format!(
                "Insufficient buffer size for PropertyMap entries: need {} bytes, have {}",
                min_required_bytes,
                bytes.len().saturating_sub(offset)
            ))
            .into());
        }

        let mut map = HashMap::with_capacity_and_hasher(count, BuildHasherDefault::default());
        // Track the actual logical size of the map to validate against consumed bytes
        let mut calculated_size: usize = 4;

        for _ in 0..count {
            // Read key length
            if bytes.len() < offset + 4 {
                return Err(StorageError::CorruptedData(
                    "Buffer too short for property key length".to_string(),
                )
                .into());
            }
            // SAFETY: Length check above guarantees 4 bytes available
            let key_len =
                u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            // Read key
            if bytes.len() < offset + key_len {
                return Err(StorageError::CorruptedData(
                    "Buffer too short for property key data".to_string(),
                )
                .into());
            }
            let key_str = std::str::from_utf8(&bytes[offset..offset + key_len]).map_err(|e| {
                StorageError::CorruptedData(format!("Invalid UTF-8 in property key: {}", e))
            })?;
            // Intern the key for efficient storage and comparison
            let key = GLOBAL_INTERNER.intern(key_str)?;
            offset += key_len;

            // Read value
            let (value, consumed) = PropertyValue::deserialize(&bytes[offset..])?;

            // Validate size consistency
            let key_size = 4 + key_len;
            calculated_size = calculated_size
                .saturating_add(key_size)
                .saturating_add(consumed);

            offset += consumed;

            if map.insert(key, value).is_some() {
                // If we encounter a duplicate key, the map's logical size shrinks (replacement),
                // but the input stream 'offset' keeps growing. This mismatch indicates
                // a non-canonical or corrupted stream (standard serialization implies unique keys).
                //
                // We enforce strict validation: offset (consumed bytes) must match
                // the logical size of the constructed map.
                return Err(StorageError::CorruptedData(format!(
                    "Duplicate property key found during deserialization: '{}'. \
                     This indicates corrupted data or invalid serialization format.",
                    key_str
                ))
                .into());
            }
        }

        // Final validation: The bytes consumed must match the logical size of the map.
        // If they differ, it implies hidden data, duplicates (caught above), or
        // inconsistent size calculations.
        if offset != calculated_size {
            return Err(StorageError::CorruptedData(format!(
                "PropertyMap deserialization size mismatch: consumed {} bytes but logical size is {}. \
                 Data corruption suspected.",
                offset, calculated_size
            ))
            .into());
        }

        Ok((
            PropertyMap {
                inner: Arc::new(map),
                cached_size: calculated_size,
            },
            offset,
        ))
    }

    /// Estimate the heap memory usage of this property map in bytes.
    ///
    /// This provides a rough estimate of heap allocations, useful for memory
    /// accounting in tiered storage migration decisions. The estimate includes:
    ///
    /// - HashMap internal storage overhead
    /// - PropertyKey storage (interned, so minimal)
    /// - PropertyValue heap allocations (strings, vectors, etc.)
    ///
    /// Note: Due to Arc sharing, actual memory usage may be lower if this
    /// PropertyMap shares its underlying data with other instances.
    pub fn estimated_heap_size(&self) -> usize {
        // HashMap overhead: capacity * (key_size + value_size + ~8 bytes overhead per entry)
        let mut size = self.inner.capacity()
            * (std::mem::size_of::<PropertyKey>() + std::mem::size_of::<PropertyValue>() + 8);

        // Add heap sizes of individual values
        for value in self.inner.values() {
            size += value.estimated_heap_size();
        }

        size
    }

    /// Calculate the number of bytes required for serialization.
    ///
    /// This returns a cached value calculated during map construction, providing
    /// O(1) access. This is critical for WAL performance where we need to
    /// pre-allocate buffers.
    #[inline(always)]
    pub fn serialized_size(&self) -> usize {
        self.cached_size
    }
}

impl PartialEq for PropertyMap {
    fn eq(&self, other: &Self) -> bool {
        // Fast path: if both maps share the same underlying Arc, they are identical.
        // This is a O(1) check that avoids iterating over the map.
        //
        // NOTE: This treats shared pointers as equal even if they contain NaNs
        // (which normally compare unequal). This is generally desired for database
        // identity semantics but technically differs from strict IEEE 754 value equality.
        if Arc::ptr_eq(&self.inner, &other.inner) {
            return true;
        }

        // Fast path: if serialized sizes differ, maps must be different.
        if self.cached_size != other.cached_size {
            return false;
        }

        // Slow path: compare contents
        self.inner == other.inner
    }
}

impl fmt::Debug for PropertyMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        // Collect entries and pre-resolve keys to sort them for deterministic output
        // We map to (resolved_str, raw_id, value)
        let mut entries: Vec<_> = self
            .inner
            .iter()
            .map(|(key, value)| {
                let resolved = GLOBAL_INTERNER.resolve_with(*key, |s| s.to_string());
                (resolved, *key, value)
            })
            .collect();

        // Sort by resolved string if available, otherwise by ID
        // Resolved keys always come before unresolved ones for consistency
        entries.sort_by(|(s1, k1, _), (s2, k2, _)| match (s1, s2) {
            (Some(a), Some(b)) => a.cmp(b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => k1.cmp(k2),
        });

        for (resolved, key, value) in entries {
            if let Some(key_str) = resolved {
                map.entry(&key_str, value);
            } else {
                map.entry(&key, value);
            }
        }
        map.finish()
    }
}

impl Default for PropertyMap {
    fn default() -> Self {
        Self::new()
    }
}

impl FromIterator<(PropertyKey, PropertyValue)> for PropertyMap {
    fn from_iter<I: IntoIterator<Item = (PropertyKey, PropertyValue)>>(iter: I) -> Self {
        let mut map = HashMap::with_hasher(BuildHasherDefault::default());
        let mut size: usize = 4; // Count field

        for (key, value) in iter {
            // Need key size for serialization
            let key_len = GLOBAL_INTERNER
                .resolve_with(key, |s| s.len())
                .unwrap_or(256);
            let key_size = 4 + key_len;
            // Use penalty size if recursion limit exceeded to prevent panic.
            // Incorrect size here is safe because serialize() re-checks limits.
            // 10MB penalty discourages abuse.
            const RECURSION_PENALTY_SIZE: usize = 10 * 1024 * 1024;

            let val_size = value.serialized_size().unwrap_or(RECURSION_PENALTY_SIZE);

            size = size.saturating_add(key_size).saturating_add(val_size);

            if let Some(old_val) = map.insert(key, value) {
                // If replaced, subtract the size of the old entry (key + value)
                // Key size is the same since it's the same key ID
                size = size
                    .saturating_sub(key_size)
                    .saturating_sub(old_val.serialized_size().unwrap_or(RECURSION_PENALTY_SIZE));
            }
        }

        PropertyMap {
            inner: Arc::new(map),
            cached_size: size,
        }
    }
}

/// Builder for creating or modifying property maps with copy-on-write semantics.
pub struct PropertyMapBuilder {
    map: HashMap<PropertyKey, PropertyValue, BuildHasherDefault<IdentityHasher>>,
    current_size: usize,
}

impl PropertyMapBuilder {
    /// Create a new builder with an empty map.
    pub fn new() -> Self {
        PropertyMapBuilder {
            map: HashMap::with_hasher(BuildHasherDefault::default()),
            current_size: 4, // Count field
        }
    }

    /// Create a builder from an existing PropertyMap.
    ///
    /// This will clone the underlying HashMap if the Arc has multiple references,
    /// implementing copy-on-write semantics.
    pub fn from_map(prop_map: PropertyMap) -> Self {
        let current_size = prop_map.cached_size;
        let map = Arc::try_unwrap(prop_map.inner).unwrap_or_else(|arc| (*arc).clone());
        PropertyMapBuilder { map, current_size }
    }

    /// Insert a property.
    ///
    /// The key is automatically interned. If interning fails (capacity exceeded),
    /// returns self unchanged.
    ///
    /// Panics if recursion depth limit is exceeded.
    pub fn insert<V: Into<PropertyValue>>(self, key: &str, value: V) -> Self {
        self.try_insert(key, value)
            .expect("Property insertion failed (recursion depth limit exceeded)")
    }

    /// Insert a property (fallible).
    pub fn try_insert<V: Into<PropertyValue>>(mut self, key: &str, value: V) -> Result<Self> {
        // Warden: Propagate interning errors (e.g. CapacityExceeded) to prevent silent data loss.
        // Previously, failure to intern would silently drop the property, which is a security risk.
        let interned_key = GLOBAL_INTERNER.intern(key)?;
        let val = value.into();
        let val_size = val.serialized_size()?;

        if let Some(old_val) = self.map.insert(interned_key, val) {
            // Replaced existing entry
            // Key size is unchanged (same key ID means same string)
            self.current_size = self
                .current_size
                .saturating_sub(old_val.serialized_size()?)
                .saturating_add(val_size);
        } else {
            // New entry
            let key_size = 4 + key.len(); // Length prefix (4) + string bytes
            self.current_size = self
                .current_size
                .saturating_add(key_size)
                .saturating_add(val_size);
        }
        Ok(self)
    }

    /// Insert a property with an already-interned key.
    ///
    /// Panics if recursion depth limit is exceeded.
    pub fn insert_by_key(self, key: PropertyKey, value: PropertyValue) -> Self {
        self.try_insert_by_key(key, value)
            .expect("Property insertion failed (recursion depth limit exceeded)")
    }

    /// Insert a property with an already-interned key (fallible).
    pub fn try_insert_by_key(mut self, key: PropertyKey, value: PropertyValue) -> Result<Self> {
        let val_size = value.serialized_size()?;

        if let Some(old_val) = self.map.insert(key, value) {
            // Replaced existing entry - key size constant
            self.current_size = self
                .current_size
                .saturating_sub(old_val.serialized_size()?)
                .saturating_add(val_size);
        } else {
            // New entry - need key size!
            // We must look up the string length since we only have the ID.
            // This is a tradeoff: we pay lookup cost for new keys, but
            // avoid it for updates and for subsequent serialization size checks.
            let key_len = GLOBAL_INTERNER
                .resolve_with(key, |s| s.len())
                .unwrap_or_else(|| {
                    // This should be unreachable if the PropertyKey is valid (which it should be).
                    // In debug builds, we panic to catch this state corruption.
                    // In release, we fallback to a safe estimate (256 bytes) to avoid crashing.
                    debug_assert!(false, "PropertyKey {} missing from interner", key.as_u32());
                    256
                });
            let key_size = 4 + key_len;
            self.current_size = self
                .current_size
                .saturating_add(key_size)
                .saturating_add(val_size);
        }
        Ok(self)
    }

    /// Insert a vector property (convenience method for embeddings).
    ///
    /// This is a convenience wrapper around `insert()` for vector properties,
    /// commonly used for storing embeddings in nodes and edges.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::core::property::PropertyMapBuilder;
    ///
    /// let embedding = vec![0.1f32, 0.2, 0.3, 0.4];
    /// let props = PropertyMapBuilder::new()
    ///     .insert("name", "Document")
    ///     .insert_vector("embedding", &embedding)
    ///     .build();
    ///
    /// assert_eq!(
    ///     props.get("embedding").and_then(|v| v.as_vector()),
    ///     Some(&embedding[..])
    /// );
    /// ```
    pub fn insert_vector(self, key: &str, vector: &[f32]) -> Self {
        self.insert(key, PropertyValue::vector(vector))
    }

    /// Insert a vector property (fallible).
    pub fn try_insert_vector(self, key: &str, vector: &[f32]) -> Result<Self> {
        self.try_insert(key, PropertyValue::try_vector(vector)?)
    }

    /// Remove a property.
    ///
    /// The key is automatically interned before removal.
    /// If interning fails (capacity exceeded), returns self unchanged.
    ///
    /// Panics if serialization size calculation fails.
    pub fn remove(self, key: &str) -> Self {
        self.try_remove(key).expect("Property removal failed")
    }

    /// Remove a property (fallible).
    pub fn try_remove(self, key: &str) -> Result<Self> {
        let Some(interned_key) = GLOBAL_INTERNER.get_id(key) else {
            return Ok(self);
        };
        self.try_remove_by_key(&interned_key)
    }

    /// Remove a property by an already-interned key.
    ///
    /// Panics if serialization size calculation fails.
    pub fn remove_by_key(self, key: &PropertyKey) -> Self {
        self.try_remove_by_key(key)
            .expect("Property removal failed")
    }

    /// Remove a property by an already-interned key (fallible).
    pub fn try_remove_by_key(mut self, key: &PropertyKey) -> Result<Self> {
        let old_val = self.map.remove(key);
        if let Some(old_val) = old_val {
            let key_len = GLOBAL_INTERNER
                .resolve_with(*key, |s| s.len())
                .unwrap_or(256);
            let key_size = 4 + key_len;
            self.current_size = self
                .current_size
                .saturating_sub(key_size)
                .saturating_sub(old_val.serialized_size()?);
        }
        Ok(self)
    }

    /// Build the final PropertyMap.
    pub fn build(self) -> PropertyMap {
        PropertyMap {
            inner: Arc::new(self.map),
            cached_size: self.current_size,
        }
    }
}

impl Default for PropertyMapBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Macro for creating property maps with a convenient syntax.
///
/// # Examples
///
/// ```ignore
/// let props = properties! {
///     "name" => "Alice",
///     "age" => 30,
///     "active" => true,
/// };
/// ```
#[macro_export]
macro_rules! properties {
    ($($key:expr => $value:expr),* $(,)?) => {
        {
            let mut builder = $crate::core::property::PropertyMapBuilder::new();
            $(
                builder = builder.insert($key, $value);
            )*
            builder.build()
        }
    };
}

#[cfg(test)]
mod tests;
