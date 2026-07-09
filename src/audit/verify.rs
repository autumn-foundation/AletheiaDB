//! Offline verification + human rendering of a signed audit export (Issue #3358).
//!
//! Everything here runs with NO database and NO network — only the artifact
//! bytes and (optionally) the signer's trusted public key. This is also the
//! reference implementation a third party can port to independently verify the
//! documented format.

use crate::audit::canonical;
use crate::audit::error::{AuditError, AuditResult};
use crate::audit::model::{
    AUDIT_FORMAT_VERSION, AuditExport, ExportMetadata, ExportedValue, HASH_ALGORITHM,
    SIGNATURE_ALGORITHM, SIGNED_DESCRIPTION,
};
use crate::audit::signing::AuditPublicKey;

/// The outcome of a successful verification: a human-facing summary (AC3).
///
/// Only ever produced when verification PASSED; a failed verification returns
/// an [`AuditError`] instead.
#[derive(Debug, Clone)]
pub struct AuditVerification {
    /// Always `true` (a value of this type means verification passed).
    pub passed: bool,
    /// Whether the signature was checked against a caller-supplied *trusted*
    /// key (`true`) or only the artifact's self-asserted embedded key (`false`).
    pub trusted_key: bool,
    /// Database identity recorded in the artifact.
    pub database_id: String,
    /// Number of entities included.
    pub entity_count: usize,
    /// Total number of versions verified.
    pub version_count: usize,
    /// `(earliest, latest)` bi-temporal span across versions (RFC 3339), if any.
    pub time_span: Option<(String, String)>,
    /// The verified chain root (hex).
    pub chain_root: String,
    /// The chain-head anchor (WAL LSN) recorded at export time.
    pub chain_anchor_lsn: u64,
    /// Property keys that were redacted at export.
    pub redacted_keys: Vec<String>,
    /// One-line-per-entity summary (kind, id, label).
    pub entity_summary: String,
    /// The signer's public key (hex).
    pub public_key: String,
}

impl AuditVerification {
    /// A compact single-line human summary (entity, versions, span, anchor).
    #[must_use]
    pub fn summary(&self) -> String {
        let span = self
            .time_span
            .as_ref()
            .map(|(a, b)| format!("{a} .. {b}"))
            .unwrap_or_else(|| "n/a".to_string());
        let trust = if self.trusted_key {
            "trusted-key"
        } else {
            "self-asserted-key"
        };
        format!(
            "PASS [{trust}] db={} entities={} versions={} span={} anchor_lsn={} root={}…",
            self.database_id,
            self.entity_count,
            self.version_count,
            span,
            self.chain_anchor_lsn,
            &self.chain_root[..self.chain_root.len().min(12)],
        )
    }
}

impl AuditExport {
    /// Serialize the artifact to pretty JSON bytes (the single-file artifact).
    ///
    /// # Errors
    /// Returns [`AuditError::Serialization`] on an unexpected serializer error.
    pub fn to_json_bytes(&self) -> AuditResult<Vec<u8>> {
        serde_json::to_vec_pretty(self).map_err(|e| AuditError::Serialization(e.to_string()))
    }

    /// Parse an artifact from JSON bytes.
    ///
    /// # Errors
    /// Returns [`AuditError::Serialization`] if the bytes are not a valid
    /// artifact.
    pub fn from_json_bytes(bytes: &[u8]) -> AuditResult<Self> {
        serde_json::from_slice(bytes).map_err(|e| AuditError::Serialization(e.to_string()))
    }

    /// Total number of versions across all included entities.
    #[must_use]
    pub fn version_count(&self) -> usize {
        self.entities.iter().map(|e| e.versions.len()).sum()
    }

    /// Number of entities included.
    #[must_use]
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    /// The export metadata.
    #[must_use]
    pub fn metadata(&self) -> &ExportMetadata {
        &self.metadata
    }

    /// The public key embedded in the artifact.
    ///
    /// # Errors
    /// Returns [`AuditError::InvalidPublicKey`] if the embedded key is malformed.
    pub fn embedded_public_key(&self) -> AuditResult<AuditPublicKey> {
        AuditPublicKey::from_hex(&self.signature.public_key)
    }

