with open("src/query/plan.rs", "r") as f:
    content = f.read()

target = """    pub fn between(range: TimeRange) -> Self {"""
replacement = """    /// Filter nodes or edges whose valid time overlaps with the given `TimeRange`.
    ///
    /// # The Details
    /// Temporal data in AletheiaDB uses a bi-temporal model. This constructor explicitly
    /// creates a `ValidTime(Overlap(...))` filter for checking if a graph element's valid
    /// time intersects with the specified time range.
    ///
    /// # Examples
    /// ```rust
    /// use aletheiadb::query::plan::TemporalFilter;
    /// use aletheiadb::core::temporal::TimeRange;
    ///
    /// let range = TimeRange::new(100, 200).unwrap();
    /// let filter = TemporalFilter::between(range);
    /// // The filter can now be applied to queries.
    /// ```
    pub fn between(range: TimeRange) -> Self {"""

content = content.replace(target, replacement)

with open("src/query/plan.rs", "w") as f:
    f.write(content)
