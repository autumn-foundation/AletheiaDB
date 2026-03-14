//! History and versioning types shared between core and storage.
//!
//! This module provides structured types for returning historical data,
//! version information, and version diffs.

use crate::core::id::VersionId;
use crate::core::interning::InternedString;
use crate::core::property::{PropertyMap, PropertyValue};
use crate::core::temporal::{BiTemporalInterval, Timestamp};

/// Information about a specific version of an entity (node or edge).
///
/// Represents a snapshot of an entity at a particular point in its history.
///
/// # Temporal Access
///
/// Note that `VersionInfo` does not expose `tx_time` (transaction time) directly as a field.
/// Instead, it is embedded within the `temporal` field (a [`BiTemporalInterval`]).
///
/// To access the timestamp when this version was recorded:
/// ```rust
/// use aletheiadb::core::history::VersionInfo;
/// use aletheiadb::core::temporal::{BiTemporalInterval, time};
/// use aletheiadb::core::{VersionId, PropertyMap};
///
/// // Create a dummy version info
/// let now = time::now();
/// let version = VersionInfo {
///     version_number: 1,
///     version_id: VersionId::new(1).unwrap(),
///     temporal: BiTemporalInterval::current(now),
///     properties: PropertyMap::new(),
///     label: "Person".to_string(),
/// };
///
/// let recorded_at = version.temporal.transaction_time().start();
/// let valid_from = version.temporal.valid_time().start();
///
/// assert_eq!(recorded_at, now);
/// assert_eq!(valid_from, now);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct VersionInfo {
    /// Sequential version number (1, 2, 3, ...)
    pub version_number: u64,
    /// Internal version ID
    pub version_id: VersionId,
    /// Bi-temporal validity interval
    pub temporal: BiTemporalInterval,
    /// Properties at this version
    pub properties: PropertyMap,
    /// Entity label
    pub label: String,
}

/// Complete history of an entity (node or edge).
///
/// Represents the full timeline of an entity, containing all versions sorted chronologically
/// by version number (oldest first).
///
/// # Example
///
/// ```rust
/// use aletheiadb::core::history::{EntityHistory, VersionInfo};
/// use aletheiadb::core::temporal::{BiTemporalInterval, time};
/// use aletheiadb::core::{VersionId, PropertyMap};
///
/// // Construct a mock history
/// let now = time::now();
/// let v1 = VersionInfo {
///     version_number: 1,
///     version_id: VersionId::new(1).unwrap(),
///     temporal: BiTemporalInterval::current(now),
///     properties: PropertyMap::new(),
///     label: "Person".to_string(),
/// };
///
/// let history = EntityHistory { versions: vec![v1] };
///
/// println!("Entity has {} versions", history.version_count());
///
/// for version in &history.versions {
///     println!("Version {}: Valid from {}",
///         version.version_number,
///         version.temporal.valid_time().start()
///     );
/// }
/// ```
#[derive(Debug, Clone)]
pub struct EntityHistory {
    /// All versions ordered by version_number (oldest first)
    pub versions: Vec<VersionInfo>,
}

impl EntityHistory {
    /// Get the total number of versions
    #[must_use]
    pub fn version_count(&self) -> usize {
        self.versions.len()
    }

    /// Get the current (latest) version
    #[must_use]
    pub fn current_version(&self) -> Option<&VersionInfo> {
        self.versions.last()
    }

    /// Get the first (initial) version
    #[must_use]
    pub fn first_version(&self) -> Option<&VersionInfo> {
        self.versions.first()
    }
}

/// Difference between two versions of an entity.
///
/// Computes the semantic difference (delta) between the property maps of two versions.
/// Useful for auditing changes, generating changelogs, or debugging.
///
/// # Change Categories
///
/// - **Added**: Key exists in `to` but not in `from`.
/// - **Removed**: Key exists in `from` but not in `to`.
/// - **Modified**: Key exists in both, but values are semantically different (using `semantically_equal`).
///   - Note: Floating point `NaN`s are considered equal to each other.
///
/// # Example
///
/// ```rust
/// use aletheiadb::core::history::VersionDiff;
/// use aletheiadb::core::{PropertyMapBuilder, VersionId};
///
/// // Define properties for two versions
/// let v1_props = PropertyMapBuilder::new()
///     .insert("name", "Alice")
///     .build();
///
/// let v2_props = PropertyMapBuilder::new()
///     .insert("name", "Alice")
///     .insert("age", 30) // Added property
///     .build();
///
/// // Compute diff
/// let diff = VersionDiff::compute(
///     &v1_props,
///     &v2_props,
///     VersionId::new(1).unwrap(),
///     VersionId::new(2).unwrap()
/// );
///
/// if diff.has_changes() {
///     println!("Changes detected:");
///     println!("  Added: {} properties", diff.added.len());
///     println!("  Removed: {} properties", diff.removed.len());
///     println!("  Modified: {} properties", diff.modified.len());
///     assert_eq!(diff.added.len(), 1);
/// }
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct VersionDiff {
    /// The older version ID (baseline)
    pub from_version: VersionId,
    /// The newer version ID (comparison target)
    pub to_version: VersionId,
    /// Properties that were added in the newer version.
    pub added: PropertyMap,
    /// Properties that were removed in the newer version.
    pub removed: PropertyMap,
    /// Properties that were modified (Key, OldValue, NewValue).
    pub modified: Vec<(InternedString, PropertyValue, PropertyValue)>,
}

