use std::sync::Arc;
use std::time::{Duration, Instant};
use std::thread;
use aletheiadb::storage::wal::flush_coordinator::FlushSignal;

#[test]
fn havoc_wal_spurious_wakeup() {
    let signal = Arc::new(FlushSignal::new());

    let signal_clone = Arc::clone(&signal);

    let t = thread::spawn(move || {
        let start = Instant::now();
        // The thread waits for 2 seconds
        let result = signal_clone.wait_for_request(Duration::from_secs(2));
        let elapsed = start.elapsed();
        (result, elapsed)
    });

    // Give it a moment to enter wait
    thread::sleep(Duration::from_millis(50));

    // Simulating spurious wakeup by manually requesting a flush BUT we swap it back instantly.
    // Actually, wait_for_request says: "Handles spurious wakeups by checking the actual requested flag."
    // Let's invoke request_flush() then IMMEDIATELY take_request() so the flag is false when the thread wakes up.
    signal.request_flush();
    signal.take_request();

    let (was_requested, elapsed) = t.join().unwrap();

    // Because wait_for_request doesn't loop, it wakes up from the condvar, checks `requested` (which is false), and returns false immediately!
    // It should have slept for ~2 seconds.
    assert!(!was_requested, "Spurious wakeup correctly resulted in false");

    if elapsed < Duration::from_secs(1) {
        panic!("👺 HAVOC SUCCESS: Spurious wakeup caused premature return! Elapsed: {:?}. The condvar didn't loop until timeout.", elapsed);
    }
}
