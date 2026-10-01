//! Tests for the SF1 preset and independent vector-scale overrides.
//!
//! These exercise parametric scaling (Issue #3628): a larger `Sf1` preset and
//! the ability to dial the vector-extension corpus (count + dimensionality)
//! independently of the SNB graph scale. Heavy scales (e.g. the 1M-vector
//! target) are asserted at the *parameter* level so nothing large is ever
//! materialized in a test.

use aletheia_bench_ldbc::generator::{GenConfigError, Scale, generate, generate_with_overrides};

#[test]
fn sf1_scale_parses_and_labels() {
    assert_eq!(Scale::parse("sf1"), Some(Scale::Sf1));
    assert_eq!(Scale::parse("SF1"), Some(Scale::Sf1));
    assert_eq!(Scale::Sf1.label(), "sf1");
}

#[test]
fn sf1_preset_is_larger_than_sf01() {
    let s1 = Scale::Sf1.params();
    let s01 = Scale::Sf01.params();
    assert!(
        s1.persons > s01.persons,
        "SF1 must have more persons than SF0.1 ({} vs {})",
        s1.persons,
        s01.persons
    );
    assert!(s1.forums > s01.forums, "SF1 must have more forums");
    assert!(s1.tags > s01.tags, "SF1 must have more tags");
    // LDBC SF1 is ~10x SF0.1; SF0.1 preset is 1,500 persons.
    assert_eq!(s1.persons, 15_000, "SF1 persons should be ~10x SF0.1");
}

#[test]
fn sf1_generate_matches_params() {
    let g = generate(Scale::Sf1, 3);
    let p = Scale::Sf1.params();
    assert_eq!(g.scale, "sf1");
    assert_eq!(g.persons.len(), p.persons);
    assert_eq!(g.forums.len(), p.forums);
    assert_eq!(g.posts.len(), p.forums * p.posts_per_forum);
    assert_eq!(g.tags.len(), p.tags);
    assert!(
        g.knows.len() > Scale::Sf01.params().persons,
        "SF1 KNOWS edge count should dwarf SF0.1 person count"
    );
}

#[test]
fn vector_dim_override_changes_generated_dim() {
    let p = Scale::Smoke
        .params_with_overrides(None, Some(256))
        .expect("dim 256 is valid");
    assert_eq!(p.embedding_dim, 256);

    // A tiny materialized run: dedicated corpus + post embeddings adopt the dim.
    let g = generate_with_overrides(Scale::Smoke, 9, Some(20), Some(16)).expect("valid overrides");
    assert_eq!(
        g.vectors.len(),
        20,
        "corpus size must equal requested count"
    );
    for v in &g.vectors {
        assert_eq!(v.embedding.len(), 16, "corpus vector adopts overridden dim");
    }
    for post in &g.posts {
        assert_eq!(
            post.embedding.len(),
            16,
            "post embedding adopts overridden dim"
        );
    }
}

#[test]
fn vector_count_override_records_requested_without_materializing() {
    // Assert the requested count is recorded at the parameter level WITHOUT
    // ever generating a million vectors (no allocation, no disk).
    let p = Scale::Sf01
        .params_with_overrides(Some(1_000_000), Some(384))
        .expect("valid overrides");
    assert_eq!(p.vector_count, 1_000_000);
    assert_eq!(p.embedding_dim, 384);
    // The SNB graph axis is untouched by the vector override.
    assert_eq!(p.persons, Scale::Sf01.params().persons);
}

#[test]
fn absent_overrides_reproduce_preset() {
    let base = Scale::Sf01.params();
    let same = Scale::Sf01
        .params_with_overrides(None, None)
        .expect("no overrides is valid");
    assert_eq!(
        base, same,
        "absent overrides must reproduce the preset exactly"
    );
    // Default preset carries no dedicated extra vector corpus.
    assert_eq!(base.vector_count, 0);
    let g = generate(Scale::Sf01, 1);
    assert!(
        g.vectors.is_empty(),
        "no override => no extra vector corpus"
    );
}

#[test]
fn vector_dim_zero_is_rejected() {
    assert_eq!(
        Scale::Smoke.params_with_overrides(Some(10), Some(0)),
        Err(GenConfigError::ZeroVectorDim),
    );
    assert!(
        generate_with_overrides(Scale::Smoke, 1, Some(10), Some(0)).is_err(),
        "dim 0 must be rejected, not silently coerced"
    );
}

#[test]
fn corpus_vectors_are_deterministic_and_unit_normalized() {
    let a = generate_with_overrides(Scale::Smoke, 7, Some(12), Some(32)).expect("ok");
    let b = generate_with_overrides(Scale::Smoke, 7, Some(12), Some(32)).expect("ok");
    assert_eq!(a.vectors, b.vectors, "same seed => identical corpus");
    for v in &a.vectors {
        let norm: f32 = v.embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-4,
            "corpus vector should be unit length"
        );
    }
}
