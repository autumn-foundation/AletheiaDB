//! Multi-tenant isolation acceptance tests (Issue #3365).
//!
//! Proves the hard-isolation, quota-enforcement, and blast-radius guarantees of
//! the [`TenantManager`] end-to-end across representative query surfaces
//! (traversal, k-NN, `AS OF` temporal reads, property lookup, error contents),
//! plus a concurrent soak asserting zero cross-tenant leaks and zero
//! quota-bypass writes.

use aletheiadb::core::tenant::{QuotaDimension, TenantError, TenantQuota};
use aletheiadb::db::SimilarityQuery;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::prelude::*;
use aletheiadb::tenant::TenantManager;
use std::sync::Arc;

fn props(name: &str) -> aletheiadb::core::property::PropertyMap {
    PropertyMapBuilder::new().insert("name", name).build()
}

/// The core hard-isolation AC: no read surface returns another tenant's data,
/// even when node/edge IDs collide across tenants.
#[test]
fn no_read_surface_leaks_across_tenants() {
    let mgr = TenantManager::new_ephemeral();
    let a = mgr.create_tenant("a", TenantQuota::unlimited()).unwrap();
    let b = mgr.create_tenant("b", TenantQuota::unlimited()).unwrap();

    // Tenant A: alice --KNOWS--> bob.
    let a_alice = a.create_node("Person", props("alice-A")).unwrap();
    let a_bob = a.create_node("Person", props("bob-A")).unwrap();
    a.create_edge(a_alice, a_bob, "KNOWS", PropertyMap::default())
        .unwrap();

    // Tenant B: a single, differently-named node, at the SAME id space.
    let b_alice = b.create_node("Person", props("alice-B")).unwrap();
    assert_eq!(a_alice, b_alice, "id spaces collide by design");

    // 1) get_node: B's id resolves to B's data, never A's.
    let seen = b.db().get_node(b_alice).unwrap();
    assert_eq!(
        seen.properties.get("name").and_then(|v| v.as_str()),
        Some("alice-B")
    );

    // 2) property lookup: B never finds A's "bob-A".
    let bobs = b
        .db()
        .find_nodes_by_property("Person", "name", &PropertyValue::from("bob-A"));
    assert!(bobs.is_empty(), "B must not see A's node by property");

    // 3) traversal: B has no KNOWS edges, so traversing from B's node finds none,
    //    even though A has a KNOWS edge from the same id.
    let b_out = b.db().get_outgoing_edges(b_alice);
    assert!(b_out.is_empty(), "B has no edges; A's edge must not leak");
    let a_out = a.db().get_outgoing_edges(a_alice);
    assert_eq!(a_out.len(), 1, "A still sees its own edge");

    // 4) AS OF temporal read: B reconstructs only its own history.
    let now = time::now();
    let b_hist = b.db().get_node_at_time(b_alice, now, now).unwrap();
    assert_eq!(
        b_hist.properties.get("name").and_then(|v| v.as_str()),
        Some("alice-B")
    );

    // 5) error-content isolation: looking up an id that exists only in A must
    //    error in a tenant that doesn't have it, and the message must not carry
    //    A's data. Give A an extra node with a high id and confirm B errors.
    let a_extra = a.create_node("Secret", props("classified-A")).unwrap();
    let err = b.db().get_node(a_extra).unwrap_err();
    let msg = err.to_string();
    assert!(
        !msg.contains("classified-A"),
        "error text must not leak another tenant's data: {msg}"
    );
}

/// k-NN vector search is scoped to a single tenant: B's search never ranks A's
/// vectors, verified by result cardinality (colliding ids alone can't prove it).
#[test]
fn knn_search_is_tenant_scoped() {
    let mgr = TenantManager::new_ephemeral();
    let a = mgr.create_tenant("a", TenantQuota::unlimited()).unwrap();
    let b = mgr.create_tenant("b", TenantQuota::unlimited()).unwrap();

    for db in [a.db(), b.db()] {
        db.vector_index("embedding")
            .hnsw(HnswConfig::new(3, DistanceMetric::Cosine))
            .enable()
            .unwrap();
    }

    // A gets many vectors; B gets exactly two.
    for i in 0..20 {
        let v = vec![i as f32, (i * 2) as f32, 1.0];
        a.create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("name", format!("a-{i}"))
                .insert_vector("embedding", &v)
                .build(),
        )
        .unwrap();
    }
    let b_query = b
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("name", "b-0")
                .insert_vector("embedding", &[0.0f32, 0.0, 1.0])
                .build(),
        )
        .unwrap();
    b.create_node(
        "Doc",
        PropertyMapBuilder::new()
            .insert("name", "b-1")
            .insert_vector("embedding", &[1.0f32, 1.0, 1.0])
            .build(),
    )
    .unwrap();

    // Ask for 10 neighbours; B only HAS 2 vectors, so it can return at most 2 —
    // if A's 20 vectors leaked, we'd see more.
    let hits = b
        .db()
        .similarity_search(SimilarityQuery::from_node(b_query).k(10))
        .unwrap();
    assert!(
        hits.len() <= 2,
        "B's k-NN must not rank A's vectors (got {} hits)",
        hits.len()
    );
    for (id, _score) in &hits {
        let node = b.db().get_node(*id).unwrap();
        let name = node
            .properties
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap();
        assert!(name.starts_with("b-"), "leaked A vector into B: {name}");
    }
}

