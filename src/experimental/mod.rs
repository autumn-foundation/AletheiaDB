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
//! | [`temporal_narrative`] | **Bard** | Generate natural language histories of graph entities. |
//! | [`gravity`] | **Gravity** | Semantic Mass and Orbit Analysis. "Who are the real influencers?" |
//! | [`gestalt`] | **Gestalt** | Semantic Subgraph Matching. "Find this pattern, but fuzzier." |
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
/// Ariadne: Semantic Thread Weaver.
pub mod ariadne;
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
/// Dissonance: Semantic Stress Detector.
pub mod dissonance;
#[cfg(feature = "nova")]
/// Highlander: Semantic Entity Resolution.
pub mod highlander;
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
/// Hindsight: Counterfactual Graph Analysis Engine.
pub mod hindsight;
#[cfg(feature = "nova")]
/// Janus: Semantic Bridge Detector.
pub mod janus;
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
/// Semantic Navigator for vector-guided pathfinding.
pub mod semantic_navigator;
/// Sherlock: Temporal Pattern Matching Engine.
pub mod sherlock;
#[cfg(feature = "nova")]
/// Sybil: Memetic Propagation Engine.
pub mod sybil;
#[cfg(feature = "nova")]
/// Telepathy: Semantic Spreading Activation Engine.
pub mod telepathy;
#[cfg(feature = "nova")]
/// Temporal Diff Engine for computing snapshot differences.
pub mod temporal_diff;
#[cfg(feature = "nova")]
/// Temporal narrative generator for natural language history logs.
pub mod temporal_narrative;

#[cfg(not(feature = "nova"))]
/// Temporal narrative generator for natural language history logs.
pub mod temporal_narrative {
    use crate::AletheiaDB;
    use crate::core::error::Result;
    use crate::core::id::NodeId;

    /// A single event in the narrative history of an entity.
    #[derive(Debug, Clone)]
    pub struct NarrativeEvent {
        /// ISO 8601 timestamp of when the event was recorded (transaction time).
        pub timestamp: String,
        /// Sequential version number.
        pub version_number: u64,
        /// High-level description of what happened.
        pub description: String,
        /// Detailed list of changes (if any).
        pub changes: Vec<String>,
    }

    /// Generator for creating natural language narratives from temporal history.
    ///
    /// # ⚠️ EXPERIMENTAL FEATURE
    ///
    /// This is a **stub implementation** that exists to provide helpful error messages.
    /// You must enable the `nova` feature to use this functionality.
    ///
    /// Add this to your `Cargo.toml`:
    /// ```toml
    /// [dependencies]
    /// aletheiadb = { version = "...", features = ["nova"] }
    /// ```
    pub struct NarrativeGenerator<'a> {
        _marker: std::marker::PhantomData<&'a ()>,
    }

    impl<'a> NarrativeGenerator<'a> {
        /// Create a new narrative generator.
        ///
        /// # Panics
        ///
        /// This function **will always panic** because the `nova` feature is not enabled.
        pub fn new(_db: &'a AletheiaDB) -> Self {
            panic!(
                "Experimental features like NarrativeGenerator require the 'nova' feature. Please enable it in your Cargo.toml:\n\n[dependencies]\naletheiadb = {{ version = \"...\", features = [\"nova\"] }}\n"
            );
        }

        #[cfg(test)]
        fn new_internal() -> Self {
            Self {
                _marker: std::marker::PhantomData,
            }
        }

        /// Generate a narrative for a specific node.
        ///
        /// # Panics
        ///
        /// This function **will always panic** because the `nova` feature is not enabled.
        pub fn generate_node_narrative(&self, _node_id: NodeId) -> Result<Vec<NarrativeEvent>> {
            panic!("Experimental features like NarrativeGenerator require the 'nova' feature.");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::AletheiaDB;
        use crate::core::id::NodeId;

        #[test]
        #[should_panic(
            expected = "Experimental features like NarrativeGenerator require the 'nova' feature. Please enable it in your Cargo.toml"
        )]
        fn test_stub_new_panics() {
            let db = AletheiaDB::new().unwrap();
            let _ = NarrativeGenerator::new(&db);
        }

        #[test]
        #[should_panic(
            expected = "Experimental features like NarrativeGenerator require the 'nova' feature."
        )]
        fn test_stub_generate_panics() {
            let generator = NarrativeGenerator::new_internal();
            // NodeId::from(0) is a valid way to create a NodeId in tests if implemented,
            // or use new_unchecked if available to crate. Since we are in the crate, we might have access?
            // Actually, NodeId::new(0) is public safe constructor.
            let _ = generator.generate_node_narrative(NodeId::new(0).unwrap());
        }
    }
}

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
