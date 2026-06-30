//! Tests for uniqueness constraints (Issue #3218).
//!
//! Tests follow TDD red → green → refactor:
//! 1. Basic constraint enforcement on create_node / update_node
//! 2. Currently-valid semantics (deleted key is reusable)
//! 3. Pre-flight scan detects existing duplicates
//! 4. Concurrency: exactly 1 of N concurrent creates wins
//! 5. Persistence: constraint survives restart
//! 6. Property test: no two currently-valid nodes ever share a constrained key

use std::sync::{Arc, Barrier};
use std::thread;

use aletheiadb::core::error::ConstraintError;
use aletheiadb::{AletheiaDB, api::transaction::WriteOps, properties};

fn in_memory_db() -> AletheiaDB {
    AletheiaDB::new().expect("failed to create in-memory DB")
}

// ──────────────────────────────────────────────────────────────
// 1. Basic create_node constraint enforcement
// ──────────────────────────────────────────────────────────────

#[test]
fn constraint_blocks_duplicate_create_node() {
    let db = in_memory_db();

    db.unique_constraint("Person", "email").enable().unwrap();

    let _alice = db
        .create_node("Person", properties! { "email" => "alice@x" })
        .expect("first create must succeed");

    let err = db
        .create_node("Person", properties! { "email" => "alice@x" })
        .expect_err("duplicate must fail");

    let constraint_err = err.as_constraint().expect("error must be ConstraintError");
    match constraint_err {
        ConstraintError::UniqueViolation {
            label, property, ..
        } => {
            assert_eq!(label, "Person");
            assert_eq!(property, "email");
        }
        other => panic!("unexpected variant: {:?}", other),
    }

    // DB has exactly one Person node
    let nodes = db.get_nodes_by_label("Person");
    assert_eq!(nodes.len(), 1, "DB must contain exactly one Person node");
}

#[test]
fn constraint_is_label_scoped() {
    let db = in_memory_db();

    db.unique_constraint("Person", "email").enable().unwrap();

    db.create_node("Person", properties! { "email" => "alice@x" })
        .expect("Person/email=alice OK");

    // Different label – should be allowed
    db.create_node("Bot", properties! { "email" => "alice@x" })
        .expect("Bot/email=alice must NOT be blocked by Person constraint");
}

#[test]
fn unconstrained_property_allows_duplicates() {
    let db = in_memory_db();

    db.unique_constraint("Person", "email").enable().unwrap();

    // 'name' is not constrained
    db.create_node(
        "Person",
        properties! { "name" => "Alice", "email" => "a@x" },
    )
    .unwrap();
    db.create_node(
        "Person",
        properties! { "name" => "Alice", "email" => "b@x" },
    )
    .unwrap();
}

// ──────────────────────────────────────────────────────────────
// 2. update_node constraint enforcement
// ──────────────────────────────────────────────────────────────

#[test]
fn constraint_blocks_duplicate_update_node() {
    let db = in_memory_db();

    db.unique_constraint("Person", "email").enable().unwrap();

    let alice_id = db
        .create_node("Person", properties! { "email" => "alice@x" })
        .unwrap();
    let bob_id = db
        .create_node("Person", properties! { "email" => "bob@x" })
        .unwrap();
    let _ = bob_id;

    // Trying to change Bob's email to Alice's email must fail
    let err = db
        .write(|tx| tx.update_node(bob_id, properties! { "email" => "alice@x" }))
        .expect_err("update to duplicate email must fail");

    let constraint_err = err.as_constraint().expect("must be ConstraintError");
    match constraint_err {
        ConstraintError::UniqueViolation { property, .. } => {
            assert_eq!(property, "email");
        }
        other => panic!("unexpected variant: {:?}", other),
    }

    // alice_id unchanged
    let _ = alice_id;
    let alice = db.get_node(alice_id).unwrap();
    assert_eq!(
        alice.properties.get("email"),
        Some(&aletheiadb::core::PropertyValue::string("alice@x"))
    );
}

#[test]
fn update_node_to_own_value_is_idempotent() {
    let db = in_memory_db();

    db.unique_constraint("Person", "email").enable().unwrap();

    let alice_id = db
        .create_node(
            "Person",
            properties! { "email" => "alice@x", "name" => "Alice" },
        )
        .unwrap();

    // Updating alice's email to itself must succeed
    db.write(|tx| {
        tx.update_node(
            alice_id,
            properties! { "email" => "alice@x", "name" => "Alice Updated" },
        )
    })
    .expect("updating a node to keep its own constrained value must succeed");
}

