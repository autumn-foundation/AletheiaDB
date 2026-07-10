#[cfg(test)]
mod server_unit_tests {
    use std::sync::Arc;

    use crate::mcp::AletheiaMcpServer;
    use crate::core::error::{Error, QueryError};
    use crate::core::id::{EdgeId, NodeId};
    use crate::db::AletheiaDB;
    use crate::query::executor::{EntityResult, QueryRow};

    fn make_server() -> AletheiaMcpServer {
        AletheiaMcpServer::new(Arc::new(AletheiaDB::new().expect("db init")))
    }

    fn error_kind(server: &AletheiaMcpServer, err: Error) -> String {
        let result = server.map_query_error(err, "aql");
        let text = AletheiaMcpServer::extract_text(result);
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        val["error"]["kind"].as_str().unwrap_or("").to_string()
    }

    /// Like [`error_kind`], but returning the whole serialized `error` object
    /// so tests can assert `code`/`retriable` alongside `kind`.
    fn error_payload(server: &AletheiaMcpServer, err: Error) -> serde_json::Value {
        let result = server.map_query_error(err, "aql");
        let text = AletheiaMcpServer::extract_text(result);
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        val["error"].clone()
    }

    #[test]
    fn map_query_error_unsupported_feature_yields_unsupported_construct() {
        let server = make_server();
        let err = Error::Query(QueryError::UnsupportedFeature {
            feature: "DISTINCT".to_string(),
        });
        assert_eq!(error_kind(&server, err), "unsupported_construct");
    }

    #[test]
    fn map_query_error_invalid_parameter_yields_invalid_params() {
        let server = make_server();
        let err = Error::Query(QueryError::InvalidParameter {
            parameter: "p".to_string(),
            reason: "out of range".to_string(),
        });
        assert_eq!(error_kind(&server, err), "invalid_params");
    }

    #[test]
    fn map_query_error_execution_error_yields_runtime_error() {
        let server = make_server();
        let err = Error::Query(QueryError::ExecutionError {
            message: "boom".to_string(),
        });
        assert_eq!(error_kind(&server, err), "runtime_error");
    }

    #[test]
    fn map_query_error_other_variant_yields_runtime_error() {
        let server = make_server();
        // Error::Other is a variant not matched by any specific arm — falls through to `other`.
        let err = Error::Other("unexpected situation".to_string());
        assert_eq!(error_kind(&server, err), "runtime_error");
    }

    #[test]
    fn map_query_error_timeout_yields_retriable_unavailable_runtime_error() {
        // A timeout keeps the query tool's own `kind` contract
        // ("runtime_error") but is classified UNAVAILABLE/retriable from the
        // underlying engine error — and `retriable: true` must survive the
        // query-tool serialization path, not just the in-memory struct.
        let server = make_server();
        let error = error_payload(
            &server,
            Error::Query(QueryError::Timeout { duration_ms: 5000 }),
        );
        assert_eq!(error["kind"], "runtime_error", "got: {error}");
        assert_eq!(error["code"], "UNAVAILABLE", "got: {error}");
        assert_eq!(error["retriable"], true, "got: {error}");
    }

    #[test]
    fn query_row_to_json_node_id_variant() {
        let server = make_server();
        let row = QueryRow::from_entity(EntityResult::NodeId(NodeId::new(42).unwrap()));
        let json = server.query_row_to_json(row);
        assert_eq!(json["entity"]["type"].as_str(), Some("node"));
        assert_eq!(json["entity"]["id"].as_u64(), Some(42));
    }

    #[test]
    fn query_row_to_json_edge_id_variant() {
        let server = make_server();
        let row = QueryRow::from_entity(EntityResult::EdgeId(EdgeId::new(99).unwrap()));
        let json = server.query_row_to_json(row);
        assert_eq!(json["entity"]["type"].as_str(), Some("edge"));
        assert_eq!(json["entity"]["id"].as_u64(), Some(99));
    }

    #[test]
    fn handle_enable_unique_constraint_invalid_json_returns_error() {
        // Covers the `Err(e) => return self.invalid_argument(...)` parse-error arm of
        // handle_enable_unique_constraint (added for Issue #3218).  The public
        // `enable_unique_constraint(req)` API always serialises a valid struct,
        // so this arm is only reachable via the internal handle_ function.
        let server = make_server();
        let result = server.handle_enable_unique_constraint(serde_json::Value::Null);
        // Must be an error CallToolResult (is_error = Some(true))
        assert!(
            result.is_error.unwrap_or(false),
            "Null JSON input must produce an error result"
        );
    }

    #[test]
    fn timestamp_to_rfc3339_out_of_chrono_range_falls_back_to_micros() {
        // Coordinates outside chrono's representable range must render as
        // the raw-microseconds fallback instead of panicking. Timestamps up
        // to MAX_VALID_TIMESTAMP (i64::MAX - 1000 µs) are storable but far
        // beyond chrono's ~year-262143 ceiling.
        let ts = crate::core::temporal::Timestamp::from(i64::MAX - 1000);
        let rendered = AletheiaMcpServer::timestamp_to_rfc3339(ts);
        assert_eq!(rendered, format!("{}us", i64::MAX - 1000));

        // Sanity: an in-range coordinate renders as RFC3339, not the fallback.
        let ts = crate::core::temporal::Timestamp::from(1_614_556_800_000_000); // 2021-03-01
        let rendered = AletheiaMcpServer::timestamp_to_rfc3339(ts);
        assert_eq!(rendered, "2021-03-01T00:00:00.000000Z");
    }

    #[test]
    fn handle_temporal_extent_invalid_by_label_type_routes_through_invalid_argument() {
        // A mistyped argument (string instead of bool) must produce the
        // structured Issue #3234 error payload with code INVALID_ARGUMENT
        // (Issue #3238).
        let server = make_server();
        let result = server.handle_temporal_extent(serde_json::json!({"by_label": "yes"}));
        assert!(
            result.is_error.unwrap_or(false),
            "mistyped by_label must produce an error result"
        );
        let text = AletheiaMcpServer::extract_text(result);
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            val["error"]["code"], "INVALID_ARGUMENT",
            "mistyped by_label must classify as INVALID_ARGUMENT: {val}"
        );
        assert_eq!(
            val["error"]["retriable"], false,
            "INVALID_ARGUMENT must not be retriable: {val}"
        );
        assert!(
            val["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("Invalid arguments")),
            "message must preserve the free-text detail: {val}"
        );
    }

    #[test]
    fn handle_temporal_extent_null_args_behaves_like_no_arguments() {
        // The tool has no required arguments: an MCP call with the
        // `arguments` object omitted entirely (routed here as Null) must
        // succeed exactly like `{}`.
        let server = make_server();
        let result = server.handle_temporal_extent(serde_json::Value::Null);
        assert!(
            !result.is_error.unwrap_or(false),
            "temporal_extent with no arguments must succeed"
        );
        let text = AletheiaMcpServer::extract_text(result);
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(val["valid_time"]["earliest"].is_null());
        assert!(val["transaction_time"]["latest"].is_null());
    }

    #[test]
    fn query_row_to_json_null_binding_serializes_entity_as_json_null() {
        // A null binding from an unmatched OPTIONAL MATCH pattern must
        // surface as an explicit JSON null entity (row preserved).
        let server = make_server();
        let value = server.query_row_to_json(QueryRow::from_entity(EntityResult::Null));
        assert!(
            value["entity"].is_null(),
            "null binding must serialize as JSON null: {value}"
        );
        assert!(value["score"].is_null());
        assert!(value["path"].is_null());
    }
}
