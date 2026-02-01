//! Macros for observability instrumentation.
//!
//! This module defines macros that conditionally expand to `metrics` crate macros
//! when the `observability-prometheus` feature is enabled. This allows zero-cost
//! instrumentation when the feature is disabled, as the macros expand to nothing.

/// Record a value to a histogram.
///
/// Usage:
/// - `observe_histogram!("name", value)`
/// - `observe_histogram!("name", value, "label" => "value", ...)`
#[macro_export]
macro_rules! observe_histogram {
    ($name:expr, $val:expr) => {
        #[cfg(feature = "observability-prometheus")]
        {
            metrics::histogram!($name).record($val);
        }
    };
    ($name:expr, $val:expr, $($labels:tt)*) => {
        #[cfg(feature = "observability-prometheus")]
        {
            metrics::histogram!($name, $($labels)*).record($val);
        }
    };
}

/// Increment a counter by 1.
///
/// Usage:
/// - `increment_counter!("name")`
/// - `increment_counter!("name", "label" => "value", ...)`
#[macro_export]
macro_rules! increment_counter {
    ($name:expr) => {
        #[cfg(feature = "observability-prometheus")]
        {
            metrics::counter!($name).increment(1);
        }
    };
    ($name:expr, $($labels:tt)*) => {
        #[cfg(feature = "observability-prometheus")]
        {
            metrics::counter!($name, $($labels)*).increment(1);
        }
    };
}

/// Measure duration of a block and record to histogram.
///
/// Usage: `timed_block!("metric_name", ["label" => "val"], { code })`
#[macro_export]
macro_rules! timed_block {
    ($name:expr, [ $($labels:tt)* ], $block:expr) => {{
        #[cfg(feature = "observability-prometheus")]
        let _start = std::time::Instant::now();

        let result = $block;

        #[cfg(feature = "observability-prometheus")]
        {
            let duration = _start.elapsed().as_secs_f64();
            metrics::histogram!($name, $($labels)*).record(duration);
        }

        result
    }};
}
