//! String interner persistence.

use crate::core::GLOBAL_INTERNER;
use std::fs;
use std::path::Path;

use super::error::{IndexPersistenceError, Result};
use super::formats::StringInternerData;
use super::{INTERNER_MAGIC, MANIFEST_VERSION};

/// Save the global string interner to disk.
pub fn save_string_interner(path: &Path) -> Result<()> {
    let strings = GLOBAL_INTERNER.get_all_strings();

    let data = StringInternerData {
        magic: INTERNER_MAGIC,
        version: MANIFEST_VERSION,
        string_count: strings.len() as u64,
        strings,
    };

    let encoded = bitcode::encode(&data);
    fs::write(path, encoded)?;

    Ok(())
}

/// Load the string interner from disk and restore GLOBAL_INTERNER.
pub fn load_string_interner(path: &Path) -> Result<StringInternerData> {
    let bytes = fs::read(path)?;
    let data: StringInternerData = bitcode::decode(&bytes)?;

    // Validate magic bytes
    if data.magic != INTERNER_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: INTERNER_MAGIC,
            got: data.magic,
        });
    }

    // Validate version
    if data.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: data.version,
            supported: MANIFEST_VERSION,
        });
    }

    Ok(data)
}

/// Restore GLOBAL_INTERNER from persisted data.
///
/// This must be called before loading any other indexes since they
/// reference string indices.
pub fn restore_string_interner(data: &StringInternerData) -> Result<()> {
    for (idx, s) in data.strings.iter().enumerate() {
        let interned_id = GLOBAL_INTERNER.intern(s).map_err(|e| {
            IndexPersistenceError::Serialization(format!("Failed to intern string: {}", e))
        })?;
        // The interner should assign indices in order
        // If not, the interner had pre-existing strings which is a bug
        if interned_id.as_u32() != idx as u32 {
            return Err(IndexPersistenceError::InternerMismatch {
                expected: idx as u32,
                got: interned_id.as_u32(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_string_interner_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interner.idx");

        // Intern some strings
        let _idx1 = GLOBAL_INTERNER.intern("test_string_1").unwrap();
        let _idx2 = GLOBAL_INTERNER.intern("test_string_2").unwrap();
        let _idx3 = GLOBAL_INTERNER.intern("test_string_3").unwrap();

        // Save
        save_string_interner(&path).unwrap();

        // Load
        let loaded = load_string_interner(&path).unwrap();

        assert_eq!(loaded.magic, INTERNER_MAGIC);
        assert!(loaded.strings.contains(&"test_string_1".to_string()));
        assert!(loaded.strings.contains(&"test_string_2".to_string()));
        assert!(loaded.strings.contains(&"test_string_3".to_string()));
    }

    #[test]
    fn test_invalid_magic_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.idx");

        // Write garbage
        let bad_data = StringInternerData {
            magic: *b"BAAD",
            version: 1,
            string_count: 0,
            strings: vec![],
        };
        let encoded = bitcode::encode(&bad_data);
        fs::write(&path, encoded).unwrap();

        // Should fail
        let result = load_string_interner(&path);
        assert!(matches!(result, Err(IndexPersistenceError::InvalidMagic { .. })));
    }
}
