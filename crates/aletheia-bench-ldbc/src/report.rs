//! Machine-readable benchmark report structures and helpers.

use crate::stats::LatencyStats;
use serde::{Deserialize, Serialize};

/// Current schema version of the emitted JSON report. Bump on breaking changes.
pub const SCHEMA_VERSION: u32 = 1;

/// The broad category an operation belongs to. String-serialized so the JSON
/// stays human-legible and stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// LDBC SNB "short read" analogue (IS-series).
    ShortRead,
    /// LDBC SNB "complex read" analogue (IC-series).
    ComplexRead,
    /// LDBC SNB insert/update stream analogue.
    InsertUpdate,
    /// Non-standard AletheiaDB temporal extension workload.
    TemporalExtension,
    /// Non-standard AletheiaDB vector extension workload.
    VectorExtension,
}

/// Result for a single benchmarked operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationResult {
    /// Stable machine key, e.g. `"is1_person_profile"`. Used by the gate.
    pub key: String,
    /// Human description of what the operation does.
    pub description: String,
    /// The SNB standard operation this maps to (e.g. `"IS1"`), or a label such
    /// as `"EXT-TEMPORAL"` / `"EXT-VECTOR"` for the non-standard extensions.
    pub snb_analogue: String,
    /// Broad category.
    pub category: Category,
    /// Number of measured (post-warmup) iterations.
    pub iterations: usize,
    /// Latency statistics for the measured iterations.
    pub stats: LatencyStats,
    /// Optional extra machine-readable notes (e.g. the vector scale actually
    /// run, or the reconstruction target).
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Captured hardware/runtime context for methodology honesty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareInfo {
    /// CPU architecture (`std::env::consts::ARCH`).
    pub arch: String,
    /// Operating system (`std::env::consts::OS`).
    pub os: String,
    /// Number of logical CPUs available to the process.
    pub logical_cpus: usize,
    /// Best-effort CPU model string (from `/proc/cpuinfo` on Linux), if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_model: Option<String>,
    /// Best-effort total system memory in MiB (from `/proc/meminfo` on Linux).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_mem_mib: Option<u64>,
}

impl HardwareInfo {
    /// Capture hardware info from the current environment (best effort).
    #[must_use]
    pub fn capture() -> Self {
        Self {
            arch: std::env::consts::ARCH.to_string(),
            os: std::env::consts::OS.to_string(),
            logical_cpus: std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(1),
            cpu_model: read_cpu_model(),
            total_mem_mib: read_total_mem_mib(),
        }
    }
}

/// Run configuration echoed into the report for reproducibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunConfig {
    /// Where the graph came from: `"synthetic"` (built-in generator) or
    /// `"datagen"` (official LDBC SNB Datagen CSV ingest). Defaults to
    /// `"synthetic"` when absent in an older report.
    #[serde(default = "default_source")]
    pub source: String,
    /// Scale point label (`"smoke"` or `"sf0.1"`; `"datagen"` for an ingested
    /// Datagen directory).
    pub scale: String,
    /// PRNG seed used to generate the fixture.
    pub seed: u64,
    /// Warmup iterations discarded before measurement.
    pub warmup: usize,
    /// Measured iterations per operation.
    pub iterations: usize,
    /// Number of nodes loaded.
    pub node_count: usize,
    /// Number of edges loaded.
    pub edge_count: usize,
    /// Number of embedding vectors actually indexed (the vector-extension
    /// scale actually run — never the aspirational 1M target).
    pub vector_count: usize,
    /// Embedding dimensionality used for the vector extension.
    pub vector_dim: usize,
}

/// Default `RunConfig::source` for reports written before the field existed.
fn default_source() -> String {
    "synthetic".to_string()
}

/// The complete benchmark report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Suite identifier.
    pub suite: String,
    /// Prominent honesty note: this is LDBC-*style*, not an audited result.
    pub disclaimer: String,
    /// RFC3339 UTC timestamp when the run completed.
    pub generated_at: String,
    /// Unix epoch seconds when the run completed.
    pub generated_at_unix: i64,
    /// Captured hardware context.
    pub hardware: HardwareInfo,
    /// Run configuration.
    pub config: RunConfig,
    /// Per-operation results, in suite order.
    pub operations: Vec<OperationResult>,
}

impl BenchmarkReport {
    /// Look up an operation result by its stable key.
    #[must_use]
    pub fn operation(&self, key: &str) -> Option<&OperationResult> {
        self.operations.iter().find(|o| o.key == key)
    }
}

fn read_cpu_model() -> Option<String> {
    let contents = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in contents.lines() {
        if let Some((k, v)) = line.split_once(':')
            && k.trim() == "model name"
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

fn read_total_mem_mib() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kib: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kib / 1024);
        }
    }
    None
}

/// Format Unix epoch seconds as an RFC3339 UTC string (`YYYY-MM-DDTHH:MM:SSZ`).
///
/// Uses Howard Hinnant's civil-from-days algorithm so we need no date crate.
#[must_use]
pub fn format_rfc3339(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = unix_secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    let ss = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Convert a day count since the Unix epoch into a civil (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Current wall-clock as Unix epoch seconds.
#[must_use]
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
