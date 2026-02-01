//! Experimental features ("Nova" playground).
//!
//! This module contains experimental features that are not yet part of the core API.
//! They are gated behind the `nova` feature flag.

#[cfg(feature = "nova")]
pub mod semantic_pathfinding;

#[cfg(feature = "nova")]
pub mod graph_context;
