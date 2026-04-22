use aletheiadb::encryption::rotation::{KeyRotationManager, RotationState};
use std::sync::Arc;
use std::thread;

#[test]
fn test_concurrent_rotations_loom() {
    let mgr = Arc::new(KeyRotationManager::new());

    let mgr1 = mgr.clone();
    let t1 = thread::spawn(move || {
        let res = mgr1.begin_rotation();
        if res.is_ok() {
            mgr1.complete_rotation().unwrap();
        }
    });

    let mgr2 = mgr.clone();
    let t2 = thread::spawn(move || {
        let res = mgr2.begin_rotation();
        if res.is_ok() {
            mgr2.cancel_rotation().unwrap();
        }
    });

    t1.join().unwrap();
    t2.join().unwrap();

    assert_eq!(mgr.state(), RotationState::Idle);
}
