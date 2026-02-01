use std::fs::File;
use tempfile::tempdir;

use crate::storage::index_persistence::error::IndexPersistenceError;
use crate::storage::index_persistence::graph::{
    load_graph_index, load_graph_index_mmap, load_graph_index_with_delta,
};
use crate::storage::index_persistence::manifest::load_manifest;
use crate::storage::index_persistence::strings::load_string_interner;
use crate::storage::index_persistence::temporal::load_temporal_index;
use crate::storage::index_persistence::vector::{load_vector_mappings, load_vector_meta};
use crate::storage::index_persistence::{
    MAX_GRAPH_INDEX_FILE_SIZE, MAX_MANIFEST_FILE_SIZE, MAX_MMAP_FILE_SIZE,
    MAX_STRING_INTERNER_FILE_SIZE, MAX_TEMPORAL_INDEX_FILE_SIZE, MAX_VECTOR_INDEX_FILE_SIZE,
};

#[test]
fn test_graph_index_size_limit_exceeded() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("too_large.idx");

    // Create a sparse file larger than the limit
    let file = File::create(&path).unwrap();
    file.set_len(MAX_GRAPH_INDEX_FILE_SIZE + 1).unwrap();

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

    // Create a valid base file so we can test delta size checking
    // (load_graph_index_with_delta validates base first)
    use crate::storage::index_persistence::graph::{new_graph_index_data, save_graph_index};
    let base_data = new_graph_index_data();
    save_graph_index(&base_data, &base_path).unwrap();

    // Create a large delta file
    let file = File::create(&delta_path).unwrap();
    file.set_len(MAX_GRAPH_INDEX_FILE_SIZE + 1).unwrap();

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
fn test_graph_index_mmap_sanity_limit_exceeded() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("mmap_too_large.idx");

    // Create a sparse file larger than the sanity limit
    let file = File::create(&path).unwrap();
    file.set_len(MAX_MMAP_FILE_SIZE + 1).unwrap();

    // Attempt to load via mmap
    let result = load_graph_index_mmap(&path);

    assert!(result.is_err());
    match result {
        Err(IndexPersistenceError::SizeLimitExceeded { message }) => {
            assert!(message.contains("Graph index file size"));
            assert!(message.contains("exceeds sanity limit"));
        }
        _ => panic!("Expected SizeLimitExceeded error, got {:?}", result),
    }
}

#[test]
fn test_vector_meta_size_limit_exceeded() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("meta_too_large.idx");

    let file = File::create(&path).unwrap();
    file.set_len(MAX_VECTOR_INDEX_FILE_SIZE + 1).unwrap();

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
    file.set_len(MAX_VECTOR_INDEX_FILE_SIZE + 1).unwrap();

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

#[test]
fn test_temporal_index_size_limit_exceeded() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("temporal_too_large.idx");

    let file = File::create(&path).unwrap();
    file.set_len(MAX_TEMPORAL_INDEX_FILE_SIZE + 1).unwrap();

    let result = load_temporal_index(&path);

    assert!(result.is_err());
    match result {
        Err(IndexPersistenceError::SizeLimitExceeded { message }) => {
            assert!(message.contains("Temporal index file size"));
            assert!(message.contains("exceeds limit"));
        }
        _ => panic!("Expected SizeLimitExceeded error, got {:?}", result),
    }
}

#[test]
fn test_string_interner_size_limit_exceeded() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("interner_too_large.idx");

    let file = File::create(&path).unwrap();
    file.set_len(MAX_STRING_INTERNER_FILE_SIZE + 1).unwrap();

    let result = load_string_interner(&path);

    assert!(result.is_err());
    match result {
        Err(IndexPersistenceError::SizeLimitExceeded { message }) => {
            assert!(message.contains("String interner file size"));
            assert!(message.contains("exceeds limit"));
        }
        _ => panic!("Expected SizeLimitExceeded error, got {:?}", result),
    }
}

#[test]
fn test_manifest_size_limit_exceeded() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("manifest_too_large.idx");

    let file = File::create(&path).unwrap();
    file.set_len(MAX_MANIFEST_FILE_SIZE + 1).unwrap();

    let result = load_manifest(&path);

    assert!(result.is_err());
    match result {
        Err(IndexPersistenceError::SizeLimitExceeded { message }) => {
            assert!(message.contains("Manifest file size"));
            assert!(message.contains("exceeds limit"));
        }
        _ => panic!("Expected SizeLimitExceeded error, got {:?}", result),
    }
}
