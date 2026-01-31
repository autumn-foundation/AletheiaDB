pub use super::*;
pub use crate::core::property::MAX_VECTOR_DIMENSIONS;
pub use crate::utils::error::{Error, VectorError};
mod unit_tests {
    use super::*;

    #[test]
    fn test_vector_dimension_new() {
        let dim = VectorDimension::new(1536);
        assert_eq!(dim.as_usize(), 1536);
    }

    #[test]
    fn test_vector_dimension_from_usize() {
        let dim: VectorDimension = 384.into();
        assert_eq!(dim.as_usize(), 384);
    }

    #[test]
    fn test_vector_dimension_into_usize() {
        let dim = VectorDimension::new(768);
        let size: usize = dim.into();
        assert_eq!(size, 768);
    }

    #[test]
    fn test_max_dimension_constant() {
        assert_eq!(MAX_DIMENSION.as_usize(), 100_000);
        assert_eq!(MAX_DIMENSION.as_usize(), MAX_VECTOR_DIMENSIONS);
    }

    #[test]
    fn test_dimension_comparison() {
        let small = VectorDimension::new(384);
        let large = VectorDimension::new(1536);

        assert!(small < large);
        assert!(large <= MAX_DIMENSION);
    }

    #[test]
    fn test_dimension_equality() {
        let dim1 = VectorDimension::new(512);
        let dim2 = VectorDimension::new(512);
        let dim3 = VectorDimension::new(1024);

        assert_eq!(dim1, dim2);
        assert_ne!(dim1, dim3);
    }

    #[test]
    fn test_dimension_display() {
        let dim = VectorDimension::new(1536);
        assert_eq!(format!("{}", dim), "1536");
    }

    #[test]
    fn test_dimension_debug() {
        let dim = VectorDimension::new(384);
        assert_eq!(format!("{:?}", dim), "VectorDimension(384)");
    }

    #[test]
    fn test_is_zero() {
        assert!(VectorDimension::new(0).is_zero());
        assert!(!VectorDimension::new(1).is_zero());
    }

    #[test]
    fn test_exceeds_max() {
        assert!(!VectorDimension::new(1000).exceeds_max());
        assert!(!MAX_DIMENSION.exceeds_max());
        assert!(VectorDimension::new(100_001).exceeds_max());
    }

    #[test]
    fn test_default() {
        let dim = VectorDimension::default();
        assert_eq!(dim.as_usize(), 0);
    }

    #[test]
    fn test_copy_semantics() {
        let dim1 = VectorDimension::new(256);
        let dim2 = dim1; // Copy, not move
        assert_eq!(dim1, dim2); // Both still valid
    }

    #[test]
    fn test_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(VectorDimension::new(384));
        set.insert(VectorDimension::new(768));
        set.insert(VectorDimension::new(384)); // Duplicate

