//! Unit tests for the multi-tenant runtime (Issue #3365).

use super::*;
use crate::core::property::PropertyMapBuilder;
use crate::core::tenant::TenantError;

fn props(name: &str) -> PropertyMap {
    PropertyMapBuilder::new().insert("name", name).build()
}

// --- Lifecycle -----------------------------------------------------------

#[test]
fn get_tenant_info_returns_single_metadata() {
    let mgr = TenantManager::new_ephemeral();
    let q = TenantQuota::unlimited().with_max_nodes(42);
    mgr.create_tenant("acme", q).unwrap();
    let info = mgr.get_tenant_info("acme").unwrap();
    assert_eq!(info.id.as_str(), "acme");
    assert_eq!(info.quota.max_nodes, Some(42));
    assert!(matches!(
        mgr.get_tenant_info("ghost"),
        Err(Error::Tenant(TenantError::NotFound { .. }))
    ));
}

/// Regression: a delete through the accounted handle of an entity created via
/// the unaccounted `db()` escape hatch must NOT underflow the usage counter to
/// `u64::MAX` and permanently brick the tenant's create path (review Finding 1/2).
#[test]
fn counter_never_underflows_on_bypass_delete() {
    let mgr = TenantManager::new_ephemeral();
    let t = mgr
        .create_tenant("t", TenantQuota::unlimited().with_max_nodes(5))
        .unwrap();
    // Create directly through db() — bypasses the reservation counter (stays 0).
    let n = t.db().create_node("N", props("bypass")).unwrap();
    assert_eq!(t.usage().node_count, 0, "bypass write is unaccounted");
    // Delete through the accounted handle: the underlying delete succeeds, and
    // the counter must clamp at 0 rather than wrap.
    t.delete_node(n).unwrap();
    assert_eq!(t.usage().node_count, 0, "counter clamped, did not wrap");
    // The tenant is NOT bricked — quota'd creates still work afterward.
    for _ in 0..5 {
        t.create_node("N", props("ok")).unwrap();
    }
    assert_eq!(t.usage().node_count, 5);
    assert!(t.create_node("N", props("over")).is_err());
}

#[test]
fn create_list_get_drop_lifecycle() {
    let mgr = TenantManager::new_ephemeral();
    assert_eq!(mgr.tenant_count(), 0);

    let a = mgr.create_tenant("acme", TenantQuota::unlimited()).unwrap();
    assert_eq!(a.id().as_str(), "acme");
    mgr.create_tenant("globex", TenantQuota::unlimited())
        .unwrap();
    assert_eq!(mgr.tenant_count(), 2);

    let names: Vec<String> = mgr
        .list_tenants()
        .into_iter()
        .map(|i| i.id.into_string())
        .collect();
    assert_eq!(names, vec!["acme", "globex"], "sorted by id");

    // get resolves.
    assert_eq!(mgr.get_tenant("acme").unwrap().id().as_str(), "acme");

    // drop deregisters.
    mgr.delete_tenant("acme").unwrap();
    assert_eq!(mgr.tenant_count(), 1);
    assert!(matches!(
        mgr.get_tenant("acme"),
        Err(Error::Tenant(TenantError::NotFound { .. }))
    ));
}

#[test]
fn duplicate_create_is_conflict() {
    let mgr = TenantManager::new_ephemeral();
    mgr.create_tenant("t1", TenantQuota::unlimited()).unwrap();
    assert!(matches!(
        mgr.create_tenant("t1", TenantQuota::unlimited()),
        Err(Error::Tenant(TenantError::AlreadyExists { .. }))
    ));
}

#[test]
fn drop_unknown_is_not_found() {
    let mgr = TenantManager::new_ephemeral();
    assert!(matches!(
        mgr.delete_tenant("ghost"),
        Err(Error::Tenant(TenantError::NotFound { .. }))
    ));
}

#[test]
fn malformed_id_is_invalid_argument() {
    let mgr = TenantManager::new_ephemeral();
    for bad in ["bad/id", "", "has space"] {
        assert!(
            matches!(
                mgr.create_tenant(bad, TenantQuota::unlimited()),
                Err(Error::Tenant(TenantError::InvalidId { .. }))
            ),
            "{bad:?} should be INVALID_ARGUMENT"
        );
    }
}

// --- Hard data isolation -------------------------------------------------

#[test]
fn ids_collide_across_tenants_without_interference() {
    let mgr = TenantManager::new_ephemeral();
    let a = mgr.create_tenant("a", TenantQuota::unlimited()).unwrap();
    let b = mgr.create_tenant("b", TenantQuota::unlimited()).unwrap();

    let a_alice = a.create_node("Person", props("alice-in-a")).unwrap();
    let b_alice = b.create_node("Person", props("alice-in-b")).unwrap();

    // IDs may collide (both fresh instances start at the same id space).
    assert_eq!(
        a_alice, b_alice,
        "independent tenants share an id space without interference"
    );

    // Each tenant only ever sees its own data — no cross leak by node id.
    let from_a = a.db().get_node(a_alice).unwrap();
    let from_b = b.db().get_node(b_alice).unwrap();
    assert_eq!(
        from_a.properties.get("name").and_then(|v| v.as_str()),
        Some("alice-in-a")
    );
    assert_eq!(
        from_b.properties.get("name").and_then(|v| v.as_str()),
        Some("alice-in-b")
    );
}

