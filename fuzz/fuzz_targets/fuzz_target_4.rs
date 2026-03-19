#![no_main]
use libfuzzer_sys::fuzz_target;
use aletheiadb::core::property::PropertyValue;

fuzz_target!(|data: &[u8]| {
    if let Ok((_pv, _len)) = PropertyValue::deserialize(data) {
        // successfully parsed a PropertyValue
    }
});
