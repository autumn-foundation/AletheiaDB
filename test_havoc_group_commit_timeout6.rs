use std::time::Duration;

fn main() {
    let _ = std::time::Instant::now().checked_add(Duration::MAX);
    println!("didn't panic");
}