// ──────────────────────────────────────────────────────────────
// 3. Currently-valid semantics: deleted key is reusable
// ──────────────────────────────────────────────────────────────

#[test]
fn deleted_key_is_reusable() {
    let db = in_memory_db();

    db.unique_constraint("Person", "email").enable().unwrap();

    let alice_id = db
        .create_node("Person", properties! { "email" => "alice@x" })
        .unwrap();

    // Delete Alice
    db.write(|tx| tx.delete_node(alice_id))
        .expect("delete must succeed");

    // Re-create with same email must succeed
    db.create_node("Person", properties! { "email" => "alice@x" })
        .expect("key reuse after deletion must be allowed");
}

// ──────────────────────────────────────────────────────────────
// 4. Pre-flight scan on enable
// ──────────────────────────────────────────────────────────────

#[test]
fn enable_fails_when_duplicates_already_exist() {
    let db = in_memory_db();

    // Create two nodes with the same email BEFORE enabling the constraint
    db.create_node("Person", properties! { "email" => "dup@x" })
        .unwrap();
    db.create_node("Person", properties! { "email" => "dup@x" })
        .unwrap();

    let err = db
        .unique_constraint("Person", "email")
        .enable()
        .expect_err("enable on label with existing duplicates must fail");

    let constraint_err = err.as_constraint().expect("must be ConstraintError");
    match constraint_err {
        ConstraintError::DuplicateOnEnable { node_ids, .. } => {
            assert!(
                node_ids.len() >= 2,
                "at least two conflicting node IDs must be reported"
            );
        }
        other => panic!("unexpected variant: {:?}", other),
    }

    // Constraint was NOT enabled (no enforcement on subsequent create)
    db.create_node("Person", properties! { "email" => "dup@x" })
        .expect("constraint not enabled, so duplicate still allowed");
}

#[test]
fn enable_succeeds_on_clean_label() {
    let db = in_memory_db();

    db.create_node("Person", properties! { "email" => "a@x" })
        .unwrap();
    db.create_node("Person", properties! { "email" => "b@x" })
        .unwrap();

    db.unique_constraint("Person", "email")
        .enable()
        .expect("no duplicates, enable must succeed");
}

// ──────────────────────────────────────────────────────────────
// 5. list_unique_constraints
// ──────────────────────────────────────────────────────────────

#[test]
fn list_unique_constraints_reflects_declarations() {
    let db = in_memory_db();

    assert!(
        db.list_unique_constraints().is_empty(),
        "no constraints declared yet"
    );

    db.unique_constraint("Person", "email").enable().unwrap();
    db.unique_constraint("Company", "vat_id").enable().unwrap();

    let constraints = db.list_unique_constraints();
    assert_eq!(constraints.len(), 2);
    assert!(
        constraints
            .iter()
            .any(|(l, p)| l == "Person" && p == "email"),
        "Person/email must be listed"
    );
    assert!(
        constraints
            .iter()
            .any(|(l, p)| l == "Company" && p == "vat_id"),
        "Company/vat_id must be listed"
    );
}

// ──────────────────────────────────────────────────────────────
// 6. Concurrency: exactly 1 of N wins
// ──────────────────────────────────────────────────────────────

#[test]
fn concurrent_creates_exactly_one_wins() {
    const N: usize = 100;

    let db = Arc::new(in_memory_db());
    db.unique_constraint("Person", "email").enable().unwrap();

    let barrier = Arc::new(Barrier::new(N));
    let mut handles = Vec::with_capacity(N);

    for _ in 0..N {
        let db_clone = Arc::clone(&db);
        let barrier_clone = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier_clone.wait();
            db_clone.create_node("Person", properties! { "email" => "shared@x" })
        }));
    }

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let successes: Vec<_> = results.iter().filter(|r| r.is_ok()).collect();
    let violations: Vec<_> = results
        .iter()
        .filter(|r| r.as_ref().err().and_then(|e| e.as_constraint()).is_some())
        .collect();

    assert_eq!(successes.len(), 1, "exactly 1 thread must win");
    assert_eq!(
        violations.len(),
        N - 1,
        "all others must get ConstraintViolation"
    );

    // DB has exactly one Person node
    let nodes = db.get_nodes_by_label("Person");
    assert_eq!(nodes.len(), 1);
}

