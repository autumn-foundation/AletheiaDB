use std::time::Duration;

fn main() {
    let timeout = Duration::from_millis(u64::MAX);
    let deadline = std::time::Instant::now() + timeout;
    println!("Deadline: {:?}", deadline);
}