        assert_eq!(set.len(), 2);
    }

    // ========================================================================
    // Cosine Similarity Tests
    // ========================================================================

    #[test]
    fn test_cosine_similarity_identical_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(
            (sim - 1.0).abs() < 1e-6,
            "Identical vectors should have similarity 1.0"
        );
    }

    #[test]
    fn test_cosine_similarity_opposite_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![-1.0, -2.0, -3.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(
            (sim + 1.0).abs() < 1e-6,
            "Opposite vectors should have similarity -1.0"
        );
    }

    #[test]
    fn test_cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(
            sim.abs() < 1e-6,
            "Orthogonal vectors should have similarity 0.0"
        );
    }

    #[test]
    fn test_cosine_similarity_3d_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(sim.abs() < 1e-6);

        let c = vec![0.0, 0.0, 1.0];
        let sim_ac = cosine_similarity(&a, &c).unwrap();
        assert!(sim_ac.abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_known_angle() {
        // 45 degree angle: cos(45°) ≈ 0.7071
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 1.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        let expected = 1.0 / 2.0_f32.sqrt(); // cos(45°)
        assert!((sim - expected).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_dimension_mismatch() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];
        let result = cosine_similarity(&a, &b);
        assert!(result.is_err());
    }

    #[test]
    fn test_cosine_similarity_empty_vectors() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert_eq!(sim, 0.0, "Zero vector should result in similarity 0.0");
    }

    #[test]
    fn test_cosine_similarity_both_zero_vectors() {
        let a = vec![0.0, 0.0];
        let b = vec![0.0, 0.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_cosine_similarity_unit_vectors() {
        // Unit vectors should give same result as non-unit
        let a = vec![3.0, 4.0]; // magnitude 5
        let b = vec![4.0, 3.0]; // magnitude 5
        let sim1 = cosine_similarity(&a, &b).unwrap();

        // Normalize manually
        let a_norm = vec![3.0 / 5.0, 4.0 / 5.0];
        let b_norm = vec![4.0 / 5.0, 3.0 / 5.0];
        let sim2 = cosine_similarity(&a_norm, &b_norm).unwrap();

        assert!((sim1 - sim2).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_negative_values() {
        let a = vec![-1.0, -2.0, 3.0];
        let b = vec![1.0, -2.0, -3.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        // dot = -1 + 4 - 9 = -6
        // mag_a = sqrt(1 + 4 + 9) = sqrt(14)
        // mag_b = sqrt(1 + 4 + 9) = sqrt(14)
        // sim = -6 / 14 ≈ -0.4286
        let expected = -6.0 / 14.0;
        assert!((sim - expected).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_single_element() {
        let a = vec![5.0];
        let b = vec![3.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(
            (sim - 1.0).abs() < 1e-6,
            "Parallel 1D vectors should have similarity 1.0"
        );

        let c = vec![-3.0];
        let sim_neg = cosine_similarity(&a, &c).unwrap();
        assert!(
            (sim_neg + 1.0).abs() < 1e-6,
            "Anti-parallel 1D vectors should have similarity -1.0"
        );
    }

    #[test]
    fn test_cosine_similarity_symmetry() {
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![4.0, 3.0, 2.0, 1.0];
        let sim_ab = cosine_similarity(&a, &b).unwrap();
        let sim_ba = cosine_similarity(&b, &a).unwrap();
        assert!(
            (sim_ab - sim_ba).abs() < 1e-6,
            "Cosine similarity should be symmetric"
        );
    }

    #[test]
    fn test_cosine_similarity_range() {
        // Various test cases to ensure result is always in [-1, 1]
        let test_cases = vec![
            (vec![1.0, 0.0], vec![1.0, 0.0]),
            (vec![1.0, 0.0], vec![-1.0, 0.0]),
            (vec![1.0, 0.0], vec![0.0, 1.0]),
            (vec![1.0, 1.0, 1.0], vec![2.0, 3.0, 4.0]),
            (vec![-1.0, -2.0], vec![3.0, 4.0]),
        ];

        for (a, b) in test_cases {
            let sim = cosine_similarity(&a, &b).unwrap();
            assert!(
                (-1.0..=1.0).contains(&sim),
                "Similarity {} is out of range [-1, 1] for vectors {:?} and {:?}",
                sim,
                a,
                b
            );
        }
    }

    // ========================================================================
    // Large Dimension Tests
    // ========================================================================

    #[test]
    fn test_cosine_similarity_large_dimension_1536() {
        // OpenAI text-embedding-3-small dimension
        let dim = 1536;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32).sin()).collect();
        let b: Vec<f32> = (0..dim).map(|i| (i as f32).cos()).collect();

        let sim = cosine_similarity(&a, &b).unwrap();
        // Sine and cosine are approximately orthogonal over many periods
        assert!(
            sim.abs() < 0.1,
            "Expected near-orthogonal vectors at dim={}, got sim={}",
            dim,
            sim
        );
    }

    #[test]
    fn test_cosine_similarity_large_dimension_3072() {
        // OpenAI text-embedding-3-large dimension
        let dim = 3072;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.1).sin()).collect();
        let b: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.1).sin()).collect();

        let sim = cosine_similarity(&a, &b).unwrap();
        // Identical vectors should have similarity 1.0
        assert!(
            (sim - 1.0).abs() < 1e-5,
            "Expected self-similarity of 1.0 at dim={}, got {}",
            dim,
            sim
        );
    }

    #[test]
    fn test_cosine_similarity_large_dimension_opposite() {
        let dim = 1536;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.01).cos()).collect();
        let b: Vec<f32> = a.iter().map(|x| -x).collect();

        let sim = cosine_similarity(&a, &b).unwrap();
        // Opposite vectors should have similarity -1.0
        assert!(
            (sim + 1.0).abs() < 1e-5,
            "Expected opposite similarity of -1.0 at dim={}, got {}",
            dim,
            sim
        );
    }

    // ========================================================================
    // NaN/Inf Propagation Tests
    // ========================================================================

    #[test]
    fn test_cosine_similarity_nan_propagation() {
        let a = vec![f32::NAN, 1.0];
        let b = vec![1.0, 1.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(sim.is_nan(), "NaN in input should propagate to output");
    }

    #[test]
    fn test_cosine_similarity_nan_in_second_vector() {
        let a = vec![1.0, 1.0];
        let b = vec![1.0, f32::NAN];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(
            sim.is_nan(),
            "NaN in second vector should propagate to output"
        );
    }

    #[test]
    fn test_cosine_similarity_inf_propagation() {
        let a = vec![f32::INFINITY, 1.0];
        let b = vec![1.0, 1.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        // Inf * finite = Inf, and the computation will result in Inf/Inf = NaN
        // or a valid number depending on the exact math
        assert!(
            sim.is_nan() || sim.is_infinite() || (-1.0..=1.0).contains(&sim),
            "Inf should propagate in some form, got {}",
            sim
        );
    }

    #[test]
    fn test_cosine_similarity_neg_inf_propagation() {
        let a = vec![f32::NEG_INFINITY, 1.0];
        let b = vec![1.0, 1.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        assert!(
            sim.is_nan() || sim.is_infinite() || (-1.0..=1.0).contains(&sim),
            "Negative Inf should propagate in some form, got {}",
            sim
        );
    }

    #[test]
    fn test_cosine_similarity_both_inf_same_sign() {
        let a = vec![f32::INFINITY, 0.0];
        let b = vec![f32::INFINITY, 0.0];
        let sim = cosine_similarity(&a, &b).unwrap();
        // Inf * Inf = Inf, sqrt(Inf * Inf) = Inf, Inf/Inf = NaN
        assert!(sim.is_nan(), "Inf/Inf should be NaN, got {}", sim);
    }

    // ========================================================================
    // Normalized Cosine Similarity Tests
    // ========================================================================
    //
    // Note: These tests use the public `normalize` function imported via super::*

    #[test]
    fn test_cosine_similarity_normalized_identical() {
        let a = normalize(&[1.0, 2.0, 3.0]);
        let sim = cosine_similarity_normalized(&a, &a).unwrap();
        assert!((sim - 1.0).abs() < 1e-6, "Self-similarity should be 1.0");
    }

    #[test]
    fn test_cosine_similarity_normalized_opposite() {
        let a = normalize(&[1.0, 0.0]);
        let b = normalize(&[-1.0, 0.0]);
        let sim = cosine_similarity_normalized(&a, &b).unwrap();
        assert!(
            (sim + 1.0).abs() < 1e-6,
            "Opposite vectors should have similarity -1.0"
        );
    }

    #[test]
    fn test_cosine_similarity_normalized_orthogonal() {
        let a = vec![1.0, 0.0, 0.0]; // Already unit
        let b = vec![0.0, 1.0, 0.0]; // Already unit
        let sim = cosine_similarity_normalized(&a, &b).unwrap();
        assert!(
            sim.abs() < 1e-6,
            "Orthogonal unit vectors should have similarity 0.0"
        );
    }

    #[test]
    fn test_cosine_similarity_normalized_45_degrees() {
        // cos(45°) = 1/sqrt(2) ≈ 0.707
        let a = vec![1.0, 0.0];
        let b = normalize(&[1.0, 1.0]);
        let sim = cosine_similarity_normalized(&a, &b).unwrap();
        let expected = 1.0 / 2.0_f32.sqrt();
        assert!(
            (sim - expected).abs() < 1e-5,
            "Expected {}, got {}",
            expected,
            sim
        );
    }

    #[test]
    fn test_cosine_similarity_normalized_matches_general() {
        let a_raw = vec![1.0, 2.0, 3.0, 4.0];
        let b_raw = vec![4.0, 3.0, 2.0, 1.0];

        // General cosine similarity
        let sim_general = cosine_similarity(&a_raw, &b_raw).unwrap();

        // Normalized version
        let a_norm = normalize(&a_raw);
        let b_norm = normalize(&b_raw);
        let sim_normalized = cosine_similarity_normalized(&a_norm, &b_norm).unwrap();

        assert!(
            (sim_general - sim_normalized).abs() < 1e-5,
            "General ({}) and normalized ({}) should match",
            sim_general,
            sim_normalized
        );
    }

    #[test]
    fn test_cosine_similarity_normalized_dimension_mismatch() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0];
        let result = cosine_similarity_normalized(&a, &b);
        assert!(result.is_err());
    }

    #[test]
    fn test_cosine_similarity_normalized_empty() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        let sim = cosine_similarity_normalized(&a, &b).unwrap();
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_cosine_similarity_normalized_high_dimension() {
        // Test with a higher dimension to exercise SIMD paths
        let dim = 384; // Sentence Transformers dimension
        let a: Vec<f32> = (0..dim).map(|i| (i as f32) / dim as f32).collect();
        let b: Vec<f32> = (0..dim).map(|i| ((dim - i) as f32) / dim as f32).collect();

        let a_norm = normalize(&a);
        let b_norm = normalize(&b);

        let sim = cosine_similarity_normalized(&a_norm, &b_norm).unwrap();
        let sim_general = cosine_similarity(&a, &b).unwrap();

        assert!(
            (sim - sim_general).abs() < 1e-4,
            "High-dim: general ({}) vs normalized ({})",
            sim_general,
            sim
        );
    }

    // ========================================================================
    // Euclidean Distance Tests
    // ========================================================================

    #[test]
    fn test_euclidean_distance_3_4_5_triangle() {
        // Classic 3-4-5 right triangle
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];
        let dist = euclidean_distance(&a, &b).unwrap();
        assert!(
            (dist - 5.0).abs() < 1e-6,
            "3-4-5 triangle distance should be 5.0, got {}",
            dist
        );
    }

    #[test]
    fn test_squared_euclidean_distance_3_4_5_triangle() {
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];
        let dist_sq = squared_euclidean_distance(&a, &b).unwrap();
        assert!(
            (dist_sq - 25.0).abs() < 1e-6,
            "3² + 4² should be 25, got {}",
            dist_sq
        );
    }

    #[test]
    fn test_euclidean_distance_same_point() {
        let a = vec![1.0, 2.0, 3.0];
        let dist = euclidean_distance(&a, &a).unwrap();
        assert!(
            dist.abs() < 1e-6,
            "Distance to self should be 0, got {}",
            dist
        );
    }

    #[test]
    fn test_squared_euclidean_distance_same_point() {
        let a = vec![1.0, 2.0, 3.0];
        let dist_sq = squared_euclidean_distance(&a, &a).unwrap();
        assert!(
            dist_sq.abs() < 1e-6,
            "Squared distance to self should be 0, got {}",
            dist_sq
        );
    }

    #[test]
    fn test_euclidean_distance_dimension_mismatch() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];
        let result = euclidean_distance(&a, &b);
        assert!(result.is_err());
    }

    #[test]
    fn test_squared_euclidean_distance_dimension_mismatch() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];
        let result = squared_euclidean_distance(&a, &b);
        assert!(result.is_err());
    }

    #[test]
    fn test_euclidean_distance_empty_vectors() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        let dist = euclidean_distance(&a, &b).unwrap();
        assert_eq!(dist, 0.0);
    }

    #[test]
    fn test_squared_euclidean_distance_empty_vectors() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        let dist_sq = squared_euclidean_distance(&a, &b).unwrap();
        assert_eq!(dist_sq, 0.0);
    }

    #[test]
    fn test_euclidean_distance_symmetry() {
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![4.0, 3.0, 2.0, 1.0];
        let dist_ab = euclidean_distance(&a, &b).unwrap();
        let dist_ba = euclidean_distance(&b, &a).unwrap();
        assert!(
            (dist_ab - dist_ba).abs() < 1e-6,
            "Euclidean distance should be symmetric"
        );
    }

    #[test]
    fn test_squared_euclidean_distance_symmetry() {
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![4.0, 3.0, 2.0, 1.0];
        let dist_sq_ab = squared_euclidean_distance(&a, &b).unwrap();
        let dist_sq_ba = squared_euclidean_distance(&b, &a).unwrap();
        assert!(
            (dist_sq_ab - dist_sq_ba).abs() < 1e-6,
            "Squared Euclidean distance should be symmetric"
        );
    }

    #[test]
    fn test_euclidean_distance_single_dimension() {
        let a = vec![5.0];
        let b = vec![2.0];
        let dist = euclidean_distance(&a, &b).unwrap();
        assert!(
            (dist - 3.0).abs() < 1e-6,
            "1D distance should be |5 - 2| = 3, got {}",
            dist
        );
    }

    #[test]
    fn test_euclidean_distance_negative_values() {
        let a = vec![-1.0, -2.0];
        let b = vec![2.0, 2.0];
        let dist = euclidean_distance(&a, &b).unwrap();
        // sqrt((2 - -1)² + (2 - -2)²) = sqrt(9 + 16) = 5
        assert!(
            (dist - 5.0).abs() < 1e-6,
            "Distance with negative values should be 5.0, got {}",
            dist
        );
    }

    #[test]
    fn test_euclidean_distance_3d() {
        // Distance from (0,0,0) to (1,2,2)
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 2.0];
        let dist = euclidean_distance(&a, &b).unwrap();
        // sqrt(1 + 4 + 4) = sqrt(9) = 3
        assert!(
            (dist - 3.0).abs() < 1e-6,
            "3D distance should be 3.0, got {}",
            dist
        );
    }

    #[test]
    fn test_euclidean_vs_squared_relationship() {
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let b = vec![5.0, 4.0, 3.0, 2.0, 1.0];
        let dist = euclidean_distance(&a, &b).unwrap();
        let dist_sq = squared_euclidean_distance(&a, &b).unwrap();
        assert!(
            (dist * dist - dist_sq).abs() < 1e-5,
            "euclidean² should equal squared_euclidean: {}² vs {}",
            dist,
            dist_sq
        );
    }

    #[test]
    fn test_euclidean_distance_large_dimension_384() {
        // Sentence Transformers dimension
        let dim = 384;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32).sin()).collect();
        let b: Vec<f32> = (0..dim).map(|i| (i as f32).cos()).collect();

        let dist = euclidean_distance(&a, &b).unwrap();
        let dist_sq = squared_euclidean_distance(&a, &b).unwrap();

        assert!(dist >= 0.0, "Distance should be non-negative");
        assert!(dist_sq >= 0.0, "Squared distance should be non-negative");
        assert!(
            (dist * dist - dist_sq).abs() < 1e-4,
            "Relationship should hold at high dimension"
        );
    }

    #[test]
    fn test_euclidean_distance_large_dimension_1536() {
        // OpenAI text-embedding-3-small dimension
        let dim = 1536;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.01).sin()).collect();
        let b: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.01).cos()).collect();

        let dist = euclidean_distance(&a, &b).unwrap();
        let dist_sq = squared_euclidean_distance(&a, &b).unwrap();

        assert!(dist >= 0.0, "Distance should be non-negative");
        assert!(
            (dist * dist - dist_sq).abs() < 1e-3,
            "Relationship should hold at dim={}: {}² vs {}",
            dim,
            dist,
            dist_sq
        );
    }

    #[test]
    fn test_squared_euclidean_distance_preserves_ordering() {
        // Squared distance should preserve distance ordering for comparisons
        let query = vec![0.0, 0.0];
        let near = vec![1.0, 1.0];
        let far = vec![3.0, 4.0];

        let dist_near = euclidean_distance(&query, &near).unwrap();
        let dist_far = euclidean_distance(&query, &far).unwrap();
        let dist_sq_near = squared_euclidean_distance(&query, &near).unwrap();
        let dist_sq_far = squared_euclidean_distance(&query, &far).unwrap();

        // If near < far in distance, should also be true for squared distance
        assert!(dist_near < dist_far);
        assert!(dist_sq_near < dist_sq_far);
    }

    #[test]
    fn test_euclidean_distance_unit_axis() {
        // Distance along unit axes
        let origin = vec![0.0, 0.0, 0.0];
        let x_axis = vec![1.0, 0.0, 0.0];
        let y_axis = vec![0.0, 1.0, 0.0];
        let z_axis = vec![0.0, 0.0, 1.0];

        assert!((euclidean_distance(&origin, &x_axis).unwrap() - 1.0).abs() < 1e-6);
        assert!((euclidean_distance(&origin, &y_axis).unwrap() - 1.0).abs() < 1e-6);
        assert!((euclidean_distance(&origin, &z_axis).unwrap() - 1.0).abs() < 1e-6);
    }

    // ========================================================================
    // Dot Product Tests
    // ========================================================================

    #[test]
    fn test_dot_product_basic() {
        // 1×4 + 2×5 + 3×6 = 4 + 10 + 18 = 32
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        let result = dot_product(&a, &b).unwrap();
        assert!(
            (result - 32.0).abs() < 1e-6,
            "Dot product should be 32, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_self_equals_squared_magnitude() {
        // dot(a, a) = ||a||²
        let a = vec![3.0, 4.0];
        let self_dot = dot_product(&a, &a).unwrap();
        // 3² + 4² = 9 + 16 = 25
        assert!(
            (self_dot - 25.0).abs() < 1e-6,
            "Self dot product should be 25, got {}",
            self_dot
        );
    }

    #[test]
    fn test_dot_product_orthogonal_vectors() {
        // Orthogonal vectors have dot product 0
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let result = dot_product(&a, &b).unwrap();
        assert!(
            result.abs() < 1e-6,
            "Orthogonal vectors should have dot product 0, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_orthogonal_3d() {
        let x = vec![1.0, 0.0, 0.0];
        let y = vec![0.0, 1.0, 0.0];
        let z = vec![0.0, 0.0, 1.0];

        assert!(dot_product(&x, &y).unwrap().abs() < 1e-6);
        assert!(dot_product(&y, &z).unwrap().abs() < 1e-6);
        assert!(dot_product(&x, &z).unwrap().abs() < 1e-6);
    }

    #[test]
    fn test_dot_product_symmetry() {
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![4.0, 3.0, 2.0, 1.0];
        let dot_ab = dot_product(&a, &b).unwrap();
        let dot_ba = dot_product(&b, &a).unwrap();
        assert!(
            (dot_ab - dot_ba).abs() < 1e-6,
            "Dot product should be symmetric: {} vs {}",
            dot_ab,
            dot_ba
        );
    }

    #[test]
    fn test_dot_product_dimension_mismatch() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];
        let result = dot_product(&a, &b);
        assert!(result.is_err());
    }

    #[test]
    fn test_dot_product_empty_vectors() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        let result = dot_product(&a, &b).unwrap();
        assert_eq!(result, 0.0);
    }

    #[test]
    fn test_dot_product_single_element() {
        let a = vec![5.0];
        let b = vec![3.0];
        let result = dot_product(&a, &b).unwrap();
        assert!(
            (result - 15.0).abs() < 1e-6,
            "Single element dot product should be 15, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_negative_values() {
        let a = vec![-1.0, 2.0, -3.0];
        let b = vec![4.0, -5.0, 6.0];
        // -1×4 + 2×(-5) + (-3)×6 = -4 - 10 - 18 = -32
        let result = dot_product(&a, &b).unwrap();
        assert!(
            (result + 32.0).abs() < 1e-6,
            "Dot product should be -32, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let result = dot_product(&a, &b).unwrap();
        assert!(
            result.abs() < 1e-6,
            "Dot product with zero vector should be 0, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_parallel_same_direction() {
        // Parallel vectors in same direction: dot = ||a|| × ||b||
        let a = vec![3.0, 0.0];
        let b = vec![4.0, 0.0];
        let result = dot_product(&a, &b).unwrap();
        // 3 × 4 = 12
        assert!(
            (result - 12.0).abs() < 1e-6,
            "Parallel same direction dot product should be 12, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_parallel_opposite_direction() {
        // Parallel vectors in opposite direction: dot = -||a|| × ||b||
        let a = vec![3.0, 0.0];
        let b = vec![-4.0, 0.0];
        let result = dot_product(&a, &b).unwrap();
        // 3 × (-4) = -12
        assert!(
            (result + 12.0).abs() < 1e-6,
            "Parallel opposite direction dot product should be -12, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_large_dimension_384() {
        // Sentence Transformers dimension
        let dim = 384;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32).sin()).collect();
        let b: Vec<f32> = (0..dim).map(|i| (i as f32).cos()).collect();

        let result = dot_product(&a, &b).unwrap();
        // Just verify it runs and produces a finite result
        assert!(result.is_finite(), "Dot product should be finite");
    }

    #[test]
    fn test_dot_product_large_dimension_1536() {
        // OpenAI text-embedding-3-small dimension
        let dim = 1536;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.01).sin()).collect();
        let b: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.01).cos()).collect();

        let result = dot_product(&a, &b).unwrap();
        assert!(
            result.is_finite(),
            "Large dimension dot product should be finite"
        );
    }

    #[test]
    fn test_dot_product_large_dimension_self() {
        // Self dot product at large dimension
        let dim = 1536;
        let a: Vec<f32> = (0..dim).map(|i| (i as f32 * 0.01).sin()).collect();

        let self_dot = dot_product(&a, &a).unwrap();
        // Self dot should equal sum of squares
        let expected: f32 = a.iter().map(|x| x * x).sum();
        assert!(
            (self_dot - expected).abs() < 1e-3,
            "Self dot at dim={} should equal sum of squares: {} vs {}",
            dim,
            self_dot,
            expected
        );
    }

    #[test]
    fn test_dot_product_nan_propagation() {
        let a = vec![f32::NAN, 1.0];
        let b = vec![1.0, 1.0];
        let result = dot_product(&a, &b).unwrap();
        assert!(result.is_nan(), "NaN in input should propagate to output");
    }

    #[test]
    fn test_dot_product_inf_propagation() {
        let a = vec![f32::INFINITY, 1.0];
        let b = vec![1.0, 1.0];
        let result = dot_product(&a, &b).unwrap();
        assert!(
            result.is_infinite() && result > 0.0,
            "Positive Inf should propagate, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_neg_inf_propagation() {
        let a = vec![f32::NEG_INFINITY, 1.0];
        let b = vec![1.0, 1.0];
        let result = dot_product(&a, &b).unwrap();
        assert!(
            result.is_infinite() && result < 0.0,
            "Negative Inf should propagate, got {}",
            result
        );
    }

    #[test]
    fn test_dot_product_matches_manual_calculation() {
        // Verify against manual calculation for various sizes
        for size in [1, 3, 7, 8, 15, 16, 17, 31, 32, 33, 100] {
            let a: Vec<f32> = (0..size).map(|i| i as f32).collect();
            let b: Vec<f32> = (0..size).map(|i| (size - i) as f32).collect();

            let simd_result = dot_product(&a, &b).unwrap();
            let manual_result: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();

            assert!(
                (simd_result - manual_result).abs() < 1e-4,
                "SIMD and manual should match at size {}: {} vs {}",
                size,
                simd_result,
                manual_result
            );
        }
    }

    #[test]
    fn test_dot_product_simd_boundary_cases() {
        // Test at SIMD boundaries (multiples of 4 and 8)
        for size in [4, 8, 12, 16, 24, 32, 64, 128] {
            let a: Vec<f32> = (0..size).map(|i| (i as f32 * 0.1).sin()).collect();
            let b: Vec<f32> = (0..size).map(|i| (i as f32 * 0.1).cos()).collect();

            let simd_result = dot_product(&a, &b).unwrap();
            let expected: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();

            assert!(
                (simd_result - expected).abs() < 1e-5,
                "SIMD boundary case failed at size {}: {} vs {}",
                size,
                simd_result,
                expected
            );
        }
    }

    // ========================================================================
    // Magnitude Tests
    // ========================================================================

    #[test]
    fn test_magnitude_3_4_triangle() {
        // Classic 3-4-5 right triangle
        let v = vec![3.0, 4.0];
        let mag = magnitude(&v);
        assert!(
            (mag - 5.0).abs() < 1e-6,
            "Magnitude of [3, 4] should be 5, got {}",
            mag
        );
    }

    #[test]
    fn test_magnitude_unit_vectors() {
        let unit_x = vec![1.0, 0.0, 0.0];
        let unit_y = vec![0.0, 1.0, 0.0];
        let unit_z = vec![0.0, 0.0, 1.0];

        assert!((magnitude(&unit_x) - 1.0).abs() < 1e-6);
        assert!((magnitude(&unit_y) - 1.0).abs() < 1e-6);
        assert!((magnitude(&unit_z) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_magnitude_zero_vector() {
        let v = vec![0.0, 0.0, 0.0];
        assert_eq!(magnitude(&v), 0.0);
    }

    #[test]
    fn test_magnitude_empty_vector() {
        let v: Vec<f32> = vec![];
        assert_eq!(magnitude(&v), 0.0);
    }

    #[test]
    fn test_magnitude_single_element() {
        assert!((magnitude(&[5.0]) - 5.0).abs() < 1e-6);
        assert!((magnitude(&[-5.0]) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn test_magnitude_negative_components() {
        let v = vec![-3.0, -4.0];
        assert!(
            (magnitude(&v) - 5.0).abs() < 1e-6,
            "Magnitude should be positive regardless of component signs"
        );
    }

    #[test]
    fn test_magnitude_large_dimension() {
        // Vector of all 1s with dimension n has magnitude sqrt(n)
        let n = 1536;
        let v: Vec<f32> = vec![1.0; n];
        let expected = (n as f32).sqrt();
        assert!(
            (magnitude(&v) - expected).abs() < 1e-4,
            "Magnitude of {} 1s should be sqrt({}), got {}",
            n,
            n,
            magnitude(&v)
        );
    }

    // ========================================================================
    // Squared Magnitude Tests
    // ========================================================================

    #[test]
    fn test_squared_magnitude_3_4_triangle() {
        let v = vec![3.0, 4.0];
        let sq_mag = squared_magnitude(&v);
        assert!(
            (sq_mag - 25.0).abs() < 1e-6,
            "Squared magnitude of [3, 4] should be 25, got {}",
            sq_mag
        );
    }

    #[test]
    fn test_squared_magnitude_zero_vector() {
        let v = vec![0.0, 0.0, 0.0];
        assert_eq!(squared_magnitude(&v), 0.0);
    }

    #[test]
    fn test_squared_magnitude_empty_vector() {
        let v: Vec<f32> = vec![];
        assert_eq!(squared_magnitude(&v), 0.0);
    }

    #[test]
    fn test_squared_magnitude_vs_magnitude() {
        let v = vec![1.0, 2.0, 3.0, 4.0];
        let mag = magnitude(&v);
        let sq_mag = squared_magnitude(&v);
        let diff = (mag * mag - sq_mag).abs();
        assert!(
            diff < 1e-5,
            "squared_magnitude should equal magnitude²: mag²={}, sq_mag={}, diff={}",
            mag * mag,
            sq_mag,
            diff
        );
    }

    // ========================================================================
    // Normalize Tests
    // ========================================================================

    #[test]
    fn test_normalize_3_4_triangle() {
        let v = vec![3.0, 4.0];
        let unit = normalize(&v);
        assert!((unit[0] - 0.6).abs() < 1e-6);
        assert!((unit[1] - 0.8).abs() < 1e-6);
        assert!((magnitude(&unit) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_normalize_already_unit() {
        let v = vec![1.0, 0.0, 0.0];
        let unit = normalize(&v);
        assert!((unit[0] - 1.0).abs() < 1e-6);
        assert!(unit[1].abs() < 1e-6);
        assert!(unit[2].abs() < 1e-6);
    }

    #[test]
    fn test_normalize_zero_vector() {
        let v = vec![0.0, 0.0, 0.0];
        let unit = normalize(&v);
        // Zero vector should return zero vector
        assert_eq!(unit, vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn test_normalize_empty_vector() {
        let v: Vec<f32> = vec![];
        let unit = normalize(&v);
        assert!(unit.is_empty());
    }

    #[test]
    fn test_normalize_negative_components() {
        let v = vec![-3.0, -4.0];
        let unit = normalize(&v);
        assert!((unit[0] - (-0.6)).abs() < 1e-6);
        assert!((unit[1] - (-0.8)).abs() < 1e-6);
        assert!((magnitude(&unit) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_normalize_mixed_sign_components() {
        // Test mixed positive and negative components: [3, -4] has magnitude 5
        let v = vec![3.0, -4.0];
        let unit = normalize(&v);
        assert!((unit[0] - 0.6).abs() < 1e-6);
        assert!((unit[1] - (-0.8)).abs() < 1e-6);
        assert!((magnitude(&unit) - 1.0).abs() < 1e-6);

        // Verify sign is preserved
        assert!(unit[0] > 0.0, "First component should remain positive");
        assert!(unit[1] < 0.0, "Second component should remain negative");
    }

    #[test]
    fn test_normalize_preserves_direction() {
        let v = vec![2.0, 4.0, 6.0];
        let unit = normalize(&v);
        // Ratios should be preserved: 1:2:3
        let ratio_1_2 = unit[0] / unit[1];
        let ratio_1_3 = unit[0] / unit[2];
        assert!((ratio_1_2 - 0.5).abs() < 1e-6);
        assert!((ratio_1_3 - (1.0 / 3.0)).abs() < 1e-6);
    }

    #[test]
    fn test_normalize_large_dimension() {
        let n = 1536;
        let v: Vec<f32> = (0..n).map(|i| (i as f32 * 0.1).sin()).collect();
        let unit = normalize(&v);
        assert!(
            (magnitude(&unit) - 1.0).abs() < 1e-5,
            "Normalized vector should have magnitude 1.0, got {}",
            magnitude(&unit)
        );
    }

    // ========================================================================
    // Normalize In-Place Tests
    // ========================================================================

    #[test]
    fn test_normalize_in_place_3_4_triangle() {
        let mut v = vec![3.0, 4.0];
        normalize_in_place(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
        assert!((magnitude(&v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_normalize_in_place_zero_vector() {
        let mut v = vec![0.0, 0.0, 0.0];
        normalize_in_place(&mut v);
        // Zero vector should remain unchanged
        assert_eq!(v, vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn test_normalize_in_place_empty_vector() {
        let mut v: Vec<f32> = vec![];
        normalize_in_place(&mut v);
        assert!(v.is_empty());
    }

    #[test]
    fn test_normalize_in_place_matches_normalize() {
        let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let expected = normalize(&v);

        let mut v_copy = v.clone();
        normalize_in_place(&mut v_copy);

        for (a, b) in v_copy.iter().zip(expected.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn test_normalize_in_place_large_dimension() {
        let n = 1536;
        let mut v: Vec<f32> = (0..n).map(|i| (i as f32 * 0.1).cos()).collect();
        normalize_in_place(&mut v);
        assert!(
            (magnitude(&v) - 1.0).abs() < 1e-5,
            "In-place normalized vector should have magnitude 1.0, got {}",
            magnitude(&v)
        );
    }

    // ========================================================================
    // Is Normalized Tests
    // ========================================================================

    #[test]
    fn test_is_normalized_unit_vectors() {
        assert!(is_normalized(&[1.0, 0.0, 0.0], 1e-6));
        assert!(is_normalized(&[0.0, 1.0, 0.0], 1e-6));
        assert!(is_normalized(&[0.0, 0.0, 1.0], 1e-6));
    }

    #[test]
    fn test_is_normalized_normalized_vector() {
        let v = normalize(&[3.0, 4.0]);
        assert!(is_normalized(&v, 1e-6));
    }

    #[test]
    fn test_is_normalized_non_unit_vector() {
        assert!(!is_normalized(&[3.0, 4.0], 1e-6)); // magnitude = 5
        assert!(!is_normalized(&[2.0, 0.0, 0.0], 1e-6)); // magnitude = 2
    }

    #[test]
    fn test_is_normalized_zero_vector() {
        // Zero vector has magnitude 0, which is not 1
        assert!(!is_normalized(&[0.0, 0.0, 0.0], 1e-6));
    }

    #[test]
    fn test_is_normalized_tolerance() {
        // Vector with magnitude 1.0001 should pass with 1e-3 tolerance
        let v = vec![1.0001, 0.0, 0.0];
        assert!(!is_normalized(&v, 1e-6));
        assert!(is_normalized(&v, 1e-3));
    }

    #[test]
    fn test_is_normalized_diagonal() {
        // Unit diagonal in 3D: each component = 1/sqrt(3)
        let c = 1.0 / 3.0_f32.sqrt();
        let v = vec![c, c, c];
        assert!(is_normalized(&v, 1e-6));
    }

    // ========================================================================
    // DistanceMetric Tests
    // ========================================================================

    #[test]
    fn test_distance_metric_default() {
        let metric = DistanceMetric::default();
        assert_eq!(metric, DistanceMetric::Cosine);
    }

    #[test]
    fn test_distance_metric_cosine_identical_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];

        // Identical vectors: similarity = 1, distance = 0
        let similarity = DistanceMetric::Cosine.compute_similarity(&a, &b).unwrap();
        let distance = DistanceMetric::Cosine.compute_distance(&a, &b).unwrap();

        assert!((similarity - 1.0).abs() < 1e-6);
        assert!((distance - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_cosine_opposite_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];

        // Opposite vectors: similarity = -1, distance = 2
        let similarity = DistanceMetric::Cosine.compute_similarity(&a, &b).unwrap();
        let distance = DistanceMetric::Cosine.compute_distance(&a, &b).unwrap();

        assert!((similarity - (-1.0)).abs() < 1e-6);
        assert!((distance - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_cosine_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];

        // Orthogonal vectors: similarity = 0, distance = 1
        let similarity = DistanceMetric::Cosine.compute_similarity(&a, &b).unwrap();
        let distance = DistanceMetric::Cosine.compute_distance(&a, &b).unwrap();

        assert!((similarity - 0.0).abs() < 1e-6);
        assert!((distance - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_euclidean_identical_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];

        // Identical vectors: distance = 0, similarity = 1
        let distance = DistanceMetric::Euclidean.compute_distance(&a, &b).unwrap();
        let similarity = DistanceMetric::Euclidean
            .compute_similarity(&a, &b)
            .unwrap();

        assert!((distance - 0.0).abs() < 1e-6);
        assert!((similarity - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_euclidean_unit_distance() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 0.0];

        // Distance = 1, similarity = 1/(1+1) = 0.5
        let distance = DistanceMetric::Euclidean.compute_distance(&a, &b).unwrap();
        let similarity = DistanceMetric::Euclidean
            .compute_similarity(&a, &b)
            .unwrap();

        assert!((distance - 1.0).abs() < 1e-6);
        assert!((similarity - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_euclidean_3_4_5_triangle() {
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];

        // Distance = 5, similarity = 1/(1+5) = 1/6
        let distance = DistanceMetric::Euclidean.compute_distance(&a, &b).unwrap();
        let similarity = DistanceMetric::Euclidean
            .compute_similarity(&a, &b)
            .unwrap();

        assert!((distance - 5.0).abs() < 1e-6);
        assert!((similarity - (1.0 / 6.0)).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_dot_product_identical_unit_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];

        // Unit vectors: dot = 1, distance = 0
        let similarity = DistanceMetric::DotProduct
            .compute_similarity(&a, &b)
            .unwrap();
        let distance = DistanceMetric::DotProduct.compute_distance(&a, &b).unwrap();

        assert!((similarity - 1.0).abs() < 1e-6);
        assert!((distance - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_dot_product_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];

        // Orthogonal: dot = 0, distance = 1
        let similarity = DistanceMetric::DotProduct
            .compute_similarity(&a, &b)
            .unwrap();
        let distance = DistanceMetric::DotProduct.compute_distance(&a, &b).unwrap();

        assert!((similarity - 0.0).abs() < 1e-6);
        assert!((distance - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_dot_product_opposite_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];

        // Opposite: dot = -1, distance = 2
        let similarity = DistanceMetric::DotProduct
            .compute_similarity(&a, &b)
            .unwrap();
        let distance = DistanceMetric::DotProduct.compute_distance(&a, &b).unwrap();

        assert!((similarity - (-1.0)).abs() < 1e-6);
        assert!((distance - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_distance_metric_dimension_mismatch() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];

        // All metrics should return an error for mismatched dimensions
        assert!(DistanceMetric::Cosine.compute_distance(&a, &b).is_err());
        assert!(DistanceMetric::Cosine.compute_similarity(&a, &b).is_err());
        assert!(DistanceMetric::Euclidean.compute_distance(&a, &b).is_err());
        assert!(
            DistanceMetric::Euclidean
                .compute_similarity(&a, &b)
                .is_err()
        );
        assert!(DistanceMetric::DotProduct.compute_distance(&a, &b).is_err());
        assert!(
            DistanceMetric::DotProduct
                .compute_similarity(&a, &b)
                .is_err()
        );
    }

    #[test]
    fn test_distance_metric_name() {
        assert_eq!(DistanceMetric::Cosine.name(), "cosine");
        assert_eq!(DistanceMetric::Euclidean.name(), "euclidean");
        assert_eq!(DistanceMetric::DotProduct.name(), "dot_product");
    }

    #[test]
    fn test_distance_metric_display() {
        assert_eq!(format!("{}", DistanceMetric::Cosine), "cosine");
        assert_eq!(format!("{}", DistanceMetric::Euclidean), "euclidean");
        assert_eq!(format!("{}", DistanceMetric::DotProduct), "dot_product");
    }

    #[test]
    fn test_distance_metric_requires_normalized() {
        assert!(!DistanceMetric::Cosine.requires_normalized_vectors());
        assert!(!DistanceMetric::Euclidean.requires_normalized_vectors());
        assert!(DistanceMetric::DotProduct.requires_normalized_vectors());
    }

    #[test]
    fn test_distance_metric_clone_copy() {
        let metric = DistanceMetric::Cosine;
        // Both Clone and Copy traits work - use Copy (implicit)
        let copied1 = metric;
        let copied2 = metric;

        assert_eq!(metric, copied1);
        assert_eq!(metric, copied2);
    }

    #[test]
    fn test_distance_metric_debug() {
        assert_eq!(format!("{:?}", DistanceMetric::Cosine), "Cosine");
        assert_eq!(format!("{:?}", DistanceMetric::Euclidean), "Euclidean");
        assert_eq!(format!("{:?}", DistanceMetric::DotProduct), "DotProduct");
    }

    #[test]
    fn test_distance_metric_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(DistanceMetric::Cosine);
        set.insert(DistanceMetric::Euclidean);
        set.insert(DistanceMetric::DotProduct);
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn test_distance_metric_consistency() {
        // For normalized vectors, cosine similarity should equal dot product
        let a = normalize(&[3.0, 4.0]);
        let b = normalize(&[1.0, 0.0]);

        let cosine_sim = DistanceMetric::Cosine.compute_similarity(&a, &b).unwrap();
        let dot_sim = DistanceMetric::DotProduct
            .compute_similarity(&a, &b)
            .unwrap();

        assert!(
            (cosine_sim - dot_sim).abs() < 1e-5,
            "For normalized vectors, cosine ({}) should equal dot product ({})",
            cosine_sim,
            dot_sim
        );
    }

    #[test]
    fn test_distance_metric_all_metrics_same_order() {
        // All metrics should agree on which of two vectors is more similar to a query
        let query = vec![1.0, 0.5, 0.0];
        let closer = vec![0.9, 0.4, 0.1];
        let farther = vec![0.1, 0.9, 0.8];

        // Cosine
        let cosine_closer = DistanceMetric::Cosine
            .compute_similarity(&query, &closer)
            .unwrap();
        let cosine_farther = DistanceMetric::Cosine
            .compute_similarity(&query, &farther)
            .unwrap();
        assert!(
            cosine_closer > cosine_farther,
            "Cosine: {} should be > {}",
            cosine_closer,
            cosine_farther
        );

        // Euclidean
        let eucl_closer = DistanceMetric::Euclidean
            .compute_distance(&query, &closer)
            .unwrap();
        let eucl_farther = DistanceMetric::Euclidean
            .compute_distance(&query, &farther)
            .unwrap();
        assert!(
            eucl_closer < eucl_farther,
            "Euclidean: {} should be < {}",
            eucl_closer,
            eucl_farther
        );

        // Dot Product
        let dot_closer = DistanceMetric::DotProduct
            .compute_similarity(&query, &closer)
            .unwrap();
        let dot_farther = DistanceMetric::DotProduct
            .compute_similarity(&query, &farther)
            .unwrap();
        assert!(
            dot_closer > dot_farther,
            "DotProduct: {} should be > {}",
            dot_closer,
            dot_farther
        );
    }

    // ========================================================================
    // Vector Validation Tests
    // ========================================================================

    #[test]
    fn test_validate_vector_valid() {
        let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert!(validate_vector(&v).is_ok());
    }

    #[test]
    fn test_validate_vector_empty() {
        let v: Vec<f32> = vec![];
        assert!(validate_vector(&v).is_ok());
    }

    #[test]
    fn test_validate_vector_single_element() {
        let v = vec![42.0];
        assert!(validate_vector(&v).is_ok());
    }

    #[test]
    fn test_validate_vector_negative_and_zero() {
        let v = vec![-1.0, 0.0, 1.0, -0.0];
        assert!(validate_vector(&v).is_ok());
    }

    #[test]
    fn test_validate_vector_nan_single() {
        let v = vec![1.0, f32::NAN, 3.0];
        let result = validate_vector(&v);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::ContainsNaN { count })) => {
                assert_eq!(count, 1);
            }
            _ => panic!("Expected ContainsNaN error"),
        }
    }

    #[test]
    fn test_validate_vector_nan_multiple() {
        let v = vec![f32::NAN, 1.0, f32::NAN, 2.0, f32::NAN];
        let result = validate_vector(&v);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::ContainsNaN { count })) => {
                assert_eq!(count, 3);
            }
            _ => panic!("Expected ContainsNaN error with count 3"),
        }
    }

    #[test]
    fn test_validate_vector_infinity_positive() {
        let v = vec![1.0, f32::INFINITY, 3.0];
        let result = validate_vector(&v);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::ContainsInfinity { count })) => {
                assert_eq!(count, 1);
            }
            _ => panic!("Expected ContainsInfinity error"),
        }
    }

    #[test]
    fn test_validate_vector_infinity_negative() {
        let v = vec![1.0, f32::NEG_INFINITY, 3.0];
        let result = validate_vector(&v);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::ContainsInfinity { count })) => {
                assert_eq!(count, 1);
            }
            _ => panic!("Expected ContainsInfinity error"),
        }
    }

    #[test]
    fn test_validate_vector_infinity_multiple() {
        let v = vec![f32::INFINITY, 1.0, f32::NEG_INFINITY];
        let result = validate_vector(&v);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::ContainsInfinity { count })) => {
                assert_eq!(count, 2);
            }
            _ => panic!("Expected ContainsInfinity error with count 2"),
        }
    }

    #[test]
    fn test_validate_vector_nan_takes_precedence() {
        // When both NaN and Infinity are present, NaN error should be returned first
        let v = vec![f32::NAN, f32::INFINITY];
        let result = validate_vector(&v);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::ContainsNaN { count })) => {
                assert_eq!(count, 1);
            }
            _ => panic!("Expected ContainsNaN error (NaN takes precedence over Infinity)"),
        }
    }

    #[test]
    fn test_validate_vector_subnormal() {
        // Subnormal (denormalized) numbers should be valid
        let v = vec![f32::MIN_POSITIVE / 2.0, 1.0];
        assert!(validate_vector(&v).is_ok());
    }

    #[test]
    fn test_validate_vector_max_min_values() {
        // Extreme but finite values should be valid
        let v = vec![f32::MAX, f32::MIN, 1.0];
        assert!(validate_vector(&v).is_ok());
    }

    // ========================================================================
    // Dimension Match Tests
    // ========================================================================

    #[test]
    fn test_check_dimensions_match_equal() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        assert!(check_dimensions_match(&a, &b).is_ok());
    }

    #[test]
    fn test_check_dimensions_match_empty() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        assert!(check_dimensions_match(&a, &b).is_ok());
    }

    #[test]
    fn test_check_dimensions_match_single() {
        let a = vec![1.0];
        let b = vec![2.0];
        assert!(check_dimensions_match(&a, &b).is_ok());
    }

    #[test]
    fn test_check_dimensions_match_unequal() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0];
        let result = check_dimensions_match(&a, &b);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::DimensionMismatch { expected, actual })) => {
                assert_eq!(expected, 3);
                assert_eq!(actual, 2);
            }
            _ => panic!("Expected DimensionMismatch error"),
        }
    }

    #[test]
    fn test_check_dimensions_match_unequal_reversed() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0, 2.0, 3.0];
        let result = check_dimensions_match(&a, &b);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::DimensionMismatch { expected, actual })) => {
                assert_eq!(expected, 2);
                assert_eq!(actual, 3);
            }
            _ => panic!("Expected DimensionMismatch error"),
        }
    }

    #[test]
    fn test_check_dimensions_match_empty_vs_nonempty() {
        let a: Vec<f32> = vec![];
        let b = vec![1.0];
        let result = check_dimensions_match(&a, &b);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::DimensionMismatch { expected, actual })) => {
                assert_eq!(expected, 0);
                assert_eq!(actual, 1);
            }
            _ => panic!("Expected DimensionMismatch error"),
        }
    }

    // ========================================================================
    // Validate Vector with Bounds Tests
    // ========================================================================

    #[test]
    fn test_validate_vector_with_bounds_valid() {
        let v = vec![1.0, 2.0, 3.0];
        assert!(validate_vector_with_bounds(&v, 10).is_ok());
    }

    #[test]
    fn test_validate_vector_with_bounds_exact() {
        let v = vec![1.0, 2.0, 3.0];
        assert!(validate_vector_with_bounds(&v, 3).is_ok());
    }

    #[test]
    fn test_validate_vector_with_bounds_too_large() {
        let v = vec![1.0, 2.0, 3.0];
        let result = validate_vector_with_bounds(&v, 2);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::DimensionTooLarge {
                dimension,
                max_allowed,
            })) => {
                assert_eq!(dimension, 3);
                assert_eq!(max_allowed, 2);
            }
            _ => panic!("Expected DimensionTooLarge error"),
        }
    }

    #[test]
    fn test_validate_vector_with_bounds_empty() {
        let v: Vec<f32> = vec![];
        assert!(validate_vector_with_bounds(&v, 0).is_ok());
    }

    #[test]
    fn test_validate_vector_with_bounds_nan() {
        // NaN should be detected even if dimension is within bounds
        let v = vec![f32::NAN, 2.0];
        let result = validate_vector_with_bounds(&v, 10);
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::ContainsNaN { .. }))
        ));
    }

    #[test]
    fn test_validate_vector_with_bounds_dimension_checked_first() {
        // If dimension exceeds bounds, that error should be returned
        // even if the vector also contains NaN
        let v = vec![f32::NAN, 2.0, 3.0, 4.0];
        let result = validate_vector_with_bounds(&v, 2);
        assert!(result.is_err());
        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::DimensionTooLarge { .. }))
        ));
    }

    // ========================================================================
    // SIMD Coverage Tests
    // ========================================================================

    #[test]
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    fn test_simd_explicit_coverage() {
        // This test explicitly calls internal SIMD functions to ensure code coverage
        // for safety assertions, even if the runtime dispatcher favors one instruction set.
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let b = vec![8.0, 7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0];

        // 1. Test SSE2 (Always available on x86_64, usually available on x86)
        if is_x86_feature_detected!("sse2") {
            unsafe {
                // dot_and_magnitudes_sse2
                let (dot, mag_a, mag_b) = super::simd::internal::dot_and_magnitudes_sse2(&a, &b);
                assert!(!dot.is_nan());
                assert!(mag_a > 0.0);
                assert!(mag_b > 0.0);

                // dot_product_sse2
                let dot_prod = super::simd::internal::dot_product_sse2(&a, &b);
                assert!((dot - dot_prod).abs() < 1e-5);

                // squared_diff_sum_sse2
                let sq_diff = super::simd::internal::squared_diff_sum_sse2(&a, &b);
                assert!(sq_diff >= 0.0);

                // scale_in_place_sse2
                let mut v = a.clone();
                super::simd::internal::scale_in_place_sse2(&mut v, 2.0);
                assert!((v[0] - 2.0).abs() < 1e-5);
            }
        }

        // 2. Test AVX2 (Conditional)
        if is_x86_feature_detected!("avx2") {
            unsafe {
                // scale_in_place_avx2 only needs AVX2
                let mut v = a.clone();
                super::simd::internal::scale_in_place_avx2(&mut v, 2.0);
                assert!((v[0] - 2.0).abs() < 1e-5);

                // Others need FMA + AVX2
                if is_x86_feature_detected!("fma") {
                    // dot_and_magnitudes_avx2
                    let (dot, mag_a, _mag_b) =
                        super::simd::internal::dot_and_magnitudes_avx2(&a, &b);
                    assert!(!dot.is_nan());
                    assert!(mag_a > 0.0);

                    // dot_product_avx2
                    let dot_prod = super::simd::internal::dot_product_avx2(&a, &b);
                    assert!((dot - dot_prod).abs() < 1e-5);

                    // squared_diff_sum_avx2
                    let sq_diff = super::simd::internal::squared_diff_sum_avx2(&a, &b);
                    assert!(sq_diff >= 0.0);
                }
            }
        }
    }
}

