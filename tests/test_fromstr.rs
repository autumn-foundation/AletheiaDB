use aletheiadb::prelude::*;
use std::str::FromStr;

#[test]
fn test_from_str() {
    let id = NodeId::from_str("42").unwrap();
    assert_eq!(id, NodeId::new(42).unwrap());
}
