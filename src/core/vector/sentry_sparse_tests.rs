#[cfg(test)]
mod tests {
    use crate::core::error::{Error, VectorError};
    use crate::core::vector::SparseVec;
    use crate::core::vector::{
        sparse_cosine_similarity, sparse_dot_product, sparse_euclidean_distance,
        sparse_squared_euclidean_distance,
    };

    #[test]
    fn test_negative_distance_regression() {
        let seed = 42;
        let size = 1000;
        let indices: Vec<u32> = (0..size).map(|i| i as u32).collect();
        let values: Vec<f32> = (0..size)
            .map(|i| {
                let mut x = i as f32 * (seed as f32 + 1.0);
                x = x % 1000.0 + 0.1;
                x
            })
            .collect();

        let vec = SparseVec::new(indices, values, size as u32).unwrap();
        let dist = sparse_euclidean_distance(&vec, &vec).unwrap();
        assert!(!dist.is_nan(), "Euclidean distance should not be NaN");
        assert!(dist >= 0.0);
    }

    #[test]
    fn test_sparse_euclidean_distance_nan_regression() {
        let seed = 34;
        let size = 1000;
        let indices: Vec<u32> = (0..size).map(|i| i as u32).collect();
        let values: Vec<f32> = (0..size)
            .map(|i| {
                let mut x = i as f32 * (seed as f32 + 1.0);
                x = x % 1000.0 + 0.1;
                x
            })
            .collect();

        let vec = SparseVec::new(indices, values, size as u32).unwrap();
        let dist = sparse_euclidean_distance(&vec, &vec).unwrap();

        assert!(!dist.is_nan(), "Euclidean distance should not be NaN");
        assert!(dist >= 0.0);
    }

    #[test]
    fn test_sparse_squared_euclidean_distance_correctness() {
        let a = SparseVec::new(vec![0, 2], vec![1.0, 3.0], 5).unwrap();
        let b = SparseVec::new(vec![0, 3], vec![2.0, 4.0], 5).unwrap();
        let dist = sparse_squared_euclidean_distance(&a, &b).unwrap();
        assert!((dist - 26.0).abs() < 1e-6);
    }

    #[test]
    fn test_nnz_and_accessors() {
        let sv = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
        assert_eq!(sv.nnz(), 2);
        assert_eq!(sv.dimension(), 5);
        assert_eq!(sv.indices(), &[0, 2]);
        assert_eq!(sv.values(), &[1.0, 2.0]);
    }

    #[test]
    fn test_to_dense() {
        let sv = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
        assert_eq!(sv.to_dense(), vec![1.0, 0.0, 2.0, 0.0, 0.0]);
    }

    #[test]
    fn test_magnitudes() {
        let sv = SparseVec::new(vec![0, 2], vec![3.0, 4.0], 5).unwrap();
        assert_eq!(sv.squared_magnitude(), 25.0);
        assert_eq!(sv.magnitude(), 5.0);
    }

    #[test]
    fn test_approx_eq() {
        let a = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
        let b = SparseVec::new(vec![0, 2], vec![1.0000001, 2.0000001], 5).unwrap();
        assert!(a.approx_eq(&b, 1e-5));
        assert!(!a.approx_eq(&b, 1e-10));
    }

    #[test]
    fn test_approx_eq_mismatch() {
        let a = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
        let dim_diff = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 6).unwrap();
        let idx_diff = SparseVec::new(vec![0, 3], vec![1.0, 2.0], 5).unwrap();
        let len_diff = SparseVec::new(vec![0, 2, 4], vec![1.0, 2.0, 3.0], 5).unwrap();

