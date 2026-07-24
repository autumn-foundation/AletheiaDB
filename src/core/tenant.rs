//! Multi-tenant isolation — core model (Issue #3365).
//!
//! A **tenant** is a fully separate logical database served from one process:
//! its own nodes, edges, bi-temporal history, vector indexes, constraints, and
//! schema. IDs and business keys may collide across tenants without
//! interference. This is a **hard isolation boundary** — deliberately unlike the
//! cooperative agent-scoped namespaces of Issue #3349, which explicitly are *not*
//! a security or resource boundary.
//!
//! This module holds the surface-independent value types — the tenant
//! identifier, resource quota, usage snapshot, public info, and the structured
//! error taxonomy. The runtime that owns one [`AletheiaDB`](crate::db::AletheiaDB)
//! per tenant and enforces the quotas lives in [`crate::tenant`].
//!
//! # The single-tenant default
//!
//! Nothing in the engine requires a tenant. `AletheiaDB::new()` / `open()` remain
//! a single, unpartitioned logical database with zero tenant overhead; the
//! [`TenantManager`](crate::tenant::TenantManager) is an additive layer a hosted
//! deployment opts into.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum length of a tenant identifier, in bytes.
pub const MAX_TENANT_ID_LEN: usize = 128;

/// Windows reserved device names (case-insensitive). A durable tenant id maps
/// to a directory name, so an id whose stem (the part before the first `.`)
/// matches one of these is rejected — Windows cannot create such a directory.
/// We only need to check lowercase forms since the charset is lowercase-only.
const WINDOWS_RESERVED_STEMS: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A validated tenant identifier.
///
/// A tenant id doubles as an on-disk directory name for a durable deployment, so
/// it is deliberately **filesystem-safe and collision-free across every
/// filesystem**, and narrower than a
/// [`Namespace`](crate::core::namespace::Namespace):
///
/// - charset `[a-z0-9._-]` — **lowercase only**, so two ids can never collide on
///   a case-insensitive/normalizing filesystem (APFS, NTFS): distinct ids always
///   map to distinct directories. This is why `Acme` and `acme` cannot both
///   exist — the uppercase form is rejected outright.
/// - never a path separator, and never the traversal tokens `.` / `..`.
/// - never a leading/trailing `.` (Windows strips trailing dots, collapsing
///   `foo.` and `foo` to the same directory).
/// - never a Windows reserved device-name stem (`con`, `nul`, `com1`, …).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct TenantId(String);

impl TenantId {
    /// Validate and construct a tenant id.
    ///
    /// # Errors
    ///
    /// [`TenantError::InvalidId`] (→ MCP `INVALID_ARGUMENT`, carrying the
    /// offending value) when `id` is empty, exceeds [`MAX_TENANT_ID_LEN`] bytes,
    /// is a path-traversal token, has a leading/trailing `.`, is a Windows
    /// reserved device name, or contains a character outside `[a-z0-9._-]`.
    pub fn new(id: impl Into<String>) -> Result<Self, TenantError> {
        let id = id.into();
        let invalid = |reason: String, id: String| TenantError::InvalidId { id, reason };
        if id.is_empty() {
            return Err(invalid("tenant id must not be empty".to_string(), id));
        }
        if id.len() > MAX_TENANT_ID_LEN {
            return Err(invalid(
                format!(
                    "tenant id must be at most {MAX_TENANT_ID_LEN} bytes (got {})",
                    id.len()
                ),
                id,
            ));
        }
        // Reject the path-traversal tokens explicitly: they pass the charset
        // check below but would escape the per-tenant data directory.
        if id == "." || id == ".." {
            return Err(invalid(
                "tenant id must not be a path-traversal token ('.' or '..')".to_string(),
                id,
            ));
        }
        // Lowercase-only charset — the key defense against case-insensitive-FS
        // directory collisions (`Acme` vs `acme` sharing one WAL/index dir).
        if let Some(bad) = id
            .chars()
            .find(|c| !matches!(c, 'a'..='z' | '0'..='9' | '.' | '_' | '-'))
        {
            let hint = if bad.is_ascii_uppercase() {
                "; tenant ids are lowercase-only to avoid case-insensitive-filesystem collisions"
            } else {
                ""
            };
            return Err(invalid(
                format!(
                    "tenant id contains invalid character {bad:?}; allowed charset is \
                     [a-z0-9._-] (no path separators){hint}"
                ),
                id,
            ));
        }
        // A leading/trailing '.' collapses to a same-directory collision on
        // Windows (trailing-dot stripping) and is confusing everywhere.
        if id.starts_with('.') || id.ends_with('.') {
            return Err(invalid(
                "tenant id must not begin or end with '.'".to_string(),
                id,
            ));
        }
        // Reject Windows reserved device-name stems (the part before the first
        // '.'), which cannot be a directory on Windows.
        let stem = id.split('.').next().unwrap_or(&id);
        if WINDOWS_RESERVED_STEMS.contains(&stem) {
            return Err(invalid(
                format!("tenant id '{id}' uses a reserved device name ('{stem}')"),
                id,
            ));
        }
        Ok(TenantId(id))
    }

