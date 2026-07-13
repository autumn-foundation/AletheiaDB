//! Structured audit logging for encryption operations (Issue #489).
//!
//! Emits compliance-friendly, structured **JSON-lines** for key-management
//! events and (optionally) individual encrypt/decrypt operations. Each line
//! matches the schema:
//!
//! ```json
//! {
//!   "timestamp": "2024-01-15T10:30:00.123Z",
//!   "event": "key.loaded",
//!   "level": "info",
//!   "data": { "key_version": 1, "provider_type": "file" },
//!   "instance_id": "node-1",
//!   "correlation_id": "evt-1a2b3c4d"
//! }
//! ```
//!
//! # Security invariants
//!
//! The audit path **never** logs key material (the MEK, DEKs, `Zeroizing`
//! buffers, or raw key bytes). Only opaque descriptors (provider name, key
//! version, error *category*) are emitted. Error strings are reduced to a
//! stable category token so that key file paths or other secrets present in a
//! raw error `Display` can never leak into the audit log.
//!
//! # Fail-safe
//!
//! An audit-logging failure (for example, the destination log file cannot be
//! opened) must never crash or block the database. Write failures degrade
//! gracefully: a one-line warning is printed to stderr and execution
//! continues.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::encryption::config::{AuditConfig, AuditDestination};

/// Level of audit logging.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "config-toml", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config-toml", serde(rename_all = "snake_case"))]
pub enum AuditLevel {
    /// No audit logging.
    None,
    /// Log only key management events (load, rotate, access-denied).
    #[default]
    KeyEvents,
    /// Log all encryption operations (high volume).
    AllOperations,
}

/// An encryption audit event.
#[derive(Debug, Clone)]
pub enum AuditEvent {
    /// Master key loaded at startup.
    KeyLoaded {
        /// Name of the key provider (e.g., "file", "env").
        provider: String,
        /// Key version that was loaded.
        key_version: u32,
    },
    /// Key rotation started.
    RotationStarted {
        /// Previous key version.
        old_version: u32,
        /// New key version being rotated to.
        new_version: u32,
    },
    /// Key rotation completed successfully.
    RotationCompleted {
        /// The new active key version.
        new_version: u32,
        /// Time taken for the rotation in milliseconds.
        duration_ms: u64,
    },
    /// Key rotation failed.
    RotationFailed {
        /// Key version that failed to rotate.
        version: u32,
        /// Error description.
        error: String,
    },
    /// Key access denied / key load failed by the provider.
    KeyAccessDenied {
        /// Name of the key provider.
        provider: String,
        /// Stable, key-safe error category (never raw key material or paths).
        error: String,
    },
    /// Encryption operation performed (high volume, only at `AllOperations` level).
    EncryptOperation {
        /// Component that performed the operation (e.g., "wal", "index").
        component: String,
        /// Key version used.
        key_version: u32,
    },
    /// Decryption operation performed (high volume, only at `AllOperations` level).
    DecryptOperation {
        /// Component that performed the operation.
        component: String,
        /// Key version used.
        key_version: u32,
    },
}

impl AuditEvent {
    /// Minimum [`AuditLevel`] at which this event is emitted.
    fn required_level(&self) -> AuditLevel {
        match self {
            AuditEvent::KeyLoaded { .. }
            | AuditEvent::RotationStarted { .. }
            | AuditEvent::RotationCompleted { .. }
            | AuditEvent::RotationFailed { .. }
            | AuditEvent::KeyAccessDenied { .. } => AuditLevel::KeyEvents,
            AuditEvent::EncryptOperation { .. } | AuditEvent::DecryptOperation { .. } => {
                AuditLevel::AllOperations
            }
        }
    }

    /// Stable dotted event name for the `event` field.
    fn event_name(&self) -> String {
        match self {
            AuditEvent::KeyLoaded { .. } => "key.loaded".to_string(),
            AuditEvent::RotationStarted { .. } => "key.rotation.started".to_string(),
            AuditEvent::RotationCompleted { .. } => "key.rotation.completed".to_string(),
            AuditEvent::RotationFailed { .. } => "key.rotation.failed".to_string(),
            AuditEvent::KeyAccessDenied { .. } => "key.access.denied".to_string(),
            AuditEvent::EncryptOperation { component, .. } => format!("encrypt.{component}"),
            AuditEvent::DecryptOperation { component, .. } => format!("decrypt.{component}"),
        }
    }

