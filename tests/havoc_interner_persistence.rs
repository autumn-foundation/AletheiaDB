use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::storage::current::CurrentStorage;
use aletheiadb::storage::historical::HistoricalStorage;
use aletheiadb::storage::persistence::Checkpoint;
use aletheiadb::storage::wal::LSN;
use std::fs;
use tempfile::tempdir;
use rand::Rng;

#[test]
fn test_havoc_interner_persistence_fix() {
    let dir = tempdir().unwrap();
    let checkpoint_path = dir.path().join("checkpoint_fix.dat");

    // 1. Create unique string that definitely isn't pre-warmed
    let random_id: u64 = rand::thread_rng().r#gen();
    let unique_string = format!("HavocUniqueString_{}", random_id);

    // 2. Intern it
    let id = GLOBAL_INTERNER.intern(&unique_string).expect("Failed to intern");
    println!("Interned '{}' as ID {}", unique_string, id.as_u32());

    // 3. Create Checkpoint
    let current = CurrentStorage::new();
    let historical = HistoricalStorage::new();
    let checkpoint = Checkpoint::new(LSN(100), &current, &historical);

    checkpoint.save(&checkpoint_path).expect("Failed to save checkpoint");

    // 4. Inspect Checkpoint File
    // We expect the string "HavocUniqueString_..." to be present in the file now.
    let data = fs::read(&checkpoint_path).expect("Failed to read checkpoint file");

    // Convert to string for searching (might contain binary, so lossy)
    let content = String::from_utf8_lossy(&data);

    if content.contains(&unique_string) {
        println!("SUCCESS: The string '{}' WAS FOUND in the checkpoint.", unique_string);
    } else {
        panic!("FAILURE: The string '{}' is still MISSING from the checkpoint.", unique_string);
    }
}
