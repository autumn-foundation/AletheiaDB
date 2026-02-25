use aletheiadb::core::vector::ops::{cosine_similarity, cosine_similarity_normalized, normalize};

#[test]
fn test_small_vector_consistency() {
    // Vector with squared magnitude 1e-20.
    // Previous threshold was 1e-14 (treated as zero).
    // New threshold is 1e-25 (treated as valid).
    let small_vec = vec![1e-10f32];
    let normal_vec = vec![1.0f32];

    // normalize(small_vec) should return a unit vector.
    let small_norm = normalize(&small_vec);
    assert_eq!(
        small_norm,
        vec![1.0f32],
        "normalize should return unit vector for small valid magnitude input"
    );

    // cosine_similarity(small_vec, normal_vec) should return 1.0 (collinear).
    let sim_raw = cosine_similarity(&small_vec, &normal_vec).unwrap();

    // cosine_similarity_normalized should also return 1.0.
    let normal_norm = normalize(&normal_vec);
    let sim_norm = cosine_similarity_normalized(&small_norm, &normal_norm).unwrap();

    println!("sim_raw: {}, sim_norm: {}", sim_raw, sim_norm);

    // Assert consistency: raw similarity should match normalized similarity (both 1.0).
    assert!(
        (sim_raw - sim_norm).abs() < 1e-6,
        "Raw similarity {} does not match normalized similarity {}",
        sim_raw,
        sim_norm
    );

    // Specifically, both should be 1.0
    assert_eq!(
        sim_raw, 1.0,
        "cosine_similarity should return 1.0 for small collinear vectors"
    );
    assert_eq!(
        sim_norm, 1.0,
        "cosine_similarity_normalized should return 1.0 for normalized vectors"
    );
}

#[test]
fn test_small_vector_pair_consistency() {
    // Both vectors small
    let a = vec![1e-10f32];
    let b = vec![1e-10f32];

    let sim_raw = cosine_similarity(&a, &b).unwrap();
    // normalize(a) -> [1.0]
    let sim_norm = cosine_similarity_normalized(&normalize(&a), &normalize(&b)).unwrap();

    assert_eq!(
        sim_raw, 1.0,
        "cosine_similarity should return 1.0 for small vector pair"
    );
    assert_eq!(
        sim_norm, 1.0,
        "cosine_similarity_normalized should return 1.0 for normalized pair"
    );
}
