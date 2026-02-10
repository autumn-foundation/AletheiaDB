use aletheiadb::core::interning::StringInterner;
use std::sync::Arc;
use std::thread;

#[test]
fn havoc_interner_capacity_overflow() {
    // 🧨 Attack: Flood interner with unique strings beyond capacity
    // 🛡️ Defense: Should return error, not crash or grow indefinitely

    let capacity = 100;
    let interner = StringInterner::with_max_capacity(capacity);

    // 1. Fill capacity
    for i in 0..capacity {
        let s = format!("string_{}", i);
        assert!(interner.intern(&s).is_ok(), "Failed to intern within capacity");
    }

    assert_eq!(interner.len(), capacity);

    // 2. Overflow
    for i in 0..50 {
        let s = format!("overflow_{}", i);
        let result = interner.intern(&s);
        assert!(result.is_err(), "Interner accepted string beyond capacity!");

        match result {
            Err(e) => {
                let msg = format!("{}", e);
                // Check for the user-facing error message, not the internal enum variant name
                assert!(msg.contains("Capacity exceeded"), "Wrong error type: {}", msg);
            }
            _ => unreachable!(),
        }
    }

    // 3. Verify size hasn't changed
    assert_eq!(interner.len(), capacity);

    // 4. Verify existing strings still work
    let id = interner.intern("string_0").unwrap();
    assert_eq!(interner.resolve_with(id, |s| s.to_string()).unwrap(), "string_0");
}

#[test]
fn havoc_interner_concurrent_flood() {
    // 🧨 Attack: Concurrent flood to race condition on capacity check
    // 🛡️ Defense: Atomic reservation + rollback

    let capacity = 1000;
    let interner = Arc::new(StringInterner::with_max_capacity(capacity));

    let mut handles = vec![];
    let num_threads = 20;
    let attempts_per_thread = 100; // Total 2000 attempts > 1000 capacity

    for t in 0..num_threads {
        let interner = interner.clone();
        handles.push(thread::spawn(move || {
            let mut success = 0;
            for i in 0..attempts_per_thread {
                let s = format!("t{}_s{}", t, i);
                if interner.intern(&s).is_ok() {
                    success += 1;
                }
            }
            success
        }));
    }

    let total_success: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();

    println!("Total successful interns: {}", total_success);

    // Should exactly fill capacity, no more
    assert_eq!(total_success, capacity, "Should exactly fill capacity despite race");
    assert_eq!(interner.len(), capacity);
}
