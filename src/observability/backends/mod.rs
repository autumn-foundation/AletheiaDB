//! Observability backend implementations
//!
//! This module contains backend-specific implementations for various observability systems:
//! - Honeycomb: Distributed tracing with tracing-honeycomb
//! - Prometheus: Metrics HTTP endpoint with metrics-exporter-prometheus

#[cfg(feature = "observability-honeycomb")]
pub mod honeycomb;

#[cfg(feature = "observability-prometheus")]
pub mod prometheus;
