//! Tests for the official LDBC SNB Datagen CSV ingest front-end (Issue #3628).
//!
//! These exercise the `datagen` module against the hand-authored tiny fixture
//! under `tests/fixtures/datagen_tiny/` (official `composite-merged-fk`
//! `|`-delimited format, header row per file).

use aletheia_bench_ldbc::datagen::{self, DatagenError};
use std::path::{Path, PathBuf};

/// Absolute path to the checked-in tiny fixture directory.
fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/datagen_tiny")
}

#[test]
fn ingests_expected_entity_counts() {
    let g = datagen::ingest(&fixture_dir(), None, None).expect("ingest should succeed");
    assert_eq!(g.persons.len(), 3, "3 persons");
    assert_eq!(g.knows.len(), 1, "1 knows edge");
    assert_eq!(g.forums.len(), 1, "1 forum");
    assert_eq!(g.posts.len(), 2, "2 posts");
    assert_eq!(g.comments.len(), 1, "1 comment");
    assert_eq!(g.tags.len(), 1, "1 tag");
    assert_eq!(g.scale, "datagen", "scale label reflects the source");
}

#[test]
fn remaps_large_ids_to_dense_indices_by_header_name() {
    // Person.csv puts `id` in a non-obvious column position; the parser must
    // find it by header name, not position. First-seen order defines the idx.
    let g = datagen::ingest(&fixture_dir(), None, None).expect("ingest should succeed");
    assert_eq!(g.persons[0].idx, 0);
    assert_eq!(g.persons[0].first_name, "Alice"); // id 100 -> idx 0
    assert_eq!(g.persons[1].first_name, "Bob"); // id 200 -> idx 1
    assert_eq!(g.persons[2].first_name, "Carol"); // id 300 -> idx 2
}

#[test]
fn knows_edge_connects_the_right_two_persons() {
    // Person 100 (idx 0) KNOWS person 200 (idx 1).
    let g = datagen::ingest(&fixture_dir(), None, None).expect("ingest should succeed");
    assert_eq!(g.knows[0], (0, 1));
}

#[test]
fn relationship_fks_are_remapped() {
    let g = datagen::ingest(&fixture_dir(), None, None).expect("ingest should succeed");
    // Post 5000 -> idx 0 (creator person 100 -> idx 0, forum 10 -> idx 0).
    assert_eq!(g.posts[0].creator, 0);
    assert_eq!(g.posts[0].forum, 0);
    // Post 5001 -> idx 1 (creator person 200 -> idx 1).
    assert_eq!(g.posts[1].creator, 1);
    // Comment 9000: creator person 300 -> idx 2, replies to post 5000 -> idx 0.
    assert_eq!(g.comments[0].creator, 2);
    assert_eq!(g.comments[0].reply_to_post, 0);
}

#[test]
fn synthetic_vector_corpus_from_overrides() {
    // Datagen has no embeddings; a --vector-count/--vector-dim override layers a
    // synthetic standalone corpus and sizes the (synthetic) post embeddings.
    let g = datagen::ingest(&fixture_dir(), Some(5), Some(8)).expect("ingest should succeed");
    assert_eq!(
        g.vectors.len(),
        5,
        "standalone corpus honours --vector-count"
    );
    assert!(g.vectors.iter().all(|v| v.embedding.len() == 8));
    assert!(
        g.posts.iter().all(|p| p.embedding.len() == 8),
        "post embeddings sized to --vector-dim"
    );
}

#[test]
fn no_vector_override_yields_empty_standalone_corpus() {
    let g = datagen::ingest(&fixture_dir(), None, None).expect("ingest should succeed");
    assert!(
        g.vectors.is_empty(),
        "no --vector-count => empty standalone Embedding corpus"
    );
}

#[test]
fn missing_required_file_is_a_descriptive_error_not_a_panic() {
    let tmp = std::env::temp_dir().join(format!("datagen_missing_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("mk tmp");
    // Empty dir: Person.csv is absent.
    let err = datagen::ingest(&tmp, None, None).expect_err("missing Person.csv must Err");
    match &err {
        DatagenError::MissingFile { file } => assert!(file.contains("Person")),
        other => panic!("expected MissingFile, got {other:?}"),
    }
    // Display is descriptive (non-empty, mentions the file).
    assert!(err.to_string().contains("Person"));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn malformed_row_is_a_descriptive_error_not_a_panic() {
    let tmp = std::env::temp_dir().join(format!("datagen_bad_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("mk tmp");
    // A Person row whose id is not an integer.
    std::fs::write(tmp.join("Person.csv"), "firstName|id\nAlice|not_a_number\n")
        .expect("write bad Person.csv");
    let err = datagen::ingest(&tmp, None, None).expect_err("bad id must Err");
    match &err {
        DatagenError::BadRow { file, .. } => assert!(file.contains("Person")),
        other => panic!("expected BadRow, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
