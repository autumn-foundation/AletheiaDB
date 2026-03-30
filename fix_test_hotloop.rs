use std::fs;

fn main() {
    let filepath = "src/index/vector/hnsw/mod.rs";
    let mut content = fs::read_to_string(filepath).unwrap();

    let bad_block = r#"        // 🛡️ Warden: Strict bounds checking to prevent out-of-bounds reads if usearch or tests
        // pass invalid dimensions to the FFI boundary.
        if dims == 0 || dims > crate::core::vector::constants::MAX_VECTOR_DIMENSIONS {
            eprintln!(
                "usearch passed invalid dimensions to metric function (got {}, max {}) - returning max distance",
                dims, crate::core::vector::constants::MAX_VECTOR_DIMENSIONS
            );
            return f32::MAX;
        }"#;

    if content.contains(bad_block) {
        content = content.replace(bad_block, "");
        fs::write(filepath, content).unwrap();
    }
}
