use super::*;

use crate::mcp::AletheiaMcpServer;

use crate::mcp::server::CallToolResult;

impl AletheiaMcpServer {
    pub(super) fn handle_enable_unique_constraint(
        &self,
        args: serde_json::Value,
    ) -> CallToolResult {
        let req: EnableUniqueConstraintRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        match self
            .db
            .unique_constraint(&req.label, &req.property)
            .enable()
        {
            Ok(()) => self.success_json(json!({
                "success": true,
                "label": req.label,
                "property": req.property
            })),
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_list_unique_constraints(
        &self,
        _args: serde_json::Value,
    ) -> CallToolResult {
        let constraints = self.db.list_unique_constraints();
        let list: Vec<serde_json::Value> = constraints
            .into_iter()
            .map(|(label, property)| json!({ "label": label, "property": property }))
            .collect();
        self.success_json(json!({
            "constraints": list,
            "count": list.len()
        }))
    }

    /// Discover the graph's schema (labels, edge types, property keys),
    /// optionally as of a bi-temporal instant. Never errors on an empty
    /// database — returns a well-formed, empty summary instead.
    pub(super) fn handle_get_schema(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetSchemaRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let temporal = match self
            .resolve_bitemporal_as_of(&req.as_of_valid_time, &req.as_of_transaction_time)
        {
            Ok(t) => t,
            Err(result) => return result,
        };

        let result = match temporal {
            None => self.db.schema(),
            Some((vt, tt)) => self.db.schema_as_of(vt, tt),
        };

        match result {
            Ok(schema) => self.success_json(self.schema_to_json(&schema)),
            Err(e) => self.db_error(e),
        }
    }

    /// Handle the `database_stats` tool (Issue #3222).
    ///
    /// Thin aggregator: delegates entirely to the public
    /// [`AletheiaDB::stats`] snapshot and serializes it — no storage logic
    /// lives here. The underlying getters are all O(1)/cached (see
    /// `src/db/stats.rs`), so this never triggers a version scan.
    pub(super) fn handle_database_stats(&self, args: serde_json::Value) -> CallToolResult {
        // The tool takes no required arguments; clients may send no
        // `arguments` at all (surfaced here as JSON null) or an empty
        // object. Normalize null so both forms are accepted.
        let args = if args.is_null() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            args
        };
        let _req: DatabaseStatsRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        match serde_json::to_value(self.db.stats()) {
            Ok(value) => self.success_json(value),
            Err(e) => self.error_result(McpError::new(
                McpErrorCode::Internal,
                format!("Failed to serialize database stats: {}", e),
            )),
        }
    }
}