        assert!(!a.approx_eq(&dim_diff, 1e-5));
        assert!(!a.approx_eq(&idx_diff, 1e-5));
        assert!(!a.approx_eq(&len_diff, 1e-5));
    }

    #[test]
    fn test_validate_value_errors() {
        let res_nan = SparseVec::new(vec![0], vec![f32::NAN], 5);
        assert!(matches!(
            res_nan.unwrap_err(),
            Error::Vector(VectorError::ContainsNaN { .. })
        ));

        let res_inf = SparseVec::new(vec![0], vec![f32::INFINITY], 5);
        assert!(matches!(
            res_inf.unwrap_err(),
            Error::Vector(VectorError::ContainsInfinity { .. })
        ));

        let res_zero = SparseVec::new(vec![0], vec![0.0], 5);
        assert!(matches!(
            res_zero.unwrap_err(),
            Error::Vector(VectorError::InvalidSparseVector { .. })
        ));

        let res_nan2 = SparseVec::new(vec![0, 1], vec![1.0, f32::NAN], 5);
        assert!(matches!(
            res_nan2.unwrap_err(),
            Error::Vector(VectorError::ContainsNaN { .. })
        ));

        let res_inf2 = SparseVec::new(vec![0, 1], vec![1.0, f32::INFINITY], 5);
        assert!(matches!(
            res_inf2.unwrap_err(),
            Error::Vector(VectorError::ContainsInfinity { .. })
        ));

        let res_zero2 = SparseVec::new(vec![0, 1], vec![1.0, 0.0], 5);
        assert!(matches!(
            res_zero2.unwrap_err(),
            Error::Vector(VectorError::InvalidSparseVector { .. })
        ));

        let res_nan_slow = SparseVec::new(vec![1, 0], vec![f32::NAN, 1.0], 5);
        assert!(matches!(
            res_nan_slow.unwrap_err(),
            Error::Vector(VectorError::ContainsNaN { .. })
        ));

        let res_inf_slow = SparseVec::new(vec![1, 0], vec![f32::INFINITY, 1.0], 5);
        assert!(matches!(
            res_inf_slow.unwrap_err(),
            Error::Vector(VectorError::ContainsInfinity { .. })
        ));

        let res_zero_slow = SparseVec::new(vec![1, 0], vec![0.0, 1.0], 5);
        assert!(matches!(
            res_zero_slow.unwrap_err(),
            Error::Vector(VectorError::InvalidSparseVector { .. })
        ));
    }

    #[test]
    fn test_duplicate_index_error() {
        let res = SparseVec::new(vec![0, 0], vec![1.0, 2.0], 5);
        assert!(matches!(
            res.unwrap_err(),
            Error::Vector(VectorError::InvalidSparseVector { .. })
        ));

        let res_slow = SparseVec::new(vec![1, 1], vec![1.0, 2.0], 5);
        assert!(matches!(
            res_slow.unwrap_err(),
            Error::Vector(VectorError::InvalidSparseVector { .. })
        ));
    }

    #[test]
    fn test_out_of_bounds_fast_path() {
        let res = SparseVec::new(vec![10], vec![1.0], 5);
        assert!(matches!(
            res.unwrap_err(),
            Error::Vector(VectorError::InvalidSparseVector { .. })
        ));
    }

    #[test]
    fn test_out_of_bounds_subsequent() {
        let res = SparseVec::new(vec![0, 10], vec![1.0, 2.0], 5);
        assert!(matches!(
            res.unwrap_err(),
            Error::Vector(VectorError::InvalidSparseVector { .. })
        ));
    }

    #[test]
    fn test_out_of_bounds_slow_path() {
        let res = SparseVec::new(vec![10, 0], vec![2.0, 1.0], 5);
        assert!(matches!(
            res.unwrap_err(),
            Error::Vector(VectorError::InvalidSparseVector { .. })
        ));
    }

    #[test]
    fn test_sparse_ops_basic() {
        let a = SparseVec::new(vec![0, 2], vec![1.0, 2.0], 5).unwrap();
        let b = SparseVec::new(vec![0, 2], vec![2.0, 3.0], 5).unwrap();

        assert_eq!(sparse_dot_product(&a, &b).unwrap(), 8.0);

        let sim = sparse_cosine_similarity(&a, &b).unwrap();
        assert!((sim - 0.99227786).abs() < 1e-5);

        let sq_dist = sparse_squared_euclidean_distance(&a, &b).unwrap();
        assert_eq!(sq_dist, 2.0);

        let dist = sparse_euclidean_distance(&a, &b).unwrap();
        assert!((dist - std::f32::consts::SQRT_2).abs() < 1e-5);
    }

    #[test]
    fn test_sparse_ops_branches() {
        let a = SparseVec::new(vec![0], vec![1.0], 5).unwrap();
        let b = SparseVec::new(vec![1], vec![2.0], 5).unwrap();
        assert_eq!(sparse_dot_product(&a, &b).unwrap(), 0.0);
        assert_eq!(sparse_cosine_similarity(&a, &b).unwrap(), 0.0);
        assert_eq!(sparse_squared_euclidean_distance(&a, &b).unwrap(), 5.0);
        assert!((sparse_euclidean_distance(&a, &b).unwrap() - 2.236068).abs() < 1e-5);

        let a = SparseVec::new(vec![1], vec![1.0], 5).unwrap();
        let b = SparseVec::new(vec![0], vec![2.0], 5).unwrap();
        assert_eq!(sparse_dot_product(&a, &b).unwrap(), 0.0);
        assert_eq!(sparse_cosine_similarity(&a, &b).unwrap(), 0.0);
        assert_eq!(sparse_squared_euclidean_distance(&a, &b).unwrap(), 5.0);
        assert!((sparse_euclidean_distance(&a, &b).unwrap() - 2.236068).abs() < 1e-5);

        let a = SparseVec::new(vec![0, 1], vec![1.0, 2.0], 5).unwrap();
        let b = SparseVec::new(vec![0, 1], vec![2.0, 3.0], 5).unwrap();
        assert_eq!(sparse_dot_product(&a, &b).unwrap(), 8.0);
        assert!((sparse_cosine_similarity(&a, &b).unwrap() - 0.99227786).abs() < 1e-5);
        assert_eq!(sparse_squared_euclidean_distance(&a, &b).unwrap(), 2.0);
        assert!(
            (sparse_euclidean_distance(&a, &b).unwrap() - std::f32::consts::SQRT_2).abs() < 1e-5
        );
    }

    #[test]
    fn test_sparse_ops_dimension_mismatch() {
        let a = SparseVec::new(vec![0], vec![1.0], 5).unwrap();
        let b = SparseVec::new(vec![0], vec![1.0], 6).unwrap();

        assert!(matches!(
            sparse_dot_product(&a, &b).unwrap_err(),
            Error::Vector(VectorError::DimensionMismatch { .. })
        ));
        assert!(matches!(
            sparse_cosine_similarity(&a, &b).unwrap_err(),
            Error::Vector(VectorError::DimensionMismatch { .. })
        ));
        assert!(matches!(
            sparse_squared_euclidean_distance(&a, &b).unwrap_err(),
            Error::Vector(VectorError::DimensionMismatch { .. })
        ));
        assert!(matches!(
            sparse_euclidean_distance(&a, &b).unwrap_err(),
            Error::Vector(VectorError::DimensionMismatch { .. })
        ));
    }

    #[test]
    fn test_sparse_dot_product_remainder() {
        let a = SparseVec::new(vec![0], vec![1.0], 5).unwrap();
        let b = SparseVec::new(vec![1, 2], vec![2.0, 3.0], 5).unwrap();
        assert_eq!(sparse_dot_product(&a, &b).unwrap(), 0.0);

        let a2 = SparseVec::new(vec![1, 2], vec![1.0, 2.0], 5).unwrap();
        let b2 = SparseVec::new(vec![0], vec![3.0], 5).unwrap();
        assert_eq!(sparse_dot_product(&a2, &b2).unwrap(), 0.0);
    }

    #[test]
    fn test_sparse_squared_euclidean_distance_remainder() {
        let a = SparseVec::new(vec![0], vec![2.0], 5).unwrap();
        let b = SparseVec::new(vec![1, 2], vec![3.0, 4.0], 5).unwrap();
        assert_eq!(sparse_squared_euclidean_distance(&a, &b).unwrap(), 29.0);

        let a2 = SparseVec::new(vec![1, 2], vec![3.0, 4.0], 5).unwrap();
        let b2 = SparseVec::new(vec![0], vec![2.0], 5).unwrap();
        assert_eq!(sparse_squared_euclidean_distance(&a2, &b2).unwrap(), 29.0);
    }

    #[test]
    fn test_sparse_cosine_similarity_edge_cases() {
        let a = SparseVec::new(vec![], vec![], 5).unwrap();
        let b = SparseVec::new(vec![1, 2], vec![3.0, 4.0], 5).unwrap();
        assert_eq!(sparse_cosine_similarity(&a, &b).unwrap(), 0.0);
        assert_eq!(sparse_cosine_similarity(&b, &a).unwrap(), 0.0);
    }
}
