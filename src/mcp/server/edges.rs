use super::*;

use crate::mcp::AletheiaMcpServer;

use crate::mcp::server::CallToolResult;

impl AletheiaMcpServer {
    pub(super) fn handle_get_edge(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetEdgeRequest = match serde_json::from_value(args) {
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

        match self.db.get_edge(edge_id) {
            Ok(edge) => {
                let now = time::now();
                let response =
                    self.edge_to_response(&edge, req.include_vectors.unwrap_or(false), now);
                self.success_json(
                    serde_json::to_value(&response)
                        .expect("response serialization should not fail"),
                )
            }
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_create_edge(&self, args: serde_json::Value) -> CallToolResult {
        let req: CreateEdgeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let source_id = match NodeId::new(req.source_id) {
            Ok(id) => id,
            Err(e) => return self.invalid_argument(&format!("Invalid source_id: {}", e)),
        };

        let target_id = match NodeId::new(req.target_id) {
            Ok(id) => id,
            Err(e) => return self.invalid_argument(&format!("Invalid target_id: {}", e)),
        };

        let properties = match req.properties {
            Some(p) => match self.json_to_property_map(&p) {
                Ok(map) => map,
                Err(e) => return self.invalid_argument(&format!("Invalid properties: {}", e)),
            },
            None => PropertyMap::default(),
        };

        let valid_from = match self.parse_opt_timestamp("valid_time", &req.valid_time) {
            Ok(v) => v,
            Err(result) => return result,
        };
        let provenance = match self.parse_opt_provenance(req.provenance) {
            Ok(p) => p,
            Err(result) => return result,
        };

        let mut options = crate::api::transaction::WriteRequestOptions::new();
        if let Some(valid_from) = valid_from {
            options = options.with_valid_from(valid_from);
        }
        if let Some(provenance) = provenance {
            options = options.with_provenance(provenance);
        }

        match self
            .db
            .create_edge_with_options(source_id, target_id, &req.label, properties, options)
        {
            Ok(edge_id) => match self.db.get_edge(edge_id) {
                Ok(edge) => {
                    let now = time::now();
                    let response = self.edge_to_response(&edge, true, now);
                    self.success_json(
                        serde_json::to_value(&response)
                            .expect("response serialization should not fail"),
                    )
                }
                Err(e) => self.db_error(e),
            },
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_update_edge(&self, args: serde_json::Value) -> CallToolResult {
        let req: UpdateEdgeRequest = match serde_json::from_value(args) {
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

        let properties = match self.json_to_property_map(&req.properties) {
            Ok(map) => map,
            Err(e) => return self.invalid_argument(&format!("Invalid properties: {}", e)),
        };

        let valid_from = match self.parse_opt_timestamp("valid_time", &req.valid_time) {
            Ok(v) => v,
            Err(result) => return result,
        };
        let provenance = match self.parse_opt_provenance(req.provenance) {
            Ok(p) => p,
            Err(result) => return result,
        };

        let mut options = crate::api::transaction::WriteRequestOptions::new();
        if let Some(valid_from) = valid_from {
            options = options.with_valid_from(valid_from);
        }
        if let Some(provenance) = provenance {
            options = options.with_provenance(provenance);
        }

        match self
            .db
            .update_edge_with_options(edge_id, properties, options)
        {
            Ok(()) => match self.db.get_edge(edge_id) {
                Ok(edge) => {
                    let now = time::now();
                    let response = self.edge_to_response(&edge, true, now);
                    self.success_json(
                        serde_json::to_value(&response)
                            .expect("response serialization should not fail"),
                    )
                }
                Err(e) => self.db_error(e),
            },
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_delete_edge(&self, args: serde_json::Value) -> CallToolResult {
        let req: DeleteEdgeRequest = match serde_json::from_value(args) {
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

        let valid_from = match self.parse_opt_timestamp("valid_time", &req.valid_time) {
            Ok(v) => v,
            Err(result) => return result,
        };

        match self
            .db
            .write(|tx| tx.delete_edge_with_valid_time(edge_id, valid_from))
        {
            Ok(()) => self.success_json(json!({
                "success": true,
                "deleted_edge_id": req.edge_id
            })),
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_retract_edge(&self, args: serde_json::Value) -> CallToolResult {
        let req: RetractEdgeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let edge_id = match EdgeId::new(req.edge_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (see handle_delete_node).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        // Matching the #3221 convention: valid_time defaults to now.
        let valid_to = match self.parse_opt_timestamp("valid_time", &req.valid_time) {
            Ok(v) => v.unwrap_or_else(time::now),
            Err(result) => return result,
        };

        match self.db.retract_edge(edge_id, valid_to) {
            Ok(result) => self.success_json(json!({
                "success": true,
                "edge_id": req.edge_id,
                "retracted": true,
                "already_retracted": result.already_retracted,
                "valid_from": Self::timestamp_to_rfc3339_micros(result.valid_from),
                "valid_to": Self::timestamp_to_rfc3339_micros(result.valid_to)
            })),
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_list_edges(&self, args: serde_json::Value) -> CallToolResult {
        // Cursor paging (Issue #3360) is not supported here: `list_edges` does
        // not enumerate edges (there is no global edge scan). Rather than
        // silently ignore the flag, direct the caller to the cursor-paged
        // adjacency tools. (No-silent-fallback culture.)
        if Self::cursor_requested(&args) {
            return self.error_result(
                McpError::new(
                    McpErrorCode::InvalidArgument,
                    "list_edges does not enumerate edges and is not cursorable. Use \
                     get_outgoing_edges or get_incoming_edges from a known node -- both support \
                     snapshot-anchored cursor paging (use_cursor / cursor).",
                )
                .details(json!({ "cursorable_alternatives": ["get_outgoing_edges", "get_incoming_edges"] })),
            );
        }

        let req: ListEdgesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        // Apply resource limits
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .min(MAX_RESULT_LIMIT);
        let offset = req.offset.unwrap_or(0);

        // Edges cannot be efficiently listed without knowing source/target nodes.
        // Provide helpful guidance to use get_outgoing_edges or get_incoming_edges.
        let mut response = json!({
            "message": "Use 'get_outgoing_edges' or 'get_incoming_edges' from a known node to list edges",
            "total_count": self.db.edge_count(),
            "edges": [],
            "count": 0,
            "offset": offset,
            "limit": limit,
            "label_filter": req.label
        });
        Self::attach_completeness(&mut response, offset, 0, false, None);
        self.success_json(response)
    }

    pub(super) fn handle_count_edges(&self, args: serde_json::Value) -> CallToolResult {
        let req: CountEdgesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        // Note: Counting by label is not efficiently supported without iterating all edges.
        // For now, we only support total count.
        if req.label.is_some() {
            self.success_json(json!({
                "message": "Counting edges by label is not supported. Use total_count instead.",
                "total_count": self.db.edge_count(),
                "count": null
            }))
        } else {
            self.success_json(json!({"count": self.db.edge_count()}))
        }
    }

    pub(super) fn handle_get_outgoing_edges(&self, args: serde_json::Value) -> CallToolResult {
        // Snapshot-anchored cursor paging (Issue #3360); the full-adjacency
        // path below is unchanged for backward compatibility.
        if Self::cursor_requested(&args) {
            return self.handle_adjacency_cursor("get_outgoing_edges", false, &args);
        }

        let req: GetOutgoingEdgesRequest = match serde_json::from_value(args) {
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

        let edge_ids = if let Some(label) = &req.label {
            self.db.get_outgoing_edges_with_label(node_id, label)
        } else {
            self.db.get_outgoing_edges(node_id)
        };

        let include_vectors = req.include_vectors.unwrap_or(false);
        // One request-scoped wallclock for every entity in the response
        // (Issue #3391).
        let now = time::now();
        let edges: Vec<EdgeResponse> = edge_ids
            .into_iter()
            .filter_map(|eid| self.db.get_edge(eid).ok())
            .map(|e| self.edge_to_response(&e, include_vectors, now))
            .collect();

        // This handler returns the complete adjacency (no limit/offset), so the
        // result is never truncated: `has_more` is always false and
        // `total_matching` equals the returned count.
        let count = edges.len();
        let mut response = json!({
            "edges": edges,
            "count": count
        });
        Self::attach_completeness(&mut response, 0, 0, false, Some(count));
        self.success_json(response)
    }

    pub(super) fn handle_get_incoming_edges(&self, args: serde_json::Value) -> CallToolResult {
        // Snapshot-anchored cursor paging (Issue #3360); the full-adjacency
        // path below is unchanged for backward compatibility.
        if Self::cursor_requested(&args) {
            return self.handle_adjacency_cursor("get_incoming_edges", true, &args);
        }

        let req: GetIncomingEdgesRequest = match serde_json::from_value(args) {
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

        let edge_ids = self.db.get_incoming_edges(node_id);

        // Filter by label if provided
        let include_vectors = req.include_vectors.unwrap_or(false);
        // One request-scoped wallclock for every entity in the response
        // (Issue #3391).
        let now = time::now();
        let edges: Vec<EdgeResponse> = edge_ids
            .into_iter()
            .filter_map(|eid| self.db.get_edge(eid).ok())
            .filter(|e| {
                req.label
                    .as_ref()
                    .map(|l| self.matches_label(e.label, l))
                    .unwrap_or(true)
            })
            .map(|e| self.edge_to_response(&e, include_vectors, now))
            .collect();

        // Complete adjacency (no limit/offset): never truncated, so
        // `has_more` is always false and `total_matching` equals the count.
        let count = edges.len();
        let mut response = json!({
            "edges": edges,
            "count": count
        });
        Self::attach_completeness(&mut response, 0, 0, false, Some(count));
        self.success_json(response)
    }

    /// Cursor-mode dispatch shared by `get_outgoing_edges` /
    /// `get_incoming_edges` (Issue #3360). `incoming` selects the direction and
    /// the tool name the cursor is bound to.
    pub(super) fn handle_adjacency_cursor(
        &self,
        tool: &str,
        incoming: bool,
        args: &serde_json::Value,
    ) -> CallToolResult {
        let now = time::now();

        if let Some(token) = args.get("cursor").and_then(|v| v.as_str()) {
            let payload = match self.cursors.decode(token, tool) {
                Ok(p) => p,
                Err(e) => return self.error_result(e),
            };
            let node_id = match NodeId::new(payload.filters["node_id"].as_u64().unwrap_or(0)) {
                Ok(id) => id,
                Err(e) => return self.invalid_argument(&e.to_string()),
            };
            let label = payload.filters["label"].as_str().map(str::to_string);
            let include_vectors = payload.filters["include_vectors"]
                .as_bool()
                .unwrap_or(false);
            let snapshot = (Timestamp::from(payload.svt), Timestamp::from(payload.stt));
            return self.emit_adjacency_cursor_page(
                tool,
                node_id,
                incoming,
                &label,
                snapshot,
                payload.after,
                payload.limit,
                include_vectors,
                payload.cid,
                now,
            );
        }

        let node_id = match NodeId::new(args.get("node_id").and_then(|v| v.as_u64()).unwrap_or(0)) {
            Ok(id) => id,
            Err(e) => return self.invalid_argument(&e.to_string()),
        };
        let label = Self::arg_str(args, "label");
        let include_vectors = args
            .get("include_vectors")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let limit = Self::arg_limit(args);
        let snapshot = (now, now);
        self.emit_adjacency_cursor_page(
            tool,
            node_id,
            incoming,
            &label,
            snapshot,
            None,
            limit,
            include_vectors,
            String::new(),
            now,
        )
    }
}
