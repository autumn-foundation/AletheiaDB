//! Query Result Types for Version and History Queries
//!
//! This module provides structured types for returning historical data,
//! version information, and version diffs to users.

// Re-export types from core::history to preserve public API
pub use crate::core::history::{EntityHistory, VersionDiff, VersionInfo, VersionSummary};
