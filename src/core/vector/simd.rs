// ============================================================================
// SIMD Support
// ============================================================================

use std::mem::MaybeUninit;

/// SIMD-accelerated vector operations for x86/x86_64 platforms.
///
/// Uses runtime feature detection to select the best available instruction set:
/// - AVX2 (256-bit vectors, 8 floats at a time)
/// - SSE2 (128-bit vectors, 4 floats at a time) - baseline for x86_64
/// - Scalar fallback for other platforms
///
/// # Performance Expectations
///
/// Compared to scalar implementation, expected speedups for vectors > 256 dimensions:
/// - **AVX2 + FMA**: ~5-8x speedup (processes 8 floats/cycle with fused multiply-add)
/// - **SSE2**: ~2-4x speedup (processes 4 floats/cycle)
///
/// For smaller vectors, SIMD overhead may reduce gains. The crossover point
/// where SIMD becomes beneficial is typically around 16-32 dimensions.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub(crate) mod x86_ops {
    use std::mem::MaybeUninit;
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    /// Computes dot product, magnitude_a², and magnitude_b² using AVX2.
    ///
    /// # Safety
    /// Caller must ensure:
    /// 1. AVX2 and FMA are available (checked via `is_x86_feature_detected!`).
    /// 2. `a.len() == b.len()`.
    #[target_feature(enable = "avx2", enable = "fma")]
    #[inline]
    pub unsafe fn dot_and_magnitudes_avx2(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
        // Warden: Enforce length equality to prevent buffer over-reads
        assert_eq!(a.len(), b.len());
        // SAFETY: The unsafe block is required by the `unsafe_op_in_unsafe_fn` lint.
        // The caller guarantees AVX2 and FMA are available via runtime feature detection.
        unsafe {
            let a_chunks = a.chunks_exact(8);
            let b_chunks = b.chunks_exact(8);
            let a_rem = a_chunks.remainder();
            let b_rem = b_chunks.remainder();

            // Accumulators for 8 floats at a time
            let mut dot_acc = _mm256_setzero_ps();
            let mut mag_a_acc = _mm256_setzero_ps();
            let mut mag_b_acc = _mm256_setzero_ps();

            // Process 8 floats at a time
            for (va_chunk, vb_chunk) in a_chunks.zip(b_chunks) {
                let va = _mm256_loadu_ps(va_chunk.as_ptr());
                let vb = _mm256_loadu_ps(vb_chunk.as_ptr());

                // Fused multiply-add for dot product and magnitudes
                dot_acc = _mm256_fmadd_ps(va, vb, dot_acc);
                mag_a_acc = _mm256_fmadd_ps(va, va, mag_a_acc);
                mag_b_acc = _mm256_fmadd_ps(vb, vb, mag_b_acc);
            }

            // Horizontal sum of 256-bit vectors
            let dot = horizontal_sum_avx(dot_acc);
            let mag_a = horizontal_sum_avx(mag_a_acc);
            let mag_b = horizontal_sum_avx(mag_b_acc);

            // Handle remainder with safe scalar operations.
            let mut dot_rem = 0.0f32;
            let mut mag_a_rem = 0.0f32;
            let mut mag_b_rem = 0.0f32;

            for (&ai, &bi) in a_rem.iter().zip(b_rem) {
                dot_rem += ai * bi;
                mag_a_rem += ai * ai;
                mag_b_rem += bi * bi;
            }

            (dot + dot_rem, mag_a + mag_a_rem, mag_b + mag_b_rem)
        }
    }

    /// Horizontal sum of 8 floats in AVX register.
    #[target_feature(enable = "avx2")]
    #[inline]
    unsafe fn horizontal_sum_avx(v: __m256) -> f32 {
        unsafe {
            // Add high 128 bits to low 128 bits
            let high = _mm256_extractf128_ps(v, 1);
            let low = _mm256_castps256_ps128(v);
            let sum128 = _mm_add_ps(high, low);

            // Continue with SSE horizontal add
            horizontal_sum_sse(sum128)
        }
    }

    /// Computes dot product, magnitude_a², and magnitude_b² using SSE2.
    ///
    /// # Safety
    /// Caller must ensure:
    /// 1. SSE2 is available (always true on x86_64).
    /// 2. `a.len() == b.len()`.
    #[target_feature(enable = "sse2")]
    #[inline]
    pub unsafe fn dot_and_magnitudes_sse2(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
        // Warden: Enforce length equality to prevent buffer over-reads
        assert_eq!(a.len(), b.len());
        // SAFETY: The unsafe block is required by the `unsafe_op_in_unsafe_fn` lint.
        // The caller guarantees SSE2 is available via runtime feature detection.
        unsafe {
            let a_chunks = a.chunks_exact(4);
            let b_chunks = b.chunks_exact(4);
            let a_rem = a_chunks.remainder();
            let b_rem = b_chunks.remainder();

            // Accumulators for 4 floats at a time
            let mut dot_acc = _mm_setzero_ps();
            let mut mag_a_acc = _mm_setzero_ps();
            let mut mag_b_acc = _mm_setzero_ps();

            // Process 4 floats at a time
            for (va_chunk, vb_chunk) in a_chunks.zip(b_chunks) {
                let va = _mm_loadu_ps(va_chunk.as_ptr());
                let vb = _mm_loadu_ps(vb_chunk.as_ptr());

                // Multiply and accumulate
                dot_acc = _mm_add_ps(dot_acc, _mm_mul_ps(va, vb));
                mag_a_acc = _mm_add_ps(mag_a_acc, _mm_mul_ps(va, va));
                mag_b_acc = _mm_add_ps(mag_b_acc, _mm_mul_ps(vb, vb));
            }

            // Horizontal sum of 128-bit vectors
            let dot = horizontal_sum_sse(dot_acc);
            let mag_a = horizontal_sum_sse(mag_a_acc);
            let mag_b = horizontal_sum_sse(mag_b_acc);

            // Handle remainder with safe scalar operations.
            let mut dot_rem = 0.0f32;
            let mut mag_a_rem = 0.0f32;
            let mut mag_b_rem = 0.0f32;

            for (&ai, &bi) in a_rem.iter().zip(b_rem) {
                dot_rem += ai * bi;
                mag_a_rem += ai * ai;
                mag_b_rem += bi * bi;
            }

            (dot + dot_rem, mag_a + mag_a_rem, mag_b + mag_b_rem)
        }
    }

    /// Horizontal sum of 4 floats in SSE register.
    #[target_feature(enable = "sse2")]
    #[inline]
    unsafe fn horizontal_sum_sse(v: __m128) -> f32 {
        // Sum pairs: [a+c, b+d, a+c, b+d]
        // SAFETY: We have #[target_feature(enable = "sse2")] so these intrinsics are safe
        let shuf = _mm_shuffle_ps(v, v, 0b10_11_00_01);
        let sum1 = _mm_add_ps(v, shuf);
        // Sum final pair
        let shuf2 = _mm_shuffle_ps(sum1, sum1, 0b00_00_11_10);
        let sum2 = _mm_add_ps(sum1, shuf2);
        _mm_cvtss_f32(sum2)
    }

    /// Computes dot product using AVX2.
    ///
    /// This is a dedicated dot-product-only function, more efficient than
    /// `dot_and_magnitudes_avx2` when magnitudes aren't needed.
    ///
    /// # Safety
    /// Caller must ensure:
    /// 1. AVX2 and FMA are available (checked via `is_x86_feature_detected!`).
    /// 2. `a.len() == b.len()`.
    #[target_feature(enable = "avx2", enable = "fma")]
    #[inline]
    pub unsafe fn dot_product_avx2(a: &[f32], b: &[f32]) -> f32 {
        // Warden: Enforce length equality to prevent buffer over-reads
        assert_eq!(a.len(), b.len());
        // SAFETY: The unsafe block is required by the `unsafe_op_in_unsafe_fn` lint.
        // The caller guarantees AVX2 and FMA are available via runtime feature detection.
        unsafe {
            let a_chunks = a.chunks_exact(8);
            let b_chunks = b.chunks_exact(8);
            let a_rem = a_chunks.remainder();
            let b_rem = b_chunks.remainder();

            // Accumulator for 8 floats at a time
            let mut acc = _mm256_setzero_ps();

            // Process 8 floats at a time
            for (va_chunk, vb_chunk) in a_chunks.zip(b_chunks) {
                let va = _mm256_loadu_ps(va_chunk.as_ptr());
                let vb = _mm256_loadu_ps(vb_chunk.as_ptr());

                // Fused multiply-add: acc = va * vb + acc
                acc = _mm256_fmadd_ps(va, vb, acc);
            }

            // Horizontal sum of 256-bit vector
            let mut sum = horizontal_sum_avx(acc);

            // Handle remainder with scalar operations
            for (&va, &vb) in a_rem.iter().zip(b_rem) {
                sum += va * vb;
            }

            sum
        }
    }

    /// Computes dot product using SSE2.
    ///
    /// This is a dedicated dot-product-only function, more efficient than
    /// `dot_and_magnitudes_sse2` when magnitudes aren't needed.
    ///
    /// # Safety
    /// Caller must ensure:
    /// 1. SSE2 is available (always true on x86_64).
    /// 2. `a.len() == b.len()`.
    #[target_feature(enable = "sse2")]
    #[inline]
    pub unsafe fn dot_product_sse2(a: &[f32], b: &[f32]) -> f32 {
        // Warden: Enforce length equality to prevent buffer over-reads
        assert_eq!(a.len(), b.len());
        // SAFETY: The unsafe block is required by the `unsafe_op_in_unsafe_fn` lint.
        // The caller guarantees SSE2 is available via runtime feature detection.
        unsafe {
            let a_chunks = a.chunks_exact(4);
            let b_chunks = b.chunks_exact(4);
            let a_rem = a_chunks.remainder();
            let b_rem = b_chunks.remainder();

            // Accumulator for 4 floats at a time
            let mut acc = _mm_setzero_ps();

            // Process 4 floats at a time
            for (va_chunk, vb_chunk) in a_chunks.zip(b_chunks) {
                let va = _mm_loadu_ps(va_chunk.as_ptr());
                let vb = _mm_loadu_ps(vb_chunk.as_ptr());

                // Multiply and accumulate
                acc = _mm_add_ps(acc, _mm_mul_ps(va, vb));
            }

            // Horizontal sum of 128-bit vector
            let mut sum = horizontal_sum_sse(acc);

            // Handle remainder with scalar operations
            for (&va, &vb) in a_rem.iter().zip(b_rem) {
                sum += va * vb;
            }

            sum
        }
    }

    /// Computes sum of squared differences using AVX2.
    ///
    /// # Safety
    /// Caller must ensure:
    /// 1. AVX2 and FMA are available (checked via `is_x86_feature_detected!`).
    /// 2. `a.len() == b.len()`.
    #[target_feature(enable = "avx2", enable = "fma")]
    #[inline]
    pub unsafe fn squared_diff_sum_avx2(a: &[f32], b: &[f32]) -> f32 {
        // Warden: Enforce length equality to prevent buffer over-reads
        assert_eq!(a.len(), b.len());
        // SAFETY: The unsafe block is required by the `unsafe_op_in_unsafe_fn` lint.
        // All unsafe operations within this unsafe fn must still be in an unsafe block.
        // The caller guarantees AVX2 and FMA are available via runtime feature detection.
        unsafe {
            let len = a.len();
            let chunks = len / 8;
            let remainder = len % 8;

            // Accumulator for 8 floats at a time
            let mut acc = _mm256_setzero_ps();

            let a_ptr = a.as_ptr();
            let b_ptr = b.as_ptr();

            // Process 8 floats at a time
            for i in 0..chunks {
                let offset = i * 8;
                let va = _mm256_loadu_ps(a_ptr.add(offset));
                let vb = _mm256_loadu_ps(b_ptr.add(offset));

                // Compute difference
                let diff = _mm256_sub_ps(va, vb);

                // Square and accumulate using FMA: acc = diff * diff + acc
                acc = _mm256_fmadd_ps(diff, diff, acc);
            }

            // Horizontal sum of 256-bit vector
            let mut sum = horizontal_sum_avx(acc);

            // Handle remainder with scalar operations
            let start = chunks * 8;
            for i in 0..remainder {
                let diff = a[start + i] - b[start + i];
                sum += diff * diff;
            }

            sum
        }
    }

    /// Computes sum of squared differences using SSE2.
    ///
    /// # Safety
    /// Caller must ensure:
    /// 1. SSE2 is available (always true on x86_64).
    /// 2. `a.len() == b.len()`.
    #[target_feature(enable = "sse2")]
    #[inline]
    pub unsafe fn squared_diff_sum_sse2(a: &[f32], b: &[f32]) -> f32 {
        // Warden: Enforce length equality to prevent buffer over-reads
        assert_eq!(a.len(), b.len());
        // SAFETY: The unsafe block is required by the `unsafe_op_in_unsafe_fn` lint.
        // All unsafe operations within this unsafe fn must still be in an unsafe block.
        // The caller guarantees SSE2 is available via runtime feature detection.
        unsafe {
            let len = a.len();
            let chunks = len / 4;
            let remainder = len % 4;

            // Accumulator for 4 floats at a time
            let mut acc = _mm_setzero_ps();

            let a_ptr = a.as_ptr();
            let b_ptr = b.as_ptr();

            // Process 4 floats at a time
            for i in 0..chunks {
                let offset = i * 4;
                let va = _mm_loadu_ps(a_ptr.add(offset));
                let vb = _mm_loadu_ps(b_ptr.add(offset));

                // Compute difference
                let diff = _mm_sub_ps(va, vb);

                // Square and accumulate
                acc = _mm_add_ps(acc, _mm_mul_ps(diff, diff));
            }

            // Horizontal sum of 128-bit vector
            let mut sum = horizontal_sum_sse(acc);

            // Handle remainder with scalar operations
            let start = chunks * 4;
            for i in 0..remainder {
                let diff = a[start + i] - b[start + i];
                sum += diff * diff;
            }

            sum
        }
    }

    /// Scales a vector in place by a scalar using AVX2.
    ///
    /// # Safety
    /// Caller must ensure AVX2 is available (checked via `is_x86_feature_detected!`).
    #[target_feature(enable = "avx2")]
    #[inline]
    pub unsafe fn scale_in_place_avx2(v: &mut [f32], scalar: f32) {
        // SAFETY: This unsafe block is required by the `unsafe_op_in_unsafe_fn` lint.
        // - The caller guarantees AVX2 is available via runtime feature detection.
        // - Pointer operations are safe because:
        //   1. chunks_exact_mut(8) guarantees each chunk has exactly 8 f32 elements
        //   2. chunk.as_ptr() returns a valid pointer to the chunk's contiguous memory
        //   3. chunk.as_mut_ptr() returns a valid mutable pointer to the same memory
        //   4. _mm256_loadu_ps/_mm256_storeu_ps handle unaligned access safely
        //   5. The slice owns the memory, so no aliasing occurs
        unsafe {
            let scalar_vec = _mm256_set1_ps(scalar);
            let mut chunks = v.chunks_exact_mut(8);

            // Process 8 floats at a time
            for chunk in chunks.by_ref() {
                let va = _mm256_loadu_ps(chunk.as_ptr());
                let result = _mm256_mul_ps(va, scalar_vec);
                _mm256_storeu_ps(chunk.as_mut_ptr(), result);
            }

            // Handle remainder with scalar operations
            for x in chunks.into_remainder() {
                *x *= scalar;
            }
        }
    }

    /// Scales a vector in place by a scalar using SSE2.
    ///
    /// # Safety
    /// Caller must ensure SSE2 is available (always true on x86_64).
    #[target_feature(enable = "sse2")]
    #[inline]
    pub unsafe fn scale_in_place_sse2(v: &mut [f32], scalar: f32) {
        // SAFETY: This unsafe block is required by the `unsafe_op_in_unsafe_fn` lint.
        // - The caller guarantees SSE2 is available via runtime feature detection.
        // - Pointer operations are safe because:
        //   1. chunks_exact_mut(4) guarantees each chunk has exactly 4 f32 elements
        //   2. chunk.as_ptr() returns a valid pointer to the chunk's contiguous memory
        //   3. chunk.as_mut_ptr() returns a valid mutable pointer to the same memory
        //   4. _mm_loadu_ps/_mm_storeu_ps handle unaligned access safely
        //   5. The slice owns the memory, so no aliasing occurs
        unsafe {
            let scalar_vec = _mm_set1_ps(scalar);
            let mut chunks = v.chunks_exact_mut(4);

            // Process 4 floats at a time
            for chunk in chunks.by_ref() {
                let va = _mm_loadu_ps(chunk.as_ptr());
                let result = _mm_mul_ps(va, scalar_vec);
                _mm_storeu_ps(chunk.as_mut_ptr(), result);
            }

            // Handle remainder with scalar operations
            for x in chunks.into_remainder() {
                *x *= scalar;
            }
        }
    }

    /// Scales `src` by `scalar` and stores result in `dst` using AVX2.
    ///
    /// # Safety
    /// Caller must ensure AVX2 is available. `src.len() == dst.len()`.
    #[target_feature(enable = "avx2")]
    #[inline]
    pub unsafe fn scale_and_copy_avx2(src: &[f32], dst: &mut [MaybeUninit<f32>], scalar: f32) {
        assert_eq!(src.len(), dst.len());
        unsafe {
            let scalar_vec = _mm256_set1_ps(scalar);
            let mut src_chunks = src.chunks_exact(8);
            let mut dst_chunks = dst.chunks_exact_mut(8);

            for (s_chunk, d_chunk) in src_chunks.by_ref().zip(dst_chunks.by_ref()) {
                let va = _mm256_loadu_ps(s_chunk.as_ptr());
                let result = _mm256_mul_ps(va, scalar_vec);
                _mm256_storeu_ps(d_chunk.as_mut_ptr() as *mut f32, result);
            }

            let src_rem = src_chunks.remainder();
            let dst_rem = dst_chunks.into_remainder();

            for (s, d) in src_rem.iter().zip(dst_rem.iter_mut()) {
                d.write(*s * scalar);
            }
        }
    }

    /// Scales `src` by `scalar` and stores result in `dst` using SSE2.
    ///
    /// # Safety
    /// Caller must ensure SSE2 is available. `src.len() == dst.len()`.
    #[target_feature(enable = "sse2")]
    #[inline]
    pub unsafe fn scale_and_copy_sse2(src: &[f32], dst: &mut [MaybeUninit<f32>], scalar: f32) {
        assert_eq!(src.len(), dst.len());
        unsafe {
            let scalar_vec = _mm_set1_ps(scalar);
            let mut src_chunks = src.chunks_exact(4);
            let mut dst_chunks = dst.chunks_exact_mut(4);

            for (s_chunk, d_chunk) in src_chunks.by_ref().zip(dst_chunks.by_ref()) {
                let va = _mm_loadu_ps(s_chunk.as_ptr());
                let result = _mm_mul_ps(va, scalar_vec);
                _mm_storeu_ps(d_chunk.as_mut_ptr() as *mut f32, result);
            }
            let src_rem = src_chunks.remainder();
            let dst_rem = dst_chunks.into_remainder();

            for (s, d) in src_rem.iter().zip(dst_rem.iter_mut()) {
                d.write(*s * scalar);
            }
        }
    }
}

