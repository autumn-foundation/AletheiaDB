use super::types::VectorDimension;

// ============================================================================
// Constants
// ============================================================================

/// Default tolerance for floating-point comparisons in normalization operations.
///
/// This tolerance (1e-6) is appropriate for most f32 operations where accumulated
/// floating-point errors are expected to be small. It's used as the default for
/// functions like [`crate::core::vector::is_normalized`] when checking if a vector has unit magnitude.
///
/// For stricter or looser comparisons, functions accept an explicit tolerance parameter.
pub const NORMALIZATION_TOLERANCE: f32 = 1e-6;

/// Squared magnitude threshold for detecting near-zero vectors.
///
/// Vectors with squared magnitude below this threshold are treated as zero vectors
/// in normalization operations. This prevents numerical instability from denormal
/// numbers and avoids division by very small values that could cause overflow.
///
/// Value: 1e-30 corresponds to magnitude ≈ 1e-15, allowing normalization of
/// small but valid vectors (e.g., 1e-15 components). This is well above the
/// smallest normal f32 value (1.17e-38) and provides sufficient headroom
/// before denormal range.
pub(crate) const SQUARED_MAGNITUDE_THRESHOLD: f32 = 1e-30;

/// Maximum allowed vector dimension.
///
/// This limit (100,000) far exceeds typical embedding sizes and exists to prevent
/// DoS attacks via memory exhaustion during deserialization.
pub const MAX_VECTOR_DIMENSIONS: usize = 100_000;

/// Type tag for Vector (dense f32 array) value.
pub const TAG_VECTOR: u8 = 7;

/// Type tag for SparseVector value.
pub const TAG_SPARSE_VECTOR: u8 = 8;

/// Maximum allowed vector dimension (typed).
///
/// Re-exported from [`MAX_VECTOR_DIMENSIONS`] for convenience.
pub const MAX_DIMENSION: VectorDimension = VectorDimension::new(MAX_VECTOR_DIMENSIONS);
