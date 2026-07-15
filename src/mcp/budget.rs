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
//!    descriptor, mirroring the vector-elision convention of Issue #3220. A
//!    value is only elided when the descriptor is *smaller* than the value it
//!    replaces, so this rung can never enlarge the response (monotonic ladder).
//! 3. **Per-entity summaries** — each entity's `properties` is reduced to the
//!    protected keys only (ids, labels, temporal coordinates, provenance and
//!    scores — the result *structure* — always survive because they are
//!    siblings of `properties`, never inside it).
//! 4. **Counts plus handles** — entity arrays are truncated to the prefix that
//!    fits; the omitted tail is disclosed as a count plus a fetch handle, and
//!    the object's own pagination/count siblings (`count`/`row_count`/
//!    `has_more`/`next_offset`/`truncated`) are rewritten to describe the
//!    retained prefix so a paginating caller sees no gap and no duplicate.
//!
//! Result *structure* (ids, labels, relationships, temporal coordinates,
//! provenance summaries, similarity scores) survives longest; bulky property
//! values are sacrificed first. `find_similar`/`hybrid_query` never reach rung 4
//! — their ranked results are never dropped or reordered to meet a budget; only
//! per-result payloads degrade (Issue #3353 AC7).

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use super::error::{McpError, McpErrorCode};

/// Bytes-per-token divisor. See the module docs for the estimation basis.
pub(crate) const BYTES_PER_TOKEN: u64 = 4;

/// Default upper bound on the number of entries accepted in a
/// `priority_properties` array (Issue #3583, surfaced via #3353).
///
/// `priority_properties` is matched against every property of every returned
/// entity at the elide and summarize rungs. Left unbounded, an attacker could
/// pass a `priority_properties` array with up to hundreds of thousands of
/// entries (bounded only by the transport body cap), turning response shaping
/// into multi-second blocking CPU. The scan itself is now an O(1) `HashSet`
/// lookup, but the array is *still* rejected above this cap so the one-time cost
/// of building that set (and of validating/allocating the array) stays bounded.
///
/// Configurable per server via
/// [`AletheiaMcpServer::with_max_priority_properties`](crate::mcp::AletheiaMcpServer::with_max_priority_properties).
pub(crate) const DEFAULT_MAX_PRIORITY_PROPERTIES: usize = 1024;

/// Property values whose pretty-serialized form exceeds this many bytes are
/// "bulky" and are the first content sacrificed (rung 2). Set above the fixed
/// cost of an elision descriptor so eliding a value can only ever shrink the
/// response; an additional per-value `descriptor < value` guard makes the
/// monotonicity guarantee exact regardless of this constant (Issue #3353 F3).
const BULKY_PROP_THRESHOLD_BYTES: usize = 160;

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
    pub(crate) fn effective_bytes(&self) -> u64 {
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
///
/// An unknown/misspelled budget key (e.g. `max_tokens` instead of
/// `max_response_tokens`) is simply ignored — consistent with the surface's
/// unknown-field tolerance — so the request is served in full rather than
/// silently budgeted under a key the caller did not intend.
///
/// `max_priority_properties` bounds the length of the `priority_properties`
/// array (defense-in-depth against the Issue #3583 DoS): an array longer than
/// the cap is rejected with a structured `INVALID_ARGUMENT` error naming both
/// the cap and the given length, so re-issuing under the cap succeeds. The cap
/// is configurable per server via
/// [`AletheiaMcpServer::with_max_priority_properties`](crate::mcp::AletheiaMcpServer::with_max_priority_properties)
/// (default [`DEFAULT_MAX_PRIORITY_PROPERTIES`]).
pub(crate) fn parse_budget(
    args: &Value,
    max_priority_properties: usize,
) -> Result<Option<BudgetRequest>, McpError> {
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
            // Defense-in-depth (Issue #3583): reject an over-long array up front,
            // before allocating/validating each element, so an oversized array
            // cannot turn response shaping into unbounded work.
            if items.len() > max_priority_properties {
                return Err(McpError::new(
                    McpErrorCode::InvalidArgument,
                    format!(
                        "priority_properties may contain at most {max_priority_properties} \
                         entries, but {} were given",
                        items.len()
                    ),
                )
                .details(json!({
                    "max_priority_properties": max_priority_properties,
                    "given": items.len(),
                })));
            }
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

/// Byte cap enforcement for a payload that is not a shapeable read-tool object
/// (non-JSON text, or a JSON scalar/array). Such payloads cannot degrade along
/// the entity ladder, but the budget contract is unconditional: never emit text
/// exceeding the cap under an active budget (Issue #3353 F6).
///
/// Returns the (possibly truncated) string to emit, or `Err` when even a
/// minimal disclosed-truncation marker cannot fit — surfaced as the AC6
/// too-small error by the caller.
pub(crate) fn enforce_raw_cap(text: String, budget: &BudgetRequest) -> Result<String, McpError> {
    let cap = budget.effective_bytes() as usize;
    if text.len() <= cap {
        return Ok(text);
    }
    // Reserve room for a disclosed truncation marker so the caller is never
    // handed a silently clipped payload.
    const MARKER: &str = "\n…[truncated: response exceeded token budget]";
    if MARKER.len() >= cap {
        return Err(too_small_error(text.len(), budget));
    }
    let keep = cap - MARKER.len();
    // Truncate on a UTF-8 char boundary at or below `keep`.
    let mut boundary = keep;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let mut out = text[..boundary].to_string();
    out.push_str(MARKER);
    Ok(out)
}

fn too_small_error(min_bytes: usize, budget: &BudgetRequest) -> McpError {
    let min_tokens = min_bytes.div_ceil(BYTES_PER_TOKEN as usize);
    McpError::new(
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
    }))
}