/// Scalar fallback for computing dot product and magnitudes.
///
/// Used on non-x86 platforms or ancient x86 CPUs without SSE2.
#[inline]
#[cfg_attr(
    all(any(target_arch = "x86", target_arch = "x86_64"), not(miri)),
    allow(dead_code)
)]
pub(crate) fn dot_and_magnitudes_scalar(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
    a.iter().zip(b.iter()).fold(
        (0.0f32, 0.0f32, 0.0f32),
        |(dot, mag_a, mag_b), (&ai, &bi)| (dot + ai * bi, mag_a + ai * ai, mag_b + bi * bi),
    )
}

/// Scalar fallback for computing sum of squared differences.
///
/// Used on non-x86 platforms or ancient x86 CPUs without SSE2.
#[inline]
#[cfg_attr(
    all(any(target_arch = "x86", target_arch = "x86_64"), not(miri)),
    allow(dead_code)
)]
pub(crate) fn squared_diff_sum_scalar(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(&ai, &bi)| {
            let diff = ai - bi;
            diff * diff
        })
        .sum()
}

/// Scalar fallback for computing dot product.
///
/// Used on non-x86 platforms or ancient x86 CPUs without SSE2.
#[inline]
#[cfg_attr(
    all(any(target_arch = "x86", target_arch = "x86_64"), not(miri)),
    allow(dead_code)
)]
pub(crate) fn dot_product_scalar(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(&ai, &bi)| ai * bi).sum()
}

