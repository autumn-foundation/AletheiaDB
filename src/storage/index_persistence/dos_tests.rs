use std::fs::File;
use tempfile::tempdir;

use crate::storage::index_persistence::error::IndexPersistenceError;
use crate::storage::index_persistence::graph::{load_graph_index, load_graph_index_with_delta};
use crate::storage::index_persistence::vector::{load_vector_meta, load_vector_mappings};
use crate::storage::index_persistence::{MAX_GRAPH_INDEX_SIZE, MAX_VECTOR_INDEX_SIZE};

#[test]
fn test_graph_index_size_limit_exceeded() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("too_large.idx");

    // Create a sparse file larger than the limit
    let file = File::create(&path).unwrap();
    file.set_len(MAX_GRAPH_INDEX_SIZE + 1).unwrap();

    // Attempt to load
    let result = load_graph_index(&path);

    assert!(result.is_err());
    match result {
        Err(IndexPersistenceError::SizeLimitExceeded { message }) => {
            assert!(message.contains("Graph index file size"));
            assert!(message.contains("exceeds limit"));
        }
        _ => panic!("Expected SizeLimitExceeded error, got {:?}", result),
    }
}

#[test]
fn test_graph_index_delta_size_limit_exceeded() {
    let dir = tempdir().unwrap();
    let base_path = dir.path().join("base.idx");
    let delta_path = dir.path().join("delta_too_large.idx");

    // Create a valid base file (needs magic bytes at least)
    // We can just use a small empty file, load_graph_index checks size first
    // But we need it to pass size check and maybe magic check if we want to reach delta loading
    // However, load_graph_index_with_delta calls load_graph_index for base first.
    // Let's create a valid small base file.
    use crate::storage::index_persistence::graph::{save_graph_index, new_graph_index_data};
    let base_data = new_graph_index_data();
    save_graph_index(&base_data, &base_path).unwrap();

    // Create a large delta file
    let file = File::create(&delta_path).unwrap();
    file.set_len(MAX_GRAPH_INDEX_SIZE + 1).unwrap();

    // Attempt to load
    let result = load_graph_index_with_delta(&base_path, &delta_path);

    assert!(result.is_err());
    match result {
        Err(IndexPersistenceError::SizeLimitExceeded { message }) => {
            assert!(message.contains("Graph index delta file size"));
            assert!(message.contains("exceeds limit"));
        }
        _ => panic!("Expected SizeLimitExceeded error, got {:?}", result),
    }
}

#[test]
fn test_vector_meta_size_limit_exceeded() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("meta_too_large.idx");

    let file = File::create(&path).unwrap();
    file.set_len(MAX_VECTOR_INDEX_SIZE + 1).unwrap();

    let result = load_vector_meta(&path);

    assert!(result.is_err());
    match result {
        Err(IndexPersistenceError::SizeLimitExceeded { message }) => {
            assert!(message.contains("Vector index file size"));
            assert!(message.contains("exceeds limit"));
        }
        _ => panic!("Expected SizeLimitExceeded error, got {:?}", result),
    }
}

#[test]
fn test_vector_mappings_size_limit_exceeded() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("mappings_too_large.idx");

    let file = File::create(&path).unwrap();
    file.set_len(MAX_VECTOR_INDEX_SIZE + 1).unwrap();

    let result = load_vector_mappings(&path);

    assert!(result.is_err());
    match result {
        Err(IndexPersistenceError::SizeLimitExceeded { message }) => {
            assert!(message.contains("Vector index file size"));
            assert!(message.contains("exceeds limit"));
        }
        _ => panic!("Expected SizeLimitExceeded error, got {:?}", result),
    }
}
