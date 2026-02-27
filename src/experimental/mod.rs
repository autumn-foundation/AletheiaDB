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
//! **These features are:**
//! - 🧪 **Experimental**: APIs may change or break without warning.
//! - 🚀 **Innovative**: Cutting-edge features for AI/LLM integration.
//! - 🚩 **Opt-in**: You must explicitly enable them.
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
//! | Module | Code Name | Description |
//! |--------|-----------|-------------|
//! | [`sherlock`] | **Sherlock** | Temporal Pattern Matching. "Did X happen before Y within 5 mins?" |
//! | [`dreamer`] | **Dreamer** | Semantic Trajectory Extrapolation. "Where is this vector going?" |
//! | [`thermos`] | **Thermos** | Semantic Volatility Gauge. "Is this data heating up?" |
//! | [`hindsight`] | **Hindsight** | Counterfactual Analysis. "What if this edge didn't exist?" |
//! | [`janus`] | **Janus** | Semantic Bridge Detection. "Who connects these two worlds?" |
//! | [`prism`] | **Prism** | Semantic Spectroscopy. Decompose vectors into conceptual components. |
//! | [`chronos`] | **Chronos** | Temporal Pathfinding. "Find a path that respects time travel." |
//! | [`ariadne`] | **Ariadne** | Semantic Thread Weaver. Connect disparate concepts via narrative threads. |
//! | [`echo`] | **Echo** | Temporal Resonance. Find nodes with similar activity patterns. |
//! | [`kaleidoscope`] | **Kaleidoscope** | Semantic Force-Directed Layout. Visualize vector spaces. |
//! | [`sentinel`] | **Sentinel** | Semantic Firewall. Validate data insertion against rules. |
//! | [`sybil`] | **Sybil** | Memetic Propagation. "How far does this idea spread?" |
//! | [`synapse`] | **Synapse** | Adaptive Graph Hebbian Learning. "Cells that fire together, wire together." |
//! | [`temporal_narrative`] | **Bard** | Generate natural language histories of graph entities. |
//! | [`gravity`] | **Gravity** | Semantic Mass and Orbit Analysis. "Who are the real influencers?" |
//! | [`gestalt`] | **Gestalt** | Semantic Subgraph Matching. "Find this pattern, but fuzzier." |
//! | [`mnemosyne`] | **Mnemosyne** | Semantic Memory Consolidation. "What matters is what changed." |
//! | [`chimera`] | **Chimera** | Hybrid Entity Synthesis. "What if we merged these two concepts?" |
//! | [`oracle`] | **Oracle** | Probabilistic Graph Reasoning. "Who is the most relevant node to X?" |
//! | [`rosetta`] | **Rosetta** | Semantic Relationship Profiling. "What does 'Parent Of' look like in vector space?" |
//!
//! # Example: Detecting Suspicious Patterns with Sherlock
//!
//! > ⚠️ **REQUIRES FEATURE 'NOVA'**
//! >
//! > This feature is experimental and requires the `nova` feature flag.
//! > Add `features = ["nova"]` to your `Cargo.toml`.
//!
//! ```rust,ignore
//! // [dependencies]
//! // aletheiadb = { version = "0.1", features = ["nova"] }
//!
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::sherlock::{Sherlock, Mystery, Clue};
//! use aletheiadb::core::property::PropertyValue;
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("User", Default::default())?;
//!
//! // Define a mystery: User logs in, then deletes file within 1 second
//! let mystery = Mystery::new(Duration::from_secs(1))
//!     .add_clue(Clue::PropertyState {
//!         key: "status".to_string(),
//!         value: Some(PropertyValue::from("LoggedIn")),
//!     })
//!     .add_clue(Clue::PropertyState {
//!         key: "action".to_string(),
//!         value: Some(PropertyValue::from("DeleteFile")),
//!     });
//!
//! let sherlock = Sherlock::new(&db);
//! let detections = sherlock.investigate(node_id, &mystery)?;
//!
//! if !detections.is_empty() {
//!     println!("🕵️ Sherlock found {} suspicious sequences!", detections.len());
//! }
//! # Ok(())
//! # }
//! ```

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
/// Rosetta: Semantic Relationship Profiling.
pub mod rosetta;
#[cfg(feature = "nova")]
/// Semantic Navigator for vector-guided pathfinding.
pub mod semantic_navigator;
#[cfg(feature = "nova")]
/// Sherlock: Temporal Pattern Matching Engine.
pub mod sherlock;
#[cfg(feature = "nova")]
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
