/**
 * The AletheiaDB TypeScript client.
 *
 * Wraps the 34 merged autumn-server REST routes (node/edge/traverse/temporal +
 * admin/health) as typed methods. Endpoints that are not yet merged server-side
 * (find_similar, hybrid_query, query, enable_vector_index, list_vector_indexes,
 * get_schema, database_stats, temporal_extent, apply_batch, lineage_upstream,
 * lineage_downstream) are provided as typed stubs that throw
 * {@link NotImplementedError}.
 *
 * @packageDocumentation
 */

import { NotImplementedError } from './errors.js';
import type { ClientOptions, RequestSpec } from './http.js';
import { Transport, unwrapData } from './http.js';
import type { TimeInput } from './time.js';
import { toWireTime } from './time.js';
import { TemporalView } from './temporal-view.js';
import type {
  CountOptions,
  CountResponse,
  CreateEdgeRequest,
  CreateKeyRequest,
  CreateKeyResponse,
  CreateNodeRequest,
  DeleteEdgeRequest,
  DeleteNodeCascadeRequest,
  DeleteNodeRequest,
  DiffEdgeVersionsRequest,
  DiffNodeVersionsRequest,
  EdgeAtTransactionTimeRequest,
  EdgeAtValidTimeRequest,
  EdgeEntity,
  EdgeHistoryResponse,
  EdgeListResponse,
  FindNodesAtTimeRequest,
  GetEdgeAtTimeRequest,
  GetEdgeOptions,
  GetNodeAtTimeRequest,
  GetNodeOptions,
  JsonValue,
  ListChangesRequest,
  ListEdgesOptions,
  ListKeysResponse,
  ListNodesOptions,
  AdjacencyOptions,
  NodeAtTransactionTimeRequest,
  NodeAtValidTimeRequest,
  NodeEntity,
  NodeHistoryOptions,
  NodeHistoryResponse,
  NodeListResponse,
  RetractEdgeRequest,
  RetractNodeRequest,
  RevokeKeyRequest,
  RevokeKeyResponse,
  StatusResponse,
  TraverseOptions,
  TraverseResponse,
  UpdateEdgeRequest,
  UpdateNodeRequest,
  LineageRef,
  Provenance,
} from './types.js';

/** Coordinates for {@link AletheiaClient.asOf}. At least one dimension is set. */
export interface AsOfCoordinates {
  /** Valid time — when the fact was true in reality (#3225). */
  validTime?: TimeInput;
  /** Transaction time — when the fact was recorded (#3225). */
  transactionTime?: TimeInput;
}

/** Wire-form provenance (snake_case, unchanged from the server contract). */
function provenanceToWire(p: Provenance | undefined): Provenance | undefined {
  return p;
}

/** Wire-form derivation refs (already snake_case in {@link LineageRef}). */
function lineageToWire(refs: LineageRef[] | undefined): LineageRef[] | undefined {
  return refs;
}

/**
 * A fully typed client for the AletheiaDB HTTP API.
 *
 * @example
 * ```ts
 * const db = new AletheiaClient({ baseUrl: 'http://localhost:8080', apiKey: KEY });
 * const alice = await db.createNode({ label: 'Person', properties: { name: 'Alice' } });
 * const friends = await db.asOf({ validTime: '2024-01-01T00:00:00Z' })
 *   .traverse({ startNodeId: alice.id, edgeLabel: 'KNOWS' });
 * ```
 */
export class AletheiaClient {
  /** @internal The underlying transport. */
  readonly transport: Transport;

  constructor(options: ClientOptions) {
    this.transport = new Transport(options);
  }

  // ───────────────────────────────────────────────────────────────────────────
  // Temporal scoping
  // ───────────────────────────────────────────────────────────────────────────

