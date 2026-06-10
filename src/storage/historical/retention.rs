//! Historical retention policy.

use super::{DEFAULT_MAX_VERSIONS_PER_ENTITY, DEFAULT_MAX_VERSION_AGE_MS};

/// Configuration for historical retention policy.
///
/// Limits the growth of the temporal history by pruning old versions based on
/// count or age thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Maximum number of versions to keep per entity
    pub max_versions_per_entity: usize,
    /// Maximum age of versions in milliseconds (older versions are pruned)
    pub max_age_ms: i64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        RetentionPolicy {
            max_versions_per_entity: DEFAULT_MAX_VERSIONS_PER_ENTITY,
            max_age_ms: DEFAULT_MAX_VERSION_AGE_MS,
        }
    }
}

impl RetentionPolicy {
    /// Create a new retention policy with custom limits
    pub fn new(max_versions_per_entity: usize, max_age_ms: i64) -> Self {
        RetentionPolicy {
            max_versions_per_entity,
            max_age_ms,
        }
    }

    /// Create a policy with no retention limits (unbounded)
    pub fn unbounded() -> Self {
        RetentionPolicy {
            max_versions_per_entity: usize::MAX,
            max_age_ms: i64::MAX,
        }
    }
}
