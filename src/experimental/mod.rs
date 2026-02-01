//! Experimental features ("Nova" playground).
//!
//! This module contains experimental features that are not yet part of the core API.
//! They are gated behind the `nova` feature flag.

#[cfg(feature = "nova")]
pub mod semantic_pathfinding;
#[cfg(feature = "nova")]
/// Temporal narrative generator for natural language history logs.
pub mod temporal_narrative;
#[cfg(feature = "nova")]
/// UI components for the Fishing Minigame.
pub mod fishing_ui;
#[cfg(feature = "nova")]
/// Arthropod GUI widgets.
pub mod arthropod;