  /**
   * Return a temporal view that injects `as_of_valid_time` /
   * `as_of_transaction_time` into supporting reads (traversal, point-in-time
   * finds). Each dimension is independent: set one, the other, or both.
   *
   * @example
   * ```ts
   * // "Who did Alice know on 2024-01-01?"
   * await db.asOf({ validTime: '2024-01-01' })
   *   .traverse({ startNodeId: alice.id, edgeLabel: 'KNOWS' });
   * ```
   */
  asOf(coords: AsOfCoordinates): TemporalView {
    return new TemporalView(this, coords);
  }

  // ───────────────────────────────────────────────────────────────────────────
  // Node reads
  // ───────────────────────────────────────────────────────────────────────────

  /** `GET /nodes/{id}` — fetch a node by id with bi-temporal bounds. */
  getNode(id: number, opts: GetNodeOptions = {}): Promise<NodeEntity> {
    return this.transport.request<NodeEntity>({
      method: 'GET',
      path: `/nodes/${id}`,
      query: {
        include_vectors: opts.includeVectors,
        ...budgetQuery(opts),
      },
    });
  }

  /** `GET /nodes` — list nodes with optional label/property filtering + paging. */
  listNodes(opts: ListNodesOptions = {}): Promise<NodeListResponse> {
    return this.transport.request<NodeListResponse>({
      method: 'GET',
      path: '/nodes',
      query: {
        label: opts.label,
        property_key: opts.propertyKey,
        property_value: opts.propertyValue,
        limit: opts.limit,
        offset: opts.offset,
        include_vectors: opts.includeVectors,
        use_cursor: opts.useCursor,
        cursor: opts.cursor,
        ...budgetQuery(opts),
      },
    });
  }

  /** `GET /nodes/count` — total node count, or nodes matching a label. */
  countNodes(opts: CountOptions = {}): Promise<CountResponse> {
    return this.transport.request<CountResponse>({
      method: 'GET',
      path: '/nodes/count',
      query: { label: opts.label },
    });
  }

  // ───────────────────────────────────────────────────────────────────────────
  // Node writes
  // ───────────────────────────────────────────────────────────────────────────

  /** `POST /nodes` — create a node (optional `validTime`, provenance, lineage). */
  createNode(req: CreateNodeRequest): Promise<NodeEntity> {
    return this.transport.request<NodeEntity>({
      method: 'POST',
      path: '/nodes',
      body: {
        label: req.label,
        properties: req.properties,
        valid_time: optTime(req.validTime),
        provenance: provenanceToWire(req.provenance),
        derived_from: lineageToWire(req.derivedFrom),
      },
    });
  }

  /** `POST /nodes/update` — replace a node's properties, recording a new version. */
  updateNode(req: UpdateNodeRequest): Promise<NodeEntity> {
    return this.transport.request<NodeEntity>({
      method: 'POST',
      path: '/nodes/update',
      body: {
        node_id: req.nodeId,
        properties: req.properties,
        valid_time: optTime(req.validTime),
        provenance: provenanceToWire(req.provenance),
        derived_from: lineageToWire(req.derivedFrom),
      },
    });
  }

  /** `POST /nodes/delete` — safe-by-default delete; `detach` cascades (#3209). */
  deleteNode(req: DeleteNodeRequest): Promise<JsonValue> {
    return this.transport.request<JsonValue>({
      method: 'POST',
      path: '/nodes/delete',
      body: {
        node_id: req.nodeId,
        detach: req.detach,
        valid_time: optTime(req.validTime),
      },
    });
  }

  /** `POST /nodes/delete_cascade` — delete a node and all connected edges atomically. */
  deleteNodeCascade(req: DeleteNodeCascadeRequest): Promise<JsonValue> {
    return this.transport.request<JsonValue>({
      method: 'POST',
      path: '/nodes/delete_cascade',
      body: { node_id: req.nodeId },
    });
  }

  /** `POST /nodes/retract` — close a node's valid-time interval without deleting history (#3230). */
  retractNode(req: RetractNodeRequest): Promise<JsonValue> {
    return this.transport.request<JsonValue>({
      method: 'POST',
      path: '/nodes/retract',
      body: {
        node_id: req.nodeId,
        valid_time: optTime(req.validTime),
        detach: req.detach,
      },
    });
  }

