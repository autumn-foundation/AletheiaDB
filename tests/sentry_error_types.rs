use aletheiadb::core::error::{Error, StorageError, PersistenceErrorKind, QueryError, TransactionError};
use aletheiadb::storage::index_persistence::IndexPersistenceError;

#[test]
fn test_error_other() {
    let err = Error::other("custom error test");
    if let Error::Other(msg) = err {
        assert_eq!(msg, "custom error test");
    } else {
        panic!("Expected Error::Other");
    }
}

#[test]
fn test_error_not_implemented() {
    let err = Error::not_implemented("feature X", "Phase 42");
    if let Error::NotImplemented { feature, reason } = err {
        assert_eq!(feature, "feature X");
        assert_eq!(reason, "Phase 42");
    } else {
        panic!("Expected Error::NotImplemented");
    }
}

#[test]
fn test_storage_error_io_error() {
    let err = StorageError::io_error("io failed");
    if let StorageError::IoError(msg) = err {
        assert_eq!(msg, "io failed");
    } else {
        panic!("Expected StorageError::IoError");
    }
}

#[test]
fn test_storage_error_corruption() {
    let err = StorageError::corruption("corrupted file");
    if let StorageError::CorruptedData(msg) = err {
        assert_eq!(msg, "corrupted file");
    } else {
        panic!("Expected StorageError::CorruptedData");
    }
}

#[test]
fn test_storage_error_persistence() {
    let err = StorageError::persistence("failed save");
    if let StorageError::PersistenceError(msg) = err {
        assert_eq!(msg, "failed save");
    } else {
        panic!("Expected StorageError::PersistenceError");
    }
}

#[test]
fn test_storage_error_persistence_with_kind() {
    let err = StorageError::persistence_with_kind(PersistenceErrorKind::MissingIndex, "missing test");
    if let StorageError::PersistenceErrorWithKind { kind, message } = err {
        assert_eq!(kind, PersistenceErrorKind::MissingIndex);
        assert_eq!(message, "missing test");
    } else {
        panic!("Expected StorageError::PersistenceErrorWithKind");
    }
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
fn test_index_persistence_error_from() {
    let dummy_err: Box<dyn std::error::Error + Send + Sync> = Box::new(std::io::Error::other("dummy"));
    let err = IndexPersistenceError::Corrupted { path: "test.db".into(), source: dummy_err };
    assert_eq!(PersistenceErrorKind::from(&err), PersistenceErrorKind::Corrupted);

    let err = IndexPersistenceError::InternerMismatch { expected: 1, got: 2 };
    assert_eq!(PersistenceErrorKind::from(&err), PersistenceErrorKind::InternerMismatch);

    let err = IndexPersistenceError::UnsupportedVersion { found: 1, supported: 2 };
    assert_eq!(PersistenceErrorKind::from(&err), PersistenceErrorKind::UnsupportedVersion);

    let err = IndexPersistenceError::MissingIndex { name: "test".into() };
    assert_eq!(PersistenceErrorKind::from(&err), PersistenceErrorKind::MissingIndex);

    let err = IndexPersistenceError::InvalidMagic { path: "test".into(), expected: [0, 0, 0, 0], got: [1, 1, 1, 1] };
    assert_eq!(PersistenceErrorKind::from(&err), PersistenceErrorKind::InvalidMagic);

    let err = IndexPersistenceError::SizeLimitExceeded { message: "test".into() };
    assert_eq!(PersistenceErrorKind::from(&err), PersistenceErrorKind::SizeLimitExceeded);

    let err = IndexPersistenceError::Io(std::io::Error::other("io"));
    assert_eq!(PersistenceErrorKind::from(&err), PersistenceErrorKind::Io);

    let err = IndexPersistenceError::Serialization("ser".into());
    assert_eq!(PersistenceErrorKind::from(&err), PersistenceErrorKind::Serialization);
}

#[test]
fn test_storage_error_from_index_persistence_error() {
    let dummy_err: Box<dyn std::error::Error + Send + Sync> = Box::new(std::io::Error::other("dummy"));
    let err = IndexPersistenceError::Corrupted { path: "test.db".into(), source: dummy_err };
    let error_string = err.to_string();
    let storage_err: StorageError = err.into();
    if let StorageError::PersistenceErrorWithKind { kind, message } = storage_err {
        assert_eq!(kind, PersistenceErrorKind::Corrupted);
        assert_eq!(message, error_string);
    } else {
        panic!("Expected StorageError::PersistenceErrorWithKind");
    }
}

#[test]
fn test_format_index_not_found() {
    let err = QueryError::IndexNotFound {
        index_type: "vector".into(),
        property_name: "embedding".into(),
        hint: None,
    };
    assert_eq!(err.to_string(), "Query requires vector index on 'embedding' property which is not enabled");

    let err_with_hint = QueryError::IndexNotFound {
        index_type: "vector".into(),
        property_name: "embedding".into(),
        hint: Some("do a thing".into()),
    };
    assert_eq!(err_with_hint.to_string(), "Query requires vector index on 'embedding' property which is not enabled. Hint: do a thing");
}

#[test]
fn test_format_clock_skew_exact() {
    let err = TransactionError::ClockSkew {
        wallclock: 1000,
        previous: 1100,
        drift_us: -100,
        max_allowed: 50,
    };
    let display = err.to_string();
    // Using string slice verification to avoid being totally brittle but confirm
    // exact direction output logic from the function.
    assert!(display.contains("backward"));
    assert!(display.contains("100"));
    assert!(display.contains("1000"));
    assert!(display.contains("1100"));

    let err2 = TransactionError::ClockSkew {
        wallclock: 1000,
        previous: 900,
        drift_us: 100,
        max_allowed: 50,
    };
    let display2 = err2.to_string();
    assert!(display2.contains("forward"));
}

#[test]
fn test_format_clock_skew_strict_boundary() {
    // Specifically test that drift_us = 0 results in "forward".
    // This kills the < replaced with <= or == mutants.
    let err = TransactionError::ClockSkew {
        wallclock: 1000,
        previous: 1000,
        drift_us: 0,
        max_allowed: 50,
    };
    let display = err.to_string();
    assert!(display.contains("forward"));
}
