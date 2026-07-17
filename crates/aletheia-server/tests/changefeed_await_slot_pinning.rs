//! Event-driven changefeed long-poll: worker-pin removal + prompt slot release
//! on client disconnect (Issue #3673), driven against the live assembled
//! autumn-web server and the core `Subscription::recv_async` primitive.
//!
//! These lock the ACs:
//!
//! - **(a) worker-starvation**: N concurrent `recv_async` long-polls on a SMALL
//!   worker pool do NOT starve unrelated work — a suspended async wait pins zero
//!   worker threads (RED if `recv_async` blocks the thread like the old
//!   `block_in_place`/`recv_timeout` path).
//! - **(b) prompt disconnect release**: an SSE `GET /changes/stream` client and an
//!   HTTP `POST /changes/await` long-poll BOTH free their per-principal slot in
//!   well under the old 15s / 60s bound when the client disconnects — event-driven
//!   (`tx.closed()` / future-drop), no commits needed to wake the worker.
//! - **(b) + #3688 churn**: rapid reconnect churn near the per-principal cap never
//!   falsely exhausts the quota (stale slots are reaped promptly).
//! - **(c) delivery unchanged**: the event-driven path drains cursor-ascending and
//!   losslessly, identical to the sync `recv_timeout` contract.
//! - normal-path timeout still releases; disconnect decrements the counter exactly
//!   once; `database_stats` per-principal returns to baseline promptly.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aletheia_server::build_server_client;
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::core::changefeed::ChangeRecord;
use aletheiadb::core::changefeed_subscription::ChangefeedConfig;
use aletheiadb::{AletheiaDB, ChangeFilter, PropertyMapBuilder};
use serde_json::{Value, json};

const READER_TOKEN: &str = "reader-token-abcdefghijklmnop";
const WRITER_TOKEN: &str = "writer-token-abcdefghijklmnop";
const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";

// ── fixtures / helpers ───────────────────────────────────────────────────────

fn new_db() -> Arc<AletheiaDB> {
    Arc::new(AletheiaDB::new().expect("create db"))
}

fn keyed_store() -> Arc<AuthStore> {
    let store = Arc::new(AuthStore::new());
    store
        .insert_bootstrap_key("reader", Role::Reader, READER_TOKEN)
        .expect("reader key");
    store
        .insert_bootstrap_key("writer", Role::Writer, WRITER_TOKEN)
        .expect("writer key");
    store
        .insert_bootstrap_key("admin", Role::Admin, ADMIN_TOKEN)
        .expect("admin key");
    store
}

fn principal_id(store: &AuthStore, token: &str) -> String {
    store.verify(token).expect("verify token").id
}

/// The live per-principal subscription count for `id` (0 if absent).
fn principal_count(db: &AletheiaDB, id: &str) -> usize {
    db.changefeed_principal_counts()
        .into_iter()
        .find(|(k, _)| k == id)
        .map(|(_, c)| c)
        .unwrap_or(0)
}

/// Poll `cond` every 20ms until it holds or `bound` elapses. Returns the elapsed
/// time on success, or `None` if the bound was hit first.
async fn wait_until(bound: Duration, mut cond: impl FnMut() -> bool) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < bound {
        if cond() {
            return Some(start.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond().then(|| start.elapsed())
}

/// A committed change wakes any live subscription (used to advance the feed).
fn commit(db: &AletheiaDB, n: i64) {
    let _ = db.create_node("Ping", PropertyMapBuilder::new().insert("n", n).build());
}

/// A stable dedup/order key equivalent to the `ChangeCursor`.
fn key(r: &ChangeRecord) -> (i64, u32, u8, u64, u64) {
    let start = r.transaction_time_range.start();
    (
        start.wallclock(),
        start.logical(),
        r.kind.ord(),
        r.entity_id,
        r.version_id,
    )
}

// ── (a) CORE: N long-polls do not starve a small worker pool ──────────────────

/// Eight concurrent `recv_async` long-polls (60s) on a **2-worker** runtime must
/// NOT pin the pool: an unrelated quick task still completes promptly, and one
/// commit wakes all eight well within a second. RED if `recv_async` blocks its
/// worker (the old `block_in_place`/`recv_timeout` path) — two blocked workers
/// starve the probe and the whole test hangs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recv_async_does_not_starve_small_worker_pool() {
    let db = new_db();
    const N: usize = 8;

    let mut handles = Vec::new();
    for _ in 0..N {
        let sub = db
            .subscribe_changes(ChangeFilter::all())
            .expect("subscribe");
        handles.push(tokio::spawn(async move {
            sub.recv_async(Duration::from_secs(60)).await
        }));
    }

    // Probe: an unrelated quick task must run promptly despite the 8 outstanding
    // long-polls. If each long-poll pinned a worker, both workers would be blocked
    // and this would never be scheduled.
    let probe = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        42u32
    });
    let probed = tokio::time::timeout(Duration::from_millis(500), probe).await;
    assert!(
        probed.is_ok(),
        "probe task starved: concurrent long-polls pinned the worker pool"
    );
    assert_eq!(probed.unwrap().expect("probe join"), 42);

    // One commit wakes ALL eight long-polls; each resolves with the change.
    commit(&db, 1);
    let joined = tokio::time::timeout(Duration::from_secs(3), async {
        for h in handles {
            let recs = h.await.expect("join").expect("recv_async ok");
            assert_eq!(recs.len(), 1, "each long-poll receives the one commit");
        }
    })
    .await;
    assert!(
        joined.is_ok(),
        "not all long-polls resolved after the commit (worker starvation)"
    );
}

