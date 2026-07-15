//! Generator determinism tests: same seed => same graph.

use aletheia_bench_ldbc::generator::{Scale, generate};

#[test]
fn same_seed_same_graph() {
    let a = generate(Scale::Smoke, 42);
    let b = generate(Scale::Smoke, 42);
    assert_eq!(a, b, "same seed must produce an identical graph");
}

#[test]
fn different_seed_different_graph() {
    let a = generate(Scale::Smoke, 1);
    let b = generate(Scale::Smoke, 2);
    // Structure counts are fixed by scale, but the sampled content (knows
    // edges, embeddings, revisions) must differ for a different seed.
    assert_ne!(
        a.knows, b.knows,
        "different seeds should yield different KNOWS edges"
    );
}

#[test]
fn deterministic_embeddings_are_unit_normalized() {
    let g = generate(Scale::Smoke, 7);
    assert!(!g.posts.is_empty());
    for post in &g.posts {
        assert_eq!(post.embedding.len(), g.params.embedding_dim);
        let norm: f32 = post.embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "embedding should be unit length");
    }
    // Determinism at the embedding level too.
    let g2 = generate(Scale::Smoke, 7);
    assert_eq!(g.posts[0].embedding, g2.posts[0].embedding);
}

#[test]
fn scale_shapes_match_params() {
    let g = generate(Scale::Smoke, 3);
    let p = Scale::Smoke.params();
    assert_eq!(g.persons.len(), p.persons);
    assert_eq!(g.forums.len(), p.forums);
    assert_eq!(g.posts.len(), p.forums * p.posts_per_forum);
    assert_eq!(g.tags.len(), p.tags);
    // History exists for the temporal extension.
    assert!(
        !g.person_revisions.is_empty(),
        "update stream must produce revisions"
    );
}

#[test]
fn scale_parse_roundtrip() {
    assert_eq!(Scale::parse("smoke"), Some(Scale::Smoke));
    assert_eq!(Scale::parse("sf0.1"), Some(Scale::Sf01));
    assert_eq!(Scale::parse("SF01"), Some(Scale::Sf01));
    assert_eq!(Scale::parse("bogus"), None);
}