#[test]
fn dropping_one_tenant_does_not_affect_others() {
    let mgr = TenantManager::new_ephemeral();
    let a = mgr.create_tenant("a", TenantQuota::unlimited()).unwrap();
    let b = mgr.create_tenant("b", TenantQuota::unlimited()).unwrap();
    let b_node = b.create_node("Person", props("bob")).unwrap();

    mgr.delete_tenant("a").unwrap();

    // b still fully functional and queryable.
    assert!(b.db().get_node(b_node).is_ok());
    let b_node2 = b.create_node("Person", props("carol")).unwrap();
    assert!(b.db().get_node(b_node2).is_ok());
    // The dropped tenant's handle (a) still owns its live db until it drops, but
    // the registry no longer resolves it.
    assert!(matches!(
        mgr.get_tenant("a"),
        Err(Error::Tenant(TenantError::NotFound { .. }))
    ));
    let _ = a; // keep alive to prove b is unaffected regardless.
}

// --- Quotas: precise node/edge count enforcement -------------------------

#[test]
fn node_quota_is_enforced_with_no_partial_write() {
    let mgr = TenantManager::new_ephemeral();
    let t = mgr
        .create_tenant("t", TenantQuota::unlimited().with_max_nodes(2))
        .unwrap();

    t.create_node("N", props("1")).unwrap();
    t.create_node("N", props("2")).unwrap();
    assert_eq!(t.usage().node_count, 2);

    // Third create is rejected with a structured quota error…
    let err = t.create_node("N", props("3")).unwrap_err();
    match err {
        Error::Tenant(TenantError::QuotaExceeded {
            dimension,
            current,
            limit,
            ..
        }) => {
            assert_eq!(dimension, QuotaDimension::Nodes);
            assert_eq!(current, 2);
            assert_eq!(limit, 2);
        }
        other => panic!("expected QuotaExceeded, got {other:?}"),
    }
    // …and NO partial write occurred: usage is unchanged.
    assert_eq!(t.usage().node_count, 2);
}

#[test]
fn edge_quota_is_enforced() {
    let mgr = TenantManager::new_ephemeral();
    let t = mgr
        .create_tenant("t", TenantQuota::unlimited().with_max_edges(1))
        .unwrap();
    let n1 = t.create_node("N", props("1")).unwrap();
    let n2 = t.create_node("N", props("2")).unwrap();
    let n3 = t.create_node("N", props("3")).unwrap();

    t.create_edge(n1, n2, "R", PropertyMap::default()).unwrap();
    assert_eq!(t.usage().edge_count, 1);
    let err = t
        .create_edge(n1, n3, "R", PropertyMap::default())
        .unwrap_err();
    assert!(matches!(
        err,
        Error::Tenant(TenantError::QuotaExceeded {
            dimension: QuotaDimension::Edges,
            ..
        })
    ));
    assert_eq!(t.usage().edge_count, 1, "no partial edge write");
}

#[test]
fn delete_frees_quota_headroom() {
    let mgr = TenantManager::new_ephemeral();
    let t = mgr
        .create_tenant("t", TenantQuota::unlimited().with_max_nodes(1))
        .unwrap();
    let n = t.create_node("N", props("1")).unwrap();
    assert!(t.create_node("N", props("2")).is_err());

    // Free the slot, then a create succeeds again.
    t.delete_node(n).unwrap();
    assert_eq!(t.usage().node_count, 0);
    assert!(t.create_node("N", props("2")).is_ok());
    assert_eq!(t.usage().node_count, 1);
}

#[test]
fn cascade_delete_decrements_both_counters() {
    let mgr = TenantManager::new_ephemeral();
    let t = mgr.create_tenant("t", TenantQuota::unlimited()).unwrap();
    let n1 = t.create_node("N", props("1")).unwrap();
    let n2 = t.create_node("N", props("2")).unwrap();
    t.create_edge(n1, n2, "R", PropertyMap::default()).unwrap();
    assert_eq!(t.usage().node_count, 2);
    assert_eq!(t.usage().edge_count, 1);

    t.delete_node_cascade(n1).unwrap();
    assert_eq!(t.usage().node_count, 1);
    assert_eq!(
        t.usage().edge_count,
        0,
        "connected edge co-deleted + accounted"
    );
}

// --- Quotas: best-effort byte dimensions ---------------------------------

