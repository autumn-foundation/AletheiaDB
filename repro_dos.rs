use aletheiadb::core::property::{PropertyValue, MAX_ARRAY_ELEMENTS, TAG_ARRAY};
use aletheiadb::core::error::StorageError;

fn main() {
    println!("Testing DoS protection for Array deserialization...");

    // Test 1: Amplification Attack
    // Construct a payload that declares 10M elements but provides only 5 bytes
    // Expected behavior: Fail FAST with "Insufficient buffer size" WITHOUT allocating 10M vector slots.
    println!("\nTest 1: Memory Amplification Attack");
    let mut bytes = Vec::new();
    bytes.push(TAG_ARRAY);
    let count = MAX_ARRAY_ELEMENTS as u32; // 10,000,000
    bytes.extend_from_slice(&count.to_le_bytes());

    // We do NOT append any data. Buffer size is 1 + 4 = 5 bytes.
    // Deserializer expects at least 10M bytes (1 byte per element tag).

    let start = std::time::Instant::now();
    let result = PropertyValue::deserialize(&bytes);
    let duration = start.elapsed();

    match result {
        Err(e) => {
            let msg = e.to_string();
            println!("Caught error: {}", msg);
            if msg.contains("Insufficient buffer size") {
                println!("PASS: Rejected amplification attack in {:?}", duration);
            } else {
                println!("FAIL: Wrong error message");
                std::process::exit(1);
            }
        },
        Ok(_) => {
            println!("FAIL: Deserialization succeeded unexpectedly!");
            std::process::exit(1);
        }
    }

    // Test 2: Capacity Limit Exceeded
    // Construct a payload that declares MAX + 1 elements
    // Expected behavior: Fail with "exceeds maximum allowed"
    println!("\nTest 2: Max Capacity Limit");
    let mut bytes_overflow = Vec::new();
    bytes_overflow.push(TAG_ARRAY);
    let count_overflow = (MAX_ARRAY_ELEMENTS + 1) as u32;
    bytes_overflow.extend_from_slice(&count_overflow.to_le_bytes());

    let result = PropertyValue::deserialize(&bytes_overflow);
    match result {
        Err(e) => {
            let msg = e.to_string();
            println!("Caught error: {}", msg);
            if msg.contains("exceeds maximum allowed") {
                println!("PASS: Rejected oversize array");
            } else {
                println!("FAIL: Wrong error message");
                std::process::exit(1);
            }
        }
        Ok(_) => {
            println!("FAIL: Oversize array accepted!");
            std::process::exit(1);
        }
    }
}
