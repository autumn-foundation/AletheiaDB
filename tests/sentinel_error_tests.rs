use aletheiadb::core::error::*;
use aletheiadb::core::id::NodeId;
use aletheiadb::storage::index_persistence::IndexPersistenceError;
use std::io;

#[test]
fn test_record_error_metric_returns_self() {
    let ok: Result<i32> = Ok(42);
    assert_eq!(ok.record_error_metric().unwrap(), 42);

    let err: Result<i32> = Err(Error::other("test error"));
    let recorded = err.record_error_metric();
    assert!(matches!(recorded, Err(Error::Other(msg)) if msg == "test error"));
}

#[test]
fn test_error_constructors_exact() {
    assert!(matches!(Error::other("foo"), Error::Other(msg) if msg == "foo"));
    assert!(matches!(Error::not_implemented("feature", "reason"),
        Error::NotImplemented { feature, reason } if feature == "feature" && reason == "reason"));
}

#[test]
fn test_error_from_implementations_exact() {
    let io_err = io::Error::new(io::ErrorKind::NotFound, "not found");
    assert!(matches!(Error::from(io_err), Error::Io(_)));

    let storage_err = StorageError::NodeNotFound(NodeId::new(1).unwrap());
    assert!(matches!(Error::from(storage_err), Error::Storage(_)));

    let temporal_err = TemporalError::InvalidTimeRange {
        start: aletheiadb::core::hlc::HybridTimestamp::new(1, 0).unwrap(),
        end: aletheiadb::core::hlc::HybridTimestamp::new(0, 0).unwrap(),
    };
    assert!(matches!(Error::from(temporal_err), Error::Temporal(_)));

    assert!(matches!(
        Error::from(QueryError::SyntaxError {
            message: "s".into()
        }),
        Error::Query(_)
    ));
    assert!(matches!(
        Error::from(TransactionError::WriteConflict {
            entity_id: "e".into(),
            reason: "c".into()
        }),
        Error::Transaction(_)
    ));
    assert!(matches!(
        Error::from(VectorError::DimensionMismatch {
            expected: 1,
            actual: 2
        }),
        Error::Vector(_)
    ));
}

#[test]
fn test_storage_error_constructors_exact() {
    let se_io = StorageError::io_error("test io");
    assert!(matches!(se_io, StorageError::IoError(msg) if msg == "test io"));

    let se_corr = StorageError::corruption("corrupted");
    assert!(matches!(se_corr, StorageError::CorruptedData(msg) if msg == "corrupted"));

    let se_persist = StorageError::persistence("persist error");
    assert!(matches!(se_persist, StorageError::PersistenceError(msg) if msg == "persist error"));

    let se_persist_kind =
        StorageError::persistence_with_kind(PersistenceErrorKind::MissingIndex, "disk full");
    assert!(
        matches!(se_persist_kind, StorageError::PersistenceErrorWithKind { kind: PersistenceErrorKind::MissingIndex, message } if message == "disk full")
    );
}

#[test]
fn test_persistence_error_kind_display() {
    assert_eq!(format!("{}", PersistenceErrorKind::Corrupted), "corrupted");
    assert_eq!(
        format!("{}", PersistenceErrorKind::InternerMismatch),
        "interner_mismatch"
    );
    assert_eq!(
        format!("{}", PersistenceErrorKind::UnsupportedVersion),
        "unsupported_version"
    );
    assert_eq!(
        format!("{}", PersistenceErrorKind::MissingIndex),
        "missing_index"
    );
    assert_eq!(
        format!("{}", PersistenceErrorKind::InvalidMagic),
        "invalid_magic"
    );
    assert_eq!(
        format!("{}", PersistenceErrorKind::SizeLimitExceeded),
        "size_limit_exceeded"
    );
}

#[test]
fn test_persistence_error_kind_from_index_persistence_error() {
    let err = IndexPersistenceError::Corrupted {
        path: ".".into(),
        source: Box::new(io::Error::other("corr")),
    };
    assert!(matches!(
        PersistenceErrorKind::from(&err),
        PersistenceErrorKind::Corrupted
    ));

    let err = IndexPersistenceError::InvalidMagic {
        path: ".".into(),
        expected: [0; 4],
        got: [1; 4],
    };
    assert!(matches!(
        PersistenceErrorKind::from(&err),
        PersistenceErrorKind::InvalidMagic
    ));

    let err = IndexPersistenceError::UnsupportedVersion {
        found: 1,
        supported: 2,
    };
    assert!(matches!(
        PersistenceErrorKind::from(&err),
        PersistenceErrorKind::UnsupportedVersion
    ));

    let err = IndexPersistenceError::MissingIndex {
        name: "test".into(),
    };
    assert!(matches!(
        PersistenceErrorKind::from(&err),
        PersistenceErrorKind::MissingIndex
    ));

    let err = IndexPersistenceError::SizeLimitExceeded {
        message: "size".into(),
    };
    assert!(matches!(
        PersistenceErrorKind::from(&err),
        PersistenceErrorKind::SizeLimitExceeded
    ));

    let err = IndexPersistenceError::InternerMismatch {
        expected: 1,
        got: 2,
    };
    assert!(matches!(
        PersistenceErrorKind::from(&err),
        PersistenceErrorKind::InternerMismatch
    ));
}

#[test]
fn test_storage_error_from_index_persistence_error() {
    let err = IndexPersistenceError::UnsupportedVersion {
        found: 1,
        supported: 2,
    };
    let storage_err = StorageError::from(err);
    assert!(matches!(
        storage_err,
        StorageError::PersistenceErrorWithKind {
            kind: PersistenceErrorKind::UnsupportedVersion,
            ..
        }
    ));
}
