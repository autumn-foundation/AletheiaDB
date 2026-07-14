//! RED→GREEN tests for the seam-independent cursor-signing core (Lane B4 /
//! Issue #3360).
//!
//! Mirrors the legacy #3360 contract (`src/mcp/cursor.rs`): an opaque, printable,
//! bounded base64url token, HMAC-SHA256-signed with a per-process secret, binding
//! the tool identity + snapshot bi-temporal coordinate + keyset/offset +
//! issued-at/expiry. Tampered / wrong-secret / wrong-tool tokens map to
//! `INVALID_ARGUMENT`; expiry and the per-connection live-cursor cap map to
//! `FAILED_PRECONDITION` (see Issue #3561 §8: cursor TTL/cap budgets).

use std::time::Duration;

use aletheia_server::security::cursor::{
    CursorError, CursorErrorClass, CursorPayload, CursorSecret, LiveCursorRegistry, MAX_TOKEN_LEN,
    TOKEN_PREFIX,
};

const NOW: i64 = 1_000_000;
const TTL: Duration = Duration::from_secs(300);

fn secret() -> CursorSecret {
    // Deterministic, explicit secret bytes — never a time/RNG-derived value in
    // tests, so round-trips are reproducible.
    CursorSecret::from_bytes([7u8; 32])
}

fn seed() -> CursorPayload {
    CursorPayload::seed((1_000, 2_000), 10, serde_json::json!({ "label": "Person" }))
}

// 1. Round-trip: mint then verify with same tool/secret/within-TTL → Ok, matches.
#[test]
fn round_trips_a_valid_token() {
    let s = secret();
    let token = s.mint_cursor(seed(), "list_nodes", NOW, TTL);
    let p = s
        .verify_cursor(&token, "list_nodes", NOW + 1_000_000)
        .expect("verify within TTL");
    assert_eq!(p.tool, "list_nodes");
    assert_eq!(p.svt, 1_000);
    assert_eq!(p.stt, 2_000);
    assert_eq!(p.limit, 10);
    assert_eq!(p.filters["label"], "Person");
    assert!(!p.cid.is_empty(), "mint stamps a cursor id");
    assert_eq!(p.iat, NOW, "mint stamps issued-at");
    assert_eq!(
        p.exp,
        NOW + TTL.as_micros() as i64,
        "mint stamps expiry = iat + ttl"
    );
}

// 2. Tamper / garbage / truncated / non-base64 → Invalid (INVALID_ARGUMENT), never panic.
#[test]
fn tampered_payload_is_invalid_argument() {
    let s = secret();
    let token = s.mint_cursor(seed(), "list_nodes", NOW, TTL);
    let body = token.strip_prefix(TOKEN_PREFIX).unwrap();
    let (payload_b64, sig_b64) = body.split_once('.').unwrap();
    let mut chars: Vec<char> = payload_b64.chars().collect();
    let idx = chars.len() / 2;
    chars[idx] = if chars[idx] == 'A' { 'B' } else { 'A' };
    let mutated: String = chars.into_iter().collect();
    let tampered = format!("{TOKEN_PREFIX}{mutated}.{sig_b64}");

    let err = s
        .verify_cursor(&tampered, "list_nodes", NOW)
        .expect_err("tampered token must be rejected");
    assert!(matches!(err, CursorError::Invalid(_)));
    assert_eq!(err.class(), CursorErrorClass::InvalidArgument);
}

#[test]
fn garbage_tokens_are_invalid_argument_never_panic() {
    let s = secret();
    for bad in [
        "",
        "not-a-cursor",
        "aletheiadb.cursor.v1.@@@.@@@",
        "aletheiadb.cursor.v1.onlyonesegment",
        TOKEN_PREFIX,
        "aletheiadb.cursor.v1.", // prefix with empty body
    ] {
        let err = s
            .verify_cursor(bad, "list_nodes", NOW)
            .expect_err("garbage must be rejected");
        assert_eq!(
            err.class(),
            CursorErrorClass::InvalidArgument,
            "token {bad:?} should be INVALID_ARGUMENT"
        );
    }
}

// 3. Wrong tool: minted for A, verified for B → Invalid.
#[test]
fn cross_tool_replay_is_invalid_argument() {
    let s = secret();
    let token = s.mint_cursor(seed(), "list_nodes", NOW, TTL);
    let err = s
        .verify_cursor(&token, "find_nodes_at_time", NOW)
        .expect_err("cross-tool replay must be rejected");
    assert_eq!(err.class(), CursorErrorClass::InvalidArgument);
}

