//! Experimental features ("Nova" playground).
//!
//! This module contains experimental features that are not yet part of the core API.
//! They are gated behind the `nova` feature flag.

#[cfg(feature = "nova")]
/// Semantic graph clustering ("Cartographer").
pub mod cartographer;
#[cfg(feature = "nova")]
/// Chronos: Temporal Graph Analysis & Pathfinding.
pub mod chronos;
#[cfg(feature = "nova")]
/// Concept Algebra for semantic vector arithmetic.
pub mod concept_algebra;
#[cfg(feature = "nova")]
/// Semantic Trajectory Extrapolation ("Dreamer").
pub mod dreamer;
#[cfg(feature = "nova")]
/// Temporal Resonance Engine ("Echo") for finding nodes with similar activity patterns.
pub mod echo;
#[cfg(feature = "nova")]
/// Associative retrieval ("Fishing") module.
pub mod fishing;
#[cfg(feature = "nova")]
/// Graph context exporter for LLM integration.
pub mod graph_context;
#[cfg(feature = "nova")]
/// Prophet Link Prediction Engine.
pub mod prophet;
#[cfg(feature = "nova")]
/// Semantic Navigator for vector-guided pathfinding.
pub mod semantic_navigator;
#[cfg(feature = "nova")]
/// Temporal narrative generator for natural language history logs.
pub mod temporal_narrative;
