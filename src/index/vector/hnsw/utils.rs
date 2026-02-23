#[cfg(test)]
use crate::core::id::NodeId;
use crate::index::vector::{DistanceMetric, Quantization};
use std::sync::Arc;
use usearch::{MetricKind, ScalarKind};

// Thread-local flag to detect re-entrant modification attempts during filtered search.
// This prevents deadlocks when user filter callbacks try to modify the index.
std::thread_local! {
    pub(crate) static IN_FILTER_CALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) type TestRaceHook = fn(&super::HnswIndex, NodeId);

#[cfg(test)]
std::thread_local! {
    // Hook to simulate race conditions in add() Occupied path.
    // Takes the HnswIndex instance and the NodeId being added.
    pub(crate) static TEST_RACE_HOOK: std::cell::Cell<Option<TestRaceHook>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) static TEST_SKIP_CAPACITY_CHECK: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// RAII guard that sets IN_FILTER_CALLBACK to true on creation and restores previous value on drop.
/// This ensures the flag is always reset, even if the callback panics.
pub(crate) struct FilterCallbackGuard {
    prev: bool,
}

impl FilterCallbackGuard {
    pub(crate) fn new() -> Self {
        let prev = IN_FILTER_CALLBACK.with(|flag| flag.replace(true));
        FilterCallbackGuard { prev }
    }
}

impl Drop for FilterCallbackGuard {
    fn drop(&mut self) {
        IN_FILTER_CALLBACK.with(|flag| flag.set(self.prev));
    }
}

/// Convert our DistanceMetric to usearch's MetricKind
pub(crate) fn to_usearch_metric(metric: DistanceMetric) -> MetricKind {
    match metric {
        DistanceMetric::Cosine => MetricKind::Cos,
        DistanceMetric::Euclidean => MetricKind::L2sq,
        DistanceMetric::DotProduct => MetricKind::IP,
        DistanceMetric::Haversine => MetricKind::Haversine,
        DistanceMetric::Hamming => MetricKind::Hamming,
        DistanceMetric::Tanimoto => MetricKind::Tanimoto,
    }
}

/// Convert our Quantization to usearch's ScalarKind
pub(crate) fn to_usearch_scalar(quantization: Quantization) -> ScalarKind {
    match quantization {
        Quantization::F32 => ScalarKind::F32,
        Quantization::F16 => ScalarKind::F16,
        Quantization::I8 => ScalarKind::I8,
    }
}

/// Check if a usearch error is transient and should be retried.
///
/// # Warning: Fragile Implementation
///
/// This function relies on string matching against usearch error messages. If usearch changes
/// its error messages in future versions, this detection may break silently. Callers should
/// monitor retry metrics to detect if retries stop working.
///
/// # Known Retryable Errors
///
/// - "No available threads to lock": Thread pool exhaustion under high concurrency
///
/// # Arguments
///
/// * `error_msg` - The error message string from usearch
///
/// # Returns
///
/// `true` if the error is transient and safe to retry, `false` otherwise
pub(crate) fn is_retryable_usearch_error(error_msg: &str) -> bool {
    // Thread pool exhaustion is a transient error that resolves when threads become available
    error_msg.contains("No available threads to lock")
}

// Helper to create the metric wrapper - extracted for testing
pub(crate) fn create_metric_wrapper<F>(
    dims: usize,
    distance_fn: Arc<F>,
) -> Box<dyn Fn(*const f32, *const f32) -> f32 + Send + Sync>
where
    F: Fn(&[f32], &[f32]) -> f32 + Send + Sync + 'static + ?Sized,
{
    Box::new(move |a: *const f32, b: *const f32| {
        // Check for null pointers to prevent UB
        if a.is_null() || b.is_null() {
            // This should never happen with a correct usearch implementation.
            // If it does, we return f32::MAX to avoid crashing/UB.
            // We cannot return an error here because the signature is fixed by usearch trait.
            eprintln!("usearch passed null pointer to metric function - returning max distance");
            return f32::MAX;
        }

        // Check for alignment to prevent UB
        // Use bitwise check for power-of-2 alignment (f32 align is 4)
        let align_mask = std::mem::align_of::<f32>() - 1;
        if (a as usize) & align_mask != 0 || (b as usize) & align_mask != 0 {
            eprintln!(
                "usearch passed unaligned pointer to metric function (expected alignment {}) - returning max distance",
                std::mem::align_of::<f32>()
            );
            return f32::MAX;
        }

        // SAFETY: usearch guarantees pointers are valid for `dims` elements.
        // We verified they are not null above.

        let slice_a = unsafe { std::slice::from_raw_parts(a, dims) };
        let slice_b = unsafe { std::slice::from_raw_parts(b, dims) };

        // SAFETY: We wrap the user-provided closure in catch_unwind to prevent
        // panics from unwinding across the FFI boundary into C++ code, which is UB.
        // If a panic occurs, we return f32::MAX (infinite distance) to effectively
        // ignore this comparison.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            distance_fn(slice_a, slice_b)
        }));

        match result {
            Ok(val) => val,
            Err(_) => {
                // Log error to stderr so operator is aware of the issue
                eprintln!(
                    "Panic in custom metric function - returning max distance to avoid FFI UB"
                );
                f32::MAX
            }
        }
    })
}
