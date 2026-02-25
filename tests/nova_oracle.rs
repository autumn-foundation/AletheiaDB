#![cfg(feature = "nova")]

use aletheiadb::AletheiaDB;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::experimental::oracle::Oracle;

#[test]
fn test_oracle_ppr_ranking() {
    let db = AletheiaDB::new().unwrap();
    let props = PropertyMapBuilder::new().build();

    // Create graph: A -> B -> C
    //               A -> D
    let a = db.create_node("Node", props.clone()).unwrap();
    let b = db.create_node("Node", props.clone()).unwrap();
    let c = db.create_node("Node", props.clone()).unwrap();
    let d = db.create_node("Node", props.clone()).unwrap();

    db.create_edge(a, b, "NEXT", props.clone()).unwrap();
    db.create_edge(b, c, "NEXT", props.clone()).unwrap();
    db.create_edge(a, d, "NEXT", props.clone()).unwrap();

    let oracle = Oracle::new(&db);

    // Run PPR from A
    // Alpha = 0.2 (20% restart)
    let ppr = oracle.personalized_page_rank(a, 0.2, 5000, 20).unwrap();

    let score_a = *ppr.get(&a).unwrap_or(&0.0);
    let score_b = *ppr.get(&b).unwrap_or(&0.0);
    let score_c = *ppr.get(&c).unwrap_or(&0.0);
    let score_d = *ppr.get(&d).unwrap_or(&0.0);

    // A is seed, always visited at start of each walk -> Highest score
    assert!(score_a > score_b, "A > B");
    assert!(score_a > score_d, "A > D");

    // B and D are 1 hop from A.
    // 50% chance to go B, 50% chance to go D (if no restart at A).
    // So B approx D.
    assert!(
        (score_b - score_d).abs() < 0.05,
        "B approx D (B={}, D={})",
        score_b,
        score_d
    );

    // C is 2 hops from A (via B).
    // B goes to C with (1 - alpha) probability.
    // So C should be < B.
    assert!(score_b > score_c, "B > C (B={}, C={})", score_b, score_c);

    // C should be > 0
    assert!(score_c > 0.0, "C should be visited");
}

#[test]
fn test_oracle_reachability() {
    let db = AletheiaDB::new().unwrap();
    let props = PropertyMapBuilder::new().build();

    let a = db.create_node("Start", props.clone()).unwrap();
    let b = db.create_node("End", props.clone()).unwrap();

    let oracle = Oracle::new(&db);

    // Disconnected: P(A->B) = 0
    let prob = oracle.reachability_probability(a, b, 5, 100).unwrap();
    assert!(prob < 0.01);

    // Connect A->B: P(A->B) = 1
    db.create_edge(a, b, "LINK", props.clone()).unwrap();
    let prob = oracle.reachability_probability(a, b, 5, 100).unwrap();
    assert!(prob > 0.99);
}
