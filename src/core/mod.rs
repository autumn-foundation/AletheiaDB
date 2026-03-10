//! Core primitives for AletheiaDB.
//!
//! This module contains the fundamental types and data structures that
//! serve as the building blocks for the database engine.
//!
//! # Core Components
//!
//! - **[`graph`]**: Fundamental graph elements ([`Node`], [`Edge`]).
//! - **[`id`]**: Strongly-typed identifiers ([`NodeId`], [`EdgeId`]) and ID generation.
//! - **[`property`]**: Copy-on-write property system with [`PropertyMap`].
//! - **[`temporal`]**: Bi-temporal primitives ([`Timestamp`], [`TimeRange`]) for tracking valid and transaction time.
//! - **[`vector`]**: Vector embeddings and similarity metrics.
//! - **[`interning`]**: String interning for memory-efficient storage of labels and keys.
//! - **[`hlc`]**: Hybrid Logical Clock implementation for distributed timekeeping.
//!
//! # Why Interning?
//! Strings like graph labels ("Person", "KNOWS") and property keys often repeat millions of times.
//! Storing them as raw `String`s would be highly memory inefficient.
//! Instead, the [`interning`] module maps these repeating strings to integers (IDs), reducing overhead
//! and making comparisons as fast as an integer check.
//!
//! # Why Hybrid Logical Clocks (HLC)?
//! In distributed systems, relying solely on wall-clock time can lead to ordering issues due to clock drift.
//! Using purely logical clocks loses the human-readable concept of "when" something happened.
//! The [`hlc`] module combines both: ensuring causal ordering for events while remaining tightly bound
//! to the physical wall clock, bridging the gap between machines and human expectations.

pub mod error;
pub mod graph;
pub mod hasher;
pub mod history;
pub mod hlc;
pub mod id;
pub mod interning;
pub mod observer;
pub mod property;
pub mod temporal;
pub mod vector;

// Re-export commonly used types for convenience
pub use error::{Error, Result, StorageError, TemporalError};
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
pub mod version;
#[cfg(test)]
pub(crate) mod sentry_property_error_tests;
