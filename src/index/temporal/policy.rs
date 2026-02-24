/// Policy for handling duplicate versions during batch insertion.
///
/// Duplicate versions are identified by their `VersionId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeduplicationPolicy {
    /// Keep the first occurrence after sorting by start time (default).
    /// This corresponds to the version with the earliest start time.
    /// Correct for idempotent WAL replay.
    #[default]
    FirstOccurrence,

    /// Keep the last occurrence after sorting by start time.
    /// Use when later data should override earlier data with same version ID.
    LastOccurrence,

    /// Reject duplicates with an error.
    /// Use when duplicates indicate a bug or data corruption.
    Reject,
}