    /// The tenant id as a string slice.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume into the owned string.
    #[inline]
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for TenantId {
    type Err = TenantError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        TenantId::new(s)
    }
}

/// The resource dimensions a per-tenant quota can bound (Issue #3365).
///
/// Used both as the machine-readable `dimension` in a
/// [`TenantError::QuotaExceeded`] and to key [`TenantUsage`] against a
/// [`TenantQuota`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum QuotaDimension {
    /// Maximum number of current-state nodes.
    Nodes,
    /// Maximum number of current-state edges.
    Edges,
    /// Maximum estimated vector-index memory, in bytes.
    VectorIndexBytes,
    /// Maximum estimated total storage, in bytes.
    StorageBytes,
}

impl QuotaDimension {
    /// The stable wire token for this dimension.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            QuotaDimension::Nodes => "nodes",
            QuotaDimension::Edges => "edges",
            QuotaDimension::VectorIndexBytes => "vector_index_bytes",
            QuotaDimension::StorageBytes => "storage_bytes",
        }
    }
}

impl std::fmt::Display for QuotaDimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-tenant capacity limits (Issue #3365).
///
/// Every dimension is `Option<u64>`; `None` means **unlimited** on that axis.
/// The default ([`TenantQuota::unlimited`]) leaves every dimension unset, so a
/// tenant created without an explicit quota behaves exactly like an
/// un-partitioned database.
///
/// Node- and edge-count limits are enforced **precisely** (an atomic reservation
/// taken before the write, released on failure). The two byte dimensions are
/// enforced **best-effort** against O(1) estimators in v1 (see
/// [`TenantUsage`]); the exact accounting is a documented follow-up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TenantQuota {
    /// Maximum number of current-state nodes, or `None` for unlimited.
    #[cfg_attr(feature = "serde", serde(default))]
    pub max_nodes: Option<u64>,
    /// Maximum number of current-state edges, or `None` for unlimited.
    #[cfg_attr(feature = "serde", serde(default))]
    pub max_edges: Option<u64>,
    /// Maximum estimated vector-index memory in bytes, or `None` for unlimited.
    #[cfg_attr(feature = "serde", serde(default))]
    pub max_vector_index_bytes: Option<u64>,
    /// Maximum estimated total storage in bytes, or `None` for unlimited.
    #[cfg_attr(feature = "serde", serde(default))]
    pub max_storage_bytes: Option<u64>,
}

