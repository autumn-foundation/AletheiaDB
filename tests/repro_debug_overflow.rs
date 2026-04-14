use aletheiadb::core::property::PropertyValue;
use std::sync::Arc;

#[test]
fn test_debug_stack_overflow() {
    // Construct a deeply nested PropertyValue iteratively.
    // Depth 20,000 is usually enough to blow the default stack (2MB-8MB).
    let depth = 20_000;
    let mut value = PropertyValue::Int(42);

    for _ in 0..depth {
        value = PropertyValue::Array(Arc::new(vec![value]));
    }

    // Try to debug print it.
    // This should NOT crash with safe implementation.
    println!("Attempting to print deeply nested value...");
    let debug_str = format!("{:?}", value);
    println!("Successfully printed debug: {:.20}...", debug_str);

    // Try to display print it.
    let display_str = format!("{}", value);
    println!("Successfully printed display: {:.20}...", display_str);

    // Drop the value.
    // This should NOT crash with safe Drop implementation.
    println!("Dropping deeply nested value...");
    drop(value);
    println!("Successfully dropped value.");
}