    /// Severity string for the `level` field (info/error).
    fn severity(&self) -> &'static str {
        match self {
            AuditEvent::RotationFailed { .. } | AuditEvent::KeyAccessDenied { .. } => "error",
            _ => "info",
        }
    }

    /// Structured `data` payload. Contains only opaque descriptors -- never key
    /// material.
    fn data(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            AuditEvent::KeyLoaded {
                provider,
                key_version,
            } => json!({ "key_version": key_version, "provider_type": provider }),
            AuditEvent::RotationStarted {
                old_version,
                new_version,
            } => json!({ "old_version": old_version, "new_version": new_version }),
            AuditEvent::RotationCompleted {
                new_version,
                duration_ms,
            } => json!({ "version": new_version, "duration_ms": duration_ms }),
            AuditEvent::RotationFailed { version, error } => {
                json!({ "version": version, "error": error })
            }
            AuditEvent::KeyAccessDenied { provider, error } => {
                json!({ "provider": provider, "error": error })
            }
            AuditEvent::EncryptOperation {
                component,
                key_version,
            }
            | AuditEvent::DecryptOperation {
                component,
                key_version,
            } => json!({ "component": component, "key_version": key_version }),
        }
    }
}

/// Where audit lines are written.
#[derive(Debug, Clone)]
enum AuditSink {
    /// Write to standard output.
    Stdout,
    /// Write to standard error (backwards-compatible default).
    Stderr,
    /// Append to a file at the given path.
    File(PathBuf),
    /// Syslog destination -- not yet implemented; falls back to stderr with a
    /// note (documented follow-up).
    Syslog,
}

/// Audit logger for encryption operations.
///
/// Filters events based on the configured [`AuditLevel`] and emits structured
/// JSON lines to the configured destination.
pub struct EncryptionAuditLogger {
    level: AuditLevel,
    instance_id: String,
    sink: AuditSink,
    /// When set, emitted lines are captured here instead of written to `sink`.
    /// Only populated by test helpers; always `None` in production.
    capture: Option<Arc<Mutex<Vec<String>>>>,
}

impl EncryptionAuditLogger {
    /// Create a new audit logger with the given level and instance identifier.
    ///
    /// Writes to stderr (backwards-compatible default). Use
    /// [`from_audit_config`](Self::from_audit_config) to honor a destination.
    ///
    /// # Why?
    /// The `instance_id` allows centralized logging platforms to differentiate
    /// encryption events coming from multiple database nodes in a clustered environment.
    ///
    /// ## Examples
    /// ```
    /// use aletheiadb::encryption::audit::{EncryptionAuditLogger, AuditLevel};
    ///
    /// let logger = EncryptionAuditLogger::new(AuditLevel::KeyEvents, "node-1");
    /// assert_eq!(logger.level(), AuditLevel::KeyEvents);
    /// ```
    #[must_use]
    pub fn new(level: AuditLevel, instance_id: impl Into<String>) -> Self {
        Self {
            level,
            instance_id: instance_id.into(),
            sink: AuditSink::Stderr,
            capture: None,
        }
    }

    /// Create a disabled audit logger that swallows all events.
    ///
    /// # Why?
    /// Provides a zero-cost abstraction for configurations where audit logging
    /// is disabled, avoiding `Option` unwrapping on every database operation.
    ///
    /// ## Examples
    /// ```
    /// use aletheiadb::encryption::audit::{EncryptionAuditLogger, AuditLevel};
    ///
    /// let logger = EncryptionAuditLogger::disabled();
    /// assert_eq!(logger.level(), AuditLevel::None);
    /// ```
    #[must_use]
    pub fn disabled() -> Self {
        Self::new(AuditLevel::None, "")
    }

