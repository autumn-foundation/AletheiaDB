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

/// The database error and result types.
///
/// **⚠️ Warning:** Importing `Result` from the prelude shadows the standard library's `std::result::Result`.
/// If you are writing a `main` function that returns a result, you must explicitly use `std::result::Result`:
///
/// ```rust
/// use aletheiadb::prelude::*;
///
/// // Notice the explicit std::result::Result here:
/// fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
///     let db = AletheiaDB::new()?;
///
///     // Database methods return aletheiadb::Result
///     let id: NodeId = db.write(|tx| {
///         tx.create_node("Test", PropertyMap::new())
///     })?;
///
///     Ok(())
/// }
/// ```
pub use crate::core::error::{Error, Result};

pub use crate::core::id::{EdgeId, NodeId, VersionId};
pub use crate::core::interning::InternedString;
pub use crate::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};
pub use crate::core::temporal::{BiTemporalInterval, TimeRange, Timestamp, time};
// Re-export the properties! macro. It is exported at the crate root because of #[macro_export],
// so we re-export it from there.
pub use crate::properties;
pub use crate::storage::wal::{DurabilityMode, WriteOptions};
