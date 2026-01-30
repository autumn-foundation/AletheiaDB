use crate::core::property::PropertyMap;
use crate::core::temporal::Timestamp;
use crate::utils::error::Result;
use std::sync::Arc;

/// Default maximum number of versions per entity (DoS protection)
pub const DEFAULT_MAX_VERSIONS_PER_ENTITY: usize = 1_000;

/// Default maximum age for versions in milliseconds (365 days)
pub const DEFAULT_MAX_VERSION_AGE_MS: i64 = 365 * 24 * 60 * 60 * 1000;

/// Maximum recursion depth for version reconstruction (DoS protection).
///
/// This limit prevents stack overflow from corrupted version chains or cycles.
/// A depth of 100 is sufficient for any legitimate use case since anchors are
/// typically created every 10-20 versions.
pub const MAX_RECONSTRUCTION_DEPTH: usize = 100;

/// Retention policy for version history (DoS protection).
///
/// Controls how many versions are kept and for how long to prevent
/// unbounded memory growth from malicious or buggy clients.
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

/// Pre-anchor hook for creating snapshots before anchor storage.
///
/// This hook is called **before** storing an anchor to create synchronized snapshots
/// and return a snapshot ID to be stored atomically with the anchor. This enables
/// strong consistency for provenance tracking between graph versioning and vector indexing.
///
/// # Arguments
///
/// * `entity_type` - Type of entity ("node" or "edge")
/// * `entity_id` - ID of the entity as u64
/// * `timestamp` - Transaction timestamp when the anchor is being created
/// * `properties` - Property map that will be stored in the anchor
///
/// # Returns
///
/// * `Ok(Some(snapshot_id))` - Snapshot created successfully, link to anchor
/// * `Ok(None)` - No snapshot needed or empty index
/// * `Err(e)` - Snapshot creation failed (anchor will still be created, graceful degradation)
///
/// # Examples
///
/// ```ignore
/// use gallifreydb::storage::historical::PreAnchorHook;
/// use std::sync::Arc;
///
/// let hook: PreAnchorHook = Arc::new(|entity_type, entity_id, timestamp, properties| {
///     // Create vector snapshot before anchor is stored
///     let snapshot_id = create_vector_snapshot(timestamp, properties)?;
///     Ok(Some(snapshot_id))
/// });
/// ```
pub type PreAnchorHook = Arc<
    dyn Fn(
            /* entity_type */ &str,
            /* entity_id */ u64,
            /* timestamp */ Timestamp,
            /* properties */ &PropertyMap,
        ) -> Result<Option<usize>>
        + Send
        + Sync,
>;

/// Context for pre-anchor hook invocation.
///
/// Groups the parameters needed to invoke a pre-anchor hook, improving
/// readability and reducing the number of function parameters.
pub(crate) struct AnchorHookContext<'a> {
    /// Entity type ("node" or "edge")
    pub(crate) entity_type: &'a str,
    /// Entity ID as u64
    pub(crate) entity_id: u64,
    /// Transaction timestamp
    pub(crate) timestamp: Timestamp,
    /// Properties being stored in the anchor
    pub(crate) properties: &'a PropertyMap,
}

/// Default cache size for reconstructed properties (10,000 entries)
pub const DEFAULT_RECONSTRUCTION_CACHE_SIZE: usize = 10_000;

/// Anchor cache size ratio relative to main cache (Improvement #1: Issue #338).
///
/// Typically 10-20% of versions become anchors depending on `anchor_interval`.
/// With default interval of 10, we get ~10% anchors. Setting to 1/5 (20%)
/// provides headroom for configurations with smaller intervals.
pub const ANCHOR_CACHE_SIZE_RATIO: usize = 5; // 20% of main cache

/// Minimum anchor cache size to ensure reasonable performance (Improvement #1: Issue #338).
///
/// Even with very small main caches, we want enough anchor cache to hold
/// at least a few anchors to avoid immediate evictions.
pub const MIN_ANCHOR_CACHE_SIZE: usize = 100;
