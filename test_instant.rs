use std::time::{Instant, Duration};

fn main() {
    let now = Instant::now();
    let huge = Duration::from_micros(u64::MAX);
    let sub = now.checked_sub(huge);
    println!("Now: {:?}", now);
    println!("Huge: {:?}", huge);
    println!("Sub: {:?}", sub);
}
