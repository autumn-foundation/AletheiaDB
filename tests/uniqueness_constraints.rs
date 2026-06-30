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

// ──────────────────────────────────────────────────────────────
// 9. Edge-case coverage: disable on never-interned label,
//    display_string for Float/Bytes, Null skip, unsupported types,
//    WAL round-trip, and restart with index persistence.
// ──────────────────────────────────────────────────────────────

#[test]
fn disable_constraint_on_unknown_label_is_noop() {
    // disable_unique_constraint on a label that was never interned must
    // return Ok(()) without panicking (exercises the get_id early-return path).
    let db = in_memory_db();
    db.disable_unique_constraint("NeverInterned", "prop")
        .expect("disable on unknown label must be Ok");
}

#[test]
fn disable_constraint_on_known_but_unconstrained_label_is_noop() {
    // Intern the label (by creating a node) but don't declare a constraint.
    // disable must still return Ok(()) without panicking.
    let db = in_memory_db();
    db.create_node("Known", aletheiadb::properties! { "x" => "y" })
        .unwrap();
    db.disable_unique_constraint("Known", "x")
        .expect("disable on unconstrained label must be Ok");
}

#[test]
fn null_valued_property_does_not_block_enable() {
    // Nodes whose constrained property is Null must not prevent enable()
    // (Null is treated as "property absent", not a key to check for duplicates).
    let db = in_memory_db();

    // Node with Null for the would-be-constrained property (property not set).
    db.create_node("Person", aletheiadb::properties! { "name" => "Alice" })
        .unwrap();
    db.create_node("Person", aletheiadb::properties! { "name" => "Bob" })
        .unwrap();

    // enable must succeed — Null/absent entries are not counted as duplicates.
    db.unique_constraint("Person", "email")
        .enable()
        .expect("enable must succeed when all nodes lack the constrained property");
}

#[test]
fn unsupported_key_type_errors_on_enable() {
    // A node with a Vector-typed property on the constrained key must cause
    // enable() to return UnsupportedKeyType, not panic.
    let db = in_memory_db();

    let props = aletheiadb::core::property::PropertyMapBuilder::new()
        .insert_vector("embedding", &[0.1f32, 0.2, 0.3])
        .build();
    db.create_node("Doc", props).unwrap();

    let err = db
        .unique_constraint("Doc", "embedding")
        .enable()
        .expect_err("enable on Vector-typed property must fail");

    match err.as_constraint().expect("must be ConstraintError") {
        aletheiadb::core::error::ConstraintError::UnsupportedKeyType { .. } => {}
        other => panic!("expected UnsupportedKeyType, got {:?}", other),
    }
}

#[test]
fn wal_constraint_entries_survive_round_trip() {
    // Append DeclareUniqueConstraint + DropUniqueConstraint to a WAL,
    // read them back, and verify the constraint registry reflects the net state.
    use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
    use aletheiadb::storage::index_persistence::PersistenceConfig;
    use aletheiadb::storage::wal::DurabilityMode;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let wal_dir = dir.path().join("wal");

    // Disable index persistence so this test exercises only the WAL replay path
    // and does not write to (or read from) the shared default "./data" directory.
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

    // Session 1: declare a constraint then drop it, then declare a different one.
    {
        let db = aletheiadb::AletheiaDB::with_unified_config(make_config()).unwrap();
        db.unique_constraint("A", "x").enable().unwrap();
        db.disable_unique_constraint("A", "x").unwrap();
        db.unique_constraint("B", "y").enable().unwrap();
    }

    // Session 2: replay the WAL — only B.y must be active.
    {
        let db = aletheiadb::AletheiaDB::with_unified_config(make_config()).unwrap();
        let active = db.list_unique_constraints();
        assert!(
            !active.iter().any(|(l, p)| l == "A" && p == "x"),
            "dropped constraint A.x must not be present after replay: {active:?}"
        );
        assert!(
            active.iter().any(|(l, p)| l == "B" && p == "y"),
            "constraint B.y must survive WAL replay: {active:?}"
        );
        // B.y must still be enforced.
        db.create_node("B", aletheiadb::properties! { "y" => "dup" })
            .unwrap();
        db.create_node("B", aletheiadb::properties! { "y" => "dup" })
            .expect_err("B.y constraint must be enforced after replay");
    }
}

