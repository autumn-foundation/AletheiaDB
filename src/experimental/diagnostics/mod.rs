//! Diagnostics: anomaly detection, validation, health monitoring.
//!
//! Modules that surface "something looks off" — semantic stress, structural
//! gaps, drift, hallucination, contradictions — and guard data quality at the
//! boundary.
//!
//! Experimental — gated by `features = ["semantic-diagnostics"]` (or the `nova` umbrella).

pub mod dissonance;
pub mod doppelganger;
pub mod entanglement;
pub mod fossil;
pub mod polygraph;
pub mod ripple;
pub mod sentinel;
pub mod thermos;
pub mod tremor;
pub use doppelganger::*;
pub mod wormhole;

// `paradox` is a stub awaiting revival — see CHANGELOG / ADR-0050.
// pub mod paradox;
