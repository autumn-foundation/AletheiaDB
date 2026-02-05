//! Property value system.
//!
//! This module defines the `PropertyValue` enum and its serialization logic.

use std::fmt;
use std::sync::Arc;

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

/// Maximum recursion depth for nested properties (e.g., arrays of arrays).
/// Set to 100 to prevent stack overflow from malicious input.
pub const MAX_RECURSION_DEPTH: usize = 100;

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
                serialize_vector_into(v, buffer);
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
        //
        // Verified by Warden (2026-02-15): Input slice 'v' is valid &[f32]. size_of_val is correct. u8 alignment is 1.
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
        //
        // Verified by Warden (2026-02-15): Destination buffer is allocated via Vec::with_capacity(dimension), ensuring correct f32 alignment. Source buffer length is explicitly checked against capacity * 4.
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
    // Reserve space to avoid reallocations:
    // tag (1) + dimension (4) + nnz (4) + indices (nnz * 4) + values (nnz * 4)
    buffer.reserve(1 + 4 + 4 + sv.nnz() * 8);

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

    // Prevent DoS via memory exhaustion from malicious input
    if nnz > MAX_VECTOR_DIMENSIONS {
        return Err(StorageError::CorruptedData(format!(
            "Sparse vector nnz {} exceeds maximum allowed {}",
            nnz, MAX_VECTOR_DIMENSIONS
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

    #[cfg(target_endian = "little")]
    let indices = {
        // SAFETY: On little-endian platforms, we can directly copy the bytes
        // into a u32 vector using a single bulk memory operation.
        //
        // Safety argument:
        // 1. We validated that bytes.len() >= total_len, where total_len includes
        //    indices_len = nnz * 4. Thus indices_slice.len() == nnz * 4 exactly.
        // 2. We allocated Vec<u32> with capacity nnz. Its byte capacity is nnz * 4.
        // 3. src_ptr (from slice) and dst_ptr (from Vec) are valid for reads/writes of
        //    indices_slice.len() bytes.
        // 4. Alignment is handled because we copy to *mut u8, and the Vec's buffer
        //    is aligned for u32.
        // 5. u32 has no invalid bit patterns, so any byte sequence is valid.
        let mut indices = Vec::with_capacity(nnz);
        if nnz > 0 {
            unsafe {
                let src_ptr = indices_slice.as_ptr();
                let dst_ptr = indices.as_mut_ptr() as *mut u8;
                std::ptr::copy_nonoverlapping(src_ptr, dst_ptr, indices_slice.len());
                indices.set_len(nnz);
            }
        }
        indices
    };

    #[cfg(not(target_endian = "little"))]
    let indices = {
        let mut indices = Vec::with_capacity(nnz);
        for chunk in indices_slice.chunks_exact(4) {
            indices.push(u32::from_le_bytes(chunk.try_into().unwrap()));
        }
        indices
    };

    // Deserialize values
    let values_end = indices_end + values_len;
    let values_slice = &bytes[indices_end..values_end];

    #[cfg(target_endian = "little")]
    let values = {
        // SAFETY: On little-endian platforms, we can directly copy the bytes
        // into an f32 vector using a single bulk memory operation.
        //
        // Safety argument:
        // 1. validated that values_len = nnz * 4, and buffer has sufficient bytes.
        // 2. Vec<f32> capacity is nnz, so byte capacity is nnz * 4.
        // 3. Pointers are valid for the copy length.
        // 4. f32 has no invalid bit patterns (NaNs are allowed).
        let mut values = Vec::with_capacity(nnz);
        if nnz > 0 {
            unsafe {
                let src_ptr = values_slice.as_ptr();
                let dst_ptr = values.as_mut_ptr() as *mut u8;
                std::ptr::copy_nonoverlapping(src_ptr, dst_ptr, values_slice.len());
                values.set_len(nnz);
            }
        }
        values
    };

    #[cfg(not(target_endian = "little"))]
    let values = {
        let mut values = Vec::with_capacity(nnz);
        for chunk in values_slice.chunks_exact(4) {
            values.push(f32::from_le_bytes(chunk.try_into().unwrap()));
        }
        values
    };

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
