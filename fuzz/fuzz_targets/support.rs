#![allow(dead_code)]

use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::{GLOBAL_INTERNER, InternedString, PropertyMap, PropertyMapBuilder, PropertyValue};
use arbitrary::Arbitrary;

pub const MAX_FUZZ_OPS: usize = 64;
pub const MAX_FUZZ_PROPS: usize = 16;
pub const MAX_FUZZ_BYTES: usize = 64;
pub const MAX_FUZZ_VECTOR_DIMS: usize = 16;

#[derive(Debug, Clone, Arbitrary)]
pub enum FuzzScalar {
    Null,
    Bool(bool),
    Int(i64),
    Float(u32),
    Text(Vec<u8>),
    Bytes(Vec<u8>),
    Vector(Vec<u32>),
}

#[derive(Debug, Clone, Arbitrary)]
pub struct FuzzPropertyEntry {
    pub key: u8,
    pub value: FuzzScalar,
}

pub fn timestamp_from_step(step: u64) -> HybridTimestamp {
    HybridTimestamp::new(1_000_000 + step as i64, 0).expect("bounded fuzz timestamp must be valid")
}

pub fn intern_label(label: &'static str) -> InternedString {
    GLOBAL_INTERNER
        .intern(label)
        .expect("static fuzz label must intern")
}

pub fn int_properties(value: i64) -> PropertyMap {
    PropertyMapBuilder::new().insert("value", value).build()
}

pub fn build_property_map(entries: &[FuzzPropertyEntry]) -> PropertyMap {
    let mut builder = PropertyMapBuilder::new();

    for (idx, entry) in entries.iter().take(MAX_FUZZ_PROPS).enumerate() {
        let key = format!("k{idx}_{}", entry.key % 16);
        builder = builder.insert(&key, entry.value.to_property_value());
    }

    builder.build()
}

impl FuzzScalar {
    pub fn to_property_value(&self) -> PropertyValue {
        match self {
            Self::Null => PropertyValue::Null,
            Self::Bool(value) => PropertyValue::Bool(*value),
            Self::Int(value) => PropertyValue::Int(*value),
            Self::Float(bits) => PropertyValue::Float(f64::from(*bits) / 1024.0),
            Self::Text(bytes) => PropertyValue::string(ascii_string(bytes)),
            Self::Bytes(bytes) => PropertyValue::bytes(
                bytes
                    .iter()
                    .copied()
                    .take(MAX_FUZZ_BYTES)
                    .collect::<Vec<_>>(),
            ),
            Self::Vector(bits) => {
                let values = bits
                    .iter()
                    .copied()
                    .take(MAX_FUZZ_VECTOR_DIMS)
                    .map(|value| value as f32 / 1024.0)
                    .collect::<Vec<_>>();
                PropertyValue::vector(values)
            }
        }
    }
}

fn ascii_string(bytes: &[u8]) -> String {
    bytes
        .iter()
        .copied()
        .take(MAX_FUZZ_BYTES)
        .map(|byte| char::from(b'a' + (byte % 26)))
        .collect()
}