// ============================================================================
// Property-Based Tests
// ============================================================================

#[cfg(test)]
#[test]
fn test_scalar_fallback_coverage() {
    // Explicitly test scalar fallbacks to ensure code coverage,
    // as they are skipped by dispatchers on x86_64/AVX2 systems.
    let a = vec![1.0, 2.0, 3.0, 4.0];
    let b = vec![4.0, 3.0, 2.0, 1.0];

    // dot_and_magnitudes_scalar
    let (dot, mag_a, mag_b) = super::simd::dot_and_magnitudes_scalar(&a, &b);
    assert!((dot - 20.0).abs() < 1e-5); // 4+6+6+4 = 20
    assert!((mag_a - 30.0).abs() < 1e-5); // 1+4+9+16 = 30
    assert!((mag_b - 30.0).abs() < 1e-5);

    // dot_product_scalar
    let dot_p = super::simd::dot_product_scalar(&a, &b);
    assert!((dot_p - 20.0).abs() < 1e-5);

    // squared_diff_sum_scalar
    let sq_diff = super::simd::squared_diff_sum_scalar(&a, &b);
    // (1-4)^2 + (2-3)^2 + (3-2)^2 + (4-1)^2 = 9 + 1 + 1 + 9 = 20
    assert!((sq_diff - 20.0).abs() < 1e-5);

    // scale_in_place_scalar
    let mut v = a.clone();
    super::simd::scale_in_place_scalar(&mut v, 2.0);
    assert!((v[0] - 2.0).abs() < 1e-5);
    assert!((v[3] - 8.0).abs() < 1e-5);
}
mod proptests {
    use super::*;
    use proptest::prelude::*;

