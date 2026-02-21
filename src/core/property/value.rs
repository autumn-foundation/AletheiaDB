//! Property value types and serialization.

use std::fmt;
use std::sync::Arc;

use crate::core::error::{Result, StorageError};
use crate::core::interning::InternedString;
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
                if a.len() != b.len() {
                    return false;
                }
                a.iter()
                    .zip(b.iter())
                    .all(|(x, y)| if x.is_nan() { y.is_nan() } else { x == y })
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

    pub(crate) fn serialize_recursive(&self, buffer: &mut Vec<u8>, depth: usize) -> Result<()> {
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
    pub(crate) fn deserialize_recursive(bytes: &[u8], depth: usize) -> Result<(Self, usize)> {
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

                let required_len = offset.checked_add(len).ok_or_else(|| {
                    StorageError::CorruptedData("String length overflow".to_string())
                })?;

                if bytes.len() < required_len {
                    return Err(StorageError::CorruptedData(format!(
                        "Buffer too short for String data: need {} bytes, have {}",
                        required_len,
                        bytes.len()
                    ))
                    .into());
                }

                let string_data = &bytes[offset..required_len];
                let s = std::str::from_utf8(string_data).map_err(|e| {
                    StorageError::CorruptedData(format!("Invalid UTF-8 in String: {}", e))
                })?;
                Ok((PropertyValue::String(Arc::from(s)), required_len))
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

                let required_len = offset.checked_add(len).ok_or_else(|| {
                    StorageError::CorruptedData("Bytes length overflow".to_string())
                })?;

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

    pub(crate) fn serialized_size_recursive(&self, depth: usize) -> Result<usize> {
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
        let size = 10_000;
        let mut original = Vec::with_capacity(size);
        for i in 0..size {
            original.push((i % 256) as u8);
        }

        // Convert to PropertyValue - this should consume the Vec (move semantics)
        let prop_value: PropertyValue = original.into();

        // Extract the Arc<[u8]> from the PropertyValue
        if let PropertyValue::Bytes(arc_bytes) = prop_value {
            assert_eq!(
                arc_bytes.len(),
                size,
                "PropertyValue should contain all elements"
            );

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
        let size = 1_000_000; // 1MB
        let large_vec: Vec<u8> = vec![0x42; size];

        let prop_value: PropertyValue = large_vec.into();

        if let PropertyValue::Bytes(arc_bytes) = &prop_value {
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
        let large_string = "x".repeat(1000);
        let prop1 = PropertyValue::string(&large_string);
        let prop2 = prop1.clone();

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

        let embedding_384 = vec![0.0f32; 384];
        let vec_prop = PropertyValue::vector(&embedding_384);
        assert_eq!(format!("{}", vec_prop), "<vector[384]>");

        let embedding_1536 = vec![0.0f32; 1536];
        let vec_prop = PropertyValue::vector(&embedding_1536);
        assert_eq!(format!("{}", vec_prop), "<vector[1536]>");
    }

    #[test]
    fn test_vector_arc_sharing() {
        let embedding: Vec<f32> = (0..384).map(|i| i as f32 * 0.001).collect();
        let prop1 = PropertyValue::vector(&embedding);
        let prop2 = prop1.clone();

        if let (PropertyValue::Vector(v1), PropertyValue::Vector(v2)) = (&prop1, &prop2) {
            assert!(
                Arc::ptr_eq(v1, v2),
                "Vector Arc should be shared after clone"
            );
        }

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
        let oversized: Vec<f32> = vec![0.0; MAX_VECTOR_DIMENSIONS + 1];
        let _ = PropertyValue::vector(oversized);
    }

    #[test]
    fn test_vector_max_dimensions_allowed() {
        let max_size: Vec<f32> = vec![0.0; MAX_VECTOR_DIMENSIONS];
        let vec_prop = PropertyValue::vector(max_size);
        assert_eq!(vec_prop.as_vector().unwrap().len(), MAX_VECTOR_DIMENSIONS);
    }

    #[test]
    fn test_vector_accessor_wrong_type() {
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

    // ========== Serialization Tests ==========

    #[test]
    fn test_serialize_null() {
        let value = PropertyValue::Null;
        let bytes = value.serialize().expect("Serialization failed");
        assert_eq!(bytes, vec![TAG_NULL]);

        let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn test_serialize_bool() {
        let value = PropertyValue::Bool(true);
        let bytes = value.serialize().expect("Serialization failed");
        assert_eq!(bytes, vec![TAG_BOOL, 1]);

        let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);
        assert_eq!(consumed, 2);

        let value = PropertyValue::Bool(false);
        let bytes = value.serialize().expect("Serialization failed");
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
            let bytes = value.serialize().expect("Serialization failed");

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
            let bytes = value.serialize().expect("Serialization failed");

            assert_eq!(bytes[0], TAG_FLOAT);
            assert_eq!(bytes.len(), 9);

            let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
            assert_eq!(deserialized, value);
            assert_eq!(consumed, 9);
        }
    }

    #[test]
    fn test_serialize_float_special_values() {
        let value = PropertyValue::Float(f64::INFINITY);
        let bytes = value.serialize().expect("Serialization failed");
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized.as_float(), Some(f64::INFINITY));

        let value = PropertyValue::Float(f64::NEG_INFINITY);
        let bytes = value.serialize().expect("Serialization failed");
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized.as_float(), Some(f64::NEG_INFINITY));

        let value = PropertyValue::Float(f64::NAN);
        let bytes = value.serialize().expect("Serialization failed");
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert!(deserialized.as_float().unwrap().is_nan());
    }

    #[test]
    fn test_serialize_string() {
        let test_values = ["", "hello", "world", "hello world!", "こんにちは", "🎉"];
        for s in test_values {
            let value = PropertyValue::string(s);
            let bytes = value.serialize().expect("Serialization failed");

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
            let bytes = value.serialize().expect("Serialization failed");

            assert_eq!(bytes[0], TAG_BYTES);

            let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
            assert_eq!(deserialized, value);
            assert_eq!(consumed, bytes.len());
        }
    }

    #[test]
    fn test_serialize_array() {
        let value = PropertyValue::array(vec![]);
        let bytes = value.serialize().expect("Serialization failed");
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);

        let value = PropertyValue::array(vec![
            PropertyValue::Int(42),
            PropertyValue::string("hello"),
            PropertyValue::Bool(true),
            PropertyValue::Float(1.5),
        ]);
        let bytes = value.serialize().expect("Serialization failed");
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);

        let inner = PropertyValue::array(vec![PropertyValue::Int(1), PropertyValue::Int(2)]);
        let value = PropertyValue::array(vec![inner, PropertyValue::Int(3)]);
        let bytes = value.serialize().expect("Serialization failed");
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);
    }

    #[test]
    fn test_serialize_vector_basic() {
        let data = [1.0f32, 2.0, 3.0];
        let value = PropertyValue::vector(data);
        let bytes = value.serialize().expect("Serialization failed");

        assert_eq!(bytes[0], TAG_VECTOR);
        assert_eq!(bytes.len(), 1 + 4 + 3 * 4);

        let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn test_deserialize_property_value_errors() {
        let result = PropertyValue::deserialize(&[]);
        assert!(result.is_err());

        let result = PropertyValue::deserialize(&[255]);
        assert!(result.is_err());

        let result = PropertyValue::deserialize(&[TAG_INT, 1, 2, 3]);
        assert!(result.is_err());

        let result = PropertyValue::deserialize(&[TAG_STRING, 100, 0, 0, 0]);
        assert!(result.is_err());
    }

    #[test]
    fn test_serialize_into_efficiency() {
        let mut buffer = vec![0xAA, 0xBB];
        let value = PropertyValue::Int(42);
        value.serialize_into(&mut buffer).unwrap();

        assert_eq!(buffer[0], 0xAA);
        assert_eq!(buffer[1], 0xBB);
        assert_eq!(buffer[2], TAG_INT);
    }

    #[test]
    fn test_all_property_types_round_trip() {
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
            let bytes = value.serialize().expect("Serialization failed");
            let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
            assert_eq!(
                consumed,
                bytes.len(),
                "Consumed bytes should match serialized length for {:?}",
                value.type_name()
            );

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
        let value = PropertyValue::Int(0x0102030405060708i64);
        let bytes = value.serialize().expect("Serialization failed");

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

    #[test]
    fn test_sparse_vector_property_value_creation() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![0, 2, 5], vec![1.0, 2.0, 3.0], 10).unwrap();
        let prop = PropertyValue::sparse_vector(sparse);

        assert_eq!(prop.type_name(), "sparse_vector");
        assert!(prop.as_sparse_vector().is_some());
        assert!(prop.as_vector().is_none());
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
    fn test_serialize_sparse_vector_basic() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![0, 2, 4], vec![1.0, 2.0, 3.0], 5).unwrap();
        let prop = PropertyValue::sparse_vector(sparse);
        let bytes = prop.serialize().expect("Serialization failed");

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
    fn test_sparse_vector_type_mismatch() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![0, 1], vec![1.0, 2.0], 5).unwrap();
        let prop = PropertyValue::sparse_vector(sparse);

        assert!(prop.as_int().is_none());
        assert!(prop.as_str().is_none());
        assert!(prop.as_vector().is_none());
        assert!(prop.as_array().is_none());
    }

    #[test]
    fn test_as_arc_vector_returns_arc() {
        let embedding: Vec<f32> = (0..384).map(|i| i as f32 * 0.001).collect();
        let prop = PropertyValue::vector(&embedding);

        let arc = prop.as_arc_vector().expect("Should return Some for Vector");

        assert_eq!(&*arc, &embedding[..]);
        assert_eq!(arc.len(), 384);
    }

    #[test]
    fn test_as_arc_vector_shares_data_with_original() {
        let embedding: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0];
        let prop = PropertyValue::vector(&embedding);

        let arc1 = prop.as_arc_vector().unwrap();
        let arc2 = prop.as_arc_vector().unwrap();

        assert!(
            Arc::ptr_eq(&arc1, &arc2),
            "Multiple calls to as_arc_vector should return Arcs to the same data"
        );
    }

    #[test]
    fn test_as_arc_vector_returns_none_for_non_vector() {
        use crate::core::vector::SparseVec;

        assert!(PropertyValue::Null.as_arc_vector().is_none());
        assert!(PropertyValue::Bool(true).as_arc_vector().is_none());
        assert!(PropertyValue::Int(42).as_arc_vector().is_none());
        assert!(PropertyValue::Float(2.5).as_arc_vector().is_none());
        assert!(PropertyValue::string("test").as_arc_vector().is_none());
        assert!(PropertyValue::bytes([1, 2, 3]).as_arc_vector().is_none());
        assert!(PropertyValue::array(vec![]).as_arc_vector().is_none());

        let sparse = SparseVec::new(vec![0, 1], vec![1.0, 2.0], 5).unwrap();
        assert!(
            PropertyValue::sparse_vector(sparse)
                .as_arc_vector()
                .is_none()
        );
    }

    #[test]
    fn test_as_arc_vector_does_not_copy_data() {
        let large_embedding: Vec<f32> = (0..4096).map(|i| i as f32).collect();
        let prop = PropertyValue::vector(&large_embedding);

        let internal_arc = if let PropertyValue::Vector(arc) = &prop {
            arc.clone()
        } else {
            panic!("Expected Vector variant");
        };

        let returned_arc = prop.as_arc_vector().unwrap();

        assert!(
            Arc::ptr_eq(&internal_arc, &returned_arc),
            "as_arc_vector should return the same Arc, not copy the data"
        );
    }

    #[test]
    fn test_estimated_heap_size_primitives() {
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
        let empty_string = PropertyValue::string("");
        assert_eq!(empty_string.estimated_heap_size(), 0);

        let hello = PropertyValue::string("hello");
        assert_eq!(hello.estimated_heap_size(), 5);

        let long_string = PropertyValue::string("hello world, this is a longer string");
        assert_eq!(long_string.estimated_heap_size(), 36);
    }

    #[test]
    fn test_estimated_heap_size_bytes() {
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

        let sparse = SparseVec::new(vec![0, 10, 100], vec![1.0, 2.0, 3.0], 1000).unwrap();
        let prop = PropertyValue::sparse_vector(sparse);

        let expected = 3 * (std::mem::size_of::<u32>() + std::mem::size_of::<f32>())
            + std::mem::size_of::<usize>();
        assert_eq!(prop.estimated_heap_size(), expected);
    }

    #[test]
    fn test_estimated_heap_size_array() {
        let empty_array = PropertyValue::array(vec![]);
        assert_eq!(empty_array.estimated_heap_size(), 0);

        let primitive_array = PropertyValue::array(vec![
            PropertyValue::Int(1),
            PropertyValue::Int(2),
            PropertyValue::Int(3),
        ]);
        assert!(primitive_array.estimated_heap_size() > 0);

        let string_array = PropertyValue::array(vec![
            PropertyValue::string("hello"),
            PropertyValue::string("world"),
        ]);
        assert!(string_array.estimated_heap_size() >= 10);
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
        let depth = MAX_RECURSION_DEPTH + 1;
        let mut bytes = Vec::new();

        for _ in 0..depth {
            bytes.push(TAG_ARRAY);
            bytes.extend_from_slice(&(1u32).to_le_bytes()); // Count = 1
        }

        bytes.push(TAG_NULL);

        let result = PropertyValue::deserialize(&bytes);

        assert!(result.is_err());
        match result {
            Err(crate::core::error::Error::Storage(StorageError::CorruptedData(msg))) => {
                assert!(msg.contains("recursion depth limit exceeded"));
            }
            _ => panic!("Expected CorruptedData error for recursion limit"),
        }
    }

    #[test]
    fn test_deserialize_recursion_limit_boundary() {
        let depth = MAX_RECURSION_DEPTH;
        let mut bytes = Vec::new();

        for _ in 0..depth {
            bytes.push(TAG_ARRAY);
            bytes.extend_from_slice(&(1u32).to_le_bytes()); // Count = 1
        }

        bytes.push(TAG_NULL);

        let result = PropertyValue::deserialize(&bytes);
        assert!(result.is_ok(), "Should succeed at recursion limit boundary");
    }

    #[test]
    fn test_deserialize_truncated_after_tag() {
        let bytes = vec![TAG_STRING];
        let result = PropertyValue::deserialize(&bytes);
        assert!(result.is_err());
        match result {
            Err(crate::core::error::Error::Storage(StorageError::CorruptedData(msg))) => {
                assert!(msg.contains("Buffer too short"));
            }
            _ => panic!("Expected CorruptedData error"),
        }
    }

    #[test]
    fn test_estimated_heap_size_nested_array() {
        let mut value = PropertyValue::string("data");
        for _ in 0..10 {
            value = PropertyValue::array(vec![value]);
        }

        let size = value.estimated_heap_size();

        let min_vec_size = std::mem::size_of::<PropertyValue>();
        let expected_min = 10 * min_vec_size + 4;

        assert!(size >= expected_min);
    }

    #[test]
    fn test_property_value_estimated_heap_size_penalty() {
        let mut value = PropertyValue::Int(42);
        for _ in 0..MAX_RECURSION_DEPTH + 1 {
            value = PropertyValue::Array(Arc::new(vec![value]));
        }

        let size = value.estimated_heap_size();

        assert_eq!(size, 10 * 1024 * 1024);
    }

    #[test]
    fn test_semantically_equal_handles_nan() {
        let nan_float = PropertyValue::Float(f64::NAN);
        assert_ne!(nan_float, nan_float, "PartialEq should treat NaN != NaN");
        assert!(
            nan_float.semantically_equal(&nan_float),
            "semantically_equal should treat NaN == NaN"
        );

        let nan_vec = PropertyValue::vector([1.0f32, f32::NAN, 2.0f32]);
        assert_ne!(
            nan_vec, nan_vec,
            "PartialEq should treat vector with NaN != itself"
        );
        assert!(
            nan_vec.semantically_equal(&nan_vec),
            "semantically_equal should treat vector with NaN == itself"
        );

        let other = PropertyValue::Int(42);
        assert!(!nan_float.semantically_equal(&other));
    }

    #[test]
    fn test_serialize_vector_into_at_limit() {
        let max_vector = vec![0.0f32; MAX_VECTOR_DIMENSIONS];
        let mut buffer = Vec::new();
        serialize_vector_into(&max_vector, &mut buffer);

        let (deserialized, _) = deserialize_vector(&buffer).unwrap();
        assert_eq!(deserialized.len(), MAX_VECTOR_DIMENSIONS);
    }

    #[test]
    fn test_array_max_elements_boundary() {
        let mut bytes = Vec::new();
        bytes.push(TAG_ARRAY);
        let count = MAX_ARRAY_ELEMENTS as u32;
        bytes.extend_from_slice(&count.to_le_bytes());

        let result = PropertyValue::deserialize(&bytes);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("Insufficient buffer size"),
            "Should pass max check and fail on buffer size: {}",
            err
        );

        let mut bytes_overflow = Vec::new();
        bytes_overflow.push(TAG_ARRAY);
        let count_overflow = (MAX_ARRAY_ELEMENTS + 1) as u32;
        bytes_overflow.extend_from_slice(&count_overflow.to_le_bytes());

        let result_overflow = PropertyValue::deserialize(&bytes_overflow);
        assert!(result_overflow.is_err());
        let err_overflow = result_overflow.unwrap_err();
        assert!(
            err_overflow.to_string().contains("exceeds maximum allowed"),
            "Should fail max check: {}",
            err_overflow
        );
    }
}
