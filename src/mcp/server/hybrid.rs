use super::*;

use crate::mcp::AletheiaMcpServer;

use crate::mcp::server::CallToolResult;

impl AletheiaMcpServer {
    pub(super) fn handle_hybrid_query(&self, args: serde_json::Value) -> CallToolResult {
        let req: HybridQueryRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        // Apply resource limits
        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .min(MAX_RESULT_LIMIT);
        let depth = req.traverse_depth.unwrap_or(1).min(MAX_TRAVERSAL_DEPTH);
        let k = req.top_k.unwrap_or(DEFAULT_VECTOR_K).min(MAX_VECTOR_K);

        // Parse temporal parameters if provided
        let valid_time = if let Some(ref vt) = req.valid_time {
            match self.parse_timestamp(vt) {
                Ok(t) => Some(t),
                Err(e) => return self.invalid_argument(&format!("Invalid valid_time: {}", e)),
            }
        } else {
            None
        };

        let tx_time = if let Some(ref tt) = req.transaction_time {
            match self.parse_timestamp(tt) {
                Ok(t) => Some(t),
                Err(e) => {
                    return self.invalid_argument(&format!("Invalid transaction_time: {}", e));
                }
            }
        } else {
            None
        };

        let include_vectors = req.include_vectors.unwrap_or(false);

        // One request-scoped wallclock for every entity in the response
        // (Issue #3391).
        let now = time::now();

        // Helper to convert rows to hybrid results with temporal info
        let rows_to_results =
            |rows: Vec<crate::query::executor::QueryRow>| -> Vec<HybridQueryResult> {
                rows.into_iter()
                    .filter_map(|row| {
                        if let EntityResult::Node(node) = row.entity {
                            Some(HybridQueryResult {
                                node: self.node_to_response(&node, include_vectors, now),
                                similarity_score: row.score,
                                traversal_path: row.path.map(|p| {
                                    p.iter()
                                        .map(|e| match e {
                                            ResultEntityId::Node(id) => id.as_u64(),
                                            ResultEntityId::Edge(id) => id.as_u64(),
                                        })
                                        .collect()
                                }),
                                timestamp: row.timestamp.map(|t| t.wallclock().to_string()),
                            })
                        } else {
                            None
                        }
                    })
                    .collect()
            };

        // Use QueryBuilder for hybrid queries
        if let Some(start_id) = req.start_node_id {
            let node_id = match NodeId::new(start_id) {
                Ok(id) => id,
                // Bare StorageError text verbatim — see the note on the other
                // ID-validation sites.
                Err(e) => return self.invalid_argument(&e.to_string()),
            };

            // If temporal filtering requested, use temporal query
            if let (Some(vt), Some(tt)) = (valid_time, tx_time) {
                // Temporal query for a single node
                return match self.db.get_node_at_time(node_id, vt, tt) {
                    Ok(node) => {
                        let response = self.node_to_response(&node, include_vectors, now);
                        self.success_json(json!({
                            "results": [HybridQueryResult {
                                node: response,
                                similarity_score: None,
                                traversal_path: Some(vec![node_id.as_u64()]),
                                timestamp: Some(vt.wallclock().to_string()),
                            }],
                            "count": 1,
                            "temporal_query": {
                                "valid_time": req.valid_time,
                                "transaction_time": req.transaction_time
                            }
                        }))
                    }
                    Err(e) => self.db_error(e),
                };
            }

            // Graph-first query with optional vector ranking
            let builder = crate::query::QueryBuilder::new().start(node_id);

            let builder = if let Some(ref edge_label) = req.traverse_edge {
                if depth > 1 {
                    builder.traverse_n(edge_label, depth)
                } else {
                    builder.traverse(edge_label)
                }
            } else {
                // Just return the start node
                return match self.db.get_node(node_id) {
                    Ok(node) => {
                        let response = self.node_to_response(&node, include_vectors, now);
                        self.success_json(json!({
                            "results": [HybridQueryResult {
                                node: response,
                                similarity_score: None,
                                traversal_path: Some(vec![node_id.as_u64()]),
                                timestamp: None,
                            }],
                            "count": 1
                        }))
                    }
                    Err(e) => self.db_error(e),
                };
            };

            // Execute and collect results
            match builder.limit(limit).execute(&self.db) {
                Ok(results) => match results.collect_all() {
                    Ok(rows) => {
                        let hybrid_results = rows_to_results(rows);
                        self.success_json(json!({
                            "results": hybrid_results,
                            "count": hybrid_results.len()
                        }))
                    }
                    Err(e) => self.db_error(e),
                },
                Err(e) => self.db_error(e),
            }
        } else if let Some(ref embedding) = req.query_embedding {
            // Vector-first query
            // Use vector_property if specified
            let property_name = req.vector_property.as_deref().unwrap_or("embedding");

            // Check if vector index is enabled for the property
            if !self.db.is_vector_index_enabled_for(property_name) {
                return self.failed_precondition(&format!(
                    "Vector index not enabled for property '{}'. Use enable_vector_index first.",
                    property_name
                ));
            }

            // Validate embedding dimensions
            if let Err(e) = self.validate_embedding_dimensions(embedding, property_name) {
                return self.invalid_argument(&e);
            }

            let builder = crate::query::QueryBuilder::new().find_similar(embedding, k);

            match builder.limit(limit).execute(&self.db) {
                Ok(results) => match results.collect_all() {
                    Ok(rows) => {
                        let hybrid_results = rows_to_results(rows);
                        self.success_json(json!({
                            "results": hybrid_results,
                            "count": hybrid_results.len(),
                            "vector_property": property_name
                        }))
                    }
                    Err(e) => self.db_error(e),
                },
                Err(e) => self.db_error(e),
            }
        } else if let Some(ref label) = req.filter_label {
            // Label scan query
            let builder = crate::query::QueryBuilder::new().scan_label(label);

            match builder.limit(limit).execute(&self.db) {
                Ok(results) => match results.collect_all() {
                    Ok(rows) => {
                        let hybrid_results = rows_to_results(rows);
                        self.success_json(json!({
                            "results": hybrid_results,
                            "count": hybrid_results.len()
                        }))
                    }
                    Err(e) => self.db_error(e),
                },
                Err(e) => self.db_error(e),
            }
        } else {
            self.invalid_argument(
                "Must specify either start_node_id, query_embedding, or filter_label",
            )
        }
    }

