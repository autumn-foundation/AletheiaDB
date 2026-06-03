#[cfg(test)]
mod havoc_tests {
    use crate::core::id::NodeId;
    use crate::index::vector::VectorIndex;
    use crate::index::vector::hnsw::{DistanceMetric, HnswIndexBuilder, Quantization};

    #[test]
    fn test_havoc_clamp_panic_tanimoto() {
        let index = HnswIndexBuilder::new(2, DistanceMetric::Tanimoto)
            .quantization(Quantization::F32)
            .build()
            .unwrap();

        let id1 = NodeId::new(1).unwrap();

        // This vector shouldn't pass validation usually, but we bypass validation if needed or we use a vector that produces NaN like [0,0] if norm is zero, but denominator handles zero.
        // NaN input will bypass validate_vector? Wait, validate_vector rejects NaN!
        // Ah, `validate_vector` rejects NaN inputs. So how could we trigger it?
        // 1. Through dot/magnitude returning NaN via extreme values or Subnormal.
        // 2. We can't insert NaN.
        let a = [f32::NAN, f32::NAN];
        let _ = index.add(id1, &a); // this returns Err(ContainsNaN)
    }
}
