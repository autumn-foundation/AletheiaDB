//! Token-budget-aware response shaping with disclosed truncation contracts
//! (Issue #3353).
//!
//! An LLM's context window is its scarcest resource, yet MCP read tools size
//! their responses by *row count* (`limit`), not by *cost*. This module lets a
//! caller pass a maximum response budget (`max_response_tokens` or the
//! byte-exact `max_response_bytes`) on any read tool and receive a response
//! *guaranteed* to fit it, with an explicit, machine-readable account of what
//! was reduced and a concrete follow-up call (a "fetch handle") that retrieves
//! exactly the omitted content.
//!
//! # Token estimation basis
//!
//! Tokens are estimated as `ceil(utf8_byte_len / 4)`. Four bytes per token is
//! the widely-used approximation of GPT/Claude-family BPE tokenizers for
//! English-plus-JSON text and holds within ~10% at the 1K-token scale. A caller
//! that needs an exact wire bound uses `max_response_bytes` instead, which is
//! enforced byte-for-byte. When both are supplied the *tighter* bound wins.
//!
//! # Degradation ladder (deterministic, disclosed per section)
//!
//! The response degrades along a fixed, ordered ladder; the same request at the
//! same budget on the same data always degrades identically:
//!
//! 1. **Full** — nothing reduced.
//! 2. **Elide bulky property values** — inside each entity's `properties`, any
//!    value whose serialized size exceeds a threshold (and is not a protected
//!    `priority_properties` key) is replaced with an `{elided: true, ...}`
//!    descriptor, mirroring the vector-elision convention of Issue #3220.
//! 3. **Per-entity summaries** — each entity's `properties` is reduced to the
//!    protected keys only (ids, labels, temporal coordinates, provenance and
//!    scores — the result *structure* — always survive because they are
//!    siblings of `properties`, never inside it).
//! 4. **Counts plus handles** — entity arrays are truncated to the prefix that
//!    fits; the omitted tail is disclosed as a count plus a fetch handle.
//!
//! Result *structure* (ids, labels, relationships, temporal coordinates,
//! provenance summaries, similarity scores) survives longest; bulky property
//! values are sacrificed first. `find_similar`/`hybrid_query` never reach rung 4
//! — their ranked results are never dropped or reordered to meet a budget; only
//! per-result payloads degrade (Issue #3353 AC7).

use serde_json::{Map, Value, json};

use super::error::{McpError, McpErrorCode};

/// Bytes-per-token divisor. See the module docs for the estimation basis.
pub(crate) const BYTES_PER_TOKEN: u64 = 4;

/// Property values whose pretty-serialized form exceeds this many bytes are
/// "bulky" and are the first content sacrificed (rung 2).
const BULKY_PROP_THRESHOLD_BYTES: usize = 48;

/// Parsed, validated budget parameters for one read request.
#[derive(Debug, Clone)]
pub(crate) struct BudgetRequest {
    /// Effective hard cap in bytes (the tighter of token*4 and byte bounds).
    effective_bytes: u64,
    /// Original requested token budget, echoed back for the caller.
    max_tokens: Option<u64>,
    /// Original requested byte budget, echoed back for the caller.
    max_bytes: Option<u64>,
    /// Property keys protected from elision at every rung.
    priority_properties: Vec<String>,
}

impl BudgetRequest {
    fn effective_bytes(&self) -> u64 {
        self.effective_bytes
    }
}

/// Which rung of the degradation ladder was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rung {
    Full,
    ElideProperties,
    Summaries,
    CountsAndHandles,
}

impl Rung {
    fn as_str(self) -> &'static str {
        match self {
            Rung::Full => "full",
            Rung::ElideProperties => "elided_properties",
            Rung::Summaries => "entity_summaries",
            Rung::CountsAndHandles => "counts_and_handles",
        }
    }
}

