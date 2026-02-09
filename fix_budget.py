import sys

with open("src/core/property.rs", "r") as f:
    content = f.read()

# Fix constant
content = content.replace("64 * 1024 * 1024", "512 * 1024 * 1024")

# Fix serialize_into logic
old_logic = """    pub fn serialize_into(&self, buffer: &mut Vec<u8>) -> Result<()> {
        let mut budget = MAX_SERIALIZATION_SIZE;
        // Account for current buffer size to prevent total growth abuse
        if buffer.len() > budget {
            return Err(StorageError::CorruptedData(
                "Buffer already exceeds max serialization size".to_string(),
            )
            .into());
        }
        budget -= buffer.len();
        self.serialize_recursive(buffer, 0, &mut budget)
    }"""

new_logic = """    pub fn serialize_into(&self, buffer: &mut Vec<u8>) -> Result<()> {
        // Track only the bytes written during this operation, not the total buffer size.
        // This allows appending small objects to a large buffer without false positives.
        let mut budget = MAX_SERIALIZATION_SIZE;
        self.serialize_recursive(buffer, 0, &mut budget)
    }"""

if old_logic in content:
    content = content.replace(old_logic, new_logic)
    print("Fixed serialize_into logic")
else:
    print("Could not find serialize_into logic to fix")

with open("src/core/property.rs", "w") as f:
    f.write(content)
