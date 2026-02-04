//! Property system with Arc-based deduplication.
//!
//! This module provides a copy-on-write property system where properties are
//! stored in immutable, reference-counted containers. This enables:
//! - Cheap cloning of property maps (just increment reference count)
//! - Deduplication of unchanged properties across versions
//! - Zero-copy sharing of immutable data

/// Property types and values.
pub mod types;
/// Property map and builder.
pub mod map;
/// Serialization logic for properties.
pub mod serialization;

#[cfg(test)]
mod tests;

pub use types::*;
pub use map::*;
pub use serialization::*;