    // ========================================================================
    // Tolerance Choice Documentation
    // ========================================================================
    //
    // We use 1e-5 absolute tolerance for property tests. This was chosen based on:
    //
    // 1. **f32 precision limits**: f32 has ~7 decimal digits of precision.
    //    For values in [-100, 100] range with up to 100 dimensions, accumulated
    //    error from multiply-add operations is bounded by roughly:
    //    - Per operation: ~1e-7 relative error (f32 epsilon)
    //    - 100 operations: ~1e-5 accumulated error (sqrt(n) * epsilon for random errors)
    //
    // 2. **SIMD vs scalar consistency**: Different code paths (AVX2, SSE2, scalar)
    //    may produce slightly different results due to operation ordering.
    //    1e-5 tolerance accounts for these differences.
    //
    // 3. **Mathematical invariants**: Cosine similarity is bounded to [-1, 1],
    //    so 1e-5 represents a relative error of 0.001% at worst.
    //
    // For applications requiring tighter bounds, normalize vectors first
    // (which reduces magnitude-related accumulation errors).
    // ========================================================================

    /// Tolerance for property-based tests.
    ///
    /// See module-level documentation for rationale behind this choice.
    const PROPTEST_TOLERANCE: f32 = 1e-5;

    // Strategy to generate non-empty vectors of the same length
    fn same_length_vectors(max_len: usize) -> impl Strategy<Value = (Vec<f32>, Vec<f32>)> {
        (1..=max_len).prop_flat_map(|len| {
            (
                prop::collection::vec(-100.0f32..100.0f32, len),
                prop::collection::vec(-100.0f32..100.0f32, len),
            )
        })
    }

