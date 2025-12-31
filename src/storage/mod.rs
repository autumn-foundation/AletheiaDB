//! Storage subsystem for graph data.
//!
//! This module contains storage engines for both current and historical data:
//! - Current storage: Optimized for fast current-state queries (hot path)
//! - Historical storage: Anchor+delta compression for temporal queries
//! - Version management: Version chain structures and compression

pub mod current;
pub mod historical;
pub mod version;

// Re-export commonly used types
pub use current::CurrentStorage;
pub use historical::{HistoricalStats, HistoricalStorage};
pub use version::{AnchorConfig, EdgeVersion, NodeVersion, PropertyDelta, VersionData};
