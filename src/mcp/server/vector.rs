use super::*;

use crate::mcp::AletheiaMcpServer;

use crate::mcp::server::CallToolResult;

impl AletheiaMcpServer {
    pub(super) fn handle_find_similar(&self, args: serde_json::Value) -> CallToolResult {
        let req: FindSimilarRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        // Apply resource limits. A page must be able to carry a continuation
        // cursor, so k is at least 1 (k:0 would otherwise report
        // has_more:true with next_offset==offset, a non-progressing page).
        let k = req.k.unwrap_or(DEFAULT_VECTOR_K).clamp(1, MAX_VECTOR_K);
        // Bound the total pagination window (offset+k) by MAX_VECTOR_K so the
        // over-fetch below never asks the vector index for more than
        // MAX_VECTOR_K+1 candidates regardless of offset -- otherwise a large
        // offset could force a search far past the MAX_VECTOR_K resource
        // budget the cap is meant to enforce. Vector-similarity pagination is
        // therefore bounded to the top MAX_VECTOR_K matches; an offset beyond
        // that horizon returns an empty, complete (`has_more: false`) page.
        let offset = req
            .offset
            .unwrap_or(0)
            .min(MAX_PAGINATION_OFFSET)
            .min(MAX_VECTOR_K.saturating_sub(k));

        if !self.db.is_vector_index_enabled_for(&req.property_name) {
            return self.failed_precondition(&format!(
                "Vector index not enabled for property '{}'. Use enable_vector_index first.",
                req.property_name
            ));
        }

        // Validate embedding dimensions
        if let Err(e) = self.validate_embedding_dimensions(&req.embedding, &req.property_name) {
            return self.invalid_argument(&e);
        }

        // Over-fetch one past the requested page (offset + k + 1, capped at
        // MAX_VECTOR_K + 1 by the offset bound above) so we can tell whether
        // more similar nodes exist beyond this page (`has_more`) without a
        // second query. The matching total would need a full index scan, so
        // `total_matching` is omitted.
        let fetch_k = k.saturating_add(offset).saturating_add(1);
        match self
            .db
            .similarity_search(crate::SimilarityQuery::from_embedding(req.embedding).k(fetch_k))
        {
            Ok(results) => {
                let has_more = results.len() > offset.saturating_add(k);
                let include_vectors = req.include_vectors.unwrap_or(false);
                // One request-scoped wallclock for every entity in the
                // response (Issue #3391).
                let now = time::now();
                let similarity_results: Vec<SimilarityResult> = results
                    .into_iter()
                    .skip(offset)
                    .take(k)
                    .filter_map(|(node_id, score)| {
                        self.db.get_node(node_id).ok().map(|node| SimilarityResult {
                            node: self.node_to_response(&node, include_vectors, now),
                            score,
                        })
                    })
                    .collect();

                let count = similarity_results.len();
                let mut response = json!({
                    "results": similarity_results,
                    "count": count
                });
                // `next_offset` advances by the requested window `k`, not the
                // (possibly smaller) resolved `count`: a since-deleted node
                // behind a stale vector-index entry is still one of the `k`
                // candidates this page consumed, so basing next_offset on
                // `count` would re-skip into already-consumed candidates and
                // duplicate a row on the next page.
                Self::attach_completeness(&mut response, offset, k, has_more, None);
                self.success_json(response)
            }
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_enable_vector_index(&self, args: serde_json::Value) -> CallToolResult {
        let req: EnableVectorIndexRequest = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return self.invalid_argument(&format!("Invalid arguments: {}", e)),
        };

        let distance_metric = match req.distance_metric.as_deref().unwrap_or("cosine") {
            "euclidean" => DistanceMetric::Euclidean,
            "dot" | "dot_product" => DistanceMetric::DotProduct,
            _ => DistanceMetric::Cosine,
        };

        let config = HnswConfig::new(req.dimensions, distance_metric);

        match self.db.enable_vector_index(&req.property_name, config) {
            Ok(()) => self.success_json(json!({
                "success": true,
                "property_name": req.property_name,
                "dimensions": req.dimensions,
                "distance_metric": req.distance_metric.unwrap_or_else(|| "cosine".to_string())
            })),
            Err(e) => self.db_error(e),
        }
    }

    pub(super) fn handle_list_vector_indexes(&self, _args: serde_json::Value) -> CallToolResult {
        let indexes = self.db.list_vector_indexes();
        let index_list: Vec<serde_json::Value> = indexes
            .into_iter()
            .map(|info| {
                json!({
                    "property_name": info.property_name,
                    "dimensions": info.dimensions,
                    "distance_metric": format!("{:?}", info.distance_metric)
                })
            })
            .collect();
        self.success_json(json!({
            "indexes": index_list,
            "count": index_list.len()
        }))
    }
}
