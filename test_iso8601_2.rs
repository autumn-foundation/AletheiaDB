use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let secs = 1609459200; // 2021-01-01 00:00:00 UTC
    let nanos = 0;
    let datetime = UNIX_EPOCH + std::time::Duration::new(secs as u64, nanos);
    println!("Output: {:?}", datetime);
}
