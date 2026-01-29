//! Property system with Arc-based deduplication.
//!
//! This module provides a copy-on-write property system where properties are
//! stored in immutable, reference-counted containers. This enables:
//! - Cheap cloning of property maps (just increment reference count)
//! - Deduplication of unchanged properties across versions
//! - Zero-copy sharing of immutable data

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::vector::SparseVec;
use crate::utils::error::{Result, StorageError};

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
/// Type tag for Vector (dense f32 array) value.
pub const TAG_VECTOR: u8 = 7;
/// Type tag for SparseVector value.
pub const TAG_SPARSE_VECTOR: u8 = 8;

// ============================================================================
// Serialization Limits
// ============================================================================
// These limits prevent DoS attacks via memory exhaustion from malicious input.

/// Maximum number of elements allowed in a deserialized array.
/// Set to 1 million elements - enough for any practical use case.
pub const MAX_ARRAY_ELEMENTS: usize = 1_000_000;

/// Maximum number of dimensions allowed in a deserialized vector.
/// Set to 100,000 - far exceeds typical embedding sizes (384-4096 dimensions).
pub const MAX_VECTOR_DIMENSIONS: usize = 100_000;

/// Property key type.
///
/// Uses interned strings for memory efficiency and O(1) equality comparisons.
/// Common keys like "name", "age", and "id" are deduplicated in memory.
pub type PropertyKey = InternedString;

