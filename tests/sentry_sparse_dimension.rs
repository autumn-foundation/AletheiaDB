#[cfg(test)]
mod sentry_sparse_dimension {
    use aletheiadb::core::vector::MAX_VECTOR_DIMENSIONS;
    use aletheiadb::core::vector::SparseVec;

    #[test]
    fn test_sparse_vec_large_dimension_failure() {
        // Try to create a sparse vector with dimension > 100,000
        // This simulates a large vocabulary (e.g., 200k tokens)
        // with few non-zero elements.
        let dimension = MAX_VECTOR_DIMENSIONS as u32 + 100;
        let indices = vec![0, 100, 200];
        let values = vec![1.0, 0.5, 0.2];

        let result = SparseVec::new(indices, values, dimension);

        // Currently this fails, which is a limitation.
        // If we fix it, this test should pass (return Ok).
        // For now, we assert it fails to confirm the limitation.
        assert!(
            result.is_err(),
            "SparseVec should reject dimension > 100,000 currently"
        );

        let err = result.unwrap_err();
        let err_str = format!("{}", err);
        assert!(
            err_str.contains("dimension"),
            "Error should mention dimension"
        );
    }
}