    pub(super) fn handle_query(&self, args: serde_json::Value) -> CallToolResult {
        // Extract language early so it can appear in error payloads even when
        // full deserialization fails (language is not yet known at that point).
        let raw_language = args
            .get("language")
            .and_then(|v| v.as_str())
            .map(|s| s.to_ascii_lowercase());

        // Cursor paging is not supported for the declarative query tool in v1
        // (Issue #3360); captured before `args` is consumed by deserialization.
        let cursor_requested = Self::cursor_requested(&args);

        let req: QueryRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => {
                return self.query_error(
                    "invalid_request",
                    &format!("Invalid arguments: {e}"),
                    None,
                    raw_language.as_deref(),
                );
            }
        };

        let language = req.language.to_ascii_lowercase();
        if language != "cypher" && language != "aql" {
            return self.query_error(
                "invalid_request",
                &format!(
                    "Unsupported query language '{}'. Use \"cypher\" or \"aql\".",
                    req.language
                ),
                None,
                Some(&language),
            );
        }

        // Cursor paging (Issue #3360) is not supported for the declarative
        // query tool in v1: arbitrary result shapes (projections, aggregates,
        // ordering) have no snapshot-anchored keyset to page over. Return a
        // structured `unsupported_construct` error rather than silently
        // serving a single truncated page (AC7: no silent fallback). Callers
        // needing consistent, resumable scans use `list_nodes` /
        // `find_nodes_at_time`, which are cursor-paged.
        if cursor_requested {
            return self.query_error(
                "unsupported_construct",
                "Cursor paging is not supported for the `query` tool in v1. Use `list_nodes` or \
                 `find_nodes_at_time` for snapshot-anchored, resumable cursor scans; the `query` \
                 tool returns a single (optionally `limit`-bounded) result set.",
                None,
                Some(&language),
            );
        }

        // Read-only guard: reject mutating statements BEFORE any execution.
        // Runs for every language so the tool can never write, even if the
        // grammars later gain write support.
        if let Some(clause) = detect_mutating_clause(&req.query) {
            return self.query_error(
                "read_only_violation",
                &format!(
                    "The `query` tool is read-only; the `{clause}` clause would mutate state and \
                     is rejected before execution."
                ),
                Some(clause),
                Some(&language),
            );
        }

        let limit = req
            .limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .min(MAX_RESULT_LIMIT);
        let has_params = req.params.as_ref().is_some_and(|p| !p.is_empty());

        let execution = match language.as_str() {
            "aql" => {
                if has_params {
                    return self.query_error(
                        "invalid_request",
                        "AQL does not support parameter bindings; inline literal values or use \
                         language \"cypher\" with $params.",
                        None,
                        Some("aql"),
                    );
                }
                self.db.execute_aql(&req.query)
            }
            "cypher" => {
                #[cfg(feature = "cypher")]
                {
                    match self.json_to_cypher_params(req.params.as_ref()) {
                        Ok(params) if params.is_empty() => self.db.execute_cypher(&req.query),
                        Ok(params) => self.db.execute_cypher_with_params(&req.query, params),
                        Err((parameter, reason)) => {
                            return self.query_error(
                                "invalid_params",
                                &format!("Invalid parameter '{parameter}': {reason}"),
                                None,
                                Some("cypher"),
                            );
                        }
                    }
                }
                #[cfg(not(feature = "cypher"))]
                {
                    return self.query_error(
                        "language_unavailable",
                        "Cypher support is not compiled in (enable the `cypher` feature). Use \
                         language \"aql\" instead.",
                        None,
                        Some("cypher"),
                    );
                }
            }
            _ => unreachable!("language already validated above"),
        };

        let results = match execution {
            Ok(results) => results,
            Err(e) => return self.map_query_error(e, &language),
        };

        // Collect one extra row to detect (and report) truncation at the cap.
        let collected = match results.take_n(limit.saturating_add(1)) {
            Ok(rows) => rows,
            Err(e) => return self.map_query_error(e, &language),
        };
        let truncated = collected.len() > limit;
        let rows: Vec<serde_json::Value> = collected
            .into_iter()
            .take(limit)
            .map(|row| self.query_row_to_json(row))
            .collect();
        let row_count = rows.len();

        self.success_json(json!({
            "language": language,
            "columns": query_columns(),
            "rows": rows,
            "row_count": row_count,
            "truncated": truncated,
        }))
    }
}
