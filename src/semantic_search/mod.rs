//! Semantic Search: retrieval, matching, clustering, traversal, entity resolution.
//!
//! Given a query (vector, seed node, or pattern), find/rank/navigate to relevant
//! nodes and subgraphs by combining vector similarity with graph topology.
//!
//! # Status
//!
//! Stable in 0.1 — graduated from `experimental::*` ("Nova") after the usearch
//! integration landed. The API may still evolve before 1.0, but breaking changes
//! will follow normal semver.
//!
//! # Enabling
//!
//! ```toml
//! [dependencies]
//! aletheiadb = { version = "0.1", features = ["semantic-search"] }
//! ```
//!
//! # Module Inventory
//!
//! | Module | Description |
//! |--------|-------------|
//! | [`fishing`] | Associative retrieval (vector + graph + freshness) |
//! | [`gestalt`] | Fuzzy semantic subgraph pattern matching |
//! | [`cartographer`] | Semantic clustering / region reification |
//! | [`highlander`] | Entity resolution / semantic dedup |
//! | [`janus`] | Semantic bridge detection between clusters |
//! | [`chameleon`] | Context-aware faceted search |
//! | [`semantic_navigator`] | Vector-guided A* pathfinding |
//! | [`concept_algebra`] | Vector arithmetic primitives (query construction) |
//! | [`serendipity`] | Divergence-maximizing scenic traversal |
//! | [`voyager`] | Novelty-maximizing traversal |
//! | [`spectre`] | Subjective/perspective-based traversal |
//! | [`telepathy`] | Spreading activation engine |
//! | [`tapestry`] | Semantic path weaving |
//! | [`horizon`] | Concept boundary / semantic event horizon |
//! | [`mosaic`] | Compositional search (build target from a set of nodes) |

pub mod cartographer;
pub mod chameleon;
pub mod concept_algebra;
pub mod fishing;
pub mod gestalt;
pub mod highlander;
pub mod horizon;
pub mod janus;
pub mod semantic_navigator;
pub mod serendipity;
pub mod spectre;
pub mod tapestry;
pub mod telepathy;
pub mod voyager;

// `mosaic` is a stub awaiting revival — see CHANGELOG / ADR-0050.
// pub mod mosaic;
