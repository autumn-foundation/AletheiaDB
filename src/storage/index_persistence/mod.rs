//! Comprehensive index persistence layer for GallifreyDB.
//!
//! This module provides persistence for all index types:
//! - Vector indexes (HNSW via usearch)
//! - Graph indexes (CSR adjacency)
//! - Temporal indexes (version chains)
//! - String interner
//!
//! # Architecture
//!
//! ```text
//! indexes/
//! ├── manifest.idx          # Index registry
//! ├── strings/interner.idx  # String interning table
//! ├── graph/adjacency.idx   # CSR adjacency data
//! ├── temporal/*.idx        # Version chains
//! └── vector/{prop}/        # Per-property vector indexes
//! ```
//!
//! # Load Order
//!
//! 1. String interner (others depend on string indices)
//! 2. Manifest (tells us what indexes exist)
//! 3. Graph, Temporal, Vector (parallel)

mod error;
pub mod formats;

pub use error::{IndexPersistenceError, Result};
pub use formats::*;

/// Current manifest format version.
pub const MANIFEST_VERSION: u16 = 1;

/// Magic bytes for manifest files.
pub const MANIFEST_MAGIC: [u8; 4] = *b"GIDX";

/// Magic bytes for string interner files.
pub const INTERNER_MAGIC: [u8; 4] = *b"GSTR";

/// Magic bytes for graph index files.
pub const GRAPH_MAGIC: [u8; 4] = *b"GGRP";

/// Magic bytes for temporal index files.
pub const TEMPORAL_MAGIC: [u8; 4] = *b"GTMP";

/// Magic bytes for vector metadata files.
pub const VECTOR_META_MAGIC: [u8; 4] = *b"GVEC";
