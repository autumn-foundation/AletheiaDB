use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::core::graph::Node;
use aletheiadb::core::id::{NodeId, VersionId};
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::query::ir::{Predicate, PredicateValue};
use aletheiadb::query::executor::{FilterIterator, ResultIterator, QueryRow, EntityResult};

fn test_eval(prop_is_int: bool, val: f64) {
    let mut builder = PropertyMapBuilder::new();
    if prop_is_int {
        builder = builder.insert("val", 25);
    } else {
        builder = builder.insert("val", 25.0);
    }
    let props = builder.build();
    let node = Node::new(
        NodeId::new(1).unwrap(),
        GLOBAL_INTERNER.intern("Person").unwrap(),
        props,
        VersionId::new(1).unwrap(),
    );

    struct SingleNodeIterator { node: Option<Node> }
    impl ResultIterator for SingleNodeIterator {
        fn next(&mut self) -> Option<Result<QueryRow, aletheiadb::core::error::Error>> {
            self.node.take().map(|n| {
                Ok(QueryRow::from_entity(EntityResult::Node(n)))
            })
        }
        fn size_hint(&self) -> (usize, Option<usize>) { (1, Some(1)) }
    }

    // Eq
    let iter = Box::new(SingleNodeIterator { node: Some(node.clone()) });
    let pred = Predicate::eq("val", PredicateValue::Float(val));
    assert!(FilterIterator::new(iter, pred).next().is_some(), "Eq Int vs Float failed");

    // Ne
    let iter = Box::new(SingleNodeIterator { node: Some(node.clone()) });
    let pred = Predicate::ne("val", PredicateValue::Float(val));
    assert!(FilterIterator::new(iter, pred).next().is_none(), "Ne Int vs Float failed");

    // Gt
    let iter = Box::new(SingleNodeIterator { node: Some(node.clone()) });
    let pred = Predicate::gt("val", PredicateValue::Float(val - 1.0));
    assert!(FilterIterator::new(iter, pred).next().is_some(), "Gt Int vs Float failed");

    // Lt
    let iter = Box::new(SingleNodeIterator { node: Some(node.clone()) });
    let pred = Predicate::lt("val", PredicateValue::Float(val + 1.0));
    assert!(FilterIterator::new(iter, pred).next().is_some(), "Lt Int vs Float failed");

    // Gte
    let iter = Box::new(SingleNodeIterator { node: Some(node.clone()) });
    let pred = Predicate::Gte { key: "val".to_string(), value: PredicateValue::Float(val) };
    assert!(FilterIterator::new(iter, pred).next().is_some(), "Gte Int vs Float failed");

    // Lte
    let iter = Box::new(SingleNodeIterator { node: Some(node.clone()) });
    let pred = Predicate::Lte { key: "val".to_string(), value: PredicateValue::Float(val) };
    assert!(FilterIterator::new(iter, pred).next().is_some(), "Lte Int vs Float failed");
}

#[test]
fn do_test() {
    test_eval(true, 25.0);
}
