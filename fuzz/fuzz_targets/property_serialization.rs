#![no_main]

mod support;

use aletheiadb::PropertyMap;
use aletheiadb::PropertyValue;
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use support::{FuzzPropertyEntry, build_property_map};

#[derive(Debug, Arbitrary)]
struct PropertySerializationCase {
    raw: Vec<u8>,
    entries: Vec<FuzzPropertyEntry>,
}

fuzz_target!(|case: PropertySerializationCase| {
    let _ = PropertyMap::deserialize(&case.raw);
    let _ = PropertyValue::deserialize(&case.raw);

    let map = build_property_map(&case.entries);
    let serialized = map.serialize().expect("fuzz property map must serialize");
    let (decoded, consumed) =
        PropertyMap::deserialize(&serialized).expect("serialized property map must decode");

    assert_eq!(consumed, serialized.len());
    assert_eq!(decoded.len(), map.len());

    let reserialized = decoded
        .serialize()
        .expect("decoded fuzz property map must serialize");
    let (_, reserialized_consumed) =
        PropertyMap::deserialize(&reserialized).expect("reserialized map must decode");
    assert_eq!(reserialized_consumed, reserialized.len());
});
