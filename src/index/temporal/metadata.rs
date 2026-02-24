use crate::core::id::VersionId;

/// Index into the consolidated version metadata storage.
///
/// This type represents a reference to version metadata stored centrally
/// in `EntityTimelines`, eliminating duplication between valid-time and
/// transaction-time indexes. Using `u32` saves 4 bytes per `TimelineEntry`
/// compared to storing `VersionId` (u64) directly.
///
/// # Memory Layout (Issue #196)
///
/// Previously, `VersionId` was stored in both valid and tx timelines,
/// causing 8 bytes of duplication per version. With consolidated storage:
/// - Timeline entries store a 4-byte index instead of 8-byte `VersionId`
/// - Version metadata is stored once in a central `Vec<TimelineVersionMetadata>`
///
/// # Key Benefit
///
/// While the net memory savings for VersionId alone is minimal, this architecture
/// enables storing additional metadata (BiTemporalInterval, provenance, etc.)
/// without proportional cost increase per timeline entry. Each additional field
/// in `TimelineVersionMetadata` is stored once, not twice.
pub type VersionMetadataIndex = u32;

/// Consolidated version metadata storage.
///
/// Stores version information in a single authoritative location,
/// eliminating duplication between valid-time and transaction-time indexes.
/// Both timelines reference this metadata via `VersionMetadataIndex`.
///
/// # Size
///
/// Current size: 8 bytes (just `VersionId`).
/// This can grow without affecting `TimelineEntry` size.
///
/// # Future Extensions
///
/// This structure can be extended to include additional metadata without
/// increasing storage per timeline entry:
/// - Entity ID (for cross-entity queries)
/// - BiTemporalInterval (for interval queries, see Issue #194)
/// - Provenance/audit information
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineVersionMetadata {
    /// The unique identifier for this version.
    version_id: VersionId,
}

impl TimelineVersionMetadata {
    /// Create new version metadata.
    #[inline]
    pub const fn new(version_id: VersionId) -> Self {
        Self { version_id }
    }

    /// Get the version ID.
    #[inline]
    pub const fn version_id(&self) -> VersionId {
        self.version_id
    }
}

use smallvec::SmallVec;

/// Optimization: Use SmallVec for index lists to avoid heap allocations for common small queries.
/// 16 * 4 bytes = 64 bytes, fits well on stack.
pub type IndexVec = SmallVec<[VersionMetadataIndex; 16]>;