/// A value that can be stored as a property.
///
/// All complex types (strings, bytes, arrays) use Arc for cheap cloning and sharing.
#[derive(Debug, Clone, PartialEq)]
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
    /// use gallifreydb::core::PropertyValue;
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
    /// - [`gallifreydb::core::vector`](crate::core::vector) for similarity functions
    ///
    /// # Panics
    ///
    /// Panics if the vector dimension exceeds [`MAX_VECTOR_DIMENSIONS`].
    /// This validation ensures that vectors can be serialized without error.
    #[inline]
    pub fn vector<V: AsRef<[f32]>>(v: V) -> Self {
        let slice = v.as_ref();
        if slice.len() > MAX_VECTOR_DIMENSIONS {
            panic!(
                "Vector dimension {} exceeds maximum allowed {}",
                slice.len(),
                MAX_VECTOR_DIMENSIONS
            );
        }
        PropertyValue::Vector(Arc::from(slice))
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
    /// use gallifreydb::core::PropertyValue;
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
    /// - [`gallifreydb::core::vector`](crate::core::vector) for similarity functions
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
    /// use gallifreydb::core::PropertyValue;
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
    /// use gallifreydb::core::PropertyValue;
    /// use gallifreydb::core::vector::SparseVec;
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
    /// use gallifreydb::core::PropertyValue;
    /// use gallifreydb::core::vector::SparseVec;
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
    /// | Null   | [tag:1]                                     |
    /// | Bool   | [tag:1][value:1]                           |
    /// | Int    | [tag:1][i64:8]                             |
    /// | Float  | [tag:1][f64:8]                             |
    /// | String | [tag:1][len:4][utf8_bytes:len]             |
    /// | Bytes  | [tag:1][len:4][bytes:len]                  |
    /// | Array  | [tag:1][count:4][elements...]              |
    /// | Vector | [tag:1][dim:4][f32_values:dim*4]           |
    pub fn serialize(&self) -> Vec<u8> {
        let mut buffer = Vec::new();
        self.serialize_into(&mut buffer);
        buffer
    }

    /// Serialize this PropertyValue into an existing buffer.
    ///
    /// This is more efficient when serializing multiple values as it avoids
    /// allocating a new Vec for each value.
    pub fn serialize_into(&self, buffer: &mut Vec<u8>) {
        match self {
            PropertyValue::Null => {
                buffer.push(TAG_NULL);
            }
            PropertyValue::Bool(b) => {
                buffer.push(TAG_BOOL);
                buffer.push(if *b { 1 } else { 0 });
            }
            PropertyValue::Int(i) => {
                buffer.push(TAG_INT);
                buffer.extend_from_slice(&i.to_le_bytes());
            }
            PropertyValue::Float(f) => {
                buffer.push(TAG_FLOAT);
                buffer.extend_from_slice(&f.to_le_bytes());
            }
            PropertyValue::String(s) => {
                buffer.push(TAG_STRING);
                let bytes = s.as_bytes();
                buffer.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                buffer.extend_from_slice(bytes);
            }
            PropertyValue::Bytes(b) => {
                buffer.push(TAG_BYTES);
                buffer.extend_from_slice(&(b.len() as u32).to_le_bytes());
                buffer.extend_from_slice(b);
            }
            PropertyValue::Array(arr) => {
                buffer.push(TAG_ARRAY);
                buffer.extend_from_slice(&(arr.len() as u32).to_le_bytes());
                for item in arr.iter() {
                    item.serialize_into(buffer);
                }
            }
            PropertyValue::Vector(v) => {
                serialize_vector_into(v, buffer);
            }
            PropertyValue::SparseVector(sv) => {
                serialize_sparse_vector_into(sv, buffer);
            }
        }
    }

    /// Deserialize a PropertyValue from bytes.
    ///
    /// Returns the deserialized value and the number of bytes consumed.
    pub fn deserialize(bytes: &[u8]) -> Result<(Self, usize)> {
        if bytes.is_empty() {
            return Err(StorageError::CorruptedData(
                "Empty buffer when deserializing PropertyValue".to_string(),
            )
            .into());
        }

        let tag = bytes[0];
        let mut offset = 1;

        match tag {
            TAG_NULL => Ok((PropertyValue::Null, offset)),

            TAG_BOOL => {
                if bytes.len() < 2 {
                    return Err(StorageError::CorruptedData(
                        "Buffer too short for Bool value".to_string(),
                    )
                    .into());
                }
                let value = bytes[1] != 0;
                Ok((PropertyValue::Bool(value), 2))
            }

            TAG_INT => {
                if bytes.len() < 9 {
                    return Err(StorageError::CorruptedData(
                        "Buffer too short for Int value".to_string(),
                    )
                    .into());
                }
                // SAFETY: Length check above guarantees slice has 8 bytes
                let value = i64::from_le_bytes(bytes[1..9].try_into().unwrap());
                Ok((PropertyValue::Int(value), 9))
            }

            TAG_FLOAT => {
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

            TAG_STRING => {
                if bytes.len() < 5 {
                    return Err(StorageError::CorruptedData(
                        "Buffer too short for String length".to_string(),
                    )
                    .into());
                }
                let len = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
                offset = 5;

                if bytes.len() < offset + len {
                    return Err(StorageError::CorruptedData(format!(
                        "Buffer too short for String data: need {} bytes, have {}",
                        offset + len,
                        bytes.len()
                    ))
                    .into());
                }

                let string_data = &bytes[offset..offset + len];
                let s = std::str::from_utf8(string_data).map_err(|e| {
                    StorageError::CorruptedData(format!("Invalid UTF-8 in String: {}", e))
                })?;
                Ok((PropertyValue::String(Arc::from(s)), offset + len))
            }

            TAG_BYTES => {
                if bytes.len() < 5 {
                    return Err(StorageError::CorruptedData(
                        "Buffer too short for Bytes length".to_string(),
                    )
                    .into());
                }
                let len = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
                offset = 5;

                if bytes.len() < offset + len {
                    return Err(StorageError::CorruptedData(format!(
                        "Buffer too short for Bytes data: need {} bytes, have {}",
                        offset + len,
                        bytes.len()
                    ))
                    .into());
                }

                let byte_data = &bytes[offset..offset + len];
                Ok((PropertyValue::Bytes(Arc::from(byte_data)), offset + len))
            }

            TAG_ARRAY => {
                if bytes.len() < 5 {
                    return Err(StorageError::CorruptedData(
                        "Buffer too short for Array count".to_string(),
                    )
                    .into());
                }
                let count = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
                offset = 5;

                // Prevent DoS via memory exhaustion from malicious input
                if count > MAX_ARRAY_ELEMENTS {
                    return Err(StorageError::CorruptedData(format!(
                        "Array count {} exceeds maximum allowed {}",
                        count, MAX_ARRAY_ELEMENTS
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
                    let (item, consumed) = PropertyValue::deserialize(&bytes[offset..])?;
                    items.push(item);
                    offset += consumed;
                }
                Ok((PropertyValue::Array(Arc::new(items)), offset))
            }

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
    /// use gallifreydb::core::PropertyValue;
    ///
    /// let small = PropertyValue::Int(42);
    /// assert_eq!(small.estimated_heap_size(), 0); // No heap allocation
    ///
    /// let string = PropertyValue::string("hello world");
    /// assert_eq!(string.estimated_heap_size(), 11); // String length
    /// ```
    pub fn estimated_heap_size(&self) -> usize {
        match self {
            PropertyValue::Null
            | PropertyValue::Bool(_)
            | PropertyValue::Int(_)
            | PropertyValue::Float(_) => 0,
            PropertyValue::String(s) => s.len(),
            PropertyValue::Bytes(b) => b.len(),
            PropertyValue::Array(arr) => {
                // Vec capacity overhead + recursive element sizes
                let mut size = arr.capacity() * std::mem::size_of::<PropertyValue>();
                for item in arr.iter() {
                    size += item.estimated_heap_size();
                }
                size
            }
            PropertyValue::Vector(v) => v.len() * std::mem::size_of::<f32>(),
            PropertyValue::SparseVector(sv) => {
                // Indices + values + SparseVec struct overhead
                sv.nnz() * (std::mem::size_of::<u32>() + std::mem::size_of::<f32>())
                    + std::mem::size_of::<usize>() // dimension field
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
    pub fn serialized_size(&self) -> usize {
        match self {
            PropertyValue::Null => 1,
            PropertyValue::Bool(_) => 2,
            PropertyValue::Int(_) => 9,
            PropertyValue::Float(_) => 9,
            PropertyValue::String(s) => 1 + 4 + s.len(),
            PropertyValue::Bytes(b) => 1 + 4 + b.len(),
            PropertyValue::Array(arr) => {
                let elements_size: usize = arr.iter().map(|v| v.serialized_size()).sum();
                1 + 4 + elements_size
            }
            PropertyValue::Vector(v) => 1 + 4 + (v.len() * 4),
            PropertyValue::SparseVector(sv) => 1 + 4 + 4 + (sv.nnz() * 8),
        }
    }
}

// ============================================================================
// Vector Serialization Functions
// ============================================================================

/// Serialize a vector (dense f32 array) to bytes.
///
/// # Binary Format
/// ```text
/// [tag:1][dimension:4][f32_0:4][f32_1:4]...[f32_n:4]
/// ```
///
/// - Tag: TAG_VECTOR (7)
/// - Dimension: u32 little-endian, number of elements
/// - Values: f32 little-endian, the vector elements
///
/// # Arguments
/// * `v` - The vector data to serialize
///
/// # Returns
/// A `Vec<u8>` containing the serialized vector
///
/// # Example
/// ```ignore
/// let embedding = [0.1f32, 0.2, 0.3];
/// let bytes = serialize_vector(&embedding);
/// // bytes = [7, 3, 0, 0, 0, <12 bytes of f32 data>]
/// ```
pub fn serialize_vector(v: &[f32]) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(1 + 4 + v.len() * 4);
    serialize_vector_into(v, &mut buffer);
    buffer
}

/// Serialize a vector into an existing buffer.
///
/// This is more efficient when serializing as part of a larger structure.
///
/// # Performance Optimization (Issue #203)
///
/// On little-endian platforms (x86, ARM, etc.), this uses bulk byte copying
/// instead of serializing each f32 individually, providing significant speedup
/// for typical embedding sizes.
///
/// **Benchmark results (1536 dimensions):**
/// - Serialization: ~73ns @ 19.7 GiB/s
/// - Deserialization: ~217ns @ 26.3 GiB/s
/// - Round-trip: ~308ns @ 37.2 GiB/s
///
/// # Panics
///
/// Panics if the vector dimension exceeds `MAX_VECTOR_DIMENSIONS`.
/// This is a defensive check; vectors should be validated at construction
/// time via [`PropertyValue::vector()`] which enforces this limit.
pub fn serialize_vector_into(v: &[f32], buffer: &mut Vec<u8>) {
    // Defensive check: vectors should be validated at construction via PropertyValue::vector()
    if v.len() > MAX_VECTOR_DIMENSIONS {
        panic!(
            "Vector dimension {} exceeds maximum allowed {}",
            v.len(),
            MAX_VECTOR_DIMENSIONS
        );
    }

    // Pre-allocate space to avoid multiple reallocations
    // Total: 1 byte (tag) + 4 bytes (length) + v.len() * 4 bytes (data)
    let required_size = 1 + 4 + std::mem::size_of_val(v);
    buffer.reserve(required_size);

    buffer.push(TAG_VECTOR);
    buffer.extend_from_slice(&(v.len() as u32).to_le_bytes());

    #[cfg(target_endian = "little")]
    {
        // SAFETY: On little-endian platforms, f32 in-memory representation
        // is identical to its to_le_bytes() output. This allows us to
        // directly copy the entire f32 slice as bytes instead of converting
        // each element individually.
        //
        // This is safe because:
        // 1. f32 has well-defined byte representation (IEEE 754)
        // 2. We're only reading, not writing through the raw pointer
        // 3. The slice lengths are correctly calculated. With the dimension check,
        //    overflow is not possible on 64-bit or 32-bit systems.
        // 4. Alignment is not an issue - we're copying to a Vec<u8>
        let byte_slice = unsafe {
            std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v))
        };
        buffer.extend_from_slice(byte_slice);
    }

    #[cfg(not(target_endian = "little"))]
    {
        // Big-endian fallback: convert each element individually
        for &value in v {
            buffer.extend_from_slice(&value.to_le_bytes());
        }
    }
}

/// Deserialize a vector from bytes.
///
/// # Binary Format
/// Expects the format produced by `serialize_vector`:
/// ```text
/// [tag:1][dimension:4][f32_values:dimension*4]
/// ```
///
/// # Arguments
/// * `bytes` - The byte slice to deserialize from
///
/// # Returns
/// * `Ok((Arc<[f32]>, usize))` - The deserialized vector and bytes consumed
/// * `Err` - If the data is malformed or truncated
///
/// # Errors
/// - `StorageError::CorruptedData` if buffer is too short
/// - `StorageError::CorruptedData` if type tag is not TAG_VECTOR
///
/// # Example
/// ```ignore
/// let bytes = serialize_vector(&[0.1f32, 0.2, 0.3]);
/// let (vector, consumed) = deserialize_vector(&bytes)?;
/// assert_eq!(vector.as_ref(), &[0.1f32, 0.2, 0.3]);
/// ```
pub fn deserialize_vector(bytes: &[u8]) -> Result<(Arc<[f32]>, usize)> {
    // Need at least tag (1) + dimension (4) = 5 bytes
    if bytes.len() < 5 {
        return Err(
            StorageError::CorruptedData("Buffer too short for vector header".to_string()).into(),
        );
    }

    let tag = bytes[0];
    if tag != TAG_VECTOR {
        return Err(StorageError::CorruptedData(format!(
            "Expected vector type tag {}, got {}",
            TAG_VECTOR, tag
        ))
        .into());
    }

    let dimension = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;

    // Prevent DoS via memory exhaustion from malicious input
    if dimension > MAX_VECTOR_DIMENSIONS {
        return Err(StorageError::CorruptedData(format!(
            "Vector dimension {} exceeds maximum allowed {}",
            dimension, MAX_VECTOR_DIMENSIONS
        ))
        .into());
    }

    // Calculate total length with overflow check
    let data_start: usize = 5;
    let data_len = dimension
        .checked_mul(4)
        .ok_or_else(|| StorageError::CorruptedData("Vector dimension overflow".to_string()))?;
    let total_len = data_start
        .checked_add(data_len)
        .ok_or_else(|| StorageError::CorruptedData("Vector size overflow".to_string()))?;

    // Validate buffer size before allocating
    if bytes.len() < total_len {
        return Err(StorageError::CorruptedData(format!(
            "Buffer too short for vector data: need {} bytes, have {}",
            total_len,
            bytes.len()
        ))
        .into());
    }

    // Deserialize f32 values
    // Performance optimization (Issue #203): use bulk byte copy on little-endian
    let data_slice = &bytes[data_start..total_len];

    #[cfg(target_endian = "little")]
    let values = {
        // SAFETY: On little-endian platforms, we can directly copy the bytes
        // into an f32 vector using a single bulk memory operation.
        //
        // This is safe because:
        // 1. We validated data_slice.len() == dimension * 4 above.
        // 2. We allocate a Vec<f32> with sufficient capacity. Its buffer is correctly
        //    aligned for f32.
        // 3. `copy_nonoverlapping` safely copies bytes from the (potentially unaligned)
        //    `data_slice` into the aligned `Vec` buffer.
        // 4. After the copy, the memory is initialized, so calling `set_len` is safe.
        // 5. Any bit pattern is valid for f32 (including NaN, infinity).
        let mut values = Vec::with_capacity(dimension);
        if dimension > 0 {
            unsafe {
                let src_ptr = data_slice.as_ptr();
                // The destination pointer is correctly aligned for f32.
                let dst_ptr = values.as_mut_ptr() as *mut u8;
                std::ptr::copy_nonoverlapping(src_ptr, dst_ptr, data_slice.len());
                values.set_len(dimension);
            }
        }
        values
    };

    #[cfg(not(target_endian = "little"))]
    let values = {
        // Big-endian fallback: convert each element individually
        let mut values = Vec::with_capacity(dimension);
        for chunk in data_slice.chunks_exact(4) {
            // SAFETY: chunks_exact guarantees exactly 4 bytes per chunk
            values.push(f32::from_le_bytes(chunk.try_into().unwrap()));
        }
        values
    };

    Ok((Arc::from(values.into_boxed_slice()), total_len))
}

// ============================================================================
// Sparse Vector Serialization Functions
// ============================================================================

/// Serialize a sparse vector to bytes.
///
/// # Binary Format
/// ```text
/// [tag:1][dimension:4][nnz:4][index_0:4]...[index_n:4][value_0:4]...[value_n:4]
/// ```
///
/// - Tag: TAG_SPARSE_VECTOR (8)
/// - Dimension: u32 little-endian, total vector dimension
/// - NNZ: u32 little-endian, number of non-zero elements
/// - Indices: u32 little-endian array of non-zero positions
/// - Values: f32 little-endian array of non-zero values
///
/// # Arguments
/// * `sv` - The sparse vector to serialize
///
/// # Returns
/// A `Vec<u8>` containing the serialized sparse vector
pub fn serialize_sparse_vector(sv: &SparseVec) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(1 + 4 + 4 + sv.nnz() * 8);
    serialize_sparse_vector_into(sv, &mut buffer);
    buffer
}

/// Serialize a sparse vector into an existing buffer.
///
/// This is more efficient when serializing as part of a larger structure.
pub fn serialize_sparse_vector_into(sv: &SparseVec, buffer: &mut Vec<u8>) {
    buffer.push(TAG_SPARSE_VECTOR);
    buffer.extend_from_slice(&(sv.dimension() as u32).to_le_bytes());
    buffer.extend_from_slice(&(sv.nnz() as u32).to_le_bytes());

    // Serialize indices
    for &idx in sv.indices() {
        buffer.extend_from_slice(&idx.to_le_bytes());
    }

    // Serialize values
    for &val in sv.values() {
        buffer.extend_from_slice(&val.to_le_bytes());
    }
}

/// Deserialize a sparse vector from bytes.
///
/// # Binary Format
/// Expects the format produced by `serialize_sparse_vector`:
/// ```text
/// [tag:1][dimension:4][nnz:4][indices:nnz*4][values:nnz*4]
/// ```
///
/// # Arguments
/// * `bytes` - The byte slice to deserialize from
///
/// # Returns
/// * `Ok((Arc<SparseVec>, usize))` - The deserialized sparse vector and bytes consumed
/// * `Err` - If the data is malformed or truncated
///
/// # Errors
/// - `StorageError::CorruptedData` if buffer is too short
/// - `StorageError::CorruptedData` if type tag is not TAG_SPARSE_VECTOR
/// - `VectorError` variants if sparse vector construction fails
pub fn deserialize_sparse_vector(bytes: &[u8]) -> Result<(Arc<SparseVec>, usize)> {
    // Need at least tag (1) + dimension (4) + nnz (4) = 9 bytes
    if bytes.len() < 9 {
        return Err(StorageError::CorruptedData(
            "Buffer too short for sparse vector header".to_string(),
        )
        .into());
    }

    let tag = bytes[0];
    if tag != TAG_SPARSE_VECTOR {
        return Err(StorageError::CorruptedData(format!(
            "Expected sparse vector type tag {}, got {}",
            TAG_SPARSE_VECTOR, tag
        ))
        .into());
    }

    let dimension = u32::from_le_bytes(bytes[1..5].try_into().unwrap());
    let nnz = u32::from_le_bytes(bytes[5..9].try_into().unwrap()) as usize;

    // Validate nnz doesn't exceed dimension
    if nnz > dimension as usize {
        return Err(StorageError::CorruptedData(format!(
            "Sparse vector nnz {} exceeds dimension {}",
            nnz, dimension
        ))
        .into());
    }

    // Calculate required size
    let data_start: usize = 9;
    let indices_len = nnz
        .checked_mul(4)
        .ok_or_else(|| StorageError::CorruptedData("Sparse vector nnz overflow".to_string()))?;
    let values_len = indices_len; // Same size for values
    let total_len = data_start
        .checked_add(indices_len)
        .and_then(|x: usize| x.checked_add(values_len))
        .ok_or_else(|| StorageError::CorruptedData("Sparse vector size overflow".to_string()))?;

    // Validate buffer size
    if bytes.len() < total_len {
        return Err(StorageError::CorruptedData(format!(
            "Buffer too short for sparse vector data: need {} bytes, have {}",
            total_len,
            bytes.len()
        ))
        .into());
    }

    // Deserialize indices
    let indices_end = data_start + indices_len;
    let indices_slice = &bytes[data_start..indices_end];
    let mut indices = Vec::with_capacity(nnz);
    for chunk in indices_slice.chunks_exact(4) {
        indices.push(u32::from_le_bytes(chunk.try_into().unwrap()));
    }

    // Deserialize values
    let values_end = indices_end + values_len;
    let values_slice = &bytes[indices_end..values_end];
    let mut values = Vec::with_capacity(nnz);
    for chunk in values_slice.chunks_exact(4) {
        values.push(f32::from_le_bytes(chunk.try_into().unwrap()));
    }

    // Construct SparseVec (this will validate the data)
    let sparse_vec = SparseVec::new(indices, values, dimension)?;

    Ok((Arc::new(sparse_vec), total_len))
}

impl fmt::Display for PropertyValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
                    write!(f, "{}", v)?;
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
        // See: https://github.com/madmax983/GallifreyDB/issues/200
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
        // See: https://github.com/madmax983/GallifreyDB/issues/200
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
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyMap {
    inner: Arc<HashMap<PropertyKey, PropertyValue>>,
}

impl PropertyMap {
    /// Create a new empty property map.
    pub fn new() -> Self {
        PropertyMap {
            inner: Arc::new(HashMap::new()),
        }
    }

    /// Create a property map with the specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        PropertyMap {
            inner: Arc::new(HashMap::with_capacity(capacity)),
        }
    }

    /// Get a property value by key.
    ///
    /// The key is automatically interned before lookup for efficient comparison.
    /// Returns None if the key is not found or if interning fails.
    #[inline]
    pub fn get(&self, key: &str) -> Option<&PropertyValue> {
        let interned_key = GLOBAL_INTERNER.intern(key).ok()?;
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
    /// The key is automatically interned before lookup for efficient comparison.
    /// Returns false if interning fails (key cannot exist if it can't be interned).
    #[inline]
    pub fn contains_key(&self, key: &str) -> bool {
        let Ok(interned_key) = GLOBAL_INTERNER.intern(key) else {
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
        let mut buffer = Vec::new();
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
        buffer.extend_from_slice(&(self.inner.len() as u32).to_le_bytes());
        for (key, value) in self.inner.iter() {
            // Serialize key: resolve InternedString to actual string
            // Use with_str to avoid Arc cloning overhead
            GLOBAL_INTERNER
                .with_str(*key, |key_str| {
                    let key_bytes = key_str.as_bytes();
                    buffer.extend_from_slice(&(key_bytes.len() as u32).to_le_bytes());
                    buffer.extend_from_slice(key_bytes);
                })
                .ok_or_else(|| {
                    crate::utils::error::Error::Storage(StorageError::InconsistentState {
                        reason: format!(
                            "PropertyKey {} not found in interner - data corruption detected",
                            key.as_u32()
                        ),
                    })
                })?;

            // Serialize value
            value.serialize_into(buffer);
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
        let mut offset = 4;
        let mut map = HashMap::with_capacity(count);

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
            offset += consumed;

            map.insert(key, value);
        }

        Ok((
            PropertyMap {
                inner: Arc::new(map),
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
    /// This is used to pre-allocate buffers for serialization, avoiding reallocation
    /// overhead during high-throughput operations.
    pub fn serialized_size(&self) -> usize {
        if self.is_empty() {
            return 4; // Count field
        }

        let mut size = 4; // Count field
        for (key, value) in self.inner.iter() {
            // Key: length prefix (4) + key bytes
            // Use with_str to avoid Arc cloning overhead (Issue #405)
            let key_len = GLOBAL_INTERNER.with_str(*key, |s| s.len()).unwrap_or(256); // Conservative fallback if key missing (corruption)

            size += 4 + key_len;
            size += value.serialized_size();
        }
        size
    }
}

impl Default for PropertyMap {
    fn default() -> Self {
        Self::new()
    }
}

impl FromIterator<(PropertyKey, PropertyValue)> for PropertyMap {
    fn from_iter<I: IntoIterator<Item = (PropertyKey, PropertyValue)>>(iter: I) -> Self {
        PropertyMap {
            inner: Arc::new(iter.into_iter().collect()),
        }
    }
}

/// Builder for creating or modifying property maps with copy-on-write semantics.
pub struct PropertyMapBuilder {
    map: HashMap<PropertyKey, PropertyValue>,
}

impl PropertyMapBuilder {
    /// Create a new builder with an empty map.
    pub fn new() -> Self {
        PropertyMapBuilder {
            map: HashMap::new(),
        }
    }

    /// Create a builder from an existing PropertyMap.
    ///
    /// This will clone the underlying HashMap if the Arc has multiple references,
    /// implementing copy-on-write semantics.
    pub fn from_map(prop_map: PropertyMap) -> Self {
        let map = Arc::try_unwrap(prop_map.inner).unwrap_or_else(|arc| (*arc).clone());
        PropertyMapBuilder { map }
    }

    /// Insert a property.
    ///
    /// The key is automatically interned. If interning fails (capacity exceeded),
    /// returns self unchanged.
    pub fn insert<V: Into<PropertyValue>>(mut self, key: &str, value: V) -> Self {
        let Ok(interned_key) = GLOBAL_INTERNER.intern(key) else {
            return self;
        };
        self.map.insert(interned_key, value.into());
        self
    }

    /// Insert a property with an already-interned key.
    ///
    /// This is more efficient than `insert()` when you already have a PropertyKey.
    /// For internal use and performance-critical paths.
    pub fn insert_by_key(mut self, key: PropertyKey, value: PropertyValue) -> Self {
        self.map.insert(key, value);
        self
    }

    /// Insert a vector property (convenience method for embeddings).
    ///
    /// This is a convenience wrapper around `insert()` for vector properties,
    /// commonly used for storing embeddings in nodes and edges.
    ///
    /// # Example
    ///
    /// ```rust
    /// use gallifreydb::core::property::PropertyMapBuilder;
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

    /// Remove a property.
    ///
    /// The key is automatically interned before removal.
    /// If interning fails (capacity exceeded), returns self unchanged.
    pub fn remove(self, key: &str) -> Self {
        let Ok(interned_key) = GLOBAL_INTERNER.intern(key) else {
            return self;
        };
        self.remove_by_key(&interned_key)
    }

    /// Remove a property by an already-interned key.
    ///
    /// This is more efficient than `remove()` when you already have an InternedString.
    /// For internal use and performance-critical paths.
    pub fn remove_by_key(mut self, key: &PropertyKey) -> Self {
        self.map.remove(key);
        self
    }

    /// Build the final PropertyMap.
    pub fn build(self) -> PropertyMap {
        PropertyMap {
            inner: Arc::new(self.map),
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
mod tests {
    use super::*;

    #[test]
    fn test_property_value_types() {
        assert!(PropertyValue::Null.is_null());
        assert_eq!(PropertyValue::Bool(true).as_bool(), Some(true));
        assert_eq!(PropertyValue::Int(42).as_int(), Some(42));
        assert_eq!(PropertyValue::Float(2.5).as_float(), Some(2.5));

        let s = PropertyValue::string("hello");
        assert_eq!(s.as_str(), Some("hello"));

        let b = PropertyValue::bytes([1, 2, 3]);
        assert_eq!(b.as_bytes(), Some(&[1u8, 2, 3][..]));

        let arr = PropertyValue::array(vec![PropertyValue::Int(1), PropertyValue::Int(2)]);
        assert_eq!(arr.as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_property_value_from() {
        let _: PropertyValue = true.into();
        let _: PropertyValue = 42i64.into();
        let _: PropertyValue = 42i32.into();
        let _: PropertyValue = 2.5f64.into();
        let _: PropertyValue = "hello".into();
        let _: PropertyValue = String::from("world").into();
        let _: PropertyValue = vec![1u8, 2, 3].into();
    }

    #[test]
    fn test_string_to_property_value_efficient_conversion() {
        // Issue #200: Verify that From<String> for PropertyValue uses
        // efficient conversion without unnecessary copying.
        // The implementation should use Arc::from(s) which leverages
        // String → Box<str> → Arc<str> conversion chain.

        // Create an owned String
        let content = "test string for efficient conversion";
        let original = String::from(content);

        // Convert to PropertyValue - should consume the String
        let prop_value: PropertyValue = original.into();

        // Verify the value is stored correctly
        assert_eq!(
            prop_value.as_str(),
            Some(content),
            "PropertyValue should contain the original string content"
        );

        // Verify it's a String variant
        assert!(matches!(prop_value, PropertyValue::String(_)));

        // Additional test: Verify the conversion works in PropertyMapBuilder
        let test_string = String::from("builder test");
        let map = PropertyMapBuilder::new().insert("key", test_string).build();

        assert_eq!(
            map.get("key").and_then(|v| v.as_str()),
            Some("builder test"),
            "PropertyMapBuilder should handle owned String efficiently"
        );
    }

    #[test]
    fn test_vec_u8_to_property_value_efficient_conversion() {
        // Issue #200, #201: Verify that From<Vec<u8>> for PropertyValue uses
        // efficient conversion without unnecessary copying.
        // The implementation should use Arc::from(v) which leverages
        // Vec<u8> → Box<[u8]> → Arc<[u8]> conversion chain.

        // Create an owned Vec<u8>
        let content: &[u8] = &[1u8, 2, 3, 4, 5, 42, 255];
        let original = content.to_vec();

        // Convert to PropertyValue - should consume the Vec
        let prop_value: PropertyValue = original.into();

        // Verify the value is stored correctly
        assert_eq!(
            prop_value.as_bytes(),
            Some(content),
            "PropertyValue should contain the original byte content"
        );

        // Verify it's a Bytes variant
        assert!(matches!(prop_value, PropertyValue::Bytes(_)));
    }

    #[test]
    fn test_vec_u8_to_property_value_consumes_vec() {
        // Issue #201: Verify that From<Vec<u8>> efficiently consumes the Vec
        // rather than copying from a slice.
        //
        // The implementation uses Arc::from(vec) which consumes the Vec and can
        // potentially reuse its buffer, rather than Arc::from(vec.as_slice())
        // which would copy from the slice while leaving the Vec to be dropped.
        //
        // Note: Arc<[u8]> requires its own allocation (for ref count + data),
        // so we can't test for pointer equality. What we verify is that:
        // 1. The Vec is consumed (move semantics)
        // 2. Data is preserved correctly
        // 3. The conversion works for large payloads without double-allocation

        // Create a large Vec to make efficient conversion meaningful
        let size = 10_000;
        let mut original = Vec::with_capacity(size);
        for i in 0..size {
            original.push((i % 256) as u8);
        }

        // Convert to PropertyValue - this should consume the Vec (move semantics)
        let prop_value: PropertyValue = original.into();
        // Note: `original` is now moved and cannot be used

        // Extract the Arc<[u8]> from the PropertyValue
        if let PropertyValue::Bytes(arc_bytes) = prop_value {
            // Verify the length is correct
            assert_eq!(
                arc_bytes.len(),
                size,
                "PropertyValue should contain all elements"
            );

            // Verify data content is preserved correctly
            for (i, &byte) in arc_bytes.iter().enumerate() {
                assert_eq!(
                    byte,
                    (i % 256) as u8,
                    "Data should be preserved correctly at index {i}"
                );
            }
        } else {
            panic!("PropertyValue should be Bytes variant");
        }
    }

    #[test]
    fn test_vec_u8_empty_conversion() {
        // Issue #201: Verify edge case of empty Vec<u8> conversion
        let empty_vec: Vec<u8> = Vec::new();
        let prop_value: PropertyValue = empty_vec.into();

        assert_eq!(
            prop_value.as_bytes(),
            Some(&[] as &[u8]),
            "Empty Vec should convert to empty Bytes"
        );
        assert!(matches!(prop_value, PropertyValue::Bytes(_)));
    }

    #[test]
    fn test_vec_u8_large_payload_conversion() {
        // Issue #201: Verify that large binary payloads convert efficiently
        // without unnecessary copying. The implementation should use Arc::from(vec)
        // which consumes the Vec, rather than Arc::from(vec.as_slice()) which
        // would copy the data.
        let size = 1_000_000; // 1MB
        let large_vec: Vec<u8> = vec![0x42; size];

        let prop_value: PropertyValue = large_vec.into();

        if let PropertyValue::Bytes(arc_bytes) = &prop_value {
            // Verify the large payload is stored correctly
            assert_eq!(arc_bytes.len(), size);
            assert!(arc_bytes.iter().all(|&b| b == 0x42));
        } else {
            panic!("PropertyValue should be Bytes variant");
        }
    }

    #[test]
    fn test_property_map_creation() {
        let map = PropertyMap::new();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);

        let map = PropertyMap::with_capacity(10);
        assert!(map.is_empty());
    }

    #[test]
    fn test_property_map_builder() {
        let map = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("active", true)
            .build();

        assert_eq!(map.len(), 3);
        assert_eq!(map.get("name").and_then(|v| v.as_str()), Some("Alice"));
        assert_eq!(map.get("age").and_then(|v| v.as_int()), Some(30));
        assert_eq!(map.get("active").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn test_property_map_copy_on_write() {
        let map1 = PropertyMapBuilder::new().insert("key", "value1").build();

        // Clone is cheap (just Arc increment)
        let map2 = map1.clone();
        assert_eq!(map1, map2);

        // Modify map2 (should not affect map1 due to copy-on-write)
        let map2 = map2.builder().insert("key", "value2").build();

        assert_ne!(map1, map2);
        assert_eq!(map1.get("key").and_then(|v| v.as_str()), Some("value1"));
        assert_eq!(map2.get("key").and_then(|v| v.as_str()), Some("value2"));
    }

    #[test]
    fn test_property_map_iteration() {
        let map = PropertyMapBuilder::new()
            .insert("a", 1i64)
            .insert("b", 2i64)
            .insert("c", 3i64)
            .build();

        let keys: Vec<_> = map.keys().cloned().collect();
        assert_eq!(keys.len(), 3);
        assert!(keys.contains(&GLOBAL_INTERNER.intern("a").unwrap()));
        assert!(keys.contains(&GLOBAL_INTERNER.intern("b").unwrap()));
        assert!(keys.contains(&GLOBAL_INTERNER.intern("c").unwrap()));

        let values: Vec<_> = map.values().cloned().collect();
        assert_eq!(values.len(), 3);
    }

    #[test]
    fn test_properties_macro() {
        let map = properties! {
            "name" => "Bob",
            "age" => 25,
            "score" => 98.5,
        };

        assert_eq!(map.len(), 3);
        assert_eq!(map.get("name").and_then(|v| v.as_str()), Some("Bob"));
        assert_eq!(map.get("age").and_then(|v| v.as_int()), Some(25));
        assert_eq!(map.get("score").and_then(|v| v.as_float()), Some(98.5));
    }

    #[test]
    fn test_property_value_display() {
        assert_eq!(format!("{}", PropertyValue::Null), "null");
        assert_eq!(format!("{}", PropertyValue::Bool(true)), "true");
        assert_eq!(format!("{}", PropertyValue::Int(42)), "42");
        assert_eq!(format!("{}", PropertyValue::Float(2.5)), "2.5");
        assert_eq!(format!("{}", PropertyValue::string("hello")), "\"hello\"");

        let arr = PropertyValue::array(vec![PropertyValue::Int(1), PropertyValue::Int(2)]);
        assert_eq!(format!("{}", arr), "[1, 2]");
    }

    #[test]
    fn test_arc_sharing() {
        // Create a property value with a large string
        let large_string = "x".repeat(1000);
        let prop1 = PropertyValue::string(&large_string);
        let prop2 = prop1.clone();

        // Both should point to the same Arc
        if let (PropertyValue::String(s1), PropertyValue::String(s2)) = (&prop1, &prop2) {
            assert!(Arc::ptr_eq(s1, s2), "Arc should be shared");
        }
    }

    // ========== Vector tests ==========

    #[test]
    fn test_vector_constructor() {
        let data = [1.0f32, 2.0, 3.0, 4.0];
        let vec_prop = PropertyValue::vector(data);

        assert_eq!(vec_prop.as_vector(), Some(&data[..]));
        assert_eq!(vec_prop.type_name(), "vector");
    }

    #[test]
    fn test_vector_from_vec() {
        let data: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        let vec_prop: PropertyValue = data.clone().into();

        assert_eq!(vec_prop.as_vector(), Some(&data[..]));
    }

    #[test]
    fn test_vector_from_slice() {
        let data = [1.5f32, 2.5, 3.5];
        let vec_prop: PropertyValue = (&data[..]).into();

        assert_eq!(vec_prop.as_vector(), Some(&data[..]));
    }

    #[test]
    fn test_vector_display() {
        let vec_prop = PropertyValue::vector([1.0f32, 2.0, 3.0]);
        assert_eq!(format!("{}", vec_prop), "<vector[3]>");

        // Test with common embedding dimensions
        let embedding_384 = vec![0.0f32; 384];
        let vec_prop = PropertyValue::vector(&embedding_384);
        assert_eq!(format!("{}", vec_prop), "<vector[384]>");

        let embedding_1536 = vec![0.0f32; 1536];
        let vec_prop = PropertyValue::vector(&embedding_1536);
        assert_eq!(format!("{}", vec_prop), "<vector[1536]>");
    }

    #[test]
    fn test_vector_arc_sharing() {
        // Create a large vector (typical embedding size)
        let embedding: Vec<f32> = (0..384).map(|i| i as f32 * 0.001).collect();
        let prop1 = PropertyValue::vector(&embedding);
        let prop2 = prop1.clone();

        // Both should point to the same Arc (cheap clone)
        if let (PropertyValue::Vector(v1), PropertyValue::Vector(v2)) = (&prop1, &prop2) {
            assert!(
                Arc::ptr_eq(v1, v2),
                "Vector Arc should be shared after clone"
            );
        }

        // Values should be equal
        assert_eq!(prop1, prop2);
        assert_eq!(prop1.as_vector(), prop2.as_vector());
    }

    #[test]
    fn test_vector_empty() {
        let empty: Vec<f32> = vec![];
        let vec_prop = PropertyValue::vector(&empty);

        assert_eq!(vec_prop.as_vector(), Some(&[][..]));
        assert_eq!(format!("{}", vec_prop), "<vector[0]>");
    }

    #[test]
    #[should_panic(expected = "Vector dimension")]
    fn test_vector_excessive_dimensions() {
        // Test that creating a vector with excessive dimensions panics
        let oversized: Vec<f32> = vec![0.0; MAX_VECTOR_DIMENSIONS + 1];
        let _ = PropertyValue::vector(oversized);
    }

    #[test]
    fn test_vector_max_dimensions_allowed() {
        // Test that MAX_VECTOR_DIMENSIONS is exactly the limit
        let max_size: Vec<f32> = vec![0.0; MAX_VECTOR_DIMENSIONS];
        let vec_prop = PropertyValue::vector(max_size);
        assert_eq!(vec_prop.as_vector().unwrap().len(), MAX_VECTOR_DIMENSIONS);
    }

    #[test]
    fn test_vector_accessor_wrong_type() {
        // as_vector should return None for non-vector types
        assert_eq!(PropertyValue::Null.as_vector(), None);
        assert_eq!(PropertyValue::Bool(true).as_vector(), None);
        assert_eq!(PropertyValue::Int(42).as_vector(), None);
        assert_eq!(PropertyValue::Float(1.5).as_vector(), None);
        assert_eq!(PropertyValue::string("hello").as_vector(), None);
        assert_eq!(PropertyValue::bytes([1, 2, 3]).as_vector(), None);
        assert_eq!(
            PropertyValue::array(vec![PropertyValue::Int(1)]).as_vector(),
            None
        );
    }

    #[test]
    fn test_vector_equality() {
        let v1 = PropertyValue::vector([1.0f32, 2.0, 3.0]);
        let v2 = PropertyValue::vector([1.0f32, 2.0, 3.0]);
        let v3 = PropertyValue::vector([1.0f32, 2.0, 4.0]);

        assert_eq!(v1, v2);
        assert_ne!(v1, v3);
    }

    #[test]
    fn test_vector_in_property_map() {
        let embedding = vec![0.1f32, 0.2, 0.3, 0.4];
        let map = PropertyMapBuilder::new()
            .insert("name", "test_node")
            .insert("embedding", PropertyValue::vector(&embedding))
            .build();

        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get("embedding").and_then(|v| v.as_vector()),
            Some(&embedding[..])
        );
    }

    // ========== Serialization Tests ==========

    #[test]
    fn test_serialize_null() {
        let value = PropertyValue::Null;
        let bytes = value.serialize();
        assert_eq!(bytes, vec![TAG_NULL]);

        let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn test_serialize_bool() {
        // Test true
        let value = PropertyValue::Bool(true);
        let bytes = value.serialize();
        assert_eq!(bytes, vec![TAG_BOOL, 1]);

        let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);
        assert_eq!(consumed, 2);

        // Test false
        let value = PropertyValue::Bool(false);
        let bytes = value.serialize();
        assert_eq!(bytes, vec![TAG_BOOL, 0]);

        let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn test_serialize_int() {
        let test_values = [0i64, 1, -1, i64::MAX, i64::MIN, 42, -12345];
        for &v in &test_values {
            let value = PropertyValue::Int(v);
            let bytes = value.serialize();

            assert_eq!(bytes[0], TAG_INT);
            assert_eq!(bytes.len(), 9);

            let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
            assert_eq!(deserialized, value);
            assert_eq!(consumed, 9);
        }
    }

    #[test]
    fn test_serialize_float() {
        let test_values = [0.0f64, 1.0, -1.0, f64::MAX, f64::MIN, 1.5, -2.5];
        for &v in &test_values {
            let value = PropertyValue::Float(v);
            let bytes = value.serialize();

            assert_eq!(bytes[0], TAG_FLOAT);
            assert_eq!(bytes.len(), 9);

            let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
            assert_eq!(deserialized, value);
            assert_eq!(consumed, 9);
        }
    }

    #[test]
    fn test_serialize_float_special_values() {
        // Test infinity
        let value = PropertyValue::Float(f64::INFINITY);
        let bytes = value.serialize();
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized.as_float(), Some(f64::INFINITY));

        // Test negative infinity
        let value = PropertyValue::Float(f64::NEG_INFINITY);
        let bytes = value.serialize();
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized.as_float(), Some(f64::NEG_INFINITY));

        // Test NaN - special case, NaN != NaN
        let value = PropertyValue::Float(f64::NAN);
        let bytes = value.serialize();
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert!(deserialized.as_float().unwrap().is_nan());
    }

    #[test]
    fn test_serialize_string() {
        let test_values = ["", "hello", "world", "hello world!", "こんにちは", "🎉"];
        for s in test_values {
            let value = PropertyValue::string(s);
            let bytes = value.serialize();

            assert_eq!(bytes[0], TAG_STRING);

            let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
            assert_eq!(deserialized, value);
            assert_eq!(consumed, bytes.len());
        }
    }

    #[test]
    fn test_serialize_bytes() {
        let test_values: &[&[u8]] = &[&[], &[1], &[1, 2, 3], &[0, 255, 128]];
        for &b in test_values {
            let value = PropertyValue::bytes(b);
            let bytes = value.serialize();

            assert_eq!(bytes[0], TAG_BYTES);

            let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
            assert_eq!(deserialized, value);
            assert_eq!(consumed, bytes.len());
        }
    }

    #[test]
    fn test_serialize_array() {
        // Empty array
        let value = PropertyValue::array(vec![]);
        let bytes = value.serialize();
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);

        // Array with mixed types
        let value = PropertyValue::array(vec![
            PropertyValue::Int(42),
            PropertyValue::string("hello"),
            PropertyValue::Bool(true),
            PropertyValue::Float(1.5),
        ]);
        let bytes = value.serialize();
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);

        // Nested array
        let inner = PropertyValue::array(vec![PropertyValue::Int(1), PropertyValue::Int(2)]);
        let value = PropertyValue::array(vec![inner, PropertyValue::Int(3)]);
        let bytes = value.serialize();
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);
    }

    #[test]
    fn test_serialize_vector_basic() {
        let data = [1.0f32, 2.0, 3.0];
        let value = PropertyValue::vector(data);
        let bytes = value.serialize();

        // Check format: tag (1) + dimension (4) + 3*4 bytes
        assert_eq!(bytes[0], TAG_VECTOR);
        assert_eq!(bytes.len(), 1 + 4 + 3 * 4);

        let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn test_serialize_vector_round_trip() {
        let data: Vec<f32> = (0..100).map(|i| i as f32 * 0.01).collect();
        let bytes = serialize_vector(&data);

        let (deserialized, consumed) = deserialize_vector(&bytes).unwrap();
        assert_eq!(deserialized.as_ref(), &data[..]);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn test_serialize_vector_empty() {
        let empty: Vec<f32> = vec![];
        let bytes = serialize_vector(&empty);

        // Should be tag (1) + dimension (4) = 5 bytes
        assert_eq!(bytes.len(), 5);
        assert_eq!(bytes[0], TAG_VECTOR);

        let (deserialized, consumed) = deserialize_vector(&bytes).unwrap();
        assert!(deserialized.is_empty());
        assert_eq!(consumed, 5);
    }

    #[test]
    fn test_serialize_vector_large() {
        // Test with typical embedding size (1536 dimensions like OpenAI ada-002)
        let large_vector: Vec<f32> = (0..1536).map(|i| (i as f32) / 1536.0).collect();
        let bytes = serialize_vector(&large_vector);

        // Expected size: tag (1) + dimension (4) + 1536*4 = 6149 bytes
        assert_eq!(bytes.len(), 1 + 4 + 1536 * 4);

        let (deserialized, consumed) = deserialize_vector(&bytes).unwrap();
        assert_eq!(deserialized.len(), 1536);
        assert_eq!(consumed, bytes.len());

        // Verify values
        for (i, &val) in deserialized.iter().enumerate() {
            assert!((val - (i as f32) / 1536.0).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn test_serialize_vector_special_values() {
        let data = [f32::INFINITY, f32::NEG_INFINITY, 0.0, -0.0, f32::NAN];
        let bytes = serialize_vector(&data);
        let (deserialized, _) = deserialize_vector(&bytes).unwrap();

        assert_eq!(deserialized[0], f32::INFINITY);
        assert_eq!(deserialized[1], f32::NEG_INFINITY);
        assert_eq!(deserialized[2], 0.0);
        assert_eq!(deserialized[3], 0.0); // -0.0 compares equal to 0.0
        assert!(deserialized[4].is_nan());
    }

    #[test]
    fn test_deserialize_vector_errors() {
        // Empty buffer
        let result = deserialize_vector(&[]);
        assert!(result.is_err());

        // Buffer too short for header
        let result = deserialize_vector(&[TAG_VECTOR, 1, 0, 0]);
        assert!(result.is_err());

        // Wrong type tag
        let result = deserialize_vector(&[TAG_INT, 3, 0, 0, 0]);
        assert!(result.is_err());

        // Buffer too short for data
        let mut bytes = vec![TAG_VECTOR, 3, 0, 0, 0]; // Dimension = 3
        bytes.extend_from_slice(&[1.0f32.to_le_bytes()[0]]); // Only 1 byte of data
        let result = deserialize_vector(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_vector_serialization_optimization_correctness() {
        // Test that optimized serialization produces correct byte-identical output
        // This validates issue #203 optimization for little-endian bulk copy

        // Test various vector sizes
        let test_cases = vec![
            vec![],                                           // Empty
            vec![1.0f32],                                     // Single element
            vec![1.0f32, 2.0, 3.0],                           // Small vector
            (0..100).map(|i| i as f32 * 0.01).collect(),      // Medium vector
            (0..384).map(|i| (i as f32) / 384.0).collect(),   // Typical embedding (384)
            (0..1536).map(|i| (i as f32) / 1536.0).collect(), // Large embedding (1536)
        ];

        for test_vector in test_cases {
            let bytes = serialize_vector(&test_vector);

            // Validate header
            assert_eq!(bytes[0], TAG_VECTOR);
            let dimension = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
            assert_eq!(dimension, test_vector.len());
            assert_eq!(bytes.len(), 1 + 4 + test_vector.len() * 4);

            // Validate deserialization produces exact same values
            let (deserialized, consumed) = deserialize_vector(&bytes).unwrap();
            assert_eq!(deserialized.len(), test_vector.len());
            assert_eq!(consumed, bytes.len());

            // Compare each element
            for (i, (&original, &recovered)) in
                test_vector.iter().zip(deserialized.iter()).enumerate()
            {
                assert_eq!(
                    original,
                    recovered,
                    "Mismatch at index {} for vector of length {}",
                    i,
                    test_vector.len()
                );
            }
        }
    }

    #[test]
    fn test_vector_serialization_special_values_optimization() {
        // Test that special f32 values are correctly serialized with optimization
        let special_values = vec![
            f32::INFINITY,
            f32::NEG_INFINITY,
            0.0,
            -0.0,
            f32::MAX,
            f32::MIN,
            f32::MIN_POSITIVE,
            1.0,
            -1.0,
            std::f32::consts::PI,
            f32::NAN,
        ];

        let bytes = serialize_vector(&special_values);
        let (deserialized, _) = deserialize_vector(&bytes).unwrap();

        assert_eq!(deserialized[0], f32::INFINITY);
        assert_eq!(deserialized[1], f32::NEG_INFINITY);
        assert_eq!(deserialized[2], 0.0);
        assert_eq!(deserialized[3], 0.0); // -0.0 compares equal to 0.0
        assert_eq!(deserialized[4], f32::MAX);
        assert_eq!(deserialized[5], f32::MIN);
        assert_eq!(deserialized[6], f32::MIN_POSITIVE);
        assert_eq!(deserialized[7], 1.0);
        assert_eq!(deserialized[8], -1.0);
        assert!((deserialized[9] - std::f32::consts::PI).abs() < f32::EPSILON);
        assert!(deserialized[10].is_nan());
    }

    #[test]
    fn test_vector_serialization_deterministic() {
        // Ensure serialization is deterministic (same input always produces same output)
        let vector: Vec<f32> = (0..100).map(|i| i as f32 * 0.1).collect();

        let bytes1 = serialize_vector(&vector);
        let bytes2 = serialize_vector(&vector);
        let bytes3 = serialize_vector(&vector);

        assert_eq!(bytes1, bytes2);
        assert_eq!(bytes2, bytes3);

        // Also test deserialization is deterministic
        let (v1, _) = deserialize_vector(&bytes1).unwrap();
        let (v2, _) = deserialize_vector(&bytes2).unwrap();
        let (v3, _) = deserialize_vector(&bytes3).unwrap();

        assert_eq!(v1.as_ref(), v2.as_ref());
        assert_eq!(v2.as_ref(), v3.as_ref());
    }

    #[test]
    fn test_vector_deserialization_unaligned() {
        // Test deserialization from an unaligned offset to ensure
        // copy_nonoverlapping handles potentially unaligned data correctly
        let vector: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let serialized = serialize_vector(&vector);

        // Create a buffer with padding to force unaligned read
        let mut padded_buffer = vec![0xFF]; // 1 byte padding
        padded_buffer.extend_from_slice(&serialized);

        // Deserialize from offset 1 (unaligned)
        let (deserialized, consumed) = deserialize_vector(&padded_buffer[1..]).unwrap();

        assert_eq!(deserialized.len(), vector.len());
        assert_eq!(consumed, serialized.len());
        for (original, recovered) in vector.iter().zip(deserialized.iter()) {
            assert_eq!(original, recovered);
        }

        // Also test with 3-byte padding (different alignment)
        let mut padded_buffer3 = vec![0xFF, 0xFF, 0xFF];
        padded_buffer3.extend_from_slice(&serialized);

        let (deserialized3, consumed3) = deserialize_vector(&padded_buffer3[3..]).unwrap();
        assert_eq!(deserialized3.len(), vector.len());
        assert_eq!(consumed3, serialized.len());
        for (original, recovered) in vector.iter().zip(deserialized3.iter()) {
            assert_eq!(original, recovered);
        }
    }

    #[test]
    fn test_deserialize_property_value_errors() {
        // Empty buffer
        let result = PropertyValue::deserialize(&[]);
        assert!(result.is_err());

        // Unknown type tag
        let result = PropertyValue::deserialize(&[255]);
        assert!(result.is_err());

        // Truncated Int
        let result = PropertyValue::deserialize(&[TAG_INT, 1, 2, 3]);
        assert!(result.is_err());

        // Truncated String (length says 100, but no data)
        let result = PropertyValue::deserialize(&[TAG_STRING, 100, 0, 0, 0]);
        assert!(result.is_err());
    }

    #[test]
    fn test_serialize_property_map_empty() {
        let map = PropertyMap::new();
        let bytes = map.serialize().expect("Serialization should succeed");

        // Empty map: just count (4 bytes) = [0, 0, 0, 0]
        assert_eq!(bytes, vec![0, 0, 0, 0]);

        let (deserialized, consumed) = PropertyMap::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, map);
        assert_eq!(consumed, 4);
    }

    #[test]
    fn test_serialize_property_map_round_trip() {
        let map = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("active", true)
            .insert("score", 98.5)
            .build();

        let bytes = map.serialize().expect("Serialization should succeed");
        let (deserialized, _) = PropertyMap::deserialize(&bytes).unwrap();

        // Check all values match
        assert_eq!(deserialized.len(), 4);
        assert_eq!(
            deserialized.get("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
        assert_eq!(deserialized.get("age").and_then(|v| v.as_int()), Some(30));
        assert_eq!(
            deserialized.get("active").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            deserialized.get("score").and_then(|v| v.as_float()),
            Some(98.5)
        );
    }

    #[test]
    fn test_serialize_property_map_with_vector() {
        let embedding = vec![0.1f32, 0.2, 0.3, 0.4];
        let map = PropertyMapBuilder::new()
            .insert("name", "node1")
            .insert("embedding", PropertyValue::vector(&embedding))
            .build();

        let bytes = map.serialize().expect("Serialization should succeed");
        let (deserialized, _) = PropertyMap::deserialize(&bytes).unwrap();

        assert_eq!(deserialized.len(), 2);
        assert_eq!(
            deserialized.get("name").and_then(|v| v.as_str()),
            Some("node1")
        );
        assert_eq!(
            deserialized.get("embedding").and_then(|v| v.as_vector()),
            Some(&embedding[..])
        );
    }

    #[test]
    fn test_serialize_property_map_with_nested_array() {
        let map = PropertyMapBuilder::new()
            .insert(
                "tags",
                PropertyValue::array(vec![
                    PropertyValue::string("rust"),
                    PropertyValue::string("database"),
                ]),
            )
            .build();

        let bytes = map.serialize().expect("Serialization should succeed");
        let (deserialized, _) = PropertyMap::deserialize(&bytes).unwrap();

        let tags = deserialized.get("tags").and_then(|v| v.as_array()).unwrap();
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].as_str(), Some("rust"));
        assert_eq!(tags[1].as_str(), Some("database"));
    }

    #[test]
    fn test_property_map_deserialize_errors() {
        // Empty buffer
        let result = PropertyMap::deserialize(&[]);
        assert!(result.is_err());

        // Truncated count
        let result = PropertyMap::deserialize(&[1, 2]);
        assert!(result.is_err());

        // Says 1 key-value but no data
        let result = PropertyMap::deserialize(&[1, 0, 0, 0]);
        assert!(result.is_err());
    }

    #[test]
    fn test_serialize_into_efficiency() {
        // Test that serialize_into appends to existing buffer correctly
        let mut buffer = vec![0xAA, 0xBB]; // Some existing data
        let value = PropertyValue::Int(42);
        value.serialize_into(&mut buffer);

        assert_eq!(buffer[0], 0xAA);
        assert_eq!(buffer[1], 0xBB);
        assert_eq!(buffer[2], TAG_INT);
        // The rest should be 42i64 in little-endian
    }

    #[test]
    fn test_all_property_types_round_trip() {
        // Comprehensive test of all PropertyValue types
        let values = vec![
            PropertyValue::Null,
            PropertyValue::Bool(true),
            PropertyValue::Bool(false),
            PropertyValue::Int(0),
            PropertyValue::Int(i64::MAX),
            PropertyValue::Int(i64::MIN),
            PropertyValue::Float(0.0),
            PropertyValue::Float(f64::MAX),
            PropertyValue::String(Arc::from("hello")),
            PropertyValue::String(Arc::from("")),
            PropertyValue::Bytes(Arc::from([1u8, 2, 3].as_slice())),
            PropertyValue::Bytes(Arc::from([].as_slice())),
            PropertyValue::Array(Arc::new(vec![
                PropertyValue::Int(1),
                PropertyValue::string("two"),
            ])),
            PropertyValue::Array(Arc::new(vec![])),
            PropertyValue::Vector(Arc::from([1.0f32, 2.0, 3.0].as_slice())),
            PropertyValue::Vector(Arc::from([].as_slice())),
        ];

        for value in values {
            let bytes = value.serialize();
            let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
            assert_eq!(
                consumed,
                bytes.len(),
                "Consumed bytes should match serialized length for {:?}",
                value.type_name()
            );

            // Special handling for NaN values
            if let PropertyValue::Float(f) = &value
                && f.is_nan()
            {
                assert!(deserialized.as_float().unwrap().is_nan());
                continue;
            }
            assert_eq!(
                deserialized,
                value,
                "Round-trip failed for {:?}",
                value.type_name()
            );
        }
    }

    #[test]
    fn test_endianness() {
        // Verify little-endian serialization
        let value = PropertyValue::Int(0x0102030405060708i64);
        let bytes = value.serialize();

        // Little-endian: least significant byte first
        assert_eq!(bytes[0], TAG_INT);
        assert_eq!(bytes[1], 0x08);
        assert_eq!(bytes[2], 0x07);
        assert_eq!(bytes[3], 0x06);
        assert_eq!(bytes[4], 0x05);
        assert_eq!(bytes[5], 0x04);
        assert_eq!(bytes[6], 0x03);
        assert_eq!(bytes[7], 0x02);
        assert_eq!(bytes[8], 0x01);
    }

    // ========================================================================
    // PropertyKey Interning Tests (Issue #16)
    // ========================================================================

    #[test]
    fn test_property_key_interning_serialization_round_trip() {
        // Create a property map with interned keys
        let map = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("active", true)
            .build();

        // Serialize
        let bytes = map.serialize().expect("Serialization should succeed");

        // Deserialize
        let (deserialized, _) =
            PropertyMap::deserialize(&bytes).expect("Deserialization should succeed");

        // Verify all values match
        assert_eq!(
            deserialized.get("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
        assert_eq!(deserialized.get("age").and_then(|v| v.as_int()), Some(30));
        assert_eq!(
            deserialized.get("active").and_then(|v| v.as_bool()),
            Some(true)
        );

        // Verify keys are interned (same ID as original)
        let original_keys: Vec<_> = map.keys().cloned().collect();
        let deserialized_keys: Vec<_> = deserialized.keys().cloned().collect();

        assert_eq!(original_keys.len(), deserialized_keys.len());
        for key in &original_keys {
            assert!(
                deserialized_keys.contains(key),
                "Key should be interned with same ID"
            );
        }
    }

    #[test]
    fn test_property_key_memory_efficiency() {
        use std::mem::size_of;

        // Verify InternedString is indeed smaller than String
        assert_eq!(
            size_of::<InternedString>(),
            4,
            "InternedString should be 4 bytes"
        );
        assert_eq!(size_of::<String>(), 24, "String should be 24 bytes");

        // Create multiple maps with the same keys
        let maps: Vec<_> = (0..100)
            .map(|i| {
                PropertyMapBuilder::new()
                    .insert("test_mem_name", format!("Person{}", i))
                    .insert("test_mem_age", i as i64)
                    .insert("test_mem_id", i as i64)
                    .build()
            })
            .collect();

        // Verify all maps use the same interned key IDs (order may vary)
        use std::collections::HashSet;
        let first_keys: HashSet<_> = maps[0].keys().cloned().collect();
        for map in &maps[1..] {
            let map_keys: HashSet<_> = map.keys().cloned().collect();
            assert_eq!(
                first_keys, map_keys,
                "All maps should share the same interned key IDs"
            );
        }

        // Verify we have exactly 3 unique keys across all maps
        assert_eq!(first_keys.len(), 3, "Should have exactly 3 unique keys");

        // Verify the specific keys exist and can be resolved
        let expected_keys = ["test_mem_name", "test_mem_age", "test_mem_id"];
        for key_str in &expected_keys {
            let exists = first_keys.iter().any(|key| {
                GLOBAL_INTERNER
                    .resolve(*key)
                    .map(|s| s.as_ref() == *key_str)
                    .unwrap_or(false)
            });
            assert!(
                exists,
                "Key '{}' should exist in the property maps",
                key_str
            );
        }
    }

    #[test]
    fn test_invalid_interned_string_serialization() {
        // Create an InternedString with a raw ID that doesn't exist in the interner
        let invalid_key = InternedString::from_raw(999999);

        // Create a property map and manually insert with invalid key
        let mut inner_map = HashMap::new();
        inner_map.insert(invalid_key, PropertyValue::Int(42));
        let map = PropertyMap {
            inner: Arc::new(inner_map),
        };

        // Serialization should return an error, not panic
        let result = map.serialize();
        assert!(
            result.is_err(),
            "Serialization should fail for invalid InternedString"
        );

        match result {
            Err(crate::utils::error::Error::Storage(StorageError::InconsistentState {
                reason,
            })) => {
                assert!(
                    reason.contains("not found in interner"),
                    "Error message should indicate missing key in interner"
                );
            }
            _ => panic!("Expected StorageError::InconsistentState"),
        }
    }

    #[test]
    fn test_serialize_with_invalid_key() {
        // This explicitly verifies the error path when the interner is missing a key
        // Uses the same logic as test_invalid_interned_string_serialization but
        // ensures the new `ok_or_else` path is hit correctly.

        let invalid_key = InternedString::from_raw(888888);
        let mut inner_map = HashMap::new();
        inner_map.insert(invalid_key, PropertyValue::Bool(true));
        let map = PropertyMap {
            inner: Arc::new(inner_map),
        };

        let mut buffer = Vec::new();
        let result = map.serialize_into(&mut buffer);

        assert!(result.is_err(), "Should return error for missing key");
        match result {
            Err(crate::utils::error::Error::Storage(StorageError::InconsistentState {
                reason,
            })) => {
                assert!(
                    reason.contains("888888"),
                    "Error message should contain the invalid key ID"
                );
            }
            _ => panic!("Expected InconsistentState error"),
        }
    }

    #[test]
    fn test_concurrent_property_key_access() {
        use std::sync::Arc;
        use std::thread;

        // Create a property map
        let map = Arc::new(
            PropertyMapBuilder::new()
                .insert("shared_key", "value")
                .insert("count", 0i64)
                .build(),
        );

        // Spawn multiple threads accessing the same property keys
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let map_clone = Arc::clone(&map);
                thread::spawn(move || {
                    // Each thread accesses properties multiple times
                    for _ in 0..100 {
                        assert_eq!(
                            map_clone.get("shared_key").and_then(|v| v.as_str()),
                            Some("value")
                        );
                        assert!(map_clone.contains_key("count"));
                    }
                })
            })
            .collect();

        // Wait for all threads to complete
        for handle in handles {
            handle.join().expect("Thread should complete successfully");
        }
    }

    #[test]
    fn test_property_key_get_efficiency() {
        // Pre-populate interner with a key
        let interned_key = GLOBAL_INTERNER.intern("test_key").unwrap();

        let map = PropertyMapBuilder::new()
            .insert("test_key", "value")
            .build();

        // get() should auto-intern the key
        let value1 = map.get("test_key");
        assert_eq!(value1.and_then(|v| v.as_str()), Some("value"));

        // get_by_interned_key() should be more efficient for repeated lookups
        let value2 = map.get_by_interned_key(&interned_key);
        assert_eq!(value2.and_then(|v| v.as_str()), Some("value"));

        // Both methods should return the same result
        assert_eq!(value1, value2);
    }

    // ========================================================================
    // SparseVector PropertyValue Tests
    // ========================================================================

    #[test]
    fn test_sparse_vector_property_value_creation() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![0, 2, 5], vec![1.0, 2.0, 3.0], 10).unwrap();
        let prop = PropertyValue::sparse_vector(sparse);

        assert_eq!(prop.type_name(), "sparse_vector");
        assert!(prop.as_sparse_vector().is_some());
        assert!(prop.as_vector().is_none()); // Should not match dense vector
    }

    #[test]
    fn test_sparse_vector_property_value_accessors() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![1, 4], vec![1.5, 2.5], 6).unwrap();
        let prop = PropertyValue::sparse_vector(sparse);

        let retrieved = prop.as_sparse_vector().unwrap();
        assert_eq!(retrieved.nnz(), 2);
        assert_eq!(retrieved.dimension(), 6);
        assert_eq!(retrieved.indices(), &[1, 4]);
        assert_eq!(retrieved.values(), &[1.5, 2.5]);
    }

    #[test]
    fn test_sparse_vector_from_conversion() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![0, 3], vec![1.0, 2.0], 5).unwrap();
        let prop: PropertyValue = sparse.into();

        assert_eq!(prop.type_name(), "sparse_vector");
        assert!(prop.as_sparse_vector().is_some());
    }

    #[test]
    fn test_sparse_vector_display() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 10).unwrap();
        let prop = PropertyValue::sparse_vector(sparse);

        let display = format!("{}", prop);
        assert_eq!(display, "<sparse_vector[dim=10, nnz=2]>");
    }

    #[test]
    fn test_sparse_vector_arc_sharing() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![0, 50, 100], vec![1.0, 2.0, 3.0], 1000).unwrap();
        let prop1 = PropertyValue::sparse_vector(sparse);
        let prop2 = prop1.clone();

        // Both should point to the same Arc (cheap clone)
        if let (PropertyValue::SparseVector(sv1), PropertyValue::SparseVector(sv2)) =
            (&prop1, &prop2)
        {
            assert!(
                Arc::ptr_eq(sv1, sv2),
                "SparseVector Arc should be shared after clone"
            );
        } else {
            panic!("Expected SparseVector variants");
        }

        assert_eq!(prop1, prop2);
    }

    #[test]
    fn test_sparse_vector_in_property_map() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![10, 42, 100], vec![2.5, 1.8, 3.2], 1000).unwrap();
        let map = PropertyMapBuilder::new()
            .insert("name", "document1")
            .insert("sparse_embedding", PropertyValue::sparse_vector(sparse))
            .build();

        assert_eq!(map.len(), 2);
        let retrieved_sparse = map
            .get("sparse_embedding")
            .and_then(|v| v.as_sparse_vector());
        assert!(retrieved_sparse.is_some());
        assert_eq!(retrieved_sparse.unwrap().nnz(), 3);
        assert_eq!(retrieved_sparse.unwrap().dimension(), 1000);
    }

    #[test]
    fn test_property_map_contains_vector_with_sparse() {
        use crate::core::vector::SparseVec;

        // Map with sparse vector
        let sparse = SparseVec::new(vec![0], vec![1.0], 10).unwrap();
        let map = PropertyMapBuilder::new()
            .insert("sparse", PropertyValue::sparse_vector(sparse))
            .build();
        assert!(map.contains_vector());

        // Map with dense vector
        let map = PropertyMapBuilder::new()
            .insert("dense", PropertyValue::vector([1.0f32, 2.0, 3.0]))
            .build();
        assert!(map.contains_vector());

        // Map without vectors
        let map = PropertyMapBuilder::new()
            .insert("name", "test")
            .insert("count", 42i64)
            .build();
        assert!(!map.contains_vector());
    }

    // ========================================================================
    // SparseVector Serialization Tests
    // ========================================================================

    #[test]
    fn test_serialize_sparse_vector_basic() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![0, 2, 4], vec![1.0, 2.0, 3.0], 5).unwrap();
        let prop = PropertyValue::sparse_vector(sparse);
        let bytes = prop.serialize();

        assert_eq!(bytes[0], TAG_SPARSE_VECTOR);

        let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());

        match deserialized {
            PropertyValue::SparseVector(sv) => {
                assert_eq!(sv.nnz(), 3);
                assert_eq!(sv.dimension(), 5);
                assert_eq!(sv.indices(), &[0, 2, 4]);
                assert_eq!(sv.values(), &[1.0, 2.0, 3.0]);
            }
            _ => panic!("Expected SparseVector"),
        }
    }

    #[test]
    fn test_serialize_sparse_vector_empty() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![], vec![], 100).unwrap();
        let bytes = serialize_sparse_vector(&sparse);

        // Should be tag (1) + dimension (4) + nnz (4) = 9 bytes
        assert_eq!(bytes.len(), 9);
        assert_eq!(bytes[0], TAG_SPARSE_VECTOR);

        let (deserialized, consumed) = deserialize_sparse_vector(&bytes).unwrap();
        assert!(deserialized.indices().is_empty());
        assert_eq!(deserialized.dimension(), 100);
        assert_eq!(consumed, 9);
    }

    #[test]
    fn test_serialize_sparse_vector_round_trip() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(
            vec![1, 10, 42, 99, 256],
            vec![1.5, 2.3, 0.8, 4.2, 1.1],
            1000,
        )
        .unwrap();

        let bytes = serialize_sparse_vector(&sparse);
        let (deserialized, consumed) = deserialize_sparse_vector(&bytes).unwrap();

        assert_eq!(consumed, bytes.len());
        assert_eq!(deserialized.nnz(), sparse.nnz());
        assert_eq!(deserialized.dimension(), sparse.dimension());
        assert_eq!(deserialized.indices(), sparse.indices());
        assert_eq!(deserialized.values(), sparse.values());
    }

    #[test]
    fn test_serialize_sparse_vector_in_property_map() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![0, 5, 10], vec![1.0, 2.0, 3.0], 20).unwrap();
        let map = PropertyMapBuilder::new()
            .insert("id", 123i64)
            .insert("sparse_vec", PropertyValue::sparse_vector(sparse))
            .build();

        let bytes = map.serialize().expect("Serialization should succeed");
        let (deserialized, _) = PropertyMap::deserialize(&bytes).unwrap();

        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized.get("id").and_then(|v| v.as_int()), Some(123));

        let sparse_result = deserialized
            .get("sparse_vec")
            .and_then(|v| v.as_sparse_vector());
        assert!(sparse_result.is_some());
        let sv = sparse_result.unwrap();
        assert_eq!(sv.nnz(), 3);
        assert_eq!(sv.dimension(), 20);
    }

    #[test]
    fn test_deserialize_sparse_vector_errors() {
        // Empty buffer
        let result = deserialize_sparse_vector(&[]);
        assert!(result.is_err());

        // Buffer too short for header
        let result = deserialize_sparse_vector(&[TAG_SPARSE_VECTOR, 1, 0, 0]);
        assert!(result.is_err());

        // Wrong type tag
        let result = deserialize_sparse_vector(&[TAG_INT, 5, 0, 0, 0, 2, 0, 0, 0]);
        assert!(result.is_err());

        // nnz > dimension
        let mut bytes = vec![TAG_SPARSE_VECTOR];
        bytes.extend_from_slice(&5u32.to_le_bytes()); // dimension = 5
        bytes.extend_from_slice(&10u32.to_le_bytes()); // nnz = 10 (invalid!)
        let result = deserialize_sparse_vector(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_serialize_sparse_vector_bm25_like() {
        use crate::core::vector::SparseVec;

        // Simulate BM25 sparse vector for a document
        let sparse = SparseVec::new(
            vec![42, 157, 891, 1023, 5000],
            vec![2.3, 1.8, 0.9, 3.1, 1.5],
            10000, // Large vocabulary
        )
        .unwrap();

        let bytes = serialize_sparse_vector(&sparse);

        // Calculate expected size:
        // tag (1) + dimension (4) + nnz (4) + 5 indices (20) + 5 values (20) = 49 bytes
        assert_eq!(bytes.len(), 49);

        // Verify it can be deserialized
        let (deserialized, _) = deserialize_sparse_vector(&bytes).unwrap();
        assert_eq!(deserialized.nnz(), 5);
        assert_eq!(deserialized.dimension(), 10000);
    }

    #[test]
    fn test_sparse_vector_type_mismatch() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![0, 1], vec![1.0, 2.0], 5).unwrap();
        let prop = PropertyValue::sparse_vector(sparse);

        // Sparse vector should not match other types
        assert!(prop.as_int().is_none());
        assert!(prop.as_str().is_none());
        assert!(prop.as_vector().is_none()); // Should not match dense vector
        assert!(prop.as_array().is_none());
    }

    // ========== Issue #188: Zero-copy vector access tests ==========

    #[test]
    fn test_as_arc_vector_returns_arc() {
        // Test that as_arc_vector returns a cloned Arc without copying the data
        let embedding: Vec<f32> = (0..384).map(|i| i as f32 * 0.001).collect();
        let prop = PropertyValue::vector(&embedding);

        // Get the Arc via as_arc_vector
        let arc = prop.as_arc_vector().expect("Should return Some for Vector");

        // The Arc should point to the same data
        assert_eq!(&*arc, &embedding[..]);
        assert_eq!(arc.len(), 384);
    }

    #[test]
    fn test_as_arc_vector_shares_data_with_original() {
        // Verify that as_arc_vector returns the same underlying Arc (not a copy)
        let embedding: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0];
        let prop = PropertyValue::vector(&embedding);

        // Get two Arcs - they should point to the same data
        let arc1 = prop.as_arc_vector().unwrap();
        let arc2 = prop.as_arc_vector().unwrap();

        // Both Arcs should point to the same allocation (same pointer)
        assert!(
            Arc::ptr_eq(&arc1, &arc2),
            "Multiple calls to as_arc_vector should return Arcs to the same data"
        );
    }

    #[test]
    fn test_as_arc_vector_returns_none_for_non_vector() {
        use crate::core::vector::SparseVec;

        // as_arc_vector should return None for non-vector types
        assert!(PropertyValue::Null.as_arc_vector().is_none());
        assert!(PropertyValue::Bool(true).as_arc_vector().is_none());
        assert!(PropertyValue::Int(42).as_arc_vector().is_none());
        assert!(PropertyValue::Float(2.5).as_arc_vector().is_none());
        assert!(PropertyValue::string("test").as_arc_vector().is_none());
        assert!(PropertyValue::bytes([1, 2, 3]).as_arc_vector().is_none());
        assert!(PropertyValue::array(vec![]).as_arc_vector().is_none());

        // SparseVector is a different type - should not match dense Vector
        let sparse = SparseVec::new(vec![0, 1], vec![1.0, 2.0], 5).unwrap();
        assert!(
            PropertyValue::sparse_vector(sparse)
                .as_arc_vector()
                .is_none()
        );
    }

    #[test]
    fn test_as_arc_vector_does_not_copy_data() {
        // Create a large vector to ensure we'd notice if data was copied
        let large_embedding: Vec<f32> = (0..4096).map(|i| i as f32).collect();
        let prop = PropertyValue::vector(&large_embedding);

        // Get the internal Arc pointer before as_arc_vector
        let internal_arc = if let PropertyValue::Vector(arc) = &prop {
            arc.clone()
        } else {
            panic!("Expected Vector variant");
        };

        // Get Arc via as_arc_vector
        let returned_arc = prop.as_arc_vector().unwrap();

        // They should point to the exact same allocation
        assert!(
            Arc::ptr_eq(&internal_arc, &returned_arc),
            "as_arc_vector should return the same Arc, not copy the data"
        );
    }

    // ========================================================================
    // Heap Size Estimation Tests
    // ========================================================================

    #[test]
    fn test_estimated_heap_size_primitives() {
        // Primitives should have zero heap size
        assert_eq!(PropertyValue::Null.estimated_heap_size(), 0);
        assert_eq!(PropertyValue::Bool(true).estimated_heap_size(), 0);
        assert_eq!(PropertyValue::Bool(false).estimated_heap_size(), 0);
        assert_eq!(PropertyValue::Int(42).estimated_heap_size(), 0);
        assert_eq!(PropertyValue::Int(i64::MAX).estimated_heap_size(), 0);
        assert_eq!(PropertyValue::Float(1.5).estimated_heap_size(), 0);
        assert_eq!(PropertyValue::Float(f64::MAX).estimated_heap_size(), 0);
    }

    #[test]
    fn test_estimated_heap_size_string() {
        // String heap size should equal string length
        let empty_string = PropertyValue::string("");
        assert_eq!(empty_string.estimated_heap_size(), 0);

        let hello = PropertyValue::string("hello");
        assert_eq!(hello.estimated_heap_size(), 5);

        let long_string = PropertyValue::string("hello world, this is a longer string");
        // 36 characters
        assert_eq!(long_string.estimated_heap_size(), 36);
    }

    #[test]
    fn test_estimated_heap_size_bytes() {
        // Bytes heap size should equal byte array length
        let empty_bytes = PropertyValue::bytes([]);
        assert_eq!(empty_bytes.estimated_heap_size(), 0);

        let some_bytes = PropertyValue::bytes([1, 2, 3, 4, 5]);
        assert_eq!(some_bytes.estimated_heap_size(), 5);

        let large_bytes: Vec<u8> = vec![0; 1000];
        let large = PropertyValue::bytes(large_bytes);
        assert_eq!(large.estimated_heap_size(), 1000);
    }

    #[test]
    fn test_estimated_heap_size_vector() {
        // Vector heap size should be len * sizeof(f32)
        let empty_vec = PropertyValue::vector::<[f32; 0]>([]);
        assert_eq!(empty_vec.estimated_heap_size(), 0);

        let small_vec = PropertyValue::vector([1.0f32, 2.0, 3.0, 4.0]);
        assert_eq!(
            small_vec.estimated_heap_size(),
            4 * std::mem::size_of::<f32>()
        );

        let embedding = PropertyValue::vector((0..384).map(|i| i as f32).collect::<Vec<_>>());
        assert_eq!(
            embedding.estimated_heap_size(),
            384 * std::mem::size_of::<f32>()
        );
    }

    #[test]
    fn test_estimated_heap_size_sparse_vector() {
        use crate::core::vector::SparseVec;

        // Sparse vector heap size: nnz * (sizeof(u32) + sizeof(f32)) + sizeof(usize)
        let sparse = SparseVec::new(vec![0, 10, 100], vec![1.0, 2.0, 3.0], 1000).unwrap();
        let prop = PropertyValue::sparse_vector(sparse);

        let expected = 3 * (std::mem::size_of::<u32>() + std::mem::size_of::<f32>())
            + std::mem::size_of::<usize>();
        assert_eq!(prop.estimated_heap_size(), expected);
    }

    #[test]
    fn test_estimated_heap_size_array() {
        // Empty array - should be 0 since no elements
        let empty_array = PropertyValue::array(vec![]);
        assert_eq!(empty_array.estimated_heap_size(), 0);

        // Array with primitives - includes Vec overhead but values have no heap size
        let primitive_array = PropertyValue::array(vec![
            PropertyValue::Int(1),
            PropertyValue::Int(2),
            PropertyValue::Int(3),
        ]);
        assert!(primitive_array.estimated_heap_size() > 0);

        // Array with strings - should include string lengths
        let string_array = PropertyValue::array(vec![
            PropertyValue::string("hello"),
            PropertyValue::string("world"),
        ]);
        // Should include at least the string lengths (5 + 5)
        assert!(string_array.estimated_heap_size() >= 10);
    }

    #[test]
    fn test_property_map_estimated_heap_size_empty() {
        let map = PropertyMap::new();
        // Empty map should have zero heap overhead
        let size = map.estimated_heap_size();
        assert_eq!(size, 0, "Empty map heap size should be zero");
    }

    #[test]
    fn test_property_map_estimated_heap_size_with_values() {
        let map = PropertyMapBuilder::new()
            .insert("name", "Alice") // string with 5 chars
            .insert("age", 30i64) // primitive, no heap
            .insert("active", true) // primitive, no heap
            .build();

        let size = map.estimated_heap_size();
        // Should include at least the string length plus HashMap overhead
        assert!(size >= 5, "Map with string should include string heap size");
    }

    #[test]
    fn test_property_map_estimated_heap_size_with_vector() {
        let embedding = vec![0.1f32; 384];
        let map = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&embedding))
            .build();

        let size = map.estimated_heap_size();
        // Should include vector heap size: 384 * 4 = 1536 bytes
        assert!(
            size >= 384 * std::mem::size_of::<f32>(),
            "Map with vector should include vector heap size"
        );
    }

    #[test]
    fn test_serialized_size_matches_actual() {
        let values = vec![
            PropertyValue::Null,
            PropertyValue::Bool(true),
            PropertyValue::Int(123),
            PropertyValue::Float(123.456),
            PropertyValue::string("test string"),
            PropertyValue::bytes([1, 2, 3]),
            PropertyValue::array(vec![PropertyValue::Int(1), PropertyValue::string("nested")]),
            PropertyValue::vector([1.0f32, 2.0, 3.0]),
        ];

        for value in values {
            let predicted = value.serialized_size();
            let actual = value.serialize().len();
            assert_eq!(
                predicted,
                actual,
                "Size mismatch for {:?}",
                value.type_name()
            );
        }
    }

    #[test]
    fn test_property_map_serialized_size() {
        let map = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30)
            .build();

        let predicted = map.serialized_size();
        let actual = map.serialize().unwrap().len();
        assert_eq!(predicted, actual);
    }
}
