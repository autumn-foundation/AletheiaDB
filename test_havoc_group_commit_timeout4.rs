use std::time::Duration;

fn main() {
    let _ = Duration::from_millis(u64::MAX) + Duration::from_millis(u64::MAX);
}