    /// Build a logger from an [`AuditConfig`].
    ///
    /// When `config.enabled` is `false`, the returned logger is
    /// [`disabled`](Self::disabled) (level `None`) and produces **zero** output,
    /// preserving backwards-compatible behavior for databases without an
    /// `[encryption.audit]` block.
    ///
    /// # Why?
    /// Centralizes translation from declarative config (destination, file path,
    /// level) into a ready-to-use logger, so callers (the encryption manager)
    /// don't reimplement destination resolution.
    #[must_use]
    pub fn from_audit_config(config: &AuditConfig) -> Self {
        if !config.enabled {
            return Self::disabled();
        }

        let sink = match config.destination {
            AuditDestination::Stdout => AuditSink::Stdout,
            AuditDestination::File => match &config.file_path {
                Some(path) => AuditSink::File(path.clone()),
                None => {
                    eprintln!(
                        "[AUDIT] warning: destination=file but no file_path configured; falling back to stdout"
                    );
                    AuditSink::Stdout
                }
            },
            AuditDestination::Syslog => AuditSink::Syslog,
        };

        let instance_id = config
            .instance_id
            .clone()
            .unwrap_or_else(default_instance_id);

        Self {
            level: config.level,
            instance_id,
            sink,
            capture: None,
        }
    }

    /// Get the configured audit level.
    ///
    /// # Why?
    /// Allows external components (like the query planner) to check if detailed
    /// logging is active, potentially optimizing away expensive telemetry data generation.
    ///
    /// ## Examples
    /// ```
    /// use aletheiadb::encryption::audit::{EncryptionAuditLogger, AuditLevel};
    ///
    /// let logger = EncryptionAuditLogger::disabled();
    /// assert_eq!(logger.level(), AuditLevel::None);
    /// ```
    #[must_use]
    pub fn level(&self) -> AuditLevel {
        self.level
    }

    /// Returns `true` if the given event would be emitted at the current level.
    #[must_use]
    pub fn is_enabled_for(&self, event: &AuditEvent) -> bool {
        self.level != AuditLevel::None && self.level >= event.required_level()
    }

    /// Format an event as a structured JSON line, or `None` if the current
    /// [`AuditLevel`] suppresses it.
    ///
    /// # Why?
    /// Separating formatting from emission lets callers (and tests) inspect the
    /// exact serialized payload -- crucial for verifying that no key material
    /// ever appears in the output.
    #[must_use]
    pub fn format_entry(&self, event: &AuditEvent) -> Option<String> {
        if !self.is_enabled_for(event) {
            return None;
        }

        let entry = serde_json::json!({
            "timestamp": now_rfc3339_millis(),
            "event": event.event_name(),
            "level": event.severity(),
            "data": event.data(),
            "instance_id": self.instance_id,
            "correlation_id": new_correlation_id(),
        });

        Some(entry.to_string())
    }

    /// Log an audit event if the configured level permits it.
    ///
    /// Emission is **fail-safe**: a write error (e.g. an unopenable log file)
    /// is reported to stderr and swallowed; it never panics or propagates.
    ///
    /// # Why?
    /// Centralizes all the filtering and formatting logic, ensuring that sensitive
    /// keys are never accidentally logged, and that log structures remain consistent.
    ///
    /// ## Examples
    /// ```
    /// use aletheiadb::encryption::audit::{EncryptionAuditLogger, AuditLevel, AuditEvent};
    ///
    /// let logger = EncryptionAuditLogger::new(AuditLevel::KeyEvents, "node-1");
    /// let event = AuditEvent::KeyLoaded {
    ///     provider: "file".to_string(),
    ///     key_version: 1,
    /// };
    ///
    /// logger.log(&event);
    /// ```
    pub fn log(&self, event: &AuditEvent) {
        if let Some(line) = self.format_entry(event) {
            self.emit(&line);
        }
    }

    /// Write a formatted line to the configured sink, degrading gracefully on
    /// failure.
    fn emit(&self, line: &str) {
        if let Some(buf) = &self.capture {
            if let Ok(mut guard) = buf.lock() {
                guard.push(line.to_string());
            }
            return;
        }

        match &self.sink {
            AuditSink::Stdout => println!("{line}"),
            AuditSink::Stderr => eprintln!("{line}"),
            AuditSink::Syslog => {
                // Not yet implemented -- documented follow-up (#489). Degrade to
                // stderr so events are never silently dropped.
                eprintln!("{line}");
            }
            AuditSink::File(path) => {
                use std::io::Write;
                let open = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path);
                match open {
                    Ok(mut f) => {
                        if let Err(e) = writeln!(f, "{line}") {
                            // Fail-safe: never crash the DB on an audit write error.
                            eprintln!(
                                "[AUDIT] warning: failed to write audit log to {}: {e}",
                                path.display()
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "[AUDIT] warning: failed to open audit log {}: {e}",
                            path.display()
                        );
                    }
                }
            }
        }
    }
}

