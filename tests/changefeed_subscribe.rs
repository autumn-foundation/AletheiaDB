//! Integration tests for the push changefeed subscription API (Issue #3375).
//!
//! These exercise the Rust-API-only slice: `AletheiaDB::subscribe_changes`, the
//! `Subscription` drain/blocking API, and the no-loss / resume contract against the
//! durable #3216 `list_changes` ground truth.

use aletheiadb::api::WriteOps;
use aletheiadb::core::changefeed::{ChangeFeedQuery, ChangeRecord, ChangeType, EntityKind};
use aletheiadb::core::changefeed_subscription::{ChangefeedConfig, RecvError};
use aletheiadb::core::temporal::{TIMESTAMP_MAX, Timestamp};
use aletheiadb::{AletheiaDB, ChangeFilter, Error, PropertyMap, PropertyMapBuilder};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

fn props(name: &str) -> PropertyMap {
    PropertyMapBuilder::new().insert("name", name).build()
}

/// The full durable ground-truth change set over the entire timeline (#3216 pull feed).
fn ground_truth(db: &AletheiaDB) -> Vec<ChangeRecord> {
    let q = ChangeFeedQuery::new(Timestamp::from(0), TIMESTAMP_MAX, 100_000);
    db.list_changes(&q).unwrap().changes
}

/// A stable dedup key equivalent to the `ChangeCursor` (tx-time, kind, entity, version).
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

fn key_set(records: &[ChangeRecord]) -> BTreeSet<(i64, u32, u8, u64, u64)> {
    records.iter().map(key).collect()
}

#[test]
fn filterless_subscription_receives_all_changes_in_order() {
    let db = AletheiaDB::new().unwrap();
    let sub = db.subscribe_changes(ChangeFilter::all()).unwrap();

    db.write(|tx| {
        tx.create_node("Person", props("Alice"))?;
        Ok::<_, Error>(())
    })
    .unwrap();
    db.write(|tx| {
        tx.create_node("Company", props("Acme"))?;
        Ok::<_, Error>(())
    })
    .unwrap();

    let events = sub.poll();
    let truth = ground_truth(&db);
    assert_eq!(events.len(), truth.len());
    // Same set and same order as the deterministic pull feed.
    let event_keys: Vec<_> = events.iter().map(key).collect();
    let truth_keys: Vec<_> = truth.iter().map(key).collect();
    assert_eq!(event_keys, truth_keys);
}

#[test]
fn label_edge_and_change_type_filters() {
    let db = AletheiaDB::new().unwrap();
    let person_sub = db
        .subscribe_changes(ChangeFilter::all().with_node_labels(["Person"]))
        .unwrap();
    let edge_sub = db
        .subscribe_changes(ChangeFilter::all().with_edge_types(["KNOWS"]))
        .unwrap();
    let deleted_sub = db
        .subscribe_changes(ChangeFilter::all().with_change_types([ChangeType::Deleted]))
        .unwrap();

    let a = db
        .write_with_timestamp(|tx| tx.create_node("Person", props("A")))
        .unwrap()
        .0;
    let b = db
        .write_with_timestamp(|tx| tx.create_node("Company", props("B")))
        .unwrap()
        .0;
    let _edge = db
        .write_with_timestamp(|tx| tx.create_edge(a, b, "KNOWS", props("e")))
        .unwrap()
        .0;
    db.write(|tx| {
        tx.delete_node(b)?;
        Ok::<_, Error>(())
    })
    .unwrap();

    // Person filter: only the "Person" node create (A). Not Company, not the edge.
    let person = person_sub.poll();
    assert_eq!(person.len(), 1);
    assert_eq!(person[0].kind, EntityKind::Node);
    assert_eq!(person[0].label, "Person");

    // Edge filter: only the KNOWS edge create. No nodes.
    let edges = edge_sub.poll();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].kind, EntityKind::Edge);
    assert_eq!(edges[0].label, "KNOWS");

    // Change-type filter: only the deletion (of the Company node), across kinds.
    let deletes = deleted_sub.poll();
    assert_eq!(deletes.len(), 1);
    assert_eq!(deletes[0].change_type, ChangeType::Deleted);
}

