use super::sparse::SparseVec;
use crate::core::vector::constants::SQUARED_MAGNITUDE_THRESHOLD;

#[test]
fn test_normalize_basic() {
    // 3-4-5 triangle
    let sv = SparseVec::new(vec![0, 2], vec![3.0, 4.0], 5).unwrap();
    let normalized = sv.normalize();

    assert_eq!(normalized.indices(), &[0, 2]);
    assert!((normalized.values()[0] - 0.6).abs() < 1e-6);
    assert!((normalized.values()[1] - 0.8).abs() < 1e-6);
    assert!((normalized.magnitude() - 1.0).abs() < 1e-6);
}

#[test]
fn test_normalize_zero_vector() {
    // Zero vector (empty)
    let sv = SparseVec::new(vec![], vec![], 5).unwrap();
    let normalized = sv.normalize();

    assert!(normalized.indices().is_empty());
    assert_eq!(normalized.magnitude(), 0.0);
}

#[test]
fn test_normalize_near_zero_vector() {
    // Vector with very small magnitude should be treated as zero
    let tiny_val = (SQUARED_MAGNITUDE_THRESHOLD / 2.0).sqrt() / 2.0;
    let sv = SparseVec::new(vec![0], vec![tiny_val], 5).unwrap();
    let normalized = sv.normalize();

    assert!(normalized.indices().is_empty());
    assert_eq!(normalized.magnitude(), 0.0);
}

#[test]
fn test_normalize_underflow_filtering() {
    // Construct a vector where one value will underflow after normalization.
    // We must ensure 'large' squared does not overflow f32::MAX (approx 3.4e38).
    // sqrt(3.4e38) ≈ 1.8e19.
    // So we use 1e15 to be safe (square is 1e30).

    let large = 1e15;
    // We want tiny / large < f32::MIN_POSITIVE (~1.17e-38).
    // tiny < 1e15 * 1.17e-38 ≈ 1.17e-23.
    // Let's pick 1e-25.
    // 1e-25 / 1e15 = 1e-40.
    // 1e-40 is clearly subnormal/zero compared to normalized scale.
    let tiny = 1e-25;

    let sv = SparseVec::new(vec![0, 1], vec![large, tiny], 5).unwrap();
    let normalized = sv.normalize();

    // The tiny value should be filtered out
    assert_eq!(normalized.indices(), &[0]);
    assert!((normalized.values()[0] - 1.0).abs() < 1e-6);
    assert_eq!(normalized.nnz(), 1);
}

#[test]
fn test_normalize_preserves_dimension() {
    let sv = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
    let normalized = sv.normalize();
    assert_eq!(normalized.dimension(), 100);
}
