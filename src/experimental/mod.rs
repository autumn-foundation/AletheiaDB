//! Experimental features ("Nova" playground).
//!
//! This module contains experimental features that are not yet part of the core API.
//! They are gated behind the `nova` feature flag.
//!
//! # The "Nova" Philosophy 🌟
//!
//! "Nova" is AletheiaDB's R&D playground. It's where we test radical new ideas like
//! semantic physics, narrative generation, and counterfactual graph analysis.
//!
//! **Feature Status:**
//! - 🟢 **Core Candidate**: High utility, stable, aligned with core. Likely to graduate.
//! - 🟡 **Incubating**: Active experiment.
//! - 🔴 **Deprecated**: Candidate for removal.
//!
//! # Enabling Nova
//!
//! Add the `nova` feature to your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! aletheiadb = { version = "0.1", features = ["nova"] }
//! ```
//!
//! # Module Inventory
//!
//! | Module | Status | Description |
//! |--------|--------|-------------|
//! | [`sherlock`] | 🟢 | Temporal Pattern Matching. |
//! | [`chronos`] | 🟢 | Temporal Pathfinding. |
//! | [`fishing`] | 🟢 | Associative Retrieval (Hybrid Search). |
//! | [`gestalt`] | 🟢 | Semantic Subgraph Matching. |
//! | [`highlander`] | 🟢 | Entity Resolution. |
//! | [`kairos`] | 🟢 | Semantic Event Detection. |
//! | [`mnemosyne`] | 🟢 | Memory Consolidation. |
//! | [`prophet`] | 🟢 | Link Prediction. |
//! | [`semantic_navigator`] | 🟢 | A* Vector Pathfinding. |
//! | [`sentinel`] | 🟢 | Semantic Validation. |
//! | [`temporal_diff`] | 🟢 | Graph State Diffing. |
//! | [`alchemy`] | 🟡 | Semantic Graph Transformation. |
//! | [`ariadne`] | 🟡 | Narrative Thread Weaving. |
//! | [`cartographer`] | 🟡 | Semantic Clustering. |
//! | [`chameleon`] | 🟡 | Faceted Search. |
//! | [`chimera`] | 🟡 | Entity Synthesis. |
//! | [`dreamer`] | 🟡 | Trajectory Extrapolation. |
//! | [`hindsight`] | 🟡 | Counterfactual Simulation. |
//! | [`janus`] | 🟡 | Bridge Detection. |
//! | [`metaphor`] | 🟡 | Graph Alignment. |
//! | [`muse`] | 🟡 | Semantic Ideation. |
//! | [`oracle`] | 🟡 | Probabilistic Reasoning. |
//! | [`prism`] | 🟡 | Vector Decomposition. |
//! | [`ripple`] | 🟡 | Causal Detection. |
//! | [`synapse`] | 🟡 | Adaptive Pathfinding. |
//! | [`telepathy`] | 🟡 | Spreading Activation. |
//! | [`temporal_narrative`] | 🟡 | Narrative Generation. |
//! | [`thermos`] | 🟡 | Volatility Gauge. |
//! | [`wormhole`] | 🟡 | Shortcut Detection. |
//! | [`echo`] | 🔴 | Temporal Resonance (Deprecated). |
//! | [`gravity`] | 🔴 | Semantic Influence (Deprecated). |
//! | [`kaleidoscope`] | 🔴 | Force-Directed Layout (Deprecated). |
//! | [`sybil`] | 🔴 | Memetic Propagation (Deprecated). |

