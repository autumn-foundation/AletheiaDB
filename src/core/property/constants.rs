//! Constants and limits for the property system.

use crate::utils::error::{Error, Result, VectorError};

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

/// Maximum capacity allowed for a deserialized property map.
/// Set to 10,000 to prevent OOM DoS attacks via malicious count fields.
pub const MAX_PROPERTY_MAP_CAPACITY: usize = 10_000;

/// Maximum recursion depth for nested properties (e.g., arrays of arrays).
/// Set to 100 to prevent stack overflow from malicious input.
pub const MAX_RECURSION_DEPTH: usize = 100;

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
