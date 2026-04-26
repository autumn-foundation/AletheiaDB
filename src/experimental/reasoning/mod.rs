//! Reasoning: prediction, synthesis, counterfactual simulation.
//!
//! The "semantic planning" cohort. Generates new knowledge by predicting links,
//! extrapolating trajectories, simulating counterfactuals, and synthesising
//! novel concepts from existing graph state.
//!
//! Experimental — gated by `features = ["semantic-reasoning"]` (or the `nova` umbrella).

pub mod alchemy;
pub mod chimera;
pub mod dreamer;
pub mod hindsight;
pub mod luna;
pub mod metaphor;
pub mod muse;
pub mod odyssey;
pub mod omen;
pub mod oracle;
pub mod prophet;
pub mod synergy;
