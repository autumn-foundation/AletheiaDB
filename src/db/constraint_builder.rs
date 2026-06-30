//! Builder for uniqueness constraints, mirroring `VectorIndexBuilder`.

use crate::core::error::Result;
use crate::db::AletheiaDB;

/// Builder for declaring and enabling a uniqueness constraint.
///
/// # Example
///
/// ```rust,no_run
/// # use aletheiadb::AletheiaDB;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let db = AletheiaDB::new()?;
/// db.unique_constraint("Person", "email").enable()?;
/// # Ok(())
/// # }
/// ```
#[must_use = "call .enable() to activate the constraint"]
pub struct UniqueConstraintBuilder<'a> {
    db: &'a AletheiaDB,
    label: String,
    property: String,
}

impl<'a> UniqueConstraintBuilder<'a> {
    pub(crate) fn new(
        db: &'a AletheiaDB,
        label: impl Into<String>,
        property: impl Into<String>,
    ) -> Self {
        Self {
            db,
            label: label.into(),
            property: property.into(),
        }
    }

    /// Enable the constraint, performing a pre-flight scan for existing duplicates.
    ///
    /// Fails with `ConstraintError::DuplicateOnEnable` if duplicate currently-valid
    /// values already exist for the given label/property combination.
    pub fn enable(self) -> Result<()> {
        self.db
            .enable_unique_constraint(&self.label, &self.property)
    }
}