/// Default per-process instance identifier when none is configured.
fn default_instance_id() -> String {
    format!("aletheiadb-{}", std::process::id())
}

/// Current UTC time as an RFC 3339 string with millisecond precision and a `Z`
/// suffix (e.g. `2024-01-15T10:30:00.123Z`).
fn now_rfc3339_millis() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Generate a per-event correlation id (`evt-<8 hex>`).
fn new_correlation_id() -> String {
    let n: u32 = rand::random();
    format!("evt-{n:08x}")
}

#[cfg(test)]
impl EncryptionAuditLogger {
    /// Test helper: build a logger that captures emitted JSON lines into a
    /// shared buffer instead of writing to a destination.
    pub(crate) fn with_capture(
        level: AuditLevel,
        instance_id: impl Into<String>,
    ) -> (Self, Arc<Mutex<Vec<String>>>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let logger = Self {
            level,
            instance_id: instance_id.into(),
            sink: AuditSink::Stdout,
            capture: Some(Arc::clone(&buf)),
        };
        (logger, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_loaded() -> AuditEvent {
        AuditEvent::KeyLoaded {
            provider: "file".into(),
            key_version: 1,
        }
    }

    #[test]
    fn disabled_logger_does_not_panic() {
        let logger = EncryptionAuditLogger::disabled();
        assert_eq!(logger.level(), AuditLevel::None);

        // Log all event variants -- none should panic.
        logger.log(&key_loaded());
        logger.log(&AuditEvent::RotationStarted {
            old_version: 1,
            new_version: 2,
        });
        logger.log(&AuditEvent::RotationCompleted {
            new_version: 2,
            duration_ms: 42,
        });
        logger.log(&AuditEvent::RotationFailed {
            version: 2,
            error: "boom".into(),
        });
        logger.log(&AuditEvent::KeyAccessDenied {
            provider: "env".into(),
            error: "denied".into(),
        });
        logger.log(&AuditEvent::EncryptOperation {
            component: "wal".into(),
            key_version: 1,
        });
        logger.log(&AuditEvent::DecryptOperation {
            component: "index".into(),
            key_version: 1,
        });
    }

    #[test]
    fn key_events_level_filters_operations() {
        let logger = EncryptionAuditLogger::new(AuditLevel::KeyEvents, "test-node");
        assert_eq!(logger.level(), AuditLevel::KeyEvents);

        // Key events emit.
        assert!(logger.format_entry(&key_loaded()).is_some());
        assert!(
            logger
                .format_entry(&AuditEvent::RotationStarted {
                    old_version: 1,
                    new_version: 2,
                })
                .is_some()
        );

        // Operation-level events are filtered out.
        assert!(
            logger
                .format_entry(&AuditEvent::EncryptOperation {
                    component: "wal".into(),
                    key_version: 1,
                })
                .is_none()
        );
        assert!(
            logger
                .format_entry(&AuditEvent::DecryptOperation {
                    component: "cold".into(),
                    key_version: 1,
                })
                .is_none()
        );
    }

    #[test]
    fn none_level_suppresses_key_events() {
        let logger = EncryptionAuditLogger::new(AuditLevel::None, "test-node");
        assert!(logger.format_entry(&key_loaded()).is_none());
    }

    #[test]
    fn all_operations_logs_everything() {
        let logger = EncryptionAuditLogger::new(AuditLevel::AllOperations, "test-node");
        assert!(logger.format_entry(&key_loaded()).is_some());
        assert!(
            logger
                .format_entry(&AuditEvent::EncryptOperation {
                    component: "wal".into(),
                    key_version: 1,
                })
                .is_some()
        );
        assert!(
            logger
                .format_entry(&AuditEvent::DecryptOperation {
                    component: "index".into(),
                    key_version: 2,
                })
                .is_some()
        );
    }

    #[test]
    fn key_loaded_entry_has_expected_json_shape() {
        let logger = EncryptionAuditLogger::new(AuditLevel::KeyEvents, "node-7");
        let line = logger.format_entry(&key_loaded()).expect("should emit");
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid json");

        assert_eq!(v["event"], "key.loaded");
        assert_eq!(v["level"], "info");
        assert_eq!(v["instance_id"], "node-7");
        assert_eq!(v["data"]["provider_type"], "file");
        assert_eq!(v["data"]["key_version"], 1);
        // Structural fields present.
        assert!(v["timestamp"].is_string());
        assert!(
            v["correlation_id"]
                .as_str()
                .is_some_and(|c| c.starts_with("evt-"))
        );
        // Timestamp is RFC3339 UTC (Z suffix).
        assert!(v["timestamp"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn key_access_denied_entry_has_category_and_no_secret() {
        let logger = EncryptionAuditLogger::new(AuditLevel::KeyEvents, "node-1");
        let event = AuditEvent::KeyAccessDenied {
            provider: "file".into(),
            error: "key_not_found".into(),
        };
        let line = logger.format_entry(&event).expect("should emit");
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid json");

        assert_eq!(v["event"], "key.access.denied");
        assert_eq!(v["level"], "error");
        assert_eq!(v["data"]["provider"], "file");
        assert_eq!(v["data"]["error"], "key_not_found");
    }

    /// Security assertion: a key.loaded audit line must not contain any raw
    /// key bytes (hex or otherwise).
    #[test]
    fn no_key_material_in_output() {
        // A known 32-byte key rendered as hex -- the sort of secret that must
        // NEVER appear in an audit line.
        let secret_hex = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
        let (logger, buf) = EncryptionAuditLogger::with_capture(AuditLevel::KeyEvents, "node-1");

        logger.log(&key_loaded());

        let lines = buf.lock().unwrap();
        assert_eq!(lines.len(), 1, "exactly one line emitted");
        let line = &lines[0];
        assert!(
            !line.contains(secret_hex),
            "audit line must not contain key material: {line}"
        );
        // The only data fields are opaque descriptors.
        assert!(line.contains("provider_type"));
        assert!(line.contains("key_version"));
    }

    #[test]
    fn capture_logger_records_emitted_lines() {
        let (logger, buf) = EncryptionAuditLogger::with_capture(AuditLevel::KeyEvents, "node-1");
        logger.log(&key_loaded());
        // Suppressed event does not record.
        logger.log(&AuditEvent::EncryptOperation {
            component: "wal".into(),
            key_version: 1,
        });
        assert_eq!(buf.lock().unwrap().len(), 1);
    }

    #[test]
    fn from_audit_config_disabled_produces_no_output() {
        let cfg = AuditConfig::default();
        assert!(!cfg.enabled);
        let logger = EncryptionAuditLogger::from_audit_config(&cfg);
        assert_eq!(logger.level(), AuditLevel::None);
        assert!(logger.format_entry(&key_loaded()).is_none());
    }

    #[test]
    fn from_audit_config_enabled_honors_level() {
        let cfg = AuditConfig {
            enabled: true,
            level: AuditLevel::KeyEvents,
            instance_id: Some("node-x".into()),
            ..AuditConfig::default()
        };
        let logger = EncryptionAuditLogger::from_audit_config(&cfg);
        let line = logger.format_entry(&key_loaded()).expect("should emit");
        assert!(line.contains("node-x"));
    }

    #[test]
    fn file_destination_write_is_fail_safe() {
        // Point at an unwritable path (nested under a nonexistent root). Emission
        // must not panic; it degrades to a stderr warning.
        let logger = EncryptionAuditLogger {
            level: AuditLevel::KeyEvents,
            instance_id: "node-1".into(),
            sink: AuditSink::File(PathBuf::from("/nonexistent-dir-489/sub/audit.log")),
            capture: None,
        };
        logger.log(&key_loaded());
    }

    #[test]
    fn audit_level_ordering() {
        assert!(AuditLevel::None < AuditLevel::KeyEvents);
        assert!(AuditLevel::KeyEvents < AuditLevel::AllOperations);
        assert!(AuditLevel::None < AuditLevel::AllOperations);
    }

    #[test]
    fn audit_level_default_is_key_events() {
        assert_eq!(AuditLevel::default(), AuditLevel::KeyEvents);
    }

    #[test]
    fn audit_event_clone_and_debug() {
        let event = key_loaded();
        let cloned = event.clone();
        let debug = format!("{cloned:?}");
        assert!(debug.contains("KeyLoaded"));
    }
}
