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
//! | `sherlock` | **Sherlock** | Temporal Pattern Matching. "Did X happen before Y within 5 mins?" |
//! | `dreamer` | **Dreamer** | Semantic Trajectory Extrapolation. "Where is this vector going?" |
//! | `aura` | **Aura** | Semantic Essence over Time. "What is your long-term identity?" |
//! | `thermos` | **Thermos** | Semantic Volatility Gauge. "Is this data heating up?" |
//! | `hindsight` | **Hindsight** | Counterfactual Analysis. "What if this edge didn't exist?" |
//! | `janus` | **Janus** | Semantic Bridge Detection. "Who connects these two worlds?" |
//! | `prism` | **Prism** | Semantic Spectroscopy. Decompose vectors into conceptual components. |
//! | `chronos` | **Chronos** | Temporal Pathfinding. "Find a path that respects time travel." |
//! | `ariadne` | **Ariadne** | Semantic Thread Weaver. Connect disparate concepts via narrative threads. |
//! | `echo` | **Echo** | Temporal Resonance. Find nodes with similar activity patterns. |
//! | `kaleidoscope` | **Kaleidoscope** | Semantic Force-Directed Layout. Visualize vector spaces. |
//! | `sentinel` | **Sentinel** | Semantic Firewall. Validate data insertion against rules. |
//! | `sybil` | **Sybil** | Memetic Propagation. "How far does this idea spread?" |
//! | `synapse` | **Synapse** | Adaptive Graph Hebbian Learning. "Cells that fire together, wire together." |
//! | `gravity` | **Gravity** | Semantic Mass and Orbit Analysis. "Who are the real influencers?" |
//! | `gestalt` | **Gestalt** | Semantic Subgraph Matching. "Find this pattern, but fuzzier." |
//! | `mnemosyne` | **Mnemosyne** | Semantic Memory Consolidation. "What matters is what changed." |
//! | `chimera` | **Chimera** | Hybrid Entity Synthesis. "What if we merged these two concepts?" |
//! | `oracle` | **Oracle** | Probabilistic Graph Reasoning. "Who is the most relevant node to X?" |
//! | `spectre` | **Spectre** | Semantic Perspective Engine. "Subjective graph traversal." |
//! | `luna` | **Luna** | Semantic Subgraph Synthesis. "Materialize the core concept bridging these ideas." |
//! | `serendipity` | **Serendipity** | Scenic Route Finder. "Find paths that maximize semantic divergence." |
//! | `tremor` | **Tremor** | Semantic Earthquake Detector. "Did the global semantic state suddenly shift?" |
//!
//! # Example: Detecting Suspicious Patterns with Sherlock
//!
//! > ⚠️ **REQUIRES FEATURE 'NOVA'**
//! >
//! > This feature is experimental and requires the `nova` feature flag.
//! > Add `features = ["nova"]` to your `Cargo.toml`.
//!
//! ```rust,no_run
//! // [dependencies]
//! // aletheiadb = { version = "0.1", features = ["nova"] }
//!
//! # #[cfg(feature = "nova")]
//! use aletheiadb::AletheiaDB;
//! # #[cfg(feature = "nova")]
//! use aletheiadb::NodeId;
//! # #[cfg(feature = "nova")]
//! use aletheiadb::Timestamp;
//! # #[cfg(feature = "nova")]
//! use aletheiadb::experimental::sherlock::{Sherlock, Mystery, Clue};
//! # #[cfg(feature = "nova")]
//! use aletheiadb::core::property::PropertyValue;
//! # #[cfg(feature = "nova")]
//! use std::time::Duration;
//!
//! # #[cfg(feature = "nova")]
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
//! # #[cfg(not(feature = "nova"))]
//! # fn main() {}
//! ```

