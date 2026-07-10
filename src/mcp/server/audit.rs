use super::*;

use crate::mcp::AletheiaMcpServer;

use crate::mcp::server::CallToolResult;

impl AletheiaMcpServer {
    /// Handle the `audit_export` tool (Issue #3358).
    ///
    /// Produces a signed, offline-verifiable evidence artifact of an entity's
    /// complete bi-temporal history. The Ed25519 signing key is operator-
    /// provided out of band via the `ALETHEIADB_AUDIT_SIGNING_KEY` environment
    /// variable (a 32-byte hex seed); the secret is never returned or logged —
    /// only the public key travels in the artifact.
    pub(super) fn handle_audit_export(&self, args: serde_json::Value) -> CallToolResult {
        use crate::audit::{AuditScope, AuditSigningKey, ExportOptions, SIGNING_KEY_ENV};

        let req: AuditExportRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let scope = match req.entity_type.as_str() {
            "node" => match NodeId::new(req.entity_id) {
                Ok(id) => AuditScope::node(id),
                Err(e) => return self.invalid_argument(&e.to_string()),
            },
            "edge" => match crate::core::id::EdgeId::new(req.entity_id) {
                Ok(id) => AuditScope::edge(id),
                Err(e) => return self.invalid_argument(&e.to_string()),
            },
            other => {
                return self.invalid_argument(&format!(
                    "entity_type must be 'node' or 'edge', got '{other}'"
                ));
            }
        };

        // The signing key is a precondition supplied by the operator, not a
        // caller argument — a missing key is a FAILED_PRECONDITION, never a
        // silent unsigned export.
        let signing_key = match AuditSigningKey::from_env(SIGNING_KEY_ENV) {
            Ok(k) => k,
            Err(_) => {
                return self.failed_precondition(&format!(
                    "audit export requires an operator-provided Ed25519 signing key in the \
                     {SIGNING_KEY_ENV} environment variable (32-byte hex seed)"
                ));
            }
        };

        let mut options =
            ExportOptions::new(req.database_id.unwrap_or_else(|| "aletheiadb".to_string()));
        if !req.redact_keys.is_empty() {
            options = options.redact(req.redact_keys);
        }

        match self.db.audit_export(scope, &signing_key, &options) {
            Ok(export) => match serde_json::to_value(&export) {
                Ok(artifact) => self.success_json(json!({
                    "artifact": artifact,
                    "public_key": signing_key.public_key().to_hex(),
                    "entity_count": export.entity_count(),
                    "version_count": export.version_count(),
                    "chain_root": export.chain.root,
                })),
                Err(e) => self.error_result(McpError::new(
                    McpErrorCode::Internal,
                    format!("failed to serialize artifact: {e}"),
                )),
            },
            Err(crate::audit::AuditError::NoHistory(msg)) => self.error_result(McpError::new(
                McpErrorCode::NotFound,
                format!("no exportable history: {msg}"),
            )),
            Err(e) => self.error_result(McpError::new(
                McpErrorCode::Internal,
                format!("audit export failed: {e}"),
            )),
        }
    }
}
