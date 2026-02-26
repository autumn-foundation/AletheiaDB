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
pub mod version;
