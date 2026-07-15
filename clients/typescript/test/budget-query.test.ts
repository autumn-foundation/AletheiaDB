import { describe, expect, it } from 'vitest';
import { AletheiaClient } from '../src/index.js';
import { edgeFixture, mockFetch, nodeFixture, type RecordedRequest } from './fixtures.js';

/**
 * Regression guard for the pre-existing `priority_properties`-on-GET bug.
 *
 * The autumn GET routes decode the query string with `axum::extract::Query` =
 * `serde_urlencoded` 0.7.1. Its value deserializer (`Part`) forwards
 * `deserialize_seq` to `deserialize_any`, which visits a plain **string**
 * (serde_urlencoded-0.7.1 `src/de.rs:234-249`). A `Vec<String>` field therefore
 * cannot be produced from ANY query-string encoding — a repeated key, a
 * comma-joined key, anything — so emitting `priority_properties` on a GET read
 * makes the extractor 400 the whole request.
 *
 * These tests assert the SDK never places a `priority_properties` key on a GET
 * query string (so the 400 cannot recur), while the scalar budget params
 * (`max_response_tokens`/`max_response_bytes`, which serde_urlencoded parses as
 * `u64`) still ride. They also assert the array budget IS preserved on the
 * POST-body reads, whose JSON body is parsed by `serde_json` (arrays fine).
 */
async function capture(
  responseBody: unknown,
  run: (db: AletheiaClient) => Promise<unknown>,
): Promise<RecordedRequest> {
  const { fetch, calls } = mockFetch(() => ({ body: responseBody }));
  const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
  await run(db);
  return calls[0]!;
}

const budget = { maxResponseTokens: 800, maxResponseBytes: 4096, priorityProperties: ['title', 'name'] };

describe('priority_properties is never emitted on GET query reads (#3353 / serde_urlencoded)', () => {
  const nodeList = { nodes: [], count: 0 };
  const edgeList = { edges: [], count: 0 };
  const traverseBody = { results: [], count: 0 };

  const getReads: Array<[string, (db: AletheiaClient) => Promise<unknown>, unknown]> = [
    ['getNode', (db) => db.getNode(1, budget), nodeFixture(1, 'Doc', {})],
    ['listNodes', (db) => db.listNodes({ label: 'Doc', ...budget }), nodeList],
    ['getEdge', (db) => db.getEdge(1, budget), edgeFixture(1, 'KNOWS', 1, 2)],
    ['listEdges', (db) => db.listEdges({ label: 'KNOWS', ...budget }), edgeList],
    ['traverse', (db) => db.traverse({ startNodeId: 1, edgeLabel: 'KNOWS', ...budget }), traverseBody],
    ['getOutgoingEdges', (db) => db.getOutgoingEdges(1, budget), edgeList],
    ['getIncomingEdges', (db) => db.getIncomingEdges(1, budget), edgeList],
    ['getNodeHistory', (db) => db.getNodeHistory(1, budget), { node_id: 1, results: [] }],
    ['getSchema', (db) => db.getSchema(budget), { node_labels: [], edge_types: [] }],
  ];

  for (const [name, run, body] of getReads) {
    it(`${name}: emits scalar budget but NO priority_properties key`, async () => {
      const req = await capture(body, run);
      expect(req.method).toBe('GET');
      // The 400-causing array key must never appear on the query string.
      expect(req.query.has('priority_properties')).toBe(false);
      // The scalar budget (serde_urlencoded parses these as u64) still rides.
      expect(req.query.get('max_response_tokens')).toBe('800');
      expect(req.query.get('max_response_bytes')).toBe('4096');
    });
  }
});

describe('priority_properties IS preserved on POST-body reads (serde_json arrays)', () => {
  it('findNodesAtTime -> body carries priority_properties as a JSON array', async () => {
    const req = await capture({ nodes: [], count: 0 }, (db) =>
      db.findNodesAtTime({ label: 'Person', validTime: '2024-01-01', ...budget }),
    );
    expect(req.method).toBe('POST');
    const b = req.body as { priority_properties: unknown; max_response_tokens: unknown };
    expect(b.priority_properties).toEqual(['title', 'name']);
    expect(b.max_response_tokens).toBe(800);
  });

  it('findSimilar -> body carries priority_properties as a JSON array', async () => {
    const req = await capture({ results: [], count: 0 }, (db) =>
      db.findSimilar({ propertyName: 'embedding', embedding: [0.1, 0.2], ...budget }),
    );
    expect(req.method).toBe('POST');
    expect((req.body as { priority_properties: unknown }).priority_properties).toEqual([
      'title',
      'name',
    ]);
  });
});
