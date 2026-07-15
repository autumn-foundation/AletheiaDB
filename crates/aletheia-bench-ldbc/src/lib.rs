//! LDBC-style benchmark suite for AletheiaDB (Issue #3373).
//!
//! This crate implements a documented subset of the LDBC Social Network
//! Benchmark (SNB) interactive workload — short reads, a representative set of
//! complex reads, and an insert/update stream — mapped onto AletheiaDB's graph
//! model, plus two clearly-labeled **non-standard extension** workloads
//! (temporal reconstruction / AS OF traversal, and vector k-NN / hybrid
//! graph+vector) that exercise AletheiaDB's differentiators on the same
//! dataset.
//!
//! ## Honesty (read this)
//!
//! This is **"LDBC-style, not an audited LDBC result."** In particular:
//!
//! * The built-in generator is **not** the official LDBC Datagen (Spark); it
//!   is a deterministic synthetic generator shaped like the SNB schema at scale
//!   points we label `smoke` and `sf0.1-equivalent`. See [`generator`].
//! * Incumbent engines (Neo4j / KuzuDB / XTDB) are **not executed** by this
//!   crate. It ships runner configs and query mappings (`incumbents/`) and an
//!   honest capability table, but every not-run/not-expressible cell is marked
//!   as such — never a fabricated number.
//! * The 1M-vector scale point for the vector extension is **aspirational**;
//!   the suite runs the largest feasible scale and reports it honestly.
//!
//! See `docs/METHODOLOGY.md` for the full methodology and deviation list.

pub mod gate;
pub mod generator;
pub mod loader;
pub mod prng;
pub mod report;
pub mod runner;
pub mod stats;
