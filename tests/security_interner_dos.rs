use std::env;
use std::process::Command;

#[test]
fn test_property_map_silent_loss_when_interner_full() {
    // Check if we are the subprocess
    if env::var("WARDEN_SUBPROCESS").is_ok() {
        run_subprocess_test();
        return;
    }

    let exe = env::current_exe().expect("failed to get current exe");
    let output = Command::new(exe)
        .arg("test_property_map_silent_loss_when_interner_full") // Run specific test
        .arg("--exact")
        .arg("--nocapture")
        .env("WARDEN_SUBPROCESS", "1")
        .env("GALLIFREYDB_MAX_INTERNED_STRINGS", "5") // Limit < COMMON_STRINGS.len() (10)
        .output()
        .expect("failed to spawn subprocess");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        println!("Subprocess stdout:\n{}", stdout);
        println!("Subprocess stderr:\n{}", stderr);

        if stdout.contains("VULNERABILITY CONFIRMED") {
            // We reproduced it!
            // To make the test "pass" in the sense of "we confirmed the bug exists",
            // we could assert(true). But normally we want tests to pass when code is correct.
            // So initially this test will FAIL.
            panic!("Vulnerability reproduced: Property silently dropped!");
        }
        panic!("Subprocess failed unexpectedly");
    }

    // If we reach here, subprocess succeeded (meaning it got an Error or Key was present)
    println!("Subprocess passed. Vulnerability fixed or not reproducible.");
}

fn run_subprocess_test() {
    use aletheiadb::core::GLOBAL_INTERNER;
    use aletheiadb::core::property::PropertyMapBuilder;

    println!("Interner len: {}", GLOBAL_INTERNER.len());

    // Try to insert a new property key
    // Since limit is 5 and we have 10 common strings pre-interned, next_id is 10.
    // 10 >= 5, so intern() should fail.
    let result = PropertyMapBuilder::new().try_insert("new_key_1", 1);

    match result {
        Ok(builder) => {
            let map = builder.build();
            if !map.contains_key("new_key_1") {
                println!("VULNERABILITY CONFIRMED: Property silently dropped!");
                // Panic to signal failure to parent
                panic!("VULNERABILITY CONFIRMED");
            } else {
                // If we reach here, it means the key WAS inserted, which implies the
                // interner limit was NOT enforced. This is a test setup failure.
                panic!(
                    "TEST SETUP FAILURE: Key was inserted successfully. Interner limit was not enforced?"
                );
            }
        }
        Err(e) => {
            println!("Got expected error: {}", e);
            // This is the desired behavior!
        }
    }
}
