//! History and versioning types shared between core and storage.
//!
//! This module provides structured types for returning historical data,
//! version information, and version diffs.

use crate::core::id::VersionId;
use crate::core::interning::{InternedString, GLOBAL_INTERNER};
use crate::core::property::{PropertyMap, PropertyValue};
use crate::core::temporal::{BiTemporalInterval, Timestamp};
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Attribute, Cell, Color, Table};
use std::fmt;

/// Information about a specific version of an entity (node or edge).
///
/// Represents a snapshot of an entity at a particular point in its history.
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
/// Contains all versions in chronological order.
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
/// Shows what properties were added, removed, or modified.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionDiff {
    /// The older version ID (from)
    pub from_version: VersionId,
    /// The newer version ID (to)
    pub to_version: VersionId,
    /// Properties that were added (key -> new value)
    pub added: PropertyMap,
    /// Properties that were removed (key -> old value)
    pub removed: PropertyMap,
    /// Properties that were modified (key -> (old value, new value))
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
                Some(from_value) if from_value != to_value => {
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

// ==================== Display Implementations ====================

impl fmt::Display for VersionInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .apply_modifier(UTF8_ROUND_CORNERS)
            .set_header(vec![
                Cell::new("Version Info").add_attribute(Attribute::Bold),
                Cell::new(format!("Version #{}", self.version_number)),
            ]);

        table.add_row(vec![
            Cell::new("Version ID"),
            Cell::new(format!("{:?}", self.version_id)),
        ]);
        table.add_row(vec![Cell::new("Label"), Cell::new(&self.label)]);
        table.add_row(vec![
            Cell::new("Valid Time"),
            Cell::new(format!("{}", self.temporal.valid_time())),
        ]);
        table.add_row(vec![
            Cell::new("Transaction Time"),
            Cell::new(format!("{}", self.temporal.transaction_time())),
        ]);

        // Properties Section
        if !self.properties.is_empty() {
            let mut prop_table = Table::new();
            prop_table.set_header(vec!["Key", "Value"]);

            // Sort keys for consistent display
            let mut keys: Vec<_> = self.properties.keys().collect();
            keys.sort_by_key(|k| k.as_u32());

            for key in keys {
                let key_str = GLOBAL_INTERNER
                    .resolve(*key)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("UnknownKey({})", key.as_u32()));

                let value = self.properties.get_by_interned_key(key).unwrap();
                let value_cell = format_property_value_cell(value);

                prop_table.add_row(vec![Cell::new(key_str), value_cell]);
            }
            table.add_row(vec![Cell::new("Properties"), Cell::new(prop_table)]);
        } else {
            table.add_row(vec![Cell::new("Properties"), Cell::new("None")]);
        }

        write!(f, "{}", table)
    }
}

impl fmt::Display for EntityHistory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.versions.is_empty() {
            return write!(f, "No history available.");
        }

        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .apply_modifier(UTF8_ROUND_CORNERS)
            .set_header(vec![
                Cell::new("Ver").add_attribute(Attribute::Bold),
                Cell::new("Valid Time").add_attribute(Attribute::Bold),
                Cell::new("Tx Time").add_attribute(Attribute::Bold),
                Cell::new("Label").add_attribute(Attribute::Bold),
                Cell::new("Properties").add_attribute(Attribute::Bold),
            ]);

        for version in &self.versions {
            let valid_time = format!("{}", version.temporal.valid_time());
            let tx_time = format!("{}", version.temporal.transaction_time());

            // Summarize properties
            let props_summary = if version.properties.is_empty() {
                String::from("-")
            } else {
                let mut parts = Vec::new();
                let mut keys: Vec<_> = version.properties.keys().collect();
                keys.sort_by_key(|k| k.as_u32()); // Consistent ordering

                for key in keys.iter().take(3) {
                    let key_str = GLOBAL_INTERNER
                        .resolve(**key)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "?".to_string());
                    let val = version.properties.get_by_interned_key(key).unwrap();
                    parts.push(format!("{}: {}", key_str, val));
                }

                if version.properties.len() > 3 {
                    format!("{}, ... (+{})", parts.join(", "), version.properties.len() - 3)
                } else {
                    parts.join(", ")
                }
            };

            table.add_row(vec![
                Cell::new(version.version_number),
                Cell::new(valid_time),
                Cell::new(tx_time),
                Cell::new(&version.label),
                Cell::new(props_summary),
            ]);
        }

        write!(f, "{}", table)
    }
}

