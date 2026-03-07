use proptest::prelude::*;

// Helper to generate two vectors of the same random length
prop_compose! {
    fn same_length_vectors()(
        len in 1..1000usize
    )(
        a in prop::collection::vec(any::<f32>(), len..=len),
        b in prop::collection::vec(any::<f32>(), len..=len)
    ) -> (Vec<f32>, Vec<f32>) {
        (a, b)
    }
}

proptest! {
    #[test]
    fn prop_simd_ops_garbage(
        (a_data, b_data) in same_length_vectors()
    ) {
        let a = a_data.as_slice();
        let b = b_data.as_slice();

        // Directly call ops logic without panic
        let _ = super::ops::dot_product(a, b);
        let _ = super::ops::cosine_similarity(a, b);
        let _ = super::ops::euclidean_distance(a, b);
        let _ = super::ops::squared_euclidean_distance(a, b);
        let _ = super::ops::squared_magnitude(a);
        let _ = super::ops::magnitude(a);
        let _ = super::ops::normalize(a);
        let mut a_clone = a.to_vec();
        super::ops::normalize_in_place(&mut a_clone);
    }
}

#[test]
fn test_havoc_simd_normalization_bug_detailed() {
    let mut data = vec![1e-15_f32; 100];
    super::ops::normalize_in_place(&mut data);

    // 1e-15 is a valid float, but its square is 1e-30.
    // 100 components -> magnitude^2 is 1e-28.
    // The threshold SQUARED_MAGNITUDE_THRESHOLD is 1e-30.
    // The expected behavior is that it normalizes to a valid unit vector.
    // Each component should be 1/sqrt(100) = 1/10 = 0.1
    let expected = 0.1_f32;
    assert!((data[0] - expected).abs() < 1e-6, "Expected valid normalization, got {}", data[0]);
}

#[test]
fn test_havoc_simd_nan_propagation() {
    let mut data = vec![f32::NAN; 100];
    super::ops::normalize_in_place(&mut data);
    assert!(data[0].is_nan());
}

#[test]
fn test_havoc_simd_infinity_propagation() {
    let mut data = vec![f32::INFINITY; 100];
    super::ops::normalize_in_place(&mut data);
    assert!(data[0].is_nan());
}
