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
  /** Property keys to protect first as the response degrades. */
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