    /// Verify this artifact offline (AC2/AC3/AC4).
    ///
    /// If `expected` is supplied, the signature is checked against that trusted
    /// key AND the embedded key must equal it (defeating public-key
    /// substitution). If `expected` is `None`, the embedded key is used and the
    /// result is marked `trusted_key = false` (trust is self-asserted).
    ///
    /// # Errors
    /// Returns a specific [`AuditError`] variant for each failure mode:
    /// content tamper, chain-root mismatch, completeness mismatch, key
    /// mismatch, or signature failure.
    pub fn verify(&self, expected: Option<&AuditPublicKey>) -> AuditResult<AuditVerification> {
        if self.format_version != AUDIT_FORMAT_VERSION {
            return Err(AuditError::UnsupportedFormat {
                found: self.format_version,
                expected: AUDIT_FORMAT_VERSION,
            });
        }

        // Descriptive/algorithm labels are not folded into the chain root, so
        // pin them to the known constants here — otherwise a byte flipped in one
        // of these fields would escape tamper detection. (An unknown algorithm
        // is also a legitimate reason to refuse an artifact this build cannot
        // interpret.)
        if self.chain.algorithm != HASH_ALGORITHM {
            return Err(AuditError::ContentTampered(format!(
                "unexpected chain algorithm '{}'",
                self.chain.algorithm
            )));
        }
        if self.signature.algorithm != SIGNATURE_ALGORITHM {
            return Err(AuditError::ContentTampered(format!(
                "unexpected signature algorithm '{}'",
                self.signature.algorithm
            )));
        }
        if self.signature.signed != SIGNED_DESCRIPTION {
            return Err(AuditError::ContentTampered(
                "unexpected signature description".to_string(),
            ));
        }

        // 1. Recompute the per-version leaves and the chain root from content.
        let (leaves, root) = canonical::compute_chain(&self.metadata, &self.entities);

        // 2. The stored per-version leaves must match exactly (precise per-version
        //    tamper detection, and detects added/removed leaves).
        if self.chain.leaves.len() != leaves.len() {
            return Err(AuditError::CompletenessMismatch(format!(
                "artifact declares {} leaves but content yields {}",
                self.chain.leaves.len(),
                leaves.len()
            )));
        }
        for (i, (stored, computed)) in self.chain.leaves.iter().zip(leaves.iter()).enumerate() {
            let stored = canonical::hex_to_32(stored).ok_or_else(|| {
                AuditError::ContentTampered(format!("leaf {i} is not a valid hash"))
            })?;
            if &stored != computed {
                return Err(AuditError::ContentTampered(format!(
                    "version leaf {i} does not match its content"
                )));
            }
        }

        // 3. The recomputed root must match the stored root.
        let stored_root = canonical::hex_to_32(&self.chain.root)
            .ok_or_else(|| AuditError::ContentTampered("chain root is not a valid hash".into()))?;
        if stored_root != root {
            return Err(AuditError::ChainRootMismatch);
        }

        // 4. Completeness: declared counts must match actual content.
        let actual_versions = self.version_count();
        if self.metadata.version_count != actual_versions {
            return Err(AuditError::CompletenessMismatch(format!(
                "metadata declares {} versions but {} are present",
                self.metadata.version_count, actual_versions
            )));
        }
        if self.metadata.entity_count != self.entities.len() {
            return Err(AuditError::CompletenessMismatch(format!(
                "metadata declares {} entities but {} are present",
                self.metadata.entity_count,
                self.entities.len()
            )));
        }

        // 5. Resolve the key to verify against and defeat key substitution.
        let embedded = self.embedded_public_key()?;
        let (verifying_key, trusted_key) = match expected {
            Some(trusted) => {
                if trusted.to_hex() != embedded.to_hex() {
                    return Err(AuditError::KeyMismatch);
                }
                (trusted.clone(), true)
            }
            None => (embedded, false),
        };

        // 6. Verify the Ed25519 signature over the (recomputed) root.
        let sig_bytes = crate::core::hex::decode(&self.signature.signature).ok_or_else(|| {
            AuditError::SignatureInvalid("signature is not valid hex".to_string())
        })?;
        if sig_bytes.len() != 64 {
            return Err(AuditError::SignatureInvalid(format!(
                "signature must be 64 bytes, got {}",
                sig_bytes.len()
            )));
        }
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&sig_bytes);
        verifying_key.verify_root(&root, &sig)?;