impl VersionDiff {
    /// Compute the difference between two property maps.
    ///
    /// # Arguments
    ///
    /// * `from` - Properties of the older version
    /// * `to` - Properties of the newer version
    /// * `from_id` - Version ID of the older version
    /// * `to_id` - Version ID of the newer version
    #[must_use]
    pub fn compute(
        from: &PropertyMap,
        to: &PropertyMap,
        from_id: VersionId,
        to_id: VersionId,
    ) -> Self {
        use crate::core::property::PropertyMapBuilder;

        let mut added_builder = PropertyMapBuilder::new();
        let mut removed_builder = PropertyMapBuilder::new();
        let mut modified = Vec::new();

        // Find added and modified properties
        for (key, to_value) in to.iter() {
            match from.get_by_interned_key(key) {
                None => {
                    // Property was added
                    added_builder = added_builder.insert_by_key(*key, to_value.clone());
                }
                Some(from_value) if !from_value.semantically_equal(to_value) => {
                    // Property was modified
                    modified.push((*key, from_value.clone(), to_value.clone()));
                }
                _ => {
                    // Property unchanged
                }
            }
        }

        // Find removed properties
        for (key, from_value) in from.iter() {
            if !to.contains_interned_key(key) {
                removed_builder = removed_builder.insert_by_key(*key, from_value.clone());
            }
        }

        VersionDiff {
            from_version: from_id,
            to_version: to_id,
            added: added_builder.build(),
            removed: removed_builder.build(),
            modified,
        }
    }

    /// Check if there are any changes between the versions
    #[must_use]
    pub fn has_changes(&self) -> bool {
        !self.added.is_empty() || !self.removed.is_empty() || !self.modified.is_empty()
    }

    /// Get the total number of property changes
    #[must_use]
    pub fn change_count(&self) -> usize {
        self.added.len() + self.removed.len() + self.modified.len()
    }
}

/// Summary of version changes for display purposes.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionSummary {
    /// Version ID
    pub version_id: VersionId,
    /// Version number (1, 2, 3, ...)
    pub version_number: u64,
    /// When the version became valid
    pub valid_from: Timestamp,
    /// When the version was recorded
    pub transaction_time: Timestamp,
    /// Count of properties added in this version
    pub properties_added: usize,
    /// Count of properties removed in this version
    pub properties_removed: usize,
    /// Count of properties modified in this version
    pub properties_modified: usize,
}

impl VersionSummary {
    /// Check if this version has any property changes
    #[must_use]
    pub fn has_changes(&self) -> bool {
        self.properties_added > 0 || self.properties_removed > 0 || self.properties_modified > 0
    }

