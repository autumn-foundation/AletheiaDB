#![no_main]

use std::io::Write;

use aletheiadb::storage::index_persistence::vector::load_vector_mappings;
use libfuzzer_sys::fuzz_target;
use tempfile::NamedTempFile;

fuzz_target!(|data: &[u8]| {
    let mut file = match NamedTempFile::new() {
        Ok(file) => file,
        Err(_) => return,
    };

    if file.write_all(data).is_err() {
        return;
    }

    let _ = load_vector_mappings(file.path());
});