/// Scalar fallback for scaling a vector in place.
///
/// Used on non-x86 platforms or ancient x86 CPUs without SSE2.
#[inline]
#[cfg_attr(
    all(any(target_arch = "x86", target_arch = "x86_64"), not(miri)),
    allow(dead_code)
)]
pub(crate) fn scale_in_place_scalar(v: &mut [f32], scalar: f32) {
    for x in v.iter_mut() {
        *x *= scalar;
    }
}

/// Scales `src` by `scalar` and stores result in `dst` (scalar fallback).
#[inline]
#[cfg_attr(
    all(any(target_arch = "x86", target_arch = "x86_64"), not(miri)),
    allow(dead_code)
)]
pub(crate) fn scale_and_copy_scalar(src: &[f32], dst: &mut [MaybeUninit<f32>], scalar: f32) {
    assert_eq!(src.len(), dst.len());
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        d.write(*s * scalar);
    }
}

/// Scales a vector in place using the best available SIMD instructions.
///
/// Uses runtime feature detection to select:
/// - AVX2 on x86/x86_64 when available
/// - SSE2 on x86/x86_64 as fallback (baseline for x86_64)
/// - Scalar implementation on other platforms
#[inline(always)]
pub(crate) fn scale_in_place(v: &mut [f32], scalar: f32) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        // Use runtime detection for best available instruction set.
        if is_x86_feature_detected!("avx2") {
            // SAFETY: We just verified AVX2 is available.
            unsafe {
                x86_ops::scale_in_place_avx2(v, scalar);
            }
            return;
        }
        if is_x86_feature_detected!("sse2") {
            // SAFETY: We just verified SSE2 is available. SSE2 is a baseline
            // requirement for x86_64, so this check is mainly for 32-bit x86.
            unsafe {
                x86_ops::scale_in_place_sse2(v, scalar);
            }
            return;
        }
    }

    // Fallback for non-x86 platforms or x86 CPUs without SSE2.
    scale_in_place_scalar(v, scalar);
}