#[cfg(feature = "nova")]
/// Alchemy: Semantic Graph Transformation Engine.
pub mod alchemy;
#[cfg(feature = "nova")]
/// Ariadne: Semantic Thread Weaver.
pub mod ariadne;
#[cfg(feature = "nova")]
/// Semantic graph clustering ("Cartographer").
pub mod cartographer;
#[cfg(feature = "nova")]
/// Chameleon: Context-Aware Faceted Search.
pub mod chameleon;
#[cfg(feature = "nova")]
/// Chimera: Hybrid Entity Synthesis Engine.
pub mod chimera;
#[cfg(feature = "nova")]
/// Chronos: Temporal Graph Analysis & Pathfinding.
pub mod chronos;
#[cfg(feature = "nova")]
/// Concept Algebra for semantic vector arithmetic.
pub mod concept_algebra;
#[cfg(feature = "nova")]
/// Dissonance: Semantic Stress Detector.
pub mod dissonance;
#[cfg(feature = "nova")]
/// Semantic Trajectory Extrapolation ("Dreamer").
pub mod dreamer;
#[cfg(feature = "nova")]
#[deprecated(note = "Echo is deprecated and will be removed in a future version.")]
/// Temporal Resonance Engine ("Echo") for finding nodes with similar activity patterns.
pub mod echo;
#[cfg(feature = "nova")]
/// Associative retrieval ("Fishing") module.
pub mod fishing;
#[cfg(feature = "nova")]
/// Graph context exporter for LLM integration.
pub mod graph_context;
#[cfg(feature = "nova")]
/// Highlander: Semantic Entity Resolution.
pub mod highlander;
#[cfg(feature = "nova")]
/// Hindsight: Counterfactual Graph Analysis Engine.
pub mod hindsight;
#[cfg(feature = "nova")]
/// Janus: Semantic Bridge Detector.
pub mod janus;
#[cfg(feature = "nova")]
/// Kairos: Semantic Event Detection & History Summarization.
pub mod kairos;
#[cfg(feature = "nova")]
#[deprecated(note = "Kaleidoscope is deprecated. Use client-side visualization tools instead.")]
/// Kaleidoscope: Semantic Force-Directed Layout Engine.
pub mod kaleidoscope;
#[cfg(feature = "nova")]
/// Prism: Semantic Spectroscopy for Vectors.
pub mod prism;
#[cfg(feature = "nova")]
/// Prophet Link Prediction Engine.
pub mod prophet;
#[cfg(feature = "nova")]
/// Ripple: Semantic Causality Detector.
pub mod ripple;
#[cfg(feature = "nova")]
/// Semantic Navigator for vector-guided pathfinding.
pub mod semantic_navigator;
#[cfg(feature = "nova")]
/// Sherlock: Temporal Pattern Matching Engine.
pub mod sherlock;
#[cfg(feature = "nova")]
#[deprecated(note = "Sybil is deprecated and will be removed in a future version.")]
/// Sybil: Memetic Propagation Engine.
pub mod sybil;
#[cfg(feature = "nova")]
/// Synapse: Adaptive Graph Hebbian Learning.
pub mod synapse;
#[cfg(feature = "nova")]
/// Telepathy: Semantic Spreading Activation Engine.
pub mod telepathy;
#[cfg(feature = "nova")]
/// Temporal Diff Engine for computing snapshot differences.
pub mod temporal_diff;
/// Temporal narrative generator for natural language history logs.
pub mod temporal_narrative;

#[cfg(feature = "nova")]
/// Thermos: Semantic Temperature & Volatility Gauge.
pub mod thermos;

#[cfg(feature = "nova")]
/// Sentinel: Semantic Firewall for validating data insertion.
pub mod sentinel;

#[cfg(feature = "nova")]
/// Wormhole: Detecting Semantic-Structural Gaps.
pub mod wormhole;

#[cfg(feature = "nova")]
/// Gestalt: Semantic Subgraph Matching Engine.
pub mod gestalt;

#[cfg(feature = "nova")]
#[deprecated(note = "Gravity is deprecated and will be removed in a future version.")]
/// Gravity: Semantic Mass and Orbit Analysis.
pub mod gravity;

#[cfg(feature = "nova")]
/// Metaphor: Semantic Graph Alignment Engine.
pub mod metaphor;

#[cfg(feature = "nova")]
/// Mnemosyne: Semantic Memory Consolidation.
pub mod mnemosyne;

#[cfg(feature = "nova")]
/// Muse: The Semantic Ideator.
pub mod muse;

#[cfg(feature = "nova")]
/// Oracle: Probabilistic Graph Reasoning.
pub mod oracle;
