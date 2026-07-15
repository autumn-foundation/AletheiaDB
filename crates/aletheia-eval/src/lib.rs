//! # aletheia-eval — deterministic retrieval evaluation harness (Issue #3366)
//!
//! A crate that measures the retrieval quality of AletheiaDB's temporal +
//! graph + vector features against committed gold datasets, entirely without
//! an LLM or any external service. It exists to make a feature's retrieval
//! impact a reproducible, gate-able number rather than a claim.
//!
//! ## Pipeline
//!
//! `load a versioned dataset → index it into an in-memory AletheiaDB → run a
//! fixed query set under a declarative retrieval config → score against gold
//! labels → emit a JSON report + human summary`.
//!
//! Every stage is deterministic given `(dataset version, config, seed)`:
//! embeddings are a seeded [feature-hashing vectorizer](crate::embedding),
//! retrieval ties break by node id, and the [metrics](crate::metrics) are pure
//! functions. Two runs with the same inputs produce byte-identical metrics.
//!
//! ## Paired comparison
//!
//! Each report compares a **full** config (hybrid + temporal anchoring +
//! provenance filter) against a **baseline** config (vector-only k-NN — the
//! pgvector-equivalent). The headline this substantiates: temporally-anchored
//! retrieval scores at least 25 percentage points higher temporal accuracy
//! than the vector-only baseline on the temporal-QA dataset.
//!
//! ## LLM-optional
//!
//! The core scores *retrieval* with no LLM. An optional answer-quality mode
//! (grading a generated answer against the gold answer with an LLM judge) is
//! **documented but not implemented** here — it would plug in as an extra
//! per-question scorer behind a feature flag, leaving the deterministic core
//! untouched. See `README.md`.
//!
//! ## CLI
//!
//! The `aletheia-eval` binary runs `aletheia-eval run <manifest.toml>`. This is
//! the shape the future `aletheia eval run <manifest.toml>` subcommand of the
//! root CLI will take; this crate does not modify the root `aletheia` binary.

pub mod config;
pub mod dataset;
pub mod embedding;
pub mod harness;
pub mod metrics;
pub mod report;
pub mod timeutil;
