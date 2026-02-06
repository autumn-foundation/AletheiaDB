#[cfg(test)]
mod tests {
    use aletheiadb::core::property::{MAX_VECTOR_DIMENSIONS, PropertyMapBuilder};
    use aletheiadb::utils::error::Error;

    #[test]
    fn test_try_insert_vector_returns_error_on_overflow() {
        // Warden: Verify that try_insert_vector now returns an error on large inputs
        // instead of panicking.
        let large_vector = vec![0.0; MAX_VECTOR_DIMENSIONS + 1];
        let result = PropertyMapBuilder::new().try_insert_vector("test", &large_vector);

        assert!(result.is_err());
        match result {
            Err(Error::Vector(aletheiadb::utils::error::VectorError::DimensionTooLarge {
                dimension,
                max_allowed,
            })) => {
                assert_eq!(dimension, MAX_VECTOR_DIMENSIONS + 1);
                assert_eq!(max_allowed, MAX_VECTOR_DIMENSIONS);
            }
            Ok(_) => panic!("Expected Error, got Ok(_)"),
            Err(e) => panic!("Expected VectorError::DimensionTooLarge, got {:?}", e),
        }
    }

    #[test]
    fn test_try_serialize_vector_into_returns_error_on_overflow() {
        use aletheiadb::core::property::try_serialize_vector_into;

        let large_vector = vec![0.0; MAX_VECTOR_DIMENSIONS + 1];
        let mut buffer = Vec::new();

        let result = try_serialize_vector_into(&large_vector, &mut buffer);

        assert!(result.is_err());
        match result {
            Err(Error::Vector(aletheiadb::utils::error::VectorError::DimensionTooLarge {
                dimension,
                max_allowed,
            })) => {
                assert_eq!(dimension, MAX_VECTOR_DIMENSIONS + 1);
                assert_eq!(max_allowed, MAX_VECTOR_DIMENSIONS);
            }
            Ok(_) => panic!("Expected Error, got Ok(_)"),
            Err(e) => panic!("Expected VectorError::DimensionTooLarge, got {:?}", e),
        }
    }
}
