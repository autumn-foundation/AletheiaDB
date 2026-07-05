use std::time::Duration;
use std::sync::{Arc, Mutex, Condvar};

fn wait_for_flush(max_delay_ms: u64, timeout_multiplier: u32, timeout_base_ms: u64, timeout_min_ms: u64, timeout_max_ms: u64) {
    let base_timeout =
        Duration::from_millis(max_delay_ms * timeout_multiplier as u64)
            + Duration::from_millis(timeout_base_ms);

    let timeout = base_timeout
        .max(Duration::from_millis(timeout_min_ms))
        .min(Duration::from_millis(timeout_max_ms));

    let deadline = std::time::Instant::now() + timeout;

    println!("Timeout: {:?} Deadline: {:?}", timeout, deadline);
}

fn main() {
    wait_for_flush(10, 1, 0, 0, u64::MAX);
}
