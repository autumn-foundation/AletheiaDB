use std::time::Duration;

fn main() {
    let _ = std::time::Instant::now() + Duration::MAX;
    println!("didn't panic");
}
