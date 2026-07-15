/**
 * The AletheiaDB TypeScript client.
 *
 * Wraps the autumn-server REST routes (node/edge/traverse/temporal +
 * admin/health) as typed methods, including the vector / query / schema / stats
 * / batch / lineage tools (find_similar, hybrid_query, query,
 * enable_vector_index, list_vector_indexes, get_schema, database_stats,
 * temporal_extent, apply_batch, lineage_upstream, lineage_downstream) that are
 * now live on the autumn surface.
 *
 * @packageDocumentation
 */

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
  FindSimilarRequest,
  FindSimilarResponse,
  EnableVectorIndexRequest,
  EnableVectorIndexResponse,
  ListVectorIndexesResponse,
  HybridQueryRequest,
  HybridQueryResponse,
  QueryRequest,
  QueryResponse,
  GetSchemaOptions,
  SchemaResponse,
  DatabaseStats,
  TemporalExtentOptions,
  TemporalExtentResponse,
  ApplyBatchRequest,
  ApplyBatchResponse,
  LineageQueryRequest,
  LineageResponse,
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
  // Vector search
  // ───────────────────────────────────────────────────────────────────────────

  /** `POST /find_similar` — k-NN over a property's embeddings (#3220 elision, #3353 budget). */
  findSimilar(req: FindSimilarRequest): Promise<FindSimilarResponse> {
    return this.transport.request<FindSimilarResponse>({
      method: 'POST',
      path: '/find_similar',
      body: {
        property_name: req.propertyName,
        embedding: req.embedding,
        k: req.k,
        offset: req.offset,
        include_vectors: req.includeVectors,
        ...budgetBody(req),
      },
    });
  }

  /** `POST /vector/indexes` — enable HNSW vector indexing on a node property. */
  enableVectorIndex(req: EnableVectorIndexRequest): Promise<EnableVectorIndexResponse> {
    return this.transport.request<EnableVectorIndexResponse>({
      method: 'POST',
      path: '/vector/indexes',
      body: {
        property_name: req.propertyName,
        dimensions: req.dimensions,
        distance_metric: req.distanceMetric,
      },
    });
  }

  /** `GET /vector/indexes` — list active vector indexes and their configuration. */
  listVectorIndexes(): Promise<ListVectorIndexesResponse> {
    return this.transport.request<ListVectorIndexesResponse>({
      method: 'GET',
      path: '/vector/indexes',
    });
  }

  // ───────────────────────────────────────────────────────────────────────────
  // Hybrid query (graph + vector + temporal)
  // ───────────────────────────────────────────────────────────────────────────

  /** `POST /hybrid_query` — graph traversal + vector similarity + temporal filtering in one call. */
  hybridQuery(req: HybridQueryRequest = {}): Promise<HybridQueryResponse> {
    return this.transport.request<HybridQueryResponse>({
      method: 'POST',
      path: '/hybrid_query',
      body: {
        start_node_id: req.startNodeId,
        traverse_edge: req.traverseEdge,
        traverse_depth: req.traverseDepth,
        vector_property: req.vectorProperty,
        query_embedding: req.queryEmbedding,
        top_k: req.topK,
        valid_time: optTime(req.validTime),
        transaction_time: optTime(req.transactionTime),
        filter_label: req.filterLabel,
        limit: req.limit,
        include_vectors: req.includeVectors,
        ...budgetBody(req),
      },
    });
  }

  // ───────────────────────────────────────────────────────────────────────────
  // Read-only declarative query (Cypher / AQL)
  // ───────────────────────────────────────────────────────────────────────────

  /** `POST /query` — execute a single read-only Cypher/AQL statement (#3353 budget). */
  query(req: QueryRequest): Promise<QueryResponse> {
    return this.transport.request<QueryResponse>({
      method: 'POST',
      path: '/query',
      body: {
        language: req.language,
        query: req.query,
        params: req.params,
        limit: req.limit,
        limits: req.limits,
        ...budgetBody(req),
      },
    });
  }

  // ───────────────────────────────────────────────────────────────────────────
  // Schema / stats / temporal extent
  // ───────────────────────────────────────────────────────────────────────────

  /**
   * `GET /schema` — node labels, edge types, and property keys with counts
   * (optional bi-temporal via `asOf*`).
   *
   * Only the scalar token/byte budget is sent: `priority_properties` is a
   * repeated-key array that the server's `GET` query-string extractor
   * (`serde_urlencoded`) cannot decode into a `Vec<String>`, so it is
   * deliberately not forwarded (see {@link GetSchemaOptions}).
   */
  getSchema(opts: GetSchemaOptions = {}): Promise<SchemaResponse> {
    return this.transport.request<SchemaResponse>({
      method: 'GET',
      path: '/schema',
      query: {
        as_of_valid_time: optTime(opts.asOfValidTime),
        as_of_transaction_time: optTime(opts.asOfTransactionTime),
        max_response_tokens: opts.maxResponseTokens,
        max_response_bytes: opts.maxResponseBytes,
      },
    });
  }

  /**
   * `GET /database_stats` — a holistic bi-temporal snapshot (no arguments).
   *
   * Requires the **metrics** (or **admin**) role: a reader/writer-only key is
   * rejected with `PERMISSION_DENIED` (`database_stats` is classified
   * `MetricsClass`, not `ReadClass`).
   */
  databaseStats(): Promise<DatabaseStats> {
    return this.transport.request<DatabaseStats>({
      method: 'GET',
      path: '/database_stats',
    });
  }

  /** `GET /temporal_extent` — the dataset's queryable bi-temporal extent (optional `byLabel`). */
  temporalExtent(opts: TemporalExtentOptions = {}): Promise<TemporalExtentResponse> {
    return this.transport.request<TemporalExtentResponse>({
      method: 'GET',
      path: '/temporal_extent',
      query: { by_label: opts.byLabel },
    });
  }

  // ───────────────────────────────────────────────────────────────────────────
  // Atomic multi-write batch (#3231)
  // ───────────────────────────────────────────────────────────────────────────

  /** `POST /batch` — apply an ordered batch of write operations atomically (all-or-nothing). */
  applyBatch(req: ApplyBatchRequest): Promise<ApplyBatchResponse> {
    return this.transport.request<ApplyBatchResponse>({
      method: 'POST',
      path: '/batch',
      body: { operations: req.operations },
    });
  }

  // ───────────────────────────────────────────────────────────────────────────
  // Derivation lineage (#3371)
  // ───────────────────────────────────────────────────────────────────────────

  /** `POST /lineage/upstream` — the transitive evidence chain: what a fact was derived from. */
  lineageUpstream(req: LineageQueryRequest): Promise<LineageResponse> {
    return this.transport.request<LineageResponse>({
      method: 'POST',
      path: '/lineage/upstream',
      body: lineageQueryToWire(req),
    });
  }

  /** `POST /lineage/downstream` — the transitive blast radius: what has been derived from a fact. */
  lineageDownstream(req: LineageQueryRequest): Promise<LineageResponse> {
    return this.transport.request<LineageResponse>({
      method: 'POST',
      path: '/lineage/downstream',
      body: lineageQueryToWire(req),
    });
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

/** Build the wire body shared by {@link AletheiaClient.lineageUpstream} / `lineageDownstream`. */
function lineageQueryToWire(
  req: LineageQueryRequest,
): Record<string, string | number | undefined> {
  return {
    entity_kind: req.entityKind,
    id: req.id,
    version: req.version,
    max_depth: req.maxDepth,
    limit: req.limit,
    offset: req.offset,
    as_of_transaction_time: optTime(req.asOfTransactionTime),
  };
}

export type { RequestSpec };
