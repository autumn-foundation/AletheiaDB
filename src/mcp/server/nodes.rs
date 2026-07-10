use super::*;

use crate::mcp::AletheiaMcpServer;

use crate::mcp::server::CallToolResult;

impl AletheiaMcpServer {
    pub(super) fn handle_get_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: GetNodeRequest = match serde_json::from_value(args) {
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

        match self.db.get_node(node_id) {
            Ok(node) => {
                let now = time::now();
                let response =
                    self.node_to_response(&node, req.include_vectors.unwrap_or(false), now);
                self.success_json(
                    serde_json::to_value(&response)
                        .expect("response serialization should not fail"),
                )
            }
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_create_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: CreateNodeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
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
            .create_node_with_options(&req.label, properties, options)
        {
            Ok(node_id) => match self.db.get_node(node_id) {
                Ok(node) => {
                    let now = time::now();
                    let response = self.node_to_response(&node, true, now);
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

    pub(super) fn handle_update_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: UpdateNodeRequest = match serde_json::from_value(args) {
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
            .update_node_with_options(node_id, properties, options)
        {
            Ok(()) => match self.db.get_node(node_id) {
                Ok(node) => {
                    let now = time::now();
                    let response = self.node_to_response(&node, true, now);
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

    pub(super) fn handle_delete_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: DeleteNodeRequest = match serde_json::from_value(args) {
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

        let detach = req.detach.unwrap_or(false);

        let valid_from = match self.parse_opt_timestamp("valid_time", &req.valid_time) {
            Ok(v) => v,
            Err(result) => return result,
        };

        if detach && valid_from.is_some() {
            return self.invalid_argument(
                "valid_time is not supported together with detach:true; cascade delete does \
                 not support backdating. Delete the connected edges individually with \
                 valid_time, or omit valid_time to cascade-delete at now.",
            );
        }

        // Perform the connected-edge check and the deletion inside a single write
        // transaction so they observe the same storage state. Splitting the count
        // into a separate transaction (or doing it before opening one) leaves a
        // check-then-act gap in which a concurrent writer could add an edge after
        // the count but before the delete, silently orphaning it. Keeping both in
        // one closure removes that cross-transaction gap (Issue #3209).
        enum Outcome {
            Refused { connected_edges: usize },
            Deleted { edges_removed: usize },
        }

        let outcome = self.db.write(|tx| -> crate::core::error::Result<Outcome> {
            // `count_connected_edges` reads the same `current` storage the
            // transaction's edge traversal uses, and also verifies the node
            // exists (errors propagate via `?`).
            let connected_edges = self.db.count_connected_edges(node_id)?;

            // Refuse-by-default: never report a bare success while silently
            // orphaning edges. The caller must opt into destruction via `detach`.
            if connected_edges > 0 && !detach {
                return Ok(Outcome::Refused { connected_edges });
            }

            if detach {
                // Cascade-equivalent delete: remove the node and all connected
                // edges, reporting exactly how many edges were removed.
                tx.delete_node_cascade(node_id)?;
                Ok(Outcome::Deleted {
                    edges_removed: connected_edges,
                })
            } else {
                // No connected edges: a plain delete cannot orphan anything.
                tx.delete_node_with_valid_time(node_id, valid_from)?;
                Ok(Outcome::Deleted { edges_removed: 0 })
            }
        });

        let outcome = match outcome {
            Ok(o) => o,
            Err(e) => return self.db_error(e),
        };

        match outcome {
            // The #3209 refusal in the #3234 structured shape:
            // `FAILED_PRECONDITION` with `details.connected_edges`, while the
            // legacy top-level fields are preserved additively (no loss).
            Outcome::Refused { connected_edges } => {
                let message = format!(
                    "Node {} has {} connected edge(s); refusing to delete. \
                     Pass `detach: true` to delete the node and its connected edges, \
                     or remove the edges first.",
                    req.node_id, connected_edges
                );
                let mut top_level = serde_json::Map::new();
                top_level.insert("success".to_string(), json!(false));
                top_level.insert("node_id".to_string(), json!(req.node_id));
                top_level.insert("connected_edges".to_string(), json!(connected_edges));
                top_level.insert("detach_required".to_string(), json!(true));
                Self::error_result_with_top_level(
                    McpError::new(McpErrorCode::FailedPrecondition, message).details(json!({
                        "node_id": req.node_id,
                        "connected_edges": connected_edges,
                        "detach_required": true
                    })),
                    top_level,
                )
            }
            Outcome::Deleted { edges_removed } => self.success_json(json!({
                "success": true,
                "deleted_node_id": req.node_id,
                "detached": detach,
                "edges_removed": edges_removed
            })),
        }
    }

    pub(super) fn handle_retract_node(&self, args: serde_json::Value) -> CallToolResult {
        let req: RetractNodeRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let node_id = match NodeId::new(req.node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (see handle_delete_node).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let detach = req.detach.unwrap_or(false);

        // Matching the #3221 convention: valid_time defaults to now.
        let valid_to = match self.parse_opt_timestamp("valid_time", &req.valid_time) {
            Ok(v) => v.unwrap_or_else(time::now),
            Err(result) => return result,
        };

        // Perform the connected-edge check and the retraction inside a single
        // write transaction so they observe the same storage state — no
        // check-then-act gap for a concurrent writer to slip an edge into
        // (same rationale as handle_delete_node, Issue #3209).
        enum Outcome {
            Refused { connected_edges: usize },
            Retracted(crate::api::transaction::RetractionResult),
        }

        let outcome = self.db.write(|tx| -> crate::core::error::Result<Outcome> {
            use crate::api::transaction::ReadOps;

            // The connected-edge contract only applies to a currently-present
            // node; an already-retracted node short-circuits below to the
            // idempotent result (and a nonexistent one to NOT_FOUND). All
            // reads go through the transaction itself (buffer-aware,
            // snapshot-isolated) rather than back through `self.db`.
            if tx.get_node(node_id).is_ok() {
                // Enumerate DISTINCT connected edges once — the refusal
                // count and the detach co-retraction share the same
                // sort/dedup list, so `connected_edges` always equals what
                // `detach: true` would retract (a self-loop appears in both
                // adjacency directions but is one edge).
                let mut edge_ids = tx.get_outgoing_edges(node_id)?;
                edge_ids.extend(tx.get_incoming_edges(node_id)?);
                edge_ids.sort_unstable();
                edge_ids.dedup();
                let connected_edges = edge_ids.len();

                // Refuse-by-default: never report a bare success that leaves
                // edges pointing at a retracted node. The caller must opt
                // into co-retraction via `detach`.
                if connected_edges > 0 && !detach {
                    return Ok(Outcome::Refused { connected_edges });
                }

                if detach && connected_edges > 0 {
                    // Co-retract every connected edge at the same valid time.
                    let mut edges_retracted = 0;
                    for edge_id in edge_ids {
                        let edge_result = tx.retract_edge(edge_id, valid_to)?;
                        if !edge_result.already_retracted {
                            edges_retracted += 1;
                        }
                    }

                    let mut result = tx.retract_node(node_id, valid_to)?;
                    result.edges_retracted = edges_retracted;
                    return Ok(Outcome::Retracted(result));
                }
            }

            Ok(Outcome::Retracted(tx.retract_node(node_id, valid_to)?))
        });

        let outcome = match outcome {
            Ok(o) => o,
            Err(e) => return self.db_error(e),
        };

        match outcome {
            // The refusal in the #3234 structured shape: FAILED_PRECONDITION
            // with `details.connected_edges`, legacy top-level fields
            // preserved additively (byte-for-byte parallel to the
            // handle_delete_node refusal).
            Outcome::Refused { connected_edges } => {
                let message = format!(
                    "Node {} has {} connected edge(s); refusing to retract. \
                     Pass `detach: true` to retract the node and its connected edges \
                     at the same valid time, or retract the edges first.",
                    req.node_id, connected_edges
                );
                let mut top_level = serde_json::Map::new();
                top_level.insert("success".to_string(), json!(false));
                top_level.insert("node_id".to_string(), json!(req.node_id));
                top_level.insert("connected_edges".to_string(), json!(connected_edges));
                top_level.insert("detach_required".to_string(), json!(true));
                Self::error_result_with_top_level(
                    McpError::new(McpErrorCode::FailedPrecondition, message).details(json!({
                        "node_id": req.node_id,
                        "connected_edges": connected_edges,
                        "detach_required": true
                    })),
                    top_level,
                )
            }
            Outcome::Retracted(result) => self.success_json(json!({
                "success": true,
                "node_id": req.node_id,
                "retracted": true,
                "already_retracted": result.already_retracted,
                "valid_from": Self::timestamp_to_rfc3339_micros(result.valid_from),
                "valid_to": Self::timestamp_to_rfc3339_micros(result.valid_to),
                "edges_retracted": result.edges_retracted
            })),
        }
    }

    pub(super) fn handle_delete_node_cascade(&self, args: serde_json::Value) -> CallToolResult {
        let req: DeleteNodeCascadeRequest = match serde_json::from_value(args) {
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

        match self.db.write(|tx| tx.delete_node_cascade(node_id)) {
            Ok(()) => self.success_json(json!({
                "success": true,
                "deleted_node_id": req.node_id,
                "cascade": true
            })),
            Err(e) => self.db_error(e),
        }
    }

    /// Cursor-mode `list_nodes` (Issue #3360): snapshot-anchored keyset paging.
    ///
    /// The scan is pinned to the transaction time captured on the first page
    /// and every page is reconstructed as of that coordinate via the same
    /// point-in-time machinery `find_nodes_at_time` uses, so concurrent writes
    /// after the anchor are invisible: the union of all pages equals exactly a
    /// single unbounded `list_nodes` at the anchor moment. Requires `label`
    /// (an unlabeled list has no enumerable, ordered candidate set).
    pub(super) fn handle_list_nodes_cursor(&self, args: &serde_json::Value) -> CallToolResult {
        let now = time::now();

        // Resume path: everything discriminating is baked into the token.
        if let Some(token) = args.get("cursor").and_then(|v| v.as_str()) {
            let payload = match self.cursors.decode(token, "list_nodes") {
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
                "list_nodes",
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

        // First page: validate and pin the snapshot at "now".
        let property_key = Self::arg_str(args, "property_key");
        let property_value = Self::arg_value(args, "property_value");
        if property_key.is_some() != property_value.is_some() {
            return self.invalid_argument(
                "Both 'property_key' and 'property_value' are required together",
            );
        }
        let label = match Self::arg_str(args, "label") {
            Some(l) => l,
            None => {
                return self.invalid_argument(
                    "Cursor paging requires 'label' (an unlabeled node list has no ordered, \
                     enumerable candidate set to page over).",
                );
            }
        };
        let limit = Self::arg_limit(args);
        let include_vectors = args
            .get("include_vectors")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let snapshot = (now, now);
        let candidates =
            match self.fetch_node_candidates(&label, &property_key, &property_value, snapshot) {
                Ok(c) => c,
                Err(result) => return result,
            };
        let filters =
            Self::node_scan_filters(&label, &property_key, &property_value, include_vectors);
        self.emit_node_cursor_page(
            "list_nodes",
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

    pub(super) fn handle_list_nodes(&self, args: serde_json::Value) -> CallToolResult {
        // Snapshot-anchored cursor paging (Issue #3360) is a distinct path
        // from the legacy offset paging below, which stays unchanged for
        // backward compatibility. The cursor parameters are additive and read
        // straight off the raw arguments (the request structs stay unchanged).
        if Self::cursor_requested(&args) {
            return self.handle_list_nodes_cursor(&args);
        }

        let req: ListNodesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        // Apply resource limits. A page must be able to carry a continuation
        // cursor, so the limit is at least 1 (limit:0 would otherwise report
        // has_more:true with next_offset==offset, a non-progressing page that
        // traps a paginating caller in an infinite loop).
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .clamp(1, MAX_RESULT_LIMIT);
        let offset = req.offset.unwrap_or(0).min(MAX_PAGINATION_OFFSET);

        // Validate property filter: both key and value are required together with label
        if req.property_key.is_some() != req.property_value.is_some() {
            return self.invalid_argument(
                "Both 'property_key' and 'property_value' are required together",
            );
        }
        if req.property_key.is_some() && req.label.is_none() {
            return self.invalid_argument("Property filtering requires 'label' to be specified");
        }

        // One request-scoped wallclock for every entity in the response
        // (Issue #3391).
        let now = time::now();

        // Property-based lookup: label + property_key + property_value
        if let (Some(label), Some(prop_key), Some(prop_val)) =
            (&req.label, &req.property_key, &req.property_value)
        {
            let property_value =
                match self.json_to_property_value(prop_val) {
                    Some(v) => v,
                    None => return self.invalid_argument(
                        "Unsupported property_value type. Use strings, numbers, booleans, or null.",
                    ),
                };

            let node_ids = self
                .db
                .find_nodes_by_property(label, prop_key, &property_value);

            // The full matching id list is already materialized, so the total
            // is cheap to report and `has_more` is exact.
            let total_matching = node_ids.len();
            let include_vectors = req.include_vectors.unwrap_or(false);
            let mut nodes = Vec::with_capacity(limit);
            for node_id in node_ids.into_iter().skip(offset).take(limit) {
                if let Ok(node) = self.db.get_node(node_id) {
                    nodes.push(self.node_to_response(&node, include_vectors, now));
                }
            }

            // `has_more`/`next_offset` are derived from the requested window
            // (`limit`) against `total_matching`, not from `nodes.len()`: a
            // stale property-index entry pointing at a since-deleted node is
            // still one of the `limit` ids this page consumed, so basing
            // `next_offset` on the (possibly smaller) resolved count would
            // re-skip into already-consumed ids and duplicate a row on the
            // next page.
            let has_more = offset.saturating_add(limit) < total_matching;
            let mut response = json!({
                "nodes": nodes,
                "count": nodes.len(),
                "offset": offset,
                "limit": limit
            });
            Self::attach_completeness(&mut response, offset, limit, has_more, Some(total_matching));
            return self.success_json(response);
        }

        // Label-only scan
        if let Some(label) = &req.label {
            let builder = crate::query::QueryBuilder::new().scan_label(label);

            // Note: We fetch offset+limit rows then skip offset. We fetch one
            // extra row (offset+limit+1) purely to detect whether more matching
            // nodes exist beyond this page (`has_more`) without paying for a
            // full-scan count. Offset is capped to prevent excessive memory use.
            match builder.limit(limit + offset + 1).execute(&self.db) {
                Ok(results) => {
                    // Use iterator-based approach to avoid allocating full Vec
                    let include_vectors = req.include_vectors.unwrap_or(false);
                    let mut nodes = Vec::with_capacity(limit);
                    let mut skipped = 0;
                    let mut has_more = false;

                    for row_result in results {
                        match row_result {
                            Ok(row) => {
                                if skipped < offset {
                                    skipped += 1;
                                    continue;
                                }
                                if let EntityResult::Node(node) = row.entity {
                                    if nodes.len() >= limit {
                                        // The extra (offset+limit+1)th matching
                                        // row proves more results remain.
                                        has_more = true;
                                        break;
                                    }
                                    nodes.push(self.node_to_response(&node, include_vectors, now));
                                }
                            }
                            Err(e) => return self.db_error(e),
                        }
                    }

                    // A label scan cannot cheaply know the matching total
                    // (that needs a full scan), so `total_matching` is omitted;
                    // `has_more` alone carries the completeness signal.
                    let mut response = json!({
                        "nodes": nodes,
                        "count": nodes.len(),
                        "offset": offset,
                        "limit": limit
                    });
                    Self::attach_completeness(&mut response, offset, nodes.len(), has_more, None);
                    self.success_json(response)
                }
                Err(e) => self.db_error(e),
            }
        } else {
            // Without a label filter, we cannot efficiently list all nodes
            // Return a helpful message
            let mut response = json!({
                "message": "Use 'label' filter to list nodes by type, or use 'count_nodes' for total count",
                "total_count": self.db.node_count(),
                "nodes": [],
                "count": 0,
                "offset": offset,
                "limit": limit
            });
            Self::attach_completeness(&mut response, offset, 0, false, None);
            self.success_json(response)
        }
    }

    pub(super) fn handle_count_nodes(&self, args: serde_json::Value) -> CallToolResult {
        let req: CountNodesRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        if let Some(label) = &req.label {
            // Use QueryBuilder to count by label efficiently without collecting all rows
            let builder = crate::query::QueryBuilder::new().scan_label(label);
            match builder.execute(&self.db) {
                Ok(mut results) => {
                    // Efficiently count without allocating a Vec
                    match results.try_fold(0usize, |acc, row| row.map(|_| acc + 1)) {
                        Ok(count) => self.success_json(json!({"count": count, "label": label})),
                        Err(e) => self.error_result(McpError::from_db_error(&e).with_message(
                            format!("Error counting nodes with label '{}': {}", label, e),
                        )),
                    }
                }
                Err(e) => self.error_result(McpError::from_db_error(&e).with_message(format!(
                    "Error executing count query for label '{}': {}",
                    label, e
                ))),
            }
        } else {
            self.success_json(json!({"count": self.db.node_count()}))
        }
    }
}