    proptest! {
        #[test]
        fn prop_cosine_similarity_is_symmetric(
            (a, b) in same_length_vectors(100)
        ) {
            let sim_ab = cosine_similarity(&a, &b).unwrap();
            let sim_ba = cosine_similarity(&b, &a).unwrap();
            prop_assert!((sim_ab - sim_ba).abs() < PROPTEST_TOLERANCE);
        }

        #[test]
        fn prop_cosine_similarity_in_range(
            (a, b) in same_length_vectors(100)
        ) {
            let sim = cosine_similarity(&a, &b).unwrap();
            // Handle NaN case (can occur with extreme values)
            if !sim.is_nan() {
                // Result is clamped, so should always be in [-1, 1]
                // Allow tiny tolerance for floating-point edge cases
                let min = -1.0 - PROPTEST_TOLERANCE;
                let max = 1.0 + PROPTEST_TOLERANCE;
                prop_assert!((min..=max).contains(&sim),
                    "Similarity {} out of range for {:?} and {:?}", sim, a, b);
            }
        }

        #[test]
        fn prop_cosine_similarity_self_is_one(
            a in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            // Skip zero vectors
            let magnitude_sq: f32 = a.iter().map(|x| x * x).sum();
            if magnitude_sq > 1e-10 {
                let sim = cosine_similarity(&a, &a).unwrap();
                prop_assert!((sim - 1.0).abs() < PROPTEST_TOLERANCE,
                    "Self-similarity should be 1.0, got {} for {:?}", sim, a);
            }
        }

        #[test]
        fn prop_cosine_similarity_negation_flips_sign(
            a in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            // Skip zero vectors
            let magnitude_sq: f32 = a.iter().map(|x| x * x).sum();
            if magnitude_sq > 1e-10 {
                let neg_a: Vec<f32> = a.iter().map(|x| -x).collect();
                let sim = cosine_similarity(&a, &neg_a).unwrap();
                prop_assert!((sim + 1.0).abs() < PROPTEST_TOLERANCE,
                    "Negation similarity should be -1.0, got {} for {:?}", sim, a);
            }
        }

        #[test]
        fn prop_cosine_similarity_scale_invariant(
            (a, b) in same_length_vectors(50),
            scale in 0.1f32..10.0f32
        ) {
            // Skip if either vector is near-zero
            let mag_a: f32 = a.iter().map(|x| x * x).sum();
            let mag_b: f32 = b.iter().map(|x| x * x).sum();
            if mag_a > 1e-10 && mag_b > 1e-10 {
                let sim1 = cosine_similarity(&a, &b).unwrap();

                let scaled_a: Vec<f32> = a.iter().map(|x| x * scale).collect();
                let sim2 = cosine_similarity(&scaled_a, &b).unwrap();

                // Cosine similarity should be scale-invariant.
                //
                // Why 10x tolerance? Scaling introduces additional error sources:
                // 1. The scaling multiplication itself: len extra multiply operations
                // 2. Magnitude changes: scaled vectors have different magnitudes,
                //    potentially causing different rounding in the sqrt and division
                // 3. For scale factors near the edges (0.1 or 10.0), the magnitude
                //    difference is up to 100x, affecting numerical stability
                //
                // Empirically, 10x base tolerance handles these cases while still
                // catching genuine scale invariance violations.
                if !sim1.is_nan() && !sim2.is_nan() {
                    prop_assert!((sim1 - sim2).abs() < PROPTEST_TOLERANCE * 10.0,
                        "Scale invariance failed: {} vs {} for scale {}", sim1, sim2, scale);
                }
            }
        }

        // ====================================================================
        // Euclidean Distance Property Tests
        // ====================================================================

        #[test]
        fn prop_euclidean_distance_is_non_negative(
            (a, b) in same_length_vectors(100)
        ) {
            let dist = euclidean_distance(&a, &b).unwrap();
            prop_assert!(dist >= 0.0, "Distance should be non-negative, got {}", dist);
        }

        #[test]
        fn prop_squared_euclidean_distance_is_non_negative(
            (a, b) in same_length_vectors(100)
        ) {
            let dist_sq = squared_euclidean_distance(&a, &b).unwrap();
            prop_assert!(dist_sq >= 0.0, "Squared distance should be non-negative, got {}", dist_sq);
        }

        #[test]
        fn prop_euclidean_distance_is_symmetric(
            (a, b) in same_length_vectors(100)
        ) {
            let dist_ab = euclidean_distance(&a, &b).unwrap();
            let dist_ba = euclidean_distance(&b, &a).unwrap();
            prop_assert!((dist_ab - dist_ba).abs() < PROPTEST_TOLERANCE,
                "Euclidean distance should be symmetric: {} vs {}", dist_ab, dist_ba);
        }

        #[test]
        fn prop_squared_euclidean_distance_is_symmetric(
            (a, b) in same_length_vectors(100)
        ) {
            let dist_sq_ab = squared_euclidean_distance(&a, &b).unwrap();
            let dist_sq_ba = squared_euclidean_distance(&b, &a).unwrap();
            prop_assert!((dist_sq_ab - dist_sq_ba).abs() < PROPTEST_TOLERANCE,
                "Squared Euclidean distance should be symmetric: {} vs {}", dist_sq_ab, dist_sq_ba);
        }

        #[test]
        fn prop_euclidean_distance_self_is_zero(
            a in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            let dist = euclidean_distance(&a, &a).unwrap();
            prop_assert!(dist.abs() < PROPTEST_TOLERANCE,
                "Distance to self should be 0, got {} for {:?}", dist, a);
        }

        #[test]
        fn prop_squared_euclidean_distance_self_is_zero(
            a in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            let dist_sq = squared_euclidean_distance(&a, &a).unwrap();
            prop_assert!(dist_sq.abs() < PROPTEST_TOLERANCE,
                "Squared distance to self should be 0, got {} for {:?}", dist_sq, a);
        }

        #[test]
        fn prop_euclidean_squared_relationship(
            (a, b) in same_length_vectors(100)
        ) {
            let dist = euclidean_distance(&a, &b).unwrap();
            let dist_sq = squared_euclidean_distance(&a, &b).unwrap();
            // dist² should equal dist_sq
            // Use relative tolerance for larger values
            let tolerance = PROPTEST_TOLERANCE * 100.0 + dist_sq * 1e-5;
            prop_assert!((dist * dist - dist_sq).abs() < tolerance,
                "euclidean² ({}) should equal squared_euclidean ({})", dist * dist, dist_sq);
        }

        #[test]
        fn prop_euclidean_distance_triangle_inequality(
            a in prop::collection::vec(-50.0f32..50.0f32, 1..50usize),
            b in prop::collection::vec(-50.0f32..50.0f32, 1..50usize),
            c in prop::collection::vec(-50.0f32..50.0f32, 1..50usize)
        ) {
            // Triangle inequality: d(a,c) <= d(a,b) + d(b,c)
            // Only test if all vectors have same length
            if a.len() == b.len() && b.len() == c.len() {
                let d_ab = euclidean_distance(&a, &b).unwrap();
                let d_bc = euclidean_distance(&b, &c).unwrap();
                let d_ac = euclidean_distance(&a, &c).unwrap();

                // Allow small tolerance for floating-point errors
                let tolerance = PROPTEST_TOLERANCE * 100.0;
                prop_assert!(d_ac <= d_ab + d_bc + tolerance,
                    "Triangle inequality violated: d(a,c)={} > d(a,b)={} + d(b,c)={}",
                    d_ac, d_ab, d_bc);
            }
        }

        // ====================================================================
        // Dot Product Property Tests
        // ====================================================================

        #[test]
        fn prop_dot_product_is_symmetric(
            (a, b) in same_length_vectors(100)
        ) {
            let dot_ab = dot_product(&a, &b).unwrap();
            let dot_ba = dot_product(&b, &a).unwrap();
            prop_assert!((dot_ab - dot_ba).abs() < PROPTEST_TOLERANCE,
                "Dot product should be symmetric: {} vs {}", dot_ab, dot_ba);
        }

        #[test]
        fn prop_dot_product_self_equals_squared_magnitude(
            a in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            let self_dot = dot_product(&a, &a).unwrap();
            // Self dot product should equal sum of squares (squared magnitude)
            let expected: f32 = a.iter().map(|x| x * x).sum();

            // Use relative tolerance for larger values
            let tolerance = PROPTEST_TOLERANCE * 100.0 + expected * 1e-5;
            prop_assert!((self_dot - expected).abs() < tolerance,
                "Self dot product should equal squared magnitude: {} vs {}", self_dot, expected);
        }

        #[test]
        fn prop_dot_product_with_zero_vector(
            a in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            let zeros: Vec<f32> = vec![0.0; a.len()];
            let result = dot_product(&a, &zeros).unwrap();
            prop_assert!(result.abs() < PROPTEST_TOLERANCE,
                "Dot product with zero vector should be 0, got {}", result);
        }

        #[test]
        fn prop_dot_product_bilinearity_scalar(
            (a, b) in same_length_vectors(50),
            scale in 0.1f32..10.0f32
        ) {
            // Scalar multiplication: dot(c*a, b) = c * dot(a, b)
            let dot_ab = dot_product(&a, &b).unwrap();
            let scaled_a: Vec<f32> = a.iter().map(|x| x * scale).collect();
            let dot_scaled = dot_product(&scaled_a, &b).unwrap();

            // Use tolerance based on intermediate value magnitudes.
            //
            // Key insight: when vectors have mixed +/- values, catastrophic
            // cancellation occurs. Large intermediate values (e.g., 9000) can
            // cancel to give small results (e.g., 16). The floating-point error
            // is bounded by the intermediate magnitudes, not the final result.
            //
            // Example: sum of products might be [+9000, -8500, +500, -984, ...]
            // giving final result of ~16, but error is proportional to ~9000.
            //
            // Solution: base tolerance on sum of absolute products, which
            // represents the "scale" of computation regardless of cancellation.
            let expected = scale * dot_ab;
            let sum_abs_products: f32 = a.iter()
                .zip(b.iter())
                .map(|(x, y)| (x * y).abs())
                .sum();
            // Error is proportional to intermediate magnitudes * scale * f32 epsilon
            // Use 1e-5 relative to intermediate values (conservative for f32)
            let tolerance = PROPTEST_TOLERANCE * 100.0 + sum_abs_products * scale * 1e-5;
            prop_assert!((dot_scaled - expected).abs() < tolerance,
                "Scalar bilinearity failed: dot({}*a, b)={} vs {}*dot(a,b)={}",
                scale, dot_scaled, scale, expected);
        }

        // ====================================================================
        // Normalization Property Tests
        // ====================================================================

        #[test]
        fn prop_normalized_vector_has_unit_magnitude(
            v in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            // Skip zero vectors using SIMD squared_magnitude
            let sq_mag = squared_magnitude(&v);
            if sq_mag > 1e-10 {
                let unit = normalize(&v);
                let mag = magnitude(&unit);
                prop_assert!((mag - 1.0).abs() < PROPTEST_TOLERANCE,
                    "Normalized vector should have magnitude 1.0, got {} for {:?}", mag, v);
            }
        }

        #[test]
        fn prop_normalize_in_place_has_unit_magnitude(
            v in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            // Skip zero vectors using SIMD squared_magnitude
            let sq_mag = squared_magnitude(&v);
            if sq_mag > 1e-10 {
                let mut v_copy = v.clone();
                normalize_in_place(&mut v_copy);
                let mag = magnitude(&v_copy);
                prop_assert!((mag - 1.0).abs() < PROPTEST_TOLERANCE,
                    "In-place normalized vector should have magnitude 1.0, got {}", mag);
            }
        }

        #[test]
        fn prop_normalize_matches_normalize_in_place(
            v in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            let unit = normalize(&v);
            let mut v_copy = v.clone();
            normalize_in_place(&mut v_copy);

            for (a, b) in unit.iter().zip(v_copy.iter()) {
                prop_assert!((a - b).abs() < PROPTEST_TOLERANCE,
                    "normalize and normalize_in_place should produce same result");
            }
        }

        #[test]
        fn prop_magnitude_is_non_negative(
            v in prop::collection::vec(-100.0f32..100.0f32, 0..100usize)
        ) {
            let mag = magnitude(&v);
            prop_assert!(mag >= 0.0, "Magnitude should be non-negative, got {}", mag);
        }

        #[test]
        fn prop_squared_magnitude_is_non_negative(
            v in prop::collection::vec(-100.0f32..100.0f32, 0..100usize)
        ) {
            let sq_mag = squared_magnitude(&v);
            prop_assert!(sq_mag >= 0.0, "Squared magnitude should be non-negative, got {}", sq_mag);
        }

        #[test]
        fn prop_magnitude_squared_relationship(
            v in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            let mag = magnitude(&v);
            let sq_mag = squared_magnitude(&v);
            // magnitude² should equal squared_magnitude
            // Use relative tolerance to avoid scale-dependent thresholds
            let diff = (mag * mag - sq_mag).abs();
            let relative_tolerance = 1e-5; // 0.001% relative error
            let threshold = sq_mag.max(1.0) * relative_tolerance;
            prop_assert!(diff < threshold,
                "magnitude² ({}) should equal squared_magnitude ({}), diff={}, threshold={}",
                mag * mag, sq_mag, diff, threshold);
        }

        #[test]
        fn prop_normalize_preserves_direction(
            v in prop::collection::vec(-100.0f32..100.0f32, 2..50usize)
        ) {
            // Skip zero or near-zero vectors using SIMD squared_magnitude
            let sq_mag = squared_magnitude(&v);
            if sq_mag > 1e-6 {
                let unit = normalize(&v);
                // Cosine similarity between original and normalized should be 1.0
                let sim = cosine_similarity(&v, &unit).unwrap();
                if !sim.is_nan() {
                    prop_assert!((sim - 1.0).abs() < PROPTEST_TOLERANCE * 10.0,
                        "Normalized vector should have same direction: sim = {}", sim);
                }
            }
        }

        #[test]
        fn prop_is_normalized_after_normalize(
            v in prop::collection::vec(-100.0f32..100.0f32, 1..100usize)
        ) {
            // Skip zero vectors using SIMD squared_magnitude
            let sq_mag = squared_magnitude(&v);
            if sq_mag > 1e-10 {
                let unit = normalize(&v);
                // Use 1e-5 tolerance matching PROPTEST_TOLERANCE
                prop_assert!(is_normalized(&unit, PROPTEST_TOLERANCE),
                    "Normalized vector should pass is_normalized check");
            }
        }
    }