// 4. Wrong secret / restart: a different secret cannot verify the token.
#[test]
fn token_does_not_survive_a_secret_change() {
    let a = secret();
    let b = CursorSecret::from_bytes([9u8; 32]);
    let token = a.mint_cursor(seed(), "list_nodes", NOW, TTL);
    let err = b
        .verify_cursor(&token, "list_nodes", NOW)
        .expect_err("a different secret must not verify");
    assert_eq!(err.class(), CursorErrorClass::InvalidArgument);
}

// 5. Expiry: now past issued_at+ttl → Expired (FAILED_PRECONDITION).
#[test]
fn expired_cursor_is_failed_precondition() {
    let s = secret();
    let token = s.mint_cursor(seed(), "list_nodes", NOW, TTL);
    let past_ttl = NOW + TTL.as_micros() as i64 + 1;
    let err = s
        .verify_cursor(&token, "list_nodes", past_ttl)
        .expect_err("expired token must be rejected");
    assert!(matches!(err, CursorError::Expired));
    assert_eq!(err.class(), CursorErrorClass::FailedPrecondition);

    // Exactly at expiry is expired (exclusive TTL boundary).
    let at_exp = NOW + TTL.as_micros() as i64;
    assert!(matches!(
        s.verify_cursor(&token, "list_nodes", at_exp),
        Err(CursorError::Expired)
    ));
}

// 6. Constant-time MAC: assert the comparison uses subtle::ConstantTimeEq
//    (usage test; a naive `==` on the tag would be a timing leak).
#[test]
fn mac_comparison_uses_constant_time_eq() {
    let src = include_str!("../src/security/cursor.rs");
    assert!(
        src.contains("use subtle::") && src.contains(".ct_eq("),
        "MAC tag comparison must go through subtle::ConstantTimeEq (.ct_eq)"
    );
    assert!(
        !src.contains("sig == expected") && !src.contains("expected == sig"),
        "MAC tag must not be compared with a variable-time `==`"
    );
}

// 7. Registry cap: register up to N ok; N+1 → FAILED_PRECONDITION; RAII release
//    frees a slot; release survives a panic.
#[test]
fn registry_enforces_per_connection_cap() {
    let reg = LiveCursorRegistry::new(2);
    let conn = 42u64;
    let a = reg.register(conn).expect("first lease");
    let _b = reg.register(conn).expect("second lease");
    assert_eq!(reg.live_count(conn), 2);

    let err = reg.register(conn).expect_err("cap exceeded");
    assert!(matches!(
        err,
        CursorError::CapExceeded {
            max_live_cursors: 2
        }
    ));
    assert_eq!(err.class(), CursorErrorClass::FailedPrecondition);

    // A different connection has its own budget.
    let _other = reg.register(7).expect("separate connection unaffected");

    // RAII release frees a slot.
    drop(a);
    assert_eq!(reg.live_count(conn), 1);
    let _c = reg.register(conn).expect("slot freed by drop");
    assert_eq!(reg.live_count(conn), 2);
}

#[test]
fn registry_releases_the_slot_on_panic() {
    let reg = std::sync::Arc::new(LiveCursorRegistry::new(1));
    let conn = 5u64;
    let reg2 = reg.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _lease = reg2.register(conn).expect("lease before panic");
        assert_eq!(reg2.live_count(conn), 1);
        panic!("boom while holding a lease");
    }));
    assert!(result.is_err(), "the closure panicked");
    // The lease's Drop ran during unwind, so the slot is free again.
    assert_eq!(reg.live_count(conn), 0);
    let _fresh = reg.register(conn).expect("slot reclaimed after panic");
}

// 8. Bounded / printable: minted token is base64url-printable and under the bound.
#[test]
fn token_is_printable_and_bounded() {
    let s = secret();
    let token = s.mint_cursor(seed(), "list_nodes", NOW, TTL);
    assert!(token.starts_with(TOKEN_PREFIX));
    assert!(token.len() < MAX_TOKEN_LEN, "token must be bounded");
    assert!(
        token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "token must be printable + escape-free, got {token}"
    );
}

// 9. Secret never logged / never Debug-leaked.
#[test]
fn secret_is_never_logged_or_debug_leaked() {
    let src = include_str!("../src/security/cursor.rs");
    for banned in ["println!", "eprintln!", "dbg!", "print!", "eprint!"] {
        assert!(
            !src.contains(banned),
            "cursor core must not contain `{banned}` (secret-leak risk)"
        );
    }
    // Debug is redacted: printing the secret reveals no key bytes.
    let s = CursorSecret::from_bytes([0xABu8; 32]);
    let dbg = format!("{s:?}");
    assert!(
        dbg.contains("redacted"),
        "CursorSecret Debug must be redacted, got {dbg}"
    );
    assert!(
        !dbg.contains("171") && !dbg.contains("ab") && !dbg.contains("AB"),
        "CursorSecret Debug must not print key bytes, got {dbg}"
    );
}
