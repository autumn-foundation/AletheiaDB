//! Experimental features ("Nova" playground).
//!
//! This module hosts experimental features whose APIs are not yet stable.
//! Modules are organized into category subfolders, each gated by its own
//! feature flag so users can opt into a slice of experimentation rather than
//! the whole blob.
//!
//! # The "Nova" Philosophy 🌟
//!
//! "Nova" is AletheiaDB's R&D playground — radical ideas like semantic physics,
//! narrative generation, and counterfactual graph analysis live here while we
//! validate them.
//!
//! **These features are:**
//! - 🧪 **Experimental**: APIs may change or break without warning.
//! - 🚀 **Innovative**: Cutting-edge features for AI/LLM integration.
//! - 🚩 **Opt-in**: You must explicitly enable them.
//!
//! Once a category proves itself, it graduates to a top-level stable feature
//! (see `crate::semantic_search` for the first graduation).
//!
//! # Enabling Nova
//!
//! Flip the umbrella flag to enable every category:
//!
//! ```toml
//! [dependencies]
//! aletheiadb = { version = "0.1", features = ["nova"] }
//! ```
//!
//! Or pick a single category:
//!
//! ```toml
//! [dependencies]
//! aletheiadb = { version = "0.1", features = ["semantic-temporal"] }
//! ```
//!
//! > ⚠️ **Breaking change in 0.1:** the `nova` umbrella no longer enables the
//! > graduated semantic-search cohort. Add `"semantic-search"` alongside `"nova"`
//! > if you want every former-nova module compiled.
//!
//! # Categories
//!
//! | Flag | What it gates | Modules |
//! |------|---------------|---------|
//! | `reasoning` (`semantic-reasoning`) | Prediction, synthesis, counterfactuals | prophet, dreamer, omen, oracle, hindsight, muse, luna, metaphor, synergy, chimera, alchemy |
//! | `temporal` (`semantic-temporal`) | Bi-temporal + semantic | sherlock, chronos, echo, kairos, temporal_narrative, temporal_diff, aura, mnemosyne, ariadne |
//! | `diagnostics` (`semantic-diagnostics`) | Anomaly, validation, health | dissonance, sentinel, fossil, tremor, polygraph, wormhole, ripple, entanglement, thermos, paradox |
//! | `characterization` (`semantic-characterization`) | Describe concepts + export | archetype, prism, gravity, sybil, synapse, kaleidoscope, papyrus, graph_context, wildfire |
//!
//! For convenience, every submodule is re-exported at this module's path —
//! existing code using `aletheiadb::experimental::sherlock::Sherlock` keeps
//! working as long as the corresponding category flag is enabled.
//!
//! # Example: Detecting Suspicious Patterns with Sherlock
//!
//! ```rust,no_run
//! # #[cfg(feature = "semantic-temporal")]
//! use aletheiadb::AletheiaDB;
//! # #[cfg(feature = "semantic-temporal")]
//! use aletheiadb::experimental::sherlock::{Sherlock, Mystery, Clue};
//! # #[cfg(feature = "semantic-temporal")]
//! use aletheiadb::core::property::PropertyValue;
//! # #[cfg(feature = "semantic-temporal")]
//! use std::time::Duration;
//!
//! # #[cfg(feature = "semantic-temporal")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("User", Default::default())?;
//!
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
//! let _detections = sherlock.investigate(node_id, &mystery)?;
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "semantic-temporal"))]
//! # fn main() {}
//! ```

#[cfg(feature = "semantic-reasoning")]
pub mod reasoning;
#[cfg(feature = "semantic-reasoning")]
pub use reasoning::*;

#[cfg(feature = "semantic-temporal")]
pub mod temporal;
#[cfg(feature = "semantic-temporal")]
pub use temporal::*;

#[cfg(feature = "semantic-diagnostics")]
pub mod diagnostics;
#[cfg(feature = "semantic-diagnostics")]
pub use diagnostics::*;

#[cfg(feature = "semantic-characterization")]
pub mod characterization;
#[cfg(feature = "semantic-characterization")]
pub use characterization::*;
