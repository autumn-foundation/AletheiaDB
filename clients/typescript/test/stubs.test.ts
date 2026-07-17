import { describe, expect, it } from 'vitest';
import { AletheiaClient, toWireTime, type BatchOperation } from '../src/index.js';
import { mockFetch, nodeFixture, type RecordedRequest } from './fixtures.js';

/** Capture the single request a call makes against a canned response body. */
async function capture(
  responseBody: unknown,
  run: (db: AletheiaClient) => Promise<unknown>,
): Promise<{ req: RecordedRequest; result: unknown }> {
  const { fetch, calls } = mockFetch(() => ({ body: responseBody }));
  const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
  const result = await run(db);
  return { req: calls[0]!, result };
}

describe('vector / query / schema / stats / batch / lineage wiring (#3627)', () => {
  it('findSimilar -> POST /find_similar with property_name/embedding/k/offset', async () => {
    const body = { results: [{ node: nodeFixture(1, 'Doc', {}), score: 0.98 }], count: 1 };
    const { req, result } = await capture(body, (db) =>
      db.findSimilar({ propertyName: 'embedding', embedding: [0.1, 0.2, 0.3], k: 5, offset: 2 }),
    );
    expect(req.method).toBe('POST');
    expect(req.path).toBe('/find_similar');
    expect(req.body).toMatchObject({
      property_name: 'embedding',
      embedding: [0.1, 0.2, 0.3],
      k: 5,
      offset: 2,
    });
    expect((result as { results: unknown[] }).results).toHaveLength(1);
  });

  it('findSimilar forwards include_vectors + #3353 budget', async () => {
    const { req } = await capture({ results: [], count: 0 }, (db) =>
      db.findSimilar({
        propertyName: 'embedding',
        embedding: [1, 2],
        includeVectors: true,
        maxResponseTokens: 512,
        priorityProperties: ['title'],
      }),
    );
    expect(req.body).toMatchObject({
      include_vectors: true,
      max_response_tokens: 512,
      priority_properties: ['title'],
    });
  });

  it('enableVectorIndex -> POST /vector/indexes with property_name/dimensions/distance_metric', async () => {
    const { req, result } = await capture(
      { success: true, property_name: 'embedding', dimensions: 384, distance_metric: 'Cosine' },
      (db) =>
        db.enableVectorIndex({
          propertyName: 'embedding',
          dimensions: 384,
          distanceMetric: 'cosine',
        }),
    );
    expect(req.method).toBe('POST');
    expect(req.path).toBe('/vector/indexes');
    expect(req.body).toEqual({
      property_name: 'embedding',
      dimensions: 384,
      distance_metric: 'cosine',
    });
    // Response parse: the typed success shape round-trips.
    expect((result as { success: boolean; dimensions: number }).success).toBe(true);
    expect((result as { dimensions: number }).dimensions).toBe(384);
  });

  it('listVectorIndexes -> GET /vector/indexes (no body)', async () => {
    const { req, result } = await capture({ indexes: [], count: 0 }, (db) =>
      db.listVectorIndexes(),
    );
    expect(req.method).toBe('GET');
    expect(req.path).toBe('/vector/indexes');
    expect(req.body).toBeUndefined();
    expect((result as { count: number }).count).toBe(0);
  });

  it('hybridQuery -> POST /hybrid_query with graph + vector + temporal params', async () => {
    const { req, result } = await capture(
      {
        results: [{ entity: nodeFixture(3, 'Person', {}), similarity_score: 0.71 }],
        count: 1,
        as_of_valid_time: '2024-01-01T00:00:00Z',
      },
      (db) =>
        db.hybridQuery({
          startNodeId: 7,
          traverseEdge: 'KNOWS',
          traverseDepth: 2,
          vectorProperty: 'embedding',
          queryEmbedding: [0.5, 0.5],
          topK: 10,
          validTime: '2024-01-01T00:00:00Z',
          transactionTime: '2024-06-01T00:00:00Z',
          filterLabel: 'Person',
          limit: 20,
        }),
    );
    expect(req.path).toBe('/hybrid_query');
    expect(req.body).toMatchObject({
      start_node_id: 7,
      traverse_edge: 'KNOWS',
      traverse_depth: 2,
      vector_property: 'embedding',
      query_embedding: [0.5, 0.5],
      top_k: 10,
      valid_time: toWireTime('2024-01-01T00:00:00Z'),
      transaction_time: toWireTime('2024-06-01T00:00:00Z'),
      filter_label: 'Person',
      limit: 20,
    });
    // Response parse: ranked results + their similarity_score survive.
    const hybrid = result as { results: Array<{ similarity_score: number }> };
    expect(hybrid.results).toHaveLength(1);
    expect(hybrid.results[0]!.similarity_score).toBe(0.71);
  });

  it('hybridQuery temporal fields serialize as STRINGS (regression guard)', async () => {
    const { req } = await capture({ results: [], count: 0 }, (db) =>
      db.hybridQuery({ validTime: 1705312800000000, transactionTime: 1705312800000000 }),
    );
    const b = req.body as { valid_time: unknown; transaction_time: unknown };
    expect(typeof b.valid_time).toBe('string');
    expect(typeof b.transaction_time).toBe('string');
  });

  it('query -> POST /query with language/query/params/limit + #3368 limits override', async () => {
    const body = { language: 'cypher', columns: ['n'], rows: [], row_count: 0 };
    const { req, result } = await capture(body, (db) =>
      db.query({
        language: 'cypher',
        query: 'MATCH (n:Person {name:$name}) RETURN n',
        params: { name: 'Alice' },
        limit: 50,
        limits: { timeout_ms: 2000, max_result_rows: 500 },
      }),
    );
    expect(req.method).toBe('POST');
    expect(req.path).toBe('/query');
    expect(req.body).toMatchObject({
      language: 'cypher',
      query: 'MATCH (n:Person {name:$name}) RETURN n',
      params: { name: 'Alice' },
      limit: 50,
      limits: { timeout_ms: 2000, max_result_rows: 500 },
    });
    expect((result as { row_count: number }).row_count).toBe(0);
  });

  it('getSchema -> GET /schema with as_of_* (bi-temporal) and scalar budget', async () => {
    const { req, result } = await capture(
      { node_labels: [{ label: 'Person', count: 2 }], edge_types: [] },
      (db) =>
        db.getSchema({
          asOfValidTime: '2024-01-01T00:00:00Z',
          asOfTransactionTime: '2024-06-01T00:00:00Z',
          maxResponseTokens: 1000,
        }),
    );
    expect(req.method).toBe('GET');
    expect(req.path).toBe('/schema');
    expect(req.query.get('as_of_valid_time')).toBe(toWireTime('2024-01-01T00:00:00Z'));
    expect(req.query.get('as_of_transaction_time')).toBe(toWireTime('2024-06-01T00:00:00Z'));
    expect(req.query.get('max_response_tokens')).toBe('1000');
    // getSchema is not one of the eight priority_properties GET reads (#3638).
    expect(req.query.has('priority_properties')).toBe(false);
    // Response parse: the schema payload round-trips.
    expect((result as { node_labels: unknown[] }).node_labels).toHaveLength(1);
  });

  it('getSchema with no args -> plain GET /schema (current-state)', async () => {
    const { req } = await capture({ node_labels: [], edge_types: [] }, (db) => db.getSchema());
    expect(req.path).toBe('/schema');
    expect(req.query.has('as_of_valid_time')).toBe(false);
    expect(req.query.has('as_of_transaction_time')).toBe(false);
  });

  it('databaseStats -> GET /database_stats (no arguments)', async () => {
    const { req, result } = await capture({ current: { node_count: 3 } }, (db) =>
      db.databaseStats(),
    );
    expect(req.method).toBe('GET');
    expect(req.path).toBe('/database_stats');
    expect(req.body).toBeUndefined();
    expect((result as { current: { node_count: number } }).current.node_count).toBe(3);
  });

  it('temporalExtent -> GET /temporal_extent with by_label', async () => {
    const { req, result } = await capture(
      {
        valid_time: { earliest: '2024-01-01T00:00:00Z', latest: '2024-12-31T00:00:00Z' },
        transaction_time: { earliest: '2024-01-01T00:00:00Z', latest: '2024-12-31T00:00:00Z' },
      },
      (db) => db.temporalExtent({ byLabel: true }),
    );
    expect(req.method).toBe('GET');
    expect(req.path).toBe('/temporal_extent');
    expect(req.query.get('by_label')).toBe('true');
    // Response parse: the bi-temporal bounds round-trip.
    const extent = result as { valid_time: { earliest: string } };
    expect(extent.valid_time.earliest).toBe('2024-01-01T00:00:00Z');
  });

  it('temporalExtent with no args -> plain GET /temporal_extent', async () => {
    const { req } = await capture({ valid_time: {}, transaction_time: {} }, (db) =>
      db.temporalExtent(),
    );
    expect(req.path).toBe('/temporal_extent');
    expect(req.query.has('by_label')).toBe(false);
  });

  it('applyBatch -> POST /batch forwarding operations verbatim', async () => {
    const ops = [
      { op: 'create_node' as const, label: 'Person', properties: { name: 'Alice' }, ref: 'alice' },
      { op: 'create_edge' as const, source_id: '$alice', target_id: 2, label: 'KNOWS' },
    ];
    const { req, result } = await capture(
      { results: [{ node_id: 10 }, { edge_id: 11 }], ref_map: { alice: 10 } },
      (db) => db.applyBatch({ operations: ops }),
    );
    expect(req.method).toBe('POST');
    expect(req.path).toBe('/batch');
    expect(req.body).toEqual({ operations: ops });
    expect((result as { ref_map: Record<string, number> }).ref_map.alice).toBe(10);
  });

  it('BatchOperation rejects camelCase / TimeInput at the TYPE level (footgun guard)', () => {
    // The wire shape is snake_case with a STRING valid_time. camelCase keys and a
    // numeric/TimeInput valid_time must be COMPILE errors, not silently dropped
    // mis-timed writes. tsc is the guard (esbuild strips these at runtime, so the
    // never-called factory has no runtime effect).
    const good = (): BatchOperation => ({
      op: 'create_edge',
      source_id: '$alice',
      target_id: 2,
      label: 'KNOWS',
      valid_time: toWireTime('2024-01-01T00:00:00Z'),
    });
    const badCamelCase = (): BatchOperation =>
      // @ts-expect-error camelCase `sourceId`/`targetId` are not valid BatchOperation keys — use snake_case source_id/target_id
      ({ op: 'create_edge', sourceId: 1, targetId: 2, label: 'KNOWS' });
    const badNumericTime = (): BatchOperation =>
      // @ts-expect-error valid_time must be a string (RFC 3339 / decimal-µs), not a number
      ({ op: 'create_node', label: 'Person', valid_time: 1705312800000000 });
    expect([good, badCamelCase, badNumericTime].every((f) => typeof f === 'function')).toBe(true);
  });

  it('lineageUpstream -> POST /lineage/upstream with version-pinned root + as_of', async () => {
    const { req, result } = await capture(
      {
        direction: 'upstream',
        root: { entity_kind: 'node', id: 5, version: 2 },
        entries: [{ entity_kind: 'node', id: 4, version: 1, depth: 1, status: 'Current' }],
        count: 1,
      },
      (db) =>
        db.lineageUpstream({
          entityKind: 'node',
          id: 5,
          version: 2,
          maxDepth: 3,
          limit: 50,
          offset: 0,
          asOfTransactionTime: '2024-06-01T00:00:00Z',
        }),
    );
    expect(req.method).toBe('POST');
    expect(req.path).toBe('/lineage/upstream');
    expect(req.body).toMatchObject({
      entity_kind: 'node',
      id: 5,
      version: 2,
      max_depth: 3,
      limit: 50,
      offset: 0,
      as_of_transaction_time: toWireTime('2024-06-01T00:00:00Z'),
    });
    // Response parse: the closure entries + version-pinned root survive.
    const view = result as { direction: string; entries: Array<{ status: string }> };
    expect(view.direction).toBe('upstream');
    expect(view.entries[0]!.status).toBe('Current');
  });

  it('lineageDownstream -> POST /lineage/downstream (blast radius)', async () => {
    const { req, result } = await capture(
      {
        direction: 'downstream',
        root: { entity_kind: 'edge', id: 9, version: 1 },
        entries: [],
        count: 0,
      },
      (db) => db.lineageDownstream({ entityKind: 'edge', id: 9, version: 1 }),
    );
    expect(req.path).toBe('/lineage/downstream');
    expect(req.body).toMatchObject({ entity_kind: 'edge', id: 9, version: 1 });
    expect((result as { direction: string }).direction).toBe('downstream');
  });
});
