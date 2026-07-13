// OWNED BY LANE B (security). Scaffold only — do not implement here.
//
//! Per-IP / per-principal rate limiting.
//!
//! TODO(Lane B): mount a `tower-governor` `GovernorLayer` (confirmed to satisfy
//! autumn 0.5's relaxed `IntoAppLayer`) via `apply_security`. No rate limiting
//! in PR1.
