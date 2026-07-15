//! Lane B security tests: RBAC registry + constant-time bearer auth
//! (Issue #3563 fill; design doc §6 ACs a/b/c/g).
//!
//! These cover the pure role/class decision layer and the credential/verify
//! plumbing without standing up the full server (the live `/mcp` gate is proven
//! by `tests/server_parity.rs`). Structure:
//!
//! - **AC (b) role×tool RBAC**: `registry_matches_inventory_exactly`,
//!   `role_access_class_matrix_matches_role_allows`, `reader_denied_write_tool`,
//!   `metrics_role_permits_only_database_stats`, `unknown_tool_has_no_class`.
//! - **AC (c) constant-time verify**: `adapter_verify_delegates_to_constant_time_store`,
//!   `store_verify_rejects_wrong_and_empty_token`.
//! - **AC (g) 403-message parity**: `permission_denied_message_is_byte_identical`.
//! - **AC (a) startup refusal**: `validate_startup_*`.
//! - credential extraction: `extract_credential_*`.

use std::collections::BTreeMap;
use std::sync::Arc;

use aletheia_server::SecurityConfig;
use aletheia_server::security::rbac::{self, Denied};
use aletheia_server::security::{
    AuthStoreTokenAdapter, MetricsClass, ReadClass, WriteClass, authorize, authorize_class,
    extract_credential, validate_startup,
};
use aletheiadb::auth::{AccessClass, AuthMode, AuthStore, Principal, Role};
use aletheiadb::http::AletheiaHttpError;

// ── helpers ────────────────────────────────────────────────────────────────

/// Map an inventory `access_class` string to the typed [`AccessClass`],
/// panicking on any value outside the known four (so an unexpected class in
/// `inventory.json` fails the conformance test loudly rather than silently).
fn class_from_inventory(s: &str) -> AccessClass {
    match s {
        "read" => AccessClass::Read,
        "write" => AccessClass::Write,
        "metrics" => AccessClass::Metrics,
        "admin" => AccessClass::Admin,
        other => panic!("unexpected access_class {other:?} in inventory.json"),
    }
}

/// Obtain a real [`Principal`] carrying `role` (the type is `#[non_exhaustive]`,
/// so it can only be produced by the store or `anonymous()`).
fn principal_with_role(role: Role) -> Principal {
    let store = AuthStore::new();
    let token = "role-fixture-token-abcdefghijklmnop";
    store
        .insert_bootstrap_key("fixture", role, token)
        .expect("insert key");
    store.verify(token).expect("verify fixture key")
}

// ── AC (b): registry / inventory conformance ────────────────────────────────

/// The RBAC registry equals `tests/parity/inventory.json`'s `mcp.tools[]`
/// EXACTLY — same 52 names, same class per name, in both directions. The
/// inventory is the cross-lane source of truth; this pins the registry to it so
/// neither can drift silently.
#[test]
fn registry_matches_inventory_exactly() {
    // Repo-root inventory, located relative to this crate's manifest dir so the
    // test is CWD-independent.
    let raw = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/inventory.json"
    ));
    let doc: serde_json::Value = serde_json::from_str(raw).expect("inventory.json parses");
    let tools = doc["mcp"]["tools"].as_array().expect("mcp.tools array");

    let inventory: BTreeMap<String, AccessClass> = tools
        .iter()
        .map(|t| {
            let name = t["name"].as_str().expect("tool name").to_string();
            let class = class_from_inventory(t["access_class"].as_str().expect("access_class"));
            (name, class)
        })
        .collect();

    let registry: BTreeMap<String, AccessClass> = rbac::MCP_TOOL_CLASSES
        .iter()
        .map(|(n, c)| ((*n).to_string(), *c))
        .collect();

    assert_eq!(
        registry.len(),
        rbac::MCP_TOOL_CLASSES.len(),
        "registry has duplicate tool names"
    );
    assert_eq!(inventory.len(), 52, "inventory advertises 52 MCP tools");
    assert_eq!(
        registry, inventory,
        "MCP_TOOL_CLASSES must equal inventory.json mcp.tools[] exactly \
         (name + class, both directions)"
    );
}

/// The 4×4 role/class matrix the server enforces equals `Role::allows`
/// (Admin ⊇ all; Writer = {Read, Write, Metrics}; Reader = {Read, Metrics};
/// Metrics = {Metrics}).
#[test]
fn role_access_class_matrix_matches_role_allows() {
    use AccessClass::{Admin, Metrics, Read, Write};

    // The full 4×4 truth table the server enforces.
    let table: &[(Role, AccessClass, bool)] = &[
        (Role::Admin, Read, true),
        (Role::Admin, Write, true),
        (Role::Admin, Admin, true),
        (Role::Admin, Metrics, true),
        (Role::Writer, Read, true),
        (Role::Writer, Write, true),
        (Role::Writer, Admin, false),
        (Role::Writer, Metrics, true),
        (Role::Reader, Read, true),
        (Role::Reader, Write, false),
        (Role::Reader, Admin, false),
        (Role::Reader, Metrics, true),
        (Role::Metrics, Read, false),
        (Role::Metrics, Write, false),
        (Role::Metrics, Admin, false),
        (Role::Metrics, Metrics, true),
    ];
    for &(role, class, expected) in table {
        assert_eq!(
            role.allows(class),
            expected,
            "{role}.allows({class}) should be {expected}"
        );
    }
}

