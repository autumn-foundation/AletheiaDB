use gallifreydb::{GallifreyDB, PropertyMapBuilder};
use gallifreydb::api::transaction::{ReadOps, WriteOps};
use std::sync::{Arc, Barrier};
use std::thread;

#[test]
fn test_phantom_delete_violation() {
    // 1. Setup: Create DB and a "Doctor" node
    let db = Arc::new(GallifreyDB::new());

    let node_id = {
        let mut tx = db.write_transaction().unwrap();
        let id = tx
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Doctor").build(),
            )
            .unwrap();
        tx.commit().unwrap();
        id
    }; // Node exists at TS=1

    // 2. Prepare the Trap
    let db_clone = Arc::clone(&db);
    let barrier = Arc::new(Barrier::new(2)); // Sync point for 2 threads
    let barrier_writer = barrier.clone();

    // ----------------------------------------------------------------
    // Thread 1: The Victim (Reader)
    // ----------------------------------------------------------------
    let reader_handle = thread::spawn(move || {
        // A. Start Transaction. Snapshot captured here
        let tx = db_clone.read_transaction().unwrap();
        println!("T1 (Victim): Started. TxId: {:?}", tx.tx_id());

        // B. Wait for the Writer to do the damage
        barrier.wait(); // Wait for Writer to start
        barrier.wait(); // Wait for Writer to COMMIT

        // C. Attempt to read the node
        // CRITICAL: The node still exists in reality at this snapshot.
        // But in CurrentStorage, it has been overwritten by T2.
        let result = tx.get_node(node_id);

        // The assertion that will fail if there's a phantom delete:
        assert!(
            result.is_ok(),
            "SCARY TRUTH: The node disappeared because a newer version exists! Error: {:?}",
            result.err()
        );

        let node = result.unwrap();
        let name_prop = node.get_property("name").expect("name property should exist");
        let name = name_prop.as_str().expect("name should be a string");

        assert_eq!(
            name, "Doctor",
            "CONSISTENCY FAIL: We read the new data ({}) instead of old (Doctor)!",
            name
        );
    });

    // ----------------------------------------------------------------
    // Thread 2: The Aggressor (Writer)
    // ----------------------------------------------------------------
    {
        barrier_writer.wait(); // Sync start

        // D. Update the node
        let mut tx = db.write_transaction().unwrap();
        // Update "Doctor" -> "Master"
        tx.update_node(
            node_id,
            PropertyMapBuilder::new().insert("name", "Master").build(),
        )
        .unwrap();
        tx.commit().unwrap();

        println!("T2 (Aggressor): Overwrote data");
        barrier_writer.wait(); // Signal T1 to resume
    }

    reader_handle.join().unwrap();
}
