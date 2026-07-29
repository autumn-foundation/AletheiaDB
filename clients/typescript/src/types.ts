/**
 * Typed request/response models mirroring the AletheiaDB HTTP entity JSON.
 *
 * The shapes here track the autumn-server routes (`crates/aletheia-server`) and
 * the parity inventory (`tests/parity/inventory.json`): the bi-temporal
 * `temporal` block (#3232), write-time `provenance`, the discriminated
 * `PropertyValue` union, and vector elision (#3220).
 *
 * No `any` appears in this surface — arbitrary JSON is modeled with
 * {@link JsonValue}, and vectors are a discriminated union of a full
 * `number[]` array versus an elided descriptor.
 *
 * @packageDocumentation
 */

import type { TimeInput } from './time.js';

// ─────────────────────────────────────────────────────────────────────────────
// JSON + property values
// ─────────────────────────────────────────────────────────────────────────────

/** Any JSON value, without ever resorting to `any`. */
export type JsonValue =
  | string
  | number
  | boolean
  | null
  | JsonValue[]
  | { [key: string]: JsonValue };

/**
 * The elided descriptor a dense vector property degrades to when
 * `includeVectors` is not set (Issue #3220). Distinct from `number[]` so a
 * caller cannot misread a descriptor as data.
 */
export interface ElidedVector {
  type: 'vector';
  dim: number;
  elided: true;
}

/** The elided descriptor for a sparse vector property (Issue #3220). */
export interface ElidedSparseVector {
  type: 'sparse_vector';
  dim: number;
  nnz: number;
  elided: true;
}

/** Either the elided descriptor or the full dense array. */
export type VectorProperty = number[] | ElidedVector | ElidedSparseVector;

/**
 * A property value as it appears on the wire. Discriminates a full vector
 * (`number[]`) from an elided descriptor via {@link isElidedVector} /
 * {@link isElidedSparseVector}.
 */
export type PropertyValue = JsonValue | ElidedVector | ElidedSparseVector;

/** A bag of entity properties keyed by name. */
export type PropertyMap = Record<string, PropertyValue>;

/** Type guard: `v` is an elided dense-vector descriptor, not a real array. */
export function isElidedVector(v: PropertyValue): v is ElidedVector {
  return (
    typeof v === 'object' &&
    v !== null &&
    !Array.isArray(v) &&
    (v as { type?: unknown }).type === 'vector' &&
    (v as { elided?: unknown }).elided === true
  );
}

/** Type guard: `v` is an elided sparse-vector descriptor. */
export function isElidedSparseVector(v: PropertyValue): v is ElidedSparseVector {
  return (
    typeof v === 'object' &&
    v !== null &&
    !Array.isArray(v) &&
    (v as { type?: unknown }).type === 'sparse_vector' &&
    (v as { elided?: unknown }).elided === true
  );
}

/** Type guard: `v` is a full dense vector array (all numbers). */
export function isFullVector(v: PropertyValue): v is number[] {
  return Array.isArray(v) && v.every((x) => typeof x === 'number');
}

// ─────────────────────────────────────────────────────────────────────────────
// Temporal + provenance
// ─────────────────────────────────────────────────────────────────────────────

/**
 * The bi-temporal bounds stamped on every read response (Issue #3232). Lower
 * bounds are RFC 3339 strings; open upper bounds are explicit `null` (never
 * omitted). `is_current` is `true` iff the transaction interval is open and now
 * falls within the valid interval.
 */
export interface TemporalBounds {
  valid_from: string | null;
  valid_to: string | null;
  transaction_from: string | null;
  transaction_to: string | null;
  is_current: boolean;
}

/** Write-time provenance bundle recorded against a version (Issue #3224/#3350). */
export interface Provenance {
  source?: string;
  confidence?: number;
  note?: string;
  correlation_id?: string;
  /** Server-stamped principal name when the write was authenticated (#3350). */
  principal?: string;
}

