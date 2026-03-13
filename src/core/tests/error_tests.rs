#[cfg(test)]
mod tests {
    use crate::core::error::*;

    use crate::storage::index_persistence::IndexPersistenceError;
    use std::io;
    use std::path::PathBuf;

    #[test]
    fn test_record_error_metric() {
        let result: Result<i32> = Err(Error::Other("test".to_string()));
        let result = result.record_error_metric();
        assert!(result.is_err());
    }

    #[test]
    fn test_error_other() {
        let err = Error::other("custom error");
        assert!(matches!(err, Error::Other(_)));
    }

    #[test]
    fn test_error_not_implemented() {
        let err = Error::not_implemented("feature_x", "reason_y");
        match err {
            Error::NotImplemented { feature, reason } => {
                assert_eq!(feature, "feature_x");
                assert_eq!(reason, "reason_y");
            }
            _ => panic!("Expected NotImplemented"),
        }
    }

    #[test]
    fn test_from_config_error() {
        let err = crate::config::ConfigError::IoError("config io error".to_string());
        let converted: Error = err.into();
        match converted {
            Error::Other(msg) => assert_eq!(msg, "I/O error: config io error"),
            _ => panic!("Expected Other"),
        }
    }

    #[test]
    #[cfg(feature = "sql")]
    fn test_from_sql_error() {
        let err = crate::sql::SqlError::ParseError("parse error".to_string());
        let converted: Error = err.into();
        assert!(matches!(converted, Error::Query(QueryError::SyntaxError { .. })));
    }

    #[test]
    fn test_persistence_error_kind_display() {
        assert_eq!(format!("{}", PersistenceErrorKind::Corrupted), "corrupted");
        assert_eq!(format!("{}", PersistenceErrorKind::InternerMismatch), "interner_mismatch");
        assert_eq!(format!("{}", PersistenceErrorKind::UnsupportedVersion), "unsupported_version");
        assert_eq!(format!("{}", PersistenceErrorKind::MissingIndex), "missing_index");
        assert_eq!(format!("{}", PersistenceErrorKind::InvalidMagic), "invalid_magic");
        assert_eq!(format!("{}", PersistenceErrorKind::SizeLimitExceeded), "size_limit_exceeded");
        assert_eq!(format!("{}", PersistenceErrorKind::Io), "io");
        assert_eq!(format!("{}", PersistenceErrorKind::Serialization), "serialization");
        assert_eq!(format!("{}", PersistenceErrorKind::Unknown), "unknown");
    }

    #[test]
    fn test_from_index_persistence_error_for_persistence_error_kind() {
        let err = IndexPersistenceError::Corrupted { path: PathBuf::from("test"), source: Box::new(io::Error::new(io::ErrorKind::Other, "test")) };
        let kind: PersistenceErrorKind = (&err).into();
        assert_eq!(kind, PersistenceErrorKind::Corrupted);

        let err = IndexPersistenceError::InternerMismatch { expected: 1, got: 2 };
        let kind: PersistenceErrorKind = (&err).into();
        assert_eq!(kind, PersistenceErrorKind::InternerMismatch);

        let err = IndexPersistenceError::UnsupportedVersion { supported: 1, found: 2 };
        let kind: PersistenceErrorKind = (&err).into();
        assert_eq!(kind, PersistenceErrorKind::UnsupportedVersion);

        let err = IndexPersistenceError::MissingIndex { name: "test".to_string() };
        let kind: PersistenceErrorKind = (&err).into();
        assert_eq!(kind, PersistenceErrorKind::MissingIndex);

        let err = IndexPersistenceError::InvalidMagic { path: PathBuf::from("test"), expected: [0;4], got: [1;4] };
        let kind: PersistenceErrorKind = (&err).into();
        assert_eq!(kind, PersistenceErrorKind::InvalidMagic);

        let err = IndexPersistenceError::SizeLimitExceeded { message: "test".to_string() };
        let kind: PersistenceErrorKind = (&err).into();
        assert_eq!(kind, PersistenceErrorKind::SizeLimitExceeded);

        let err = IndexPersistenceError::Io(io::Error::new(io::ErrorKind::Other, "test"));
        let kind: PersistenceErrorKind = (&err).into();
        assert_eq!(kind, PersistenceErrorKind::Io);

        let err = IndexPersistenceError::Serialization("test".to_string());
        let kind: PersistenceErrorKind = (&err).into();
        assert_eq!(kind, PersistenceErrorKind::Serialization);
    }

    #[test]
    fn test_storage_error_constructors() {
        assert_eq!(StorageError::io_error("test_io"), StorageError::IoError("test_io".to_string()));
        assert_eq!(StorageError::corruption("test_corr"), StorageError::CorruptedData("test_corr".to_string()));
        assert_eq!(StorageError::persistence("test_pers"), StorageError::PersistenceError("test_pers".to_string()));
        assert_eq!(StorageError::persistence_with_kind(PersistenceErrorKind::Corrupted, "test"),
            StorageError::PersistenceErrorWithKind { kind: PersistenceErrorKind::Corrupted, message: "test".to_string() });
    }

    #[test]
    fn test_from_index_persistence_error_for_storage_error() {
        let err = IndexPersistenceError::Corrupted { path: PathBuf::from("test"), source: Box::new(io::Error::new(io::ErrorKind::Other, "test")) };
        let storage_err: StorageError = err.into();
        match storage_err {
            StorageError::PersistenceErrorWithKind { kind, .. } => {
                assert_eq!(kind, PersistenceErrorKind::Corrupted);
            }
            _ => panic!("Expected PersistenceErrorWithKind"),
        }
    }
}
    #[test]
    fn test_format_index_not_found() {
        let msg = crate::core::error::format_index_not_found("test_type", "test_prop", &None);
        assert_eq!(msg, "Query requires test_type index on 'test_prop' property which is not enabled");

        let msg2 = crate::core::error::format_index_not_found("test_type", "test_prop", &Some("test_hint".to_string()));
        assert_eq!(msg2, "Query requires test_type index on 'test_prop' property which is not enabled. Hint: test_hint");
    }

    #[test]
    fn test_format_clock_skew() {
        let msg = crate::core::error::format_clock_skew(1000, 1100, -100, 50);
        assert!(msg.contains("backward"));
        assert!(!msg.contains("forward"));
        assert!(msg.contains("by 100"));
        assert!(msg.contains("max allowed: 50"));
        assert!(msg.contains("Wallclock: 1000"));
        assert!(msg.contains("Previous: 1100"));

        let msg2 = crate::core::error::format_clock_skew(1100, 1000, 100, 50);
        assert!(msg2.contains("forward"));
        assert!(!msg2.contains("backward"));
        assert!(msg2.contains("by 100"));
        assert!(msg2.contains("max allowed: 50"));
        assert!(msg2.contains("Wallclock: 1100"));
        assert!(msg2.contains("Previous: 1000"));

        let msg3 = crate::core::error::format_clock_skew(1000, 1000, 0, 50);
        assert!(msg3.contains("forward"));
        assert!(!msg3.contains("backward"));
        assert!(msg3.contains("by 0"));
        assert!(msg3.contains("max allowed: 50"));
        assert!(msg3.contains("Wallclock: 1000"));
        assert!(msg3.contains("Previous: 1000"));
    }
