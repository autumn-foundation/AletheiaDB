//! Observability backend implementations
//!
//! This module contains backend-specific implementations for various observability systems:
//! - Prometheus: Metrics HTTP endpoint with metrics-exporter-prometheus

#[cfg(feature = "observability-prometheus")]
pub mod prometheus;