/// Scales `src` by `scalar` and stores result in `dst` using the best available SIMD instructions.
#[inline(always)]
pub(crate) fn scale_and_copy(src: &[f32], dst: &mut [MaybeUninit<f32>], scalar: f32) {
    assert_eq!(src.len(), dst.len());
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        // Use runtime detection for best available instruction set.
        if is_x86_feature_detected!("avx2") {
            // SAFETY: We just verified AVX2 is available.
            unsafe {
                x86_ops::scale_and_copy_avx2(src, dst, scalar);
            }
            return;
        }
        if is_x86_feature_detected!("sse2") {
            // SAFETY: We just verified SSE2 is available.
            unsafe {
                x86_ops::scale_and_copy_sse2(src, dst, scalar);
            }
            return;
        }
    }

    // Fallback for non-x86 platforms or x86 CPUs without SSE2.
    scale_and_copy_scalar(src, dst, scalar);
}

/// Computes dot product and both squared magnitudes using the best available
/// SIMD instructions.
///
/// Uses runtime feature detection to select:
/// - AVX2 with FMA on x86/x86_64 when available
/// - SSE2 on x86/x86_64 as fallback (baseline for x86_64)
/// - Scalar implementation on other platforms
#[inline]
pub(crate) fn dot_and_magnitudes(a: &[f32], b: &[f32]) -> (f32, f32, f32) {
    assert_eq!(a.len(), b.len());
    #[cfg(target_arch = "x86_64")]
    {
        // Use runtime detection for best available instruction set
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            // SAFETY: We just verified AVX2 and FMA are available
            return unsafe { x86_ops::dot_and_magnitudes_avx2(a, b) };
        }

        // SAFETY: SSE2 is always available on x86_64 (baseline requirement)
        unsafe { x86_ops::dot_and_magnitudes_sse2(a, b) }
    }

    #[cfg(target_arch = "x86")]
    {
        // Use runtime detection for best available instruction set
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            // SAFETY: We just verified AVX2 and FMA are available
            return unsafe { x86_ops::dot_and_magnitudes_avx2(a, b) };
        }

        if is_x86_feature_detected!("sse2") {
            // SAFETY: We just verified SSE2 is available
            return unsafe { x86_ops::dot_and_magnitudes_sse2(a, b) };
        }

        // Fall through to scalar on ancient x86 without SSE2
        return dot_and_magnitudes_scalar(a, b);
    }

    // Fallback for non-x86 platforms
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    dot_and_magnitudes_scalar(a, b)
}

