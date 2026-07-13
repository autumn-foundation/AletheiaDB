// OWNED BY LANE B (security). Scaffold only — do not implement here.
//
//! In-flight-query cap and per-query resource limits.
//!
//! TODO(Lane B): port the in-flight worker cap (`ConcurrencyLimit`) and the
//! per-query timeout/row/byte limits (Issue #3368) into the shared handler
//! path. No resource limiting in PR1.
