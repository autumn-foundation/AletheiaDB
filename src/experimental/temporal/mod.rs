//! Temporal: bi-temporal + semantic analysis.
//!
//! Modules that lean on AletheiaDB's bi-temporal storage to detect patterns,
//! reconstruct narratives, and reason over how the graph (and its semantics)
//! evolves through time.
//!
//! Experimental — gated by `features = ["semantic-temporal"]` (or the `nova` umbrella).

#[cfg(feature = "semantic-temporal")]
pub mod ariadne;
#[cfg(feature = "semantic-temporal")]
pub mod aura;
#[cfg(feature = "semantic-temporal")]
pub mod chronos;
#[cfg(feature = "semantic-temporal")]
pub mod echo;
#[cfg(feature = "semantic-temporal")]
pub mod kairos;
#[cfg(feature = "semantic-temporal")]
pub mod mnemosyne;
#[cfg(feature = "semantic-temporal")]
pub mod sherlock;
#[cfg(feature = "semantic-temporal")]
pub mod temporal_diff;
/// Temporal narrative generator: natural-language history logs from version diffs.
pub mod temporal_narrative;
