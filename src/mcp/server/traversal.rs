use super::*;

use crate::mcp::AletheiaMcpServer;

use crate::mcp::server::CallToolResult;

impl AletheiaMcpServer {
    pub(super) fn handle_traverse(&self, args: serde_json::Value) -> CallToolResult {
        // Snapshot-anchored cursor paging (Issue #3360). Unlike the id-keyset
        // node/adjacency scans, a DFS result order is not a simple id keyset,
        // so traverse's cursor pins the bi-temporal snapshot (making every
        // continuation page consistent -- AC2) and continues by an internal
        // offset over the deterministic DFS order (v1; a depth-independent
        // keyset traversal is a documented follow-up). The offset path below
        // is unchanged for backward compatibility.
        if Self::cursor_requested(&args) {
            return self.handle_traverse_cursor(&args);
        }

        let req: TraverseRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let start_id = match NodeId::new(req.start_node_id) {
            Ok(id) => id,
            // An out-of-range ID is a caller fault; emit the bare
            // `StorageError` text verbatim (`db_error` would wrap it in
            // `Error::Storage`, prefixing "Storage error: " — a message
            // regression vs pre-#3234 responses).
            Err(e) => return self.invalid_argument(&e.to_string()),
        };

        let temporal = match self
            .resolve_bitemporal_as_of(&req.as_of_valid_time, &req.as_of_transaction_time)
        {
            Ok(t) => t,
            Err(result) => return result,
        };

        // Apply resource limits to prevent DoS. A page must be able to carry a
        // continuation cursor, so the limit is at least 1 (limit:0 would
        // otherwise report has_more:true with next_offset==offset, a
        // non-progressing page that traps a paginating caller in a loop).
        let depth = req.depth.unwrap_or(1).min(MAX_TRAVERSAL_DEPTH);
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .clamp(1, MAX_RESULT_LIMIT);
        let offset = req.offset.unwrap_or(0).min(MAX_PAGINATION_OFFSET);
        let direction = req.direction.as_deref().unwrap_or("outgoing");
        // One request-scoped wallclock for every entity in the response
        // (Issue #3391).
        let now = time::now();

        let (results, has_more) = self.run_traversal(
            start_id,
            &req.edge_label,
            direction,
            depth,
            limit,
            offset,
            temporal,
            req.include_vectors.unwrap_or(false),
            now,
        );

        let count = results.len();
        let mut response = match temporal {
            Some((vt, tt)) => json!({
                "results": results,
                "count": count,
                "as_of_valid_time": time::to_iso8601(vt),
                "as_of_transaction_time": time::to_iso8601(tt),
            }),
            None => json!({
                "results": results,
                "count": count
            }),
        };
        // The matching total would require exhausting the traversal, so
        // `total_matching` is omitted; `has_more`/`next_offset` carry the
        // completeness signal.
        Self::attach_completeness(&mut response, offset, count, has_more, None);
        self.success_json(response)
    }

