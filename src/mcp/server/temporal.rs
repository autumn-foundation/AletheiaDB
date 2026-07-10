use super::*;

use crate::mcp::AletheiaMcpServer;

use crate::mcp::server::CallToolResult;

impl AletheiaMcpServer {
    pub(super) fn handle_get_node_at_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetNodeAtTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let valid_time = match self.parse_timestamp(&req.valid_time) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        let tx_time = match self.parse_optional_tx_time(req.transaction_time.as_deref()) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        match self.db.get_node_at_time(node_id, valid_time, tx_time) {
            Ok(node) => {
                let now = time::now();
                let response = self.node_to_response(&node, true, now);
                self.success_json(json!({
                    "node": response,
                    "valid_time": req.valid_time,
                    "transaction_time": Self::format_tx_time_response(req.transaction_time)
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_get_edge_at_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetEdgeAtTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let valid_time = match self.parse_timestamp(&req.valid_time) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        let tx_time = match self.parse_optional_tx_time(req.transaction_time.as_deref()) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        match self.db.get_edge_at_time(edge_id, valid_time, tx_time) {
            Ok(edge) => {
                let now = time::now();
                let response = self.edge_to_response(&edge, true, now);
                self.success_json(json!({
                    "edge": response,
                    "valid_time": req.valid_time,
                    "transaction_time": Self::format_tx_time_response(req.transaction_time)
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    /// Cursor-mode `find_nodes_at_time` (Issue #3360): snapshot-anchored keyset
    /// paging. The snapshot is the caller's requested `(valid_time,
    /// transaction_time)` -- already a point-in-time read, so consistency is
    /// native; continuation just seeks by node id past the last returned.
    pub(super) fn handle_find_nodes_at_time_cursor(
        &self,
        args: &serde_json::Value,
    ) -> CallToolResult {
        let now = time::now();

        if let Some(token) = args.get("cursor").and_then(|v| v.as_str()) {
            let payload = match self.cursors.decode(token, "find_nodes_at_time") {
                Ok(p) => p,
                Err(e) => return self.error_result(e),
            };
            let label = payload.filters["label"].as_str().unwrap_or("").to_string();
            let property_key = payload.filters["property_key"].as_str().map(str::to_string);
            let property_value = match &payload.filters["property_value"] {
                serde_json::Value::Null => None,
                v => Some(v.clone()),
            };
            let include_vectors = payload.filters["include_vectors"]
                .as_bool()
                .unwrap_or(false);
            let snapshot = (Timestamp::from(payload.svt), Timestamp::from(payload.stt));
            let candidates = match self.fetch_node_candidates(
                &label,
                &property_key,
                &property_value,
                snapshot,
            ) {
                Ok(c) => c,
                Err(result) => return result,
            };
            return self.emit_node_cursor_page(
                "find_nodes_at_time",
                snapshot,
                payload.after,
                payload.limit,
                include_vectors,
                payload.filters.clone(),
                candidates,
                payload.cid,
                now,
            );
        }

        // First page: validate filter combo and pin the requested coordinate.
        let label = Self::arg_str(args, "label").unwrap_or_default();
        let property_key = Self::arg_str(args, "property_key");
        let property_value = Self::arg_value(args, "property_value");
        if property_key.is_some() != property_value.is_some() {
            return self.invalid_argument(
                "Both 'property_key' and 'property_value' are required together",
            );
        }
        let valid_time_str = Self::arg_str(args, "valid_time").unwrap_or_default();
        let valid_time = match self.parse_timestamp(&valid_time_str) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&format!("Invalid valid_time: {}", e)),
        };
        let tx_time = match self
            .parse_optional_tx_time(Self::arg_str(args, "transaction_time").as_deref())
        {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&format!("Invalid transaction_time: {}", e)),
        };
        let limit = Self::arg_limit(args);
        let include_vectors = args
            .get("include_vectors")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let snapshot = (valid_time, tx_time);
        let candidates =
            match self.fetch_node_candidates(&label, &property_key, &property_value, snapshot) {
                Ok(c) => c,
                Err(result) => return result,
            };
        let filters =
            Self::node_scan_filters(&label, &property_key, &property_value, include_vectors);
        self.emit_node_cursor_page(
            "find_nodes_at_time",
            snapshot,
            None,
            limit,
            include_vectors,
            filters,
            candidates,
            String::new(),
            now,
        )
    }

    /// Find nodes by label (and optional exact property match) as of a
    /// bi-temporal point (Issue #3236).
    ///
    /// Validation mirrors `handle_list_nodes` (both-or-neither property
    /// filter, same limit/offset clamps); the temporal reconstruction is
    /// delegated to `AletheiaDB::find_nodes_at_time` /
    /// `find_nodes_by_property_at`, which reconstruct each candidate from
    /// the historical version visible at the queried coordinate -- so nodes
    /// deleted from current state are still found when both dimensions
    /// anchor before the deletion. The candidate set is capped at the same
    /// `max_schema_as_of_entities` limit bi-temporal `get_schema` uses; when
    /// truncated, the response discloses it via `sampled: true` and
    /// `total_matching`/`has_more` count matches within the sampled
    /// candidate set only.
    pub(super) fn handle_find_nodes_at_time(&self, args: serde_json::Value) -> CallToolResult {
        // Snapshot-anchored cursor paging (Issue #3360); offset paging below
        // is unchanged for backward compatibility.
        if Self::cursor_requested(&args) {
            return self.handle_find_nodes_at_time_cursor(&args);
        }

        let req: FindNodesAtTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        // Validate property filter: both key and value are required together
        // (mirroring list_nodes).
        if req.property_key.is_some() != req.property_value.is_some() {
            return self.invalid_argument(
                "Both 'property_key' and 'property_value' are required together",
            );
        }

        let valid_time = match self.parse_timestamp(&req.valid_time) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&format!("Invalid valid_time: {}", e)),
        };
        let tx_time = match self.parse_optional_tx_time(req.transaction_time.as_deref()) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&format!("Invalid transaction_time: {}", e)),
        };

        // Apply resource limits exactly like list_nodes: a page must be able
        // to carry a continuation cursor, so the limit is at least 1.
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .clamp(1, MAX_RESULT_LIMIT);
        let offset = req.offset.unwrap_or(0).min(MAX_PAGINATION_OFFSET);

        let matches =
            if let (Some(prop_key), Some(prop_val)) = (&req.property_key, &req.property_value) {
                let property_value = match self.json_to_property_value(prop_val) {
                    Some(v) => v,
                    None => return self.invalid_argument(
                        "Unsupported property_value type. Use strings, numbers, booleans, or null.",
                    ),
                };
                self.db.find_nodes_by_property_at(
                    &req.label,
                    prop_key,
                    &property_value,
                    valid_time,
                    tx_time,
                )
            } else {
                self.db.find_nodes_at_time(&req.label, valid_time, tx_time)
            };

        match matches {
            Ok(matches) => {
                // The matching set is already materialized (sorted by node
                // id for stable pagination), so the total is cheap to report
                // and `has_more` is exact *within the candidate set*. When
                // `sampled` is true the candidate enumeration was truncated
                // at the configured cap, so `total_matching` is honest only
                // about the sampled candidates -- the flag discloses that.
                let sampled = matches.sampled;
                let matches = matches.nodes;
                let total_matching = matches.len();
                let include_vectors = req.include_vectors.unwrap_or(false);
                // One request-scoped wallclock for every entity in the
                // response (Issue #3391).
                let now = time::now();
                let nodes: Vec<NodeResponse> = matches
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .map(|node| self.node_to_response(node, include_vectors, now))
                    .collect();

                let has_more = offset.saturating_add(limit) < total_matching;
                let mut response = json!({
                    "nodes": nodes,
                    "count": nodes.len(),
                    "offset": offset,
                    "limit": limit,
                    // Candidate-set truncation disclosure, mirroring
                    // get_schema's `sampled` (same underlying cap).
                    "sampled": sampled,
                    // The resolved coordinate this answer holds at -- the
                    // omitted transaction_time resolves to a concrete "now".
                    "valid_time": Self::format_timestamp_rfc3339(valid_time),
                    "transaction_time": Self::format_timestamp_rfc3339(tx_time),
                });
                Self::attach_completeness(
                    &mut response,
                    offset,
                    limit,
                    has_more,
                    Some(total_matching),
                );
                self.success_json(response)
            }
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_list_changes(&self, args: serde_json::Value) -> CallToolResult {
        let req: ListChangesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let tx_from = match self.parse_timestamp(&req.tx_from) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&format!("Invalid tx_from: {}", e)),
        };
        let tx_to = match self.parse_timestamp(&req.tx_to) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&format!("Invalid tx_to: {}", e)),
        };

        let valid_from = match self.parse_opt_timestamp("valid_from", &req.valid_from) {
            Ok(v) => v,
            Err(resp) => return resp,
        };
        let valid_to = match self.parse_opt_timestamp("valid_to", &req.valid_to) {
            Ok(v) => v,
            Err(resp) => return resp,
        };

        // A page must be able to carry a continuation cursor, so the limit is at least 1.
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .clamp(1, MAX_RESULT_LIMIT);

        let query = ChangeFeedQuery {
            tx_from,
            tx_to,
            valid_from,
            valid_to,
            label: req.label.clone(),
            limit,
            cursor: req.cursor.clone(),
        };

        match self.db.list_changes(&query) {
            Ok(page) => {
                let changes: Vec<serde_json::Value> = page
                    .changes
                    .iter()
                    .map(|record| {
                        json!({
                            "entity_id": record.entity_id,
                            "version_id": record.version_id,
                            "kind": record.kind.as_str(),
                            "change_type": record.change_type.as_str(),
                            "label": record.label,
                            "transaction_time": time::to_iso8601(record.transaction_time()),
                            "transaction_time_range": {
                                "start": time::to_iso8601(record.transaction_time_range.start()),
                                "end": time::to_iso8601(record.transaction_time_range.end()),
                            },
                            "valid_time_range": {
                                "start": time::to_iso8601(record.valid_time_range.start()),
                                "end": time::to_iso8601(record.valid_time_range.end()),
                            },
                        })
                    })
                    .collect();

                self.success_json(json!({
                    "changes": changes,
                    "count": page.changes.len(),
                    "next_cursor": page.next_cursor,
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_get_node_at_valid_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetNodeAtValidTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let valid_time = match self.parse_timestamp(&req.valid_time) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        match self.db.get_node_at_valid_time(node_id, valid_time) {
            Ok(node) => {
                let now = time::now();
                let response = self.node_to_response(&node, true, now);
                self.success_json(json!({
                    "node": response,
                    "valid_time": req.valid_time
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_get_node_at_transaction_time(
        &self,
        args: serde_json::Value,
    ) -> CallToolResult {
        let req: GetNodeAtTransactionTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let tx_time = match self.parse_timestamp(&req.transaction_time) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        match self.db.get_node_at_transaction_time(node_id, tx_time) {
            Ok(node) => {
                let now = time::now();
                let response = self.node_to_response(&node, true, now);
                self.success_json(json!({
                    "node": response,
                    "transaction_time": req.transaction_time
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_get_node_history(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetNodeHistoryRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        match self.db.get_node_history(node_id) {
            Ok(history) => {
                let versions: Vec<_> = history
                    .versions
                    .iter()
                    .map(|v| self.version_info_to_response(v))
                    .collect();

                self.success_json(json!({
                    "node_id": req.node_id,
                    "versions": versions,
                    "version_count": versions.len()
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_diff_node_versions(&self, args: serde_json::Value) -> CallToolResult {
        let req: DiffNodeVersionsRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let from_version = match crate::core::id::VersionId::new(req.from_version) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let to_version = match crate::core::id::VersionId::new(req.to_version) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        match self
            .db
            .diff_node_versions(node_id, from_version, to_version)
        {
            Ok(diff) => {
                let response = self.version_diff_to_response(&diff);
                self.success_json(json!(response))
            }
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_get_edge_at_valid_time(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetEdgeAtValidTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let valid_time = match self.parse_timestamp(&req.valid_time) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        match self.db.get_edge_at_valid_time(edge_id, valid_time) {
            Ok(edge) => {
                let now = time::now();
                let response = self.edge_to_response(&edge, true, now);
                self.success_json(json!({
                    "edge": response,
                    "valid_time": req.valid_time
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_get_edge_at_transaction_time(
        &self,
        args: serde_json::Value,
    ) -> CallToolResult {
        let req: GetEdgeAtTransactionTimeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let tx_time = match self.parse_timestamp(&req.transaction_time) {
            Ok(t) => t,
            Err(e) => return self.invalid_argument(&e),
        };

        match self.db.get_edge_at_transaction_time(edge_id, tx_time) {
            Ok(edge) => {
                let now = time::now();
                let response = self.edge_to_response(&edge, true, now);
                self.success_json(json!({
                    "edge": response,
                    "transaction_time": req.transaction_time
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_get_edge_history(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetEdgeHistoryRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        match self.db.get_edge_history(edge_id) {
            Ok(history) => {
                let versions: Vec<_> = history
                    .versions
                    .iter()
                    .map(|v| self.version_info_to_response(v))
                    .collect();

                self.success_json(json!({
                    "edge_id": req.edge_id,
                    "versions": versions,
                    "version_count": versions.len()
                }))
            }
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_diff_edge_versions(&self, args: serde_json::Value) -> CallToolResult {
        let req: DiffEdgeVersionsRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let from_version = match crate::core::id::VersionId::new(req.from_version) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let to_version = match crate::core::id::VersionId::new(req.to_version) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        match self
            .db
            .diff_edge_versions(edge_id, from_version, to_version)
        {
            Ok(diff) => {
                let response = self.version_diff_to_response(&diff);
                self.success_json(json!(response))
            }
            Err(e) => self.db_error(e),
        }
    }

    /// Report the dataset's queryable bi-temporal extent (Issue #3238).
    ///
    /// The handler only serializes: bounds come from the public
    /// `AletheiaDB::temporal_extent` / `temporal_extent_by_label` API.
    pub(super) fn handle_temporal_extent(&self, args: serde_json::Value) -> CallToolResult {
        // The tool has no required arguments: a call with no `arguments`
        // object at all must behave like `{}`.
        let args = if args.is_null() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            args
        };

        let req: TemporalExtentRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let result = if req.by_label.unwrap_or(false) {
            self.db.temporal_extent_by_label()
        } else {
            self.db.temporal_extent()
        };

        match result {
            Ok(extent) => self.success_json(Self::temporal_extent_to_json(&extent)),
            Err(e) => self.db_error(e),
        }
    }
}
