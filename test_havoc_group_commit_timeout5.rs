use std::time::Duration;

fn main() {
    let _ = Duration::from_millis(u64::MAX).saturating_add(Duration::from_millis(u64::MAX));
}
