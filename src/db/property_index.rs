//! Public API for the secondary property (equality) index.
//!
//! See [`crate::index::property_index`] for the design, scope boundaries, and the
//! rationale for excluding `Float`. The index is **opt-in, node-only,
//! current-state, and rebuilt-on-load** (never persisted).

use crate::core::error::Result;
use crate::db::AletheiaDB;
use crate::index::property_index::PropertyIndexInfo;

/// Fluent builder for enabling a property index, mirroring
/// [`vector_index`](AletheiaDB::vector_index).
///
/// Obtained from [`AletheiaDB::property_index`]. The index covers a single
/// `(label, property)` pair; call [`enable`](Self::enable) to create and backfill
/// it.
///
/// # Example
///
/// ```rust,no_run
/// # use aletheiadb::AletheiaDB;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let db = AletheiaDB::new()?;
/// db.property_index("Person", "email").enable()?;
/// # Ok(())
/// # }
/// ```
#[must_use = "PropertyIndexBuilder does nothing unless .enable() is called"]
pub struct PropertyIndexBuilder<'a> {
    db: &'a AletheiaDB,
    label: String,
    property: String,
}

impl<'a> PropertyIndexBuilder<'a> {
    /// Create a new builder for the given `(label, property)` pair.
    pub fn new(db: &'a AletheiaDB, label: String, property: String) -> Self {
        PropertyIndexBuilder {
            db,
            label,
            property,
        }
    }

    /// Enable the index and backfill it from current state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FailedPrecondition`](crate::core::error::Error::FailedPrecondition)
    /// if an index is already enabled for this `(label, property)` pair.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn enable(self) -> Result<()> {
        self.db
            .current
            .enable_property_index(&self.label, &self.property)
    }
}

impl AletheiaDB {
    /// Create a builder to enable a current-state equality index on
    /// `(label, property)`.
    ///
    /// Indexable value types are `String`, `Int`, and `Bool`; a lookup whose
    /// value is any other variant (notably `Float`) transparently falls back to
    /// the equality scan. See [`crate::index::property_index`] for details.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// db.property_index("Person", "email").enable()?;
    /// assert!(db.has_property_index("Person", "email"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn property_index(&self, label: &str, property: &str) -> PropertyIndexBuilder<'_> {
        PropertyIndexBuilder::new(self, label.to_string(), property.to_string())
    }

    /// Whether an equality index is enabled for `(label, property)`.
    pub fn has_property_index(&self, label: &str, property: &str) -> bool {
        self.current.has_property_index(label, property)
    }

    /// List all enabled property indexes.
    pub fn list_property_indexes(&self) -> Vec<PropertyIndexInfo> {
        self.current.list_property_indexes()
    }

    /// Drop the equality index on `(label, property)`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FailedPrecondition`](crate::core::error::Error::FailedPrecondition)
    /// if no index is enabled for this `(label, property)` pair.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn drop_property_index(&self, label: &str, property: &str) -> Result<()> {
        self.current.drop_property_index(label, property)
    }
}
