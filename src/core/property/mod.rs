//! Property system with Arc-based deduplication.
//!
//! This module provides a copy-on-write property system where properties are
//! stored in immutable, reference-counted containers. This enables:
//! - Cheap cloning of property maps (just increment reference count)
//! - Deduplication of unchanged properties across versions
//! - Zero-copy sharing of immutable data

mod map;
mod serialization;
#[cfg(test)]
mod tests;
mod value;

pub use map::{PropertyMap, PropertyMapBuilder};
pub use serialization::{
    deserialize_sparse_vector, deserialize_vector, serialize_sparse_vector,
    serialize_vector, MAX_ARRAY_ELEMENTS, MAX_PROPERTY_MAP_CAPACITY,
    MAX_RECURSION_DEPTH, MAX_VECTOR_DIMENSIONS, TAG_ARRAY, TAG_BOOL, TAG_BYTES,
    TAG_FLOAT, TAG_INT, TAG_NULL, TAG_SPARSE_VECTOR, TAG_STRING, TAG_VECTOR,
};
pub use value::{PropertyKey, PropertyValue};
