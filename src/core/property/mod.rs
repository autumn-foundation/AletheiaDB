//! Property system with Arc-based deduplication.
//!
//! This module provides a copy-on-write property system where properties are
//! stored in immutable, reference-counted containers. This enables:
//! - Cheap cloning of property maps (just increment reference count)
//! - Deduplication of unchanged properties across versions
//! - Zero-copy sharing of immutable data


use crate::core::interning::InternedString;

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


mod builder;
mod map;
mod value;

pub use builder::PropertyMapBuilder;
pub use map::PropertyMap;
pub use value::PropertyValue;

#[cfg(test)]
#[macro_use]
mod tests;