/// The single-tenant default is untouched: a plain `AletheiaDB` needs no manager
/// and behaves exactly as before.
#[test]
fn single_tenant_default_preserved() {
    let db = AletheiaDB::new().unwrap();
    let id = db.create_node("Person", props("solo")).unwrap();
    assert_eq!(
        db.get_node(id)
            .unwrap()
            .properties
            .get("name")
            .and_then(|v| v.as_str()),
        Some("solo")
    );
}

/// A tenant-scoped MCP server exposes the standard tool surface but sees only
/// its own tenant's data — existing agent tooling works unchanged.
#[cfg(feature = "mcp-server")]
#[test]
fn mcp_server_can_be_tenant_scoped() {
    let mgr = TenantManager::new_ephemeral();
    let a = mgr.create_tenant("a", TenantQuota::unlimited()).unwrap();
    let b = mgr.create_tenant("b", TenantQuota::unlimited()).unwrap();
    let a_secret = a.create_node("Secret", props("only-in-a")).unwrap();

    // A server scoped to B is backed by B's db (Arc-identical) — not A's.
    let b_server = b.mcp_server();
    assert!(
        Arc::ptr_eq(b_server.db(), b.db()),
        "scoped server must wrap the tenant's own db"
    );
    assert!(
        !Arc::ptr_eq(b_server.db(), a.db()),
        "scoped server must not wrap another tenant's db"
    );
    // And it cannot surface A's node under any parameterization: B has no such
    // node (colliding-id or otherwise), so the read behind the tool errors.
    assert!(
        b_server.db().get_node(a_secret).is_err(),
        "B-scoped server must not resolve A's node"
    );
}

/// Concurrent soak: 10 tenants interleave writes under per-tenant quotas. After
/// the storm, every tenant's data count exactly equals what it wrote (0 leaks)
/// and no tenant exceeded its quota (0 quota-bypass writes).
#[test]
fn concurrent_soak_zero_leaks_zero_quota_bypass() {
    const TENANTS: usize = 10;
    const PER_TENANT_CAP: u64 = 500;

    let mgr = Arc::new(TenantManager::new_ephemeral());
    let mut handles = Vec::new();
    for t in 0..TENANTS {
        let id = format!("tenant-{t}");
        mgr.create_tenant(&id, TenantQuota::unlimited().with_max_nodes(PER_TENANT_CAP))
            .unwrap();
    }

    // Each thread hammers its own tenant with more creates than the cap allows,
    // so the quota MUST reject the overflow — precisely, under concurrency.
    for t in 0..TENANTS {
        let mgr = Arc::clone(&mgr);
        let id = format!("tenant-{t}");
        handles.push(std::thread::spawn(move || {
            let handle = mgr.get_tenant(&id).unwrap();
            let mut accepted = 0u64;
            let mut rejected = 0u64;
            for i in 0..(PER_TENANT_CAP + 200) {
                match handle.create_node("N", props(&format!("{id}-{i}"))) {
                    Ok(_) => accepted += 1,
                    Err(Error::Tenant(TenantError::QuotaExceeded {
                        dimension: QuotaDimension::Nodes,
                        ..
                    })) => rejected += 1,
                    Err(e) => panic!("unexpected error: {e}"),
                }
            }
            (accepted, rejected)
        }));
    }

    for (t, h) in handles.into_iter().enumerate() {
        let (accepted, rejected) = h.join().unwrap();
        // Exactly the cap was accepted; the rest rejected — no quota bypass.
        assert_eq!(accepted, PER_TENANT_CAP, "tenant-{t} bypassed its quota");
        assert_eq!(rejected, 200, "tenant-{t} rejected the wrong count");
    }

    // Zero leaks: every tenant holds exactly its own cap-many nodes, and each
    // node's data belongs to that tenant.
    for t in 0..TENANTS {
        let id = format!("tenant-{t}");
        let handle = mgr.get_tenant(&id).unwrap();
        assert_eq!(handle.usage().node_count, PER_TENANT_CAP);
        assert_eq!(
            handle.db().stats().current.node_count as u64,
            PER_TENANT_CAP,
            "tenant-{t} storage count diverged from quota accounting"
        );
        let mine = handle.db().find_nodes_by_property(
            "N",
            "name",
            &PropertyValue::from(format!("{id}-0")),
        );
        assert_eq!(mine.len(), 1, "tenant-{t} lost or leaked its own node");
    }
}

/// Blast-radius: one tenant saturating writes does not break another tenant's
/// reads — they stay correct and keep completing while the writer storms.
#[test]
fn noisy_writer_does_not_break_reader() {
    let mgr = Arc::new(TenantManager::new_ephemeral());
    let writer = mgr
        .create_tenant("writer", TenantQuota::unlimited())
        .unwrap();
    let reader = mgr
        .create_tenant("reader", TenantQuota::unlimited())
        .unwrap();
    // Seed the reader with a known, stable node.
    let stable = reader.create_node("Person", props("stable")).unwrap();

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_thread = {
        let writer = writer.clone();
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut n = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                writer.create_node("N", props(&format!("w-{n}"))).unwrap();
                n += 1;
            }
            n
        })
    };

    // While the writer storms, the reader's view is stable and correct.
    for _ in 0..2_000 {
        let node = reader.db().get_node(stable).unwrap();
        assert_eq!(
            node.properties.get("name").and_then(|v| v.as_str()),
            Some("stable")
        );
        // Reader never sees any of the writer's nodes.
        assert_eq!(reader.usage().node_count, 1);
    }

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let written = writer_thread.join().unwrap();
    assert!(written > 0, "writer should have made progress");
    // The writer's data stayed entirely in its own tenant.
    assert_eq!(reader.usage().node_count, 1);
    assert_eq!(writer.usage().node_count, written);
}
