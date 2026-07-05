use std::time::Duration;

fn main() {
    let timeout_max_ms = u64::MAX;
    let base_timeout = Duration::from_millis(u64::MAX) + Duration::from_millis(0);
    let timeout = base_timeout
        .max(Duration::from_millis(0))
        .min(Duration::from_millis(timeout_max_ms));

    let deadline = std::time::Instant::now() + timeout;
    println!("Deadline: {:?}", deadline);
}