    // ========== New Error Handling Tests for Coverage ==========

    #[test]
    fn test_validate_vector_nan_rejection() {
        let vector_with_nan = vec![1.0, 2.0, f32::NAN, 4.0];

        let result = validate_vector(&vector_with_nan);

        assert!(result.is_err());
        assert!(
            matches!(
                result.unwrap_err(),
                crate::utils::error::Error::Vector(crate::utils::error::VectorError::ContainsNaN {
                    count: 1
                })
            ),
            "Expected ContainsNaN error with count=1"
        );
    }

    #[test]
    fn test_validate_vector_infinity_rejection() {
        let vector_with_inf = vec![1.0, 2.0, f32::INFINITY, 4.0];

        let result = validate_vector(&vector_with_inf);

        assert!(
            result.is_err(),
            "validate_vector should reject infinity values"
        );
        assert!(
            matches!(
                result.unwrap_err(),
                crate::utils::error::Error::Vector(
                    crate::utils::error::VectorError::ContainsInfinity { count: 1 }
                )
            ),
            "Expected ContainsInfinity error with count=1"
        );
    }

    #[test]
    fn test_cosine_similarity_zero_magnitude_handling() {
        let zero_vector = vec![0.0, 0.0, 0.0];
        let normal_vector = vec![1.0, 2.0, 3.0];

        // The implementation returns 0.0 for zero magnitude vectors.
        let result = cosine_similarity(&zero_vector, &normal_vector);

        match result {
            Ok(sim) => {
                assert_eq!(sim, 0.0, "Cosine similarity with zero vector should be 0.0");
            }
            Err(e) => {
                panic!(
                    "Expected Ok(0.0) for zero magnitude vector, but got Err: {}",
                    e
                );
            }
        }
    }

