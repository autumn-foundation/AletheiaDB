//! Serde model for the audit-export artifact — the published, compatibility
//! bearing wire format (Issue #3358).
//!
//! The JSON shape here is what a third party parses; the SIGNATURE, however, is
//! computed over a separate deterministic *canonical binary encoding* of these
//! same typed values (see [`super::canonical`]) so that benign JSON reformatting
//! never breaks a signature while any change to signed content always does.
//!
//! Floats are stored as strings (see [`float_string`]) so the artifact can carry
//! `NaN`/`Infinity` faithfully (JSON numbers cannot) and so their textual form is
//! unambiguous for an independent verifier.

use serde::{Deserialize, Serialize};

/// Format version of the artifact. Bumped only on breaking format changes.
pub const AUDIT_FORMAT_VERSION: u32 = 1;

/// Hash-chain algorithm identifier embedded in the artifact.
pub const HASH_ALGORITHM: &str = "sha256-linked-v1";

/// Signature algorithm identifier embedded in the artifact.
pub const SIGNATURE_ALGORITHM: &str = "ed25519";

/// Canonical description of what the signature covers. Checked verbatim during
/// verification so no descriptive field escapes tamper detection.
pub const SIGNED_DESCRIPTION: &str = "ed25519 detached signature over chain.root";

/// A complete signed audit export of one or more entities' bi-temporal history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditExport {
    /// Artifact format version.
    pub format_version: u32,
    /// Export-level metadata (identity, time, scope, anchor, redaction).
    pub metadata: ExportMetadata,
    /// The exported entities, each with its full version history.
    pub entities: Vec<ExportedEntity>,
    /// Integrity proof: per-version leaf hashes and the chain root.
    pub chain: ChainProof,
    /// Detached signature over the chain root.
    pub signature: SignatureBlock,
}

/// Export-level metadata (AC1 "export's own metadata").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportMetadata {
    /// Operator-supplied database identity.
    pub database_id: String,
    /// RFC 3339 wallclock time the export was produced (UTC, `Z`).
    pub exported_at: String,
    /// Version of the AletheiaDB build that produced the export.
    pub tool_version: String,
    /// Human-readable one-line description of the scope.
    pub scope_description: String,
    /// Structured scope record: what was requested vs actually included.
    pub scope: ScopeRecord,
    /// Anchor to the database's write position at export time (chain head).
    pub chain_anchor: ChainAnchor,
    /// Property keys that were redacted at export (AC6), sorted, de-duplicated.
    pub redacted_keys: Vec<String>,
    /// Number of entities included.
    pub entity_count: usize,
    /// Total number of versions across all included entities.
    pub version_count: usize,
    /// Hash-chain algorithm identifier.
    pub hash_algorithm: String,
    /// Signature algorithm identifier.
    pub signature_algorithm: String,
}

/// Structured record of the requested scope and the resolved membership so that
/// absence of data is never ambiguous (AC5).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeRecord {
    /// `single_node` | `single_edge` | `entity_set` | `neighborhood`.
    pub kind: String,
    /// Entities the caller explicitly asked for (empty for pure neighborhood).
    pub requested: Vec<EntityIdRecord>,
    /// Entities actually included in this artifact.
    pub included: Vec<EntityIdRecord>,
    /// Requested entities that could NOT be included, each with a reason.
    pub not_included: Vec<NotIncludedRecord>,
    /// Bi-temporal coordinate the neighborhood was resolved at (if any).
    pub coordinate: Option<CoordinateRecord>,
    /// Neighborhood hop radius (if any).
    pub hops: Option<u32>,
}

/// A `(kind, id)` reference to a node or edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntityIdRecord {
    /// `node` | `edge`.
    pub kind: String,
    /// Numeric entity id.
    pub id: u64,
}

/// A requested entity that was omitted, with the reason it was omitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotIncludedRecord {
    /// `node` | `edge`.
    pub kind: String,
    /// Numeric entity id.
    pub id: u64,
    /// Why it was not included (e.g. `no_history`).
    pub reason: String,
}

/// A bi-temporal coordinate, both dimensions optional and RFC 3339 encoded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoordinateRecord {
    /// Valid-time coordinate (RFC 3339) if pinned.
    pub valid_time: Option<String>,
    /// Transaction-time coordinate (RFC 3339) if pinned.
    pub transaction_time: Option<String>,
}

/// Anchor tying the artifact to the database's write position at export time.
///
/// Records the current WAL LSN as the "chain head position" (AC1/AC2). When the
/// global tamper-evident hash chain (#3351) lands, the per-version leaves here
/// can additionally be anchored to that chain's head — documented forward-compat.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainAnchor {
    /// WAL log-sequence number at export time (the chain-head position).
    pub source_lsn: u64,
    /// Human description of what the anchor references.
    pub description: String,
}

/// One entity (node or edge) and its complete version history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedEntity {
    /// `node` | `edge`.
    pub kind: String,
    /// Numeric entity id.
    pub id: u64,
    /// Current/last-known label (informational).
    pub label: Option<String>,
    /// Source node id (edges only).
    pub source: Option<u64>,
    /// Target node id (edges only).
    pub target: Option<u64>,
    /// Every version, oldest first (incl. superseded versions + tombstones).
    pub versions: Vec<ExportedVersion>,
}