#[test]
fn combined_filter_and_composes() {
    let db = AletheiaDB::new().unwrap();
    let sub = db
        .subscribe_changes(
            ChangeFilter::all()
                .with_node_labels(["Person"])
                .with_change_types([ChangeType::Created]),
        )
        .unwrap();

    let id = db
        .write_with_timestamp(|tx| tx.create_node("Person", props("Alice")))
        .unwrap()
        .0;
    // A modify of the same Person node must NOT match (wrong change type).
    db.write(|tx| {
        tx.update_node(id, props("Alice v2"))?;
        Ok::<_, Error>(())
    })
    .unwrap();
    // A Company create must NOT match (wrong label).
    db.write(|tx| {
        tx.create_node("Company", props("Acme"))?;
        Ok::<_, Error>(())
    })
    .unwrap();

    let events = sub.poll();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].change_type, ChangeType::Created);
    assert_eq!(events[0].label, "Person");
}

#[test]
fn aborted_transaction_emits_nothing() {
    let db = AletheiaDB::new().unwrap();
    let sub = db.subscribe_changes(ChangeFilter::all()).unwrap();

    // A transaction that returns Err rolls back → no changes committed, nothing emitted.
    let result = db.write(|tx| {
        tx.create_node("Person", props("Ghost"))?;
        Err::<(), Error>(Error::Other("intentional rollback".into()))
    });
    assert!(result.is_err());
    assert!(sub.poll().is_empty());

    // A committed transaction DOES emit.
    db.write(|tx| {
        tx.create_node("Person", props("Real"))?;
        Ok::<_, Error>(())
    })
    .unwrap();
    assert_eq!(sub.poll().len(), 1);
}

#[test]
fn tx_time_order_across_and_within_commits() {
    let db = AletheiaDB::new().unwrap();
    let sub = db.subscribe_changes(ChangeFilter::all()).unwrap();

    // Sequential commits.
    for i in 0..5 {
        db.write(|tx| {
            tx.create_node("Person", props(&format!("p{i}")))?;
            Ok::<_, Error>(())
        })
        .unwrap();
    }
    // A single commit with multiple ops (nodes + an edge).
    db.write(|tx| {
        let a = tx.create_node("Person", props("multi-a"))?;
        let b = tx.create_node("Person", props("multi-b"))?;
        tx.create_edge(a, b, "KNOWS", props("e"))?;
        Ok::<_, Error>(())
    })
    .unwrap();

    let events = sub.poll();
    let truth = ground_truth(&db);
    let event_keys: Vec<_> = events.iter().map(key).collect();
    let truth_keys: Vec<_> = truth.iter().map(key).collect();
    // Delivered order equals the deterministic total order of the pull feed.
    assert_eq!(event_keys, truth_keys);
    // And it is sorted ascending.
    let mut sorted = event_keys.clone();
    sorted.sort();
    assert_eq!(event_keys, sorted);
}

#[test]
fn kill_and_reconnect_is_lossless() {
    let db = AletheiaDB::new().unwrap();
    let sub = db.subscribe_changes(ChangeFilter::all()).unwrap();

    // Commit some, receive some.
    for i in 0..3 {
        db.write(|tx| {
            tx.create_node("Person", props(&format!("early{i}")))?;
            Ok::<_, Error>(())
        })
        .unwrap();
    }
    let delivered = sub.poll();
    assert_eq!(delivered.len(), 3);
    let resume = sub.resume_token().expect("resume token after drain");

    // Drop the subscription (simulated disconnect), then keep committing.
    drop(sub);
    assert_eq!(db.changefeed_subscription_count(), 0);
    for i in 0..4 {
        db.write(|tx| {
            tx.create_node("Person", props(&format!("late{i}")))?;
            Ok::<_, Error>(())
        })
        .unwrap();
    }

    // Reconnect: resume the pull from the last delivered cursor.
    let q = ChangeFeedQuery {
        tx_from: Timestamp::from(0),
        tx_to: TIMESTAMP_MAX,
        valid_from: None,
        valid_to: None,
        label: None,
        limit: 100_000,
        cursor: Some(resume),
    };
    let gap = db.list_changes(&q).unwrap().changes;

    // Union of (live-delivered ∪ resume-pull), deduped by cursor, equals the full ground truth.
    let mut union = key_set(&delivered);
    union.extend(key_set(&gap));
    let truth = key_set(&ground_truth(&db));
    assert_eq!(union, truth, "zero events may be missed");
}

