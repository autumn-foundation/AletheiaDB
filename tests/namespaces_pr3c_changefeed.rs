//! Namespace-scoped changefeed subscriptions (Issue #3349, PR3c).
//!
//! A subscriber scoped to namespace `A` must receive only `A`'s committed changes
//! (across create/update/delete of nodes AND edges); an unscoped subscriber must
//! still see everything (back-compat). Every change record must carry the entity's
//! immutable namespace, and the reserved `__aletheia_ns` ride-along key must never
//! surface.

use aletheiadb::api::WriteOps;
use aletheiadb::core::changefeed::{ChangeRecord, EntityKind};
use aletheiadb::{
    AletheiaDB, ChangeFilter, Error, Namespace, NamespaceScope, PropertyMap, PropertyMapBuilder,
};

fn props(name: &str) -> PropertyMap {
    PropertyMapBuilder::new().insert("name", name).build()
}

fn scope_a() -> ChangeFilter {
    ChangeFilter::all()
        .with_namespace_scope(NamespaceScope::Single(Namespace::new("agent:a").unwrap()))
}

/// The set of namespaces observed across a batch of change records.
fn namespaces_seen(records: &[ChangeRecord]) -> std::collections::BTreeSet<String> {
    records
        .iter()
        .map(|r| r.namespace.as_str().to_string())
        .collect()
}

#[test]
fn subscriber_scoped_to_a_receives_only_a_node_changes() {
    let db = AletheiaDB::new().unwrap();
    let sub = db.subscribe_changes(scope_a()).unwrap();

    // agent:a node lifecycle: create → update → delete.
    let a = db
        .create_node_in_namespace("Person", props("Alice"), "agent:a")
        .unwrap();
    db.write(|tx| {
        tx.update_node(a, props("Alice2"))?;
        Ok::<_, Error>(())
    })
    .unwrap();
    db.write(|tx| {
        tx.delete_node(a)?;
        Ok::<_, Error>(())
    })
    .unwrap();

    // agent:b noise that must NOT be delivered.
    let b = db
        .create_node_in_namespace("Person", props("Bob"), "agent:b")
        .unwrap();
    db.write(|tx| {
        tx.update_node(b, props("Bob2"))?;
        Ok::<_, Error>(())
    })
    .unwrap();

    let events = sub.poll();
    assert!(!events.is_empty(), "A-scoped subscriber received nothing");
    assert_eq!(
        namespaces_seen(&events),
        std::collections::BTreeSet::from(["agent:a".to_string()]),
        "A-scoped subscriber must only see agent:a changes, got {:?}",
        namespaces_seen(&events)
    );
    // All three lifecycle events for the agent:a node arrived.
    assert!(events.iter().all(|r| r.entity_id == a.as_u64()));
    assert_eq!(
        events.len(),
        3,
        "expected create+update+delete of the agent:a node"
    );
}

#[test]
fn subscriber_scoped_to_a_receives_only_a_edge_changes() {
    let db = AletheiaDB::new().unwrap();
    let sub = db.subscribe_changes(scope_a()).unwrap();

    let a1 = db
        .create_node_in_namespace("Person", props("a1"), "agent:a")
        .unwrap();
    let a2 = db
        .create_node_in_namespace("Person", props("a2"), "agent:a")
        .unwrap();
    // agent:a edge lifecycle.
    let ea = db
        .create_edge_in_namespace(a1, a2, "KNOWS", props(""), "agent:a")
        .unwrap();
    db.write(|tx| {
        tx.update_edge(ea, props("close"))?;
        Ok::<_, Error>(())
    })
    .unwrap();

    // agent:b edge noise.
    let b1 = db
        .create_node_in_namespace("Person", props("b1"), "agent:b")
        .unwrap();
    let b2 = db
        .create_node_in_namespace("Person", props("b2"), "agent:b")
        .unwrap();
    let eb = db
        .create_edge_in_namespace(b1, b2, "KNOWS", props(""), "agent:b")
        .unwrap();
    db.write(|tx| {
        tx.delete_edge(eb)?;
        Ok::<_, Error>(())
    })
    .unwrap();

    let events = sub.poll();
    assert_eq!(
        namespaces_seen(&events),
        std::collections::BTreeSet::from(["agent:a".to_string()]),
        "A-scoped subscriber saw a foreign namespace: {:?}",
        namespaces_seen(&events)
    );
    // The agent:a edge's create + update are present; no agent:b entity leaked.
    // NOTE: node ids and edge ids are separate u64 spaces, so a record must be
    // matched on (kind, entity_id) — not entity_id alone (which can collide across
    // kinds). The namespaces_seen assertion above is the primary isolation proof.
    assert!(
        events
            .iter()
            .any(|r| r.kind == EntityKind::Edge && r.entity_id == ea.as_u64()),
        "missing the agent:a edge change"
    );
    assert!(
        events
            .iter()
            .all(|r| !(r.kind == EntityKind::Edge && r.entity_id == eb.as_u64())),
        "the agent:b edge leaked into the A-scoped feed"
    );
    assert!(
        events
            .iter()
            .all(|r| !(r.kind == EntityKind::Node && r.entity_id == b1.as_u64())),
        "an agent:b node leaked into the A-scoped feed"
    );
}