/// A single bi-temporal version of an entity (AC1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedVersion {
    /// Sequential version number (1-based).
    pub version_number: u64,
    /// Internal version id.
    pub version_id: u64,
    /// Entity label at this version.
    pub label: String,
    /// Valid-time start (microseconds since epoch).
    pub valid_from_micros: i64,
    /// Valid-time start logical counter (HLC).
    pub valid_from_logical: u32,
    /// Valid-time end (microseconds), `None` = open-ended.
    pub valid_to_micros: Option<i64>,
    /// Valid-time end logical counter, `None` = open-ended.
    pub valid_to_logical: Option<u32>,
    /// Transaction-time start (microseconds).
    pub transaction_from_micros: i64,
    /// Transaction-time start logical counter.
    pub transaction_from_logical: u32,
    /// Transaction-time end (microseconds), `None` = open-ended.
    pub transaction_to_micros: Option<i64>,
    /// Transaction-time end logical counter, `None` = open-ended.
    pub transaction_to_logical: Option<u32>,
    /// Whether this version is the current one (both intervals open at now).
    pub is_current: bool,
    /// Per-version provenance, if any (`None` = none recorded — never fabricated).
    pub provenance: Option<ExportedProvenance>,
    /// Properties at this version, sorted by key; redacted keys carry no value.
    pub properties: Vec<ExportedProperty>,
}

/// Write-time provenance surfaced verbatim from the version (Issue #3224/#3421).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedProvenance {
    /// Declared source system/identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Writer confidence in `[0,1]` (string-encoded float).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[serde(with = "float_string_opt")]
    pub confidence: Option<f64>,
    /// Free-text note / reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Correlation id grouping writes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Authenticated principal that made the write (surfaced where present).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

/// One property key/value at a version, or a redaction marker (AC6).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedProperty {
    /// Property key.
    pub key: String,
    /// Value, or `None` when redacted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<ExportedValue>,
    /// True when this key was redacted at export; value is then absent.
    #[serde(default, skip_serializing_if = "is_false")]
    pub redacted: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A property value in an unambiguous, self-describing, serializable form.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExportedValue {
    /// SQL-style null.
    Null,
    /// Boolean.
    Bool {
        /// The boolean.
        value: bool,
    },
    /// 64-bit signed integer.
    Int {
        /// The integer.
        value: i64,
    },
    /// 64-bit float (string-encoded, `NaN`/`inf` safe).
    Float {
        /// The float, string-encoded.
        #[serde(with = "float_string")]
        value: f64,
    },
    /// UTF-8 string.
    String {
        /// The string.
        value: String,
    },
    /// Raw bytes (lowercase hex).
    Bytes {
        /// Hex-encoded bytes.
        hex: String,
    },
    /// Heterogeneous array.
    Array {
        /// The elements.
        values: Vec<ExportedValue>,
    },
    /// Dense float vector (each element string-encoded).
    Vector {
        /// The vector elements, string-encoded floats.
        values: Vec<String>,
    },
    /// Sparse float vector.
    SparseVector {
        /// Full dimensionality.
        dimension: usize,
        /// Non-zero indices.
        indices: Vec<u32>,
        /// Non-zero values, string-encoded floats.
        values: Vec<String>,
    },
}

/// Integrity proof: per-version leaf hashes and the chain root (AC2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainProof {
    /// Algorithm identifier (`sha256-linked-v1`).
    pub algorithm: String,
    /// Per-version leaf hashes (hex), in flattened entity→version order.
    pub leaves: Vec<String>,
    /// Chain root (hex) that the signature covers.
    pub root: String,
}

/// Detached signature over the chain root (AC2/AC3).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureBlock {
    /// Algorithm identifier (`ed25519`).
    pub algorithm: String,
    /// Signer's Ed25519 public key (hex, 32 bytes).
    pub public_key: String,
    /// Detached signature (hex, 64 bytes) over the chain root bytes.
    pub signature: String,
    /// What was signed, for an independent implementer.
    pub signed: String,
}

/// Render a finite/NaN/Inf `f64` to a lossless, parseable token.
pub(crate) fn f64_to_token(v: f64) -> String {
    if v.is_nan() {
        "NaN".to_string()
    } else if v.is_infinite() {
        if v.is_sign_positive() {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        }
    } else {
        // `{:?}` yields the shortest round-tripping decimal for f64.
        format!("{v:?}")
    }
}

/// Parse a token produced by [`f64_to_token`] back to an `f64`.
pub(crate) fn token_to_f64(s: &str) -> Option<f64> {
    match s {
        "NaN" => Some(f64::NAN),
        "Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        other => other.parse::<f64>().ok(),
    }
}

/// Serde adapter: `f64` <-> lossless string token.
pub(crate) mod float_string {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::f64_to_token(*v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        let s = String::deserialize(d)?;
        super::token_to_f64(&s).ok_or_else(|| serde::de::Error::custom("invalid float token"))
    }
}

/// Serde adapter for `Option<f64>` as a string token.
pub(crate) mod float_string_opt {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(f) => s.serialize_str(&super::f64_to_token(*f)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
        let opt = Option::<String>::deserialize(d)?;
        match opt {
            Some(s) => super::token_to_f64(&s)
                .map(Some)
                .ok_or_else(|| serde::de::Error::custom("invalid float token")),
            None => Ok(None),
        }
    }
}
