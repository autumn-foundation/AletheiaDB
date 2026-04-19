import sys

new_tests = """
    #[test]
    fn test_trivial_error_conversions() {
        let err = Error::from(std::io::Error::new(std::io::ErrorKind::NotFound, "test"));
        assert!(matches!(err, Error::Io(_)));

        let err = Error::from(TransactionError::Aborted { tx_id: 1 });
        assert!(matches!(err, Error::Transaction(_)));

        let err = Error::from(VectorError::ContainsNaN { count: 1 });
        assert!(matches!(err, Error::Vector(_)));

        let err = Error::from(crate::config::ConfigError::InvalidValue("test".to_string()));
        assert!(matches!(err, Error::Other(_)));
    }

    #[cfg(feature = "sql")]
    #[test]
    fn test_sql_error_conversions() {
        use crate::sql::SqlError;
        let cases = vec![
            (SqlError::ParseError("e".to_string()), "Syntax error"),
            (SqlError::UnsupportedFeature("f".to_string()), "Unsupported feature"),
            (SqlError::InvalidTable("t".to_string()), "Invalid parameter 'table'"),
            (SqlError::InvalidColumn("c".to_string()), "Invalid parameter 'column'"),
            (SqlError::InvalidTemporalClause("t".to_string()), "Invalid parameter 'temporal_clause'"),
            (SqlError::InvalidTimestamp("t".to_string()), "Invalid parameter 'timestamp'"),
            (SqlError::MissingClause("c".to_string()), "Invalid parameter 'clause'"),
            (SqlError::TypeError("t".to_string()), "Type mismatch"),
            (SqlError::ParameterError("p".to_string()), "Invalid parameter 'parameter'"),
        ];
        for (err, expect) in cases {
            let core_err = Error::from(err);
            assert!(format!("{}", core_err).contains(expect));
        }
    }

    #[cfg(feature = "cypher")]
    #[test]
    fn test_cypher_error_conversions() {
        use crate::cypher::CypherError;
        let cases = vec![
            (CypherError::LexError { message: "e".to_string(), position: 0 }, "Syntax error"),
            (CypherError::ParseError { message: "e".to_string(), position: 0 }, "Syntax error"),
            (CypherError::UnsupportedFeature("f".to_string()), "Unsupported feature"),
            (CypherError::InvalidTemporalClause("t".to_string()), "Invalid parameter 'temporal_clause'"),
            (CypherError::InvalidTimestamp("t".to_string()), "Invalid parameter 'timestamp'"),
            (CypherError::TypeError("t".to_string()), "Type mismatch"),
            (CypherError::ParameterError("e".to_string()), "Invalid parameter 'parameter'"),
        ];
        for (err, expect) in cases {
            let core_err = Error::from(err);
            assert!(format!("{}", core_err).contains(expect));
        }
    }

    #[test]
    fn test_persistence_error_conversions() {
        use crate::storage::index_persistence::IndexPersistenceError;
        use std::path::PathBuf;
        let errs = vec![
            IndexPersistenceError::Corrupted { path: PathBuf::new(), source: Box::new(std::io::Error::new(std::io::ErrorKind::Other, "test")) },
            IndexPersistenceError::InternerMismatch { expected: 1, got: 2 },
            IndexPersistenceError::UnsupportedVersion { found: 1, supported: 2 },
            IndexPersistenceError::MissingIndex { name: "test".to_string() },
            IndexPersistenceError::InvalidMagic { path: PathBuf::new(), expected: [0; 4], got: [1; 4] },
            IndexPersistenceError::SizeLimitExceeded { message: "test".to_string() },
            IndexPersistenceError::Io(std::io::Error::new(std::io::ErrorKind::Other, "test")),
            IndexPersistenceError::Serialization("test".to_string()),
        ];
        for err in errs {
            let kind = PersistenceErrorKind::from(&err);
            let storage_err = StorageError::from(err);
            assert!(matches!(storage_err, StorageError::PersistenceErrorWithKind { kind: ref k, .. } if *k == kind));
        }
    }

    #[test]
    fn test_encryption_and_key_provider_conversions() {
        use crate::encryption::{EncryptionError, KeyProviderError};
        let err = StorageError::from(EncryptionError::EncryptFailed("bad data".into()));
        assert!(matches!(err, StorageError::Encryption(_)));

        let err = StorageError::from(KeyProviderError::KeyNotFound);
        assert!(matches!(err, StorageError::KeyProvider(_)));
    }
"""

with open("src/core/error.rs", "r") as f:
    content = f.read()

pos = content.rfind("}")
content = content[:pos] + new_tests + content[pos:]

with open("src/core/error.rs", "w") as f:
    f.write(content)
