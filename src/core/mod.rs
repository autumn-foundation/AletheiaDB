//! Core primitives for GallifreyDB.
//!
//! This module contains the fundamental types and data structures that
//! everything else is built upon.

pub mod graph;
pub mod id;
pub mod interning;
pub mod property;
pub mod temporal;
pub mod vector;

// Re-export commonly used types for convenience
pub use graph::{Edge, Node};
pub use id::{EdgeId, EntityId, IdGenerator, NodeId, VersionId};
pub use interning::{GLOBAL_INTERNER, InternedString, StringInterner};
pub use property::{PropertyKey, PropertyMap, PropertyMapBuilder, PropertyValue};
pub use temporal::{BiTemporalInterval, TIMESTAMP_MAX, TimeRange, Timestamp};
pub use vector::{
    VectorDimension, cosine_similarity, cosine_similarity_normalized, dot_product,
    euclidean_distance, squared_euclidean_distance,
};
