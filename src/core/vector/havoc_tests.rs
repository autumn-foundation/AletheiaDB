//! Unit tests and test helpers for this module.
//!
//! # Why?
//! Provides test coverage for the associated module's logic.

#[cfg(test)]
mod tests {
    use crate::core::vector::simd;
    use std::mem::MaybeUninit;

    // AletheiaDB safely asserts length invariants inside these unsafe blocks!
    // The "havoc" persona explicitly ensures the panic catches the invalid arguments before unsafe memory access
    // as instructed by the repository AGENTS constraints on bounds testing with `catch_unwind`.

    #[test]
    fn test_dot_product_avx2_panic() {
        if !is_x86_feature_detected!("avx2") || !is_x86_feature_detected!("fma") {
            return;
        }
        let a = vec![1.0; 10];
        let b = vec![2.0; 5];

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            // AletheiaDB correctly has `assert_eq!(a.len(), b.len());` inside this unsafe function
            simd::x86_ops::dot_product_avx2(&a, &b)
        }));

        assert!(
            result.is_err(),
            "Should have panicked due to length mismatch instead of causing UB"
        );
    }

    #[test]
    fn test_scale_and_copy_avx2_panic() {
        if !is_x86_feature_detected!("avx2") {
            return;
        }
        let src = vec![1.0; 10];
        let mut dst = vec![0.0; 5];
        // We only allocate 5 elements but pass 10 elements in src.
        // AletheiaDB correctly has `assert_eq!(src.len(), dst.len());` inside this unsafe function
        let dst_slice =
            unsafe { std::slice::from_raw_parts_mut(dst.as_mut_ptr() as *mut MaybeUninit<f32>, 5) };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            simd::x86_ops::scale_and_copy_avx2(&src, dst_slice, 2.0)
        }));

        assert!(
            result.is_err(),
            "Should have panicked due to length mismatch instead of causing UB"
        );
    }
}
