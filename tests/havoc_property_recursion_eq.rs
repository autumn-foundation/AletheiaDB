use aletheiadb::core::property::PropertyValue;
use std::sync::Arc;

#[test]
fn test_havoc_partial_eq_stack_overflow() {
    let mut v1 = PropertyValue::Int(1);
    let mut v2 = PropertyValue::Int(1);
    for _ in 0..100000 {
        v1 = PropertyValue::Array(Arc::new(vec![v1]));
        v2 = PropertyValue::Array(Arc::new(vec![v2]));
    }
    assert!(v1 == v2);
}