/// A reader is denied a write-class tool: `authorize_tool` returns the pure
/// [`Denied`] carrying `required_class = Write` / `principal_role = Reader`
/// (the `details` an error surface would attach). PERMISSION_DENIED is
/// non-retriable by contract (see the message-parity test for the HTTP shape).
#[test]
fn reader_denied_write_tool() {
    assert_eq!(
        rbac::authorize_tool(Role::Reader, "create_node"),
        Err(Denied {
            required_class: AccessClass::Write,
            principal_role: Role::Reader,
        }),
    );
    // A reader IS allowed a read-class tool.
    assert_eq!(rbac::authorize_tool(Role::Reader, "get_node"), Ok(()));
}

/// The `metrics` role permits only `database_stats` (the sole Metrics tool) and
/// is denied a sample read tool and a sample write tool.
#[test]
fn metrics_role_permits_only_database_stats() {
    assert_eq!(
        rbac::authorize_tool(Role::Metrics, "database_stats"),
        Ok(())
    );

    // Denied a read tool.
    assert_eq!(
        rbac::authorize_tool(Role::Metrics, "get_node"),
        Err(Denied {
            required_class: AccessClass::Read,
            principal_role: Role::Metrics,
        }),
    );
    // Denied a write tool.
    assert_eq!(
        rbac::authorize_tool(Role::Metrics, "create_node"),
        Err(Denied {
            required_class: AccessClass::Write,
            principal_role: Role::Metrics,
        }),
    );

    // database_stats is the ONLY tool a metrics role may invoke across the whole
    // registry.
    for (tool, _) in rbac::MCP_TOOL_CLASSES {
        let ok = rbac::authorize_tool(Role::Metrics, tool).is_ok();
        assert_eq!(
            ok,
            *tool == "database_stats",
            "metrics role: {tool} should be {}",
            if *tool == "database_stats" {
                "allowed"
            } else {
                "denied"
            }
        );
    }
}

/// An unknown/unregistered tool name has no class (`tool_access_class` → `None`,
/// which the dispatch gate MUST treat as reject) and `authorize_tool` rejects it
/// even for an admin — an unroutable name can never bypass the gate.
#[test]
fn unknown_tool_has_no_class() {
    assert_eq!(rbac::tool_access_class("nonexistent_tool"), None);
    assert!(rbac::authorize_tool(Role::Admin, "nonexistent_tool").is_err());
}

// ── AC (c): constant-time verify ────────────────────────────────────────────

/// The autumn `ApiTokenStore` adapter verifies **by delegation** to
/// `AuthStore::verify`, which compares SHA-256 digests with
/// `subtle::ConstantTimeEq` and never early-exits on a mismatch
/// (`src/auth/store.rs:319-330`). The adapter adds no timing-observable
/// comparison of its own: a correct token resolves to the principal id, a
/// wrong/empty token resolves to `None` (→ uniform 401 upstream).
#[tokio::test]
async fn adapter_verify_delegates_to_constant_time_store() {
    use autumn_web::auth::ApiTokenStore;

    let store = Arc::new(AuthStore::new());
    let token = "adapter-token-abcdefghijklmnop";
    store
        .insert_bootstrap_key("svc", Role::Reader, token)
        .expect("insert key");
    let principal_id = store.verify(token).expect("principal").id;

    let adapter = AuthStoreTokenAdapter::new(store);

    assert_eq!(
        adapter.verify(token).await.expect("verify ok"),
        Some(principal_id),
        "correct token resolves to the principal id via the constant-time store"
    );
    assert_eq!(
        adapter
            .verify("wrong-token-abcdefghijklmnop")
            .await
            .expect("verify ok"),
        None,
        "wrong token → None (uniform 401 upstream)"
    );
    assert_eq!(
        adapter.verify("").await.expect("verify ok"),
        None,
        "empty token → None"
    );
}

/// Direct constant-time-store negative test (`src/auth/store.rs:319-330`): a
/// wrong or empty presented key never verifies.
#[test]
fn store_verify_rejects_wrong_and_empty_token() {
    let store = AuthStore::new();
    let token = "store-token-abcdefghijklmnop";
    store
        .insert_bootstrap_key("svc", Role::Admin, token)
        .expect("insert key");

    assert!(store.verify(token).is_some(), "correct token verifies");
    assert!(store.verify("nope").is_none(), "wrong token rejected");
    assert!(store.verify("").is_none(), "empty token rejected");
}

// ── AC (g): 403-message parity ──────────────────────────────────────────────