impl TenantQuota {
    /// An unlimited quota (every dimension unset). Equivalent to
    /// [`TenantQuota::default`].
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            max_nodes: None,
            max_edges: None,
            max_vector_index_bytes: None,
            max_storage_bytes: None,
        }
    }

    /// Builder-style setter for the node-count limit.
    #[must_use]
    pub const fn with_max_nodes(mut self, n: u64) -> Self {
        self.max_nodes = Some(n);
        self
    }

    /// Builder-style setter for the edge-count limit.
    #[must_use]
    pub const fn with_max_edges(mut self, n: u64) -> Self {
        self.max_edges = Some(n);
        self
    }

    /// Builder-style setter for the vector-index-memory limit (bytes).
    #[must_use]
    pub const fn with_max_vector_index_bytes(mut self, n: u64) -> Self {
        self.max_vector_index_bytes = Some(n);
        self
    }

    /// Builder-style setter for the total-storage limit (bytes).
    #[must_use]
    pub const fn with_max_storage_bytes(mut self, n: u64) -> Self {
        self.max_storage_bytes = Some(n);
        self
    }

    /// The configured limit for a given dimension, or `None` if unlimited.
    #[must_use]
    pub const fn limit_for(&self, dim: QuotaDimension) -> Option<u64> {
        match dim {
            QuotaDimension::Nodes => self.max_nodes,
            QuotaDimension::Edges => self.max_edges,
            QuotaDimension::VectorIndexBytes => self.max_vector_index_bytes,
            QuotaDimension::StorageBytes => self.max_storage_bytes,
        }
    }
}

/// A point-in-time snapshot of one tenant's resource consumption (Issue #3365).
///
/// Every field is an O(1)/cached counter read, suitable for metering — it never
/// triggers a version scan (consistent with the `database_stats` approach of
/// Issue #3222). Node and edge counts are exact; the two byte fields are
/// best-effort estimates in v1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
#[non_exhaustive]
pub struct TenantUsage {
    /// Current-state node count (exact).
    pub node_count: u64,
    /// Current-state edge count (exact).
    pub edge_count: u64,
    /// Estimated vector-index memory, in bytes (best-effort in v1).
    pub vector_index_bytes: u64,
    /// Estimated total storage, in bytes (best-effort in v1).
    pub storage_bytes: u64,
}

impl TenantUsage {
    /// The consumed amount on a given dimension.
    #[must_use]
    pub const fn used(&self, dim: QuotaDimension) -> u64 {
        match dim {
            QuotaDimension::Nodes => self.node_count,
            QuotaDimension::Edges => self.edge_count,
            QuotaDimension::VectorIndexBytes => self.vector_index_bytes,
            QuotaDimension::StorageBytes => self.storage_bytes,
        }
    }
}

/// Public, serializable description of a registered tenant (Issue #3365).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
#[non_exhaustive]
pub struct TenantInfo {
    /// The tenant identifier.
    pub id: TenantId,
    /// When the tenant was created (transaction-time wallclock microseconds).
    pub created_at_micros: i64,
    /// The tenant's current resource quota.
    pub quota: TenantQuota,
}