  /** `POST /nodes/find_at_time` — resolve nodes by label/property at a bi-temporal point (#3236). */
  findNodesAtTime(req: FindNodesAtTimeRequest): Promise<NodeListResponse> {
    return this.transport.request<NodeListResponse>({
      method: 'POST',
      path: '/nodes/find_at_time',
      body: {
        label: req.label,
        property_key: req.propertyKey,
        property_value: req.propertyValue,
        valid_time: optTime(req.validTime),
        transaction_time: optTime(req.transactionTime),
        limit: req.limit,
        offset: req.offset,
        include_vectors: req.includeVectors,
        use_cursor: req.useCursor,
        cursor: req.cursor,
        ...budgetBody(req),
      },
    });
  }

  // ───────────────────────────────────────────────────────────────────────────
  // Edge reads
  // ───────────────────────────────────────────────────────────────────────────

  /** `GET /edges/{id}` — fetch an edge by id with bi-temporal bounds. */
  getEdge(id: number, opts: GetEdgeOptions = {}): Promise<EdgeEntity> {
    return this.transport.request<EdgeEntity>({
      method: 'GET',
      path: `/edges/${id}`,
      query: {
        include_vectors: opts.includeVectors,
        ...budgetQuery(opts),
      },
    });
  }

  /** `GET /edges` — list edges with optional label filtering + paging. */
  listEdges(opts: ListEdgesOptions = {}): Promise<EdgeListResponse> {
    return this.transport.request<EdgeListResponse>({
      method: 'GET',
      path: '/edges',
      query: {
        label: opts.label,
        limit: opts.limit,
        offset: opts.offset,
        include_vectors: opts.includeVectors,
        ...budgetQuery(opts),
      },
    });
  }

  /** `GET /edges/count` — total edge count, or edges matching a label. */
  countEdges(opts: CountOptions = {}): Promise<CountResponse> {
    return this.transport.request<CountResponse>({
      method: 'GET',
      path: '/edges/count',
      query: { label: opts.label },
    });
  }

  /** `GET /nodes/{node_id}/edges/outgoing` — outgoing edges from a node. */
  getOutgoingEdges(nodeId: number, opts: AdjacencyOptions = {}): Promise<EdgeListResponse> {
    return this.transport.request<EdgeListResponse>({
      method: 'GET',
      path: `/nodes/${nodeId}/edges/outgoing`,
      query: adjacencyQuery(opts),
    });
  }

  /** `GET /nodes/{node_id}/edges/incoming` — incoming edges to a node. */
  getIncomingEdges(nodeId: number, opts: AdjacencyOptions = {}): Promise<EdgeListResponse> {
    return this.transport.request<EdgeListResponse>({
      method: 'GET',
      path: `/nodes/${nodeId}/edges/incoming`,
      query: adjacencyQuery(opts),
    });
  }

  // ───────────────────────────────────────────────────────────────────────────
  // Edge writes
  // ───────────────────────────────────────────────────────────────────────────

  /** `POST /edges` — create an edge (optional `validTime`, provenance, lineage). */
  createEdge(req: CreateEdgeRequest): Promise<EdgeEntity> {
    return this.transport.request<EdgeEntity>({
      method: 'POST',
      path: '/edges',
      body: {
        source_id: req.sourceId,
        target_id: req.targetId,
        label: req.label,
        properties: req.properties,
        valid_time: optTime(req.validTime),
        provenance: provenanceToWire(req.provenance),
        derived_from: lineageToWire(req.derivedFrom),
      },
    });
  }

  /** `POST /edges/update` — replace an edge's properties, recording a new version. */
  updateEdge(req: UpdateEdgeRequest): Promise<EdgeEntity> {
    return this.transport.request<EdgeEntity>({
      method: 'POST',
      path: '/edges/update',
      body: {
        edge_id: req.edgeId,
        properties: req.properties,
        valid_time: optTime(req.validTime),
        provenance: provenanceToWire(req.provenance),
        derived_from: lineageToWire(req.derivedFrom),
      },
    });
  }

