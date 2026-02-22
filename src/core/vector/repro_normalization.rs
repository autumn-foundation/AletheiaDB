#[cfg(test)]
mod tests {
    use crate::core::vector::ops::*;

    #[test]
    fn test_normalize_small_vector() {
        // 1e-8 is small but valid (sq_mag 1e-16)
        // Current threshold is 1e-14, so this returns zero vector.
        let v = vec![1e-8, 0.0];
        let normalized = normalize(&v);

        // Should be [1.0, 0.0]
        assert!((normalized[0] - 1.0).abs() < 1e-6, "Small vector normalization failed: {:?}", normalized);
    }

    #[test]
    fn test_normalize_large_vector() {
        // 1e20 is large (sq_mag 1e40 -> Inf)
        // Returns zero vector due to inv_mag = 0.
        let v = vec![1e20, 0.0];
        let normalized = normalize(&v);

        // Should be [1.0, 0.0]
        assert!((normalized[0] - 1.0).abs() < 1e-6, "Large vector normalization failed: {:?}", normalized);
    }

    #[test]
    fn test_cosine_similarity_large_vector() {
        // 1e20 -> Inf dot product and magnitude
        let v = vec![1e20, 0.0];
        let result = cosine_similarity(&v, &v).unwrap();

        // Should be 1.0
        assert!((result - 1.0).abs() < 1e-6, "Large vector cosine similarity failed: {}", result);
    }
}