// ─────────────────────────────────────────────────────────────────────────────
// Entities
// ─────────────────────────────────────────────────────────────────────────────

/** A node as returned by the read/write surface. */
export interface NodeEntity {
  id: number;
  label: string;
  properties: PropertyMap;
  temporal?: TemporalBounds;
  provenance?: Provenance;
  /** The version id of the returned version, when the response carries it. */
  version_id?: number;
}

/** An edge as returned by the read/write surface. */
export interface EdgeEntity {
  id: number;
  label: string;
  source_id: number;
  target_id: number;
  properties: PropertyMap;
  temporal?: TemporalBounds;
  provenance?: Provenance;
  version_id?: number;
}

// ─────────────────────────────────────────────────────────────────────────────
// Completeness / pagination signals (Issue #3226 / #3360 / #3353)
// ─────────────────────────────────────────────────────────────────────────────

/** The per-section token-budget rung applied to a response (Issue #3353). */
export interface BudgetInfo {
  [section: string]: unknown;
}

/**
 * The completeness/pagination signals a bounded read can carry. All optional —
 * a given response surfaces only what applies.
 */
export interface Completeness {
  count?: number;
  has_more?: boolean;
  next_offset?: number;
  total_matching?: number;
  truncated?: boolean;
  sampled?: boolean;
  /** Opaque snapshot-anchored cursor token (#3360), present when `use_cursor`. */
  cursor?: string;
  /** The snapshot valid time the cursor scan was pinned at (#3360). */
  snapshot_valid_time?: string;
  /** The snapshot transaction time the cursor scan was pinned at (#3360). */
  snapshot_transaction_time?: string;
  paging?: string;
  budget?: BudgetInfo;
}

/** A list-nodes response: the page plus completeness signals. */
export interface NodeListResponse extends Completeness {
  nodes: NodeEntity[];
}

/** A list-edges / adjacency response: the page plus completeness signals. */
export interface EdgeListResponse extends Completeness {
  edges: EdgeEntity[];
}

/** One traversal result row: the reached node plus optional path metadata. */
export interface TraverseResult {
  node: NodeEntity;
  depth?: number;
  [key: string]: JsonValue | NodeEntity | undefined;
}

/** A traverse response: the reached rows plus completeness signals. */
export interface TraverseResponse extends Completeness {
  results: TraverseResult[];
}

/** A `{ count, label? }` count response. */
export interface CountResponse {
  count: number;
  label?: string;
}

/** A node/edge history response: every version oldest-first. */
export interface NodeHistoryResponse extends Completeness {
  node_id: number;
  results?: NodeEntity[];
  versions?: NodeEntity[];
}