#[test]
fn unscoped_subscriber_sees_all_namespaces_back_compat() {
    let db = AletheiaDB::new().unwrap();
    // No namespace axis → back-compat: every namespace delivered.
    let sub = db.subscribe_changes(ChangeFilter::all()).unwrap();

    db.create_node_in_namespace("Person", props("a"), "agent:a")
        .unwrap();
    db.create_node_in_namespace("Person", props("b"), "agent:b")
        .unwrap();
    db.write(|tx| {
        tx.create_node("Person", props("legacy"))?; // default namespace
        Ok::<_, Error>(())
    })
    .unwrap();

    let events = sub.poll();
    assert_eq!(
        namespaces_seen(&events),
        std::collections::BTreeSet::from([
            "agent:a".to_string(),
            "agent:b".to_string(),
            "default".to_string(),
        ]),
        "unscoped subscriber must see all namespaces, got {:?}",
        namespaces_seen(&events)
    );
}

#[test]
fn union_scope_sees_only_named_namespaces() {
    let db = AletheiaDB::new().unwrap();
    let filter = ChangeFilter::all().with_namespace_scope(
        NamespaceScope::list(vec![
            Namespace::new("agent:a").unwrap(),
            Namespace::new("shared").unwrap(),
        ])
        .unwrap(),
    );
    let sub = db.subscribe_changes(filter).unwrap();

    db.create_node_in_namespace("Person", props("a"), "agent:a")
        .unwrap();
    db.create_node_in_namespace("Doc", props("sh"), "shared")
        .unwrap();
    db.create_node_in_namespace("Person", props("b"), "agent:b")
        .unwrap();

    let events = sub.poll();
    assert_eq!(
        namespaces_seen(&events),
        std::collections::BTreeSet::from(["agent:a".to_string(), "shared".to_string()]),
        "union scope leaked a namespace: {:?}",
        namespaces_seen(&events)
    );
}

#[test]
fn default_entity_change_carries_default_namespace() {
    let db = AletheiaDB::new().unwrap();
    let sub = db.subscribe_changes(ChangeFilter::all()).unwrap();
    db.write(|tx| {
        tx.create_node("Person", props("legacy"))?;
        Ok::<_, Error>(())
    })
    .unwrap();
    let events = sub.poll();
    assert_eq!(events.len(), 1);
    assert!(events[0].namespace.is_default());
    assert_eq!(events[0].namespace.as_str(), "default");
}

#[test]
fn list_changes_records_carry_namespace_and_never_leak_reserved_key() {
    use aletheiadb::core::changefeed::ChangeFeedQuery;
    use aletheiadb::core::temporal::{TIMESTAMP_MAX, Timestamp};

    let db = AletheiaDB::new().unwrap();
    db.create_node_in_namespace("Person", props("Alice"), "agent:a")
        .unwrap();
    db.write(|tx| {
        tx.create_node("Person", props("legacy"))?;
        Ok::<_, Error>(())
    })
    .unwrap();

    let q = ChangeFeedQuery::new(Timestamp::from(0), TIMESTAMP_MAX, 100_000);
    let page = db.list_changes(&q).unwrap();
    // Every record carries a resolved namespace; the reserved key never appears as
    // a namespace value (it is surfaced as the field, not the raw key).
    let seen = namespaces_seen(&page.changes);
    assert!(seen.contains("agent:a"));
    assert!(seen.contains("default"));
    assert!(
        !seen.iter().any(|ns| ns.starts_with("__aletheia_")),
        "reserved ride-along key leaked as a namespace value: {seen:?}"
    );
}

/// A namespace-scoped subscription's `matches()` composes with a label filter:
/// only the intersection is delivered.
#[test]
fn namespace_and_label_intersection() {
    let db = AletheiaDB::new().unwrap();
    let filter = ChangeFilter::all()
        .with_node_labels(["Person"])
        .with_namespace_scope(NamespaceScope::Single(Namespace::new("agent:a").unwrap()));
    let sub = db.subscribe_changes(filter).unwrap();

    db.create_node_in_namespace("Person", props("a"), "agent:a")
        .unwrap(); // match
    db.create_node_in_namespace("Company", props("a"), "agent:a")
        .unwrap(); // wrong label
    db.create_node_in_namespace("Person", props("b"), "agent:b")
        .unwrap(); // wrong namespace

    let events = sub.poll();
    assert_eq!(events.len(), 1, "only Person@agent:a should match");
    assert_eq!(events[0].label, "Person");
    assert_eq!(events[0].namespace.as_str(), "agent:a");
}
