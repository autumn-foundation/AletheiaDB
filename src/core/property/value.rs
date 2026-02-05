//! PropertyValue implementation.

use std::fmt;
use std::sync::Arc;

use crate::core::interning::InternedString;
use crate::core::property::constants::*;
use crate::core::property::vector_serde::{
    deserialize_sparse_vector, deserialize_vector, serialize_sparse_vector_into,
    try_serialize_vector_into,
};
use crate::core::vector::SparseVec;
use crate::utils::error::{Result, StorageError};

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
    fn test_estimated_heap_size_nested_array() {
        // Create a nested array: [[[[...]]]] (depth 10) containing a string at the bottom
        let mut value = PropertyValue::string("data");
        for _ in 0..10 {
            value = PropertyValue::array(vec![value]);
        }

        let size = value.estimated_heap_size();

        // Size should be:
        // 10 * (Vec capacity overhead + sizeof(PropertyValue)) + string length
        // Vec capacity is at least 1.
        let min_vec_size = std::mem::size_of::<PropertyValue>();
        let expected_min = 10 * min_vec_size + 4; // "data".len() = 4

        assert!(size >= expected_min);
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
            let predicted = value.serialized_size().expect("Size calculation failed");
            let actual = value.serialize().expect("Serialization failed").len();
            assert_eq!(
                predicted,
                actual,
                "Size mismatch for {:?}",
                value.type_name()
            );
        }
    }

    #[test]
    fn test_deserialize_recursion_limit() {
        // Construct a deeply nested array exceeding the recursion limit
        // Format: [TAG_ARRAY][count:1][TAG_ARRAY][count:1]...[TAG_NULL]
        let depth = MAX_RECURSION_DEPTH + 1;
        let mut bytes = Vec::new();

        for _ in 0..depth {
            bytes.push(TAG_ARRAY);
            bytes.extend_from_slice(&(1u32).to_le_bytes()); // Count = 1
        }

        // Terminate with a Null value
        bytes.push(TAG_NULL);

        // Try to deserialize
        let result = PropertyValue::deserialize(&bytes);

        // Should fail with recursion limit error
        assert!(result.is_err());
        match result {
            Err(crate::utils::error::Error::Storage(StorageError::CorruptedData(msg))) => {
                assert!(msg.contains("recursion depth limit exceeded"));
            }
            _ => panic!("Expected CorruptedData error for recursion limit"),
        }
    }

    #[test]
    fn test_deserialize_recursion_limit_boundary() {
        // Construct a deeply nested array exactly AT the limit (should succeed)
        let depth = MAX_RECURSION_DEPTH;
        let mut bytes = Vec::new();

        for _ in 0..depth {
            bytes.push(TAG_ARRAY);
            bytes.extend_from_slice(&(1u32).to_le_bytes()); // Count = 1
        }

        // Terminate with a Null value
        bytes.push(TAG_NULL);

        // Try to deserialize
        let result = PropertyValue::deserialize(&bytes);
        assert!(result.is_ok(), "Should succeed at recursion limit boundary");
    }

    #[test]
    fn test_deserialize_truncated_after_tag() {
        // Buffer containing only a tag but no data
        let bytes = vec![TAG_STRING]; // String expects length prefix
        let result = PropertyValue::deserialize(&bytes);
        assert!(result.is_err());
        match result {
            Err(crate::utils::error::Error::Storage(StorageError::CorruptedData(msg))) => {
                assert!(msg.contains("Buffer too short"));
            }
            _ => panic!("Expected CorruptedData error"),
        }
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;

    /// 🎯 Target: PropertyValue::estimated_heap_size
    /// 💣 Risk: Calculation failure should default to a "penalty" size to prevent cache monopolization by malicious inputs.
    /// 🧪 Strategy: Call estimated_heap_size on a deeply nested structure.
    /// 🔬 Verification: Verify result is 10MB (10 * 1024 * 1024).
    #[test]
    fn test_property_value_estimated_heap_size_penalty() {
        let mut value = PropertyValue::Int(42);
        for _ in 0..MAX_RECURSION_DEPTH + 1 {
            value = PropertyValue::Array(Arc::new(vec![value]));
        }

        // Should swallow the recursion error and return the penalty size
        let size = value.estimated_heap_size();

        // 10MB penalty
        assert_eq!(size, 10 * 1024 * 1024);
    }
}