/// Parse the optional budget parameters from a tool's raw arguments.
///
/// Returns `Ok(None)` when neither `max_response_tokens` nor
/// `max_response_bytes` is present (behavior is then unchanged — the caller
/// skips shaping entirely). Returns a structured `INVALID_ARGUMENT` error when a
/// budget field is present but malformed (wrong type, zero, or negative).
pub(crate) fn parse_budget(args: &Value) -> Result<Option<BudgetRequest>, McpError> {
    let obj = match args.as_object() {
        Some(o) => o,
        None => return Ok(None),
    };

    let max_tokens = parse_positive_u64(obj.get("max_response_tokens"), "max_response_tokens")?;
    let max_bytes = parse_positive_u64(obj.get("max_response_bytes"), "max_response_bytes")?;

    if max_tokens.is_none() && max_bytes.is_none() {
        return Ok(None);
    }

    let priority_properties = match obj.get("priority_properties") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => {
                        return Err(McpError::new(
                            McpErrorCode::InvalidArgument,
                            "priority_properties must be an array of strings",
                        ));
                    }
                }
            }
            out
        }
        Some(_) => {
            return Err(McpError::new(
                McpErrorCode::InvalidArgument,
                "priority_properties must be an array of strings",
            ));
        }
    };

    let from_tokens = max_tokens.map(|t| t.saturating_mul(BYTES_PER_TOKEN));
    let effective_bytes = match (from_tokens, max_bytes) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => unreachable!("guarded above"),
    };

    Ok(Some(BudgetRequest {
        effective_bytes,
        max_tokens,
        max_bytes,
        priority_properties,
    }))
}

fn parse_positive_u64(value: Option<&Value>, field: &str) -> Result<Option<u64>, McpError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_u64() {
            Some(0) => Err(McpError::new(
                McpErrorCode::InvalidArgument,
                format!("{field} must be a positive integer"),
            )),
            Some(n) => Ok(Some(n)),
            None => Err(McpError::new(
                McpErrorCode::InvalidArgument,
                format!("{field} must be a positive integer"),
            )),
        },
    }
}

/// Serialize exactly as [`AletheiaMcpServer::success_json`] does, so the byte
/// measurement here is the byte length actually emitted on the wire.
fn measured_bytes(value: &Value) -> usize {
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|_| value.to_string())
        .len()
}

/// Shape a successful response value to fit `budget`, attaching a disclosed
/// `budget` metadata block describing the rung applied.
///
/// `tool` is the tool name; it selects the entity fetch-handle tool and whether
/// the tool is *ranked* (ranked tools never reach the counts-and-handles rung).
///
/// Returns the shaped value on success, or a structured `INVALID_ARGUMENT`
/// error naming the minimum viable budget when even the minimal rung cannot fit
/// (Issue #3353 AC6) — never a silently emptied success.
pub(crate) fn shape_response(
    value: Value,
    budget: &BudgetRequest,
    tool: &str,
) -> Result<Value, McpError> {
    // Only objects carry the read-tool response shape; anything else is passed
    // through with the metadata attached if it fits, else surfaced as too-small.
    let ranked = is_ranked_tool(tool);
    let cap = budget.effective_bytes();

    let ladder: &[Rung] = if ranked {
        &[Rung::Full, Rung::ElideProperties, Rung::Summaries]
    } else {
        &[
            Rung::Full,
            Rung::ElideProperties,
            Rung::Summaries,
            Rung::CountsAndHandles,
        ]
    };

    let mut last_candidate: Option<(Value, usize)> = None;
    for &rung in ladder {
        let candidate = build_candidate(value.clone(), rung, budget, tool, cap);
        let bytes = measured_bytes(&candidate);
        if bytes as u64 <= cap {
            return Ok(candidate);
        }
        last_candidate = Some((candidate, bytes));
    }

    // Even the minimal rung overflows: report the minimum viable budget so the
    // caller can re-issue with a sufficient budget (AC6).
    let min_bytes = last_candidate.map(|(_, b)| b).unwrap_or(0);
    let min_tokens = min_bytes.div_ceil(BYTES_PER_TOKEN as usize);
    Err(McpError::new(
        McpErrorCode::InvalidArgument,
        format!(
            "requested budget is too small to return even the minimal response for this request; \
             minimum viable budget is approximately {min_tokens} tokens ({min_bytes} bytes)"
        ),
    )
    .details(json!({
        "min_viable_tokens": min_tokens,
        "min_viable_bytes": min_bytes,
        "requested_tokens": budget.max_tokens,
        "requested_bytes": budget.max_bytes,
    })))
}