    // ========================================================================
    // SparseVec Tests
    // ========================================================================

    #[test]
    fn test_sparse_vec_creation_valid() {
        let sparse = SparseVec::new(vec![0, 2, 5], vec![1.0, 2.0, 3.0], 10).unwrap();
        assert_eq!(sparse.nnz(), 3);
        assert_eq!(sparse.dimension(), 10);
        assert_eq!(sparse.indices(), &[0, 2, 5]);
        assert_eq!(sparse.values(), &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_sparse_vec_empty() {
        let sparse = SparseVec::new(vec![], vec![], 10).unwrap();
        assert_eq!(sparse.nnz(), 0);
        assert_eq!(sparse.dimension(), 10);
        assert_eq!(sparse.indices(), &[] as &[u32]);
        assert_eq!(sparse.values(), &[] as &[f32]);
    }

    #[test]
    fn test_sparse_vec_sorts_indices() {
        // Provide unsorted indices - should be sorted after construction
        let sparse = SparseVec::new(vec![5, 1, 3], vec![1.0, 2.0, 3.0], 10).unwrap();
        assert_eq!(sparse.indices(), &[1, 3, 5]);
        assert_eq!(sparse.values(), &[2.0, 3.0, 1.0]); // Values reordered to match sorted indices
    }

    #[test]
    fn test_sparse_vec_dimension_mismatch() {
        // indices.len() != values.len()
        let result = SparseVec::new(vec![0, 1], vec![1.0], 10);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Error::Vector(VectorError::DimensionMismatch { .. })
        ));
    }