// ── (a reverse): event-driven, no busy-spin when idle ─────────────────────────

/// An idle `recv_async` waits the full timeout (no early spurious returns / no
/// interval polling), and a single commit yields exactly one delivery. RED for a
/// busy-poll design that returns early or spins.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recv_async_is_event_driven_no_busy_spin_when_idle() {
    let db = new_db();
    let sub = db
        .subscribe_changes(ChangeFilter::all())
        .expect("subscribe");

    let start = Instant::now();
    let empty = sub
        .recv_async(Duration::from_millis(300))
        .await
        .expect("recv ok");
    assert!(empty.is_empty(), "no data was committed");
    assert!(
        start.elapsed() >= Duration::from_millis(250),
        "recv_async returned early ({:?}) — not event-driven?",
        start.elapsed()
    );

    // One commit → exactly one prompt delivery (no missed / duplicate wake).
    commit(&db, 7);
    let got = tokio::time::timeout(
        Duration::from_secs(2),
        sub.recv_async(Duration::from_secs(2)),
    )
    .await
    .expect("did not hang")
    .expect("recv ok");
    assert_eq!(got.len(), 1, "single commit delivered exactly once");
}

// ── (c): event-driven delivery is ordered and lossless ────────────────────────

/// Draining via `recv_async` across interleaved commits yields records in
/// cursor-ascending order and the union equals the durable ground truth —
/// identical to the sync `recv_timeout` contract (AC c).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_driven_delivery_is_ordered_and_lossless() {
    let db = new_db();
    let sub = db
        .subscribe_changes(ChangeFilter::all())
        .expect("subscribe");

    const COMMITS: i64 = 25;
    let mut collected: Vec<ChangeRecord> = Vec::new();
    for i in 0..COMMITS {
        commit(&db, i);
        // Drain whatever is available with a short async wait.
        loop {
            let batch = sub
                .recv_async(Duration::from_millis(50))
                .await
                .expect("recv ok");
            if batch.is_empty() {
                break;
            }
            collected.extend(batch);
        }
    }
    // Flush any remainder.
    loop {
        let batch = sub
            .recv_async(Duration::from_millis(50))
            .await
            .expect("recv ok");
        if batch.is_empty() {
            break;
        }
        collected.extend(batch);
    }

    // Cursor-ascending order.
    let keys: Vec<_> = collected.iter().map(key).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "records delivered in cursor-ascending order");

    // Lossless: each commit is one node-create record, delivered exactly once.
    assert_eq!(db.changefeed_subscription_count(), 1, "sub still live");
    assert_eq!(
        collected.len() as i64,
        COMMITS,
        "every committed change delivered exactly once"
    );
}

// ── (b): SSE client disconnect releases the slot under the tight bound ─────────

