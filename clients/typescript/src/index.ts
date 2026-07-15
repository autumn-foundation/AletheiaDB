/**
 * `@aletheiadb/client` — the official TypeScript SDK for AletheiaDB.
 *
 * Typed bi-temporal graph + vector queries over the HTTP API: node/edge CRUD
 * (with optional `validTime`), traversal (with `asOf` coordinates),
 * point-in-time reads, structured typed errors, and an opt-in retry policy.
 *
 * @example
 * ```ts
 * import { AletheiaClient } from '@aletheiadb/client';
 *
 * const db = new AletheiaClient({ baseUrl: 'http://localhost:8080', apiKey: KEY });
 * const alice = await db.createNode({ label: 'Person', properties: { name: 'Alice' } });
 * ```
 *
 * @packageDocumentation
 */

export { AletheiaClient } from './client.js';
export type { AsOfCoordinates } from './client.js';
export { TemporalView } from './temporal-view.js';

export type {
  ClientOptions,
  RequestOptions,
  AuthScheme,
  FetchLike,
  FetchResponseLike,
  RequestSpec,
} from './http.js';

export type { RetryOptions } from './retry.js';

export { toEpochMicros, toWireTime } from './time.js';
export type { TimeInput } from './time.js';

export {
  AletheiaError,
  NotFoundError,
  InvalidArgumentError,
  ConstraintViolationError,
  FailedPreconditionError,
  ConflictError,
  UnavailableError,
  InternalError,
  UnauthenticatedError,
  PermissionDeniedError,
  ResourceExhaustedError,
  NotImplementedError,
  makeError,
} from './errors.js';
export type { AletheiaErrorCode, AletheiaErrorInit, ErrorDetails } from './errors.js';

export {
  isElidedVector,
  isElidedSparseVector,
  isFullVector,
} from './types.js';

export type {
  JsonValue,
  PropertyValue,
  PropertyMap,
  ElidedVector,
  ElidedSparseVector,
  VectorProperty,
  TemporalBounds,
  Provenance,
  NodeEntity,
  EdgeEntity,
  Completeness,
  BudgetInfo,
  NodeListResponse,
  EdgeListResponse,
  TraverseResult,
  TraverseResponse,
  CountResponse,
  NodeHistoryResponse,
  EdgeHistoryResponse,
  IncludeVectorsOption,
  BudgetOptions,
  OffsetPageOptions,
  CursorOptions,
  LineageRef,
  CreateNodeRequest,
  UpdateNodeRequest,
  DeleteNodeRequest,
  DeleteNodeCascadeRequest,
  RetractNodeRequest,
  CreateEdgeRequest,
  UpdateEdgeRequest,
  DeleteEdgeRequest,
  RetractEdgeRequest,
  GetNodeOptions,
  GetEdgeOptions,
  ListNodesOptions,
  ListEdgesOptions,
  AdjacencyOptions,
  TraverseOptions,
  FindNodesAtTimeRequest,
  GetNodeAtTimeRequest,
  GetEdgeAtTimeRequest,
  NodeAtValidTimeRequest,
  NodeAtTransactionTimeRequest,
  EdgeAtValidTimeRequest,
  EdgeAtTransactionTimeRequest,
  DiffNodeVersionsRequest,
  DiffEdgeVersionsRequest,
  ListChangesRequest,
  NodeHistoryOptions,
  CountOptions,
  Role,
  StatusResponse,
  CreateKeyRequest,
  CreateKeyResponse,
  KeyPrincipal,
  ListKeysResponse,
  RevokeKeyRequest,
  RevokeKeyResponse,
  DistanceMetric,
  FindSimilarRequest,
  SimilarityResult,
  FindSimilarResponse,
  EnableVectorIndexRequest,
  EnableVectorIndexResponse,
  VectorIndexInfo,
  ListVectorIndexesResponse,
  HybridQueryRequest,
  HybridResult,
  HybridQueryResponse,
  QueryLimitsOverride,
  QueryRequest,
  QueryResponse,
  GetSchemaOptions,
  SchemaResponse,
  CurrentStateStats,
  HistoricalDepthStats,
  TierAccessStats,
  ColdStorageTierStats,
  WalStateStats,
  LastVerifiedStats,
  ProvenanceChainStats,
  DatabaseStats,
  TemporalExtentOptions,
  TemporalExtentResponse,
  BatchNodeRef,
  BatchOperation,
  BatchCreateNode,
  BatchCreateEdge,
  BatchUpdateNode,
  BatchUpdateEdge,
  BatchDeleteNode,
  BatchDeleteEdge,
  ApplyBatchRequest,
  ApplyBatchResponse,
  LineageQueryRequest,
  LineageEntry,
  LineageResponse,
} from './types.js';