    /// Get the total number of property changes
    #[must_use]
    pub fn change_count(&self) -> usize {
        self.properties_added + self.properties_removed + self.properties_modified
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::GLOBAL_INTERNER;
    use crate::core::hlc::HybridTimestamp;
    use crate::core::property::PropertyMapBuilder;

    fn test_timestamp(wallclock: i64) -> Timestamp {
        HybridTimestamp::new(wallclock, 0).unwrap()
    }

    fn test_version_id(id: u64) -> VersionId {
        VersionId::new(id).unwrap()
    }

    // ==================== VersionDiff Tests ====================

    #[test]
    fn test_version_diff_compute_detects_added() {
        let from_props = PropertyMapBuilder::new().insert("name", "Alice").build();

        let to_props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let diff = VersionDiff::compute(
            &from_props,
            &to_props,
            test_version_id(1),
            test_version_id(2),
        );

        assert_eq!(diff.added.len(), 1);
        assert!(diff.added.contains_key("age"));
        assert!(diff.removed.is_empty());
        assert!(diff.modified.is_empty());
    }

    #[test]
    fn test_version_diff_compute_detects_removed() {
        let from_props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let to_props = PropertyMapBuilder::new().insert("name", "Alice").build();

        let diff = VersionDiff::compute(
            &from_props,
            &to_props,
            test_version_id(1),
            test_version_id(2),
        );

        assert!(diff.added.is_empty());
        assert_eq!(diff.removed.len(), 1);
        assert!(diff.removed.contains_key("age"));
        assert!(diff.modified.is_empty());
    }

    #[test]
    fn test_version_diff_compute_detects_modified() {
        let from_props = PropertyMapBuilder::new().insert("name", "Alice").build();

        let to_props = PropertyMapBuilder::new().insert("name", "Bob").build();

        let diff = VersionDiff::compute(
            &from_props,
            &to_props,
            test_version_id(1),
            test_version_id(2),
        );

        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert_eq!(diff.modified.len(), 1);
        use crate::core::GLOBAL_INTERNER;
        let key_str = GLOBAL_INTERNER
            .resolve_with(diff.modified[0].0, |s| s.to_string())
            .unwrap();
        assert_eq!(key_str, "name");
    }

    #[test]
    fn test_version_diff_compute_detects_multiple_changes() {
        let from_props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("status", "active")
            .build();

        let to_props = PropertyMapBuilder::new()
            .insert("name", "Alice") // Unchanged
            .insert("age", 31i64) // Modified
            .insert("city", "NYC") // Added
            // status removed
            .build();

        let diff = VersionDiff::compute(
            &from_props,
            &to_props,
            test_version_id(1),
            test_version_id(2),
        );

        assert_eq!(diff.added.len(), 1);
        assert!(diff.added.contains_key("city"));

        assert_eq!(diff.removed.len(), 1);
        assert!(diff.removed.contains_key("status"));

        assert_eq!(diff.modified.len(), 1);
        let key_str = GLOBAL_INTERNER
            .resolve_with(diff.modified[0].0, |s| s.to_string())
            .unwrap();
        assert_eq!(key_str, "age");
    }

    #[test]
    fn test_version_diff_has_changes() {
        let from_props = PropertyMapBuilder::new().insert("name", "Alice").build();

        let to_props = PropertyMapBuilder::new().insert("name", "Bob").build();

        let diff = VersionDiff::compute(
            &from_props,
            &to_props,
            test_version_id(1),
            test_version_id(2),
        );

        assert!(diff.has_changes());
        assert_eq!(diff.change_count(), 1);
    }

    #[test]
    fn test_version_diff_no_changes() {
        let from_props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let to_props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let diff = VersionDiff::compute(
            &from_props,
            &to_props,
            test_version_id(1),
            test_version_id(2),
        );

        assert!(!diff.has_changes());
        assert_eq!(diff.change_count(), 0);
    }

    // ==================== EntityHistory Tests ====================

    #[test]
    fn test_entity_history_version_count() {
        // Table-driven test to kill multiple mutants for `version_count`
        let test_cases = vec![
            (vec![], 0),
            (vec![create_test_version_info(1, 1000)], 1),
            (
                vec![
                    create_test_version_info(1, 1000),
                    create_test_version_info(2, 2000),
                ],
                2,
            ),
        ];

        for (versions, expected_count) in test_cases {
            let history = EntityHistory { versions };
            assert_eq!(history.version_count(), expected_count);
        }
    }

    #[test]
    fn test_entity_history_current_version() {
        // Test with empty history
        let empty_history = EntityHistory { versions: vec![] };
        assert!(empty_history.current_version().is_none());

        // Test with multiple versions
        let v1 = create_test_version_info(1, 1000);
        let v2 = create_test_version_info(2, 2000);

        let history = EntityHistory {
            versions: vec![v1.clone(), v2.clone()],
        };

        let current = history.current_version().unwrap();
        assert_eq!(current.version_number, 2);
        assert_eq!(current.version_id, test_version_id(2));
    }

    #[test]
    fn test_entity_history_first_version() {
        // Test with empty history
        let empty_history = EntityHistory { versions: vec![] };
        assert!(empty_history.first_version().is_none());

        // Test with multiple versions
        let v1 = create_test_version_info(1, 1000);
        let v2 = create_test_version_info(2, 2000);

        let history = EntityHistory {
            versions: vec![v1.clone(), v2.clone()],
        };

        let first = history.first_version().unwrap();
        assert_eq!(first.version_number, 1);
        assert_eq!(first.version_id, test_version_id(1));
    }

    #[test]
    fn test_entity_history_empty() {
        let history = EntityHistory { versions: vec![] };

        assert_eq!(history.version_count(), 0);
        assert!(history.current_version().is_none());
        assert!(history.first_version().is_none());
    }

    // ==================== VersionSummary Tests ====================

    #[test]
    fn test_version_summary_changes_and_count() {
        // Table-driven test to kill all mutants related to has_changes and change_count
        let test_cases = vec![
            // (added, removed, modified, expected_has_changes, expected_change_count)
            (0, 0, 0, false, 0),
            (1, 0, 0, true, 1),
            (0, 1, 0, true, 1),
            (0, 0, 1, true, 1),
            (1, 1, 0, true, 2),
            (1, 0, 1, true, 2),
            (0, 1, 1, true, 2),
            (1, 1, 1, true, 3),
            (2, 1, 3, true, 6),
            (0, 0, 5, true, 5),
            (5, 0, 0, true, 5),
            (0, 5, 0, true, 5),
        ];

        for (added, removed, modified, expected_has_changes, expected_change_count) in test_cases {
            let summary = VersionSummary {
                version_id: test_version_id(1),
                version_number: 1,
                valid_from: test_timestamp(1000),
                transaction_time: test_timestamp(2000),
                properties_added: added,
                properties_removed: removed,
                properties_modified: modified,
            };

            assert_eq!(
                summary.has_changes(),
                expected_has_changes,
                "has_changes mismatch for added={}, removed={}, modified={}",
                added,
                removed,
                modified
            );
            assert_eq!(
                summary.change_count(),
                expected_change_count,
                "change_count mismatch for added={}, removed={}, modified={}",
                added,
                removed,
                modified
            );
        }
    }

    #[test]
    fn test_version_diff_has_changes_added_only() {
        let from_props = PropertyMapBuilder::new().build();
        let to_props = PropertyMapBuilder::new().insert("name", "Alice").build();

        let diff = VersionDiff::compute(
            &from_props,
            &to_props,
            test_version_id(10),
            test_version_id(11),
        );

        assert_eq!(diff.added.len(), 1);
        assert!(diff.removed.is_empty());
        assert!(diff.modified.is_empty());
        assert!(diff.has_changes());
    }

    #[test]
    fn test_version_diff_has_changes_removed_only() {
        let from_props = PropertyMapBuilder::new().insert("status", "active").build();
        let to_props = PropertyMapBuilder::new().build();

        let diff = VersionDiff::compute(
            &from_props,
            &to_props,
            test_version_id(12),
            test_version_id(13),
        );

        assert!(diff.added.is_empty());
        assert_eq!(diff.removed.len(), 1);
        assert!(diff.modified.is_empty());
        assert!(diff.has_changes());
    }

    #[test]
    fn test_version_diff_change_count_and_has_changes() {
        // Table-driven test for VersionDiff methods change_count and has_changes
        let test_cases = vec![
            // (from_props, to_props, expected_added, expected_removed, expected_modified)
            // No changes
            (
                PropertyMapBuilder::new().insert("name", "Alice").build(),
                PropertyMapBuilder::new().insert("name", "Alice").build(),
                0,
                0,
                0,
            ),
            // Only Added
            (
                PropertyMapBuilder::new().build(),
                PropertyMapBuilder::new().insert("name", "Alice").build(),
                1,
                0,
                0,
            ),
            // Only Removed
            (
                PropertyMapBuilder::new().insert("name", "Alice").build(),
                PropertyMapBuilder::new().build(),
                0,
                1,
                0,
            ),
            // Only Modified
            (
                PropertyMapBuilder::new().insert("name", "Alice").build(),
                PropertyMapBuilder::new().insert("name", "Bob").build(),
                0,
                0,
                1,
            ),
            // Added, Removed, and Modified
            (
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 30i64)
                    .insert("status", "active")
                    .build(),
                PropertyMapBuilder::new()
                    .insert("name", "Bob")
                    .insert("city", "NYC")
                    .build(),
                1,
                2,
                1,
            ),
        ];

        for (from_props, to_props, exp_added, exp_removed, exp_modified) in test_cases {
            let diff = VersionDiff::compute(
                &from_props,
                &to_props,
                test_version_id(1),
                test_version_id(2),
            );

            assert_eq!(diff.added.len(), exp_added);
            assert_eq!(diff.removed.len(), exp_removed);
            assert_eq!(diff.modified.len(), exp_modified);

            let expected_count = exp_added + exp_removed + exp_modified;
            let expected_has_changes = expected_count > 0;

            assert_eq!(
                diff.has_changes(),
                expected_has_changes,
                "has_changes mismatch for {:?}",
                diff
            );
            assert_eq!(
                diff.change_count(),
                expected_count,
                "change_count mismatch for {:?}",
                diff
            );
        }
    }

