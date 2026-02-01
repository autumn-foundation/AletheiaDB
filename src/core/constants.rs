//! Global constants and resource limits.

/// Maximum number of results to return in a single query.
/// This prevents excessive memory usage and response sizes.
pub const MAX_RESULT_LIMIT: usize = 10_000;

/// Maximum traversal depth to prevent stack overflow and excessive computation.
pub const MAX_TRAVERSAL_DEPTH: usize = 10;

/// Maximum pagination offset to prevent excessive memory usage.
/// Since many queries fetch limit+offset rows and then skip, this limits total rows fetched.
pub const MAX_PAGINATION_OFFSET: usize = 10_000;
