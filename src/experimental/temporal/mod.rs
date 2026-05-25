//! Temporal: bi-temporal + semantic analysis.
//!
//! Modules that lean on AletheiaDB's bi-temporal storage to detect patterns,
//! reconstruct narratives, and reason over how the graph (and its semantics)
//! evolves through time.
//!
//! Experimental — gated by `features = ["semantic-temporal"]` (or the `nova` umbrella).

pub mod ariadne;
pub mod aura;
pub mod chronos;
pub mod deja_vu;
pub mod echo;
pub mod kairos;
pub mod mnemosyne;
pub mod sherlock;
pub mod temporal_diff;
/// Temporal narrative generator: natural-language history logs from version diffs.
pub mod temporal_narrative;