fn is_ranked_tool(tool: &str) -> bool {
    matches!(tool, "find_similar" | "hybrid_query")
}

/// Build a candidate response value at `rung`, with the disclosed `budget`
/// metadata block attached.
fn build_candidate(
    mut value: Value,
    rung: Rung,
    budget: &BudgetRequest,
    tool: &str,
    cap: u64,
) -> Value {
    let mut sections: Vec<Value> = Vec::new();
    match rung {
        Rung::Full => {}
        Rung::ElideProperties => {
            let n = elide_bulky_properties(&mut value, &budget.priority_properties);
            if n > 0 {
                sections.push(json!({
                    "section": "properties",
                    "rung": "elided_properties",
                    "elided_values": n,
                }));
            }
        }
        Rung::Summaries => {
            let n = summarize_entities(&mut value, &budget.priority_properties);
            if n > 0 {
                sections.push(json!({
                    "section": "properties",
                    "rung": "entity_summaries",
                    "entities_summarized": n,
                }));
            }
        }
        Rung::CountsAndHandles => {
            // First reduce every entity to its summary, then truncate arrays.
            summarize_entities(&mut value, &budget.priority_properties);
            let truncations = truncate_arrays_to_fit(&mut value, tool, cap);
            for t in truncations {
                sections.push(t);
            }
            sections.push(json!({
                "section": "properties",
                "rung": "entity_summaries",
            }));
        }
    }

    if let Value::Object(map) = &mut value {
        map.insert(
            "budget".to_string(),
            json!({
                "applied": true,
                "rung": rung.as_str(),
                "token_estimation_basis": "ceil(utf8_bytes / 4)",
                "requested_max_tokens": budget.max_tokens,
                "requested_max_bytes": budget.max_bytes,
                "effective_max_bytes": cap,
                "priority_properties": budget.priority_properties,
                "sections": sections,
            }),
        );
    }
    value
}

/// Does this object look like an entity carrying a `properties` object?
fn is_entity_object(map: &Map<String, Value>) -> bool {
    map.get("properties").map(Value::is_object).unwrap_or(false)
}

/// Build the concrete fetch handle that retrieves this entity's full content.
fn entity_fetch_handle(map: &Map<String, Value>) -> Value {
    let is_edge = map.contains_key("source_id") || map.contains_key("target_id");
    let id = map.get("id").cloned().unwrap_or(Value::Null);
    if is_edge {
        json!({
            "tool": "get_edge",
            "arguments": { "edge_id": id, "include_vectors": true },
        })
    } else {
        json!({
            "tool": "get_node",
            "arguments": { "node_id": id, "include_vectors": true },
        })
    }
}

/// Rung 2: elide bulky, unprotected property *values* in place. Returns the
/// number of values elided.
fn elide_bulky_properties(value: &mut Value, priority: &[String]) -> usize {
    let mut count = 0;
    match value {
        Value::Object(map) => {
            if is_entity_object(map) {
                let handle = entity_fetch_handle(map);
                if let Some(Value::Object(props)) = map.get_mut("properties") {
                    for (key, val) in props.iter_mut() {
                        if priority.iter().any(|p| p == key) {
                            continue;
                        }
                        if already_elided(val) {
                            continue;
                        }
                        let size = measured_bytes(val);
                        if size > BULKY_PROP_THRESHOLD_BYTES {
                            *val = json!({
                                "elided": true,
                                "reason": "budget",
                                "type": json_type_name(val),
                                "size_bytes": size,
                                "fetch": handle.clone(),
                            });
                            count += 1;
                        }
                    }
                }
            }
            // Recurse into all children to reach nested entity objects
            // (traverse results, query rows, history versions, ...).
            for (_k, child) in map.iter_mut() {
                count += elide_bulky_properties(child, priority);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                count += elide_bulky_properties(item, priority);
            }
        }
        _ => {}
    }
    count
}

