// OWNED BY LANE B (security). Scaffold only — do not implement here.
//
//! Opaque cursor signing / TTL / per-connection cap (Issue #3360).
//!
//! TODO(Lane B): carry the per-process cursor signing secret + TTL/cap registry
//! in app state and validate resumed cursors here. No cursor support in PR1.