/// Alchemy: Semantic Graph Transformation Engine.
#[cfg(feature = "nova")]
pub mod alchemy;
/// Archetype: Semantic concept extraction and purity scoring.
#[cfg(feature = "nova")]
pub mod archetype;
/// Ariadne: Semantic Thread Weaver.
#[cfg(feature = "nova")]
pub mod ariadne;
/// Aura: Semantic Essence over Time.
#[cfg(feature = "nova")]
pub mod aura;
/// Semantic graph clustering ("Cartographer").
#[cfg(feature = "nova")]
pub mod cartographer;
/// Chameleon: Context-Aware Faceted Search.
#[cfg(feature = "nova")]
pub mod chameleon;
/// Chimera: Hybrid Entity Synthesis Engine.
#[cfg(feature = "nova")]
pub mod chimera;
/// Chronos: Temporal Graph Analysis & Pathfinding.
#[cfg(feature = "nova")]
pub mod chronos;
/// Concept Algebra for semantic vector arithmetic.
#[cfg(feature = "nova")]
pub mod concept_algebra;
/// Dissonance: Semantic Stress Detector.
#[cfg(feature = "nova")]
pub mod dissonance;
/// Semantic Trajectory Extrapolation ("Dreamer").
#[cfg(feature = "nova")]
pub mod dreamer;
/// Temporal Resonance Engine ("Echo") for finding nodes with similar activity patterns.
#[cfg(feature = "nova")]
pub mod echo;
/// Entanglement: Detecting Quantum Entanglement in the Graph.
#[cfg(feature = "nova")]
pub mod entanglement;
/// Associative retrieval ("Fishing") module.
#[cfg(feature = "nova")]
pub mod fishing;
/// Fossil: Semantic Stagnation Detector.
#[cfg(feature = "nova")]
pub mod fossil;
/// Graph context exporter for LLM integration.
#[cfg(feature = "nova")]
pub mod graph_context;
/// Highlander: Semantic Entity Resolution.
#[cfg(feature = "nova")]
pub mod highlander;
/// Hindsight: Counterfactual Graph Analysis Engine.
#[cfg(feature = "nova")]
pub mod hindsight;
/// Janus: Semantic Bridge Detector.
#[cfg(feature = "nova")]
pub mod janus;
/// Kairos: Semantic Event Detection & History Summarization.
#[cfg(feature = "nova")]
pub mod kairos;
/// Kaleidoscope: Semantic Force-Directed Layout Engine.
#[cfg(feature = "nova")]
pub mod kaleidoscope;
/// Prism: Semantic Spectroscopy for Vectors.
#[cfg(feature = "nova")]
pub mod prism;
#[cfg(feature = "nova")]
pub mod prophet;
/// Ripple: Semantic Causality Detector.
#[cfg(feature = "nova")]
pub mod ripple;
/// Semantic Navigator for vector-guided pathfinding.
#[cfg(feature = "nova")]
pub mod semantic_navigator;
/// Sherlock: Temporal Pattern Matching Engine.
#[cfg(feature = "nova")]
pub mod sherlock;
/// Sybil: Memetic Propagation Engine.
#[cfg(feature = "nova")]
pub mod sybil;
/// Synapse: Adaptive Graph Hebbian Learning.
#[cfg(feature = "nova")]
pub mod synapse;
#[cfg(feature = "nova")]
pub mod synergy;
/// Telepathy: Semantic Spreading Activation Engine.
#[cfg(feature = "nova")]
pub mod telepathy;
/// Temporal Diff Engine for computing snapshot differences.
#[cfg(feature = "nova")]
pub mod temporal_diff;
/// Temporal narrative generator for natural language history logs.
#[cfg(feature = "nova")]
pub mod temporal_narrative;

/// Thermos: Semantic Temperature & Volatility Gauge.
#[cfg(feature = "nova")]
pub mod thermos;

/// Sentinel: Semantic Firewall for validating data insertion.
#[cfg(feature = "nova")]
pub mod sentinel;

/// Wormhole: Detecting Semantic-Structural Gaps.
#[cfg(feature = "nova")]
pub mod wormhole;

/// Gestalt: Semantic Subgraph Matching Engine.
#[cfg(feature = "nova")]
pub mod gestalt;

/// Gravity: Semantic Mass and Orbit Analysis.
#[cfg(feature = "nova")]
pub mod gravity;

/// Metaphor: Semantic Graph Alignment Engine.
#[cfg(feature = "nova")]
pub mod metaphor;

/// Mnemosyne: Semantic Memory Consolidation.
#[cfg(feature = "nova")]
pub mod mnemosyne;

/// Muse: The Semantic Ideator.
#[cfg(feature = "nova")]
pub mod muse;

#[cfg(feature = "nova")]
pub mod omen;
/// Oracle: Probabilistic Graph Reasoning.
#[cfg(feature = "nova")]
pub mod oracle;

/// Luna: Semantic Subgraph Synthesis.
#[cfg(feature = "nova")]
pub mod luna;

/// Spectre: Semantic Perspective Engine.
#[cfg(feature = "nova")]
pub mod spectre;

/// Voyager: Maximal Novelty Traversal Engine.
#[cfg(feature = "nova")]
pub mod voyager;

/// Serendipity: The Scenic Route Finder.
#[cfg(feature = "nova")]
pub mod serendipity;

/// Tremor: Semantic Earthquake Detector.
#[cfg(feature = "nova")]
pub mod tremor;