    /// Cursor-mode `traverse` (Issue #3360): snapshot-pinned offset
    /// continuation. On the first page the bi-temporal snapshot is pinned
    /// (to the request's `as_of_*` coordinate, or to "now" if none was given,
    /// so a current-state traversal still becomes a consistent point-in-time
    /// scan for the duration of the cursor). Every continuation re-walks the
    /// deterministic DFS as of that pinned snapshot and skips the already-seen
    /// prefix, so all pages reflect one consistent moment.
    pub(super) fn handle_traverse_cursor(&self, args: &serde_json::Value) -> CallToolResult {
        let now = time::now();

        // Resolve page parameters, start node, and pinned snapshot, from the
        // token when resuming or from the request on the first page.
        let (
            start_id,
            edge_label,
            direction,
            depth,
            limit,
            offset,
            include_vectors,
            snapshot,
            parent_cid,
        ) = if let Some(token) = args.get("cursor").and_then(|v| v.as_str()) {
            let payload = match self.cursors.decode(token, "traverse") {
                Ok(p) => p,
                Err(e) => return self.error_result(e),
            };
            let f = &payload.filters;
            let start_id = match NodeId::new(f["start_node_id"].as_u64().unwrap_or(0)) {
                Ok(id) => id,
                Err(e) => return self.invalid_argument(&e.to_string()),
            };
            (
                start_id,
                f["edge_label"].as_str().unwrap_or("").to_string(),
                f["direction"].as_str().unwrap_or("outgoing").to_string(),
                f["depth"].as_u64().unwrap_or(1) as usize,
                payload.limit,
                payload.off as usize,
                f["include_vectors"].as_bool().unwrap_or(false),
                (Timestamp::from(payload.svt), Timestamp::from(payload.stt)),
                payload.cid,
            )
        } else {
            // First page: parse the request and pin the snapshot. If no as_of
            // was supplied, anchor at "now" so the whole scan is consistent.
            let start_id = match NodeId::new(
                args.get("start_node_id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            ) {
                Ok(id) => id,
                Err(e) => return self.invalid_argument(&e.to_string()),
            };
            let temporal = match self.resolve_bitemporal_as_of(
                &Self::arg_str(args, "as_of_valid_time"),
                &Self::arg_str(args, "as_of_transaction_time"),
            ) {
                Ok(t) => t,
                Err(result) => return result,
            };
            let snapshot = temporal.unwrap_or((now, now));
            (
                start_id,
                Self::arg_str(args, "edge_label").unwrap_or_default(),
                Self::arg_str(args, "direction").unwrap_or_else(|| "outgoing".into()),
                args.get("depth")
                    .and_then(|v| v.as_u64())
                    .map(|d| d as usize)
                    .unwrap_or(1)
                    .min(MAX_TRAVERSAL_DEPTH),
                Self::arg_limit(args),
                0usize,
                args.get("include_vectors")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                snapshot,
                String::new(),
            )
        };

        let (results, has_more) = self.run_traversal(
            start_id,
            &edge_label,
            &direction,
            depth,
            limit,
            offset,
            Some(snapshot),
            include_vectors,
            now,
        );
        let count = results.len();

        let mut obj = serde_json::Map::new();
        obj.insert(
            "results".to_string(),
            serde_json::to_value(&results).unwrap_or_else(|_| json!([])),
        );
        obj.insert("count".to_string(), json!(count));
        obj.insert(
            "snapshot_valid_time".to_string(),
            json!(Self::format_timestamp_rfc3339(snapshot.0)),
        );
        obj.insert(
            "snapshot_transaction_time".to_string(),
            json!(Self::format_timestamp_rfc3339(snapshot.1)),
        );
        obj.insert("has_more".to_string(), json!(has_more));
        obj.insert("paging".to_string(), json!("cursor"));

        if has_more {
            let filters = json!({
                "start_node_id": start_id.as_u64(),
                "edge_label": edge_label,
                "direction": direction,
                "depth": depth,
                "include_vectors": include_vectors,
            });
            let mut payload = CursorPayload::seed(
                "traverse",
                (snapshot.0.wallclock(), snapshot.1.wallclock()),
                limit,
                filters,
            );
            // Continuation offset advances by the number of rows ACTUALLY
            // EMITTED (`count`). When #3353 token budgets land and can trim a
            // page short of `limit`, `off` MUST advance by the post-trim row
            // count (not `limit`), or the next page would skip trimmed rows.
            payload.off = (offset + count) as u64;
            payload.cid = parent_cid;
            match self.cursors.issue(payload) {
                Ok(token) => {
                    obj.insert("cursor".to_string(), json!(token));
                    obj.insert(
                        "cursor_ttl_seconds".to_string(),
                        json!(self.cursors.ttl().as_secs()),
                    );
                }
                Err(e) => return self.error_result(e),
            }
        }

        self.success_json(serde_json::Value::Object(obj))
    }
}