#[test]
fn storage_byte_quota_rejects_when_over() {
    let mgr = TenantManager::new_ephemeral();
    // A tiny storage budget: the estimator charges EST_ENTITY_BYTES per entity,
    // so a handful of nodes trips the cap.
    let t = mgr
        .create_tenant(
            "t",
            TenantQuota::unlimited().with_max_storage_bytes(EST_ENTITY_BYTES * 2),
        )
        .unwrap();
    // First writes fit under the estimate…
    t.create_node("N", props("1")).unwrap();
    t.create_node("N", props("2")).unwrap();
    // …now usage estimate >= limit, so the next write is rejected.
    let err = t.create_node("N", props("3")).unwrap_err();
    assert!(matches!(
        err,
        Error::Tenant(TenantError::QuotaExceeded {
            dimension: QuotaDimension::StorageBytes,
            ..
        })
    ));
}

#[test]
fn set_quota_adjusts_enforcement() {
    let mgr = TenantManager::new_ephemeral();
    let t = mgr
        .create_tenant("t", TenantQuota::unlimited().with_max_nodes(1))
        .unwrap();
    t.create_node("N", props("1")).unwrap();
    assert!(t.create_node("N", props("2")).is_err());

    // Raise the quota → the previously-rejected write now succeeds.
    mgr.set_tenant_quota("t", TenantQuota::unlimited().with_max_nodes(5))
        .unwrap();
    assert!(t.create_node("N", props("2")).is_ok());
    assert_eq!(t.usage().node_count, 2);
}

// --- Usage accounting ----------------------------------------------------

#[test]
fn usage_reflects_counts() {
    let mgr = TenantManager::new_ephemeral();
    let t = mgr.create_tenant("t", TenantQuota::unlimited()).unwrap();
    assert_eq!(t.usage(), TenantUsage::default());
    let n1 = t.create_node("N", props("1")).unwrap();
    let n2 = t.create_node("N", props("2")).unwrap();
    t.create_edge(n1, n2, "R", PropertyMap::default()).unwrap();
    let u = t.usage();
    assert_eq!(u.node_count, 2);
    assert_eq!(u.edge_count, 1);
    assert!(u.storage_bytes > 0, "storage estimate is populated");

    // manager-level usage endpoint agrees.
    assert_eq!(mgr.tenant_usage("t").unwrap(), u);
}

// --- Durable persistence + restart ---------------------------------------

#[cfg(all(feature = "serde", not(target_arch = "wasm32")))]
#[test]
fn durable_tenants_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let node_id;
    {
        let mgr = TenantManager::open(dir.path()).unwrap();
        assert!(mgr.is_durable());
        let t = mgr
            .create_tenant("acme", TenantQuota::unlimited().with_max_nodes(100))
            .unwrap();
        node_id = t.create_node("Person", props("alice")).unwrap();
        // mgr drops here, flushing each tenant db + the registry sidecar.
    }

    // Reopen: the tenant, its quota, and its data all come back.
    let mgr = TenantManager::open(dir.path()).unwrap();
    assert_eq!(mgr.tenant_count(), 1);
    let t = mgr.get_tenant("acme").unwrap();
    assert_eq!(t.quota().max_nodes, Some(100));
    let node = t.db().get_node(node_id).unwrap();
    assert_eq!(
        node.properties.get("name").and_then(|v| v.as_str()),
        Some("alice")
    );
    // Reservation counter was reseeded from storage, so quota still enforces.
    assert_eq!(t.usage().node_count, 1);
}

#[cfg(all(feature = "serde", not(target_arch = "wasm32")))]
#[test]
fn corrupt_registry_is_quarantined_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mgr = TenantManager::open(dir.path()).unwrap();
        mgr.create_tenant("t", TenantQuota::unlimited()).unwrap();
    }
    let sidecar = dir.path().join("tenants.json");
    std::fs::write(&sidecar, b"not json {{{").unwrap();

    // Reopen does not brick; the bad sidecar is quarantined.
    let mgr = TenantManager::open(dir.path()).unwrap();
    assert_eq!(mgr.tenant_count(), 0);
    assert!(!sidecar.exists());
    assert!(dir.path().join("tenants.json.corrupt").exists());
}

// --- Independent per-tenant backup / restore -----------------------------

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn per_tenant_backup_restore_is_independent() {
    let mgr = TenantManager::new_ephemeral();
    let src = mgr.create_tenant("src", TenantQuota::unlimited()).unwrap();
    let other = mgr
        .create_tenant("other", TenantQuota::unlimited())
        .unwrap();
    let n = src.create_node("Person", props("alice")).unwrap();
    other.create_node("Person", props("untouched")).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let artifact = dir.path().join("src.albk");
    src.backup(&artifact).unwrap();

    // Restore into a brand-new tenant; the other tenant is never touched.
    let restored = mgr
        .restore_tenant("restored", &artifact, TenantQuota::unlimited())
        .unwrap();
    let node = restored.db().get_node(n).unwrap();
    assert_eq!(
        node.properties.get("name").and_then(|v| v.as_str()),
        Some("alice")
    );
    assert_eq!(restored.usage().node_count, 1);
    // The unrelated tenant is unaffected by the restore.
    assert_eq!(other.usage().node_count, 1);
}
