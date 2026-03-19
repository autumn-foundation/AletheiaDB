#![no_main]
use libfuzzer_sys::fuzz_target;
use aletheiadb::db::AletheiaDB;

fuzz_target!(|data: &[u8]| {
    // testing generic multithreading
});