    #[test]
    fn test_version_summary_has_changes_removed_only() {
        let summary = VersionSummary {
            version_id: test_version_id(3),
            version_number: 3,
            valid_from: test_timestamp(3000),
            transaction_time: test_timestamp(4000),
            properties_added: 0,
            properties_removed: 1,
            properties_modified: 0,
        };

        assert!(summary.has_changes());
        assert_eq!(summary.change_count(), 1);
    }

    #[test]
    fn test_version_summary_has_changes_modified_only() {
        let summary = VersionSummary {
            version_id: test_version_id(4),
            version_number: 4,
            valid_from: test_timestamp(4000),
            transaction_time: test_timestamp(5000),
            properties_added: 0,
            properties_removed: 0,
            properties_modified: 1,
        };

        assert!(summary.has_changes());
        assert_eq!(summary.change_count(), 1);
    }
    // ==================== Helper Functions ====================

    fn create_test_version_info(version_num: u64, wallclock: i64) -> VersionInfo {
        let timestamp = test_timestamp(wallclock);
        VersionInfo {
            version_number: version_num,
            version_id: test_version_id(version_num),
            temporal: BiTemporalInterval::with_valid_time(timestamp, timestamp),
            properties: PropertyMapBuilder::new()
                .insert("version", version_num as i64)
                .build(),
            label: "Test".to_string(),
        }
    }

