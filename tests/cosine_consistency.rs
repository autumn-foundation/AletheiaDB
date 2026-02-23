use aletheiadb::core::vector::ops::{cosine_similarity, cosine_similarity_normalized, normalize};

#[test]
fn test_small_vector_consistency() {
    // Vector with squared magnitude significantly below SQUARED_MAGNITUDE_THRESHOLD (1e-14).
    // e.g. magnitude 1e-10 -> squared 1e-20
    let small_vec = vec![1e-10f32];
    let normal_vec = vec![1.0f32];

    // normalize(small_vec) should return a zero vector because magnitude is too small.
    let small_norm = normalize(&small_vec);
    assert_eq!(
        small_norm,
        vec![0.0f32],
        "normalize should return zero vector for small magnitude input"
    );

    // cosine_similarity(small_vec, normal_vec) currently returns ~1.0 (if collinear) or whatever.
    // After fix, it should return 0.0 because small_vec is effectively zero.
    let sim_raw = cosine_similarity(&small_vec, &normal_vec).unwrap();

    // cosine_similarity_normalized(small_norm, normal_vec) currently panics in debug because small_norm is 0 vector.
    // After fix, it should return 0.0 without panic.
    let normal_norm = normalize(&normal_vec);
    let sim_norm = cosine_similarity_normalized(&small_norm, &normal_norm).unwrap();

    println!("sim_raw: {}, sim_norm: {}", sim_raw, sim_norm);

    // Assert consistency: raw similarity should match normalized similarity (both 0.0).
    assert!(
        (sim_raw - sim_norm).abs() < 1e-6,
        "Raw similarity {} does not match normalized similarity {}",
        sim_raw,
        sim_norm
    );

    // Specifically, both should be 0.0
    assert_eq!(
        sim_raw, 0.0,
        "cosine_similarity should return 0.0 for small vectors"
    );
    assert_eq!(
        sim_norm, 0.0,
        "cosine_similarity_normalized should return 0.0 for zero vectors"
    );
}

#[test]
fn test_small_vector_pair_consistency() {
    // Both vectors small
    let a = vec![1e-10f32];
    let b = vec![1e-10f32];

    let sim_raw = cosine_similarity(&a, &b).unwrap();
    let sim_norm = cosine_similarity_normalized(&normalize(&a), &normalize(&b)).unwrap();

    assert_eq!(
        sim_raw, 0.0,
        "cosine_similarity should return 0.0 for small vector pair"
    );
    assert_eq!(
        sim_norm, 0.0,
        "cosine_similarity_normalized should return 0.0 for zero vector pair"
    );
}