/// Rung 3: reduce each entity's `properties` to protected keys only. Returns the
/// number of entities summarized (i.e. that dropped at least one property).
fn summarize_entities(value: &mut Value, priority: &[String]) -> usize {
    let mut count = 0;
    match value {
        Value::Object(map) => {
            if is_entity_object(map) {
                let handle = entity_fetch_handle(map);
                if let Some(Value::Object(props)) = map.get_mut("properties") {
                    let original_len = props.len();
                    let mut kept = Map::new();
                    for (key, val) in props.iter() {
                        if priority.iter().any(|p| p == key) {
                            kept.insert(key.clone(), val.clone());
                        }
                    }
                    let dropped = original_len - kept.len();
                    if dropped > 0 {
                        kept.insert(
                            "_budget_omitted".to_string(),
                            json!({
                                "omitted_properties": dropped,
                                "reason": "budget",
                                "fetch": handle,
                            }),
                        );
                        count += 1;
                    }
                    *props = kept;
                }
            }
            for (_k, child) in map.iter_mut() {
                count += summarize_entities(child, priority);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                count += summarize_entities(item, priority);
            }
        }
        _ => {}
    }
    count
}

/// Rung 4: truncate top-level entity arrays so the whole response fits `cap`.
/// Returns one disclosure section per truncated array. Deterministic: elements
/// are kept as a prefix (stable order) and only the tail is dropped.
fn truncate_arrays_to_fit(value: &mut Value, tool: &str, cap: u64) -> Vec<Value> {
    let mut disclosures = Vec::new();
    let map = match value {
        Value::Object(m) => m,
        _ => return disclosures,
    };

    // Identify top-level fields that are arrays of entity objects, largest first
    // for deterministic, greatest-impact-first truncation.
    let mut array_fields: Vec<(String, usize)> = map
        .iter()
        .filter_map(|(k, v)| match v {
            Value::Array(items) if items.iter().any(is_entity_array_element) => {
                Some((k.clone(), measured_bytes(v)))
            }
            _ => None,
        })
        .collect();
    array_fields.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    for (field, _) in array_fields {
        if measured_bytes(&Value::Object(map.clone())) as u64 <= cap {
            break;
        }
        let original_len = match map.get(&field) {
            Some(Value::Array(items)) => items.len(),
            _ => continue,
        };
        // Binary search the largest prefix length that keeps the whole object
        // within budget.
        let mut lo = 0usize;
        let mut hi = original_len;
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            set_array_prefix(map, &field, mid);
            if measured_bytes(&Value::Object(map.clone())) as u64 <= cap {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        set_array_prefix(map, &field, lo);
        let omitted = original_len - lo;
        if omitted > 0 {
            disclosures.push(json!({
                "section": field,
                "rung": "counts_and_handles",
                "returned": lo,
                "omitted_count": omitted,
                "fetch": {
                    "tool": tool,
                    "reason": "budget_truncated",
                    "hint": "re-request without max_response_tokens/max_response_bytes, \
                             or page the remainder using offset/next_offset, to retrieve \
                             the omitted results",
                },
            }));
        }
    }
    disclosures
}

/// Truncate `map[field]` (an array) to its first `len` elements in place.
fn set_array_prefix(map: &mut Map<String, Value>, field: &str, len: usize) {
    if let Some(Value::Array(items)) = map.get_mut(field) {
        items.truncate(len);
    }
}

/// Is this array element an entity object (directly or via a nested wrapper)?
fn is_entity_array_element(item: &Value) -> bool {
    match item {
        Value::Object(map) => is_entity_object(map) || map.values().any(is_entity_array_element),
        _ => false,
    }
}

fn already_elided(value: &Value) -> bool {
    value
        .as_object()
        .and_then(|m| m.get("elided"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
