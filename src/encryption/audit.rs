//! Structured audit logging for encryption operations.
//!
//! Emits compliance-friendly log lines for key management events and
//! (optionally) individual encrypt/decrypt operations.

use std::time::SystemTime;

/// Level of audit logging.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuditLevel {
    /// No audit logging.
    None,
    /// Log only key management events (load, rotate).
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
    /// Key access denied by the provider.
    KeyAccessDenied {
        /// Name of the key provider.
        provider: String,
        /// Error description.
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

/// Audit logger for encryption operations.
///
/// Filters events based on the configured [`AuditLevel`] and emits
/// structured log lines to stderr.
pub struct EncryptionAuditLogger {
    level: AuditLevel,
    instance_id: String,
}

impl EncryptionAuditLogger {
    /// Create a new audit logger with the given level and instance identifier.
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

    /// Log an audit event if the configured level permits it.
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
        let required_level = match event {
            AuditEvent::KeyLoaded { .. }
            | AuditEvent::KeyAccessDenied { .. } => AuditLevel::KeyEvents,
            AuditEvent::EncryptOperation { .. } | AuditEvent::DecryptOperation { .. } => {
                AuditLevel::AllOperations
            }
        };

        if self.level < required_level {
            return;
        }

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        match event {
            AuditEvent::KeyLoaded {
                provider,
                key_version,
            } => {
                eprintln!(
                    "[AUDIT] ts={timestamp} instance={} event=key.loaded provider={provider} version={key_version}",
                    self.instance_id
                );
            }
            AuditEvent::KeyAccessDenied { provider, error } => {
                eprintln!(
                    "[AUDIT] ts={timestamp} instance={} event=key.access.denied provider={provider} error={error}",
                    self.instance_id
                );
            }
            AuditEvent::EncryptOperation {
                component,
                key_version,
            } => {
                eprintln!(
                    "[AUDIT] ts={timestamp} instance={} event=encrypt component={component} version={key_version}",
                    self.instance_id
                );
            }
            AuditEvent::DecryptOperation {
                component,
                key_version,
            } => {
                eprintln!(
                    "[AUDIT] ts={timestamp} instance={} event=decrypt component={component} version={key_version}",
                    self.instance_id
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_logger_does_not_panic() {
        let logger = EncryptionAuditLogger::disabled();
        assert_eq!(logger.level(), AuditLevel::None);

        // Log all event variants -- none should panic.
        logger.log(&AuditEvent::KeyLoaded {
            provider: "file".into(),
            key_version: 1,
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

        // Key events should be logged (no panic, just stderr output).
        logger.log(&AuditEvent::KeyLoaded {
            provider: "file".into(),
            key_version: 1,
        });

        // Operation-level events are filtered out (still no panic).
        logger.log(&AuditEvent::EncryptOperation {
            component: "wal".into(),
            key_version: 1,
        });
        logger.log(&AuditEvent::DecryptOperation {
            component: "cold".into(),
            key_version: 1,
        });
    }

    #[test]
    fn all_operations_logs_everything() {
        let logger = EncryptionAuditLogger::new(AuditLevel::AllOperations, "test-node");
        assert_eq!(logger.level(), AuditLevel::AllOperations);

        // All event types should be accepted (no panic).
        logger.log(&AuditEvent::KeyLoaded {
            provider: "file".into(),
            key_version: 1,
        });
        logger.log(&AuditEvent::EncryptOperation {
            component: "wal".into(),
            key_version: 1,
        });
        logger.log(&AuditEvent::DecryptOperation {
            component: "index".into(),
            key_version: 2,
        });
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
        let event = AuditEvent::KeyLoaded {
            provider: "file".into(),
            key_version: 1,
        };
        let cloned = event.clone();
        // Debug output should contain the variant name.
        let debug = format!("{cloned:?}");
        assert!(debug.contains("KeyLoaded"));
    }
}
