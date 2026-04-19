//! Ripple: Semantic Causality Detector.
//!
//! "Does a change in A cause a Ripple in B?"
//!
//! The Ripple Detector identifies causal relationships between nodes by analyzing
//! the time-lagged correlation of their semantic flux (rate of vector change).
//!
//! # Concept
//!
//! If Node A moves in vector space (high flux) and Node B consistently moves
//! in vector space shortly after (lagged flux), there may be a causal link.
//!
//! # Use Cases
//! - **Causal Inference**: "Did the news article (A) cause the stock price (B) to move?"
//! - **Leading Indicators**: Identifying nodes that predict future activity in others.
//! - **Propagation Analysis**: Tracing how information flows through the graph.
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::ripple::{RippleDetector, RippleConfig};
//! use aletheiadb::core::temporal::{TimeRange, time};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let source = db.create_node("Source", Default::default())?;
//! # let target = db.create_node("Target", Default::default())?;
//!
//! let detector = RippleDetector::new(&db);
//!
//! // Look for causal ripples in the last hour, with 1-minute resolution
//! let config = RippleConfig {
//!     window: TimeRange::new(time::now() - 3600 * 1_000_000, time::now())?,
//!     bin_size_us: 60 * 1_000_000, // 1 minute
//!     max_lag_bins: 10,            // Check lag up to 10 minutes
//!     min_correlation: 0.5,        // Minimum correlation to report
//! };
//!
//! if let Some(ripple) = detector.detect_causality(source, target, &config, "embedding")? {
//!     println!("Found Ripple! Lag: {} mins, Correlation: {:.2}",
//!         ripple.lag_bins, ripple.correlation);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result, StorageError};
use crate::core::history::EntityHistory;
use crate::core::id::NodeId;
use crate::core::temporal::TimeRange;
use crate::core::vector::ops::euclidean_distance;

/// Maximum number of bins allowed to prevent OOM / DoS.
const MAX_BINS: usize = 1_000_000;

/// Configuration for Ripple detection.
#[derive(Debug, Clone)]
pub struct RippleConfig {
    /// The time window to analyze.
    pub window: TimeRange,
    /// The resolution of time bins in microseconds.
    pub bin_size_us: i64,
    /// Maximum lag to check (in number of bins).
    pub max_lag_bins: usize,
    /// Minimum correlation coefficient to consider a valid ripple.
    pub min_correlation: f32,
}

impl Default for RippleConfig {
    fn default() -> Self {
        // Safe default: 1 hour window from Epoch 0.
        // Users should override `window` with a meaningful range relative to their data.
        let hour_us = 3600 * 1_000_000;
        Self {
            window: TimeRange::new(0.into(), hour_us.into()).unwrap(),
            bin_size_us: 1_000_000, // 1 second
            max_lag_bins: 10,
            min_correlation: 0.5,
        }
    }
}

/// A detected causal ripple.
#[derive(Debug, Clone, PartialEq)]
pub struct RippleEffect {
    /// The source node.
    pub source: NodeId,
    /// The target node.
    pub target: NodeId,
    /// The lag at which maximum correlation was found (in bins).
    /// A positive lag means Source leads Target.
    pub lag_bins: usize,
    /// The lag in microseconds.
    pub lag_us: i64,
    /// The Pearson correlation coefficient at the optimal lag (-1.0 to 1.0).
    pub correlation: f32,
    /// Confidence score (based on number of data points).
    pub confidence: f32,
}

