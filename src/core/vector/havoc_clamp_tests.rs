#[cfg(test)]
mod havoc_tests {
    use crate::core::vector::ops::{cosine_similarity, cosine_similarity_normalized};

    #[test]
    fn test_havoc_clamp_panic_normalized() {
        // Trigger NaN in dot_product which causes panic in clamp
        let a = [f32::NAN];
        let b = [1.0];

        let result = std::panic::catch_unwind(|| {
            let _ = cosine_similarity_normalized(&a, &b);
        });

        // Assert it doesn't panic
        assert!(
            result.is_ok(),
            "cosine_similarity_normalized panicked on NaN input"
        );
    }

    #[test]
    fn test_havoc_clamp_panic_regular() {
        // Trigger NaN in cosine_similarity which causes panic in clamp
        let a = [f32::NAN];
        let b = [1.0];

        let result = std::panic::catch_unwind(|| {
            let _ = cosine_similarity(&a, &b);
        });

        // Assert it doesn't panic
        assert!(result.is_ok(), "cosine_similarity panicked on NaN input");
    }
}