impl fmt::Display for VersionDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .apply_modifier(UTF8_ROUND_CORNERS)
            .set_header(vec![
                Cell::new("Change").add_attribute(Attribute::Bold),
                Cell::new("Key").add_attribute(Attribute::Bold),
                Cell::new("Old Value").add_attribute(Attribute::Bold),
                Cell::new("New Value").add_attribute(Attribute::Bold),
            ]);

        // Added properties
        for (key, val) in self.added.iter() {
            let key_str = GLOBAL_INTERNER
                .resolve(*key)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "?".to_string());
            table.add_row(vec![
                Cell::new("ADDED").fg(Color::Green),
                Cell::new(key_str),
                Cell::new("-"),
                format_property_value_cell(val),
            ]);
        }

        // Modified properties
        for (key, old_val, new_val) in &self.modified {
            let key_str = GLOBAL_INTERNER
                .resolve(*key)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "?".to_string());
            table.add_row(vec![
                Cell::new("MODIFIED").fg(Color::Yellow),
                Cell::new(key_str),
                format_property_value_cell(old_val),
                format_property_value_cell(new_val),
            ]);
        }

        // Removed properties
        for (key, val) in self.removed.iter() {
            let key_str = GLOBAL_INTERNER
                .resolve(*key)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "?".to_string());
            table.add_row(vec![
                Cell::new("REMOVED").fg(Color::Red),
                Cell::new(key_str),
                format_property_value_cell(val),
                Cell::new("-"),
            ]);
        }

        if !self.has_changes() {
            return write!(f, "No changes between versions.");
        }

        write!(f, "{}", table)
    }
}

impl fmt::Display for VersionSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Version #{}: +{} added, -{} removed, ~{} modified",
            self.version_number,
            self.properties_added,
            self.properties_removed,
            self.properties_modified
        )
    }
}

/// Helper to format property values with colors
fn format_property_value_cell(value: &PropertyValue) -> Cell {
    match value {
        PropertyValue::Bool(true) => Cell::new("true").fg(Color::Green),
        PropertyValue::Bool(false) => Cell::new("false").fg(Color::Red),
        PropertyValue::Null => Cell::new("null").fg(Color::DarkGrey),
        _ => Cell::new(value.to_string()),
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
        let key_str = GLOBAL_INTERNER.resolve(diff.modified[0].0).unwrap();
        assert_eq!(key_str.as_ref(), "name");
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
        let key_str = GLOBAL_INTERNER.resolve(diff.modified[0].0).unwrap();
        assert_eq!(key_str.as_ref(), "age");
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
        let history = EntityHistory {
            versions: vec![
                create_test_version_info(1, 1000),
                create_test_version_info(2, 2000),
            ],
        };

        assert_eq!(history.version_count(), 2);
    }

    #[test]
    fn test_entity_history_current_version() {
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
    fn test_version_summary_has_changes() {
        let summary = VersionSummary {
            version_id: test_version_id(1),
            version_number: 1,
            valid_from: test_timestamp(1000),
            transaction_time: test_timestamp(2000),
            properties_added: 1,
            properties_removed: 0,
            properties_modified: 0,
        };

        assert!(summary.has_changes());
        assert_eq!(summary.change_count(), 1);
    }

    #[test]
    fn test_version_summary_no_changes() {
        let summary = VersionSummary {
            version_id: test_version_id(1),
            version_number: 1,
            valid_from: test_timestamp(1000),
            transaction_time: test_timestamp(2000),
            properties_added: 0,
            properties_removed: 0,
            properties_modified: 0,
        };

        assert!(!summary.has_changes());
        assert_eq!(summary.change_count(), 0);
    }

    #[test]
    fn test_version_summary_multiple_changes() {
        let summary = VersionSummary {
            version_id: test_version_id(2),
            version_number: 2,
            valid_from: test_timestamp(2000),
            transaction_time: test_timestamp(3000),
            properties_added: 2,
            properties_removed: 1,
            properties_modified: 3,
        };

        assert!(summary.has_changes());
        assert_eq!(summary.change_count(), 6); // 2 + 1 + 3
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
}
