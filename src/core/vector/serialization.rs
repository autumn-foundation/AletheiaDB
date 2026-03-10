//! Serialization utilities for vector properties.
//!
//! This module provides functions for serializing and deserializing dense and sparse vectors
//! into the binary format used by `PropertyValue`.
//!
//! These functions are primarily used by `PropertyValue` serialization, but are also
//! exposed for use in tests and other serialization contexts.

use std::sync::Arc;

use super::constants::{MAX_VECTOR_DIMENSIONS, TAG_SPARSE_VECTOR, TAG_VECTOR};
use super::sparse::SparseVec;
use crate::core::error::{Error, Result, StorageError, VectorError};

/// Validates that a vector dimension does not exceed the maximum allowed.
/// Returns Ok(()) if valid, Err(VectorError::DimensionTooLarge) otherwise.
#[inline]
pub(crate) fn validate_vector_dimensions(len: usize) -> Result<()> {
    if len > MAX_VECTOR_DIMENSIONS {
        return Err(Error::Vector(VectorError::DimensionTooLarge {
            dimension: len,
            max_allowed: MAX_VECTOR_DIMENSIONS,
        }));
    }
    Ok(())
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
///
/// For a fallible version that returns `Result` instead of panicking,
/// use [`try_serialize_vector_into`].
pub fn serialize_vector_into(v: &[f32], buffer: &mut Vec<u8>) {
    try_serialize_vector_into(v, buffer).unwrap_or_else(|e| panic!("{}", e))
}

/// Serialize a vector into an existing buffer (fallible).
///
/// This is the fallible version of [`serialize_vector_into`]. It returns
/// an error if the vector dimension exceeds `MAX_VECTOR_DIMENSIONS`
/// instead of panicking.
pub fn try_serialize_vector_into(v: &[f32], buffer: &mut Vec<u8>) -> Result<()> {
    // Defensive check: vectors should be validated at construction via PropertyValue::vector()
    validate_vector_dimensions(v.len())?;

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
    Ok(())
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

    let dimension = u32::from_le_bytes(bytes[1..5].try_into().unwrap_or_default()) as usize;

    // Prevent DoS via memory exhaustion from malicious input
    validate_vector_dimensions(dimension)?;

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
            values.push(f32::from_le_bytes(chunk.try_into().unwrap_or_default()));
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

    let dimension = u32::from_le_bytes(bytes[1..5].try_into().unwrap_or_default());
    let nnz = u32::from_le_bytes(bytes[5..9].try_into().unwrap_or_default()) as usize;

    // Validate nnz doesn't exceed dimension
    if nnz > dimension as usize {
        return Err(StorageError::CorruptedData(format!(
            "Sparse vector nnz {} exceeds dimension {}",
            nnz, dimension
        ))
        .into());
    }

    // Prevent DoS via memory exhaustion from malicious input
    validate_vector_dimensions(nnz)?;

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
            indices.push(u32::from_le_bytes(chunk.try_into().unwrap_or_default()));
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
            values.push(f32::from_le_bytes(chunk.try_into().unwrap_or_default()));
        }
        values
    };

    // Construct SparseVec (this will validate the data)
    let sparse_vec = SparseVec::new(indices, values, dimension)?;

    Ok((Arc::new(sparse_vec), total_len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_vector_basic() {
        let data = [1.0f32, 2.0, 3.0];
        let bytes = serialize_vector(&data);

        // Check format: tag (1) + dimension (4) + 3*4 bytes
        assert_eq!(bytes[0], TAG_VECTOR);
        assert_eq!(bytes.len(), 1 + 4 + 3 * 4);

        let (deserialized, consumed) = deserialize_vector(&bytes).unwrap();
        assert_eq!(deserialized.as_ref(), &data[..]);
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
        let result = deserialize_vector(&[TAG_VECTOR + 1, 3, 0, 0, 0]);
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
            let dimension = u32::from_le_bytes(bytes[1..5].try_into().unwrap_or_default()) as usize;
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
    fn test_serialize_sparse_vector_basic() {
        let sparse = SparseVec::new(vec![0, 2, 4], vec![1.0, 2.0, 3.0], 5).unwrap();
        let bytes = serialize_sparse_vector(&sparse);

        assert_eq!(bytes[0], TAG_SPARSE_VECTOR);

        let (deserialized, consumed) = deserialize_sparse_vector(&bytes).unwrap();
        assert_eq!(consumed, bytes.len());

        assert_eq!(deserialized.nnz(), 3);
        assert_eq!(deserialized.dimension(), 5);
        assert_eq!(deserialized.indices(), &[0, 2, 4]);
        assert_eq!(deserialized.values(), &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_serialize_sparse_vector_empty() {
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
    fn test_deserialize_sparse_vector_errors() {
        // Empty buffer
        let result = deserialize_sparse_vector(&[]);
        assert!(result.is_err());

        // Buffer too short for header
        let result = deserialize_sparse_vector(&[TAG_SPARSE_VECTOR, 1, 0, 0]);
        assert!(result.is_err());

        // Wrong type tag
        let result = deserialize_sparse_vector(&[TAG_SPARSE_VECTOR + 1, 5, 0, 0, 0, 2, 0, 0, 0]);
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
    #[should_panic(expected = "Vector dimension")]
    fn test_serialize_vector_into_panics_on_overflow() {
        // 💣 Risk: serialize_vector_into panics on large inputs instead of returning Result.
        let large_vector = vec![0.0; MAX_VECTOR_DIMENSIONS + 1];
        let mut buffer = Vec::new();
        serialize_vector_into(&large_vector, &mut buffer);
    }

    #[test]
    fn test_serialize_vector_into_buffer_appending() {
        // 🧪 Strategy: Verify that serialize_vector_into correctly appends to an existing buffer
        // and doesn't overwrite data or corrupt offsets.
        let mut buffer = vec![0xAA, 0xBB, 0xCC]; // Existing data
        let vector = vec![1.0f32, 2.0, 3.0];

        serialize_vector_into(&vector, &mut buffer);

        // Verify prefix is intact
        assert_eq!(buffer[0], 0xAA);
        assert_eq!(buffer[1], 0xBB);
        assert_eq!(buffer[2], 0xCC);

        // Verify vector serialization starts at offset 3
        assert_eq!(buffer[3], TAG_VECTOR);

        // Verify deserialization works from the offset
        let (deserialized, consumed) = deserialize_vector(&buffer[3..]).unwrap();
        assert_eq!(deserialized.as_ref(), &vector[..]);
        assert_eq!(consumed, buffer.len() - 3);
    }

    #[test]
    fn test_deserialize_sparse_vector_validates_duplicates() {
        // 💣 Risk: Malicious input could provide duplicate indices, violating invariants.
        // SparseVec::new checks this, so deserialize should fail.
        let mut buffer = Vec::new();
        buffer.push(TAG_SPARSE_VECTOR);

        let dim = 10u32;
        let nnz = 2u32;
        buffer.extend_from_slice(&dim.to_le_bytes());
        buffer.extend_from_slice(&nnz.to_le_bytes());

        // Duplicate index 5
        buffer.extend_from_slice(&5u32.to_le_bytes());
        buffer.extend_from_slice(&5u32.to_le_bytes());

        // Values
        buffer.extend_from_slice(&1.0f32.to_le_bytes());
        buffer.extend_from_slice(&2.0f32.to_le_bytes());

        let result = deserialize_sparse_vector(&buffer);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::InvalidSparseVector { reason })) => {
                assert!(reason.contains("Duplicate index"));
            }
            _ => panic!("Expected InvalidSparseVector error, got {:?}", result),
        }
    }

    #[test]
    fn test_deserialize_sparse_vector_validates_zeros() {
        // 💣 Risk: Sparse vectors should not contain zero values.
        let mut buffer = Vec::new();
        buffer.push(TAG_SPARSE_VECTOR);

        let dim = 10u32;
        let nnz = 1u32;
        buffer.extend_from_slice(&dim.to_le_bytes());
        buffer.extend_from_slice(&nnz.to_le_bytes());

        // Index 0
        buffer.extend_from_slice(&0u32.to_le_bytes());

        // Value 0.0 (Invalid!)
        buffer.extend_from_slice(&0.0f32.to_le_bytes());

        let result = deserialize_sparse_vector(&buffer);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::InvalidSparseVector { reason })) => {
                assert!(reason.contains("zero value"));
            }
            _ => panic!("Expected InvalidSparseVector error, got {:?}", result),
        }
    }

    #[test]
    fn test_deserialize_sparse_vector_sorts_indices() {
        // 🧪 Strategy: Verify that deserializing unsorted indices results in a sorted SparseVec.
        // SparseVec::new sorts them, so this should succeed and return sorted indices.
        let mut buffer = Vec::new();
        buffer.push(TAG_SPARSE_VECTOR);

        let dim = 10u32;
        let nnz = 2u32;
        buffer.extend_from_slice(&dim.to_le_bytes());
        buffer.extend_from_slice(&nnz.to_le_bytes());

        // Unsorted indices: 5, 2
        buffer.extend_from_slice(&5u32.to_le_bytes());
        buffer.extend_from_slice(&2u32.to_le_bytes());

        // Values: 1.0 corresponds to 5, 2.0 corresponds to 2
        buffer.extend_from_slice(&1.0f32.to_le_bytes());
        buffer.extend_from_slice(&2.0f32.to_le_bytes());

        let (sv_arc, _) =
            deserialize_sparse_vector(&buffer).expect("Should succeed and sort indices");

        // Check sorted order
        assert_eq!(sv_arc.indices(), &[2, 5]);
        // Check values moved with indices (2 was paired with 2.0)
        assert_eq!(sv_arc.values(), &[2.0, 1.0]);
    }

    #[test]
    fn test_vector_bitwise_preservation() {
        // Construct special float values
        let pos_zero = 0.0f32;
        let neg_zero = -0.0f32;
        let inf = f32::INFINITY;
        let neg_inf = f32::NEG_INFINITY;

        // Construct distinct NaN payloads if possible
        // Standard NaN: 0x7fc00000
        let nan1 = f32::from_bits(0x7fc00001); // Signaling/Quiet NaN with payload 1
        let nan2 = f32::from_bits(0x7fc00002); // Different payload

        let data = vec![pos_zero, neg_zero, inf, neg_inf, nan1, nan2];
        let bytes = serialize_vector(&data);
        let (deserialized, _) = deserialize_vector(&bytes).unwrap();

        assert_eq!(deserialized.len(), data.len());

        for (i, &val) in data.iter().enumerate() {
            assert_eq!(
                val.to_bits(),
                deserialized[i].to_bits(),
                "Bitwise mismatch at index {}: expected {:08x}, got {:08x}",
                i,
                val.to_bits(),
                deserialized[i].to_bits()
            );
        }
    }

    #[test]
    fn test_serialize_vector_slice_offsets() {
        let full_vec: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        // Take a slice from the middle: [2.0, 3.0, 4.0]
        let slice = &full_vec[1..4];

        let bytes = serialize_vector(slice);
        let (deserialized, _) = deserialize_vector(&bytes).unwrap();

        assert_eq!(&*deserialized, &[2.0, 3.0, 4.0]);
    }

    #[test]
    fn test_deserialize_vector_zero_dim() {
        let empty: Vec<f32> = Vec::new();
        let bytes = serialize_vector(&empty);
        let (deserialized, _) =
            deserialize_vector(&bytes).expect("Should deserialize empty vector");
        assert!(deserialized.is_empty());
    }
}
