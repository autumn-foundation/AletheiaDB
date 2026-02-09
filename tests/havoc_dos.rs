use aletheiadb::core::property::PropertyValue;
use std::sync::Arc;
use std::time::Instant;

#[test]
fn test_billion_laughs_dos() {
    // Construct a "Billion Laughs" attack using Arc sharing.
    // Each level doubles the nodes of the previous level.
    // Depth 30 = 2^30 nodes = ~1 billion nodes.
    // Depth 40 = 2^40 nodes = ~1 trillion nodes.

    let mut value = PropertyValue::Null;

    // Create a DAG of depth 40 (1 Trillion nodes when expanded)
    // This fits in memory easily because we share the Arc pointer.
    for _ in 0..40 {
        value = PropertyValue::Array(Arc::new(vec![value.clone(), value.clone()]));
    }

    println!("Constructed DAG. Calculating size...");
    let start = Instant::now();

    // This should now fail gracefully with an error instead of hanging or OOMing.
    let result = value.serialized_size();
    println!("Result in {:?}: {:?}", start.elapsed(), result);

    assert!(result.is_err(), "serialized_size should error out on exponential expansion");
    let err = result.unwrap_err();
    assert!(format!("{}", err).contains("limit exceeded"), "Error should be about limits: {}", err);

    println!("Attempting serialization (should also fail gracefully)...");
    let ser_result = value.serialize();
    assert!(ser_result.is_err(), "serialize should error out");
    assert!(format!("{}", ser_result.unwrap_err()).contains("limit exceeded"));
}
