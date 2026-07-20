//! Temporal: bi-temporal + semantic analysis.
//!
//! Modules that lean on AletheiaDB's bi-temporal storage to detect patterns,
//! reconstruct narratives, and reason over how the graph (and its semantics)
//! evolves through time.
//!
//! Experimental — gated by `features = ["semantic-temporal"]` (or the `nova` umbrella).

pub mod ariadne;
pub mod aura;
/// Belief-revision audit: classify *when and why* an entity's stored facts
/// changed (correction / world-change / retraction / reaffirmation) — Issue #3362.
pub mod belief_revision;
pub mod chronos;
/// Counterfactual exclusion replay: materialize a read-only shadow view of the
/// database as it would exist had a named source's writes never been recorded,
/// with a divergence (blast-radius) report — Issue #3357.
pub mod counterfactual;
/// Temporal semantic drift alarms: watch embedding evolution against declared
/// thresholds and surface "this concept changed meaning" as durable,
/// bi-temporal, changefeed-delivered alarm events — Issue #3367.
pub mod drift_alarm;
pub mod echo;
/// Knowledge half-life analytics: survival analysis over fact volatility —
/// per-cohort half-lives, per-fact freshness, staleness inventories (Issue #3377).
pub mod half_life;
pub mod kairos;
pub mod mnemosyne;
pub mod sherlock;
pub mod temporal_diff;
/// Temporal narrative generator: natural-language history logs from version diffs.
pub mod temporal_narrative;
