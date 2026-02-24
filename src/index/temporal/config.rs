/// Configuration for temporal indexes.
#[derive(Debug, Clone)]
pub struct TemporalIndexConfig {
    /// Maximum versions allowed per entity (default: 1,000,000).
    /// Prevents OOM attacks from malicious or buggy clients creating
    /// unbounded version histories.
    pub max_versions_per_entity: usize,
}

impl Default for TemporalIndexConfig {
    fn default() -> Self {
        Self {
            max_versions_per_entity: 1_000_000,
        }
    }
}
