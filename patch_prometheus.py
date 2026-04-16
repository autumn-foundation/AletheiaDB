with open("src/observability/backends/prometheus.rs", "r") as f:
    content = f.read()

target = """#[cfg(not(feature = "observability-prometheus"))]
pub struct PrometheusBackend;"""
replacement = """/// Prometheus metrics backend (Fallback)
///
/// This is a stub implementation provided when the `observability-prometheus`
/// feature is disabled. It ensures that downstream code doesn't fail to compile.
///
/// # Examples
/// ```rust
/// use aletheiadb::observability::backends::prometheus::{PrometheusBackend, PrometheusConfig};
///
/// let config = PrometheusConfig { bind_addr: "127.0.0.1:9090".to_string() };
/// let backend = PrometheusBackend::new(config);
/// // Calling `backend.start()` will fail because the feature is disabled.
/// ```
#[cfg(not(feature = "observability-prometheus"))]
pub struct PrometheusBackend;"""

content = content.replace(target, replacement)

with open("src/observability/backends/prometheus.rs", "w") as f:
    f.write(content)
