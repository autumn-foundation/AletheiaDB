//! Core primitives for GallifreyDB.
//!
//! This module contains the fundamental types and data structures that
//! everything else is built upon.

pub mod id;
pub mod temporal;
pub mod property;
pub mod interning;
pub mod graph;

// Re-export commonly used types for convenience
pub use id::{NodeId, EdgeId, VersionId, EntityId, IdGenerator};
pub use temporal::{Timestamp, TimeRange, BiTemporalInterval, TIMESTAMP_MAX};
pub use property::{PropertyKey, PropertyValue, PropertyMap, PropertyMapBuilder};
pub use interning::{InternedString, StringInterner, GLOBAL_INTERNER};
pub use graph::{Node, Edge};