/// Computes sum of squared differences using the best available SIMD instructions.
///
/// Uses runtime feature detection to select:
/// - AVX2 with FMA on x86/x86_64 when available
/// - SSE2 on x86/x86_64 as fallback (baseline for x86_64)
/// - Scalar implementation on other platforms
#[inline]
pub(crate) fn squared_diff_sum(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        // Use runtime detection for best available instruction set.
        // The order of checks is from most to least performant.
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            // SAFETY: We just verified AVX2 and FMA are available.
            return unsafe { x86_ops::squared_diff_sum_avx2(a, b) };
        }
        if is_x86_feature_detected!("sse2") {
            // SAFETY: We just verified SSE2 is available. SSE2 is a baseline
            // requirement for x86_64, so this check is mainly for 32-bit x86.
            return unsafe { x86_ops::squared_diff_sum_sse2(a, b) };
        }
    }

    // Fallback for non-x86 platforms or x86 CPUs without SSE2.
    squared_diff_sum_scalar(a, b)
}

/// Computes dot product using the best available SIMD instructions.
///
/// Uses runtime feature detection to select:
/// - AVX2 with FMA on x86/x86_64 when available
/// - SSE2 on x86/x86_64 as fallback (baseline for x86_64)
/// - Scalar implementation on other platforms
#[inline]
pub(crate) fn dot_product_sum(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        // Use runtime detection for best available instruction set.
        // The order of checks is from most to least performant.
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            // SAFETY: We just verified AVX2 and FMA are available.
            return unsafe { x86_ops::dot_product_avx2(a, b) };
        }
        if is_x86_feature_detected!("sse2") {
            // SAFETY: We just verified SSE2 is available. SSE2 is a baseline
            // requirement for x86_64, so this check is mainly for 32-bit x86.
            return unsafe { x86_ops::dot_product_sse2(a, b) };
        }
    }

    // Fallback for non-x86 platforms or x86 CPUs without SSE2.
    dot_product_scalar(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scale_and_copy_implementation_coverage() {
        let src = vec![1.0f32; 17]; // 17 to force remainder logic (8*2 + 1)
        let mut dst = vec![0.0f32; 17];
        let scalar = 2.0;
        let expected = vec![2.0f32; 17];

        // Helper to get mutable uninit slice from vec
        let as_uninit = |v: &mut Vec<f32>| -> &mut [MaybeUninit<f32>] {
            unsafe {
                std::slice::from_raw_parts_mut(
                    v.as_mut_ptr() as *mut MaybeUninit<f32>,
                    v.len(),
                )
            }
        };

        // 1. Unconditional Scalar coverage
        scale_and_copy_scalar(&src, as_uninit(&mut dst), scalar);
        assert_eq!(dst, expected, "Scalar implementation failed");
        dst.fill(0.0);

        // 2. Conditional x86 SIMD coverage
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            // Try SSE2 if available
            if is_x86_feature_detected!("sse2") {
                unsafe { x86_ops::scale_and_copy_sse2(&src, as_uninit(&mut dst), scalar) };
                assert_eq!(dst, expected, "SSE2 implementation failed");
                dst.fill(0.0);
            }

            // Try AVX2 if available
            if is_x86_feature_detected!("avx2") {
                unsafe { x86_ops::scale_and_copy_avx2(&src, as_uninit(&mut dst), scalar) };
                assert_eq!(dst, expected, "AVX2 implementation failed");
                dst.fill(0.0);
            }
        }
    }

    #[test]
    fn test_scale_in_place_implementation_coverage() {
        let v = vec![1.0f32; 17];
        let scalar = 2.0;
        let expected = vec![2.0f32; 17];

        // 1. Unconditional Scalar coverage
        let mut v_scalar = v.clone();
        scale_in_place_scalar(&mut v_scalar, scalar);
        assert_eq!(v_scalar, expected, "Scalar implementation failed");

        // 2. Conditional x86 SIMD coverage
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            // Try SSE2
            if is_x86_feature_detected!("sse2") {
                let mut v_sse2 = v.clone();
                unsafe { x86_ops::scale_in_place_sse2(&mut v_sse2, scalar) };
                assert_eq!(v_sse2, expected, "SSE2 implementation failed");
            }

            // Try AVX2
            if is_x86_feature_detected!("avx2") {
                let mut v_avx2 = v.clone();
                unsafe { x86_ops::scale_in_place_avx2(&mut v_avx2, scalar) };
                assert_eq!(v_avx2, expected, "AVX2 implementation failed");
            }
        }
    }
}