        Ok(AuditVerification {
            passed: true,
            trusted_key,
            database_id: self.metadata.database_id.clone(),
            entity_count: self.entities.len(),
            version_count: actual_versions,
            time_span: self.time_span(),
            chain_root: self.chain.root.clone(),
            chain_anchor_lsn: self.metadata.chain_anchor.source_lsn,
            redacted_keys: self.metadata.redacted_keys.clone(),
            entity_summary: self.entity_summary(),
            public_key: verifying_key.to_hex(),
        })
    }

    /// Compute the `(earliest, latest)` bi-temporal span across all versions.
    fn time_span(&self) -> Option<(String, String)> {
        let mut earliest: Option<i64> = None;
        let mut latest: Option<i64> = None;
        for entity in &self.entities {
            for v in &entity.versions {
                let mut consider = |micros: i64| {
                    earliest = Some(earliest.map_or(micros, |e| e.min(micros)));
                    latest = Some(latest.map_or(micros, |l| l.max(micros)));
                };
                consider(v.valid_from_micros);
                consider(v.transaction_from_micros);
                if let Some(m) = v.valid_to_micros {
                    consider(m);
                }
                if let Some(m) = v.transaction_to_micros {
                    consider(m);
                }
            }
        }
        match (earliest, latest) {
            (Some(a), Some(b)) => Some((format_micros(a), format_micros(b))),
            _ => None,
        }
    }

    fn entity_summary(&self) -> String {
        self.entities
            .iter()
            .map(|e| {
                let label = e.label.as_deref().unwrap_or("");
                format!("{} {} ({label})", e.kind, e.id)
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Render the artifact as a human-readable chronology (AC7).
    ///
    /// This reads only the artifact's own bytes — call [`verify`](Self::verify)
    /// first if you need assurance the bytes are authentic.
    #[must_use]
    pub fn render_chronology(&self) -> String {
        let mut out = String::new();
        out.push_str("=== AletheiaDB Signed Audit Export ===\n");
        out.push_str(&format!("Database:    {}\n", self.metadata.database_id));
        out.push_str(&format!("Exported at: {}\n", self.metadata.exported_at));
        out.push_str(&format!(
            "Scope:       {}\n",
            self.metadata.scope_description
        ));
        out.push_str(&format!(
            "Chain root:  {}\nChain anchor (WAL LSN): {}\n",
            self.chain.root, self.metadata.chain_anchor.source_lsn
        ));
        if !self.metadata.redacted_keys.is_empty() {
            out.push_str(&format!(
                "Redacted keys: {}\n",
                self.metadata.redacted_keys.join(", ")
            ));
        }
        if !self.metadata.scope.not_included.is_empty() {
            out.push_str("Not included:\n");
            for ni in &self.metadata.scope.not_included {
                out.push_str(&format!("  - {} {} ({})\n", ni.kind, ni.id, ni.reason));
            }
        }
        out.push('\n');

        for entity in &self.entities {
            let label = entity.label.as_deref().unwrap_or("");
            out.push_str(&format!(
                "--- {} {} ({label}) — {} version(s) ---\n",
                entity.kind,
                entity.id,
                entity.versions.len()
            ));
            if let (Some(s), Some(t)) = (entity.source, entity.target) {
                out.push_str(&format!("  endpoints: {s} -> {t}\n"));
            }
            for v in &entity.versions {
                let valid_to = v
                    .valid_to_micros
                    .map_or_else(|| "current".to_string(), format_micros);
                let tx_to = v
                    .transaction_to_micros
                    .map_or_else(|| "current".to_string(), format_micros);
                out.push_str(&format!(
                    "  Version {} (id {}){}\n",
                    v.version_number,
                    v.version_id,
                    if v.is_current { " [current]" } else { "" }
                ));
                out.push_str(&format!(
                    "    valid: [{}, {})  recorded: [{}, {})\n",
                    format_micros(v.valid_from_micros),
                    valid_to,
                    format_micros(v.transaction_from_micros),
                    tx_to,
                ));
                if let Some(p) = &v.provenance {
                    let mut parts = Vec::new();
                    if let Some(s) = &p.source {
                        parts.push(format!("source={s}"));
                    }
                    if let Some(c) = p.confidence {
                        parts.push(format!("confidence={c}"));
                    }
                    if let Some(pr) = &p.principal {
                        parts.push(format!("principal={pr}"));
                    }
                    if let Some(n) = &p.note {
                        parts.push(format!("note={n}"));
                    }
                    if !parts.is_empty() {
                        out.push_str(&format!("    Provenance: {}\n", parts.join(", ")));
                    }
                } else {
                    out.push_str("    Provenance: (none recorded)\n");
                }
                for prop in &v.properties {
                    if prop.redacted {
                        out.push_str(&format!("    {} = [REDACTED]\n", prop.key));
                    } else if let Some(val) = &prop.value {
                        out.push_str(&format!("    {} = {}\n", prop.key, render_value(val)));
                    }
                }
            }
            out.push('\n');
        }
        out
    }
}

/// Verify raw artifact bytes offline (AC3 entry point).
///
/// # Errors
/// Returns an [`AuditError`] if the bytes cannot be parsed or verification
/// fails.
pub fn verify_json_bytes(
    bytes: &[u8],
    expected: Option<&AuditPublicKey>,
) -> AuditResult<AuditVerification> {
    let export = AuditExport::from_json_bytes(bytes)?;
    export.verify(expected)
}

/// Compact rendering of a value for the human chronology.
fn render_value(v: &ExportedValue) -> String {
    match v {
        ExportedValue::Null => "null".to_string(),
        ExportedValue::Bool { value } => value.to_string(),
        ExportedValue::Int { value } => value.to_string(),
        ExportedValue::Float { value } => value.to_string(),
        ExportedValue::String { value } => format!("\"{value}\""),
        ExportedValue::Bytes { hex } => format!("0x{hex}"),
        ExportedValue::Array { values } => {
            let inner: Vec<String> = values.iter().map(render_value).collect();
            format!("[{}]", inner.join(", "))
        }
        ExportedValue::Vector { values } => format!("<vector dim={}>", values.len()),
        ExportedValue::SparseVector { dimension, .. } => {
            format!("<sparse_vector dim={dimension}>")
        }
    }
}

/// Format microseconds-since-epoch as RFC 3339 (UTC), or raw micros if out of
/// chrono's representable range.
fn format_micros(micros: i64) -> String {
    use chrono::{DateTime, Utc};
    match DateTime::<Utc>::from_timestamp_micros(micros) {
        Some(dt) => dt.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        None => format!("{micros}us"),
    }
}
