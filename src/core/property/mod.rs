//! Property system with Arc-based deduplication.
//!
//! This module provides a copy-on-write property system where properties are
//! stored in immutable, reference-counted containers. This enables:
//! - Cheap cloning of property maps (just increment reference count)
//! - Deduplication of unchanged properties across versions
//! - Zero-copy sharing of immutable data

pub mod constants;
pub mod map;
pub mod value;
pub mod vector_serde;

pub use constants::*;
pub use map::*;
pub use value::*;
pub use vector_serde::*;
