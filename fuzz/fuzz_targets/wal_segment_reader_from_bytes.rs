#![no_main]

use std::io::Write;

use aletheiadb::storage::wal::segment_reader::read_segment;
use aletheiadb::storage::wal::LSN;
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

    let _ = read_segment(file.path(), LSN(1));
});
