use aletheiadb::core::version::{VectorDelta, PropertyDelta, floats_approx_equal};

#[test]
fn test_floats_approx_equal() {
    assert!(floats_approx_equal(1.0, 1.0));
    assert!(floats_approx_equal(1.0, 1.0 + 1e-6));
    assert!(!floats_approx_equal(1.0, 1.1));
}
