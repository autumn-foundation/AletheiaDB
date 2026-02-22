//! AletheiaDB Prelude
//!
//! The prelude module re-exports the most commonly used traits and types
//! to make starting with AletheiaDB easier.
//!
//! # Usage
//!
//! ```rust
//! use aletheiadb::prelude::*;
//! ```

pub use crate::AletheiaDB;
pub use crate::api::{ReadOps, ReadTransaction, WriteOps, WriteTransaction};
pub use crate::core::error::{Error, Result};
pub use crate::core::id::{EdgeId, NodeId, VersionId};
pub use crate::core::interning::{GLOBAL_INTERNER, InternedString};
pub use crate::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};
pub use crate::core::temporal::{BiTemporalInterval, TimeRange, Timestamp, time};
pub use crate::storage::wal::{DurabilityMode, WriteOptions};