  /** `POST /edges/delete` — delete an edge (optional `validTime`). */
  deleteEdge(req: DeleteEdgeRequest): Promise<JsonValue> {
    return this.transport.request<JsonValue>({
      method: 'POST',
      path: '/edges/delete',
      body: { edge_id: req.edgeId, valid_time: optTime(req.validTime) },
    });
  }

  /** `POST /edges/retract` — close an edge's valid-time interval without deleting history (#3230). */
  retractEdge(req: RetractEdgeRequest): Promise<JsonValue> {
    return this.transport.request<JsonValue>({
      method: 'POST',
      path: '/edges/retract',
      body: { edge_id: req.edgeId, valid_time: optTime(req.validTime) },
    });
  }

  // ───────────────────────────────────────────────────────────────────────────
  // Traversal + temporal
  // ───────────────────────────────────────────────────────────────────────────

  /** `GET /traverse` — multi-hop traversal, optionally as of a bi-temporal point (#3225). */
  traverse(opts: TraverseOptions = {}): Promise<TraverseResponse> {
    return this.transport.request<TraverseResponse>({
      method: 'GET',
      path: '/traverse',
      query: {
        start_node_id: opts.startNodeId,
        edge_label: opts.edgeLabel,
        direction: opts.direction,
        depth: opts.depth,
        limit: opts.limit,
        offset: opts.offset,
        include_vectors: opts.includeVectors,
        as_of_valid_time: optTime(opts.asOfValidTime),
        as_of_transaction_time: optTime(opts.asOfTransactionTime),
        use_cursor: opts.useCursor,
        cursor: opts.cursor,
        ...budgetQuery(opts),
      },
    });
  }

  /** `GET /nodes/{id}/history` — full version history of a node, oldest first. */
  getNodeHistory(id: number, opts: NodeHistoryOptions = {}): Promise<NodeHistoryResponse> {
    return this.transport.request<NodeHistoryResponse>({
      method: 'GET',
      path: `/nodes/${id}/history`,
      query: { ...budgetQuery(opts) },
    });
  }

  /** `GET /edges/{id}/history` — full version history of an edge, oldest first. */
  getEdgeHistory(id: number): Promise<EdgeHistoryResponse> {
    return this.transport.request<EdgeHistoryResponse>({
      method: 'GET',
      path: `/edges/${id}/history`,
    });
  }

  /** `POST /nodes/at_time` — a node's state at `(validTime, transactionTime)`. */
  getNodeAtTime(req: GetNodeAtTimeRequest): Promise<NodeEntity> {
    return this.transport.request<NodeEntity>({
      method: 'POST',
      path: '/nodes/at_time',
      body: {
        node_id: req.nodeId,
        valid_time: reqTime(req.validTime),
        transaction_time: optTime(req.transactionTime),
      },
    });
  }

  /** `POST /edges/at_time` — an edge's state at `(validTime, transactionTime)`. */
  getEdgeAtTime(req: GetEdgeAtTimeRequest): Promise<EdgeEntity> {
    return this.transport.request<EdgeEntity>({
      method: 'POST',
      path: '/edges/at_time',
      body: {
        edge_id: req.edgeId,
        valid_time: reqTime(req.validTime),
        transaction_time: optTime(req.transactionTime),
      },
    });
  }

  /** `POST /nodes/at_valid_time` — a node's state at a valid time (tx = now). */
  getNodeAtValidTime(req: NodeAtValidTimeRequest): Promise<NodeEntity> {
    return this.transport.request<NodeEntity>({
      method: 'POST',
      path: '/nodes/at_valid_time',
      body: { node_id: req.nodeId, valid_time: reqTime(req.validTime) },
    });
  }

  /** `POST /nodes/at_transaction_time` — a node's state at a transaction time (valid = now). */
  getNodeAtTransactionTime(req: NodeAtTransactionTimeRequest): Promise<NodeEntity> {
    return this.transport.request<NodeEntity>({
      method: 'POST',
      path: '/nodes/at_transaction_time',
      body: { node_id: req.nodeId, transaction_time: reqTime(req.transactionTime) },
    });
  }

