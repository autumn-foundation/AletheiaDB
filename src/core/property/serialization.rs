use std::sync::Arc;
use crate::core::vector::SparseVec;
use crate::utils::error::{Result, StorageError};

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

/// Maximum capacity allowed for a deserialized property map.
/// Set to 10,000 to prevent OOM DoS attacks via malicious count fields.
pub const MAX_PROPERTY_MAP_CAPACITY: usize = 10_000;

/// Maximum recursion depth for nested properties (e.g., arrays of arrays).
/// Set to 100 to prevent stack overflow from malicious input.
pub const MAX_RECURSION_DEPTH: usize = 100;


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
