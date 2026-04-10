import re

with open("src/core/history.rs", "r") as f:
    content = f.read()

content = content.replace("""    /// Get the total number of versions
    #[must_use]
    pub fn version_count(&self) -> usize {""", """    /// Returns the total number of versions in this entity's history.
    ///
    /// This is useful for understanding the update frequency or lifespan of a node or edge.
    /// A high version count might indicate a highly volatile fact, while a count of 1
    /// indicates a fact that has never been modified since its creation.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use aletheiadb::core::history::{EntityHistory, VersionInfo};
    /// use aletheiadb::core::temporal::{BiTemporalInterval, time};
    /// use aletheiadb::core::{VersionId, PropertyMap};
    ///
    /// let now = time::now();
    /// let v1 = VersionInfo {
    ///     version_number: 1,
    ///     version_id: VersionId::new(1).unwrap(),
    ///     temporal: BiTemporalInterval::current(now),
    ///     properties: PropertyMap::new(),
    ///     label: "Person".to_string(),
    /// };
    /// let history = EntityHistory { versions: vec![v1] };
    /// assert_eq!(history.version_count(), 1);
    /// ```
    #[must_use]
    pub fn version_count(&self) -> usize {""")

content = content.replace("""    /// Get the current (latest) version
    #[must_use]
    pub fn current_version(&self) -> Option<&VersionInfo> {""", """    /// Returns the most recent version of this entity.
    ///
    /// Retrieves the latest state of the entity (the version with the highest `version_number`).
    /// This is typically the active version, unless the entity has been logically deleted.
    /// Returns `None` if the history is completely empty.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use aletheiadb::core::history::{EntityHistory, VersionInfo};
    /// use aletheiadb::core::temporal::{BiTemporalInterval, time};
    /// use aletheiadb::core::{VersionId, PropertyMap};
    ///
    /// let now = time::now();
    /// let v1 = VersionInfo {
    ///     version_number: 1,
    ///     version_id: VersionId::new(1).unwrap(),
    ///     temporal: BiTemporalInterval::current(now),
    ///     properties: PropertyMap::new(),
    ///     label: "Person".to_string(),
    /// };
    /// let history = EntityHistory { versions: vec![v1] };
    /// assert_eq!(history.current_version().unwrap().version_number, 1);
    /// ```
    #[must_use]
    pub fn current_version(&self) -> Option<&VersionInfo> {""")

content = content.replace("""    /// Get the first (initial) version
    #[must_use]
    pub fn first_version(&self) -> Option<&VersionInfo> {""", """    /// Returns the initial version of this entity when it was first created.
    ///
    /// Retrieves the first state of the entity. This is useful for provenance tracking
    /// to see how and when an entity originated.
    /// Returns `None` if the history is empty.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use aletheiadb::core::history::{EntityHistory, VersionInfo};
    /// use aletheiadb::core::temporal::{BiTemporalInterval, time};
    /// use aletheiadb::core::{VersionId, PropertyMap};
    ///
    /// let now = time::now();
    /// let v1 = VersionInfo {
    ///     version_number: 1,
    ///     version_id: VersionId::new(1).unwrap(),
    ///     temporal: BiTemporalInterval::current(now),
    ///     properties: PropertyMap::new(),
    ///     label: "Person".to_string(),
    /// };
    /// let history = EntityHistory { versions: vec![v1] };
    /// assert_eq!(history.first_version().unwrap().version_number, 1);
    /// ```
    #[must_use]
    pub fn first_version(&self) -> Option<&VersionInfo> {""")

# We only do this replacement if it hasn't been done already by git merge diff
if "Get the total number of property changes" in content:
    content = content.replace("""    /// Get the total number of property changes
    #[must_use]
    pub fn change_count(&self) -> usize {
        self.added.len() + self.removed.len() + self.modified.len()
    }""", """    /// Calculates the total number of property modifications between the two versions.
    ///
    /// This aggregates the counts of added, removed, and modified properties.
    /// It is useful for quickly assessing the "size" or impact of a version update
    /// without needing to inspect the specific properties that changed.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use aletheiadb::core::history::VersionDiff;
    /// use aletheiadb::core::{PropertyMapBuilder, VersionId};
    ///
    /// let v1_props = PropertyMapBuilder::new().insert("name", "Alice").build();
    /// let v2_props = PropertyMapBuilder::new().insert("name", "Alice").insert("age", 30).build();
    ///
    /// let diff = VersionDiff::compute(&v1_props, &v2_props, VersionId::new(1).unwrap(), VersionId::new(2).unwrap());
    /// assert_eq!(diff.change_count(), 1); // 1 property added
    /// ```
    #[must_use]
    pub fn change_count(&self) -> usize {
        self.added.len() + self.removed.len() + self.modified.len()
    }""")

    content = content.replace("""    /// Get the total number of property changes
    #[must_use]
    pub fn change_count(&self) -> usize {
        self.properties_added + self.properties_removed + self.properties_modified
    }""", """    /// Calculates the total number of property modifications recorded in this summary.
    ///
    /// This aggregates the counts of added, removed, and modified properties.
    /// It is useful for quickly assessing the impact of a version update
    /// when you only have summary information (e.g., from an index or metadata scan)
    /// without the full `VersionDiff`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use aletheiadb::core::history::VersionSummary;
    /// use aletheiadb::core::temporal::Timestamp;
    /// use aletheiadb::core::VersionId;
    ///
    /// let summary = VersionSummary {
    ///     version_id: VersionId::new(1).unwrap(),
    ///     version_number: 1,
    ///     valid_from: Timestamp::new(100, 0).unwrap(),
    ///     transaction_time: Timestamp::new(200, 0).unwrap(),
    ///     properties_added: 2,
    ///     properties_removed: 0,
    ///     properties_modified: 1,
    /// };
    /// assert_eq!(summary.change_count(), 3);
    /// ```
    #[must_use]
    pub fn change_count(&self) -> usize {
        self.properties_added + self.properties_removed + self.properties_modified
    }""")

with open("src/core/history.rs", "w") as f:
    f.write(content)
