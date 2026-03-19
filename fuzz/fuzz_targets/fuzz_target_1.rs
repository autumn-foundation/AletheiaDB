#![no_main]
use libfuzzer_sys::fuzz_target;
use aletheiadb::sql::parse_sql;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = parse_sql(s);
    }
});