/// Context threaded through the recursion so a fetch handle can be built for an
/// entity that lives inside a history wrapper (whose parent id is not on the
/// version object itself). Owns the parent id (cheaply cloned — an integer
/// `Value`) so it never borrows the map being mutated during shaping.
#[derive(Clone, Default)]
struct ShapeCtx {
    /// Parent entity id for a history-version object (`get_node_history` /
    /// `get_edge_history`); `None` for ordinary entity responses.
    history_parent_id: Option<Value>,
    /// Whether the history wrapper is an edge history (selects the
    /// `get_edge_at_time` fetch tool) rather than a node history.
    history_is_edge: bool,
}

/// Shape a successful response value to fit `budget`, attaching a disclosed
/// `budget` metadata block describing the rung applied.
///
/// `tool` is the tool name; it selects the entity fetch-handle tool and whether
/// the tool is *ranked* (ranked tools never reach the counts-and-handles rung).
/// `args` is the original request arguments, threaded so the rung-4 truncation
/// handle can emit a concrete `offset`-based resume call for paginated tools.
///
/// Returns the shaped value on success, or a structured `INVALID_ARGUMENT`
/// error naming the minimum viable budget when even the minimal rung cannot fit
/// (Issue #3353 AC6) — never a silently emptied success.
pub(crate) fn shape_response(
    value: Value,
    budget: &BudgetRequest,
    tool: &str,
    args: &Value,
) -> Result<Value, McpError> {
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

    for &rung in ladder {
        let candidate = build_candidate(value.clone(), rung, budget, tool, args, cap);
        let bytes = measured_bytes(&candidate);
        if bytes as u64 <= cap {
            return Ok(candidate);
        }
    }

    // Even the minimal rung overflows. Report a minimum viable budget the caller
    // can actually re-issue at. The reported number must self-consistently fit:
    // a larger budget makes the disclosed `budget` block's own numbers
    // (`effective_max_bytes`, `requested_max_tokens`) wider, so a naive "size at
    // the current tiny cap" underestimates. Iterate to a fixpoint, simulating
    // the caller re-issuing `max_response_tokens = min_tokens`, until the
    // minimal-rung candidate actually fits that budget (Issue #3353 AC6/T6).
    let minimal_rung = *ladder.last().expect("ladder is non-empty");
    let mut min_bytes = measured_bytes(&build_candidate(
        value.clone(),
        minimal_rung,
        budget,
        tool,
        args,
        cap,
    ));
    let mut min_tokens = (min_bytes as u64).div_ceil(BYTES_PER_TOKEN);
    for _ in 0..8 {
        let probe_cap = min_tokens.saturating_mul(BYTES_PER_TOKEN);
        let probe = BudgetRequest {
            effective_bytes: probe_cap,
            max_tokens: Some(min_tokens),
            max_bytes: None,
            priority_properties: budget.priority_properties.clone(),
        };
        let cand = build_candidate(value.clone(), minimal_rung, &probe, tool, args, probe_cap);
        let b = measured_bytes(&cand);
        min_bytes = b;
        if b as u64 <= probe_cap {
            break;
        }
        min_tokens = (b as u64).div_ceil(BYTES_PER_TOKEN);
    }
    // Report the fitting token budget (and its byte equivalent) so re-issuing at
    // `min_viable_tokens` — or `min_viable_bytes` — succeeds.
    let report_bytes = (min_tokens.saturating_mul(BYTES_PER_TOKEN)).max(min_bytes as u64) as usize;
    Err(too_small_error(report_bytes, budget))
}