  /** `POST /edges/at_valid_time` — an edge's state at a valid time (tx = now). */
  getEdgeAtValidTime(req: EdgeAtValidTimeRequest): Promise<EdgeEntity> {
    return this.transport.request<EdgeEntity>({
      method: 'POST',
      path: '/edges/at_valid_time',
      body: { edge_id: req.edgeId, valid_time: reqTime(req.validTime) },
    });
  }

  /** `POST /edges/at_transaction_time` — an edge's state at a transaction time (valid = now). */
  getEdgeAtTransactionTime(req: EdgeAtTransactionTimeRequest): Promise<EdgeEntity> {
    return this.transport.request<EdgeEntity>({
      method: 'POST',
      path: '/edges/at_transaction_time',
      body: { edge_id: req.edgeId, transaction_time: reqTime(req.transactionTime) },
    });
  }

  /** `POST /nodes/diff` — property-level diff between two node versions. */
  diffNodeVersions(req: DiffNodeVersionsRequest): Promise<JsonValue> {
    return this.transport.request<JsonValue>({
      method: 'POST',
      path: '/nodes/diff',
      body: {
        node_id: req.nodeId,
        from_version: req.fromVersion,
        to_version: req.toVersion,
      },
    });
  }

  /** `POST /edges/diff` — property-level diff between two edge versions. */
  diffEdgeVersions(req: DiffEdgeVersionsRequest): Promise<JsonValue> {
    return this.transport.request<JsonValue>({
      method: 'POST',
      path: '/edges/diff',
      body: {
        edge_id: req.edgeId,
        from_version: req.fromVersion,
        to_version: req.toVersion,
      },
    });
  }

  /** `POST /changes` — node/edge versions committed within a transaction-time window. */
  listChanges(req: ListChangesRequest): Promise<JsonValue> {
    return this.transport.request<JsonValue>({
      method: 'POST',
      path: '/changes',
      body: {
        tx_from: reqTime(req.txFrom),
        tx_to: reqTime(req.txTo),
        valid_from: optTime(req.validFrom),
        valid_to: optTime(req.validTo),
        label: req.label,
        limit: req.limit,
        cursor: req.cursor,
      },
    });
  }

  // ───────────────────────────────────────────────────────────────────────────
  // Admin + health
  // ───────────────────────────────────────────────────────────────────────────

  /** `GET /status` — liveness probe. */
  status(): Promise<StatusResponse> {
    return this.transport.request<StatusResponse>({ method: 'GET', path: '/status' });
  }

  /** `POST /admin/keys` — create an API key (admin). Plaintext returned once. */
  async createKey(req: CreateKeyRequest): Promise<CreateKeyResponse> {
    const body = await this.transport.request<unknown>({
      method: 'POST',
      path: '/admin/keys',
      body: { name: req.name, role: req.role },
    });
    return unwrapData<CreateKeyResponse>(body);
  }

  /** `GET /admin/keys` — list masked key principals (admin). */
  async listKeys(): Promise<ListKeysResponse> {
    const body = await this.transport.request<unknown>({ method: 'GET', path: '/admin/keys' });
    return unwrapData<ListKeysResponse>(body);
  }

  /** `POST /admin/keys/revoke` — revoke a key by principal id (admin). */
  async revokeKey(req: RevokeKeyRequest): Promise<RevokeKeyResponse> {
    const body = await this.transport.request<unknown>({
      method: 'POST',
      path: '/admin/keys/revoke',
      body: { id: req.id },
    });
    return unwrapData<RevokeKeyResponse>(body);
  }

  // ───────────────────────────────────────────────────────────────────────────
  // Not-yet-merged endpoints — typed stubs (throw NotImplementedError).
  // TODO(#3369-followup): wire when PR5–PR8 land (vector/hybrid/query/schema/
  // stats/batch/lineage REST routes).
  // ───────────────────────────────────────────────────────────────────────────