#[test]
fn constraint_survives_restart_via_index_persistence() {
    // Exercises the replay_constraint_declarations_from_wal path that is only
    // reached when index-persistence data exists on startup.
    use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
    use aletheiadb::storage::index_persistence::PersistenceConfig;
    use aletheiadb::storage::wal::DurabilityMode;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let wal_dir = dir.path().join("wal");
    let idx_dir = dir.path().join("indexes");

    let make_config = || {
        AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(wal_dir.clone())
                    .durability_mode(DurabilityMode::Synchronous)
                    .build(),
            )
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: idx_dir.clone(),
                load_on_startup: true,
                ..PersistenceConfig::default()
            })
            .build()
    };

    // Session 1: enable constraint and write a node.  Drop the DB cleanly so
    // the background persistence thread's shutdown handler writes a complete
    // snapshot (persist_all_indexes) that includes the manifest, string
    // interner, and graph index.
    {
        let db = aletheiadb::AletheiaDB::with_unified_config(make_config()).unwrap();
        db.unique_constraint("Person", "email").enable().unwrap();
        db.create_node("Person", aletheiadb::properties! { "email" => "keep@x" })
            .unwrap();
    }

    // Session 2: constraint must still be enforced via the combined
    // index-load + WAL-replay + replay_constraint_declarations_from_wal path.
    {
        let db = aletheiadb::AletheiaDB::with_unified_config(make_config()).unwrap();

        assert_eq!(db.node_count(), 1, "node must survive restart");

        db.create_node("Person", aletheiadb::properties! { "email" => "keep@x" })
            .expect_err("Person.email constraint must be enforced after index-persistence restart");

        db.create_node("Person", aletheiadb::properties! { "email" => "new@x" })
            .expect("different email must be allowed");
    }
}

// ──────────────────────────────────────────────────────────────
// 10. Additional coverage tests
// ──────────────────────────────────────────────────────────────

#[test]
fn update_node_to_new_unique_value_frees_old_slot() {
    // When a node's constrained property changes to a NEW value, the old
    // reservation must be freed on commit so another node can claim it.
    // This exercises ReservationGuard::commit() with a non-empty `to_remove`
    // set and the `guard.to_remove.insert(key)` path in reserve_for_transaction.
    let db = in_memory_db();
    db.unique_constraint("Person", "email").enable().unwrap();

    let alice_id = db
        .create_node(
            "Person",
            properties! { "email" => "alice@x", "name" => "Alice" },
        )
        .unwrap();

    // Update Alice to a brand-new email.
    db.write(|tx| {
        tx.update_node(
            alice_id,
            properties! { "email" => "alice_new@x", "name" => "Alice" },
        )
    })
    .expect("updating to a new unique email must succeed");

    // The old slot (alice@x) must now be free for a new node.
    db.create_node("Person", properties! { "email" => "alice@x" })
        .expect("old email slot must be freed after node update");

    // The new slot (alice_new@x) must still be reserved.
    db.create_node("Person", properties! { "email" => "alice_new@x" })
        .expect_err("new email slot must still be reserved for Alice");
}

#[test]
fn disable_constraint_with_interned_label_uninterned_property() {
    // Intern the label by creating a node, but never intern the property name.
    // disable_unique_constraint should return Ok(()) without panicking —
    // exercises the property get_id() → None early return in ops.rs.
    let db = in_memory_db();
    db.create_node("KnownLabel", properties! { "x" => "y" })
        .unwrap();

    // "PropertyNeverEverInterned" was never used as a property key anywhere.
    db.disable_unique_constraint("KnownLabel", "PropertyNeverEverInterned")
        .expect("disable with interned label but uninterned property must be Ok");
}