/** An edge history response: every version oldest-first. */
export interface EdgeHistoryResponse extends Completeness {
  edge_id: number;
  results?: EdgeEntity[];
  versions?: EdgeEntity[];
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared request option fragments
// ─────────────────────────────────────────────────────────────────────────────

/** Whether to include full vector arrays instead of the elided descriptor (#3220). */
export interface IncludeVectorsOption {
  includeVectors?: boolean;
}

/** The Issue #3353 token-budget options, accepted by budgetable reads. */
export interface BudgetOptions {
  /** Cap the response at roughly this many tokens (estimated `ceil(bytes/4)`). */
  maxResponseTokens?: number;
  /** Byte-exact response cap. */
  maxResponseBytes?: number;
  /**
   * Property keys to protect first as the response degrades.
   *
   * Honored on both surfaces. The POST-body reads
   * ({@link AletheiaClient.findNodesAtTime}, {@link AletheiaClient.findSimilar},
   * {@link AletheiaClient.hybridQuery}, {@link AletheiaClient.query}) carry it
   * as a JSON array. The nine budgetable GET reads
   * (`getNode`/`listNodes`/`getEdge`/`listEdges`/`traverse`/`getNodeHistory`/
   * `getSchema` and the two adjacency reads) serialize it **comma-joined** onto
   * the query string, which the server (PR #3638) splits on `,` server-side.
   *
   * Normalized identically on both surfaces before sending: entries are
   * trimmed, empty/whitespace-only entries are dropped, and a list with nothing
   * left is omitted entirely — never a bare `?priority_properties=`. A property
   * name containing a **comma** throws `InvalidArgumentError`: it is
   * unrepresentable on the GET wire (the server would split it into two names),
   * and sending it would silently protect the wrong properties.
   *
   * Only meaningful alongside a budget: with neither `maxResponseTokens` nor
   * `maxResponseBytes` set there is no budget to degrade against, and the
   * server discards `priority_properties` entirely.
   */
  priorityProperties?: string[];
}

/** Offset-based pagination (Issue #3226). */
export interface OffsetPageOptions {
  limit?: number;
  offset?: number;
}

/** Snapshot-anchored cursor continuation (Issue #3360). */
export interface CursorOptions {
  /** Request a cursor on the first page. */
  useCursor?: boolean;
  /** Continuation token from a prior page; pass it back alone. */
  cursor?: string;
}

// ─────────────────────────────────────────────────────────────────────────────
// Write request bodies (mirror the MCP request structs)
// ─────────────────────────────────────────────────────────────────────────────

/** A version-pinned derivation reference (Issue #3371). */
export interface LineageRef {
  entity_kind: 'node' | 'edge';
  id: number;
  version: number;
}

/** Create a node. `validTime` accepts a {@link TimeInput} (#3221). */
export interface CreateNodeRequest {
  label: string;
  properties?: PropertyMap;
  validTime?: TimeInput;
  provenance?: Provenance;
  derivedFrom?: LineageRef[];
}

/** Update a node's properties (replaces all), recording a new version. */
export interface UpdateNodeRequest {
  nodeId: number;
  properties: PropertyMap;
  validTime?: TimeInput;
  provenance?: Provenance;
  derivedFrom?: LineageRef[];
}

/** Safe-by-default delete (Issue #3209). `detach` cascades; `validTime` back-dates. */
export interface DeleteNodeRequest {
  nodeId: number;
  detach?: boolean;
  validTime?: TimeInput;
}

/** Cascade delete: node + every connected edge, atomically. */
export interface DeleteNodeCascadeRequest {
  nodeId: number;
}

/** Retract a node — close its valid-time interval without deleting history (#3230). */
export interface RetractNodeRequest {
  nodeId: number;
  validTime?: TimeInput;
  detach?: boolean;
}

/** Create an edge between two nodes. `validTime` accepts a {@link TimeInput}. */
export interface CreateEdgeRequest {
  sourceId: number;
  targetId: number;
  label: string;
  properties?: PropertyMap;
  validTime?: TimeInput;
  provenance?: Provenance;
  derivedFrom?: LineageRef[];
}

/** Update an edge's properties (replaces all), recording a new version. */
export interface UpdateEdgeRequest {
  edgeId: number;
  properties: PropertyMap;
  validTime?: TimeInput;
  provenance?: Provenance;
  derivedFrom?: LineageRef[];
}

/** Delete an edge, optionally back-dated (#3221). */
export interface DeleteEdgeRequest {
  edgeId: number;
  validTime?: TimeInput;
}

/** Retract an edge — close its valid-time interval without deleting history (#3230). */
export interface RetractEdgeRequest {
  edgeId: number;
  validTime?: TimeInput;
}

// ─────────────────────────────────────────────────────────────────────────────
// Read request options
// ─────────────────────────────────────────────────────────────────────────────

/** Options for {@link AletheiaClient.getNode}. */
export type GetNodeOptions = IncludeVectorsOption & BudgetOptions;

/** Options for {@link AletheiaClient.getEdge}. */
export type GetEdgeOptions = IncludeVectorsOption & BudgetOptions;

/** Options for {@link AletheiaClient.listNodes}. */
export interface ListNodesOptions
  extends IncludeVectorsOption,
    BudgetOptions,
    OffsetPageOptions,
    CursorOptions {
  label?: string;
  propertyKey?: string;
  propertyValue?: string;
}

/** Options for {@link AletheiaClient.listEdges}. */
export interface ListEdgesOptions
  extends IncludeVectorsOption,
    BudgetOptions,
    OffsetPageOptions {
  label?: string;
}

/** Options for the adjacency reads (outgoing/incoming edges). */
export interface AdjacencyOptions
  extends IncludeVectorsOption,
    BudgetOptions,
    CursorOptions {
  label?: string;
  limit?: number;
}

/** Options for a graph traversal (Issue #3225 `as_of_*` handled via {@link TemporalView}). */
export interface TraverseOptions
  extends IncludeVectorsOption,
    BudgetOptions,
    OffsetPageOptions,
    CursorOptions {
  startNodeId?: number;
  edgeLabel?: string;
  direction?: 'outgoing' | 'incoming' | 'both';
  depth?: number;
  /** Valid-time coordinate (#3225). Prefer {@link AletheiaClient.asOf}. */
  asOfValidTime?: TimeInput;
  /** Transaction-time coordinate (#3225). Prefer {@link AletheiaClient.asOf}. */
  asOfTransactionTime?: TimeInput;
}

/** Point-in-time node find (Issue #3236). */
export interface FindNodesAtTimeRequest
  extends IncludeVectorsOption,
    BudgetOptions,
    OffsetPageOptions,
    CursorOptions {
  label?: string;
  propertyKey?: string;
  propertyValue?: JsonValue;
  validTime?: TimeInput;
  transactionTime?: TimeInput;
}

/** Bi-temporal point-in-time node read (`get_node_at_time`). */
export interface GetNodeAtTimeRequest {
  nodeId: number;
  validTime: TimeInput;
  transactionTime?: TimeInput;
}

/** Bi-temporal point-in-time edge read (`get_edge_at_time`). */
export interface GetEdgeAtTimeRequest {
  edgeId: number;
  validTime: TimeInput;
  transactionTime?: TimeInput;
}

/** Single-dimension valid-time node read. */
export interface NodeAtValidTimeRequest {
  nodeId: number;
  validTime: TimeInput;
}

/** Single-dimension transaction-time node read. */
export interface NodeAtTransactionTimeRequest {
  nodeId: number;
  transactionTime: TimeInput;
}

/** Single-dimension valid-time edge read. */
export interface EdgeAtValidTimeRequest {
  edgeId: number;
  validTime: TimeInput;
}

/** Single-dimension transaction-time edge read. */
export interface EdgeAtTransactionTimeRequest {
  edgeId: number;
  transactionTime: TimeInput;
}

/** Property-level diff between two node versions. */
export interface DiffNodeVersionsRequest {
  nodeId: number;
  fromVersion: number;
  toVersion: number;
}

/** Property-level diff between two edge versions. */
export interface DiffEdgeVersionsRequest {
  edgeId: number;
  fromVersion: number;
  toVersion: number;
}

/** List graph-wide changes in a transaction-time window (`list_changes`). */
export interface ListChangesRequest {
  txFrom: TimeInput;
  txTo: TimeInput;
  validFrom?: TimeInput;
  validTo?: TimeInput;
  label?: string;
  limit?: number;
  cursor?: string;
}

/** History read options: the #3353 token budget on a single-entity history read. */
export type NodeHistoryOptions = BudgetOptions;

// ─────────────────────────────────────────────────────────────────────────────
// Count options
// ─────────────────────────────────────────────────────────────────────────────

/** Options for {@link AletheiaClient.countNodes} / {@link AletheiaClient.countEdges}. */
export interface CountOptions {
  label?: string;
}

// ─────────────────────────────────────────────────────────────────────────────
// Admin / health
// ─────────────────────────────────────────────────────────────────────────────

/** The four RBAC roles. */
export type Role = 'admin' | 'writer' | 'reader' | 'metrics';

/** `GET /status` response. */
export interface StatusResponse {
  status: string;
}

/** `POST /admin/keys` request. */
export interface CreateKeyRequest {
  name: string;
  role: Role;
}

/** `POST /admin/keys` response — the plaintext key is returned exactly once. */
export interface CreateKeyResponse {
  id: string;
  name: string;
  role: Role;
  key_prefix: string;
  created_at: string;
  /** Plaintext key material, returned only on creation. */
  key: string;
}

/** A masked key principal from `GET /admin/keys` (never full material). */
export interface KeyPrincipal {
  id: string;
  name: string;
  role: Role;
  key_prefix: string;
  created_at: string;
}

/** `GET /admin/keys` response. */
export interface ListKeysResponse {
  keys: KeyPrincipal[];
}

/** `POST /admin/keys/revoke` request. */
export interface RevokeKeyRequest {
  id: string;
}

/** `POST /admin/keys/revoke` response. */
export interface RevokeKeyResponse {
  revoked: boolean;
  id: string;
}

// ─────────────────────────────────────────────────────────────────────────────
// Vector search (find_similar / enable_vector_index / list_vector_indexes)
// ─────────────────────────────────────────────────────────────────────────────

/** A supported vector distance metric for {@link AletheiaClient.enableVectorIndex}. */
export type DistanceMetric = 'cosine' | 'euclidean' | 'dot';

/** `POST /find_similar` request — k-NN over a property's embeddings. */
export interface FindSimilarRequest extends IncludeVectorsOption, BudgetOptions {
  /** The property name that contains the vector embedding (e.g. `"embedding"`). */
  propertyName: string;
  /** The query embedding vector. */
  embedding: number[];
  /** Number of similar results to return (default 10). */
  k?: number;
  /** Number of results to skip (offset pagination, #3226). */
  offset?: number;
}

/** One ranked node from {@link AletheiaClient.findSimilar} with its similarity score. */
export interface SimilarityResult {
  node: NodeEntity;
  /** The similarity score; always returned in full, never elided/budget-dropped. */
  score: number;
}

/** `POST /find_similar` response. */
export interface FindSimilarResponse extends Completeness {
  results: SimilarityResult[];
}

/** `POST /vector/indexes` request — enable HNSW indexing on a node property. */
export interface EnableVectorIndexRequest {
  /** The property name to index (e.g. `"embedding"`). */
  propertyName: string;
  /** The dimension of the vectors. */
  dimensions: number;
  /** Distance metric (default `"cosine"`). */
  distanceMetric?: DistanceMetric;
}

/** `POST /vector/indexes` response. */
export interface EnableVectorIndexResponse {
  success: boolean;
  property_name: string;
  dimensions: number;
  distance_metric: string;
}

/** One active vector index's configuration. */
export interface VectorIndexInfo {
  property_name: string;
  dimensions: number;
  distance_metric: string;
}

/** `GET /vector/indexes` response. */
export interface ListVectorIndexesResponse {
  indexes: VectorIndexInfo[];
  count: number;
}

// ─────────────────────────────────────────────────────────────────────────────
// Hybrid query (graph + vector + temporal)
// ─────────────────────────────────────────────────────────────────────────────

/** `POST /hybrid_query` request — combined graph traversal + vector + temporal. */
export interface HybridQueryRequest extends IncludeVectorsOption, BudgetOptions {
  /** Starting node id (for graph-first queries). */
  startNodeId?: number;
  /** Edge label for traversal. */
  traverseEdge?: string;
  /** Traversal depth (default 1). */
  traverseDepth?: number;
  /** Property name for vector similarity. */
  vectorProperty?: string;
  /** Query embedding for vector similarity. */
  queryEmbedding?: number[];
  /** Number of similar results for the vector stage. */
  topK?: number;
  /** Valid-time coordinate for temporal filtering. */
  validTime?: TimeInput;
  /** Transaction-time coordinate for temporal filtering. */
  transactionTime?: TimeInput;
  /** Filter results by node label. */
  filterLabel?: string;
  /** Maximum number of results (default 100). */
  limit?: number;
}

/** One hybrid-query result row. Carries a `similarity_score` when the vector stage ran. */
export interface HybridResult {
  entity?: NodeEntity;
  similarity_score?: number;
  [key: string]: JsonValue | NodeEntity | undefined;
}

/** `POST /hybrid_query` response. */
export interface HybridQueryResponse extends Completeness {
  results: HybridResult[];
  as_of_valid_time?: string;
  as_of_transaction_time?: string;
}

// ─────────────────────────────────────────────────────────────────────────────
// Read-only declarative query (Cypher / AQL)
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Per-call resource-limit overrides for {@link AletheiaClient.query} (Issue
 * #3368). Each is bounded by an operator ceiling (an over-ceiling value is
 * rejected with `INVALID_ARGUMENT`); `0` means unlimited (only under an
 * unbounded ceiling). Field names are the server wire shape (snake_case).
 */
export interface QueryLimitsOverride {
  /** Per-call wall-clock timeout in milliseconds (`0` = unlimited). */
  timeout_ms?: number;
  /** Per-call maximum result rows (`0` = unlimited); over-cap results truncate, not reject. */
  max_result_rows?: number;
  /** Per-call maximum serialized response bytes (`0` = unlimited); exceeding fails closed. */
  max_response_bytes?: number;
}

/** `POST /query` request — a single read-only Cypher/AQL statement. */
export interface QueryRequest extends BudgetOptions {
  /** Query language. */
  language: 'cypher' | 'aql';
  /** The read-only statement to execute. */
  query: string;
  /** Optional `$param` bindings (Cypher only). Numeric arrays are treated as embeddings. */
  params?: Record<string, JsonValue>;
  /** Maximum rows to return (default 100, capped at 10000). */
  limit?: number;
  /** Optional per-call resource-limit overrides (#3368). */
  limits?: QueryLimitsOverride;
}

/** `POST /query` response — structured rows plus column metadata. */
export interface QueryResponse {
  language: string;
  columns: string[];
  rows: JsonValue[];
  row_count: number;
  truncated?: boolean;
  [key: string]: JsonValue | undefined;
}

// ─────────────────────────────────────────────────────────────────────────────
// Schema / stats / temporal extent
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Options for {@link AletheiaClient.getSchema}. `asOf*` switch to a bi-temporal
 * snapshot.
 *
 * `getSchema` is one of the nine budgetable GET reads (#3353), so it composes
 * the shared {@link BudgetOptions}: it accepts the full token/byte budget
 * **and** `priorityProperties`, which `GET /schema` reads comma-separated
 * through the same `de_priority_properties` splitter as every other GET read
 * (`GetSchemaQuery`, `crates/aletheia-server/src/schema_batch_tools.rs`).
 */
export interface GetSchemaOptions extends BudgetOptions {
  asOfValidTime?: TimeInput;
  asOfTransactionTime?: TimeInput;
}

/**
 * `GET /schema` response — node labels, edge types, and property keys with
 * counts. Modeled as an open JSON object (the exact shape is a bare tool result).
 */
export interface SchemaResponse {
  node_labels?: JsonValue;
  edge_types?: JsonValue;
  [key: string]: JsonValue | undefined;
}

/** Current-state (hot tier) graph size. */
export interface CurrentStateStats {
  node_count: number;
  edge_count: number;
}

/** Bi-temporal depth of the in-RAM historical store, with anchor/delta compression. */
export interface HistoricalDepthStats {
  total_node_versions: number;
  total_edge_versions: number;
  unique_nodes: number;
  unique_edges: number;
  anchor_count: number;
  delta_count: number;
  node_anchor_count: number;
  node_delta_count: number;
  edge_anchor_count: number;
  edge_delta_count: number;
  /** Anchors as a fraction of total versions (0.0–1.0; lower is better compression). */
  compression_ratio: number;
}

/** Where historical-version reads were served from (hot/warm/cold distribution). */
export interface TierAccessStats {
  hot_hits: number;
  warm_hits: number;
  cold_hits: number;
  misses: number;
}

/**
 * Cold-tier (disk) storage state. When disabled the response is exactly
 * `{ enabled: false }` — the count fields are structurally absent (never a
 * misleading zero). When enabled, `ColdStorageDetails` is flattened alongside.
 */
export interface ColdStorageTierStats {
  enabled: boolean;
  /** Present only when `enabled` is `true` (flattened server-side). */
  node_versions_stored?: number;
  /** Present only when `enabled` is `true`. */
  edge_versions_stored?: number;
  /** Present only when `enabled` is `true`. */
  compression_ratio?: number;
  /** Present only when `enabled` is `true`. */
  tier_access?: TierAccessStats;
}

/** Write-ahead-log durability state. */
export interface WalStateStats {
  enabled: boolean;
  /** Stable token: `"synchronous"`, `"async"`, `"group_commit"`, or `"async_batched"`. */
  durability_mode: string;
  /** The next LSN to be allocated (one past the most recent). */
  current_lsn: number;
  total_appends: number;
  healthy: boolean;
}

/** Outcome of the most recent provenance-chain verification (Issue #3351). */
export interface LastVerifiedStats {
  passed: boolean;
  at_micros: number;
}

/** Tamper-evident provenance hash chain status (Issue #3351). */
export interface ProvenanceChainStats {
  enabled: boolean;
  head_seq: number | null;
  head_digest: string | null;
  genesis_digest: string | null;
  last_verified: LastVerifiedStats | null;
}

/**
 * `GET /database_stats` response — a holistic bi-temporal snapshot (Issue
 * #3222), mirroring the Rust `DatabaseStats` struct. Every field is an O(1)
 * cached counter read.
 */
export interface DatabaseStats {
  current: CurrentStateStats;
  historical: HistoricalDepthStats;
  cold_storage: ColdStorageTierStats;
  wal: WalStateStats;
  chain: ProvenanceChainStats;
}

/** Options for {@link AletheiaClient.temporalExtent}. */
export interface TemporalExtentOptions {
  /** Additionally return per-node-label / per-edge-type bounds. */
  byLabel?: boolean;
}

/**
 * `GET /temporal_extent` response — the queryable bi-temporal extent (open JSON
 * object; `valid_time`/`transaction_time` carry `{earliest, latest}` bounds).
 */
export interface TemporalExtentResponse {
  valid_time?: JsonValue;
  transaction_time?: JsonValue;
  node_labels?: JsonValue;
  edge_types?: JsonValue;
  [key: string]: JsonValue | undefined;
}

// ─────────────────────────────────────────────────────────────────────────────
// Atomic multi-write batch (apply_batch, #3231)
// ─────────────────────────────────────────────────────────────────────────────

/**
 * A batch edge endpoint: a committed node id (`number`) or a local reference
 * string to a node created earlier in the same batch — `"$alias"` (a
 * `create_node`'s `ref`) or positional `"$<index>"`.
 */
export type BatchNodeRef = number | string;

/**
 * `apply_batch` field names are the **server wire shape (snake_case)**, NOT this
 * SDK's camelCase single-op requests — the operations array is forwarded to the
 * server verbatim (`#[serde(tag = "op", rename_all = "snake_case")]`). In
 * particular:
 * - endpoints are `source_id`/`target_id`/`node_id`/`edge_id` (never `sourceId`…);
 * - `valid_time` is an RFC 3339 **string** or decimal-microseconds **string**
 *   (a JSON number fails server deserialization) — pass `toWireTime(t)`, not a
 *   `TimeInput`/`Date`/`number`.
 *
 * The type is a strict discriminated union with **no** index signature, so a
 * camelCase key or a numeric `valid_time` is a **compile error**, not a silently
 * dropped/mis-timed write.
 */
export type BatchOperation =
  | BatchCreateNode
  | BatchCreateEdge
  | BatchUpdateNode
  | BatchUpdateEdge
  | BatchDeleteNode
  | BatchDeleteEdge;

/** Create a node; `ref` makes it addressable later as `"$<ref>"`. */
export interface BatchCreateNode {
  op: 'create_node';
  label: string;
  properties?: PropertyMap;
  /** RFC 3339 or decimal-µs string (use `toWireTime`). */
  valid_time?: string;
  provenance?: Provenance;
  /** Optional alias for referencing this node later in the batch. */
  ref?: string;
}

/** Create an edge; endpoints accept committed ids and local `$refs` interchangeably. */
export interface BatchCreateEdge {
  op: 'create_edge';
  source_id: BatchNodeRef;
  target_id: BatchNodeRef;
  label: string;
  properties?: PropertyMap;
  /** RFC 3339 or decimal-µs string (use `toWireTime`). */
  valid_time?: string;
  provenance?: Provenance;
}

/** Update a committed node's properties (v1 rejects updating a batch-created ref). */
export interface BatchUpdateNode {
  op: 'update_node';
  node_id: BatchNodeRef;
  properties: PropertyMap;
  /** RFC 3339 or decimal-µs string (use `toWireTime`). */
  valid_time?: string;
  provenance?: Provenance;
}

/** Update a committed edge's properties. */
export interface BatchUpdateEdge {
  op: 'update_edge';
  edge_id: number;
  properties: PropertyMap;
  /** RFC 3339 or decimal-µs string (use `toWireTime`). */
  valid_time?: string;
  provenance?: Provenance;
}

/** Delete a committed node (safe-by-default DETACH contract; `detach` cascades). */
export interface BatchDeleteNode {
  op: 'delete_node';
  node_id: BatchNodeRef;
  /** Cascade-remove connected edges. Not supported together with `valid_time`. */
  detach?: boolean;
  /** RFC 3339 or decimal-µs string (use `toWireTime`). */
  valid_time?: string;
}

/** Delete a committed edge. */
export interface BatchDeleteEdge {
  op: 'delete_edge';
  edge_id: number;
  /** RFC 3339 or decimal-µs string (use `toWireTime`). */
  valid_time?: string;
}

/** `POST /batch` request — an ordered, all-or-nothing batch of write operations. */
export interface ApplyBatchRequest {
  operations: BatchOperation[];
}

/**
 * `POST /batch` response — per-op results in input order plus a `ref_map`
 * (alias → committed id). Modeled as an open JSON object.
 */
export interface ApplyBatchResponse {
  results?: JsonValue[];
  ref_map?: Record<string, number>;
  [key: string]: JsonValue | undefined;
}

// ─────────────────────────────────────────────────────────────────────────────
// Derivation lineage (upstream / downstream, #3371)
// ─────────────────────────────────────────────────────────────────────────────

/** `POST /lineage/{upstream,downstream}` request — a version-pinned closure query. */
export interface LineageQueryRequest {
  /** Whether the root fact is a node or an edge. */
  entityKind: 'node' | 'edge';
  /** The root entity's id. */
  id: number;
  /** The root fact's version id (lineage is version-pinned). */
  version: number;
  /** Maximum transitive hop depth from the root. */
  maxDepth?: number;
  /** Maximum closure entries to return (default 100). */
  limit?: number;
  /** Number of entries to skip (breadth-first order). */
  offset?: number;
  /** Only follow lineage recorded at or before this transaction time. */
  asOfTransactionTime?: TimeInput;
}

/** One resolved lineage entry: a version-pinned ref plus its depth and current status. */
export interface LineageEntry {
  entity_kind: 'node' | 'edge';
  id: number;
  version: number;
  depth: number;
  /** Current-state status of the referenced fact (`Current`/`Superseded`/`Absent`). */
  status: string;
}

/** `POST /lineage/{upstream,downstream}` response. */
export interface LineageResponse extends Completeness {
  direction: 'upstream' | 'downstream';
  root: LineageRef;
  entries: LineageEntry[];
}
