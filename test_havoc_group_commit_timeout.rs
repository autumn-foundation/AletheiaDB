use std::time::Duration;
use std::sync::{Arc, Mutex, Condvar};

fn wait_for_flush(max_delay_ms: u64, timeout_multiplier: u32, timeout_base_ms: u64, timeout_min_ms: u64, timeout_max_ms: u64) {
    let base_timeout =
        Duration::from_millis(max_delay_ms.saturating_mul(timeout_multiplier as u64))
            .saturating_add(Duration::from_millis(timeout_base_ms));

    let timeout = base_timeout
        .max(Duration::from_millis(timeout_min_ms))
        .min(Duration::from_millis(timeout_max_ms));

    let deadline = std::time::Instant::now().checked_add(timeout).unwrap_or(std::time::Instant::now() + Duration::from_secs(60 * 60 * 24 * 365));

    println!("Timeout: {:?} Deadline: {:?}", timeout, deadline);
}

fn main() {
    wait_for_flush(10, 1, u64::MAX, 0, u64::MAX);
}