fn is_ranked_tool(tool: &str) -> bool {
    // `semantic_search` reuses the exact `find_similar` k-NN path, so its
    // ranked `results` must enjoy the same protection: never dropped or
    // reordered to meet a token budget (Issue #2906).
    matches!(tool, "find_similar" | "hybrid_query" | "semantic_search")
}

/// Build a candidate response value at `rung`, with the disclosed `budget`
/// metadata block attached.
fn build_candidate(
    mut value: Value,
    rung: Rung,
    budget: &BudgetRequest,
    tool: &str,
    args: &Value,
    cap: u64,
) -> Value {
    let ctx = derive_ctx(&value, tool);
    // Build the protected-key lookup once per candidate (Issue #3583): the elide
    // and summarize rungs consult it per-property-per-entity, so an O(1)
    // `HashSet` lookup replaces the previous O(len(priority_properties)) linear
    // scan and keeps shaping cost independent of the array length.
    let priority: HashSet<&str> = budget
        .priority_properties
        .iter()
        .map(String::as_str)
        .collect();
    let mut sections: Vec<Value> = Vec::new();
    match rung {
        Rung::Full => {}
        Rung::ElideProperties => {
            let n = elide_bulky_properties(&mut value, &priority, &ctx);
            if n > 0 {
                sections.push(json!({
                    "section": "properties",
                    "rung": "elided_properties",
                    "elided_values": n,
                }));
            }
        }
        Rung::Summaries => {
            let n = summarize_entities(&mut value, &priority, &ctx);
            if n > 0 {
                sections.push(json!({
                    "section": "properties",
                    "rung": "entity_summaries",
                }));
            }
        }
        Rung::CountsAndHandles => {
            // First reduce every entity to its summary, then truncate arrays.
            summarize_entities(&mut value, &priority, &ctx);
            let mut sections_so_far = vec![json!({
                "section": "properties",
                "rung": "entity_summaries",
            })];
            // Truncation measures the fully-assembled candidate (object plus the
            // budget/disclosure block) against `cap`, so the finalized response
            // is guaranteed to fit even though the budget block is appended
            // afterward (Issue #3353 F4).
            let truncations =
                truncate_arrays_to_fit(&mut value, tool, args, cap, budget, &sections_so_far);
            sections_so_far.extend(truncations);
            sections = sections_so_far;
        }
    }

    attach_budget_block(&mut value, rung, budget, cap, sections);
    value
}

/// Attach the disclosed `budget` metadata block to a response object.
fn attach_budget_block(
    value: &mut Value,
    rung: Rung,
    budget: &BudgetRequest,
    cap: u64,
    sections: Vec<Value>,
) {
    if let Value::Object(map) = value {
        map.insert(
            "budget".to_string(),
            budget_block(rung, budget, cap, sections),
        );
    }
}

fn budget_block(rung: Rung, budget: &BudgetRequest, cap: u64, sections: Vec<Value>) -> Value {
    json!({
        "applied": true,
        "rung": rung.as_str(),
        "token_estimation_basis": "ceil(utf8_bytes / 4)",
        "requested_max_tokens": budget.max_tokens,
        "requested_max_bytes": budget.max_bytes,
        "effective_max_bytes": cap,
        "priority_properties": budget.priority_properties,
        "sections": sections,
    })
}

/// Establish the shaping context for the top-level response of `tool`: for a
/// history response, capture the parent entity id so version fetch handles can
/// address the exact superseded version via `get_node_at_time` /
/// `get_edge_at_time`.
fn derive_ctx(value: &Value, tool: &str) -> ShapeCtx {
    match tool {
        "get_node_history" => ShapeCtx {
            history_parent_id: value.get("node_id").cloned(),
            history_is_edge: false,
        },
        "get_edge_history" => ShapeCtx {
            history_parent_id: value.get("edge_id").cloned(),
            history_is_edge: true,
        },
        _ => ShapeCtx::default(),
    }
}

/// Does this object look like an entity carrying a `properties` object?
fn is_entity_object(map: &Map<String, Value>) -> bool {
    map.get("properties").map(Value::is_object).unwrap_or(false)
}

/// Does this object look like a bi-temporal history *version* (as emitted by
/// `version_info_to_response`) rather than a live entity? A version carries
/// `version_id` and its own valid/transaction coordinates, but no `id`.
fn is_history_version(map: &Map<String, Value>) -> bool {
    map.contains_key("version_id")
        && map.contains_key("valid_from")
        && map.contains_key("transaction_from")
        && !map.contains_key("id")
}