#[test]
fn non_string_typed_unique_keys_are_enforced() {
    // Exercises ValueKey::display_string() for Bool, Int, Float, and Bytes
    // variants — each produces a distinct display string used in error messages.

    // --- Bool ---
    {
        let db = in_memory_db();
        db.unique_constraint("Flag", "active").enable().unwrap();
        db.create_node("Flag", properties! { "active" => true })
            .unwrap();
        let err = db
            .create_node("Flag", properties! { "active" => true })
            .expect_err("duplicate Bool key must fail");
        let cv = err.as_constraint().expect("must be ConstraintError");
        match cv {
            ConstraintError::UniqueViolation { value, .. } => {
                // display_string for Bool: "true"
                assert!(!value.is_empty(), "Bool display string must not be empty");
            }
            other => panic!("expected UniqueViolation, got {:?}", other),
        }
    }

    // --- Int ---
    {
        let db = in_memory_db();
        db.unique_constraint("Counter", "count").enable().unwrap();
        db.create_node("Counter", properties! { "count" => 42i64 })
            .unwrap();
        let err = db
            .create_node("Counter", properties! { "count" => 42i64 })
            .expect_err("duplicate Int key must fail");
        let cv = err.as_constraint().expect("must be ConstraintError");
        match cv {
            ConstraintError::UniqueViolation { value, .. } => {
                assert_eq!(value, "42", "Int display string must be decimal");
            }
            other => panic!("expected UniqueViolation, got {:?}", other),
        }
    }

    // --- Float ---
    {
        let db = in_memory_db();
        db.unique_constraint("Score", "rating").enable().unwrap();
        db.create_node("Score", properties! { "rating" => 9.81f64 })
            .unwrap();
        let err = db
            .create_node("Score", properties! { "rating" => 9.81f64 })
            .expect_err("duplicate Float key must fail");
        let cv = err.as_constraint().expect("must be ConstraintError");
        match cv {
            ConstraintError::UniqueViolation { value, .. } => {
                assert!(!value.is_empty(), "Float display string must not be empty");
            }
            other => panic!("expected UniqueViolation, got {:?}", other),
        }
    }

    // --- Bytes (trigger DuplicateOnEnable since bytes can't be set via properties! macro) ---
    {
        use aletheiadb::core::property::{PropertyMapBuilder, PropertyValue};

        let db = in_memory_db();

        let make_bytes_props = || {
            PropertyMapBuilder::new()
                .insert("data", PropertyValue::bytes(b"\x01\x02\x03"))
                .build()
        };

        db.create_node("Doc", make_bytes_props()).unwrap();
        db.create_node("Doc", make_bytes_props()).unwrap();

        let err = db
            .unique_constraint("Doc", "data")
            .enable()
            .expect_err("enable with bytes duplicates must fail");
        let cv = err.as_constraint().expect("must be ConstraintError");
        match cv {
            ConstraintError::DuplicateOnEnable {
                value, node_ids, ..
            } => {
                // display_string for Bytes: "<bytes:N>"
                assert!(
                    value.starts_with("<bytes:"),
                    "Bytes display string must start with '<bytes:': {}",
                    value
                );
                assert_eq!(node_ids.len(), 2);
            }
            other => panic!("expected DuplicateOnEnable, got {:?}", other),
        }
    }
}

// ──────────────────────────────────────────────────────────────
// 11. Null-property coverage
// ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod null_property_coverage {
    use super::*;
    use aletheiadb::core::property::{PropertyMapBuilder, PropertyValue};

    /// A node whose constrained property is explicitly `PropertyValue::Null`
    /// must be allowed through — the reservation index skips Null values.
    /// Covers constraint.rs `reserve_for_transaction` `None => continue` arm.
    #[test]
    fn null_valued_constrained_property_is_skipped_in_reservation() {
        let db = in_memory_db();
        db.unique_constraint("Person", "email").enable().unwrap();

        let props = PropertyMapBuilder::new()
            .insert("email", PropertyValue::Null)
            .insert("name", "Alice")
            .build();

        db.create_node("Person", props)
            .expect("node with Null constrained property must be allowed");
    }

    /// Multiple nodes may share an explicit Null on the constrained property
    /// because Null is not indexed — enabling must succeed on such data.
    /// Covers constraint.rs `check_no_duplicates` Null `continue` arm.
    #[test]
    fn enable_constraint_skips_null_valued_nodes() {
        let db = in_memory_db();

        let null_props = PropertyMapBuilder::new()
            .insert("email", PropertyValue::Null)
            .build();
        db.create_node("Person", null_props.clone()).unwrap();
        db.create_node("Person", null_props).unwrap();

        db.unique_constraint("Person", "email").enable().expect(
            "enable must succeed when all existing nodes have Null on constrained property",
        );
    }
}