/// The Ripple Detector engine.
pub struct RippleDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> RippleDetector<'a> {
    /// Create a new RippleDetector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect if there is a causal ripple from source to target.
    pub fn detect_causality(
        &self,
        source: NodeId,
        target: NodeId,
        config: &RippleConfig,
        property: &str,
    ) -> Result<Option<RippleEffect>> {
        // 1. Get histories
        let source_hist = self.db.get_node_history(source)?;
        let target_hist = self.db.get_node_history(target)?;

        // 2. Compute Semantic Flux Series
        let source_flux = self.compute_flux(&source_hist, config, property)?;
        let target_flux = self.compute_flux(&target_hist, config, property)?;

        // 3. Check data sufficiency
        // If either series is all zeros or too short, we can't detect causality.
        if source_flux.iter().all(|&x| x == 0.0) || target_flux.iter().all(|&x| x == 0.0) {
            return Ok(None);
        }

        // 4. Cross-Correlate
        let (best_lag, max_corr) =
            self.cross_correlate(&source_flux, &target_flux, config.max_lag_bins);

        if max_corr >= config.min_correlation {
            Ok(Some(RippleEffect {
                source,
                target,
                lag_bins: best_lag,
                lag_us: best_lag as i64 * config.bin_size_us,
                correlation: max_corr,
                confidence: 1.0, // Placeholder
            }))
        } else {
            Ok(None)
        }
    }

    /// Compute the semantic flux (magnitude of vector change) time series.
    fn compute_flux(
        &self,
        history: &EntityHistory,
        config: &RippleConfig,
        property: &str,
    ) -> Result<Vec<f32>> {
        let start_time = config.window.start().wallclock();
        let end_time = config.window.end().wallclock();
        let duration = end_time - start_time;

        if duration <= 0 {
            return Ok(Vec::new());
        }

        let num_bins = (duration / config.bin_size_us) as usize + 1;

        if num_bins > MAX_BINS {
            return Err(Error::Storage(StorageError::CapacityExceeded {
                resource: "Ripple detection bins".to_string(),
                current: num_bins,
                limit: MAX_BINS,
            }));
        }

        let mut flux = vec![0.0; num_bins];

        // We need pairs of consecutive versions to compute distance.
        // We filter versions within the window first.
        // Actually, we need the version BEFORE the window start to compute the first flux if an update happens right at start?
        // Let's iterate all versions and check timestamps.

        let mut prev_vec: Option<Vec<f32>> = None;

        for version in &history.versions {
            let ts = version.temporal.valid_time().start().wallclock();

            // Extract vector
            let current_vec = version
                .properties
                .get(property)
                .and_then(|v| v.as_vector())
                .map(|v| v.to_vec());

            // If timestamp is within window
            #[allow(clippy::collapsible_if)]
            if ts >= start_time && ts < end_time {
                if let (Some(curr), Some(prev)) = (&current_vec, &prev_vec) {
                    let dist = euclidean_distance(curr, prev)?;
                    let bin_idx = ((ts - start_time) / config.bin_size_us) as usize;
                    if bin_idx < num_bins {
                        flux[bin_idx] += dist;
                    }
                }
            }

            // Update previous vector for next iteration
            // We update prev_vec regardless of window, so that if we enter the window,
            // we have the state just before it.
            if current_vec.is_some() {
                prev_vec = current_vec;
            }
        }

        Ok(flux)
    }

    /// Compute cross-correlation for lags 0..max_lag.
    /// Returns (best_lag, max_correlation).
    fn cross_correlate(&self, source: &[f32], target: &[f32], max_lag: usize) -> (usize, f32) {
        let n = source.len().min(target.len());
        if n == 0 {
            return (0, 0.0);
        }

        let source = &source[0..n];
        let target = &target[0..n];

        // Pre-calculate means and std devs for full series?
        // Technically, correlation should be over the overlapping window.
        // But for simplicity in sliding window, we'll assume stationarity or use full series stats.
        // Let's use the standard Pearson correlation on the shifted arrays.

        let mut best_lag = 0;
        let mut max_corr = -1.0;

        for lag in 0..=max_lag {
            if lag >= n {
                break;
            }

            // Shift source forward by lag (Source leads Target)
            // So we compare Source[0..N-lag] with Target[lag..N]
            let s_slice = &source[0..n - lag];
            let t_slice = &target[lag..n];

            let corr = self.pearson_correlation(s_slice, t_slice);

            if corr > max_corr {
                max_corr = corr;
                best_lag = lag;
            }
        }

        (best_lag, max_corr)
    }

    fn pearson_correlation(&self, x: &[f32], y: &[f32]) -> f32 {
        let n = x.len();
        if n < 2 {
            return 0.0;
        }

        let mean_x = x.iter().sum::<f32>() / n as f32;
        let mean_y = y.iter().sum::<f32>() / n as f32;

        let mut num = 0.0;
        let mut den_x = 0.0;
        let mut den_y = 0.0;

        for i in 0..n {
            let dx = x[i] - mean_x;
            let dy = y[i] - mean_y;
            num += dx * dy;
            den_x += dx * dx;
            den_y += dy * dy;
        }

        if den_x == 0.0 || den_y == 0.0 {
            return 0.0;
        }

        num / (den_x.sqrt() * den_y.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_ripple_exact_match_zero_lag() {
        let db = AletheiaDB::new().unwrap();
        let t0 = time::now();
        let bin_size = 1_000; // 1ms

        // Create Source
        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let a = db.create_node("Source", props_a).unwrap();

        // Create Target
        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let b = db.create_node("Target", props_b).unwrap();

        // Update both at T+10ms
        std::thread::sleep(std::time::Duration::from_millis(10)); // Ensure distinct time from creation
        let _t1 = time::now(); // Approx creation time? No, now is T1.

        // Actually, creating history with precise timestamps is hard with `create_node`.
        // We'll use `update_node_with_valid_time` if available, or just sleep.
        // Sleeping is flaky.
        // Let's use `update_node` and rely on system clock, but with small sleeps.

        // Update A and B in the same transaction to ensure identical timestamps
        db.write(|tx| {
            tx.update_node(
                a,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )?;
            tx.update_node(
                b,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        let t_end = time::now();
        let detector = RippleDetector::new(&db);
        let config = RippleConfig {
            window: TimeRange::new(t0, t_end).unwrap(),
            bin_size_us: bin_size, // 1s bins might be too large if updates are instant
            // Let's make bins small
            max_lag_bins: 5,
            min_correlation: 0.9,
        };

        // If updates happened in same bin, lag is 0.
        // Correlation should be 1.0 (both spiked).
        // Wait, if flux is [0, 0, 1, 0...] for both.
        // Correlation is 1.0.

        let ripple = detector
            .detect_causality(a, b, &config, "vec")
            .unwrap()
            .unwrap();

        assert_eq!(ripple.lag_bins, 0);
        assert!(ripple.correlation > 0.9);
    }

    #[test]
    fn test_ripple_lagged() {
        // We need to simulate lag.
        // Since we can't easily force timestamps in the public API without `update_node_with_valid_time`
        // (which is available on `WriteTransaction` but maybe not exposed conveniently in `AletheiaDB` wrapper?
        // Let's check `AletheiaDB::write_transaction`.

        let db = AletheiaDB::new().unwrap();
        let t_base = time::now().wallclock();
        let bin = 1_000_000; // 1s

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0])
            .build();

        // Helper to update at specific offset
        let update_at = |id: NodeId, offset_us: i64, val: f32| {
            let ts = crate::core::temporal::Timestamp::new(t_base + offset_us, 0).unwrap();
            let mut tx = db.write_transaction().unwrap();
            tx.update_node_with_valid_time(
                id,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[val])
                    .build(),
                Some(ts),
            )
            .unwrap();
            tx.commit().unwrap();
        };

        // Create nodes
        let a = db.create_node("S", props.clone()).unwrap();
        let b = db.create_node("T", props.clone()).unwrap();

        // Pattern: Spike at T=2s for A, T=5s for B (Lag 3s)
        update_at(a, 2 * bin, 1.0); // Flux 1.0 at bin 2
        update_at(a, 3 * bin, 1.0); // Flux 0.0 at bin 3

        update_at(b, 5 * bin, 1.0); // Flux 1.0 at bin 5 (Lag 3)
        update_at(b, 6 * bin, 1.0); // Flux 0.0

        let detector = RippleDetector::new(&db);
        let config = RippleConfig {
            window: TimeRange::new((t_base).into(), (t_base + 10 * bin).into()).unwrap(),
            bin_size_us: bin,
            max_lag_bins: 5,
            min_correlation: 0.8,
        };

        // Flux A: [0, 0, 1, 0, 0, 0, 0...]
        // Flux B: [0, 0, 0, 0, 0, 1, 0...]
        // Lag should be 5 - 2 = 3.

        let ripple = detector.detect_causality(a, b, &config, "vec").unwrap();

        assert!(ripple.is_some(), "Should detect ripple");
        let r = ripple.unwrap();
        assert_eq!(r.lag_bins, 3);
        assert!(r.correlation > 0.9);
    }
}