/// Build the concrete fetch handle that retrieves this entity's full content.
///
/// * A live node/edge → `get_node`/`get_edge` by id.
/// * A history version → `get_node_at_time`/`get_edge_at_time` addressing the
///   exact superseded version by the parent id (threaded via `ctx`) plus the
///   version's own valid/transaction coordinates (Issue #3353 F2). A plain
///   `get_node` would return the *current* state, not the historical version.
fn entity_fetch_handle(map: &Map<String, Value>, ctx: &ShapeCtx) -> Value {
    if is_history_version(map) {
        let parent = ctx.history_parent_id.clone().unwrap_or(Value::Null);
        let valid_time = map.get("valid_from").cloned().unwrap_or(Value::Null);
        let transaction_time = map.get("transaction_from").cloned().unwrap_or(Value::Null);
        if ctx.history_is_edge {
            return json!({
                "tool": "get_edge_at_time",
                "arguments": {
                    "edge_id": parent,
                    "valid_time": valid_time,
                    "transaction_time": transaction_time,
                },
            });
        }
        return json!({
            "tool": "get_node_at_time",
            "arguments": {
                "node_id": parent,
                "valid_time": valid_time,
                "transaction_time": transaction_time,
            },
        });
    }

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
/// number of values elided. A value is elided only when its `{elided,...}`
/// descriptor serializes *smaller* than the value it replaces, so this rung can
/// never enlarge the response (Issue #3353 F3).
fn elide_bulky_properties(value: &mut Value, priority: &HashSet<&str>, ctx: &ShapeCtx) -> usize {
    let mut count = 0;
    match value {
        Value::Object(map) => {
            if is_entity_object(map) {
                let handle = entity_fetch_handle(map, ctx);
                if let Some(Value::Object(props)) = map.get_mut("properties") {
                    for (key, val) in props.iter_mut() {
                        if priority.contains(key.as_str()) {
                            continue;
                        }
                        if already_elided(val) {
                            continue;
                        }
                        let size = measured_bytes(val);
                        if size <= BULKY_PROP_THRESHOLD_BYTES {
                            continue;
                        }
                        let descriptor = json!({
                            "elided": true,
                            "reason": "budget",
                            "type": json_type_name(val),
                            "size_bytes": size,
                            "fetch": handle.clone(),
                        });
                        // Monotonicity guard: never replace a value with a
                        // descriptor that is not strictly smaller than it.
                        if measured_bytes(&descriptor) < size {
                            *val = descriptor;
                            count += 1;
                        }
                    }
                }
            }
            // Recurse into all children to reach nested entity objects
            // (traverse results, query rows, history versions, ...). Thread the
            // history context so version handles keep addressing their parent.
            let child = child_ctx(map, ctx);
            for (_k, c) in map.iter_mut() {
                count += elide_bulky_properties(c, priority, &child);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                count += elide_bulky_properties(item, priority, ctx);
            }
        }
        _ => {}
    }
    count
}

/// Rung 3: reduce each entity's `properties` to protected keys only. Returns the
/// number of entities summarized (i.e. that dropped at least one property).
fn summarize_entities(value: &mut Value, priority: &HashSet<&str>, ctx: &ShapeCtx) -> usize {
    let mut count = 0;
    match value {
        Value::Object(map) => {
            if is_entity_object(map) {
                let handle = entity_fetch_handle(map, ctx);
                if let Some(Value::Object(props)) = map.get_mut("properties") {
                    let original_len = props.len();
                    let mut kept = Map::new();
                    for (key, val) in props.iter() {
                        if priority.contains(key.as_str()) {
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
            let child = child_ctx(map, ctx);
            for (_k, c) in map.iter_mut() {
                count += summarize_entities(c, priority, &child);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                count += summarize_entities(item, priority, ctx);
            }
        }
        _ => {}
    }
    count
}

/// Refine the shaping context when recursing into `map`'s children: a history
/// wrapper (`node_id`/`edge_id` + a `versions` array) establishes the parent id
/// used by its version fetch handles.
fn child_ctx(map: &Map<String, Value>, inherited: &ShapeCtx) -> ShapeCtx {
    if map.get("versions").map(Value::is_array).unwrap_or(false) {
        if let Some(id) = map.get("node_id") {
            return ShapeCtx {
                history_parent_id: Some(id.clone()),
                history_is_edge: false,
            };
        }
        if let Some(id) = map.get("edge_id") {
            return ShapeCtx {
                history_parent_id: Some(id.clone()),
                history_is_edge: true,
            };
        }
    }
    inherited.clone()
}

/// Rung 4: truncate top-level entity arrays so the whole *assembled* response
/// (object plus its budget/disclosure block) fits `cap`. Returns one disclosure
/// section per truncated array and rewrites the object's pagination/count
/// siblings so a paginating caller sees no gap and no duplicate (Issue #3353
/// F1). Deterministic: elements are kept as a prefix (stable order) and only the
/// tail is dropped.
fn truncate_arrays_to_fit(
    value: &mut Value,
    tool: &str,
    args: &Value,
    cap: u64,
    budget: &BudgetRequest,
    base_sections: &[Value],
) -> Vec<Value> {
    let mut disclosures = Vec::new();
    let map = match value {
        Value::Object(m) => m,
        _ => return disclosures,
    };

    // The original request offset (paginated tools only). Prefer the echoed
    // response `offset`, falling back to the request argument, defaulting to 0.
    let original_offset = map
        .get("offset")
        .and_then(Value::as_u64)
        .or_else(|| args.get("offset").and_then(Value::as_u64))
        .unwrap_or(0);

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
        let original_len = match map.get(&field) {
            Some(Value::Array(items)) => items.len(),
            _ => continue,
        };

        // Fast-exit when the fully-assembled candidate already fits with this
        // field untouched.
        if assembled_len(
            map,
            budget,
            cap,
            base_sections,
            &disclosures,
            &field,
            original_len,
        ) <= cap as usize
        {
            continue;
        }

        // Binary search the largest prefix length whose *assembled* candidate
        // (including the finalized budget block) stays within budget (F4).
        let mut lo = 0usize;
        let mut hi = original_len;
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            if assembled_len(map, budget, cap, base_sections, &disclosures, &field, mid)
                <= cap as usize
            {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        set_array_prefix(map, &field, lo);
        let omitted = original_len - lo;
        if omitted > 0 {
            let handle = truncation_fetch_handle(tool, args, original_offset, lo);
            disclosures.push(json!({
                "section": field,
                "rung": "counts_and_handles",
                "returned": lo,
                "omitted_count": omitted,
                "fetch": handle,
            }));
            rewrite_pagination_siblings(map, &field, lo, original_offset, tool);
        }
    }
    disclosures
}

/// Measure the assembled candidate (object + budget block) with `field`
/// tentatively truncated to `prefix_len`, without persisting the mutation to
/// `map`. Used inside the rung-4 search so the *finalized* response size —
/// including the budget and disclosure metadata appended afterward — is what is
/// bounded by `cap` (Issue #3353 F4).
fn assembled_len(
    map: &Map<String, Value>,
    budget: &BudgetRequest,
    cap: u64,
    base_sections: &[Value],
    prior_disclosures: &[Value],
    field: &str,
    prefix_len: usize,
) -> usize {
    let original_len = map
        .get(field)
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(prefix_len);
    let omitted = original_len.saturating_sub(prefix_len);

    let mut trial = map.clone();
    set_array_prefix(&mut trial, field, prefix_len);

    let mut sections: Vec<Value> = base_sections.to_vec();
    sections.extend(prior_disclosures.iter().cloned());
    if omitted > 0 {
        rewrite_pagination_siblings(&mut trial, field, prefix_len, 0, "");
        // A representative disclosure of maximal-ish size (concrete handle plus
        // counts) so the reserved headroom matches the finalized block.
        sections.push(json!({
            "section": field,
            "rung": "counts_and_handles",
            "returned": prefix_len,
            "omitted_count": omitted,
            "fetch": {
                "tool": "",
                "arguments": { "offset": u64::MAX },
                "reason": "budget_truncated",
                "hint": "re-request with a larger max_response_tokens/max_response_bytes, \
                         or page the remainder using offset, to retrieve the omitted results",
            },
        }));
    }
    trial.insert(
        "budget".to_string(),
        budget_block(Rung::CountsAndHandles, budget, cap, sections),
    );
    measured_bytes(&Value::Object(trial))
}

/// Build the rung-4 truncation fetch handle. Paginated tools (those that accept
/// an `offset`) get a concrete resume call advancing the offset past the
/// retained prefix; non-paginated tools get an honest disclosure that does not
/// reference an offset they do not have (Issue #3353 F5).
fn truncation_fetch_handle(
    tool: &str,
    args: &Value,
    original_offset: u64,
    retained: usize,
) -> Value {
    if is_offset_paginated_tool(tool) {
        let mut resume_args = args.as_object().cloned().unwrap_or_default();
        // Strip the budget knobs so the resume call returns the full remainder.
        resume_args.remove("max_response_tokens");
        resume_args.remove("max_response_bytes");
        resume_args.remove("priority_properties");
        resume_args.insert(
            "offset".to_string(),
            json!(original_offset.saturating_add(retained as u64)),
        );
        json!({
            "tool": tool,
            "arguments": Value::Object(resume_args),
            "reason": "budget_truncated",
            "hint": "call this to retrieve the omitted results, then continue paging with \
                     `next_offset`",
        })
    } else {
        json!({
            "tool": tool,
            "reason": "budget_truncated",
            "hint": "re-request with a larger max_response_tokens/max_response_bytes to retrieve \
                     the omitted results (this tool does not page by offset)",
        })
    }
}

/// Does this tool accept an `offset` argument for prefix pagination, so a
/// concrete offset-advancing resume handle is meaningful?
fn is_offset_paginated_tool(tool: &str) -> bool {
    matches!(tool, "list_nodes" | "traverse" | "find_nodes_at_time")
}

/// After truncating `field` to `retained` elements, rewrite the object's own
/// pagination/count siblings so they describe the retained prefix rather than
/// the original (untruncated) page (Issue #3353 F1). This prevents the
/// disclosed `next_offset` from skipping the dropped rows.
fn rewrite_pagination_siblings(
    map: &mut Map<String, Value>,
    field: &str,
    retained: usize,
    original_offset: u64,
    tool: &str,
) {
    // The count sibling co-located with each known array shape.
    let count_key = match field {
        "nodes" | "edges" | "results" => Some("count"),
        "rows" => Some("row_count"),
        _ => None,
    };
    if let Some(key) = count_key
        && map.contains_key(key)
    {
        map.insert(key.to_string(), json!(retained));
    }

    // A `query`-style `truncated` flag becomes true.
    if map.contains_key("truncated") {
        map.insert("truncated".to_string(), json!(true));
    }

    // Pagination cursor: mark `has_more` and, for offset-paginated tools,
    // advance `next_offset` past the retained prefix so a paginating caller
    // resumes exactly where the page was cut.
    if map.contains_key("has_more") {
        map.insert("has_more".to_string(), json!(true));
    }
    if is_offset_paginated_tool(tool) {
        map.insert(
            "next_offset".to_string(),
            json!(original_offset.saturating_add(retained as u64)),
        );
    }
    // `total_matching`, when present, already counts the full matching set and
    // remains correct; it is deliberately left unchanged.
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
        Value::Object(map) => {
            is_entity_object(map)
                || is_history_version(map)
                || map.values().any(is_entity_array_element)
        }
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

#[cfg(test)]
mod unit_tests {
    use super::*;

    fn budget_req(bytes: u64, priority: &[&str]) -> BudgetRequest {
        BudgetRequest {
            effective_bytes: bytes,
            max_tokens: None,
            max_bytes: Some(bytes),
            priority_properties: priority.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// F3: eliding several small (below-descriptor-cost) properties must never
    /// make the elide rung serialize larger than the full rung — the ladder is
    /// monotonic.
    #[test]
    fn f3_elide_rung_never_larger_than_full() {
        // Several ~60-90 byte string properties, each smaller than the elision
        // descriptor's fixed cost, so none should be elided.
        let node = json!({
            "id": 1,
            "label": "Doc",
            "properties": {
                "a": "a".repeat(60),
                "b": "b".repeat(75),
                "c": "c".repeat(90),
                "d": "d".repeat(80),
            }
        });
        let budget = budget_req(10_000, &[]);
        let args = json!({});
        let full = build_candidate(node.clone(), Rung::Full, &budget, "get_node", &args, 10_000);
        let elided = build_candidate(
            node.clone(),
            Rung::ElideProperties,
            &budget,
            "get_node",
            &args,
            10_000,
        );
        // Compare the payload excluding the disclosed `budget` metadata block:
        // its `rung` label alone is intrinsically longer for the elide rung
        // ("elided_properties" vs "full") and is not data the caller pays for
        // in content terms. What must never grow is the *data* itself.
        assert!(
            payload_bytes(&elided) <= payload_bytes(&full),
            "elide-rung payload ({}) must not exceed full-rung payload ({})",
            payload_bytes(&elided),
            payload_bytes(&full),
        );
        // None of the small props should have been elided.
        assert!(
            !json_contains_elided(&elided),
            "no property small enough to shrink should be elided"
        );

        // A genuinely bulky property, by contrast, IS elided and shrinks.
        let big = json!({
            "id": 2,
            "label": "Doc",
            "properties": { "bio": "x".repeat(4000) }
        });
        let big_full = build_candidate(big.clone(), Rung::Full, &budget, "get_node", &args, 10_000);
        let big_elided = build_candidate(
            big,
            Rung::ElideProperties,
            &budget,
            "get_node",
            &args,
            10_000,
        );
        assert!(json_contains_elided(&big_elided), "bulky prop must elide");
        assert!(measured_bytes(&big_elided) < measured_bytes(&big_full));
    }

    fn json_contains_elided(value: &Value) -> bool {
        match value {
            Value::Object(m) => {
                if m.get("elided").and_then(Value::as_bool).unwrap_or(false) {
                    return true;
                }
                m.values().any(json_contains_elided)
            }
            Value::Array(a) => a.iter().any(json_contains_elided),
            _ => false,
        }
    }

    /// Serialized size of a candidate with the disclosed `budget` metadata block
    /// removed, so payload-only (data) growth can be compared across rungs
    /// without the intrinsic difference in the rung label string.
    fn payload_bytes(value: &Value) -> usize {
        let mut v = value.clone();
        if let Value::Object(m) = &mut v {
            m.remove("budget");
        }
        measured_bytes(&v)
    }

    /// F2: a `get_node_history` version's fetch handle addresses the exact
    /// superseded version via `get_node_at_time` with the PARENT node id (from
    /// the wrapper) plus the version's own coordinates — never a null id.
    #[test]
    fn f2_history_version_handle_targets_parent_at_time() {
        let history = json!({
            "node_id": 42,
            "version_count": 1,
            "versions": [
                {
                    "version_id": 7,
                    "version_number": 1,
                    "valid_from": "2024-01-01T00:00:00Z",
                    "valid_to": null,
                    "transaction_from": "2024-01-02T00:00:00Z",
                    "transaction_to": null,
                    "label": "Person",
                    "properties": { "bio": "x".repeat(4000) }
                }
            ]
        });
        let budget = budget_req(600, &[]);
        let shaped = build_candidate(
            history,
            Rung::Summaries,
            &budget,
            "get_node_history",
            &json!({ "node_id": 42 }),
            600,
        );
        let handle = find_any_fetch(&shaped).expect("a version fetch handle must exist");
        assert_eq!(handle["tool"], json!("get_node_at_time"));
        assert_eq!(handle["arguments"]["node_id"], json!(42));
        assert_eq!(
            handle["arguments"]["valid_time"],
            json!("2024-01-01T00:00:00Z")
        );
        assert_eq!(
            handle["arguments"]["transaction_time"],
            json!("2024-01-02T00:00:00Z")
        );
    }

    fn find_any_fetch(value: &Value) -> Option<Value> {
        match value {
            Value::Object(m) => {
                if let Some(f) = m.get("fetch")
                    && f.get("tool").is_some()
                {
                    return Some(f.clone());
                }
                m.values().find_map(find_any_fetch)
            }
            Value::Array(a) => a.iter().find_map(find_any_fetch),
            _ => None,
        }
    }

    /// F6: a non-object payload is still held to the byte cap with a disclosed
    /// truncation marker rather than emitted unbounded.
    #[test]
    fn f6_enforce_raw_cap_truncates_with_marker() {
        let budget = budget_req(80, &[]);
        let long = "z".repeat(1000);
        let out = enforce_raw_cap(long, &budget).expect("should truncate, not error");
        assert!(out.len() <= 80, "raw payload capped: {} bytes", out.len());
        assert!(out.contains("truncated"), "truncation is disclosed");
    }

    #[test]
    fn f6_enforce_raw_cap_too_small_errors() {
        let budget = budget_req(4, &[]);
        let err = enforce_raw_cap("hello world".repeat(10), &budget).unwrap_err();
        assert_eq!(err.code(), McpErrorCode::InvalidArgument);
    }

    /// Issue #2906: `semantic_search` reuses the `find_similar` k-NN path, so it
    /// must be classified as a *ranked* tool — its ranked `results` are then
    /// never dropped or reordered (never reach the counts-and-handles rung) to
    /// meet a token budget, preserving find_similar parity.
    #[test]
    fn semantic_search_is_a_ranked_tool() {
        assert!(
            is_ranked_tool("semantic_search"),
            "semantic_search must be ranked-protected (find_similar parity)"
        );
        // Regression guard for the pre-existing ranked tools.
        assert!(is_ranked_tool("find_similar"));
        assert!(is_ranked_tool("hybrid_query"));
        // A non-ranked read tool stays non-ranked.
        assert!(!is_ranked_tool("list_nodes"));
    }

    /// T5: the byte cap holds for multibyte/unicode payloads, and truncation
    /// never splits a UTF-8 char boundary (the result is valid UTF-8).
    #[test]
    fn t5_enforce_raw_cap_respects_utf8_boundaries() {
        let budget = budget_req(90, &[]);
        // Each 'é' / emoji is multibyte; a naive byte cut could split them.
        let s = "héllo wörld 🌍🌎🌏 ".repeat(50);
        let out = enforce_raw_cap(s, &budget).expect("truncates");
        assert!(out.len() <= 90, "byte cap holds: {} bytes", out.len());
        // Valid UTF-8 by construction (String), and re-checkable:
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    // ---- Issue #3583: priority_properties DoS bound ------------------------

    /// The protected-key `HashSet` lookup path protects *exactly* the same keys
    /// as the previous linear scan: a representative node with a mix of
    /// protected and unprotected bulky properties keeps the protected ones in
    /// full and elides the unprotected ones. Behavior-preserving evidence for
    /// the O(1) rewrite.
    #[test]
    fn priority_hashset_protects_same_keys_as_linear_scan() {
        let node = json!({
            "id": 1,
            "label": "Doc",
            "properties": {
                "bio": "x".repeat(4000),      // protected → survives
                "notes": "y".repeat(4000),    // unprotected → elided
                "summary": "z".repeat(4000),  // protected → survives
                "extra": "w".repeat(4000),    // unprotected → elided
            }
        });
        let budget = budget_req(100_000, &["bio", "summary"]);
        let args = json!({});
        let elided = build_candidate(
            node,
            Rung::ElideProperties,
            &budget,
            "get_node",
            &args,
            100_000,
        );
        let props = &elided["properties"];
        // Protected keys retain their full value.
        assert_eq!(props["bio"].as_str().map(str::len), Some(4000));
        assert_eq!(props["summary"].as_str().map(str::len), Some(4000));
        // Unprotected bulky keys are elided descriptors, not the raw string.
        assert_eq!(props["notes"]["elided"], json!(true));
        assert_eq!(props["extra"]["elided"], json!(true));
    }

    /// The summarize rung keeps exactly the protected keys via the `HashSet`
    /// path (plus the disclosure marker), dropping everything else — matching
    /// the pre-rewrite linear-scan semantics.
    #[test]
    fn priority_hashset_summarize_keeps_only_protected() {
        let node = json!({
            "id": 7,
            "label": "Doc",
            "properties": {
                "bio": "x".repeat(4000),
                "notes": "y".repeat(4000),
                "keepme": "k".repeat(10),
            }
        });
        let budget = budget_req(500, &["keepme"]);
        let args = json!({});
        let shaped = build_candidate(node, Rung::Summaries, &budget, "get_node", &args, 500);
        let props = shaped["properties"].as_object().expect("properties object");
        assert!(props.contains_key("keepme"), "protected key survives");
        assert!(!props.contains_key("bio"), "unprotected key dropped");
        assert!(!props.contains_key("notes"), "unprotected key dropped");
        assert!(
            props.contains_key("_budget_omitted"),
            "summary discloses the omission"
        );
    }

    /// A `priority_properties` array at or below the cap parses successfully.
    #[test]
    fn parse_budget_accepts_priority_at_cap() {
        let items: Vec<Value> = (0..DEFAULT_MAX_PRIORITY_PROPERTIES)
            .map(|i| json!(format!("p{i}")))
            .collect();
        let args = json!({
            "max_response_tokens": 1000,
            "priority_properties": items,
        });
        let parsed = parse_budget(&args, DEFAULT_MAX_PRIORITY_PROPERTIES)
            .expect("at-cap array is accepted")
            .expect("budget present");
        assert_eq!(
            parsed.priority_properties.len(),
            DEFAULT_MAX_PRIORITY_PROPERTIES
        );
    }

    /// A `priority_properties` array over the cap is rejected with a structured
    /// `INVALID_ARGUMENT` error naming the cap and the given length, before any
    /// shaping work — the #3583 defense-in-depth bound.
    #[test]
    fn parse_budget_rejects_priority_over_cap() {
        let cap = 8usize;
        let items: Vec<Value> = (0..cap + 1).map(|i| json!(format!("p{i}"))).collect();
        let given = items.len();
        let args = json!({
            "max_response_tokens": 1000,
            "priority_properties": items,
        });
        let err = parse_budget(&args, cap).expect_err("over-cap array must be rejected");
        assert_eq!(err.code(), McpErrorCode::InvalidArgument);
        assert!(!err.is_retriable(), "caller-fault error is non-retriable");
        let json = err.to_json();
        assert_eq!(json["details"]["max_priority_properties"], json!(cap));
        assert_eq!(json["details"]["given"], json!(given));
        // The message names both the cap and the given length.
        assert!(err.message().contains(&cap.to_string()));
        assert!(err.message().contains(&given.to_string()));
        // Self-consistent: re-issuing under the cap succeeds.
        let ok_items: Vec<Value> = (0..cap).map(|i| json!(format!("p{i}"))).collect();
        let ok_args = json!({
            "max_response_tokens": 1000,
            "priority_properties": ok_items,
        });
        assert!(
            parse_budget(&ok_args, cap).is_ok(),
            "re-issuing under the cap must succeed"
        );
    }

    /// The cap is inert when `priority_properties` is absent or empty (the
    /// common case): behavior is completely unchanged.
    #[test]
    fn parse_budget_cap_inert_without_priority() {
        let args = json!({ "max_response_tokens": 1000 });
        let parsed = parse_budget(&args, 1)
            .expect("no priority array")
            .expect("budget present");
        assert!(parsed.priority_properties.is_empty());
    }
}