/// An SSE `GET /changes/stream` client disconnect frees the per-principal slot in
/// well under 2s WITHOUT any commits to wake the worker — the async worker races
/// `tx.closed()` and drops its `Subscription` at once. RED under the old
/// `spawn_blocking` + `recv_timeout(15s)` idle poll (release lagged up to 15s and
/// required a commit to wake).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sse_disconnect_releases_slot_under_two_seconds() {
    let db = new_db();
    let store = keyed_store();
    let reader_id = principal_id(&store, READER_TOKEN);
    db.set_changefeed_config(
        ChangefeedConfig::default()
            .with_max_subscriptions(128)
            .with_max_subscriptions_per_principal(4),
    );
    let client = Arc::new(build_server_client(
        Arc::clone(&db),
        store,
        AuthMode::Required,
    ));

    let stream_task = {
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            let _ = client
                .get("/changes/stream")
                .header("authorization", &format!("Bearer {READER_TOKEN}"))
                .send()
                .await;
        })
    };

    let registered = wait_until(Duration::from_secs(5), || {
        principal_count(&db, &reader_id) == 1
    })
    .await;
    assert!(registered.is_some(), "SSE stream should register the slot");

    // Disconnect — NO commits to wake the worker (the whole point).
    stream_task.abort();

    let released = wait_until(Duration::from_secs(2), || {
        principal_count(&db, &reader_id) == 0
    })
    .await;
    assert!(
        released.is_some(),
        "SSE disconnect must free the slot in <2s with no commits (was up to 15s)"
    );

    // Genuinely free: the reader can re-subscribe.
    let _fresh = db
        .subscribe_changes_for_principal(Some(&reader_id), ChangeFilter::all())
        .expect("reader re-subscribes after prompt release");
}

// ── (b): HTTP await long-poll disconnect releases the slot promptly ───────────

/// A `POST /changes/await` long-poll (60s) client disconnect frees the
/// per-principal slot in well under 2s — the async handler future is dropped on
/// disconnect, dropping the `Subscription`. RED under the old `spawn_blocking`
/// path: the blocking worker is NOT cancelled on future-drop and held the slot
/// for the full 60s timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn await_disconnect_releases_slot_under_two_seconds() {
    let db = new_db();
    let store = keyed_store();
    let reader_id = principal_id(&store, READER_TOKEN);
    db.set_changefeed_config(
        ChangefeedConfig::default()
            .with_max_subscriptions(128)
            .with_max_subscriptions_per_principal(4),
    );
    let client = Arc::new(build_server_client(
        Arc::clone(&db),
        store,
        AuthMode::Required,
    ));

    let await_task = {
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            let _ = client
                .post("/changes/await")
                .header("authorization", &format!("Bearer {READER_TOKEN}"))
                .json(&json!({ "timeout_ms": 60000 }))
                .send()
                .await;
        })
    };

    let registered = wait_until(Duration::from_secs(5), || {
        principal_count(&db, &reader_id) == 1
    })
    .await;
    assert!(
        registered.is_some(),
        "await long-poll should register the slot"
    );

    await_task.abort();

    let released = wait_until(Duration::from_secs(2), || {
        principal_count(&db, &reader_id) == 0
    })
    .await;
    assert!(
        released.is_some(),
        "await disconnect must free the slot in <2s (was up to 60s)"
    );
}

// ── (b + #3688): reconnect churn never falsely exhausts the quota ─────────────

/// Rapid open→disconnect churn near the per-principal cap never falsely exhausts
/// the quota: each disconnect frees its slot promptly (event-driven) so the next
/// open always fits. RED if stale slots linger 15/60s and accumulate past the
/// cap (the #3688 transient false-exhaustion this issue eliminates).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_churn_no_false_exhaustion() {
    let db = new_db();
    let store = keyed_store();
    let reader_id = principal_id(&store, READER_TOKEN);
    const CAP: usize = 2;
    db.set_changefeed_config(
        ChangefeedConfig::default()
            .with_max_subscriptions(128)
            .with_max_subscriptions_per_principal(CAP),
    );
    let client = Arc::new(build_server_client(
        Arc::clone(&db),
        store,
        AuthMode::Required,
    ));

    for round in 0..10 {
        // The per-principal count must be strictly below CAP before each open,
        // else a prior disconnect leaked its slot (false-exhaustion source).
        assert!(
            principal_count(&db, &reader_id) < CAP,
            "round {round}: stale slots lingered past the cap (false-exhaustion)"
        );

        let task = {
            let client = Arc::clone(&client);
            tokio::spawn(async move {
                let _ = client
                    .post("/changes/await")
                    .header("authorization", &format!("Bearer {READER_TOKEN}"))
                    .json(&json!({ "timeout_ms": 60000 }))
                    .send()
                    .await;
            })
        };

        assert!(
            wait_until(Duration::from_secs(5), || principal_count(&db, &reader_id)
                >= 1)
            .await
            .is_some(),
            "round {round}: open should register"
        );
        task.abort();
        assert!(
            wait_until(Duration::from_secs(2), || principal_count(&db, &reader_id)
                == 0)
            .await
            .is_some(),
            "round {round}: disconnect should free the slot promptly"
        );
    }

    // Fully released; a fresh CAP subscribes succeed and the CAP+1 is rejected.
    let mut held = Vec::new();
    for _ in 0..CAP {
        held.push(
            db.subscribe_changes_for_principal(Some(&reader_id), ChangeFilter::all())
                .expect("fresh cap subscribes after churn"),
        );
    }
    assert!(
        db.subscribe_changes_for_principal(Some(&reader_id), ChangeFilter::all())
            .is_err(),
        "cap still binds after churn (no leak)"
    );
}

