#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if s.len() > 100 { return; }

        let props = aletheiadb::core::PropertyMapBuilder::new()
            .insert("test", aletheiadb::core::PropertyValue::string(s))
            .build();
    }
});
