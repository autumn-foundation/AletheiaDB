//! Core primitives for GallifreyDB.
//!
//! This module contains the fundamental types and data structures that
//! everything else is built upon.

pub mod graph;
pub mod hlc;
pub mod id;
pub mod interning;
pub mod property;
pub mod temporal;
pub mod vector;

// Re-export commonly used types for convenience
pub use graph::{Edge, Node};
pub use id::{EdgeId, EntityId, IdGenerator, NodeId, VersionId};
pub use interning::{
    DEFAULT_MAX_INTERNED_STRINGS, GLOBAL_INTERNER, InternedString, MAX_INTERNED_STRINGS_ENV,
    StringInterner,
};
pub use property::{PropertyKey, PropertyMap, PropertyMapBuilder, PropertyValue};
pub use temporal::{BiTemporalInterval, TIMESTAMP_MAX, TimeRange, Timestamp};
pub use vector::{
    // Types and constants
    DistanceMetric,
    NORMALIZATION_TOLERANCE,
    VectorDimension,
    // Validation functions
    check_dimensions_match,
    // Similarity functions
    cosine_similarity,
    cosine_similarity_normalized,
    // Inner product
    dot_product,
    // Distance functions
    euclidean_distance,
    // Normalization functions
    is_normalized,
    is_normalized_default,
    magnitude,
    normalize,
    normalize_in_place,
    squared_euclidean_distance,
    squared_magnitude,
    validate_vector,
    validate_vector_with_bounds,
};