// ── (b normal path): await timeout still releases the slot ────────────────────

/// A `POST /changes/await` that times out normally (no data, no disconnect)
/// returns `timed_out: true` and frees the slot promptly — the async rewrite
/// preserves the normal timeout→Drop path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn await_timeout_still_releases_slot() {
    let db = new_db();
    let store = keyed_store();
    let reader_id = principal_id(&store, READER_TOKEN);
    db.set_changefeed_config(
        ChangefeedConfig::default()
            .with_max_subscriptions(128)
            .with_max_subscriptions_per_principal(4),
    );
    let client = build_server_client(Arc::clone(&db), store, AuthMode::Required);

    let resp = client
        .post("/changes/await")
        .header("authorization", &format!("Bearer {READER_TOKEN}"))
        .json(&json!({ "timeout_ms": 150 }))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = resp.json();
    assert_eq!(body["timed_out"], true, "clean timeout: {body}");

    let released = wait_until(Duration::from_secs(2), || {
        principal_count(&db, &reader_id) == 0
    })
    .await;
    assert!(
        released.is_some(),
        "a timed-out long-poll must free its slot promptly"
    );
}

// ── (c quota): disconnect decrements the principal counter exactly once ────────

/// A single subscribe→drop decrements the per-principal counter to exactly 0 and
/// removes the map entry; a re-subscribe then succeeds. Guards against a double
/// decrement/underflow — `Subscription::Drop` must remain the sole release
/// anchor even after the async rewrite.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_decrements_principal_counter_exactly_once() {
    let db = new_db();
    let sub = db
        .subscribe_changes_for_principal(Some("alice"), ChangeFilter::all())
        .expect("subscribe");
    assert_eq!(principal_count(&db, "alice"), 1);
    drop(sub);
    assert_eq!(principal_count(&db, "alice"), 0, "exactly one decrement");
    assert!(
        db.changefeed_principal_counts()
            .iter()
            .all(|(k, _)| k != "alice"),
        "the zeroed principal entry is pruned"
    );
    let _again = db
        .subscribe_changes_for_principal(Some("alice"), ChangeFilter::all())
        .expect("re-subscribe after release");
    assert_eq!(principal_count(&db, "alice"), 1);
}

// ── (b + #3688 stats): database_stats returns to baseline after disconnect ─────

/// After an SSE client disconnect, the admin `database_stats` changefeed
/// per-principal breakdown returns to baseline (the reader absent, aggregate 0)
/// within the tight bound — mirrors the per-principal live count and the #3688
/// stats surface.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quota_stats_return_to_baseline_after_disconnect() {
    let db = new_db();
    let store = keyed_store();
    let reader_id = principal_id(&store, READER_TOKEN);
    db.set_changefeed_config(
        ChangefeedConfig::default()
            .with_max_subscriptions(128)
            .with_max_subscriptions_per_principal(4),
    );
    let client = Arc::new(build_server_client(
        Arc::clone(&db),
        Arc::clone(&store),
        AuthMode::Required,
    ));

    let stream_task = {
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            let _ = client
                .get("/changes/stream")
                .header("authorization", &format!("Bearer {READER_TOKEN}"))
                .send()
                .await;
        })
    };
    assert!(
        wait_until(Duration::from_secs(5), || principal_count(&db, &reader_id)
            == 1)
        .await
        .is_some(),
        "reader slot registers"
    );

    stream_task.abort();

    // The per-principal live count returns to zero promptly (the stats source).
    let released = wait_until(Duration::from_secs(2), || {
        principal_count(&db, &reader_id) == 0
    })
    .await;
    assert!(
        released.is_some(),
        "stats source returns to baseline in <2s"
    );

    // And the admin database_stats roster no longer lists the reader.
    let resp = client
        .get("/database_stats")
        .header("authorization", &format!("Bearer {ADMIN_TOKEN}"))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body: Value = resp.json();
    let cf = body
        .get("changefeed")
        .or_else(|| body.get("data").and_then(|d| d.get("changefeed")))
        .expect("changefeed stats block");
    let lists_reader = cf["per_principal"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .any(|e| e["principal"].as_str() == Some(reader_id.as_str()))
        })
        .unwrap_or(false);
    assert!(
        !lists_reader,
        "database_stats must not list the disconnected reader: {cf}"
    );
}