/// The 403 body is byte-identical to the legacy surface:
/// `"role '{role}' does not permit {class} access"`.
#[test]
fn permission_denied_message_is_byte_identical() {
    let reader = principal_with_role(Role::Reader);

    // via authorize_class
    let err = authorize_class(&reader, AccessClass::Write).expect_err("reader denied write");
    assert!(
        matches!(err, AletheiaHttpError::PermissionDenied { .. }),
        "expected PermissionDenied"
    );
    // The Display free text is byte-identical to the legacy message.
    assert_eq!(
        err.to_string(),
        "role 'reader' does not permit write access"
    );

    // via the marker-generic authorize::<C> (the shape Authorized<C> calls)
    let err = authorize::<WriteClass>(&reader).expect_err("reader denied write class");
    assert!(
        matches!(err, AletheiaHttpError::PermissionDenied { .. }),
        "expected PermissionDenied"
    );
    assert_eq!(
        err.to_string(),
        "role 'reader' does not permit write access"
    );

    // A permitted class does not error.
    assert!(authorize::<ReadClass>(&reader).is_ok());

    // metrics role denied metrics? no — metrics IS permitted; check a denial the
    // matrix guarantees: metrics role denied read.
    let metrics = principal_with_role(Role::Metrics);
    let err = authorize::<ReadClass>(&metrics).expect_err("metrics denied read");
    assert!(
        matches!(err, AletheiaHttpError::PermissionDenied { .. }),
        "expected PermissionDenied"
    );
    assert_eq!(
        err.to_string(),
        "role 'metrics' does not permit read access"
    );
    assert!(authorize::<MetricsClass>(&metrics).is_ok());
}

/// Anonymous principals carry `Role::Admin`, so `authorize::<C>` passes every
/// class — anonymous mode's documented full access.
#[test]
fn anonymous_principal_passes_every_class() {
    let anon = Principal::anonymous();
    assert!(authorize::<ReadClass>(&anon).is_ok());
    assert!(authorize::<WriteClass>(&anon).is_ok());
    assert!(authorize::<MetricsClass>(&anon).is_ok());
    assert!(authorize_class(&anon, AccessClass::Admin).is_ok());
}

// ── AC (a): startup refusal ─────────────────────────────────────────────────

/// `validate_startup` refuses `Required` mode with an empty store (every request
/// would be rejected — fail closed at boot instead), allows `Required` with at
/// least one key, and always allows `Anonymous`.
#[test]
fn validate_startup_required_empty_store_is_refused() {
    let store = Arc::new(AuthStore::new());
    let cfg = SecurityConfig::new(store, AuthMode::Required);
    assert!(validate_startup(&cfg).is_err());
}

#[test]
fn validate_startup_required_with_key_is_ok() {
    let store = Arc::new(AuthStore::new());
    store
        .insert_bootstrap_key("admin", Role::Admin, "startup-token-abcdefghijklmnop")
        .expect("insert key");
    let cfg = SecurityConfig::new(store, AuthMode::Required);
    assert!(validate_startup(&cfg).is_ok());
}

#[test]
fn validate_startup_anonymous_is_ok() {
    let store = Arc::new(AuthStore::new());
    let cfg = SecurityConfig::new(store, AuthMode::Anonymous);
    assert!(validate_startup(&cfg).is_ok());
}

// ── credential extraction ───────────────────────────────────────────────────

/// Build request `Parts` carrying the given headers.
fn parts_with(headers: &[(&str, &str)]) -> axum::http::request::Parts {
    let mut builder = axum::http::Request::builder();
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    builder.body(()).expect("build request").into_parts().0
}

#[test]
fn extract_credential_bearer_is_case_insensitive() {
    for scheme in ["Bearer", "bearer", "BEARER", "BeArEr"] {
        let parts = parts_with(&[("authorization", &format!("{scheme} tok123"))]);
        assert_eq!(
            extract_credential(&parts).as_deref(),
            Some("tok123"),
            "scheme {scheme} should extract the token"
        );
    }
}

#[test]
fn extract_credential_x_api_key_is_accepted() {
    let parts = parts_with(&[("x-api-key", "tok456")]);
    assert_eq!(extract_credential(&parts).as_deref(), Some("tok456"));
}

#[test]
fn extract_credential_rejects_basic_missing_empty_and_whitespace() {
    // Non-bearer scheme.
    let basic = parts_with(&[("authorization", "Basic dXNlcjpwYXNz")]);
    assert_eq!(extract_credential(&basic), None, "Basic scheme rejected");

    // No credential header at all.
    let missing = parts_with(&[]);
    assert_eq!(extract_credential(&missing), None, "missing header → None");

    // Empty bearer token.
    let empty_bearer = parts_with(&[("authorization", "Bearer ")]);
    assert_eq!(
        extract_credential(&empty_bearer),
        None,
        "empty bearer token → None"
    );

    // Whitespace-only bearer token (trimmed to empty).
    let ws_bearer = parts_with(&[("authorization", "Bearer     ")]);
    assert_eq!(
        extract_credential(&ws_bearer),
        None,
        "whitespace-only bearer token → None"
    );

    // Whitespace-only x-api-key (trimmed to empty).
    let ws_key = parts_with(&[("x-api-key", "   ")]);
    assert_eq!(
        extract_credential(&ws_key),
        None,
        "whitespace-only x-api-key → None"
    );
}
