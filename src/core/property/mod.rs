//! Property system with Arc-based deduplication.
//!
//! This module provides a copy-on-write property system where properties are
//! stored in immutable, reference-counted containers. This enables:
//! - Cheap cloning of property maps (just increment reference count)
//! - Deduplication of unchanged properties across versions
//! - Zero-copy sharing of immutable data

pub mod map;
pub mod value;

pub use map::{MAX_PROPERTY_MAP_CAPACITY, PropertyMap, PropertyMapBuilder};
pub use value::{
    MAX_ARRAY_ELEMENTS, MAX_RECURSION_DEPTH, PropertyKey, PropertyValue, TAG_ARRAY, TAG_BOOL,
    TAG_BYTES, TAG_FLOAT, TAG_INT, TAG_NULL, TAG_STRING,
};

// Re-export vector constants
pub use crate::core::vector::constants::{MAX_VECTOR_DIMENSIONS, TAG_SPARSE_VECTOR, TAG_VECTOR};
pub use crate::core::vector::serialization::{
    deserialize_sparse_vector, deserialize_vector, serialize_sparse_vector,
    serialize_sparse_vector_into, serialize_vector, serialize_vector_into,
    try_serialize_vector_into,
};

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