/// Structured errors for the tenant lifecycle and quota enforcement (Issue
/// #3365), mapped to the #3234 MCP/HTTP error codes at the surface boundary.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TenantError {
    /// The tenant id is malformed. → `INVALID_ARGUMENT`.
    #[error("invalid tenant id {id:?}: {reason}")]
    InvalidId {
        /// The offending id.
        id: String,
        /// Why it was rejected.
        reason: String,
    },
    /// No tenant with the given id exists. → `NOT_FOUND`.
    #[error("tenant not found: {id}")]
    NotFound {
        /// The id that was looked up.
        id: String,
    },
    /// A tenant with the given id already exists. → `CONFLICT`.
    #[error("tenant already exists: {id}")]
    AlreadyExists {
        /// The conflicting id.
        id: String,
    },
    /// A write would push a tenant past a configured quota. → `RESOURCE_EXHAUSTED`
    /// (non-retriable: a capacity limit heals only by freeing data or raising the
    /// quota, never by retrying the same call). The write is rejected atomically —
    /// **never** a partial write.
    #[error(
        "tenant '{tenant}' quota exceeded on {dimension}: current={current}, limit={limit} \
         (the write was rejected; no partial write occurred)"
    )]
    QuotaExceeded {
        /// The tenant whose quota was hit.
        tenant: String,
        /// Which dimension was exceeded.
        dimension: QuotaDimension,
        /// The tenant's usage on that dimension before the rejected write.
        current: u64,
        /// The configured limit on that dimension.
        limit: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ids_accepted() {
        for id in [
            "acme",
            "team-42",
            "cust_9",
            "a.b-c_d",
            "a1",
            &"x".repeat(128),
        ] {
            assert!(TenantId::new(id).is_ok(), "{id} should be valid");
        }
    }

    #[test]
    fn invalid_ids_rejected() {
        for bad in [
            "",
            "has space",
            "slash/here",
            "colon:here",
            ".",
            "..",
            "tab\tx",
            // Uppercase is rejected: lowercase-only prevents case-insensitive-FS
            // directory collisions ("Acme" vs "acme").
            "Acme",
            "A1",
            // Leading/trailing dot collapses on Windows.
            ".hidden",
            "trailing.",
            // Windows reserved device names (and with an extension).
            "con",
            "nul",
            "com1",
            "lpt9",
            "con.log",
        ] {
            assert!(
                matches!(TenantId::new(bad), Err(TenantError::InvalidId { .. })),
                "{bad:?} should be rejected"
            );
        }
        // Over the length cap.
        assert!(matches!(
            TenantId::new("z".repeat(129)),
            Err(TenantError::InvalidId { .. })
        ));
    }

    #[test]
    fn case_variant_ids_cannot_both_be_valid() {
        // The lowercase-only rule makes a case-insensitive-FS directory collision
        // impossible by construction: at most one of a case-variant pair is a
        // valid id at all.
        assert!(TenantId::new("acme").is_ok());
        assert!(TenantId::new("Acme").is_err());
        assert!(TenantId::new("ACME").is_err());
    }

    #[test]
    fn invalid_id_carries_offending_value() {
        match TenantId::new("bad/id") {
            Err(TenantError::InvalidId { id, .. }) => assert_eq!(id, "bad/id"),
            other => panic!("expected InvalidId, got {other:?}"),
        }
    }

    #[test]
    fn quota_builder_and_lookup() {
        let q = TenantQuota::unlimited()
            .with_max_nodes(10)
            .with_max_edges(20)
            .with_max_vector_index_bytes(1024)
            .with_max_storage_bytes(4096);
        assert_eq!(q.limit_for(QuotaDimension::Nodes), Some(10));
        assert_eq!(q.limit_for(QuotaDimension::Edges), Some(20));
        assert_eq!(q.limit_for(QuotaDimension::VectorIndexBytes), Some(1024));
        assert_eq!(q.limit_for(QuotaDimension::StorageBytes), Some(4096));
        assert_eq!(
            TenantQuota::default().limit_for(QuotaDimension::Nodes),
            None
        );
    }

    #[test]
    fn usage_used_lookup() {
        let u = TenantUsage {
            node_count: 3,
            edge_count: 5,
            vector_index_bytes: 7,
            storage_bytes: 9,
        };
        assert_eq!(u.used(QuotaDimension::Nodes), 3);
        assert_eq!(u.used(QuotaDimension::Edges), 5);
        assert_eq!(u.used(QuotaDimension::VectorIndexBytes), 7);
        assert_eq!(u.used(QuotaDimension::StorageBytes), 9);
    }

    #[test]
    fn dimension_wire_tokens_stable() {
        assert_eq!(QuotaDimension::Nodes.as_str(), "nodes");
        assert_eq!(QuotaDimension::Edges.as_str(), "edges");
        assert_eq!(
            QuotaDimension::VectorIndexBytes.as_str(),
            "vector_index_bytes"
        );
        assert_eq!(QuotaDimension::StorageBytes.as_str(), "storage_bytes");
    }
}