    #[test]
    fn test_version_diff_nan_equality() {
        // Setup: Create two property maps with the same key but seemingly different NaN values
        // (IEEE 754 says NaN != NaN, but our system should treat them as "no change")

        let from_props = PropertyMapBuilder::new()
            .insert("score", PropertyValue::Float(f64::NAN))
            .build();

        let to_props = PropertyMapBuilder::new()
            .insert("score", PropertyValue::Float(f64::NAN))
            .build();

        // Compute diff
        let diff = VersionDiff::compute(
            &from_props,
            &to_props,
            test_version_id(1),
            test_version_id(2),
        );

        // Verify: Should report NO changes
        assert!(
            diff.modified.is_empty(),
            "VersionDiff should ignore NaN -> NaN 'changes' for Float"
        );
        assert!(!diff.has_changes(), "VersionDiff should not report changes");
    }

    #[test]
    fn test_version_diff_vector_nan_equality() {
        // Setup: Vector containing NaN
        let vec_with_nan = vec![1.0, f32::NAN, 3.0];

        let from_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&vec_with_nan))
            .build();

        let to_props = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&vec_with_nan))
            .build();

        // Compute diff
        let diff = VersionDiff::compute(
            &from_props,
            &to_props,
            test_version_id(1),
            test_version_id(2),
        );

        // Verify: Should report NO changes
        assert!(
            diff.modified.is_empty(),
            "VersionDiff should ignore NaN -> NaN 'changes' for Vector"
        );
        assert!(!diff.has_changes(), "VersionDiff should not report changes");
    }

    #[test]
    fn test_version_diff_null_equality() {
        // Setup: Null -> Null
        let from_props = PropertyMapBuilder::new()
            .insert("data", PropertyValue::Null)
            .build();

        let to_props = PropertyMapBuilder::new()
            .insert("data", PropertyValue::Null)
            .build();

        // Compute diff
        let diff = VersionDiff::compute(
            &from_props,
            &to_props,
            test_version_id(1),
            test_version_id(2),
        );

        // Verify
        assert!(
            diff.modified.is_empty(),
            "VersionDiff should ignore Null -> Null"
        );
        assert!(!diff.has_changes());
    }

    #[test]
    fn test_version_diff_compute_verifies_ids() {
        let from_props = PropertyMapBuilder::new().build();
        let to_props = PropertyMapBuilder::new().build();
        let from_id = test_version_id(1);
        let to_id = test_version_id(2);

        let diff = VersionDiff::compute(&from_props, &to_props, from_id, to_id);

        // Verify that ID fields are correctly assigned
        // This kills mutants that swap or default-initialize these fields
        assert_eq!(diff.from_version, from_id, "from_version must match input");
        assert_eq!(diff.to_version, to_id, "to_version must match input");
        assert_ne!(from_id, to_id, "Test requires distinct IDs");
    }
}
