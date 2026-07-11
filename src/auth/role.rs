//! Roles and access classes for RBAC (Issue #3350).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// The category of access an operation requires.
///
/// Every API surface operation is classified into exactly one access class;
/// [`Role::allows`] decides whether a role may perform it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessClass {
    /// Read the graph (lookups, traversals, read-only queries).
    Read,
    /// Mutate the graph (create/update/delete, mutating queries).
    Write,
    /// Manage credentials and server administration.
    Admin,
    /// Liveness/health/metrics probes.
    Metrics,
}

impl fmt::Display for AccessClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read => f.write_str("read"),
            Self::Write => f.write_str("write"),
            Self::Admin => f.write_str("admin"),
            Self::Metrics => f.write_str("metrics"),
        }
    }
}

/// Error returned when parsing an unknown role string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown role '{0}': expected one of admin, writer, reader, metrics")]
pub struct RoleParseError(pub String);

/// A principal's role, deciding which [`AccessClass`]es it may exercise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Full access: read, write, admin, metrics.
    Admin,
    /// Read + write + metrics; no credential management.
    Writer,
    /// Read + metrics only.
    Reader,
    /// Metrics only (health probes, monitoring agents).
    Metrics,
}

impl Role {
    /// Whether this role may exercise the given access class.
    ///
    /// | Role    | Read | Write | Admin | Metrics |
    /// |---------|------|-------|-------|---------|
    /// | Admin   | yes  | yes   | yes   | yes     |
    /// | Writer  | yes  | yes   | no    | yes     |
    /// | Reader  | yes  | no    | no    | yes     |
    /// | Metrics | no   | no    | no    | yes     |
    #[must_use]
    pub fn allows(self, class: AccessClass) -> bool {
        match self {
            Self::Admin => true,
            Self::Writer => matches!(
                class,
                AccessClass::Read | AccessClass::Write | AccessClass::Metrics
            ),
            Self::Reader => matches!(class, AccessClass::Read | AccessClass::Metrics),
            Self::Metrics => matches!(class, AccessClass::Metrics),
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Admin => f.write_str("admin"),
            Self::Writer => f.write_str("writer"),
            Self::Reader => f.write_str("reader"),
            Self::Metrics => f.write_str("metrics"),
        }
    }
}

impl FromStr for Role {
    type Err = RoleParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "admin" => Ok(Self::Admin),
            "writer" => Ok(Self::Writer),
            "reader" => Ok(Self::Reader),
            "metrics" => Ok(Self::Metrics),
            other => Err(RoleParseError(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full 4x4 truth table for the role/permission matrix.
    #[test]
    fn role_permission_matrix_truth_table() {
        use AccessClass::{Admin, Metrics, Read, Write};

        let cases: &[(Role, AccessClass, bool)] = &[
            (Role::Admin, Read, true),
            (Role::Admin, Write, true),
            (Role::Admin, Admin, true),
            (Role::Admin, Metrics, true),
            (Role::Writer, Read, true),
            (Role::Writer, Write, true),
            (Role::Writer, Admin, false),
            (Role::Writer, Metrics, true),
            (Role::Reader, Read, true),
            (Role::Reader, Write, false),
            (Role::Reader, Admin, false),
            (Role::Reader, Metrics, true),
            (Role::Metrics, Read, false),
            (Role::Metrics, Write, false),
            (Role::Metrics, Admin, false),
            (Role::Metrics, Metrics, true),
        ];
        for &(role, class, expected) in cases {
            assert_eq!(
                role.allows(class),
                expected,
                "{role}.allows({class}) should be {expected}"
            );
        }
    }

    #[test]
    fn role_from_str_accepts_known_roles_case_insensitively() {
        assert_eq!("admin".parse::<Role>(), Ok(Role::Admin));
        assert_eq!("writer".parse::<Role>(), Ok(Role::Writer));
        assert_eq!("reader".parse::<Role>(), Ok(Role::Reader));
        assert_eq!("metrics".parse::<Role>(), Ok(Role::Metrics));
        assert_eq!("ADMIN".parse::<Role>(), Ok(Role::Admin));
        assert_eq!(" Reader ".parse::<Role>(), Ok(Role::Reader));
    }

    #[test]
    fn role_from_str_rejects_unknown_roles() {
        assert!("root".parse::<Role>().is_err());
        assert!("".parse::<Role>().is_err());
        assert!("read-only".parse::<Role>().is_err());
        let err = "superuser".parse::<Role>().unwrap_err();
        assert!(err.to_string().contains("superuser"));
    }

    #[test]
    fn role_serde_lowercase() {
        assert_eq!(serde_json::to_string(&Role::Reader).unwrap(), "\"reader\"");
        assert_eq!(serde_json::to_string(&Role::Admin).unwrap(), "\"admin\"");
        let parsed: Role = serde_json::from_str("\"writer\"").unwrap();
        assert_eq!(parsed, Role::Writer);
        // Unknown role strings are rejected by serde too.
        assert!(serde_json::from_str::<Role>("\"root\"").is_err());
    }

    #[test]
    fn role_display_lowercase() {
        assert_eq!(Role::Admin.to_string(), "admin");
        assert_eq!(Role::Writer.to_string(), "writer");
        assert_eq!(Role::Reader.to_string(), "reader");
        assert_eq!(Role::Metrics.to_string(), "metrics");
    }

    #[test]
    fn access_class_display_lowercase() {
        assert_eq!(AccessClass::Read.to_string(), "read");
        assert_eq!(AccessClass::Write.to_string(), "write");
        assert_eq!(AccessClass::Admin.to_string(), "admin");
        assert_eq!(AccessClass::Metrics.to_string(), "metrics");
    }
}