  /** STUB: `find_similar` (vector k-NN). Not merged server-side yet. */
  findSimilar(): Promise<never> {
    return stub('find_similar');
  }

  /** STUB: `hybrid_query` (graph + vector + temporal). Not merged server-side yet. */
  hybridQuery(): Promise<never> {
    return stub('hybrid_query');
  }

  /** STUB: `query` (read-only Cypher/AQL). Not merged server-side yet. */
  query(): Promise<never> {
    return stub('query');
  }

  /** STUB: `enable_vector_index`. Not merged server-side yet. */
  enableVectorIndex(): Promise<never> {
    return stub('enable_vector_index');
  }

  /** STUB: `list_vector_indexes`. Not merged server-side yet. */
  listVectorIndexes(): Promise<never> {
    return stub('list_vector_indexes');
  }

  /** STUB: `get_schema` (labels/types/property keys with counts). Not merged yet. */
  getSchema(): Promise<never> {
    return stub('get_schema');
  }

  /** STUB: `database_stats` (holistic snapshot). Not merged server-side yet. */
  databaseStats(): Promise<never> {
    return stub('database_stats');
  }

  /** STUB: `temporal_extent` (queryable bi-temporal extent). Not merged yet. */
  temporalExtent(): Promise<never> {
    return stub('temporal_extent');
  }

  /** STUB: `apply_batch` (atomic multi-write batch). Not merged server-side yet. */
  applyBatch(): Promise<never> {
    return stub('apply_batch');
  }

  /** STUB: `lineage_upstream` (derivation closure). Not merged server-side yet. */
  lineageUpstream(): Promise<never> {
    return stub('lineage_upstream');
  }

  /** STUB: `lineage_downstream` (derivation blast radius). Not merged yet. */
  lineageDownstream(): Promise<never> {
    return stub('lineage_downstream');
  }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/** Coerce an optional {@link TimeInput} to a wire value, or `undefined`. */
function optTime(t: TimeInput | undefined): string | undefined {
  return t === undefined ? undefined : toWireTime(t);
}

/** Coerce a required {@link TimeInput} to a wire value. */
function reqTime(t: TimeInput): string {
  return toWireTime(t);
}

/** Build the #3353 budget query fragment (query-string form). */
function budgetQuery(opts: {
  maxResponseTokens?: number;
  maxResponseBytes?: number;
  priorityProperties?: string[];
}): Record<string, string | number | string[] | undefined> {
  return {
    max_response_tokens: opts.maxResponseTokens,
    max_response_bytes: opts.maxResponseBytes,
    priority_properties: opts.priorityProperties,
  };
}

/** Build the #3353 budget body fragment (JSON-body form). */
function budgetBody(opts: {
  maxResponseTokens?: number;
  maxResponseBytes?: number;
  priorityProperties?: string[];
}): Record<string, number | string[] | undefined> {
  return {
    max_response_tokens: opts.maxResponseTokens,
    max_response_bytes: opts.maxResponseBytes,
    priority_properties: opts.priorityProperties,
  };
}

/** Build the shared adjacency query (label/flag/#3353 budget/#3360 cursor). */
function adjacencyQuery(
  opts: AdjacencyOptions,
): Record<string, string | number | boolean | string[] | undefined> {
  return {
    label: opts.label,
    include_vectors: opts.includeVectors,
    limit: opts.limit,
    use_cursor: opts.useCursor,
    cursor: opts.cursor,
    ...budgetQuery(opts),
  };
}

/** Throw the standard {@link NotImplementedError} for an un-merged endpoint. */
function stub(tool: string): Promise<never> {
  return Promise.reject(
    new NotImplementedError(
      `${tool} is not wrapped yet: its REST route is not merged server-side. ` +
        `Tracking in the #3369 follow-up (PR5–PR8). See COMPATIBILITY.md.`,
    ),
  );
}

export type { RequestSpec };