    #[test]
    fn test_sparse_vec_index_out_of_bounds() {
        // Index 10 is out of bounds for dimension 10 (valid indices: 0-9)
        let result = SparseVec::new(vec![0, 10], vec![1.0, 2.0], 10);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Error::Vector(VectorError::InvalidSparseVector { .. })
        ));
    }

    #[test]
    fn test_sparse_vec_duplicate_indices() {
        let result = SparseVec::new(vec![0, 1, 1], vec![1.0, 2.0, 3.0], 10);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Error::Vector(VectorError::InvalidSparseVector { .. })
        ));
    }

    #[test]
    fn test_sparse_vec_zero_value() {
        // Sparse vectors should not store zero values
        let result = SparseVec::new(vec![0, 1, 2], vec![1.0, 0.0, 3.0], 10);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Error::Vector(VectorError::InvalidSparseVector { .. })
        ));
    }

    #[test]
    fn test_sparse_vec_nan_value() {
        let result = SparseVec::new(vec![0, 1], vec![1.0, f32::NAN], 10);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Error::Vector(VectorError::ContainsNaN { .. })
        ));
    }

    #[test]
    fn test_sparse_vec_infinity_value() {
        let result = SparseVec::new(vec![0, 1], vec![1.0, f32::INFINITY], 10);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Error::Vector(VectorError::ContainsInfinity { .. })
        ));
    }

    #[test]
    fn test_sparse_vec_dimension_too_large() {
        let result = SparseVec::new(vec![0], vec![1.0], MAX_VECTOR_DIMENSIONS as u32 + 1);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Error::Vector(VectorError::DimensionTooLarge { .. })
        ));
    }

    #[test]
    fn test_sparse_vec_to_dense() {
        let sparse = SparseVec::new(vec![1, 3, 5], vec![1.5, 2.5, 3.5], 7).unwrap();
        let dense = sparse.to_dense();
        assert_eq!(dense, vec![0.0, 1.5, 0.0, 2.5, 0.0, 3.5, 0.0]);
    }

    #[test]
    fn test_sparse_vec_to_dense_empty() {
        let sparse = SparseVec::new(vec![], vec![], 5).unwrap();
        let dense = sparse.to_dense();
        assert_eq!(dense, vec![0.0, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn test_sparse_vec_squared_magnitude() {
        let sparse = SparseVec::new(vec![0, 1, 2], vec![1.0, 2.0, 2.0], 5).unwrap();
        // magnitude² = 1² + 2² + 2² = 1 + 4 + 4 = 9
        assert_eq!(sparse.squared_magnitude(), 9.0);
    }

    #[test]
    fn test_sparse_vec_magnitude() {
        let sparse = SparseVec::new(vec![0, 1], vec![3.0, 4.0], 5).unwrap();
        // magnitude = sqrt(3² + 4²) = sqrt(9 + 16) = 5.0
        assert_eq!(sparse.magnitude(), 5.0);
    }

    #[test]
    fn test_sparse_vec_magnitude_empty() {
        let sparse = SparseVec::new(vec![], vec![], 10).unwrap();
        assert_eq!(sparse.magnitude(), 0.0);
        assert_eq!(sparse.squared_magnitude(), 0.0);
    }

    #[test]
    fn test_sparse_vec_clone() {
        let sparse1 = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
        let sparse2 = sparse1.clone();
        // Use approx_eq instead of == due to floating-point concerns
        assert!(
            sparse1.approx_eq(&sparse2, 1e-10),
            "Cloned sparse vector should be approximately equal to original"
        );
        assert_eq!(sparse1.indices(), sparse2.indices());
        assert_eq!(sparse1.values(), sparse2.values());
    }

    #[test]
    fn test_sparse_vec_approx_eq() {
        let a = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
        let b = SparseVec::new(vec![0, 2], vec![1.0000001, 2.0000001], 5).unwrap();

        // Small floating-point differences should be tolerated with appropriate epsilon
        assert!(
            a.approx_eq(&b, 1e-5),
            "Vectors with small floating-point differences should be approximately equal"
        );
        assert!(
            !a.approx_eq(&b, 1e-10),
            "Vectors should not be equal with very strict epsilon"
        );

        // Different dimensions
        let c = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 10).unwrap();
        assert!(
            !a.approx_eq(&c, 1e-5),
            "Vectors with different dimensions should not be equal"
        );

        // Different indices
        let d = SparseVec::new(vec![0, 3], vec![1.0, 2.0], 5).unwrap();
        assert!(
            !a.approx_eq(&d, 1e-5),
            "Vectors with different indices should not be equal"
        );

        // Different values
        let e = SparseVec::new(vec![0, 2], vec![1.0, 3.0], 5).unwrap();
        assert!(
            !a.approx_eq(&e, 1e-5),
            "Vectors with significantly different values should not be equal"
        );
    }

    #[test]
    fn test_sparse_vec_debug() {
        let sparse = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
        let debug_str = format!("{:?}", sparse);
        assert!(debug_str.contains("SparseVec"));
    }

    #[test]
    fn test_sparse_vec_single_element() {
        let sparse = SparseVec::new(vec![5], vec![1.0], 10).unwrap();
        assert_eq!(sparse.nnz(), 1);
        assert_eq!(sparse.dimension(), 10);
        let dense = sparse.to_dense();
        assert_eq!(
            dense,
            vec![0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn test_sparse_vec_bm25_like() {
        // Simulate a BM25-style sparse vector (typical for information retrieval)
        // Only a few terms are present in a document out of a large vocabulary
        let sparse = SparseVec::new(
            vec![10, 42, 100, 257],
            vec![2.5, 1.8, 3.2, 1.2],
            10000, // Large vocabulary size
        )
        .unwrap();

        assert_eq!(sparse.nnz(), 4);
        assert_eq!(sparse.dimension(), 10000);
        // Verify memory efficiency: only storing 4 non-zero values instead of 10000
    }

    #[test]
    fn test_sparse_vec_negative_values() {
        // Sparse vectors can have negative values
        let sparse = SparseVec::new(vec![0, 2, 4], vec![-1.0, 2.0, -3.0], 5).unwrap();
        assert_eq!(sparse.values(), &[-1.0, 2.0, -3.0]);
        let dense = sparse.to_dense();
        assert_eq!(dense, vec![-1.0, 0.0, 2.0, 0.0, -3.0]);
    }

    // ========================================================================
    // Sparse Vector Similarity Function Tests
    // ========================================================================

    #[test]
    fn test_sparse_dot_product_basic() {
        // Sparse vectors: [1, 0, 2, 0, 0] and [0, 0, 3, 0, 4]
        let a = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
        let b = SparseVec::new(vec![2, 4], vec![3.0, 4.0], 5).unwrap();

        // Only index 2 overlaps: 2.0 * 3.0 = 6.0
        let dot = sparse_dot_product(&a, &b).unwrap();
        assert_eq!(dot, 6.0);
    }

    #[test]
    fn test_sparse_dot_product_no_overlap() {
        // Sparse vectors with no overlapping indices
        let a = SparseVec::new(vec![0, 1], vec![1.0, 2.0], 10).unwrap();
        let b = SparseVec::new(vec![5, 6], vec![3.0, 4.0], 10).unwrap();

        let dot = sparse_dot_product(&a, &b).unwrap();
        assert_eq!(dot, 0.0);
    }

    #[test]
    fn test_sparse_dot_product_complete_overlap() {
        // Sparse vectors with complete overlap
        let a = SparseVec::new(vec![0, 2, 4], vec![1.0, 2.0, 3.0], 5).unwrap();
        let b = SparseVec::new(vec![0, 2, 4], vec![2.0, 3.0, 4.0], 5).unwrap();

        // 1*2 + 2*3 + 3*4 = 2 + 6 + 12 = 20
        let dot = sparse_dot_product(&a, &b).unwrap();
        assert_eq!(dot, 20.0);
    }

    #[test]
    fn test_sparse_dot_product_empty() {
        let a = SparseVec::new(vec![], vec![], 10).unwrap();
        let b = SparseVec::new(vec![0, 5], vec![1.0, 2.0], 10).unwrap();

        let dot = sparse_dot_product(&a, &b).unwrap();
        assert_eq!(dot, 0.0);
    }

    #[test]
    fn test_sparse_dot_product_different_dimensions() {
        // Vectors with different dimensions still work for dot product
        // Only common indices contribute
        let a = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
        let b = SparseVec::new(vec![2, 4], vec![3.0, 4.0], 10).unwrap();

        // Index 2 overlaps: 2.0 * 3.0 = 6.0
        let dot = sparse_dot_product(&a, &b).unwrap();
        assert_eq!(dot, 6.0);
    }

    #[test]
    fn test_sparse_cosine_similarity_identical() {
        let a = SparseVec::new(vec![0, 2], vec![1.0, 1.0], 5).unwrap();
        let b = SparseVec::new(vec![0, 2], vec![1.0, 1.0], 5).unwrap();

        // Identical vectors have cosine similarity = 1.0
        let sim = sparse_cosine_similarity(&a, &b).unwrap();
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_sparse_cosine_similarity_orthogonal() {
        // Orthogonal sparse vectors (no overlapping indices)
        let a = SparseVec::new(vec![0, 1], vec![1.0, 1.0], 5).unwrap();
        let b = SparseVec::new(vec![3, 4], vec![1.0, 1.0], 5).unwrap();

        // Orthogonal vectors have cosine similarity = 0.0
        let sim = sparse_cosine_similarity(&a, &b).unwrap();
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_sparse_cosine_similarity_opposite() {
        let a = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
        let b = SparseVec::new(vec![0, 2], vec![-1.0, -2.0], 5).unwrap();

        // Opposite vectors have cosine similarity = -1.0
        let sim = sparse_cosine_similarity(&a, &b).unwrap();
        assert!((sim + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_sparse_cosine_similarity_zero_magnitude() {
        let a = SparseVec::new(vec![], vec![], 10).unwrap(); // Zero vector
        let b = SparseVec::new(vec![0, 1], vec![1.0, 2.0], 10).unwrap();

        // Zero magnitude should return 0.0
        let sim = sparse_cosine_similarity(&a, &b).unwrap();
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_sparse_squared_euclidean_distance_basic() {
        let a = SparseVec::new(vec![0], vec![3.0], 5).unwrap();
        let b = SparseVec::new(vec![], vec![], 5).unwrap(); // Zero vector (all zeros)

        // Distance from [3,0,0,0,0] to [0,0,0,0,0] = 9
        let dist_sq = sparse_squared_euclidean_distance(&a, &b).unwrap();
        assert_eq!(dist_sq, 9.0);
    }

    #[test]
    fn test_sparse_squared_euclidean_distance_identical() {
        let a = SparseVec::new(vec![0, 2, 4], vec![1.0, 2.0, 3.0], 5).unwrap();
        let b = a.clone();

        let dist_sq = sparse_squared_euclidean_distance(&a, &b).unwrap();
        assert_eq!(dist_sq, 0.0);
    }

    #[test]
    fn test_sparse_squared_euclidean_distance_dimension_mismatch() {
        let a = SparseVec::new(vec![0], vec![1.0], 5).unwrap();
        let b = SparseVec::new(vec![0], vec![1.0], 10).unwrap();

        let result = sparse_squared_euclidean_distance(&a, &b);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Error::Vector(VectorError::DimensionMismatch { .. })
        ));
    }

    #[test]
    fn test_sparse_euclidean_distance_basic() {
        let a = SparseVec::new(vec![0], vec![3.0], 5).unwrap();
        let b = SparseVec::new(vec![], vec![], 5).unwrap(); // Zero vector (all zeros)

        // Distance from [3,0,0,0,0] to [0,0,0,0,0] = 3.0
        let dist = sparse_euclidean_distance(&a, &b).unwrap();
        assert_eq!(dist, 3.0);
    }

    #[test]
    fn test_sparse_euclidean_distance_pythagorean() {
        // Test Pythagorean triple: 3-4-5
        let a = SparseVec::new(vec![0, 1], vec![3.0, 4.0], 5).unwrap();
        let b = SparseVec::new(vec![], vec![], 5).unwrap(); // Zero vector

        // Distance should be 5.0
        let dist = sparse_euclidean_distance(&a, &b).unwrap();
        assert_eq!(dist, 5.0);
    }

    #[test]
    fn test_sparse_similarity_with_bm25_like_vectors() {
        // Simulate BM25 document vectors
        let doc1 = SparseVec::new(vec![10, 42, 100, 257], vec![2.5, 1.8, 3.2, 1.2], 10000).unwrap();
        let doc2 = SparseVec::new(vec![10, 50, 100, 300], vec![2.3, 1.5, 3.0, 1.0], 10000).unwrap();

        // These documents overlap at indices 10 and 100
        let dot = sparse_dot_product(&doc1, &doc2).unwrap();
        assert!(dot > 0.0); // Should have positive dot product

        let sim = sparse_cosine_similarity(&doc1, &doc2).unwrap();
        assert!(sim > 0.0); // Should have positive similarity
        assert!(sim <= 1.0); // Should be <= 1.0
    }

    #[test]
    fn test_sparse_vs_dense_dot_product_equivalence() {
        // Verify sparse and dense implementations give same results
        let sparse_a = SparseVec::new(vec![1, 3, 5], vec![1.0, 2.0, 3.0], 7).unwrap();
        let sparse_b = SparseVec::new(vec![0, 1, 5], vec![2.0, 3.0, 4.0], 7).unwrap();

        let sparse_dot = sparse_dot_product(&sparse_a, &sparse_b).unwrap();

        // Convert to dense and compute
        let dense_a = sparse_a.to_dense();
        let dense_b = sparse_b.to_dense();
        let dense_dot = dot_product(&dense_a, &dense_b).unwrap();

        assert!((sparse_dot - dense_dot).abs() < 1e-6);
    }

    #[test]
    fn test_sparse_vs_dense_cosine_similarity_equivalence() {
        let sparse_a = SparseVec::new(vec![0, 2, 4], vec![1.0, 2.0, 3.0], 6).unwrap();
        let sparse_b = SparseVec::new(vec![1, 2, 5], vec![1.0, 3.0, 2.0], 6).unwrap();

        let sparse_sim = sparse_cosine_similarity(&sparse_a, &sparse_b).unwrap();

        let dense_a = sparse_a.to_dense();
        let dense_b = sparse_b.to_dense();
        let dense_sim = cosine_similarity(&dense_a, &dense_b).unwrap();

        assert!((sparse_sim - dense_sim).abs() < 1e-6);
    }

    #[test]
    fn test_sparse_vs_dense_euclidean_distance_equivalence() {
        let sparse_a = SparseVec::new(vec![0, 3], vec![1.0, 2.0], 5).unwrap();
        let sparse_b = SparseVec::new(vec![1, 3], vec![1.0, 4.0], 5).unwrap();

        let sparse_dist = sparse_euclidean_distance(&sparse_a, &sparse_b).unwrap();

        let dense_a = sparse_a.to_dense();
        let dense_b = sparse_b.to_dense();
        let dense_dist = euclidean_distance(&dense_a, &dense_b).unwrap();

        assert!((sparse_dist - dense_dist).abs() < 1e-5);
    }
}
