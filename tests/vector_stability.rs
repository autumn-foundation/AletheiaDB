use aletheiadb::core::vector::ops::cosine_similarity;

#[test]
fn test_cosine_similarity_stability_with_subnormals() {
    // Vectors from repro_panic.rs that caused instability
    // a: [-8.161245e-22] -> mag_sq approx 6.6e-43
    // b: [-125.53673]
    //
    // The squared magnitude of 'a' (6.6e-43) is below SQUARED_MAGNITUDE_THRESHOLD (1e-25).
    // Therefore, the function should treat 'a' as a zero vector and return 0.0 immediately,
    // avoiding the unstable division that would produce > 1.0 results.

    let a = vec![-8.161245e-22f32];
    let b = vec![-125.53673f32];

    let sim = cosine_similarity(&a, &b).expect("Should compute similarity");

    println!("Computed similarity: {}", sim);

    // Expect 0.0 because 'a' is treated as zero vector
    assert_eq!(
        sim, 0.0,
        "Subnormal vector should be treated as zero to prevent instability"
    );
}

#[test]
fn test_cosine_similarity_nan_propagation() {
    let a = vec![f32::NAN, 1.0];
    let b = vec![1.0, 1.0];

    let sim = cosine_similarity(&a, &b).expect("Should compute (propagate NaN)");
    assert!(sim.is_nan());
}

#[test]
fn test_cosine_similarity_infinity() {
    let a = vec![f32::INFINITY, 0.0];
    let b = vec![1.0, 0.0];

    let sim = cosine_similarity(&a, &b).expect("Should compute");
    assert!(sim.is_nan());
}

#[test]
fn test_cosine_similarity_zero_vectors() {
    let zero = vec![0.0; 10];
    let b = vec![1.0; 10];

    let sim = cosine_similarity(&zero, &b).unwrap();
    assert_eq!(sim, 0.0);

    let sim_both_zero = cosine_similarity(&zero, &zero).unwrap();
    assert_eq!(sim_both_zero, 0.0);
}

#[test]
fn test_cosine_similarity_small_vectors_under_threshold() {
    // SQUARED_MAGNITUDE_THRESHOLD is 1e-30
    let tiny_val = 1.0e-16; // sq mag = 1e-32 < 1e-30
    let tiny = vec![tiny_val];
    let normal = vec![1.0];

    let sim = cosine_similarity(&tiny, &normal).unwrap();
    assert_eq!(sim, 0.0, "Tiny vector should be treated as zero");
}