// ──────────────────────────────────────────────────────────────
// 7. Persistence: constraint survives restart
// ──────────────────────────────────────────────────────────────

#[test]
fn constraint_survives_restart_via_wal() {
    use aletheiadb::config::AletheiaDBConfig;
    use aletheiadb::config::WalConfigBuilder;
    use aletheiadb::storage::index_persistence::PersistenceConfig;
    use aletheiadb::storage::wal::DurabilityMode;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let wal_dir = dir.path().join("wal");

    // Use WAL-only persistence so the test is deterministic and isolated:
    // no shared ./data directory that could be polluted by other test runs.
    let make_config = || {
        AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(wal_dir.clone())
                    .durability_mode(DurabilityMode::Synchronous)
                    .build(),
            )
            .persistence(PersistenceConfig {
                enabled: false,
                ..PersistenceConfig::default()
            })
            .build()
    };

    // First session: enable constraint and create a node
    {
        let db = AletheiaDB::with_unified_config(make_config()).unwrap();
        db.unique_constraint("Person", "email").enable().unwrap();
        db.create_node("Person", properties! { "email" => "alive@x" })
            .unwrap();
    }

    // Second session: constraint must still be enforced via WAL replay
    {
        let db = AletheiaDB::with_unified_config(make_config()).unwrap();

        let err = db
            .create_node("Person", properties! { "email" => "alive@x" })
            .expect_err("constraint must be enforced after restart");

        err.as_constraint()
            .expect("must be ConstraintError after restart");
    }
}

// ──────────────────────────────────────────────────────────────
// 8. Property test: no duplicate currently-valid nodes under random ops
// ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod proptest_suite {
    use super::*;
    use aletheiadb::core::NodeId;
    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    enum Op {
        Create(String),
        UpdateEmail { node_idx: usize, email: String },
        Delete { node_idx: usize },
    }

    fn email_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("a@x".to_string()),
            Just("b@x".to_string()),
            Just("c@x".to_string()),
        ]
    }

    fn op_strategy(max_nodes: usize) -> impl Strategy<Value = Op> {
        prop_oneof![
            email_strategy().prop_map(Op::Create),
            (0..max_nodes, email_strategy()).prop_map(|(idx, email)| Op::UpdateEmail {
                node_idx: idx,
                email
            }),
            (0..max_nodes).prop_map(|idx| Op::Delete { node_idx: idx }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]
        #[test]
        fn no_duplicates_under_random_ops(ops in proptest::collection::vec(op_strategy(5), 1..20)) {
            let db = in_memory_db();
            db.unique_constraint("Person", "email").enable().unwrap();

            let mut live_nodes: Vec<NodeId> = Vec::new();

            for op in ops {
                match op {
                    Op::Create(email) => {
                        if let Ok(id) = db.create_node("Person", properties! { "email" => email.as_str() }) {
                            live_nodes.push(id);
                        }
                    }
                    Op::UpdateEmail { node_idx, email } => {
                        if let Some(&node_id) = live_nodes.get(node_idx) {
                            let _ = db.write(|tx| tx.update_node(node_id, properties! { "email" => email.as_str() }));
                        }
                    }
                    Op::Delete { node_idx } => {
                        if let Some(&node_id) = live_nodes.get(node_idx) {
                            let _ = db.write(|tx| tx.delete_node(node_id));
                            live_nodes.retain(|&id| id != node_id);
                        }
                    }
                }
            }

            // Invariant: no two currently-valid nodes share the same email
            let persons = db.get_nodes_by_label("Person");
            let mut seen_emails: std::collections::HashSet<String> = std::collections::HashSet::new();
            for node in &persons {
                if let Some(aletheiadb::core::PropertyValue::String(email)) = node.properties.get("email") {
                    let email_str = email.to_string();
                    prop_assert!(
                        seen_emails.insert(email_str.clone()),
                        "duplicate email {} found among currently-valid nodes",
                        email_str
                    );
                }
            }
        }
    }
}