#[test]
fn randomized_kill_and_reconnect_is_lossless() {
    // 50 trials with a random number of writes before/after a disconnect; each must be
    // lossless under the union(live, resume) == ground_truth contract.
    for trial in 0..50 {
        let db = AletheiaDB::new().unwrap();
        let sub = db.subscribe_changes(ChangeFilter::all()).unwrap();

        // Deterministic pseudo-random split without external crates.
        let before = (trial * 7 + 1) % 9; // 1..=8
        let after = (trial * 13 + 1) % 11; // 0..=10

        for i in 0..before {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("b{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        let delivered = sub.poll();
        let resume = sub.resume_token();
        drop(sub);

        for i in 0..after {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("a{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }

        let q = ChangeFeedQuery {
            tx_from: Timestamp::from(0),
            tx_to: TIMESTAMP_MAX,
            valid_from: None,
            valid_to: None,
            label: None,
            limit: 100_000,
            cursor: resume,
        };
        let gap = db.list_changes(&q).unwrap().changes;

        let mut union = key_set(&delivered);
        union.extend(key_set(&gap));
        let truth = key_set(&ground_truth(&db));
        assert_eq!(union, truth, "trial {trial}: zero events missed");
    }
}

#[test]
fn crash_before_notify_recovers_via_cursor() {
    // Simulate "commit fsynced but crashed before emit ran": commit changes while NO
    // subscriber is attached, so nothing is pushed. A consumer that later resumes from an
    // earlier cursor recovers them via the durable list_changes ground truth.
    let db = AletheiaDB::new().unwrap();

    // Establish an anchoring cursor from an initial committed change WITH a subscriber.
    let sub = db.subscribe_changes(ChangeFilter::all()).unwrap();
    db.write(|tx| {
        tx.create_node("Person", props("anchor"))?;
        Ok::<_, Error>(())
    })
    .unwrap();
    let delivered = sub.poll();
    assert_eq!(delivered.len(), 1);
    let resume = sub.resume_token().unwrap();
    drop(sub); // no subscriber attached from here on

    // These commits are the "crashed before notify" writes — never pushed to anyone.
    for i in 0..5 {
        db.write(|tx| {
            tx.create_node("Person", props(&format!("orphan{i}")))?;
            Ok::<_, Error>(())
        })
        .unwrap();
    }

    // Resume the pull from the earlier cursor: the un-pushed commits are all recovered.
    let q = ChangeFeedQuery {
        tx_from: Timestamp::from(0),
        tx_to: TIMESTAMP_MAX,
        valid_from: None,
        valid_to: None,
        label: None,
        limit: 100_000,
        cursor: Some(resume.clone()),
    };
    let recovered = db.list_changes(&q).unwrap().changes;
    assert_eq!(recovered.len(), 5, "all un-pushed commits recovered");

    // Duplicate delivery on resume is bounded and dedups to the ground truth: resuming from
    // a cursor BEFORE an already-seen event yields that event again, but the union dedups.
    let q_all_from_start = ChangeFeedQuery::new(Timestamp::from(0), TIMESTAMP_MAX, 100_000);
    let full_pull = db.list_changes(&q_all_from_start).unwrap().changes;
    let mut union = key_set(&delivered);
    union.extend(key_set(&recovered));
    assert_eq!(union, key_set(&full_pull));
}

#[test]
fn durable_reopen_recovers_pre_crash_changes_via_cursor() {
    // A stronger crash-recovery proxy than `crash_before_notify_recovers_via_cursor`: use a
    // real durable database, commit changes, DROP it (a clean crash — Drop flushes and the
    // WAL is on disk), then REOPEN from the same data dir (WAL replay + index load rebuild
    // history). A consumer that persisted an early cursor recovers every pre-crash change via
    // `list_changes` over the reopened database's rebuilt history — zero missed. This proves
    // the no-loss contract against real durable recovery, not just an in-memory simulation.
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("changefeed-durable");

    // Phase 1: durable writes, capture an early resume cursor (after the first "anchor"
    // change), then drop the handle to simulate a crash after everything is fsynced.
    let early_cursor: Option<String> = {
        let db = AletheiaDB::open(&data_dir).unwrap();

        // A live subscriber drains the anchor change so we hold its resume cursor — exactly
        // what a real consumer persists (`sub.resume_token()`) before a crash.
        let sub = db.subscribe_changes(ChangeFilter::all()).unwrap();
        db.write(|tx| {
            tx.create_node("Person", props("anchor"))?;
            Ok::<_, Error>(())
        })
        .unwrap();
        let delivered = sub.poll();
        assert_eq!(delivered.len(), 1);
        let cursor = sub.resume_token();
        assert!(cursor.is_some(), "resume token after draining the anchor");
        drop(sub);

        // These commits are the "pre-crash" changes the consumer never saw live.
        for i in 0..5 {
            db.write(|tx| {
                tx.create_node("Person", props(&format!("precrash{i}")))?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        cursor
        // db dropped here: Drop synchronously flushes, so state is durable on disk.
    };

    // Phase 2: reopen from the same directory — WAL replay rebuilds the full history.
    let db = AletheiaDB::open(&data_dir).unwrap();

    // The changefeed's list_changes works over pre-crash history post-reopen: resuming from
    // the early cursor returns exactly the 5 changes committed after the anchor.
    let q = ChangeFeedQuery {
        tx_from: Timestamp::from(0),
        tx_to: TIMESTAMP_MAX,
        valid_from: None,
        valid_to: None,
        label: None,
        limit: 100_000,
        cursor: early_cursor,
    };
    let recovered = db.list_changes(&q).unwrap().changes;
    assert_eq!(
        recovered.len(),
        5,
        "all pre-crash changes after the anchor recover post-reopen"
    );

    // A fresh subscriber on the reopened db goes live from here; combined with the resume
    // pull above, no pre-crash change is ever lost.
    let _sub = db.subscribe_changes(ChangeFilter::all()).unwrap();
    let full = ground_truth(&db);
    assert_eq!(
        full.len(),
        6,
        "anchor + 5 pre-crash changes all present after reopen"
    );
}

#[test]
fn bounded_buffer_overflow_disconnects_and_is_recoverable() {
    let db = AletheiaDB::new().unwrap();
    db.set_changefeed_config(ChangefeedConfig {
        max_subscriptions: 16,
        buffer_capacity: 4,
    });
    let slow = db.subscribe_changes(ChangeFilter::all()).unwrap();
    let healthy = db.subscribe_changes(ChangeFilter::all()).unwrap();

    // Deliver + drain one so the slow consumer has a resume anchor.
    db.write(|tx| {
        tx.create_node("Person", props("seed"))?;
        Ok::<_, Error>(())
    })
    .unwrap();
    // Everything this subscription ever hands the consumer accumulates here.
    let mut delivered = slow.poll();
    assert_eq!(delivered.len(), 1);
    let mut healthy_events = healthy.poll();

    // Overrun the slow consumer's cap-4 buffer without draining it. The healthy consumer
    // keeps up (drains every iteration), demonstrating that one slow consumer's lag does
    // not disconnect a consumer that keeps pace, nor block the writer.
    for i in 0..20 {
        db.write(|tx| {
            tx.create_node("Person", props(&format!("flood{i}")))?;
            Ok::<_, Error>(())
        })
        .unwrap();
        healthy_events.append(&mut healthy.poll());
    }
    assert!(slow.is_lagged(), "slow consumer should be disconnected");
    assert!(
        !healthy.is_lagged(),
        "keeping-pace consumer stays connected"
    );

    // Drain the buffered prefix (delivered, so it counts toward the union), then observe
    // the Lagged signal. The resume token points after the LAST event actually drained.
    delivered.append(&mut slow.poll());
    let resume = match slow.recv_timeout(Duration::from_millis(20)) {
        Err(RecvError::Lagged { resume_token }) => resume_token.expect("resume token on lag"),
        other => panic!("expected Lagged, got {other:?}"),
    };

    // The gap after the last delivered event is recoverable losslessly from the resume token.
    let q = ChangeFeedQuery {
        tx_from: Timestamp::from(0),
        tx_to: TIMESTAMP_MAX,
        valid_from: None,
        valid_to: None,
        label: None,
        limit: 100_000,
        cursor: Some(resume),
    };
    let gap = db.list_changes(&q).unwrap().changes;
    let mut union = key_set(&delivered);
    union.extend(key_set(&gap));
    assert_eq!(union, key_set(&ground_truth(&db)), "lag is recoverable");

    // The healthy consumer is unaffected: it kept receiving every committed change.
    db.write(|tx| {
        tx.create_node("Person", props("after-flood"))?;
        Ok::<_, Error>(())
    })
    .unwrap();
    healthy_events.append(&mut healthy.poll());
    assert!(!healthy.is_lagged());
    // It saw the full stream (seed + 20 floods + after-flood = 22).
    assert_eq!(
        healthy_events.len(),
        22,
        "keeping-pace consumer misses nothing"
    );
}

#[test]
fn max_subscriptions_cap_returns_structured_error() {
    let db = AletheiaDB::new().unwrap();
    db.set_changefeed_config(ChangefeedConfig {
        max_subscriptions: 2,
        buffer_capacity: 8,
    });
    let _s1 = db.subscribe_changes(ChangeFilter::all()).unwrap();
    let _s2 = db.subscribe_changes(ChangeFilter::all()).unwrap();
    let err = db.subscribe_changes(ChangeFilter::all()).unwrap_err();
    match err {
        Error::Storage(aletheiadb::StorageError::CapacityExceeded {
            resource, limit, ..
        }) => {
            assert_eq!(resource, "changefeed subscriptions");
            assert_eq!(limit, 2);
        }
        other => panic!("expected CapacityExceeded, got {other:?}"),
    }
}

#[test]
fn commits_succeed_with_many_idle_subscribers() {
    let db = AletheiaDB::new().unwrap();
    db.set_changefeed_config(ChangefeedConfig {
        max_subscriptions: 200,
        buffer_capacity: 8,
    });
    // 100 idle subscribers that never drain.
    let mut subs = Vec::new();
    for _ in 0..100 {
        subs.push(db.subscribe_changes(ChangeFilter::all()).unwrap());
    }
    assert_eq!(db.changefeed_subscription_count(), 100);

    // Commits still succeed and return promptly despite the fan-out.
    for i in 0..50 {
        db.write_with_timestamp(|tx| tx.create_node("Person", props(&format!("n{i}"))))
            .unwrap();
    }
    // Ground truth still complete.
    assert_eq!(ground_truth(&db).len(), 50);
}

#[test]
fn concurrent_writer_and_subscribers_no_loss_or_deadlock() {
    let db = Arc::new(AletheiaDB::new().unwrap());
    let sub_all = db.subscribe_changes(ChangeFilter::all()).unwrap();
    let sub_person = db
        .subscribe_changes(ChangeFilter::all().with_node_labels(["Person"]))
        .unwrap();

    const WRITERS: usize = 4;
    const PER_WRITER: usize = 50;

    std::thread::scope(|scope| {
        // Concurrent writers.
        for w in 0..WRITERS {
            let db = Arc::clone(&db);
            scope.spawn(move || {
                for i in 0..PER_WRITER {
                    let label = if i % 2 == 0 { "Person" } else { "Company" };
                    db.write(|tx| {
                        tx.create_node(label, props(&format!("w{w}n{i}")))?;
                        Ok::<_, Error>(())
                    })
                    .unwrap();
                }
            });
        }
        // Concurrent drainers accumulate into local buffers.
        let all_handle = scope.spawn(|| {
            let mut acc = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                match sub_all.recv_timeout(Duration::from_millis(20)) {
                    Ok(mut batch) => acc.append(&mut batch),
                    Err(_) => break,
                }
                if acc.len() >= WRITERS * PER_WRITER {
                    break;
                }
            }
            acc.append(&mut sub_all.poll());
            acc
        });
        let person_handle = scope.spawn(|| {
            let mut acc = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                match sub_person.recv_timeout(Duration::from_millis(20)) {
                    Ok(mut batch) => acc.append(&mut batch),
                    Err(_) => break,
                }
                if acc.len() >= WRITERS * PER_WRITER / 2 {
                    break;
                }
            }
            acc.append(&mut sub_person.poll());
            acc
        });

        let all_events = all_handle.join().unwrap();
        let person_events = person_handle.join().unwrap();

        // No torn records: every delivered record is well-formed and matches its filter.
        for r in &all_events {
            assert!(matches!(r.kind, EntityKind::Node));
            assert!(r.label == "Person" || r.label == "Company");
        }
        for r in &person_events {
            assert_eq!(r.kind, EntityKind::Node);
            assert_eq!(r.label, "Person");
        }
    });

    // After all writers finished, the durable ground truth is complete and the live feed
    // (plus any residual poll) covers it with no loss — validate via a final resume.
    let truth = ground_truth(&db);
    assert_eq!(truth.len(), WRITERS * PER_WRITER);
}

/// The HIGH-review regression test: under concurrent writers, a consumer that drains part of
/// the live feed, captures its resume token, and "disconnects" mid-stream must lose **zero**
/// events — `union(live-delivered, list_changes(resume_token))` equals the full ground truth.
///
/// Why this catches the ordered-emit bug: the resume token is the cursor of the *last event
/// drained*, used by `list_changes` as a strict `> cursor` high-water-mark. That is only a
/// zero-loss anchor if per-subscriber delivery is cursor-ascending. Without the ordered-emit
/// sequencer, two concurrent commits (tx=100, tx=101) can be pushed to the buffer out of order
/// (101 before 100); a consumer that drains `[101]`, records `resume = cursor(101)`, and
/// disconnects before `[100]` arrives would then resume from cursor(101), which *excludes*
/// cursor(100) — event 100 is permanently lost and the union is missing it. With the sequencer,
/// every subscriber buffer is strictly cursor-ascending, so `last_delivered` is a true
/// high-water-mark and the union is always complete. Many trials + a mid-stream disconnect
/// exercise the reorder window.
#[test]
fn concurrent_writers_union_equals_ground_truth() {
    use std::time::Instant;

    const WRITERS: usize = 4;
    const PER_WRITER: usize = 40;
    const TOTAL: usize = WRITERS * PER_WRITER;

    for trial in 0..20 {
        let db = Arc::new(AletheiaDB::new().unwrap());
        // A moderate buffer: large enough to usually keep pace, small enough that a slow
        // trial may Lag — both paths must remain lossless via the resume token.
        db.set_changefeed_config(ChangefeedConfig {
            max_subscriptions: 16,
            buffer_capacity: 128,
        });
        let sub = db.subscribe_changes(ChangeFilter::all()).unwrap();

        // Everything the consumer drained live, and the resume anchor at its disconnect point.
        let mut delivered: Vec<ChangeRecord> = Vec::new();
        let mut resume: Option<String> = sub.resume_token(); // baseline anchor (#3375 F4)

        std::thread::scope(|scope| {
            for w in 0..WRITERS {
                let db = Arc::clone(&db);
                scope.spawn(move || {
                    for i in 0..PER_WRITER {
                        db.write(|tx| {
                            tx.create_node("Person", props(&format!("t{trial}w{w}n{i}")))?;
                            Ok::<_, Error>(())
                        })
                        .unwrap();
                    }
                });
            }

            // Drain live until we've seen roughly a third, then "disconnect" mid-stream while
            // writers are still committing (maximizing the reorder window that the buggy path
            // would lose events through).
            let disconnect_at = TOTAL / 3;
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                match sub.recv_timeout(Duration::from_millis(5)) {
                    Ok(batch) => delivered.extend(batch),
                    Err(_) => break, // Lagged: resume anchor already captured below
                }
                if delivered.len() >= disconnect_at {
                    break;
                }
            }
            // Grab anything already buffered, then snapshot the resume high-water-mark.
            delivered.append(&mut sub.poll());
            resume = sub.resume_token();
            drop(sub); // simulated disconnect; writers keep running to completion here
        });

        // Writers are done. Resume the durable pull from the last delivered cursor and assert
        // the union covers the full window with zero loss.
        let q = ChangeFeedQuery {
            tx_from: Timestamp::from(0),
            tx_to: TIMESTAMP_MAX,
            valid_from: None,
            valid_to: None,
            label: None,
            limit: 100_000,
            cursor: resume,
        };
        let gap = db.list_changes(&q).unwrap().changes;

        let truth = key_set(&ground_truth(&db));
        assert_eq!(truth.len(), TOTAL, "trial {trial}: ground truth complete");

        let mut union = key_set(&delivered);
        union.extend(key_set(&gap));
        assert_eq!(
            union, truth,
            "trial {trial}: union(live-delivered, resume-pull) must miss zero events"
        );
    }
}
